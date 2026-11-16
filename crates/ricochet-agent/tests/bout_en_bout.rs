//! Un vrai relais et un vrai agent, reliés en local.

use std::{net::SocketAddr, sync::Arc, time::Duration};

use futures_util::{SinkExt, StreamExt};
use ricochet_agent::{
    Config,
    config::ConfigTerminal,
    connexion::{Fin, session},
    systeme::Collecteur,
};
use ricochet_protocole::Refus;
use ricochet_serveur::{Base, Relais, application, jeton};
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest, http::HeaderValue};

/// Jeton de l'API pour les essais du terminal.
const JETON_OPERATEUR: &str = "jeton-operateur";

async fn demarrer_relais() -> (String, Arc<Relais>) {
    let config = ricochet_serveur::Config::depuis_texte(&format!(
        "intervalle_etat_s = 1\njeton_operateur_sha256 = \"{}\"",
        jeton::empreinte(JETON_OPERATEUR)
    ))
    .unwrap();
    let base = Base::en_memoire();
    base.ajouter_machine("pc", None, &jeton::empreinte("secret-pc"), 0)
        .unwrap();
    let relais = Arc::new(Relais::new(config, base));
    let routeur = application(relais.clone());
    let auditeur = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let adresse = auditeur.local_addr().unwrap();
    tokio::spawn(async move {
        axum_serve(auditeur, routeur).await;
    });
    (format!("ws://{adresse}/agent"), relais)
}

async fn axum_serve(auditeur: tokio::net::TcpListener, routeur: axum::Router) {
    axum::serve(
        auditeur,
        routeur.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .unwrap();
}

fn agent(relais: &str, jeton: &str) -> Config {
    Config {
        relais: relais.into(),
        id: "pc".into(),
        jeton: jeton.into(),
        terminal: Some(ConfigTerminal {
            commande: vec!["/bin/sh".into()],
            maximum: 2,
        }),
        fichiers: Some(Default::default()),
    }
}

async fn attendre_connexion(relais: &Relais) {
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if relais.registre.liste(&relais.base.machines().unwrap())[0].connectee {
            return;
        }
    }
    panic!("l'agent ne s'est pas connecté");
}

/// Ouvre le terminal de « pc » comme le ferait le navigateur, avec l'origine donnée.
async fn ouvrir_terminal(
    url_agent: &str,
    origine: Option<&str>,
) -> Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    tokio_tungstenite::tungstenite::Error,
> {
    let url = url_agent.replace("/agent", "/api/machines/pc/terminal?colonnes=100&lignes=30");
    let mut requete = url.into_client_request().unwrap();
    let entetes = requete.headers_mut();
    let autorisation = format!("Bearer {JETON_OPERATEUR}");
    entetes.insert(
        "authorization",
        HeaderValue::from_str(&autorisation).unwrap(),
    );
    if let Some(origine) = origine {
        entetes.insert("origin", HeaderValue::from_str(origine).unwrap());
    }
    Ok(tokio_tungstenite::connect_async(requete).await?.0)
}

#[tokio::test]
async fn terminal_de_bout_en_bout() {
    let (url, relais) = demarrer_relais().await;
    let config = agent(&url, "secret-pc");
    tokio::spawn(async move { session(&config, &mut Collecteur::new().unwrap()).await });
    attendre_connexion(&relais).await;

    // Un autre site ne peut pas ouvrir le terminal depuis le navigateur de l'opérateur.
    assert!(
        ouvrir_terminal(&url, Some("https://evil.example"))
            .await
            .is_err()
    );

    let hote = url.trim_start_matches("ws://").trim_end_matches("/agent");
    let mut ws = ouvrir_terminal(&url, Some(&format!("http://{hote}")))
        .await
        .unwrap();
    ws.send(Message::binary(&b"stty size; echo ricochet-$((2+3))\n"[..]))
        .await
        .unwrap();
    let mut vu = String::new();
    let fermeture = loop {
        let message = tokio::time::timeout(Duration::from_secs(10), ws.next())
            .await
            .expect("le terminal doit répondre")
            .unwrap()
            .unwrap();
        match message {
            Message::Binary(octets) => {
                vu.push_str(&String::from_utf8_lossy(&octets));
                if vu.contains("ricochet-5") && vu.contains("30 100") {
                    ws.send(Message::binary(&b"exit 4\n"[..])).await.unwrap();
                }
            }
            Message::Close(trame) => break trame.unwrap(),
            _ => {}
        }
    };
    assert!(vu.contains("ricochet-5"), "{vu}");
    assert_eq!(fermeture.reason.as_str(), "fin du shell (code 4)");

    let journal: Vec<String> = relais
        .base
        .journal(10)
        .unwrap()
        .into_iter()
        .map(|l| l.action)
        .collect();
    assert!(
        journal.contains(&"terminal_ouvert".to_owned()),
        "{journal:?}"
    );
    for _ in 0..20 {
        if relais.base.journal(1).unwrap()[0].action == "terminal_ferme" {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("la fermeture du terminal doit être journalisée");
}

#[tokio::test]
async fn agent_connecte_et_envoie_son_etat() {
    let (url, relais) = demarrer_relais().await;
    let config = agent(&url, "secret-pc");
    let tache =
        tokio::spawn(async move { session(&config, &mut Collecteur::new().unwrap()).await });

    let mut vue = None;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let machine = relais
            .registre
            .liste(&relais.base.machines().unwrap())
            .remove(0);
        if machine.connectee && machine.etat.is_some() {
            vue = Some(machine);
            break;
        }
    }
    let machine = vue.expect("l'agent n'est pas apparu connecté avec son état");
    assert_eq!(machine.adresse.as_deref(), Some("127.0.0.1"));
    assert!(machine.info.unwrap().version_agent.starts_with("0."));

    // Une seconde connexion du même agent remplace la première, qui se termine proprement.
    let config = agent(&url, "secret-pc");
    let _seconde =
        tokio::spawn(async move { session(&config, &mut Collecteur::new().unwrap()).await });
    let fin = tokio::time::timeout(Duration::from_secs(5), tache)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(fin, Fin::Fermee);
}

#[tokio::test]
async fn machine_revoquee_deconnectee() {
    let (url, relais) = demarrer_relais().await;
    let config = agent(&url, "secret-pc");
    let tache =
        tokio::spawn(async move { session(&config, &mut Collecteur::new().unwrap()).await });
    attendre_connexion(&relais).await;
    relais.base.supprimer_machine("pc").unwrap();
    relais.registre.retirer("pc");
    let fin = tokio::time::timeout(Duration::from_secs(5), tache)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(fin, Fin::Fermee);
    // Le jeton ne vaut plus rien.
    let fin = session(&agent(&url, "secret-pc"), &mut Collecteur::new().unwrap())
        .await
        .unwrap();
    assert_eq!(fin, Fin::Refusee(Refus::Identification));
}

#[tokio::test]
async fn mauvais_jeton_refuse() {
    let (url, relais) = demarrer_relais().await;
    let fin = session(&agent(&url, "faux"), &mut Collecteur::new().unwrap())
        .await
        .unwrap();
    assert_eq!(fin, Fin::Refusee(Refus::Identification));
    assert!(!relais.registre.liste(&relais.base.machines().unwrap())[0].connectee);
}

#[tokio::test]
async fn relais_injoignable() {
    let erreur = session(
        &agent("ws://127.0.0.1:9/agent", "x"),
        &mut Collecteur::new().unwrap(),
    )
    .await;
    assert!(erreur.is_err());
}

/// Appelle l'API du relais avec le jeton de l'opérateur ; renvoie statut, en-têtes et corps.
async fn appeler_api(
    relais: &Arc<Relais>,
    methode: &str,
    chemin: &str,
    entetes: &[(&str, &str)],
    corps: &[u8],
) -> (u16, axum::http::HeaderMap, Vec<u8>) {
    use tower::ServiceExt;
    let mut requete = axum::http::Request::builder()
        .method(methode)
        .uri(chemin)
        .header("authorization", format!("Bearer {JETON_OPERATEUR}"));
    for (nom, valeur) in entetes {
        requete = requete.header(*nom, *valeur);
    }
    let reponse = application(relais.clone())
        .oneshot(
            requete
                .body(axum::body::Body::from(corps.to_vec()))
                .unwrap(),
        )
        .await
        .unwrap();
    let statut = reponse.status().as_u16();
    let entetes = reponse.headers().clone();
    let corps = axum::body::to_bytes(reponse.into_body(), 1 << 24)
        .await
        .unwrap();
    (statut, entetes, corps.to_vec())
}

#[tokio::test]
async fn fichiers_de_bout_en_bout() {
    let (url, relais) = demarrer_relais().await;
    let config = agent(&url, "secret-pc");
    tokio::spawn(async move { session(&config, &mut Collecteur::new().unwrap()).await });
    attendre_connexion(&relais).await;

    let dossier = std::env::temp_dir().join(format!("ricochet-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dossier);
    std::fs::create_dir_all(&dossier).unwrap();
    let d = dossier.to_str().unwrap();
    // Plus d'un morceau, pour que le téléchargement en demande plusieurs à l'agent.
    let contenu: Vec<u8> = (0..700_000u32).map(|i| (i % 251) as u8).collect();
    let (debut, fin) = contenu.split_at(400_000);

    let envoi = |decalage: usize, fin: bool| {
        format!("/api/machines/pc/fichiers?chemin={d}/f.bin&decalage={decalage}&fin={fin}")
    };
    let (statut, _, corps) = appeler_api(&relais, "PUT", &envoi(0, false), &[], debut).await;
    assert_eq!(
        (statut, corps.as_slice()),
        (200, &br#"{"taille":400000}"#[..])
    );
    let partiel = format!("/api/machines/pc/fichiers/partiel?chemin={d}/f.bin");
    let (_, _, corps) = appeler_api(&relais, "GET", &partiel, &[], b"").await;
    assert_eq!(corps, br#"{"taille":400000}"#);
    let (statut, _, _) = appeler_api(&relais, "PUT", &envoi(400_000, true), &[], fin).await;
    assert_eq!(statut, 200);
    assert_eq!(std::fs::read(dossier.join("f.bin")).unwrap(), contenu);

    let (statut, _, corps) = appeler_api(
        &relais,
        "GET",
        &format!("/api/machines/pc/fichiers?chemin={d}"),
        &[],
        b"",
    )
    .await;
    assert_eq!(statut, 200);
    let liste: serde_json::Value = serde_json::from_slice(&corps).unwrap();
    assert_eq!(liste["entrees"][0]["nom"], "f.bin");
    assert_eq!(liste["entrees"][0]["taille"], 700_000);

    let telecharger = format!("/api/machines/pc/fichiers/telecharger?chemin={d}/f.bin");
    let (statut, entetes, corps) = appeler_api(&relais, "GET", &telecharger, &[], b"").await;
    assert_eq!(statut, 200);
    assert_eq!(
        entetes["content-disposition"],
        "attachment; filename*=UTF-8''f.bin"
    );
    assert_eq!(corps, contenu);
    let (statut, entetes, corps) = appeler_api(
        &relais,
        "GET",
        &telecharger,
        &[("range", "bytes=600000-")],
        b"",
    )
    .await;
    assert_eq!(statut, 206);
    assert_eq!(entetes["content-range"], "bytes 600000-699999/700000");
    assert_eq!(corps, &contenu[600_000..]);

    let (statut, _, corps) = appeler_api(
        &relais,
        "GET",
        &format!("/api/machines/pc/fichiers/telecharger?chemin={d}/absent"),
        &[],
        b"",
    )
    .await;
    assert_eq!(statut, 422, "{}", String::from_utf8_lossy(&corps));
    let audit = relais.base.journal(10).unwrap();
    let actions = serde_json::to_string(&audit).unwrap();
    assert!(actions.contains("fichier_recu") && actions.contains("fichier_telecharge"));
    std::fs::remove_dir_all(dossier).unwrap();
}
