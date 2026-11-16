//! Interface web : quelques fichiers statiques, intégrés au binaire.

use std::sync::Arc;

use axum::{Router, http::header, response::IntoResponse, routing::get};

use crate::Relais;

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
        .route(
            "/",
            get(|| async { fichier("text/html; charset=utf-8", ACCUEIL) }),
        )
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
