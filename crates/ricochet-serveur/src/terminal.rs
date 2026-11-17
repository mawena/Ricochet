//! Terminal dans le navigateur : un WebSocket entre l'opérateur et le relais, que le relais
//! prolonge jusqu'au shell de l'agent.
//!
//! Du navigateur au relais : trames binaires = ce que l'opérateur tape ; trames texte = JSON
//! `{"colonnes": …, "lignes": …}` quand la fenêtre change de taille. Du relais au navigateur :
//! trames binaires = ce que le shell affiche, puis une trame de fermeture avec la raison.

use std::{sync::Arc, time::Duration};

use axum::{
    Extension,
    extract::{
        Path, Query, State,
        ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use ricochet_protocole::{NumeroTerminal, RelaisVersAgent};
use serde::Deserialize;
use tokio::sync::mpsc;

use crate::{
    Adresse, Relais,
    api::{Erreur, Operateur},
    registre::{Commande, ErreurEnvoi, EvenementTerminal, maintenant},
};

/// Taille maximale d'un message du navigateur (un collage de texte, au plus).
const TAILLE_MAX: usize = 64 * 1024;
/// Période des pings au navigateur (nginx coupe une connexion muette) et de la vérification
/// de la session de l'opérateur.
const PERIODE: Duration = Duration::from_secs(30);

#[derive(Deserialize)]
pub(crate) struct Taille {
    #[serde(default = "colonnes_par_defaut")]
    colonnes: u16,
    #[serde(default = "lignes_par_defaut")]
    lignes: u16,
}

fn colonnes_par_defaut() -> u16 {
    80
}

fn lignes_par_defaut() -> u16 {
    24
}

/// Un navigateur ouvre ce WebSocket depuis n'importe quel site s'il le veut : seule
/// l'interface du relais est acceptée. Sans `Origin`, ce n'est pas un navigateur.
pub(crate) fn origine_acceptee(entetes: &HeaderMap) -> bool {
    let Some(origine) = entetes.get(header::ORIGIN) else {
        return true;
    };
    let hote = entetes.get(header::HOST).and_then(|v| v.to_str().ok());
    let origine = origine
        .to_str()
        .ok()
        .and_then(|o| o.split_once("://"))
        .map(|(_, reste)| reste);
    matches!((origine, hote), (Some(o), Some(h)) if o.eq_ignore_ascii_case(h))
}

pub(crate) async fn connexion(
    ws: WebSocketUpgrade,
    State(relais): State<Arc<Relais>>,
    Extension(operateur): Extension<Operateur>,
    Adresse(adresse): Adresse,
    Path(id): Path<String>,
    Query(taille): Query<Taille>,
    entetes: HeaderMap,
) -> Result<Response, Erreur> {
    if !origine_acceptee(&entetes) {
        return Ok((StatusCode::FORBIDDEN, "origine refusée").into_response());
    }
    if relais.base.empreinte_machine(&id)?.is_none() {
        return Ok((StatusCode::NOT_FOUND, "machine inconnue").into_response());
    }
    Ok(ws
        .max_message_size(TAILLE_MAX)
        .on_upgrade(move |socket| session(socket, relais, operateur, adresse, id, taille)))
}

pub(crate) async fn fermer(mut socket: WebSocket, raison: &str) {
    // Une raison de fermeture tient en 123 octets.
    let mut fin = raison.len().min(120);
    while !raison.is_char_boundary(fin) {
        fin -= 1;
    }
    let trame = CloseFrame {
        code: 1000,
        reason: raison[..fin].into(),
    };
    let _ = socket.send(Message::Close(Some(trame))).await;
}

async fn session(
    socket: WebSocket,
    relais: Arc<Relais>,
    operateur: Operateur,
    adresse: String,
    id: String,
    taille: Taille,
) {
    let ouvert = relais
        .registre
        .ouvrir_terminal(&id, taille.colonnes, taille.lignes);
    let (terminal, sortie) = match ouvert {
        Ok(ouvert) => ouvert,
        Err(erreur) => {
            let raison = match erreur {
                ErreurEnvoi::Deconnectee => "machine déconnectée",
                ErreurEnvoi::Saturee
                | ErreurEnvoi::SansFichiers
                | ErreurEnvoi::SansTunnel
                | ErreurEnvoi::TropDeTunnels => "machine occupée, réessayer",
                ErreurEnvoi::SansTerminal => {
                    "terminal désactivé sur cette machine (section [terminal] de l'agent)"
                }
                ErreurEnvoi::TropDeTerminaux => "trop de terminaux ouverts sur cette machine",
            };
            return fermer(socket, raison).await;
        }
    };
    let compte = Some(operateur.compte.as_str());
    let detail = format!("{id} n° {terminal}");
    relais.noter(compte, &adresse, "terminal_ouvert", &detail);

    let raison = relayer(socket, &relais, &operateur, &id, terminal, sortie).await;
    // Encore ouvert de ce côté : c'est l'opérateur qui part, arrêter le shell.
    if relais.registre.fermer_terminal(terminal).is_some() {
        let _ = relais.registre.envoyer(
            &id,
            Commande::Envoyer(RelaisVersAgent::FermerTerminal { terminal }),
        );
    }
    relais.noter(
        compte,
        &adresse,
        "terminal_ferme",
        &format!("{detail} : {raison}"),
    );
}

/// Échange entre le navigateur et l'agent jusqu'à la fin ; renvoie la raison de la fin.
async fn relayer(
    mut socket: WebSocket,
    relais: &Relais,
    operateur: &Operateur,
    id: &str,
    terminal: NumeroTerminal,
    mut sortie: mpsc::Receiver<EvenementTerminal>,
) -> String {
    let Some(agent) = relais.registre.file(id) else {
        fermer(socket, "machine déconnectée").await;
        return "machine déconnectée".into();
    };
    let mut periode = tokio::time::interval(PERIODE);
    periode.tick().await;
    let raison = loop {
        tokio::select! {
            evenement = sortie.recv() => match evenement {
                Some(EvenementTerminal::Sortie(donnees)) => {
                    if socket.send(Message::Binary(donnees.into())).await.is_err() {
                        return "navigateur parti".into();
                    }
                }
                Some(EvenementTerminal::Ferme(raison)) => break raison,
                None => break "terminal fermé par le relais (machine déconnectée ?)".into(),
            },
            message = socket.recv() => {
                let ordre = match message {
                    Some(Ok(Message::Binary(donnees))) if !donnees.is_empty() => {
                        RelaisVersAgent::Entree { terminal, donnees: donnees.to_vec() }
                    }
                    Some(Ok(Message::Text(texte))) => match serde_json::from_str::<Taille>(&texte) {
                        Ok(Taille { colonnes, lignes }) => {
                            RelaisVersAgent::Redimensionner { terminal, colonnes, lignes }
                        }
                        Err(_) => continue,
                    },
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => {
                        return "fermé par l'opérateur".into();
                    }
                    Some(Ok(_)) => continue,
                };
                if agent.send(Commande::Envoyer(ordre)).await.is_err() {
                    break "machine déconnectée".into();
                }
            }
            _ = periode.tick() => {
                if let Some(empreinte) = &operateur.session
                    && !matches!(relais.base.session(empreinte, maintenant()), Ok(Some(_)))
                {
                    break "session expirée".into();
                }
                if socket.send(Message::Ping(Default::default())).await.is_err() {
                    return "navigateur parti".into();
                }
            }
        }
    };
    fermer(socket, &raison).await;
    raison
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origine() {
        let mut entetes = HeaderMap::new();
        assert!(origine_acceptee(&entetes), "pas un navigateur");
        entetes.insert(header::HOST, "ricochet.mawena.cloud".parse().unwrap());
        entetes.insert(
            header::ORIGIN,
            "https://ricochet.mawena.cloud".parse().unwrap(),
        );
        assert!(origine_acceptee(&entetes));
        entetes.insert(header::ORIGIN, "https://evil.example".parse().unwrap());
        assert!(!origine_acceptee(&entetes));
        entetes.insert(header::ORIGIN, "null".parse().unwrap());
        assert!(!origine_acceptee(&entetes));
        entetes.remove(header::HOST);
        entetes.insert(
            header::ORIGIN,
            "https://ricochet.mawena.cloud".parse().unwrap(),
        );
        assert!(!origine_acceptee(&entetes));
    }
}
