//! API de l'opérateur, protégée par un jeton (`Authorization: Bearer <jeton>`).

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use ricochet_protocole::RelaisVersAgent;

use crate::{
    Relais, jeton,
    registre::{Commande, ErreurEnvoi, VueMachine},
};

pub fn routeur(relais: Arc<Relais>) -> Router<Arc<Relais>> {
    Router::new()
        .route("/machines", get(machines))
        .route("/machines/{id}/actualiser", post(actualiser))
        .layer(middleware::from_fn_with_state(relais, authentifier))
}

async fn authentifier(
    State(relais): State<Arc<Relais>>,
    requete: Request,
    suite: Next,
) -> Response {
    let jeton_recu = requete
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    match jeton_recu {
        Some(recu) if jeton::verifier(recu, &relais.config.jeton_operateur_sha256) => {
            suite.run(requete).await
        }
        _ => (StatusCode::UNAUTHORIZED, "jeton manquant ou refusé").into_response(),
    }
}

async fn machines(State(relais): State<Arc<Relais>>) -> Json<Vec<VueMachine>> {
    Json(relais.registre.liste(&relais.config))
}

/// Demande à une machine connectée d'envoyer son état sans attendre.
async fn actualiser(State(relais): State<Arc<Relais>>, Path(id): Path<String>) -> Response {
    if relais.config.agent(&id).is_none() {
        return (StatusCode::NOT_FOUND, "machine inconnue").into_response();
    }
    match relais
        .registre
        .envoyer(&id, Commande::Envoyer(RelaisVersAgent::DemandeEtat))
    {
        Ok(()) => StatusCode::ACCEPTED.into_response(),
        Err(ErreurEnvoi::Deconnectee) => {
            (StatusCode::CONFLICT, "machine déconnectée").into_response()
        }
        Err(ErreurEnvoi::Saturee) => {
            (StatusCode::SERVICE_UNAVAILABLE, "machine occupée").into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::body::{Body, to_bytes};
    use tower::ServiceExt;

    use crate::{Config, application};

    fn config() -> Config {
        Config::depuis_texte(&format!(
            "jeton_operateur_sha256 = \"{}\"\n[[agents]]\nid = \"pc\"\njeton_sha256 = \"{}\"\nnom = \"Mon PC\"\n",
            crate::jeton::empreinte("operateur"),
            crate::jeton::empreinte("agent"),
        ))
        .unwrap()
    }

    async fn appeler(methode: &str, chemin: &str, jeton: Option<&str>) -> (u16, String) {
        let (routeur, _) = application(config());
        let mut requete = axum::http::Request::builder().method(methode).uri(chemin);
        if let Some(jeton) = jeton {
            requete = requete.header("authorization", format!("Bearer {jeton}"));
        }
        let reponse = routeur
            .oneshot(requete.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let statut = reponse.status().as_u16();
        let corps = to_bytes(reponse.into_body(), 1 << 20).await.unwrap();
        (statut, String::from_utf8(corps.to_vec()).unwrap())
    }

    #[tokio::test]
    async fn sante_sans_jeton() {
        assert_eq!(appeler("GET", "/sante", None).await, (200, "ok".into()));
    }

    #[tokio::test]
    async fn jeton_obligatoire() {
        assert_eq!(appeler("GET", "/api/machines", None).await.0, 401);
        assert_eq!(appeler("GET", "/api/machines", Some("faux")).await.0, 401);
        // Le jeton d'un agent n'ouvre pas l'API.
        assert_eq!(appeler("GET", "/api/machines", Some("agent")).await.0, 401);
    }

    #[tokio::test]
    async fn liste_des_machines() {
        let (statut, corps) = appeler("GET", "/api/machines", Some("operateur")).await;
        assert_eq!(statut, 200);
        let liste: serde_json::Value = serde_json::from_str(&corps).unwrap();
        assert_eq!(liste[0]["id"], "pc");
        assert_eq!(liste[0]["nom"], "Mon PC");
        assert_eq!(liste[0]["connectee"], false);
    }

    #[tokio::test]
    async fn actualiser_machine() {
        assert_eq!(
            appeler(
                "POST",
                "/api/machines/inconnue/actualiser",
                Some("operateur")
            )
            .await
            .0,
            404
        );
        assert_eq!(
            appeler("POST", "/api/machines/pc/actualiser", Some("operateur"))
                .await
                .0,
            409
        );
    }
}
