//! Connexions WebSocket des agents.

use std::{
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    extract::{
        ConnectInfo, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::HeaderMap,
    response::Response,
};
use ricochet_protocole::{
    AgentVersRelais, InfoMachine, Refus, RelaisVersAgent, VERSION_PROTOCOLE, decoder, encoder,
};

use crate::{Relais, jeton, registre::Commande};

/// Délai laissé à l'agent pour se présenter.
const DELAI_PRESENTATION: Duration = Duration::from_secs(10);
/// Période des pings envoyés à l'agent.
const PERIODE_PING: Duration = Duration::from_secs(20);
/// Sans aucun message de l'agent pendant ce temps, la connexion est jugée morte.
const DELAI_SILENCE: Duration = Duration::from_secs(60);
/// Taille maximale d'un message d'agent.
const TAILLE_MAX: usize = 1 << 20;

pub async fn connexion(
    ws: WebSocketUpgrade,
    State(relais): State<Arc<Relais>>,
    ConnectInfo(pair): ConnectInfo<SocketAddr>,
    entetes: HeaderMap,
) -> Response {
    let adresse = adresse_client(pair, &entetes);
    ws.max_message_size(TAILLE_MAX)
        .on_upgrade(move |socket| session(socket, relais, adresse))
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

async fn envoyer(socket: &mut WebSocket, message: &RelaisVersAgent) -> bool {
    socket
        .send(Message::Text(encoder(message).into()))
        .await
        .is_ok()
}

async fn refuser(mut socket: WebSocket, raison: Refus, adresse: &str) {
    tracing::warn!("agent refusé depuis {adresse} : {raison}");
    envoyer(&mut socket, &RelaisVersAgent::Refus { raison }).await;
    let _ = socket.send(Message::Close(None)).await;
}

/// Attend le `Bonjour` de l'agent et vérifie son identité.
async fn presentation(
    socket: &mut WebSocket,
    relais: &Relais,
) -> Result<(String, InfoMachine), Refus> {
    let message = match tokio::time::timeout(DELAI_PRESENTATION, socket.recv()).await {
        Ok(Some(Ok(Message::Text(texte)))) => decoder::<AgentVersRelais>(&texte).ok(),
        _ => None,
    };
    let Some(AgentVersRelais::Bonjour {
        version_protocole,
        id,
        jeton,
        info,
    }) = message
    else {
        return Err(Refus::Protocole);
    };
    if version_protocole != VERSION_PROTOCOLE {
        return Err(Refus::Version);
    }
    match relais.config.agent(&id) {
        Some(agent) if jeton::verifier(&jeton, &agent.jeton_sha256) => Ok((id, info)),
        _ => Err(Refus::Identification),
    }
}

async fn session(mut socket: WebSocket, relais: Arc<Relais>, adresse: String) {
    let (id, info) = match presentation(&mut socket, &relais).await {
        Ok(presente) => presente,
        Err(raison) => return refuser(socket, raison, &adresse).await,
    };
    tracing::info!(
        "agent « {id} » connecté depuis {adresse} ({})",
        info.nom_hote
    );
    let (numero, mut commandes) = relais.registre.ouvrir_session(&id, info, adresse);
    let bienvenue = RelaisVersAgent::Bienvenue {
        intervalle_etat_s: relais.config.intervalle_etat_s,
    };

    if envoyer(&mut socket, &bienvenue).await {
        boucle(&mut socket, &relais, &id, numero, &mut commandes).await;
    }
    relais.registre.fermer_session(&id, numero);
    tracing::info!("agent « {id} » déconnecté");
}

async fn boucle(
    socket: &mut WebSocket,
    relais: &Relais,
    id: &str,
    numero: u64,
    commandes: &mut tokio::sync::mpsc::Receiver<Commande>,
) {
    let mut ping = tokio::time::interval(PERIODE_PING);
    ping.tick().await;
    let mut dernier_message = Instant::now();
    loop {
        tokio::select! {
            commande = commandes.recv() => match commande {
                Some(Commande::Envoyer(message)) => {
                    if !envoyer(socket, &message).await {
                        return;
                    }
                }
                Some(Commande::Remplacer) | None => {
                    tracing::info!("agent « {id} » : connexion remplacée par une nouvelle");
                    let _ = socket.send(Message::Close(None)).await;
                    return;
                }
            },
            _ = ping.tick() => {
                if dernier_message.elapsed() > DELAI_SILENCE {
                    tracing::warn!("agent « {id} » silencieux, connexion fermée");
                    return;
                }
                if socket.send(Message::Ping(Default::default())).await.is_err() {
                    return;
                }
            }
            message = socket.recv() => {
                let Some(Ok(message)) = message else { return };
                dernier_message = Instant::now();
                relais.registre.toucher(id, numero);
                match message {
                    Message::Text(texte) => match decoder::<AgentVersRelais>(&texte) {
                        Ok(AgentVersRelais::Etat { etat }) => {
                            relais.registre.mettre_a_jour_etat(id, numero, etat);
                        }
                        Ok(AgentVersRelais::Bonjour { .. }) => {
                            tracing::warn!("agent « {id} » : second bonjour ignoré");
                        }
                        Err(erreur) => tracing::warn!("agent « {id} » : message illisible ({erreur})"),
                    },
                    Message::Close(_) => return,
                    _ => {}
                }
            }
        }
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
