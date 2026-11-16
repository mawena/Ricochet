//! Relais de Ricochet.
//!
//! Les agents s'y connectent en WebSocket sortant (`/agent`) ; l'opérateur consulte et commande
//! les machines par l'API (`/api/...`), protégée par un jeton. Le relais écoute en local et
//! nginx assure le HTTPS devant lui.

pub mod config;
pub mod jeton;
pub mod registre;

mod agents;
mod api;

use std::{net::SocketAddr, sync::Arc};

use axum::{Router, routing::get};

pub use config::Config;
pub use registre::Registre;

/// État partagé du relais.
pub struct Relais {
    pub config: Config,
    pub registre: Registre,
}

/// Construit l'application : à servir avec les informations de connexion
/// (`into_make_service_with_connect_info::<SocketAddr>()`).
pub fn application(config: Config) -> (Router, Arc<Relais>) {
    let relais = Arc::new(Relais {
        config,
        registre: Registre::default(),
    });
    let routeur = Router::new()
        .route("/sante", get(|| async { "ok" }))
        .route("/agent", get(agents::connexion))
        .nest("/api", api::routeur(relais.clone()))
        .with_state(relais.clone());
    (routeur, relais)
}

/// Sert le relais jusqu'à l'arrêt du processus.
pub async fn servir(config: Config) -> anyhow::Result<()> {
    let ecoute = config.ecoute;
    let (routeur, _) = application(config);
    let auditeur = tokio::net::TcpListener::bind(ecoute).await?;
    tracing::info!("relais à l'écoute sur {ecoute}");
    axum::serve(
        auditeur,
        routeur.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await?;
    Ok(())
}
