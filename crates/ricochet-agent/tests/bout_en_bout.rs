//! Un vrai relais et un vrai agent, reliés en local.

use std::{net::SocketAddr, sync::Arc, time::Duration};

use ricochet_agent::{
    Config,
    connexion::{Fin, session},
    systeme::Collecteur,
};
use ricochet_protocole::Refus;
use ricochet_serveur::{Relais, application, jeton};

async fn demarrer_relais() -> (String, Arc<Relais>) {
    let config = ricochet_serveur::Config::depuis_texte(&format!(
        "jeton_operateur_sha256 = \"{}\"\nintervalle_etat_s = 1\n[[agents]]\nid = \"pc\"\njeton_sha256 = \"{}\"\n",
        jeton::empreinte("operateur"),
        jeton::empreinte("secret-pc"),
    ))
    .unwrap();
    let (routeur, relais) = application(config);
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
    }
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
        let machine = relais.registre.liste(&relais.config).remove(0);
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
async fn mauvais_jeton_refuse() {
    let (url, relais) = demarrer_relais().await;
    let fin = session(&agent(&url, "faux"), &mut Collecteur::new().unwrap())
        .await
        .unwrap();
    assert_eq!(fin, Fin::Refusee(Refus::Identification));
    assert!(!relais.registre.liste(&relais.config)[0].connectee);
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
