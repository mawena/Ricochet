//! Tunnel vers un port local d'une machine : un WebSocket entre un client (la commande
//! `ricochet-agent tunnel`, lancée par SSH) et le relais, que le relais prolonge jusqu'à la
//! connexion TCP ouverte par l'agent. Trames binaires dans les deux sens, puis une trame de
//! fermeture avec la raison.

use std::{sync::Arc, time::Duration};

use axum::{
    Extension,
    extract::{
        Path, Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use ricochet_protocole::{NumeroTerminal, RelaisVersAgent};
use serde::Deserialize;
use tokio::sync::mpsc;

use crate::{
    Adresse, Relais,
    api::{Erreur, Operateur},
    registre::{Commande, ErreurEnvoi, EvenementTerminal, maintenant},
    terminal::{fermer, origine_acceptee},
};

/// Taille maximale d'un message du client.
const TAILLE_MAX: usize = 256 * 1024;
/// Période des pings au client (nginx coupe une connexion muette) et de la vérification de la
/// session de l'opérateur.
const PERIODE: Duration = Duration::from_secs(30);

#[derive(Deserialize)]
pub(crate) struct Cible {
    port: u16,
}

pub(crate) async fn connexion(
    ws: WebSocketUpgrade,
    State(relais): State<Arc<Relais>>,
    Extension(operateur): Extension<Operateur>,
    Adresse(adresse): Adresse,
    Path(id): Path<String>,
    Query(cible): Query<Cible>,
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
        .on_upgrade(move |socket| session(socket, relais, operateur, adresse, id, cible.port)))
}

async fn session(
    socket: WebSocket,
    relais: Arc<Relais>,
    operateur: Operateur,
    adresse: String,
    id: String,
    port: u16,
) {
    let compte = Some(operateur.compte.as_str());
    let (tunnel, sortie) = match relais.registre.ouvrir_tunnel(&id, port) {
        Ok(ouvert) => ouvert,
        Err(erreur) => {
            let raison = match erreur {
                ErreurEnvoi::Deconnectee => "machine déconnectée",
                ErreurEnvoi::SansTunnel => {
                    "port non autorisé sur cette machine (section [tunnel] de l'agent)"
                }
                ErreurEnvoi::TropDeTunnels => "trop de tunnels ouverts vers cette machine",
                _ => "machine occupée, réessayer",
            };
            relais.noter(
                compte,
                &adresse,
                "tunnel_refuse",
                &format!("{id} port {port} : {raison}"),
            );
            return fermer(socket, raison).await;
        }
    };
    let detail = format!("{id} port {port} n° {tunnel}");
    relais.noter(compte, &adresse, "tunnel_ouvert", &detail);

    let raison = relayer(socket, &relais, &operateur, &id, tunnel, sortie).await;
    // Encore ouvert de ce côté : c'est le client qui part, couper la connexion sur la machine.
    if relais.registre.fermer_terminal(tunnel).is_some() {
        let _ = relais.registre.envoyer(
            &id,
            Commande::Envoyer(RelaisVersAgent::FermerTerminal { terminal: tunnel }),
        );
    }
    relais.noter(
        compte,
        &adresse,
        "tunnel_ferme",
        &format!("{detail} : {raison}"),
    );
}

/// Échange entre le client et l'agent jusqu'à la fin ; renvoie la raison de la fin.
async fn relayer(
    mut socket: WebSocket,
    relais: &Relais,
    operateur: &Operateur,
    id: &str,
    tunnel: NumeroTerminal,
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
                        return "client parti".into();
                    }
                }
                Some(EvenementTerminal::Ferme(raison)) => break raison,
                None => break "tunnel fermé par le relais (machine déconnectée ?)".into(),
            },
            message = socket.recv() => {
                let donnees = match message {
                    Some(Ok(Message::Binary(donnees))) if !donnees.is_empty() => donnees.to_vec(),
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => {
                        return "fermé par le client".into();
                    }
                    Some(Ok(_)) => continue,
                };
                let ordre = RelaisVersAgent::Entree { terminal: tunnel, donnees };
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
                    return "client parti".into();
                }
            }
        }
    };
    fermer(socket, &raison).await;
    raison
}
