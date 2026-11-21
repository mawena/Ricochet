//! Applications web locales : le relais demande une requête HTTP, l'agent la fait à
//! 127.0.0.1 sur le port déclaré et renvoie la réponse au fil de l'eau.
//!
//! Seules les applications de la section `[applis.<nom>]` sont joignables, et seulement sur
//! leur port : le relais ne donne qu'un nom, jamais un port ni une adresse. Les en-têtes
//! configurés (un jeton d'accès, par exemple) sont ajoutés ici et ne quittent jamais la
//! machine. Rien n'est écrit sur le disque ni dans le journal : ni corps, ni en-têtes.

use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex},
    time::Duration,
};

use bytes::Bytes;
use http::{HeaderMap, HeaderName, HeaderValue, Method, Request, header};
use http_body_util::{BodyExt, Full};
use hyper_util::rt::TokioIo;
use ricochet_protocole::{AgentVersRelais, EntetesHttp, InfoAppli, NumeroRequete};
use tokio::{net::TcpStream, sync::mpsc, task::AbortHandle};

use crate::config::ConfigAppli;

/// Requêtes en cours en même temps (au-delà, la requête est refusée).
pub const REQUETES_MAX: usize = 32;
/// Taille maximale d'un morceau de corps renvoyé au relais.
const MORCEAU: usize = 48 * 1024;
const DELAI_CONNEXION: Duration = Duration::from_secs(5);
/// Attente maximale de l'en-tête de la réponse (le corps, lui, peut durer : flux d'évènements).
const DELAI_ENTETE: Duration = Duration::from_secs(30);

/// En-têtes que l'agent ne transmet jamais à l'application, même si le relais les envoie :
/// authentification du relais, cadrage de la connexion.
const REFUSES: &[&str] = &[
    "host",
    "cookie",
    "authorization",
    "proxy-authorization",
    "connection",
    "keep-alive",
    "transfer-encoding",
    "te",
    "trailer",
    "upgrade",
    "content-length",
    "expect",
    "x-ricochet",
    "x-real-ip",
    "x-forwarded-for",
];

/// En-têtes de la réponse qui ne sont pas renvoyés : cadrage de la connexion et cookies (une
/// application ne pose jamais de cookie sur le domaine du relais).
const REPONSE_REFUSES: &[&str] = &[
    "connection",
    "keep-alive",
    "transfer-encoding",
    "te",
    "trailer",
    "upgrade",
    "proxy-authenticate",
    "set-cookie",
    "set-cookie2",
];

const METHODES: &[&str] = &["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"];

/// Les applications d'une session, et les requêtes qu'elles servent en ce moment.
pub struct Applis {
    config: Arc<BTreeMap<String, ConfigAppli>>,
    sortie: mpsc::Sender<AgentVersRelais>,
    actives: Arc<Mutex<HashMap<NumeroRequete, AbortHandle>>>,
}

impl Applis {
    /// Renvoie aussi la file des messages à envoyer au relais.
    pub fn new(config: BTreeMap<String, ConfigAppli>) -> (Self, mpsc::Receiver<AgentVersRelais>) {
        let (sortie, reception) = mpsc::channel(16);
        (
            Self {
                config: Arc::new(config),
                sortie,
                actives: Arc::default(),
            },
            reception,
        )
    }

    /// Ce que l'agent annonce au relais : les noms et les libellés, jamais les ports ni les
    /// en-têtes.
    pub fn infos(config: &BTreeMap<String, ConfigAppli>) -> Vec<InfoAppli> {
        config
            .iter()
            .map(|(nom, appli)| InfoAppli {
                nom: nom.clone(),
                libelle: appli.libelle.clone().unwrap_or_else(|| nom.clone()),
            })
            .collect()
    }

    /// Lance la requête ; la réponse part par la file de sortie.
    pub fn lancer(&self, requete: NumeroRequete, demande: Demande) {
        let sortie = self.sortie.clone();
        let actives = self.actives.clone();
        let Some(appli) = self.config.get(&demande.appli).cloned() else {
            tracing::warn!("application « {} » non déclarée : refusée", demande.appli);
            fin_immediate(
                sortie,
                requete,
                "application non déclarée sur cette machine",
            );
            return;
        };
        let mut en_cours = self.actives.lock().unwrap();
        if en_cours.len() >= REQUETES_MAX {
            drop(en_cours);
            fin_immediate(sortie, requete, "trop de requêtes en cours");
            return;
        }
        let tache = tokio::spawn(async move {
            let resultat = servir(&appli, demande, requete, &sortie).await;
            actives.lock().unwrap().remove(&requete);
            let _ = sortie
                .send(AgentVersRelais::FinAppli {
                    requete,
                    erreur: resultat.err(),
                })
                .await;
        });
        en_cours.insert(requete, tache.abort_handle());
    }

    /// Coupe une requête (le navigateur est parti) : la connexion à l'application est fermée.
    pub fn annuler(&self, requete: NumeroRequete) {
        if let Some(tache) = self.actives.lock().unwrap().remove(&requete) {
            tache.abort();
        }
    }
}

impl Drop for Applis {
    fn drop(&mut self) {
        for (_, tache) in self.actives.lock().unwrap().drain() {
            tache.abort();
        }
    }
}

fn fin_immediate(sortie: mpsc::Sender<AgentVersRelais>, requete: NumeroRequete, raison: &str) {
    let raison = raison.to_owned();
    tokio::spawn(async move {
        let _ = sortie
            .send(AgentVersRelais::FinAppli {
                requete,
                erreur: Some(raison),
            })
            .await;
    });
}

/// Une requête demandée par le relais.
pub struct Demande {
    pub appli: String,
    pub methode: String,
    pub chemin: String,
    pub entetes: EntetesHttp,
    pub corps: Vec<u8>,
}

/// Chemin acceptable : forme d'origine (`/…`), caractères ASCII visibles seulement (aucun
/// retour à la ligne ni espace : pas d'injection d'en-tête), pas de remontée `..`.
fn chemin_valide(chemin: &str) -> bool {
    chemin.starts_with('/')
        && !chemin.starts_with("//")
        && chemin
            .bytes()
            .all(|c| (0x21..=0x7e).contains(&c) && c != b'\\')
        && !chemin
            .split('?')
            .next()
            .unwrap_or("")
            .split('/')
            .any(|segment| segment == ".." || segment.eq_ignore_ascii_case("%2e%2e"))
}

/// Fait la requête et renvoie la réponse morceau par morceau. `Err` : raison de l'échec.
async fn servir(
    appli: &ConfigAppli,
    demande: Demande,
    requete: NumeroRequete,
    sortie: &mpsc::Sender<AgentVersRelais>,
) -> Result<(), String> {
    let methode = METHODES
        .contains(&demande.methode.as_str())
        .then(|| Method::from_bytes(demande.methode.as_bytes()).ok())
        .flatten()
        .ok_or("méthode non permise")?;
    if !chemin_valide(&demande.chemin) {
        return Err("chemin refusé".into());
    }
    let mut entetes = HeaderMap::new();
    for (nom, valeur) in &demande.entetes {
        let nom = nom.to_ascii_lowercase();
        if REFUSES.contains(&nom.as_str()) {
            continue;
        }
        if let (Ok(nom), Ok(valeur)) = (
            HeaderName::from_bytes(nom.as_bytes()),
            HeaderValue::from_str(valeur),
        ) {
            entetes.append(nom, valeur);
        }
    }
    // Les en-têtes de la configuration remplacent ceux du navigateur.
    for (nom, valeur) in &appli.entetes {
        if let (Ok(nom), Ok(valeur)) = (
            HeaderName::from_bytes(nom.as_bytes()),
            HeaderValue::from_str(valeur),
        ) {
            entetes.insert(nom, valeur);
        }
    }
    entetes.insert(
        header::HOST,
        HeaderValue::from_str(&format!("127.0.0.1:{}", appli.port)).map_err(|e| e.to_string())?,
    );
    entetes.insert(header::CONNECTION, HeaderValue::from_static("close"));

    // Toujours 127.0.0.1 et le port déclaré : rien de ce que dit le relais n'y change.
    let flux = tokio::time::timeout(
        DELAI_CONNEXION,
        TcpStream::connect(("127.0.0.1", appli.port)),
    )
    .await
    .map_err(|_| "l'application ne répond pas (connexion)".to_owned())?
    .map_err(|e| format!("application injoignable ({e})"))?;
    let (mut envoi, connexion) =
        hyper::client::conn::http1::handshake::<_, Full<Bytes>>(TokioIo::new(flux))
            .await
            .map_err(|e| format!("échange avec l'application impossible ({e})"))?;
    let connexion = tokio::spawn(async move {
        let _ = connexion.await;
    });
    // La connexion est coupée quand la requête se termine ou est annulée.
    let _garde = ArretALaFin(connexion.abort_handle());

    let mut http = Request::builder()
        .method(methode)
        .uri(demande.chemin.as_str())
        .body(Full::new(Bytes::from(demande.corps)))
        .map_err(|e| format!("requête invalide ({e})"))?;
    *http.headers_mut() = entetes;
    let reponse = tokio::time::timeout(DELAI_ENTETE, envoi.send_request(http))
        .await
        .map_err(|_| "l'application ne répond pas".to_owned())?
        .map_err(|e| format!("l'application a coupé la connexion ({e})"))?;

    let (tete, mut corps) = reponse.into_parts();
    let entetes: EntetesHttp = tete
        .headers
        .iter()
        .filter(|(nom, _)| !REPONSE_REFUSES.contains(&nom.as_str()))
        .filter_map(|(nom, valeur)| {
            Some((nom.as_str().to_owned(), valeur.to_str().ok()?.to_owned()))
        })
        .collect();
    sortie
        .send(AgentVersRelais::EnteteAppli {
            requete,
            statut: tete.status.as_u16(),
            entetes,
        })
        .await
        .map_err(|_| "relais déconnecté".to_owned())?;
    // Chaque morceau part dès qu'il arrive : les évènements d'un flux ne sont pas retenus.
    while let Some(trame) = corps.frame().await {
        let trame = trame.map_err(|e| format!("réponse interrompue ({e})"))?;
        if let Ok(donnees) = trame.into_data() {
            for morceau in donnees.chunks(MORCEAU) {
                sortie
                    .send(AgentVersRelais::CorpsAppli {
                        requete,
                        donnees: morceau.to_vec(),
                    })
                    .await
                    .map_err(|_| "relais déconnecté".to_owned())?;
            }
        }
    }
    Ok(())
}

struct ArretALaFin(AbortHandle);

impl Drop for ArretALaFin {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    /// Mini serveur HTTP : lit la requête (jusqu'à la fin des en-têtes), la garde, répond.
    async fn serveur(reponse: &'static str) -> (u16, mpsc::Receiver<String>) {
        let ecoute = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = ecoute.local_addr().unwrap().port();
        let (envoi, reception) = mpsc::channel(8);
        tokio::spawn(async move {
            while let Ok((mut flux, _)) = ecoute.accept().await {
                let envoi = envoi.clone();
                tokio::spawn(async move {
                    let mut tampon = vec![0u8; 8192];
                    let mut recu = Vec::new();
                    while !recu.windows(4).any(|f| f == b"\r\n\r\n") {
                        let n = flux.read(&mut tampon).await.unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        recu.extend_from_slice(&tampon[..n]);
                    }
                    let _ = envoi
                        .send(String::from_utf8_lossy(&recu).into_owned())
                        .await;
                    let _ = flux.write_all(reponse.as_bytes()).await;
                    let _ = flux.shutdown().await;
                });
            }
        });
        (port, reception)
    }

    fn config(port: u16, entetes: &[(&str, &str)]) -> BTreeMap<String, ConfigAppli> {
        BTreeMap::from([(
            "essai".to_owned(),
            ConfigAppli {
                port,
                libelle: None,
                entetes: entetes
                    .iter()
                    .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
                    .collect(),
            },
        )])
    }

    fn demande(appli: &str, chemin: &str, entetes: &[(&str, &str)]) -> Demande {
        Demande {
            appli: appli.into(),
            methode: "GET".into(),
            chemin: chemin.into(),
            entetes: entetes
                .iter()
                .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
                .collect(),
            corps: Vec::new(),
        }
    }

    /// Rassemble la réponse d'une requête : (statut, en-têtes, corps, erreur).
    async fn lire(
        reception: &mut mpsc::Receiver<AgentVersRelais>,
    ) -> (u16, EntetesHttp, Vec<u8>, Option<String>) {
        let (mut statut, mut entetes, mut corps) = (0, Vec::new(), Vec::new());
        loop {
            match tokio::time::timeout(Duration::from_secs(5), reception.recv())
                .await
                .expect("réponse attendue")
                .expect("file ouverte")
            {
                AgentVersRelais::EnteteAppli {
                    statut: s,
                    entetes: e,
                    ..
                } => (statut, entetes) = (s, e),
                AgentVersRelais::CorpsAppli { donnees, .. } => corps.extend(donnees),
                AgentVersRelais::FinAppli { erreur, .. } => {
                    return (statut, entetes, corps, erreur);
                }
                autre => panic!("message inattendu : {autre:?}"),
            }
        }
    }

    #[tokio::test]
    async fn en_tetes_ajoutes_par_l_agent() {
        let (port, mut vues) = serveur(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nSet-Cookie: a=b\r\nContent-Length: 2\r\n\r\nok",
        )
        .await;
        let (applis, mut reception) = Applis::new(config(port, &[("Cookie", "cofiticket=jeton")]));
        applis.lancer(
            1,
            demande(
                "essai",
                "/api/etat?x=1",
                &[
                    ("Cookie", "__Host-ricochet=SESSION-RICOCHET"),
                    ("Authorization", "Bearer secret-ricochet"),
                    ("Accept", "application/json"),
                    ("Host", "ricochet.mawena.cloud"),
                ],
            ),
        );
        let (statut, entetes, corps, erreur) = lire(&mut reception).await;
        assert_eq!((statut, corps.as_slice(), erreur), (200, &b"ok"[..], None));
        // Le Set-Cookie de l'application ne repart pas.
        assert!(
            entetes
                .iter()
                .all(|(n, _)| !n.eq_ignore_ascii_case("set-cookie"))
        );
        assert!(
            entetes
                .iter()
                .any(|(n, v)| n == "content-type" && v == "text/plain")
        );
        let vue = vues.recv().await.unwrap().to_ascii_lowercase();
        assert!(vue.starts_with("get /api/etat?x=1 http/1.1"), "{vue}");
        // L'en-tête configuré est ajouté ; le cookie de session de Ricochet n'arrive pas.
        assert!(vue.contains("cookie: cofiticket=jeton"), "{vue}");
        assert!(
            !vue.contains("session-ricochet") && !vue.contains("authorization"),
            "{vue}"
        );
        assert!(vue.contains("accept: application/json"), "{vue}");
        assert!(vue.contains(&format!("host: 127.0.0.1:{port}")), "{vue}");
    }

    #[tokio::test]
    async fn application_non_declaree_et_port_hors_liste_refuses() {
        let (port_declare, _) = serveur("HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
        let (port_autre, mut vues_autre) =
            serveur("HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
        let (applis, mut reception) = Applis::new(config(port_declare, &[]));
        // Un nom inconnu, un port donné à la place du nom, une adresse : rien n'est joignable.
        for nom in [
            "autre",
            &port_autre.to_string(),
            &format!("127.0.0.1:{port_autre}"),
        ] {
            applis.lancer(7, demande(nom, "/", &[]));
            let (_, _, _, erreur) = lire(&mut reception).await;
            assert_eq!(
                erreur.as_deref(),
                Some("application non déclarée sur cette machine"),
                "{nom}"
            );
        }
        // Un chemin ne fait pas changer d'hôte non plus.
        for chemin in [
            "//127.0.0.1:1/x",
            "http://exemple.org/",
            "/a b",
            "/a\r\nX: 1",
            "/../etc",
            "x",
        ] {
            applis.lancer(8, demande("essai", chemin, &[]));
            let (_, _, _, erreur) = lire(&mut reception).await;
            assert_eq!(erreur.as_deref(), Some("chemin refusé"), "{chemin}");
        }
        let mut mauvaise = demande("essai", "/", &[]);
        mauvaise.methode = "CONNECT".into();
        applis.lancer(9, mauvaise);
        assert_eq!(
            lire(&mut reception).await.3.as_deref(),
            Some("méthode non permise")
        );
        // Le serveur du port non déclaré n'a reçu aucune connexion.
        assert!(
            tokio::time::timeout(Duration::from_millis(200), vues_autre.recv())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn annulation_coupe_la_connexion() {
        // Un flux qui ne finit jamais : l'annulation doit fermer la connexion à l'application.
        let ecoute = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = ecoute.local_addr().unwrap().port();
        let (ferme, mut ferme_recu) = mpsc::channel(1);
        tokio::spawn(async move {
            let (mut flux, _) = ecoute.accept().await.unwrap();
            let mut tampon = [0u8; 4096];
            let _ = flux.read(&mut tampon).await;
            flux.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\ndata: 1\n\n",
            )
            .await
            .unwrap();
            // Lecture jusqu'à la fermeture par l'agent.
            while flux.read(&mut tampon).await.unwrap_or(0) > 0 {}
            let _ = ferme.send(()).await;
        });
        let (applis, mut reception) = Applis::new(config(port, &[]));
        applis.lancer(3, demande("essai", "/flux", &[]));
        loop {
            if let AgentVersRelais::CorpsAppli { .. } = reception.recv().await.unwrap() {
                break;
            }
        }
        applis.annuler(3);
        tokio::time::timeout(Duration::from_secs(5), ferme_recu.recv())
            .await
            .expect("l'application doit voir la connexion fermée")
            .unwrap();
    }
}
