//! Applications web des machines : un vrai relais, un vrai agent et une fausse application
//! locale (un petit serveur HTTP), avec un navigateur simulé (cookie de session de Ricochet).

use std::{
    collections::BTreeMap,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{
    Router,
    body::{Body, Bytes},
    extract::Request,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use http_body_util::{BodyExt, Full};
use hyper_util::rt::TokioIo;
use ricochet_agent::{Config, config::ConfigAppli, connexion::session, systeme::Collecteur};
use ricochet_serveur::{Base, Relais, application, jeton};
use tokio::sync::Notify;

const SESSION: &str = "valeur-secrete-de-session-ricochet";
const JETON_APPLI: &str = "jeton-secret-de-cofiticket";
const HOTE: &str = "ricochet.essai";

/// Ce que la fausse application a reçu.
#[derive(Debug, Clone)]
struct Vue {
    chemin: String,
    entetes: HeaderMap,
    corps: Vec<u8>,
}

struct FausseAppli {
    port: u16,
    vues: Arc<Mutex<Vec<Vue>>>,
    /// Libère le second évènement du flux.
    suite: Arc<Notify>,
}

async fn noter(vues: &Mutex<Vec<Vue>>, requete: Request) -> Vue {
    let (parties, corps) = requete.into_parts();
    let corps = corps.collect().await.unwrap().to_bytes().to_vec();
    let vue = Vue {
        chemin: parties.uri.to_string(),
        entetes: parties.headers,
        corps,
    };
    vues.lock().unwrap().push(vue.clone());
    vue
}

async fn fausse_appli() -> FausseAppli {
    let vues = Arc::new(Mutex::new(Vec::new()));
    let suite = Arc::new(Notify::new());
    let (v1, v2, v3, s) = (vues.clone(), vues.clone(), vues.clone(), suite.clone());
    let routeur = Router::new()
        .route(
            "/api/etat",
            get(move |requete: Request| async move {
                noter(&v1, requete).await;
                (
                    [
                        (header::SET_COOKIE, "pose-par-appli=1; Path=/"),
                        (header::CONTENT_TYPE, "application/json"),
                    ],
                    r#"{"ok":true}"#,
                )
            }),
        )
        .route(
            "/api/ticket/R/1/resoudre",
            post(move |requete: Request| async move {
                let vue = noter(&v2, requete).await;
                (StatusCode::CREATED, vue.corps)
            }),
        )
        .route(
            "/redirige",
            get(|| async {
                (StatusCode::FOUND, [(header::LOCATION, "/api/etat")]).into_response()
            }),
        )
        .route(
            "/flux",
            get(move |requete: Request| async move {
                noter(&v3, requete).await;
                let flux = futures_util::stream::unfold(0, move |etape| {
                    let s = s.clone();
                    async move {
                        match etape {
                            0 => Some((
                                Ok::<_, std::io::Error>(Bytes::from("event: etat\ndata: 1\n\n")),
                                1,
                            )),
                            1 => {
                                s.notified().await;
                                Some((Ok(Bytes::from("event: etat\ndata: 2\n\n")), 2))
                            }
                            _ => None,
                        }
                    }
                });
                Response::builder()
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .body(Body::from_stream(flux))
                    .unwrap()
            }),
        );
    let ecoute = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = ecoute.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(ecoute, routeur).await.unwrap();
    });
    FausseAppli { port, vues, suite }
}

fn dossier_temporaire(nom: &str) -> PathBuf {
    let nano = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dossier = std::env::temp_dir().join(format!(
        "ricochet-applis-{nom}-{}-{nano}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dossier).unwrap();
    dossier
}

struct Banc {
    relais: Arc<Relais>,
    adresse: SocketAddr,
    url_agent: String,
}

/// Relais avec une base dans `base` (un fichier : on peut y chercher des données) et un
/// opérateur « charles » dont la session est ouverte.
async fn relais(base: Option<&Path>) -> Banc {
    let config = ricochet_serveur::Config::depuis_texte(&format!(
        "intervalle_etat_s = 1\njeton_operateur_sha256 = \"{}\"",
        jeton::empreinte("jeton-operateur")
    ))
    .unwrap();
    let base = match base {
        Some(chemin) => Base::ouvrir(chemin).unwrap(),
        None => Base::en_memoire(),
    };
    base.ajouter_machine("pc", None, &jeton::empreinte("secret-pc"), 0)
        .unwrap();
    base.ajouter_machine("vieille", None, &jeton::empreinte("secret-vieille"), 0)
        .unwrap();
    base.creer_compte("charles", "x", "y", 0).unwrap();
    let maintenant = ricochet_serveur::registre::maintenant();
    base.creer_session(
        &jeton::empreinte(SESSION),
        "charles",
        maintenant,
        maintenant + 3600,
        "127.0.0.1",
    )
    .unwrap();
    let relais = Arc::new(Relais::new(config, base));
    let routeur = application(relais.clone());
    let ecoute = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let adresse = ecoute.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            ecoute,
            routeur.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    Banc {
        relais,
        adresse,
        url_agent: format!("ws://{adresse}/agent"),
    }
}

fn config_agent(
    banc: &Banc,
    id: &str,
    jeton: &str,
    applis: BTreeMap<String, ConfigAppli>,
) -> Config {
    Config {
        relais: banc.url_agent.clone(),
        id: id.into(),
        jeton: jeton.into(),
        terminal: None,
        fichiers: None,
        tunnel: None,
        messages: None,
        applis,
    }
}

fn appli_cofiticket(port: u16) -> BTreeMap<String, ConfigAppli> {
    BTreeMap::from([(
        "cofiticket".to_owned(),
        ConfigAppli {
            port,
            libelle: Some("CofiTicket".into()),
            entetes: BTreeMap::from([("Cookie".to_owned(), format!("cofiticket={JETON_APPLI}"))]),
        },
    )])
}

fn lancer_agent(config: Config) {
    tokio::spawn(async move { session(&config, &mut Collecteur::new().unwrap()).await });
}

async fn attendre(banc: &Banc, id: &str) {
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let machines = banc
            .relais
            .registre
            .liste(&banc.relais.base.machines().unwrap());
        if machines.iter().any(|m| m.id == id && m.connectee) {
            return;
        }
    }
    panic!("l'agent {id} ne s'est pas connecté");
}

/// Un navigateur simulé : requête HTTP au relais, avec le cookie de session.
async fn requete(
    banc: &Banc,
    methode: &str,
    chemin: &str,
    entetes: &[(&str, &str)],
    corps: &str,
) -> (StatusCode, HeaderMap, hyper::body::Incoming) {
    let flux = tokio::net::TcpStream::connect(banc.adresse).await.unwrap();
    let (mut envoi, connexion) =
        hyper::client::conn::http1::handshake::<_, Full<Bytes>>(TokioIo::new(flux))
            .await
            .unwrap();
    tokio::spawn(connexion);
    let mut construction = hyper::Request::builder()
        .method(methode)
        .uri(chemin)
        .header("host", HOTE)
        .header("cookie", format!("__Host-ricochet={SESSION}; autre=1"));
    for (nom, valeur) in entetes {
        construction = construction.header(*nom, *valeur);
    }
    let reponse = envoi
        .send_request(
            construction
                .body(Full::new(Bytes::from(corps.to_owned())))
                .unwrap(),
        )
        .await
        .unwrap();
    let (tete, corps) = reponse.into_parts();
    (tete.status, tete.headers, corps)
}

async fn texte(corps: hyper::body::Incoming) -> String {
    String::from_utf8_lossy(&corps.collect().await.unwrap().to_bytes()).into_owned()
}

async fn journal(banc: &Banc) -> Vec<(String, String)> {
    banc.relais
        .base
        .journal(100)
        .unwrap()
        .into_iter()
        .map(|l| (l.action, l.detail))
        .collect()
}

#[tokio::test]
async fn requete_transmise_avec_les_en_tetes_de_l_agent() {
    let appli = fausse_appli().await;
    let banc = relais(None).await;
    lancer_agent(config_agent(
        &banc,
        "pc",
        "secret-pc",
        appli_cofiticket(appli.port),
    ));
    attendre(&banc, "pc").await;

    // Le relais connaît le nom et le libellé, jamais le port ni le jeton.
    let machines = banc
        .relais
        .registre
        .liste(&banc.relais.base.machines().unwrap());
    let info = machines[0].info.clone().unwrap();
    assert_eq!(info.applis.len(), 1);
    assert_eq!(info.applis[0].nom, "cofiticket");
    let vue = serde_json::to_string(&machines[0]).unwrap();
    assert!(
        !vue.contains(JETON_APPLI) && !vue.contains(&appli.port.to_string()),
        "{vue}"
    );

    let (statut, entetes, corps) = requete(
        &banc,
        "GET",
        "/api/machines/pc/applis/cofiticket/api/etat?a=1",
        &[
            ("accept", "application/json"),
            ("authorization", "Bearer jeton-operateur"),
        ],
        "",
    )
    .await;
    assert_eq!(statut, 200);
    assert_eq!(texte(corps).await, r#"{"ok":true}"#);
    // Le Set-Cookie de l'application est retiré.
    assert!(entetes.get(header::SET_COOKIE).is_none(), "{entetes:?}");
    assert_eq!(entetes[header::CONTENT_TYPE], "application/json");
    assert_eq!(entetes["x-accel-buffering"], "no");
    assert!(entetes.contains_key(header::CONTENT_SECURITY_POLICY));

    let vues = appli.vues.lock().unwrap().clone();
    assert_eq!(vues.len(), 1);
    assert_eq!(vues[0].chemin, "/api/etat?a=1");
    // L'agent a ajouté le jeton ; le cookie de session de Ricochet n'est pas arrivé.
    assert_eq!(
        vues[0].entetes["cookie"],
        format!("cofiticket={JETON_APPLI}").as_str()
    );
    let tout = format!("{:?}", vues[0].entetes);
    assert!(
        !tout.contains(SESSION) && !tout.contains("jeton-operateur"),
        "{tout}"
    );
    assert!(!tout.contains("autre=1"), "{tout}");
    assert_eq!(vues[0].entetes["accept"], "application/json");
    assert_eq!(
        vues[0].entetes["host"],
        format!("127.0.0.1:{}", appli.port).as_str()
    );
}

#[tokio::test]
async fn post_exige_l_origine_du_relais_et_transmet_le_corps() {
    let appli = fausse_appli().await;
    let banc = relais(None).await;
    lancer_agent(config_agent(
        &banc,
        "pc",
        "secret-pc",
        appli_cofiticket(appli.port),
    ));
    attendre(&banc, "pc").await;
    let url = "/api/machines/pc/applis/cofiticket/api/ticket/R/1/resoudre";

    // Sans origine, ou avec celle d'un autre site : refusé, l'application ne voit rien.
    for entetes in [
        vec![],
        vec![("origin", "https://evil.example")],
        vec![("origin", "null")],
    ] {
        let (statut, _, _) = requete(&banc, "POST", url, &entetes, "{}").await;
        assert_eq!(statut, 403, "{entetes:?}");
    }
    assert!(appli.vues.lock().unwrap().is_empty());

    let origine = format!("https://{HOTE}");
    let (statut, _, corps) = requete(
        &banc,
        "POST",
        url,
        &[
            ("origin", origine.as_str()),
            ("content-type", "application/json"),
        ],
        r#"{"solution":"ok"}"#,
    )
    .await;
    assert_eq!(statut, 201);
    assert_eq!(texte(corps).await, r#"{"solution":"ok"}"#);
    let vues = appli.vues.lock().unwrap().clone();
    assert_eq!(vues.len(), 1);
    // L'origine du relais est transmise : c'est elle que l'application compare à sa liste.
    assert_eq!(vues[0].entetes["origin"], origine.as_str());
    assert_eq!(vues[0].entetes["content-type"], "application/json");
}

#[tokio::test]
async fn application_non_declaree_ou_agent_ancien_refuse() {
    let appli = fausse_appli().await;
    let banc = relais(None).await;
    lancer_agent(config_agent(
        &banc,
        "pc",
        "secret-pc",
        appli_cofiticket(appli.port),
    ));
    // Un agent sans section [applis] (comme avant cette fonction).
    lancer_agent(config_agent(
        &banc,
        "vieille",
        "secret-vieille",
        BTreeMap::new(),
    ));
    attendre(&banc, "pc").await;
    attendre(&banc, "vieille").await;

    for chemin in [
        "/api/machines/pc/applis/autre/api/etat".to_owned(),
        // Un numéro de port à la place du nom : aucune application de ce nom.
        format!("/api/machines/pc/applis/{}/api/etat", appli.port),
        "/api/machines/vieille/applis/cofiticket/api/etat".to_owned(),
        "/api/machines/inconnue/applis/cofiticket/api/etat".to_owned(),
    ] {
        let (statut, _, _) = requete(&banc, "GET", &chemin, &[], "").await;
        assert_eq!(statut, 404, "{chemin}");
    }
    assert!(appli.vues.lock().unwrap().is_empty());
    // Sans session : refusé, rien ne passe.
    let flux = tokio::net::TcpStream::connect(banc.adresse).await.unwrap();
    let (mut envoi, connexion) =
        hyper::client::conn::http1::handshake::<_, Full<Bytes>>(TokioIo::new(flux))
            .await
            .unwrap();
    tokio::spawn(connexion);
    let reponse = envoi
        .send_request(
            hyper::Request::builder()
                .uri("/api/machines/pc/applis/cofiticket/api/etat")
                .header("host", HOTE)
                .body(Full::new(Bytes::new()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(reponse.status(), 401);
    assert!(appli.vues.lock().unwrap().is_empty());
    // Les refus sont à l'audit.
    assert!(
        journal(&banc)
            .await
            .iter()
            .any(|(a, d)| a == "appli_refusee" && d == "vieille:cofiticket")
    );
}

#[tokio::test]
async fn flux_d_evenements_transmis_au_fil_de_l_eau() {
    let appli = fausse_appli().await;
    let banc = relais(None).await;
    lancer_agent(config_agent(
        &banc,
        "pc",
        "secret-pc",
        appli_cofiticket(appli.port),
    ));
    attendre(&banc, "pc").await;

    let (statut, entetes, mut corps) = requete(
        &banc,
        "GET",
        "/api/machines/pc/applis/cofiticket/flux",
        &[("accept", "text/event-stream")],
        "",
    )
    .await;
    assert_eq!(statut, 200);
    assert_eq!(entetes[header::CONTENT_TYPE], "text/event-stream");
    // Le premier évènement arrive alors que le flux n'est pas fini : l'application attend
    // que le test la libère pour envoyer le second.
    let premier = tokio::time::timeout(Duration::from_secs(5), corps.frame())
        .await
        .expect("le premier évènement doit arriver sans attendre la fin")
        .unwrap()
        .unwrap()
        .into_data()
        .unwrap();
    assert_eq!(&premier[..], b"event: etat\ndata: 1\n\n");
    appli.suite.notify_one();
    let second = tokio::time::timeout(Duration::from_secs(5), corps.frame())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .into_data()
        .unwrap();
    assert_eq!(&second[..], b"event: etat\ndata: 2\n\n");
    assert!(corps.frame().await.is_none());
}

#[tokio::test]
async fn redirection_reecrite_sous_le_prefixe() {
    let appli = fausse_appli().await;
    let banc = relais(None).await;
    lancer_agent(config_agent(
        &banc,
        "pc",
        "secret-pc",
        appli_cofiticket(appli.port),
    ));
    attendre(&banc, "pc").await;
    let (statut, entetes, _) = requete(
        &banc,
        "GET",
        "/api/machines/pc/applis/cofiticket/redirige",
        &[],
        "",
    )
    .await;
    assert_eq!(statut, 302);
    assert_eq!(
        entetes[header::LOCATION],
        "/api/machines/pc/applis/cofiticket/api/etat"
    );
    // Sans barre finale : redirigé vers la forme avec barre (les adresses relatives en dépendent).
    let (statut, entetes, _) = requete(
        &banc,
        "GET",
        "/api/machines/pc/applis/cofiticket?x=1",
        &[],
        "",
    )
    .await;
    assert_eq!(statut, 308);
    assert_eq!(
        entetes[header::LOCATION],
        "/api/machines/pc/applis/cofiticket/?x=1"
    );
}

#[tokio::test]
async fn rien_d_ecrit_par_le_relais_hors_audit() {
    let appli = fausse_appli().await;
    let dossier = dossier_temporaire("disque");
    let banc = relais(Some(&dossier.join("ricochet.db"))).await;
    lancer_agent(config_agent(
        &banc,
        "pc",
        "secret-pc",
        appli_cofiticket(appli.port),
    ));
    attendre(&banc, "pc").await;
    let avant = fichiers_du_dossier(&dossier);

    // Des données reconnaissables dans le chemin, le corps et la réponse.
    let origine = format!("https://{HOTE}");
    let (statut, _, corps) = requete(
        &banc,
        "POST",
        "/api/machines/pc/applis/cofiticket/api/ticket/R/1/resoudre?client=MARQUEUR-CHEMIN",
        &[
            ("origin", origine.as_str()),
            ("content-type", "application/json"),
        ],
        r#"{"client":"MARQUEUR-CORPS-DONNEES-CLIENT"}"#,
    )
    .await;
    assert_eq!(statut, 201);
    assert!(texte(corps).await.contains("MARQUEUR-CORPS"));
    let (_, _, corps) = requete(
        &banc,
        "GET",
        "/api/machines/pc/applis/cofiticket/api/etat",
        &[],
        "",
    )
    .await;
    texte(corps).await;

    // L'audit note l'ouverture (une seule fois pour les deux requêtes), sans contenu.
    let notes = journal(&banc).await;
    let ouvertures: Vec<_> = notes.iter().filter(|(a, _)| a == "appli_ouverte").collect();
    assert_eq!(ouvertures.len(), 1, "{notes:?}");
    assert_eq!(ouvertures[0].1, "pc:cofiticket");
    let audit = format!("{notes:?}");
    assert!(
        !audit.contains("MARQUEUR") && !audit.contains(JETON_APPLI),
        "{audit}"
    );

    // Aucun fichier du dossier du relais ne contient les données, ni le jeton de l'appli ; il
    // n'y a pas non plus de nouveau fichier.
    let apres = fichiers_du_dossier(&dossier);
    assert_eq!(
        avant.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>(),
        apres.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>(),
        "fichiers créés pendant les requêtes"
    );
    for (nom, contenu) in &apres {
        for marque in ["MARQUEUR", JETON_APPLI, "pose-par-appli"] {
            assert!(
                !contenu
                    .windows(marque.len())
                    .any(|f| f == marque.as_bytes()),
                "{nom} contient {marque}"
            );
        }
    }
    // La base du relais a bien été lue pour le test (elle n'est pas vide).
    assert!(
        apres
            .iter()
            .any(|(n, c)| n.ends_with("ricochet.db") && !c.is_empty())
    );
    std::fs::remove_dir_all(&dossier).ok();
}

fn fichiers_du_dossier(dossier: &Path) -> Vec<(String, Vec<u8>)> {
    let mut fichiers: Vec<_> = std::fs::read_dir(dossier)
        .unwrap()
        .map(|e| {
            let chemin = e.unwrap().path();
            (
                chemin.file_name().unwrap().to_string_lossy().into_owned(),
                std::fs::read(&chemin).unwrap_or_default(),
            )
        })
        .collect();
    fichiers.sort();
    fichiers
}

#[tokio::test]
async fn navigateur_parti_ferme_la_connexion_a_l_application() {
    // Une application qui garde le flux ouvert : quand le navigateur part, l'agent doit couper.
    let ecoute = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = ecoute.local_addr().unwrap().port();
    let (ferme, mut ferme_recu) = tokio::sync::mpsc::channel(1);
    tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut flux, _) = ecoute.accept().await.unwrap();
        let mut tampon = [0u8; 4096];
        let _ = flux.read(&mut tampon).await;
        flux.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\ndata: 1\n\n")
            .await
            .unwrap();
        while flux.read(&mut tampon).await.unwrap_or(0) > 0 {}
        let _ = ferme.send(()).await;
    });
    let banc = relais(None).await;
    lancer_agent(config_agent(
        &banc,
        "pc",
        "secret-pc",
        appli_cofiticket(port),
    ));
    attendre(&banc, "pc").await;
    let (_, _, mut corps) = requete(
        &banc,
        "GET",
        "/api/machines/pc/applis/cofiticket/flux",
        &[],
        "",
    )
    .await;
    corps.frame().await.unwrap().unwrap();
    drop(corps);
    tokio::time::timeout(Duration::from_secs(5), ferme_recu.recv())
        .await
        .expect("l'application doit voir la connexion fermée")
        .unwrap();
}

#[tokio::test]
async fn application_arretee_donne_une_erreur_claire() {
    // Un port où personne n'écoute.
    let libre = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let banc = relais(None).await;
    lancer_agent(config_agent(
        &banc,
        "pc",
        "secret-pc",
        appli_cofiticket(libre),
    ));
    attendre(&banc, "pc").await;
    let (statut, _, corps) =
        requete(&banc, "GET", "/api/machines/pc/applis/cofiticket/", &[], "").await;
    assert_eq!(statut, 502);
    assert!(texte(corps).await.contains("injoignable"));
    let _ = HeaderValue::from_static("x");
}
