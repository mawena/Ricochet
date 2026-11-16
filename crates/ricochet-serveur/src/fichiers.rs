//! Fichiers des machines : l'API traduit chaque appel en ordres pour l'agent.
//!
//! - `GET  /api/machines/{id}/fichiers?chemin=…` : contenu d'un dossier ;
//! - `GET  /api/machines/{id}/fichiers/telecharger?chemin=…` : le fichier, lu par morceaux et
//!   transmis au fil de l'eau (en-tête `Range: bytes=N-` accepté pour reprendre) ;
//! - `GET  /api/machines/{id}/fichiers/partiel?chemin=…` : où reprendre un envoi interrompu ;
//! - `PUT  /api/machines/{id}/fichiers?chemin=…&decalage=N&fin=true` : un morceau à écrire.

use std::{sync::Arc, time::Duration};

use axum::{
    Extension, Json,
    body::{Body, Bytes},
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use ricochet_protocole::{MORCEAU_MAX, OrdreFichiers, ReponseFichiers};
use serde::Deserialize;
use serde_json::json;

use crate::{
    Adresse, Relais,
    api::{Erreur, Operateur},
    registre::ErreurEnvoi,
};

/// Attente maximale d'une réponse de l'agent.
const DELAI_REPONSE: Duration = Duration::from_secs(30);

#[derive(Deserialize)]
pub(crate) struct Chemin {
    #[serde(default)]
    chemin: String,
}

#[derive(Deserialize)]
pub(crate) struct Morceau {
    chemin: String,
    decalage: u64,
    #[serde(default)]
    fin: bool,
}

fn refus(statut: StatusCode, texte: &str) -> Response {
    (statut, texte.to_owned()).into_response()
}

/// Envoie un ordre à l'agent et attend sa réponse ; une erreur est déjà une réponse HTTP.
#[allow(clippy::result_large_err)] // l'erreur part telle quelle au navigateur
async fn demander(
    relais: &Relais,
    id: &str,
    ordre: OrdreFichiers,
) -> Result<ReponseFichiers, Response> {
    match relais.base.empreinte_machine(id) {
        Ok(Some(_)) => {}
        Ok(None) => return Err(refus(StatusCode::NOT_FOUND, "machine inconnue")),
        Err(erreur) => return Err(Erreur::from(erreur).into_response()),
    }
    let (requete, reception) =
        relais
            .registre
            .demander_fichiers(id, ordre)
            .map_err(|e| match e {
                ErreurEnvoi::Deconnectee => refus(StatusCode::CONFLICT, "machine déconnectée"),
                ErreurEnvoi::SansFichiers => refus(
                    StatusCode::FORBIDDEN,
                    "accès aux fichiers désactivé sur cette machine",
                ),
                _ => refus(StatusCode::SERVICE_UNAVAILABLE, "machine occupée"),
            })?;
    match tokio::time::timeout(DELAI_REPONSE, reception).await {
        Ok(Ok(ReponseFichiers::Erreur { message })) => {
            Err(refus(StatusCode::UNPROCESSABLE_ENTITY, &message))
        }
        Ok(Ok(reponse)) => Ok(reponse),
        Ok(Err(_)) => Err(refus(StatusCode::CONFLICT, "machine déconnectée")),
        Err(_) => {
            relais.registre.oublier_requete(requete);
            Err(refus(
                StatusCode::GATEWAY_TIMEOUT,
                "la machine ne répond pas",
            ))
        }
    }
}

fn inattendue() -> Response {
    refus(StatusCode::BAD_GATEWAY, "réponse inattendue de la machine")
}

pub(crate) async fn lister(
    State(relais): State<Arc<Relais>>,
    Path(id): Path<String>,
    Query(Chemin { chemin }): Query<Chemin>,
) -> Response {
    match demander(&relais, &id, OrdreFichiers::Lister { chemin }).await {
        Ok(ReponseFichiers::Dossier {
            chemin,
            entrees,
            tronque,
        }) => Json(json!({ "chemin": chemin, "entrees": entrees, "tronque": tronque }))
            .into_response(),
        Ok(_) => inattendue(),
        Err(reponse) => reponse,
    }
}

pub(crate) async fn partiel(
    State(relais): State<Arc<Relais>>,
    Path(id): Path<String>,
    Query(Chemin { chemin }): Query<Chemin>,
) -> Response {
    match demander(&relais, &id, OrdreFichiers::Partiel { chemin }).await {
        Ok(ReponseFichiers::Ecrit { taille }) => Json(json!({ "taille": taille })).into_response(),
        Ok(_) => inattendue(),
        Err(reponse) => reponse,
    }
}

pub(crate) async fn envoyer(
    State(relais): State<Arc<Relais>>,
    Extension(operateur): Extension<Operateur>,
    Adresse(adresse): Adresse,
    Path(id): Path<String>,
    Query(morceau): Query<Morceau>,
    donnees: Bytes,
) -> Response {
    if donnees.len() > MORCEAU_MAX as usize {
        return refus(StatusCode::PAYLOAD_TOO_LARGE, "morceau trop gros");
    }
    let ordre = OrdreFichiers::Ecrire {
        chemin: morceau.chemin.clone(),
        decalage: morceau.decalage,
        donnees: donnees.to_vec(),
        fin: morceau.fin,
    };
    let attendu = morceau.decalage + donnees.len() as u64;
    match demander(&relais, &id, ordre).await {
        Ok(ReponseFichiers::Ecrit { taille }) => {
            if morceau.fin && taille == attendu {
                relais.noter(
                    Some(&operateur.compte),
                    &adresse,
                    "fichier_recu",
                    &format!("{id}:{} ({taille} octets)", morceau.chemin),
                );
            }
            Json(json!({ "taille": taille })).into_response()
        }
        Ok(_) => inattendue(),
        Err(reponse) => reponse,
    }
}

/// Début demandé par l'en-tête `Range` (seule la forme `bytes=N-` est reconnue).
fn debut_demande(entetes: &HeaderMap) -> Option<u64> {
    let valeur = entetes.get(header::RANGE)?.to_str().ok()?;
    valeur
        .strip_prefix("bytes=")?
        .strip_suffix('-')?
        .parse()
        .ok()
}

/// `Content-Disposition` avec le nom encodé (RFC 6266 et 5987) : aucun caractère du nom ne
/// peut casser l'en-tête.
fn disposition(chemin: &str) -> HeaderValue {
    let nom = chemin
        .rsplit('/')
        .next()
        .filter(|n| !n.is_empty())
        .unwrap_or("fichier");
    let mut encode = String::new();
    for octet in nom.bytes() {
        if octet.is_ascii_alphanumeric() || b"-._~".contains(&octet) {
            encode.push(octet as char);
        } else {
            encode.push_str(&format!("%{octet:02X}"));
        }
    }
    HeaderValue::from_str(&format!("attachment; filename*=UTF-8''{encode}"))
        .unwrap_or(HeaderValue::from_static("attachment"))
}

pub(crate) async fn telecharger(
    State(relais): State<Arc<Relais>>,
    Extension(operateur): Extension<Operateur>,
    Adresse(adresse): Adresse,
    Path(id): Path<String>,
    Query(Chemin { chemin }): Query<Chemin>,
    entetes: HeaderMap,
) -> Response {
    let debut = debut_demande(&entetes).unwrap_or(0);
    let lire = |decalage, taille| OrdreFichiers::Lire {
        chemin: chemin.clone(),
        decalage,
        taille,
    };
    let (premier, total) = match demander(&relais, &id, lire(debut, MORCEAU_MAX)).await {
        Ok(ReponseFichiers::Morceau {
            donnees,
            taille_totale,
        }) => (donnees, taille_totale),
        Ok(_) => return inattendue(),
        Err(reponse) => return reponse,
    };
    if debut > 0 && debut >= total {
        return (
            StatusCode::RANGE_NOT_SATISFIABLE,
            [(header::CONTENT_RANGE, format!("bytes */{total}"))],
        )
            .into_response();
    }
    if debut == 0 {
        relais.noter(
            Some(&operateur.compte),
            &adresse,
            "fichier_telecharge",
            &format!("{id}:{chemin} ({total} octets)"),
        );
    }

    let nom = disposition(&chemin);
    // Le reste est lu morceau par morceau, au rythme du navigateur, sans dépasser la taille
    // annoncée même si le fichier grandit entre-temps.
    let suite = debut + premier.len() as u64;
    let etat = (relais, id, chemin, suite, Some(premier));
    let flux =
        futures_util::stream::unfold(etat, move |(relais, id, chemin, decalage, premier)| {
            async move {
                if let Some(donnees) = premier {
                    return Some((
                        Ok(Bytes::from(donnees)),
                        (relais, id, chemin, decalage, None),
                    ));
                }
                if decalage >= total {
                    return None;
                }
                let taille = (total - decalage).min(u64::from(MORCEAU_MAX)) as u32;
                let ordre = OrdreFichiers::Lire {
                    chemin: chemin.clone(),
                    decalage,
                    taille,
                };
                let erreur = match demander(&relais, &id, ordre).await {
                    Ok(ReponseFichiers::Morceau { donnees, .. }) if !donnees.is_empty() => {
                        let suite = decalage + donnees.len() as u64;
                        return Some((Ok(Bytes::from(donnees)), (relais, id, chemin, suite, None)));
                    }
                    Ok(_) => "fichier raccourci pendant la lecture".to_owned(),
                    Err(reponse) => format!("lecture interrompue ({})", reponse.status()),
                };
                tracing::warn!("téléchargement de {id}:{chemin} : {erreur}");
                // Une erreur coupe la réponse : le navigateur voit un téléchargement inachevé.
                Some((
                    Err(std::io::Error::other(erreur)),
                    (relais, id, chemin, total, None),
                ))
            }
        });

    let mut reponse = Response::new(Body::from_stream(flux));
    let en_tetes = reponse.headers_mut();
    en_tetes.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    en_tetes.insert(header::CONTENT_DISPOSITION, nom);
    en_tetes.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    en_tetes.insert(header::CONTENT_LENGTH, HeaderValue::from(total - debut));
    if debut > 0 {
        *reponse.status_mut() = StatusCode::PARTIAL_CONTENT;
        reponse.headers_mut().insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {debut}-{}/{total}", total - 1))
                .expect("en-tête ASCII"),
        );
    }
    reponse
}
