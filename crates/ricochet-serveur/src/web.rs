//! Interface web : quelques fichiers statiques, intégrés au binaire.

use std::sync::Arc;

use axum::{
    Router,
    extract::State,
    http::{HeaderValue, header, request::Parts},
    response::{IntoResponse, Response},
    routing::get,
};

use crate::{Adresse, Relais, api, jeton, portier, registre::maintenant};

const ACCUEIL: &str = include_str!("../web/index.html");
const SCRIPT: &str = include_str!("../web/app.js");
const STYLE: &str = include_str!("../web/style.css");
const MANIFESTE: &str = include_str!("../web/manifest.json");
const ICONE: &str = include_str!("../web/icone.svg");
const XTERM: &str = include_str!("../web/vendor/xterm.js");
const XTERM_STYLE: &str = include_str!("../web/vendor/xterm.css");
const XTERM_AJUSTEMENT: &str = include_str!("../web/vendor/addon-fit.js");

fn fichier(type_mime: &'static str, contenu: &'static str) -> impl IntoResponse {
    ([(header::CONTENT_TYPE, type_mime)], contenu)
}

pub fn routeur() -> Router<Arc<Relais>> {
    Router::new()
        .route("/", get(accueil))
        .route(
            "/app.js",
            get(|| async { fichier("text/javascript; charset=utf-8", SCRIPT) }),
        )
        .route(
            "/style.css",
            get(|| async { fichier("text/css; charset=utf-8", STYLE) }),
        )
        .route(
            "/manifest.json",
            get(|| async { fichier("application/manifest+json", MANIFESTE) }),
        )
        .route(
            "/icone.svg",
            get(|| async { fichier("image/svg+xml", ICONE) }),
        )
        .route(
            "/vendor/xterm.js",
            get(|| async { fichier("text/javascript; charset=utf-8", XTERM) }),
        )
        .route(
            "/vendor/xterm.css",
            get(|| async { fichier("text/css; charset=utf-8", XTERM_STYLE) }),
        )
        .route(
            "/vendor/addon-fit.js",
            get(|| async { fichier("text/javascript; charset=utf-8", XTERM_AJUSTEMENT) }),
        )
}

/// Page de l'interface. En mode Portier, si nginx annonce un utilisateur autorisé et que le
/// navigateur n'a pas déjà une session à ce nom, une session est ouverte sans mot de passe ni
/// TOTP (Portier les a déjà vérifiés).
async fn accueil(
    State(relais): State<Arc<Relais>>,
    Adresse(adresse): Adresse,
    parts: Parts,
) -> Response {
    let page = fichier("text/html; charset=utf-8", ACCUEIL);
    let Some(nom) = portier::utilisateur(&relais, &parts) else {
        return page.into_response();
    };
    let instant = maintenant();
    let deja = api::cookie_recu(&relais, &parts.headers).is_some_and(|valeur| {
        relais
            .sessions_portier
            .compte(&jeton::empreinte(&valeur), instant)
            .is_some_and(|compte| compte == nom)
    });
    if deja {
        return page.into_response();
    }
    let valeur = match jeton::nouveau() {
        Ok(valeur) => valeur,
        Err(erreur) => return api::Erreur::from(erreur).into_response(),
    };
    let duree_s = relais.config.duree_session_h * 3600;
    relais
        .sessions_portier
        .ouvrir(jeton::empreinte(&valeur), &nom, instant + duree_s);
    relais.noter(Some(&nom), &adresse, "connexion_portier", "");
    match HeaderValue::from_str(&api::cookie_session(&relais, &valeur, duree_s)) {
        Ok(cookie) => ([(header::SET_COOKIE, cookie)], page).into_response(),
        Err(erreur) => api::Erreur::from(erreur).into_response(),
    }
}
