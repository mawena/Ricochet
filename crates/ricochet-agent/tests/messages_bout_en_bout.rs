//! Messages des machines, de bout en bout : un vrai relais, un vrai agent, le vrai socket, et un
//! faux service de push qui reçoit la requête et déchiffre le contenu.

use std::{
    collections::HashMap,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU16, Ordering},
    },
    time::Duration,
};

use p256::{
    SecretKey,
    elliptic_curve::{
        rand_core::{OsRng, RngCore},
        sec1::ToEncodedPoint,
    },
};
use ricochet_agent::{
    Config,
    config::ConfigMessages,
    connexion::session_avec,
    messages::{self, Messagerie},
    messages_client,
    systeme::Collecteur,
};
use ricochet_serveur::{
    Base, Relais, application, jeton,
    webpush::{base64url, dechiffrer},
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

const JETON_OPERATEUR: &str = "jeton-operateur";
const JETON_TUNNEL: &str = "jeton-tunnel";

/// Une requête reçue par le faux service de push.
#[derive(Debug, Clone)]
struct RequetePush {
    chemin: String,
    entetes: HashMap<String, String>,
    corps: Vec<u8>,
}

/// Faux service de push : garde les requêtes et répond avec le statut demandé.
struct FauxPush {
    port: u16,
    recues: Arc<Mutex<Vec<RequetePush>>>,
    statut: Arc<AtomicU16>,
}

async fn faux_push() -> FauxPush {
    let ecoute = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = ecoute.local_addr().unwrap().port();
    let recues = Arc::new(Mutex::new(Vec::new()));
    let statut = Arc::new(AtomicU16::new(201));
    let (r, s) = (recues.clone(), statut.clone());
    tokio::spawn(async move {
        loop {
            let (mut flux, _) = ecoute.accept().await.unwrap();
            let (r, s) = (r.clone(), s.clone());
            tokio::spawn(async move {
                let mut tampon = Vec::new();
                let mut bloc = [0u8; 4096];
                let fin_entetes = loop {
                    let n = flux.read(&mut bloc).await.unwrap();
                    assert!(n > 0, "requête incomplète");
                    tampon.extend_from_slice(&bloc[..n]);
                    if let Some(i) = tampon.windows(4).position(|f| f == b"\r\n\r\n") {
                        break i;
                    }
                };
                let tete = String::from_utf8_lossy(&tampon[..fin_entetes]).into_owned();
                let mut lignes = tete.lines();
                let chemin = lignes
                    .next()
                    .unwrap()
                    .split_whitespace()
                    .nth(1)
                    .unwrap()
                    .to_owned();
                let entetes: HashMap<String, String> = lignes
                    .filter_map(|l| l.split_once(':'))
                    .map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_owned()))
                    .collect();
                let longueur: usize = entetes["content-length"].parse().unwrap();
                let mut corps = tampon[fin_entetes + 4..].to_vec();
                while corps.len() < longueur {
                    let n = flux.read(&mut bloc).await.unwrap();
                    assert!(n > 0);
                    corps.extend_from_slice(&bloc[..n]);
                }
                r.lock().unwrap().push(RequetePush {
                    chemin,
                    entetes,
                    corps,
                });
                let reponse = format!(
                    "HTTP/1.1 {} X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    s.load(Ordering::Relaxed)
                );
                let _ = flux.write_all(reponse.as_bytes()).await;
            });
        }
    });
    FauxPush {
        port,
        recues,
        statut,
    }
}

/// Un appel HTTP à l'API du relais, avec le jeton donné.
async fn http(
    adresse: SocketAddr,
    jeton: &str,
    methode: &str,
    chemin: &str,
    corps: Option<Value>,
) -> (u16, String) {
    let mut flux = TcpStream::connect(adresse).await.unwrap();
    let corps = corps.map(|c| c.to_string()).unwrap_or_default();
    let requete = format!(
        "{methode} {chemin} HTTP/1.1\r\nHost: relais\r\nAuthorization: Bearer {jeton}\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{corps}",
        corps.len()
    );
    flux.write_all(requete.as_bytes()).await.unwrap();
    let mut reponse = Vec::new();
    flux.read_to_end(&mut reponse).await.unwrap();
    let texte = String::from_utf8_lossy(&reponse).into_owned();
    let (tete, mut corps) = texte.split_once("\r\n\r\n").unwrap();
    let statut = tete.split_whitespace().nth(1).unwrap().parse().unwrap();
    // Corps en morceaux (Transfer-Encoding: chunked) : recomposé.
    if tete
        .to_ascii_lowercase()
        .contains("transfer-encoding: chunked")
    {
        let mut brut = String::new();
        while let Some((taille, suite)) = corps.split_once("\r\n") {
            let n = usize::from_str_radix(taille.trim(), 16).unwrap();
            if n == 0 {
                break;
            }
            brut.push_str(&suite[..n]);
            corps = &suite[n + 2..];
        }
        return (statut, brut);
    }
    (statut, corps.to_owned())
}

async fn api(
    adresse: SocketAddr,
    methode: &str,
    chemin: &str,
    corps: Option<Value>,
) -> (u16, Value) {
    let (statut, texte) = http(adresse, JETON_OPERATEUR, methode, chemin, corps).await;
    (
        statut,
        serde_json::from_str(&texte).unwrap_or(Value::String(texte)),
    )
}

struct Banc {
    adresse: SocketAddr,
    relais: Arc<Relais>,
    url_agent: String,
    push: FauxPush,
    dossier: PathBuf,
    messagerie: Arc<Messagerie>,
    socket: PathBuf,
}

fn dossier_temporaire(nom: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ricochet-e2e-{nom}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn config_messages(dossier: &Path, utilisateurs: Vec<String>) -> ConfigMessages {
    ConfigMessages {
        utilisateurs,
        socket: dossier.join("messages.sock"),
        conservation_jours: 30,
        dossier_etat: dossier.join("etat"),
    }
}

fn nom_utilisateur_courant() -> String {
    // SAFETY : getpwuid renvoie un pointeur sur une zone statique, lue tout de suite.
    unsafe {
        let entree = libc::getpwuid(libc::geteuid());
        std::ffi::CStr::from_ptr((*entree).pw_name)
            .to_string_lossy()
            .into_owned()
    }
}

async fn demarrer(nom: &str) -> Banc {
    let push = faux_push().await;
    let config = ricochet_serveur::Config::depuis_texte(&format!(
        "intervalle_etat_s = 1\njeton_operateur_sha256 = \"{}\"\njeton_tunnel_sha256 = \"{}\"\n\
         [push]\nurl_publique = \"https://ricochet.test\"\nautoriser_http = true\n\
         hotes_autorises = [\"127.0.0.1\"]\n",
        jeton::empreinte(JETON_OPERATEUR),
        jeton::empreinte(JETON_TUNNEL)
    ))
    .unwrap();
    let base = Base::en_memoire();
    base.ajouter_machine(
        "pc",
        Some("PC du bureau"),
        &jeton::empreinte("secret-pc"),
        0,
    )
    .unwrap();
    let relais = Arc::new(Relais::new(config, base));
    let routeur = application(relais.clone());
    let auditeur = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let adresse = auditeur.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            auditeur,
            routeur.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    let dossier = dossier_temporaire(nom);
    let config = config_messages(&dossier, vec![nom_utilisateur_courant()]);
    let socket = config.socket.clone();
    let messagerie = Messagerie::ouvrir(&config).unwrap();
    let pour_socket = messagerie.clone();
    let chemin = socket.clone();
    tokio::spawn(async move { messages::servir(pour_socket, chemin).await.unwrap() });
    Banc {
        adresse,
        relais,
        url_agent: format!("ws://{adresse}/agent"),
        push,
        dossier,
        messagerie,
        socket,
    }
}

fn config_agent(banc: &Banc) -> Config {
    Config {
        relais: banc.url_agent.clone(),
        id: "pc".into(),
        jeton: "secret-pc".into(),
        terminal: None,
        fichiers: None,
        tunnel: None,
        messages: Some(config_messages(&banc.dossier, vec![])),
        applis: Default::default(),
    }
}

/// Lance une session de l'agent ; l'abandon de la tâche la coupe, comme une panne réseau.
fn lancer_agent(banc: &Banc) -> tokio::task::JoinHandle<()> {
    let config = config_agent(banc);
    let messagerie = banc.messagerie.clone();
    tokio::spawn(async move {
        let _ = session_avec(&config, &mut Collecteur::new().unwrap(), Some(&messagerie)).await;
    })
}

async fn attendre<F: std::future::Future<Output = bool>>(
    raison: &str,
    mut condition: impl FnMut() -> F,
) {
    for _ in 0..100 {
        if condition().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("délai dépassé : {raison}");
}

async fn attendre_agent(banc: &Banc, connecte: bool) {
    attendre("état de la connexion de l'agent", || async {
        banc.relais
            .registre
            .liste(&banc.relais.base.machines().unwrap())[0]
            .connectee
            == connecte
    })
    .await;
}

/// Une requête sur le socket de l'agent, comme le ferait CofiTicket.
async fn socket(banc: &Banc, requete: Value) -> Value {
    let chemin = banc.socket.clone();
    tokio::task::spawn_blocking(move || messages_client::appeler(&chemin, &requete).unwrap())
        .await
        .unwrap()
}

struct Appareil {
    secret: SecretKey,
    public: Vec<u8>,
    auth: [u8; 16],
}

fn nouvel_appareil() -> Appareil {
    let secret = SecretKey::random(&mut OsRng);
    let public = secret
        .public_key()
        .to_encoded_point(false)
        .as_bytes()
        .to_vec();
    let mut auth = [0u8; 16];
    OsRng.fill_bytes(&mut auth);
    Appareil {
        secret,
        public,
        auth,
    }
}

impl Appareil {
    fn abonnement(&self, port: u16, nom: &str) -> Value {
        json!({
            "endpoint": format!("http://127.0.0.1:{port}/push/{nom}"),
            "keys": { "p256dh": base64url(&self.public), "auth": base64url(&self.auth) },
            "appareil": nom,
        })
    }

    fn dechiffrer(&self, requete: &RequetePush) -> Value {
        let clair = dechiffrer(
            &requete.corps,
            &self.secret.to_bytes(),
            &self.public,
            &self.auth,
        )
        .expect("le contenu poussé doit se déchiffrer avec la clé de l'appareil");
        serde_json::from_slice(&clair).unwrap()
    }
}

#[tokio::test]
async fn messages_de_bout_en_bout() {
    let banc = demarrer("complet").await;
    let appareil = nouvel_appareil();

    // Le jeton des tunnels n'ouvre rien de tout cela ; sans jeton : refus.
    for (methode, chemin) in [
        ("GET", "/api/messages"),
        ("GET", "/api/push/cle"),
        ("GET", "/api/push/abonnements"),
        ("POST", "/api/push/essai"),
    ] {
        let (statut, _) = http(banc.adresse, JETON_TUNNEL, methode, chemin, None).await;
        assert_eq!(statut, 403, "{methode} {chemin} avec le jeton des tunnels");
        let (statut, _) = http(banc.adresse, "faux", methode, chemin, None).await;
        assert_eq!(statut, 401, "{methode} {chemin} sans bon jeton");
    }

    // Abonnement de l'appareil : la clé publique VAPID vient du relais.
    let (statut, cle) = api(banc.adresse, "GET", "/api/push/cle", None).await;
    assert_eq!(statut, 200);
    let cle_vapid = cle["cle"].as_str().unwrap().to_owned();
    assert_eq!(cle_vapid, base64url(banc.relais.push.vapid.publique()));
    // Un abonnement vers un hôte non autorisé est refusé.
    let mut mauvais = appareil.abonnement(banc.push.port, "x");
    mauvais["endpoint"] = json!("http://localhost:9/push/x");
    assert_eq!(
        api(banc.adresse, "POST", "/api/push/abonnements", Some(mauvais))
            .await
            .0,
        400
    );
    let (statut, _) = api(
        banc.adresse,
        "POST",
        "/api/push/abonnements",
        Some(appareil.abonnement(banc.push.port, "iPhone")),
    )
    .await;
    assert_eq!(statut, 201);

    // Message envoyé AVANT la connexion de l'agent : il attend dans la file de l'agent.
    let reponse = socket(
        &banc,
        json!({"op": "envoyer", "source": "cofiticket", "cle": "R-1:attente",
               "titre": "R-1 : compte dormant", "texte": "Renvoyer vers les Opérations",
               "genre": "action", "boutons": [{"id": "resoudre", "libelle": "Résoudre"}],
               "detail": "ligne 1\nligne 2 : donnée client très confidentielle"}),
    )
    .await;
    assert_eq!(reponse["ok"], true, "{reponse}");
    let id = reponse["id"].as_str().unwrap().to_owned();
    assert_eq!(socket(&banc, json!({"op": "etat"})).await["en_attente"], 1);
    assert_eq!(
        socket(&banc, json!({"op": "etat"})).await["relais_connecte"],
        false
    );

    let agent = lancer_agent(&banc);
    attendre_agent(&banc, true).await;
    // La capacité est annoncée au relais.
    let (_, machines) = api(banc.adresse, "GET", "/api/machines", None).await;
    assert_eq!(machines[0]["info"]["messages"], true);

    // Le relais l'a reçu et accusé ; un doublon (source, cle) n'en crée pas un second.
    attendre("message reçu par le relais", || async {
        api(banc.adresse, "GET", "/api/messages", None).await.1["messages"]
            .as_array()
            .is_some_and(|l| l.len() == 1)
    })
    .await;
    attendre("accusé de réception à l'agent", || async {
        banc.messagerie.etat()["en_attente"] == 0
    })
    .await;
    let doublon = socket(
        &banc,
        json!({"op": "envoyer", "source": "cofiticket", "cle": "R-1:attente", "titre": "autre"}),
    )
    .await;
    assert_eq!(doublon["id"], id.as_str());

    let (_, liste) = api(banc.adresse, "GET", "/api/messages", None).await;
    assert_eq!(liste["non_lus"], 1);
    let message = &liste["messages"][0];
    assert_eq!(message["id"], id.as_str());
    assert_eq!(message["machine"], "pc");
    assert_eq!(message["source"], "cofiticket");
    assert_eq!(message["genre"], "action");
    assert_eq!(message["detail"], true);
    assert_eq!(message["boutons"][0]["libelle"], "Résoudre");
    // Le détail n'est nulle part dans ce que le relais garde ni dans ce qu'il liste.
    assert!(!liste.to_string().contains("confidentielle"));
    let (_, un) = api(banc.adresse, "GET", &format!("/api/messages/{id}"), None).await;
    assert!(!un.to_string().contains("confidentielle"));
    let (_, relu) = api(banc.adresse, "GET", &format!("/api/messages/{id}"), None).await;
    assert!(relu["lu_le"].is_u64());

    // Notification : déchiffrée par l'appareil, avec seulement les cinq champs permis.
    attendre("notification poussée", || async {
        !banc.push.recues.lock().unwrap().is_empty()
    })
    .await;
    let requete = banc.push.recues.lock().unwrap()[0].clone();
    assert_eq!(requete.chemin, "/push/iPhone");
    assert_eq!(requete.entetes["content-encoding"], "aes128gcm");
    assert_eq!(requete.entetes["ttl"], "86400");
    assert_eq!(requete.entetes["urgency"], "normal");
    let autorisation = &requete.entetes["authorization"];
    assert!(autorisation.starts_with("vapid t="), "{autorisation}");
    assert!(autorisation.ends_with(&format!(", k={cle_vapid}")));
    let jwt = autorisation["vapid t=".len()..].split(',').next().unwrap();
    let revendications: Value = serde_json::from_slice(
        &ricochet_serveur::webpush::decoder_base64url(jwt.split('.').nth(1).unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(revendications["sub"], "https://ricochet.test");
    assert_eq!(
        revendications["aud"],
        format!("http://127.0.0.1:{}", banc.push.port)
    );
    let contenu = appareil.dechiffrer(&requete);
    assert_eq!(
        contenu,
        json!({"id": id, "titre": "R-1 : compte dormant", "texte": "Renvoyer vers les Opérations",
               "machine": "PC du bureau", "genre": "action"})
    );
    assert!(!String::from_utf8_lossy(&requete.corps).contains("confidentielle"));

    // Détail : demandé à l'agent au moment de l'affichage.
    let (statut, texte) = http(
        banc.adresse,
        JETON_OPERATEUR,
        "GET",
        &format!("/api/messages/{id}/detail"),
        None,
    )
    .await;
    assert_eq!(statut, 200);
    assert_eq!(
        texte,
        "ligne 1\nligne 2 : donnée client très confidentielle"
    );
    let (statut, _) = http(
        banc.adresse,
        JETON_OPERATEUR,
        "GET",
        "/api/messages/inconnu/detail",
        None,
    )
    .await;
    assert_eq!(statut, 404);

    // Réponse à un bouton : une seule fois ; l'agent la rend une seule fois.
    let chemin_reponse = format!("/api/messages/{id}/reponse");
    assert_eq!(
        api(
            banc.adresse,
            "POST",
            &chemin_reponse,
            Some(json!({"bouton": "autre"}))
        )
        .await
        .0,
        400
    );
    let (statut, corps) = api(
        banc.adresse,
        "POST",
        &chemin_reponse,
        Some(json!({"bouton": "resoudre"})),
    )
    .await;
    assert_eq!((statut, &corps["transmise"]), (200, &json!(true)));
    assert_eq!(
        api(
            banc.adresse,
            "POST",
            &chemin_reponse,
            Some(json!({"bouton": "resoudre"}))
        )
        .await
        .0,
        409
    );
    let reponses = {
        let mut rendues = Value::Null;
        for _ in 0..50 {
            let r = socket(&banc, json!({"op": "reponses", "source": "cofiticket"})).await;
            if !r["reponses"].as_array().unwrap().is_empty() {
                rendues = r;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        rendues
    };
    assert_eq!(reponses["reponses"][0]["id"], id.as_str());
    assert_eq!(reponses["reponses"][0]["bouton"], "resoudre");
    assert_eq!(reponses["reponses"][0]["par"], "jeton-api");
    assert!(
        reponses["reponses"][0]["quand"]
            .as_str()
            .unwrap()
            .ends_with('Z')
    );
    let encore = socket(&banc, json!({"op": "reponses", "source": "cofiticket"})).await;
    assert!(encore["reponses"].as_array().unwrap().is_empty());
    // L'accusé de l'agent est revenu au relais.
    attendre("accusé de la réponse", || async {
        banc.relais.base.reponses_a_livrer("pc").unwrap().is_empty()
    })
    .await;

    // Coupure : agent hors ligne. Détail impossible (erreur claire), réponse gardée par le relais.
    let second = socket(
        &banc,
        json!({"op": "envoyer", "source": "cofiticket", "titre": "second",
               "boutons": [{"id": "ok", "libelle": "OK"}], "detail": "détail du second"}),
    )
    .await["id"]
        .as_str()
        .unwrap()
        .to_owned();
    attendre("second message au relais", || async {
        banc.relais.base.message(&second).unwrap().is_some()
    })
    .await;
    agent.abort();
    let _ = agent.await;
    attendre_agent(&banc, false).await;
    let (statut, texte) = http(
        banc.adresse,
        JETON_OPERATEUR,
        "GET",
        &format!("/api/messages/{second}/detail"),
        None,
    )
    .await;
    assert_eq!(statut, 409);
    assert!(texte.contains("hors ligne"), "{texte}");
    let (statut, corps) = api(
        banc.adresse,
        "POST",
        &format!("/api/messages/{second}/reponse"),
        Some(json!({"bouton": "ok"})),
    )
    .await;
    assert_eq!((statut, &corps["transmise"]), (200, &json!(false)));
    // Message envoyé pendant la coupure : gardé par l'agent.
    let troisieme = socket(
        &banc,
        json!({"op": "envoyer", "source": "ccs", "titre": "pendant la coupure", "genre": "alerte"}),
    )
    .await;
    assert_eq!(troisieme["ok"], true);
    assert_eq!(
        socket(&banc, json!({"op": "etat"})).await["relais_connecte"],
        false
    );
    assert_eq!(socket(&banc, json!({"op": "etat"})).await["en_attente"], 1);

    // Reconnexion : la réponse est livrée, le message en attente est envoyé.
    let _agent = lancer_agent(&banc);
    attendre("réponse livrée à la reconnexion", || async {
        let r = socket(&banc, json!({"op": "reponses", "source": "cofiticket"})).await;
        r["reponses"]
            .as_array()
            .unwrap()
            .first()
            .is_some_and(|x| x["id"] == second.as_str() && x["bouton"] == "ok")
    })
    .await;
    attendre("message de la coupure reçu", || async {
        banc.relais
            .base
            .message(troisieme["id"].as_str().unwrap())
            .unwrap()
            .is_some()
    })
    .await;
    attendre("plus rien en attente", || async {
        socket(&banc, json!({"op": "etat"})).await["en_attente"] == 0
    })
    .await;
    // Le genre « alerte » part en urgence haute.
    attendre("notification d'alerte", || async {
        banc.push
            .recues
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.entetes["urgency"] == "high")
    })
    .await;

    // Notification d'essai, puis abonnement périmé (410) supprimé par le relais.
    let avant = banc.push.recues.lock().unwrap().len();
    let (statut, bilan) = api(banc.adresse, "POST", "/api/push/essai", None).await;
    assert_eq!((statut, bilan["envoyes"].clone()), (200, json!(1)));
    let essai = banc.push.recues.lock().unwrap()[avant].clone();
    assert_eq!(appareil.dechiffrer(&essai)["titre"], "Notification d'essai");
    let (_, abonnements) = api(banc.adresse, "GET", "/api/push/abonnements", None).await;
    assert_eq!(abonnements.as_array().unwrap().len(), 1);
    assert!(
        abonnements[0].get("endpoint").is_none(),
        "l'adresse reste secrète"
    );
    banc.push.statut.store(410, Ordering::Relaxed);
    let (_, bilan) = api(banc.adresse, "POST", "/api/push/essai", None).await;
    assert_eq!(bilan["supprimes"], 1);
    let (_, abonnements) = api(banc.adresse, "GET", "/api/push/abonnements", None).await;
    assert!(abonnements.as_array().unwrap().is_empty());

    // Suppression manuelle d'un appareil.
    banc.push.statut.store(201, Ordering::Relaxed);
    let (_, ajoute) = api(
        banc.adresse,
        "POST",
        "/api/push/abonnements",
        Some(appareil.abonnement(banc.push.port, "Chrome")),
    )
    .await;
    let numero = ajoute["id"].as_i64().unwrap();
    assert_eq!(
        api(
            banc.adresse,
            "DELETE",
            &format!("/api/push/abonnements/{numero}"),
            None
        )
        .await
        .0,
        204
    );
    assert_eq!(
        api(
            banc.adresse,
            "DELETE",
            &format!("/api/push/abonnements/{numero}"),
            None
        )
        .await
        .0,
        404
    );

    // Tout est dans le journal d'audit.
    let journal: Vec<String> = banc
        .relais
        .base
        .journal(200)
        .unwrap()
        .into_iter()
        .map(|l| l.action)
        .collect();
    for attendu in [
        "message_recu",
        "message_detail",
        "message_reponse",
        "push_abonnement_ajoute",
        "push_abonnement_retire",
        "push_essai",
    ] {
        assert!(
            journal.iter().any(|a| a == attendu),
            "{attendu} absent de {journal:?}"
        );
    }
    let _ = std::fs::remove_dir_all(&banc.dossier);
}

#[tokio::test]
async fn socket_refuse_a_un_compte_non_liste() {
    // Root passe toujours : le refus ne se vérifie pas en tant que root.
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("exécuté en root : essai ignoré");
        return;
    }
    let dossier = dossier_temporaire("uid");
    let config = config_messages(&dossier, vec!["nobody".into()]);
    let messagerie = Messagerie::ouvrir(&config).unwrap();
    let chemin = config.socket.clone();
    tokio::spawn(async move { messages::servir(messagerie, chemin).await.unwrap() });
    for _ in 0..50 {
        if config.socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let chemin = config.socket.clone();
    let reponse = tokio::task::spawn_blocking(move || {
        messages_client::appeler(
            &chemin,
            &json!({"op": "envoyer", "source": "x", "titre": "t"}),
        )
        .unwrap()
    })
    .await
    .unwrap();
    assert_eq!(reponse["ok"], false);
    assert!(
        reponse["erreur"].as_str().unwrap().contains("refusé"),
        "{reponse}"
    );
    // Droits du socket : 0666, la liste fait foi.
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&config.socket)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o666
    );
    let _ = std::fs::remove_dir_all(&dossier);
}

#[tokio::test]
async fn requete_trop_grande_et_illisible() {
    let banc = demarrer("limites").await;
    // Une ligne de plus de 96 Kio est refusée sans être lue en entier.
    let chemin = banc.socket.clone();
    let reponse = tokio::task::spawn_blocking(move || {
        use std::io::{Read, Write};
        let mut flux = std::os::unix::net::UnixStream::connect(chemin).unwrap();
        let mut ligne = vec![b'a'; 200 * 1024];
        ligne.push(b'\n');
        let _ = flux.write_all(&ligne);
        let mut sortie = String::new();
        let _ = flux.read_to_string(&mut sortie);
        sortie
    })
    .await
    .unwrap();
    assert!(reponse.contains("trop grande"), "{reponse}");
    let (_, texte) = (0, socket(&banc, json!({"op": "reponses"})).await);
    assert_eq!(texte["ok"], false);
    let _ = std::fs::remove_dir_all(&banc.dossier);
}
