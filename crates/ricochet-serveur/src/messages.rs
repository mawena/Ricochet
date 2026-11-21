//! Messages des machines : réception depuis les agents et routes de l'API.
//!
//! - `GET  /api/messages?machine=&source=&non_lus=1&limite=` : la liste, avec le nombre total de
//!   messages non lus ;
//! - `GET  /api/messages/{id}` : un message (il est alors marqué lu) ;
//! - `GET  /api/messages/{id}/detail` : le détail, demandé à l'agent à ce moment-là ; le relais
//!   le transmet sans jamais l'enregistrer ;
//! - `POST /api/messages/{id}/reponse` `{"bouton": "…"}` : une seule réponse par message, gardée
//!   par le relais et transmise à l'agent (tout de suite, ou à sa prochaine connexion).

use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};

use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use ricochet_protocole::{MessageMachine, RelaisVersAgent};
use serde::Deserialize;
use serde_json::json;

use crate::{
    Adresse, Relais,
    api::{Erreur, Operateur},
    base::{Enregistre, FiltreMessages},
    push::{self, Charge},
    registre::{Commande, ErreurEnvoi, maintenant},
};

type Resultat = Result<Response, Erreur>;

/// Messages d'une machine comptés sur cette durée pour la limite de débit.
const FENETRE_DEBIT_S: u64 = 60;

/// Limite de débit des messages, par machine (fenêtre glissante d'une minute).
#[derive(Default)]
pub struct Debit {
    instants: Mutex<HashMap<String, VecDeque<u64>>>,
}

impl Debit {
    /// Note un message de la machine ; `false` si elle dépasse `maximum` messages par minute.
    pub fn autorise(&self, machine: &str, instant: u64, maximum: usize) -> bool {
        let mut tous = self.instants.lock().unwrap_or_else(|e| e.into_inner());
        let file = tous.entry(machine.to_owned()).or_default();
        while file.front().is_some_and(|t| t + FENETRE_DEBIT_S <= instant) {
            file.pop_front();
        }
        if file.len() >= maximum {
            return false;
        }
        file.push_back(instant);
        true
    }
}

fn source_valide(source: &str) -> bool {
    (1..=32).contains(&source.len())
        && source
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

/// Le relais ne fait pas aveuglément confiance à l'agent : mêmes limites que le contrat, un peu
/// élargies pour un agent plus récent.
fn message_valide(message: &MessageMachine) -> Result<(), &'static str> {
    if message.id.is_empty()
        || message.id.len() > 64
        || !message
            .id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
    {
        return Err("identifiant invalide");
    }
    if !source_valide(&message.source) {
        return Err("source invalide");
    }
    if message.titre.trim().is_empty() || message.titre.chars().count() > 120 {
        return Err("titre invalide");
    }
    if message.texte.chars().count() > 300 {
        return Err("texte trop long");
    }
    if message.boutons.len() > 3
        || message
            .boutons
            .iter()
            .any(|b| b.id.is_empty() || b.id.len() > 32 || b.libelle.chars().count() > 40)
    {
        return Err("boutons invalides");
    }
    Ok(())
}

/// Reçoit un message d'un agent : l'enregistre et, s'il est nouveau, le pousse en notification.
/// Renvoie vrai si l'agent doit recevoir l'accusé (le message est enregistré, déjà connu, ou
/// définitivement refusé : dans tous les cas il ne doit plus le renvoyer).
pub fn recevoir(
    relais: &Arc<Relais>,
    machine: &str,
    adresse: &str,
    message: MessageMachine,
) -> bool {
    if let Err(raison) = message_valide(&message) {
        relais.noter(
            None,
            adresse,
            "message_refuse",
            &format!("{machine} : {raison}"),
        );
        return true;
    }
    let instant = maintenant();
    match relais.base.enregistrer_message(machine, &message, instant) {
        Ok(Enregistre::Nouveau) => {}
        Ok(Enregistre::Deja) => return true,
        Ok(Enregistre::AutreMachine) => {
            relais.noter(
                None,
                adresse,
                "message_refuse",
                &format!("{machine} : identifiant pris"),
            );
            return true;
        }
        Err(erreur) => {
            // Pas d'accusé : l'agent renverra le message à sa prochaine connexion.
            tracing::error!("enregistrement d'un message : {erreur:#}");
            return false;
        }
    }
    relais.noter(
        None,
        adresse,
        "message_recu",
        &format!(
            "{machine} : {} {} ({})",
            message.source,
            message.id,
            message.genre.texte()
        ),
    );
    if !relais
        .limite_messages
        .autorise(machine, instant, relais.config.messages.limite_par_minute)
    {
        relais.noter(
            None,
            adresse,
            "message_limite",
            &format!("{machine} : trop de messages, pas de notification"),
        );
        return true;
    }
    let nom = relais
        .base
        .machines()
        .ok()
        .and_then(|m| m.into_iter().find(|m| m.id == machine))
        .and_then(|m| m.nom)
        .unwrap_or_else(|| machine.to_owned());
    let charge = Charge {
        id: message.id,
        titre: message.titre,
        texte: message.texte,
        machine: nom,
        genre: message.genre,
    };
    let relais = relais.clone();
    tokio::spawn(async move {
        push::diffuser(&relais, charge).await;
    });
    true
}

fn refus(statut: StatusCode, texte: &str) -> Response {
    (statut, texte.to_owned()).into_response()
}

#[derive(Deserialize)]
pub(crate) struct Filtre {
    machine: Option<String>,
    source: Option<String>,
    #[serde(default)]
    non_lus: bool,
    limite: Option<u32>,
}

pub(crate) async fn liste(
    State(relais): State<Arc<Relais>>,
    Query(filtre): Query<Filtre>,
) -> Resultat {
    let filtre_base = FiltreMessages {
        machine: filtre.machine,
        source: filtre.source,
        non_lus: filtre.non_lus,
    };
    let (messages, non_lus) = relais
        .base
        .messages(&filtre_base, filtre.limite.unwrap_or(50).clamp(1, 200))?;
    Ok(Json(json!({ "messages": messages, "non_lus": non_lus })).into_response())
}

pub(crate) async fn un(State(relais): State<Arc<Relais>>, Path(id): Path<String>) -> Resultat {
    let Some(message) = relais.base.message(&id)? else {
        return Ok(refus(StatusCode::NOT_FOUND, "message inconnu"));
    };
    relais.base.marquer_lu(&id, maintenant())?;
    Ok(Json(relais.base.message(&id)?.unwrap_or(message)).into_response())
}

/// Le détail d'un message : demandé à l'agent, transmis tel quel, jamais enregistré.
pub(crate) async fn detail(
    State(relais): State<Arc<Relais>>,
    Extension(operateur): Extension<Operateur>,
    Adresse(adresse): Adresse,
    Path(id): Path<String>,
) -> Resultat {
    let Some(message) = relais.base.message(&id)? else {
        return Ok(refus(StatusCode::NOT_FOUND, "message inconnu"));
    };
    if !message.detail {
        return Ok(refus(StatusCode::NOT_FOUND, "ce message n'a pas de détail"));
    }
    relais.noter(
        Some(&operateur.compte),
        &adresse,
        "message_detail",
        &format!("{} : {id}", message.machine),
    );
    let (demande, reception) = match relais.registre.demander_detail(&message.machine, &id) {
        Ok(demande) => demande,
        Err(ErreurEnvoi::Deconnectee) => {
            return Ok(refus(
                StatusCode::CONFLICT,
                "machine hors ligne : le détail reste sur la machine et ne peut pas être lu \
                 tant que son agent n'est pas reconnecté",
            ));
        }
        Err(ErreurEnvoi::SansMessages) => {
            return Ok(refus(
                StatusCode::FORBIDDEN,
                "messages désactivés sur cette machine (section [messages] de l'agent)",
            ));
        }
        Err(_) => return Ok(refus(StatusCode::SERVICE_UNAVAILABLE, "machine occupée")),
    };
    let delai = Duration::from_secs(relais.config.messages.delai_detail_s);
    match tokio::time::timeout(delai, reception).await {
        Ok(Ok(Ok(texte))) => {
            Ok(([(header::CONTENT_TYPE, "text/plain; charset=utf-8")], texte).into_response())
        }
        Ok(Ok(Err(raison))) => Ok(refus(StatusCode::NOT_FOUND, &raison)),
        Ok(Err(_)) => Ok(refus(
            StatusCode::CONFLICT,
            "machine hors ligne : la connexion de l'agent s'est coupée",
        )),
        Err(_) => {
            relais.registre.oublier_detail(demande);
            Ok(refus(
                StatusCode::GATEWAY_TIMEOUT,
                "la machine ne répond pas",
            ))
        }
    }
}

#[derive(Deserialize)]
pub(crate) struct Reponse {
    bouton: String,
}

pub(crate) async fn repondre(
    State(relais): State<Arc<Relais>>,
    Extension(operateur): Extension<Operateur>,
    Adresse(adresse): Adresse,
    Path(id): Path<String>,
    Json(corps): Json<Reponse>,
) -> Resultat {
    let Some(message) = relais.base.message(&id)? else {
        return Ok(refus(StatusCode::NOT_FOUND, "message inconnu"));
    };
    if !message.boutons.iter().any(|b| b.id == corps.bouton) {
        return Ok(refus(
            StatusCode::BAD_REQUEST,
            "ce message n'a pas ce bouton",
        ));
    }
    let instant = maintenant();
    if !relais
        .base
        .repondre_message(&id, &corps.bouton, &operateur.compte, instant)?
    {
        return Ok(refus(
            StatusCode::CONFLICT,
            "ce message a déjà reçu une réponse",
        ));
    }
    relais.noter(
        Some(&operateur.compte),
        &adresse,
        "message_reponse",
        &format!("{} : {id} -> {}", message.machine, corps.bouton),
    );
    // Hors ligne ou saturé : la réponse reste dans la base et part à la prochaine connexion.
    let transmise = relais
        .registre
        .envoyer(
            &message.machine,
            Commande::Envoyer(RelaisVersAgent::Reponse {
                id,
                bouton: corps.bouton,
                par: operateur.compte,
                quand: instant,
            }),
        )
        .is_ok();
    Ok(Json(json!({ "transmise": transmise })).into_response())
}

/// Renvoie à l'agent qui vient de se connecter les réponses qu'il n'a pas encore accusées.
pub fn reponses_en_attente(relais: &Relais, machine: &str) -> Vec<RelaisVersAgent> {
    match relais.base.reponses_a_livrer(machine) {
        Ok(liste) => liste
            .into_iter()
            .map(|(id, bouton, par, quand)| RelaisVersAgent::Reponse {
                id,
                bouton,
                par,
                quand,
            })
            .collect(),
        Err(erreur) => {
            tracing::error!("réponses à livrer : {erreur:#}");
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ricochet_protocole::{BoutonMessage, GenreMessage};

    fn message() -> MessageMachine {
        MessageMachine {
            id: "ab12".into(),
            source: "cofiticket".into(),
            titre: "t".into(),
            texte: String::new(),
            genre: GenreMessage::Info,
            boutons: vec![],
            date: 1,
            detail: false,
        }
    }

    #[test]
    fn validation_cote_relais() {
        assert!(message_valide(&message()).is_ok());
        let mut m = message();
        m.id = "../x".into();
        assert!(message_valide(&m).is_err());
        let mut m = message();
        m.source = "Mauvaise".into();
        assert!(message_valide(&m).is_err());
        let mut m = message();
        m.titre = " ".into();
        assert!(message_valide(&m).is_err());
        let mut m = message();
        m.boutons = (0..4)
            .map(|i| BoutonMessage {
                id: format!("b{i}"),
                libelle: "x".into(),
            })
            .collect();
        assert!(message_valide(&m).is_err());
    }

    #[test]
    fn debit_par_machine() {
        let debit = Debit::default();
        for i in 0..3 {
            assert!(debit.autorise("pc", 100 + i, 3));
        }
        assert!(!debit.autorise("pc", 110, 3));
        assert!(
            debit.autorise("vps", 110, 3),
            "limite propre à chaque machine"
        );
        assert!(debit.autorise("pc", 160, 3), "la fenêtre glisse");
    }
}
