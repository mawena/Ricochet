//! Relais de Ricochet.
//!
//! Les agents s'y connectent en WebSocket sortant (`/agent`) ; l'opérateur se connecte à
//! l'interface web (`/`) avec son mot de passe et un code TOTP, et l'interface parle à l'API
//! (`/api/...`). Le relais écoute en local et nginx assure le HTTPS devant lui.

pub mod base;
pub mod comptes;
pub mod config;
pub mod jeton;
pub mod registre;
pub mod totp;

mod agents;
mod api;
mod terminal;
mod web;

use std::{net::SocketAddr, sync::Arc, time::Duration};

use axum::{
    Router,
    extract::{ConnectInfo, FromRequestParts},
    http::{HeaderMap, HeaderValue, header, request::Parts},
    middleware,
    response::Response,
    routing::get,
};

pub use base::Base;
pub use config::Config;
pub use registre::Registre;

/// État partagé du relais.
pub struct Relais {
    pub config: Config,
    pub base: Base,
    pub registre: Registre,
    pub essais_adresse: comptes::Limiteur,
    pub essais_compte: comptes::Limiteur,
}

impl Relais {
    pub fn new(config: Config, base: Base) -> Self {
        Self {
            config,
            base,
            registre: Registre::default(),
            essais_adresse: comptes::Limiteur::new(comptes::ECHECS_MAX_ADRESSE),
            essais_compte: comptes::Limiteur::new(comptes::ECHECS_MAX_COMPTE),
        }
    }

    /// Note une action dans le journal d'audit ; une erreur d'écriture est signalée sans
    /// interrompre l'action.
    pub fn noter(&self, compte: Option<&str>, adresse: &str, action: &str, detail: &str) {
        tracing::info!(
            "audit : {action} ({detail}) par {} depuis {adresse}",
            compte.unwrap_or("-")
        );
        if let Err(erreur) =
            self.base
                .noter(registre::maintenant(), compte, adresse, action, detail)
        {
            tracing::error!("journal d'audit : {erreur:#}");
        }
    }
}

/// Construit l'application : à servir avec les informations de connexion
/// (`into_make_service_with_connect_info::<SocketAddr>()`).
pub fn application(relais: Arc<Relais>) -> Router {
    Router::new()
        .route("/sante", get(|| async { "ok" }))
        .route("/agent", get(agents::connexion))
        .nest("/api", api::routeur(relais.clone()))
        .merge(web::routeur())
        .layer(middleware::map_response(entetes_de_securite))
        .with_state(relais)
}

/// Ouvre la base indiquée par la configuration et prépare l'état du relais.
pub fn preparer(config: Config) -> anyhow::Result<Arc<Relais>> {
    let base = Base::ouvrir(&config.base)?;
    Ok(Arc::new(Relais::new(config, base)))
}

/// Sert le relais jusqu'à l'arrêt du processus.
pub async fn servir(config: Config) -> anyhow::Result<()> {
    let ecoute = config.ecoute;
    let relais = preparer(config)?;
    tokio::spawn(purger_sessions(relais.clone()));
    let auditeur = tokio::net::TcpListener::bind(ecoute).await?;
    tracing::info!("relais à l'écoute sur {ecoute}");
    axum::serve(
        auditeur,
        application(relais).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await?;
    Ok(())
}

async fn purger_sessions(relais: Arc<Relais>) {
    let mut periode = tokio::time::interval(Duration::from_secs(3600));
    loop {
        periode.tick().await;
        if let Err(erreur) = relais.base.purger_sessions(registre::maintenant()) {
            tracing::warn!("purge des sessions : {erreur:#}");
        }
    }
}

/// En-têtes ajoutés à toutes les réponses : l'interface ne charge que ses propres fichiers,
/// ne s'affiche pas dans un cadre, et rien n'est gardé en cache. Seuls les styles en ligne
/// sont permis, car xterm.js crée ses propres balises `<style>` ; jamais les scripts.
async fn entetes_de_securite(mut reponse: Response) -> Response {
    let entetes = reponse.headers_mut();
    for (nom, valeur) in [
        (
            header::CONTENT_SECURITY_POLICY,
            "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; connect-src 'self'; \
             img-src 'self' data:; manifest-src 'self'; form-action 'self'; \
             frame-ancestors 'none'; base-uri 'none'",
        ),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        (header::X_FRAME_OPTIONS, "DENY"),
        (header::REFERRER_POLICY, "no-referrer"),
        (header::CACHE_CONTROL, "no-store"),
    ] {
        entetes.insert(nom, HeaderValue::from_static(valeur));
    }
    reponse
}

/// Adresse réelle du client : derrière nginx (connexion locale), celle de l'en-tête `X-Real-IP`.
fn adresse_client(pair: SocketAddr, entetes: &HeaderMap) -> String {
    if pair.ip().is_loopback()
        && let Some(reelle) = entetes.get("x-real-ip").and_then(|v| v.to_str().ok())
    {
        return reelle.to_owned();
    }
    pair.ip().to_string()
}

/// Extracteur de l'adresse réelle du client.
pub(crate) struct Adresse(pub String);

impl<S: Send + Sync> FromRequestParts<S> for Adresse {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(Adresse(
            match parts.extensions.get::<ConnectInfo<SocketAddr>>() {
                Some(ConnectInfo(pair)) => adresse_client(*pair, &parts.headers),
                None => "inconnue".into(),
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adresse_reelle_derriere_nginx() {
        let mut entetes = HeaderMap::new();
        entetes.insert("x-real-ip", "203.0.113.5".parse().unwrap());
        let local: SocketAddr = "127.0.0.1:5000".parse().unwrap();
        let distant: SocketAddr = "198.51.100.7:5000".parse().unwrap();
        assert_eq!(adresse_client(local, &entetes), "203.0.113.5");
        // Un client direct ne peut pas se faire passer pour un autre.
        assert_eq!(adresse_client(distant, &entetes), "198.51.100.7");
        assert_eq!(adresse_client(local, &HeaderMap::new()), "127.0.0.1");
    }
}
