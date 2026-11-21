//! Applications web des machines : `/api/machines/{id}/applis/{nom}/{chemin}`.
//!
//! Le relais transmet la requête HTTP de l'opérateur (méthode, chemin, requête, corps et
//! quelques en-têtes) à l'agent, qui la fait à l'application locale déclarée, puis renvoie la
//! réponse au navigateur au fil de l'eau (flux d'évènements compris). Les données de
//! l'application traversent le relais sans y être gardées : ni corps, ni en-têtes, ni réponses
//! ne sont écrits (ni en base, ni dans le journal) ; seule l'ouverture d'une application est
//! inscrite à l'audit (machine, application, opérateur), jamais le contenu.
//!
//! Le cookie de session de Ricochet et l'en-tête `Authorization` ne sont jamais transmis ; les
//! `Set-Cookie` de l'application sont retirés. L'agent ajoute lui-même les en-têtes configurés
//! (un jeton d'accès, par exemple).
//!
//! Protection contre les requêtes forgées : les applications n'ajoutent pas `X-Ricochet` à
//! leurs requêtes ; une requête qui modifie quelque chose doit donc porter un `Origin` qui est
//! celui du relais (ou `X-Ricochet`), en plus du cookie `SameSite=Strict`.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Extension,
    body::{Body, Bytes, to_bytes},
    extract::{OriginalUri, Path, Request, State},
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use ricochet_protocole::EntetesHttp;

use crate::{
    Adresse, Relais,
    api::Operateur,
    registre::{Commande, DemandeAppli, ErreurEnvoi, EvenementAppli, maintenant},
};

/// Corps de requête accepté (au-delà : 413).
pub const CORPS_MAX: usize = 4 * 1024 * 1024;
/// Attente maximale de l'en-tête de la réponse.
const DELAI_ENTETE: Duration = Duration::from_secs(35);
/// Une ouverture d'application n'est inscrite à l'audit qu'une fois par période et par
/// (compte, machine, application) : une page fait des dizaines de requêtes.
const PERIODE_AUDIT_S: u64 = 600;

/// En-têtes de la requête du navigateur transmis à l'agent. Ni `Cookie` ni `Authorization`.
const TRANSMIS: &[&str] = &[
    "accept",
    "accept-language",
    "cache-control",
    "content-type",
    "if-modified-since",
    "if-none-match",
    "last-event-id",
    "origin",
    "pragma",
    "range",
    "user-agent",
    "x-requested-with",
];

/// En-têtes de la réponse qui ne sont pas renvoyés au navigateur.
const RETIRES: &[&str] = &[
    "connection",
    "keep-alive",
    "transfer-encoding",
    "te",
    "trailer",
    "upgrade",
    "proxy-authenticate",
    "set-cookie",
    "set-cookie2",
    "x-accel-buffering",
    "x-accel-redirect",
    "x-accel-limit-rate",
];

/// Dernières ouvertures notées à l'audit (en mémoire seulement).
#[derive(Default)]
pub struct Ouvertures(Mutex<HashMap<(String, String, String), u64>>);

impl Ouvertures {
    /// `true` s'il faut noter cette ouverture.
    fn a_noter(&self, compte: &str, machine: &str, appli: &str, instant: u64) -> bool {
        let mut vues = self.0.lock().unwrap();
        if vues.len() > 256 {
            vues.retain(|_, vu| instant.saturating_sub(*vu) < PERIODE_AUDIT_S);
        }
        let cle = (compte.to_owned(), machine.to_owned(), appli.to_owned());
        match vues.get(&cle) {
            Some(vu) if instant.saturating_sub(*vu) < PERIODE_AUDIT_S => false,
            _ => {
                vues.insert(cle, instant);
                true
            }
        }
    }
}

fn refus(statut: StatusCode, texte: &str) -> Response {
    (statut, texte.to_owned()).into_response()
}

/// Autorité (`hote[:port]`) d'une origine `schéma://autorité`.
fn autorite(origine: &str) -> Option<&str> {
    origine
        .strip_prefix("https://")
        .or_else(|| origine.strip_prefix("http://"))
        .filter(|a| !a.is_empty() && !a.contains('/'))
}

/// Une requête qui modifie quelque chose, faite avec le cookie, doit venir de la page du relais
/// elle-même : `Origin` égal à `Host` (nginx transmet `Host`), ou l'en-tête `X-Ricochet` que
/// seule l'interface du relais sait ajouter. Avec un jeton `Bearer` (scripts), rien à vérifier.
pub(crate) async fn origine_exigee(requete: Request, suite: Next) -> Response {
    let lecture = matches!(
        *requete.method(),
        Method::GET | Method::HEAD | Method::OPTIONS
    );
    let entetes = requete.headers();
    let bearer = entetes
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("Bearer "));
    if !lecture && !bearer && !entetes.contains_key("x-ricochet") {
        let origine = entetes
            .get(header::ORIGIN)
            .and_then(|v| v.to_str().ok())
            .and_then(autorite);
        let hote = entetes.get(header::HOST).and_then(|v| v.to_str().ok());
        if origine.is_none() || origine != hote {
            return refus(StatusCode::FORBIDDEN, "origine de la requête refusée");
        }
    }
    suite.run(requete).await
}

/// `/api/machines/{id}/applis/{nom}` sans barre finale : redirige vers la forme avec barre, la
/// seule où les adresses relatives de l'application tombent juste.
pub(crate) async fn racine(OriginalUri(uri): OriginalUri) -> Response {
    let mut cible = format!("{}/", uri.path());
    if let Some(requete) = uri.query() {
        cible.push('?');
        cible.push_str(requete);
    }
    match HeaderValue::from_str(&cible) {
        Ok(cible) => (StatusCode::PERMANENT_REDIRECT, [(header::LOCATION, cible)]).into_response(),
        Err(_) => refus(StatusCode::BAD_REQUEST, "adresse invalide"),
    }
}

/// Chemin et requête à transmettre, pris tels qu'ils ont été reçus (pas de décodage, pas de
/// normalisation) : tout ce qui suit `/api/machines/{id}/applis/{nom}`.
fn chemin_appli(uri: &axum::http::Uri) -> String {
    let rest = uri.path().splitn(7, '/').nth(6).unwrap_or("");
    let mut chemin = format!("/{rest}");
    if let Some(requete) = uri.query() {
        chemin.push('?');
        chemin.push_str(requete);
    }
    chemin
}

/// Entrées de la liste des en-têtes de la requête à transmettre.
fn entetes_transmis(entetes: &HeaderMap) -> EntetesHttp {
    entetes
        .iter()
        .filter(|(nom, _)| TRANSMIS.contains(&nom.as_str()))
        .filter_map(|(nom, valeur)| {
            Some((nom.as_str().to_owned(), valeur.to_str().ok()?.to_owned()))
        })
        .collect()
}

/// Dit à l'agent d'abandonner une requête dont le navigateur est parti.
struct Garde {
    relais: Arc<Relais>,
    requete: u64,
}

impl Drop for Garde {
    fn drop(&mut self) {
        if let Some(machine) = self.relais.registre.oublier_appli(self.requete) {
            let _ = self.relais.registre.envoyer(
                &machine,
                Commande::Envoyer(ricochet_protocole::RelaisVersAgent::AnnulerAppli {
                    requete: self.requete,
                }),
            );
        }
    }
}

/// Adresse de l'application dans le relais, pour réécrire les redirections.
fn prefixe(id: &str, nom: &str) -> String {
    format!("/api/machines/{id}/applis/{nom}")
}

/// Réécrit un `Location` qui vise l'application (chemin absolu ou adresse locale) pour qu'il
/// reste sous le préfixe du relais.
fn lieu_reecrit(valeur: &str, prefixe: &str) -> String {
    let chemin = ["http://127.0.0.1", "http://localhost"]
        .iter()
        .find_map(|debut| {
            let reste = valeur.strip_prefix(debut)?;
            let reste = reste.trim_start_matches(|c: char| c == ':' || c.is_ascii_digit());
            reste.starts_with('/').then_some(reste)
        })
        .unwrap_or(valeur);
    if chemin.starts_with('/') && !chemin.starts_with("//") {
        format!("{prefixe}{chemin}")
    } else {
        chemin.to_owned()
    }
}

pub(crate) async fn requete(
    State(relais): State<Arc<Relais>>,
    Extension(operateur): Extension<Operateur>,
    Adresse(adresse): Adresse,
    Path(parametres): Path<HashMap<String, String>>,
    OriginalUri(uri): OriginalUri,
    requete: Request,
) -> Response {
    let (Some(id), Some(nom)) = (parametres.get("id"), parametres.get("nom")) else {
        return refus(StatusCode::NOT_FOUND, "application inconnue");
    };
    let methode = requete.method().as_str().to_owned();
    if !["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"].contains(&methode.as_str()) {
        return refus(StatusCode::METHOD_NOT_ALLOWED, "méthode non permise");
    }
    match relais.base.empreinte_machine(id) {
        Ok(Some(_)) => {}
        Ok(None) => return refus(StatusCode::NOT_FOUND, "machine inconnue"),
        Err(erreur) => return crate::api::Erreur::from(erreur).into_response(),
    }
    let chemin = chemin_appli(&uri);
    let entetes = entetes_transmis(requete.headers());
    let corps = match to_bytes(requete.into_body(), CORPS_MAX).await {
        Ok(corps) => corps.to_vec(),
        Err(_) => return refus(StatusCode::PAYLOAD_TOO_LARGE, "corps de requête trop gros"),
    };
    let demande = DemandeAppli {
        methode,
        chemin,
        entetes,
        corps,
    };
    let (numero, mut evenements) = match relais.registre.ouvrir_appli(id, nom, demande) {
        Ok(ouverte) => ouverte,
        Err(ErreurEnvoi::SansAppli) => {
            relais.noter(
                Some(&operateur.compte),
                &adresse,
                "appli_refusee",
                &format!("{id}:{nom}"),
            );
            return refus(
                StatusCode::NOT_FOUND,
                "application non déclarée sur cette machine (ou agent trop ancien)",
            );
        }
        Err(ErreurEnvoi::Deconnectee) => {
            return refus(StatusCode::CONFLICT, "machine déconnectée");
        }
        Err(ErreurEnvoi::TropDApplis) => {
            return refus(StatusCode::TOO_MANY_REQUESTS, "trop de requêtes en cours");
        }
        Err(_) => return refus(StatusCode::SERVICE_UNAVAILABLE, "machine occupée"),
    };
    let garde = Garde {
        relais: relais.clone(),
        requete: numero,
    };
    if relais
        .ouvertures
        .a_noter(&operateur.compte, id, nom, maintenant())
    {
        relais.noter(
            Some(&operateur.compte),
            &adresse,
            "appli_ouverte",
            &format!("{id}:{nom}"),
        );
    }

    // Premier évènement : l'en-tête de la réponse, ou l'échec.
    let (statut, entetes) = match tokio::time::timeout(DELAI_ENTETE, evenements.recv()).await {
        Ok(Some(EvenementAppli::Entete { statut, entetes })) => (statut, entetes),
        Ok(Some(EvenementAppli::Fin(erreur))) => {
            let raison = erreur.unwrap_or_else(|| "réponse vide de l'agent".into());
            return refus(StatusCode::BAD_GATEWAY, &format!("application : {raison}"));
        }
        Ok(Some(EvenementAppli::Corps(_))) => {
            return refus(StatusCode::BAD_GATEWAY, "réponse inattendue de la machine");
        }
        Ok(None) => return refus(StatusCode::CONFLICT, "machine déconnectée"),
        Err(_) => {
            return refus(StatusCode::GATEWAY_TIMEOUT, "l'application ne répond pas");
        }
    };

    let flux = futures_util::stream::unfold(
        (evenements, garde, false),
        |(mut evenements, garde, fini)| async move {
            if fini {
                return None;
            }
            match evenements.recv().await {
                Some(EvenementAppli::Corps(donnees)) => {
                    Some((Ok(Bytes::from(donnees)), (evenements, garde, false)))
                }
                Some(EvenementAppli::Fin(None)) => None,
                Some(EvenementAppli::Fin(Some(erreur))) => Some((
                    Err(std::io::Error::other(erreur)),
                    (evenements, garde, true),
                )),
                Some(EvenementAppli::Entete { .. }) => None,
                None => Some((
                    Err(std::io::Error::other("machine déconnectée")),
                    (evenements, garde, true),
                )),
            }
        },
    );
    let mut reponse = Response::new(Body::from_stream(flux));
    *reponse.status_mut() = StatusCode::from_u16(statut).unwrap_or(StatusCode::BAD_GATEWAY);
    let prefixe = prefixe(id, nom);
    for (nom_entete, valeur) in entetes {
        let nom_entete = nom_entete.to_ascii_lowercase();
        if RETIRES.contains(&nom_entete.as_str()) {
            continue;
        }
        let valeur = if nom_entete == "location" {
            lieu_reecrit(&valeur, &prefixe)
        } else {
            valeur
        };
        if let (Ok(nom), Ok(valeur)) = (
            HeaderName::from_bytes(nom_entete.as_bytes()),
            HeaderValue::from_str(&valeur),
        ) {
            reponse.headers_mut().append(nom, valeur);
        }
    }
    // nginx ne doit pas retenir les évènements d'un flux.
    reponse
        .headers_mut()
        .insert("x-accel-buffering", HeaderValue::from_static("no"));
    reponse
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chemins_transmis_tels_quels() {
        let uri = |u: &str| u.parse::<axum::http::Uri>().unwrap();
        let chemin = |u| chemin_appli(&uri(u));
        assert_eq!(chemin("/api/machines/pc/applis/a/"), "/");
        assert_eq!(
            chemin("/api/machines/pc/applis/a/api/etat?x=1&y=%20"),
            "/api/etat?x=1&y=%20"
        );
        assert_eq!(
            chemin("/api/machines/pc/applis/a/statique/app.js"),
            "/statique/app.js"
        );
        assert_eq!(chemin("/api/machines/pc/applis/a/a%2Fb/c"), "/a%2Fb/c");
    }

    #[test]
    fn origines() {
        assert_eq!(
            autorite("https://ricochet.mawena.cloud"),
            Some("ricochet.mawena.cloud")
        );
        assert_eq!(autorite("http://127.0.0.1:7870"), Some("127.0.0.1:7870"));
        assert_eq!(autorite("https://x/y"), None);
        assert_eq!(autorite("null"), None);
    }

    #[test]
    fn redirections() {
        let p = "/api/machines/pc/applis/a";
        assert_eq!(
            lieu_reecrit("/ticket/1", p),
            "/api/machines/pc/applis/a/ticket/1"
        );
        assert_eq!(
            lieu_reecrit("http://127.0.0.1:8790/x", p),
            "/api/machines/pc/applis/a/x"
        );
        assert_eq!(lieu_reecrit("liste", p), "liste");
        assert_eq!(lieu_reecrit("//autre.example/x", p), "//autre.example/x");
        assert_eq!(
            lieu_reecrit("https://autre.example/x", p),
            "https://autre.example/x"
        );
    }

    #[test]
    fn audit_une_fois_par_periode() {
        let ouvertures = Ouvertures::default();
        assert!(ouvertures.a_noter("c", "pc", "a", 1000));
        assert!(!ouvertures.a_noter("c", "pc", "a", 1100));
        assert!(ouvertures.a_noter("c", "pc", "b", 1100));
        assert!(ouvertures.a_noter("c", "pc", "a", 1000 + PERIODE_AUDIT_S));
    }
}
