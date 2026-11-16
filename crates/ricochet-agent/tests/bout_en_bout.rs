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
