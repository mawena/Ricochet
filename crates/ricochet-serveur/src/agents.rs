//! Connexions WebSocket des agents.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::Response,
};
use ricochet_protocole::{
    AgentVersRelais, InfoMachine, Refus, RelaisVersAgent, VERSION_PROTOCOLE, decoder, encoder,
};

use crate::{
    Adresse, Relais, jeton,
    registre::{Commande, EvenementAppli, EvenementTerminal},
};

/// Délai laissé à l'agent pour se présenter.
const DELAI_PRESENTATION: Duration = Duration::from_secs(10);
/// Période des pings envoyés à l'agent.
const PERIODE_PING: Duration = Duration::from_secs(20);
/// Sans aucun message de l'agent pendant ce temps, la connexion est jugée morte.
const DELAI_SILENCE: Duration = Duration::from_secs(60);
/// Attente maximale quand le navigateur ne suit pas la sortie d'un terminal : au-delà, le
/// terminal est fermé plutôt que de bloquer toute la connexion de l'agent.
const DELAI_TERMINAL: Duration = Duration::from_secs(5);
/// Taille maximale d'un message d'agent.
const TAILLE_MAX: usize = 2 << 20;

pub async fn connexion(
    ws: WebSocketUpgrade,
    State(relais): State<Arc<Relais>>,
    Adresse(adresse): Adresse,
) -> Response {
    ws.max_message_size(TAILLE_MAX)
        .on_upgrade(move |socket| session(socket, relais, adresse))
}

async fn envoyer(socket: &mut WebSocket, message: &RelaisVersAgent) -> bool {
    socket
        .send(Message::Text(encoder(message).into()))
        .await
        .is_ok()
}

async fn refuser(mut socket: WebSocket, raison: Refus) {
    envoyer(&mut socket, &RelaisVersAgent::Refus { raison }).await;
    let _ = socket.send(Message::Close(None)).await;
}

/// Attend le `Bonjour` de l'agent et vérifie son identité.
async fn presentation(
    socket: &mut WebSocket,
    relais: &Relais,
) -> Result<(String, InfoMachine), (Refus, Option<String>)> {
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
        return Err((Refus::Protocole, None));
    };
    if version_protocole != VERSION_PROTOCOLE {
        return Err((Refus::Version, Some(id)));
    }
    let empreinte = relais.base.empreinte_machine(&id).unwrap_or_else(|erreur| {
        tracing::error!("lecture de la machine « {id} » : {erreur:#}");
        None
    });
    match empreinte {
        Some(empreinte) if jeton::verifier(&jeton, &empreinte) => Ok((id, info)),
        _ => Err((Refus::Identification, Some(id))),
    }
}

async fn session(mut socket: WebSocket, relais: Arc<Relais>, adresse: String) {
    let (id, info) = match presentation(&mut socket, &relais).await {
        Ok(presente) => presente,
        Err((raison, id)) => {
            let detail = format!("{} : {raison}", id.as_deref().unwrap_or("?"));
            relais.noter(None, &adresse, "agent_refuse", &detail);
            return refuser(socket, raison).await;
        }
    };
    relais.noter(
        None,
        &adresse,
        "agent_connecte",
        &format!("{id} ({})", info.nom_hote),
    );
    let (numero, mut commandes) = relais.registre.ouvrir_session(&id, info, adresse.clone());
    // La machine a pu être révoquée pendant la présentation.
    if !matches!(relais.base.empreinte_machine(&id), Ok(Some(_))) {
        relais.registre.retirer(&id);
        return;
    }
    let bienvenue = RelaisVersAgent::Bienvenue {
        intervalle_etat_s: relais.config.intervalle_etat_s,
    };

    // Les réponses que l'agent n'a pas encore accusées (il était hors ligne) repartent d'abord.
    let mut accueilli = envoyer(&mut socket, &bienvenue).await;
    if accueilli {
        for reponse in crate::messages::reponses_en_attente(&relais, &id) {
            if !envoyer(&mut socket, &reponse).await {
                accueilli = false;
                break;
            }
        }
    }
    if accueilli {
        boucle(&mut socket, &relais, &adresse, &id, numero, &mut commandes).await;
    }
    relais.registre.fermer_session(&id, numero);
    relais.noter(None, &adresse, "agent_deconnecte", &id);
}

async fn boucle(
    socket: &mut WebSocket,
    relais: &Arc<Relais>,
    adresse: &str,
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
                        Ok(AgentVersRelais::Sortie { terminal, donnees }) => {
                            let Some(sortie) = relais.registre.sortie_terminal(id, numero, terminal)
                            else {
                                continue;
                            };
                            let envoi = sortie.send(EvenementTerminal::Sortie(donnees));
                            if !matches!(tokio::time::timeout(DELAI_TERMINAL, envoi).await, Ok(Ok(())))
                                && relais.registre.fermer_terminal(terminal).is_some()
                            {
                                tracing::warn!("agent « {id} » : terminal {terminal} saturé, fermé");
                                let fermer = RelaisVersAgent::FermerTerminal { terminal };
                                if !envoyer(socket, &fermer).await {
                                    return;
                                }
                            }
                        }
                        Ok(AgentVersRelais::TerminalFerme { terminal, raison }) => {
                            relais.registre.terminal_ferme(id, numero, terminal, raison);
                        }
                        Ok(AgentVersRelais::Fichiers { requete, reponse }) => {
                            relais.registre.reponse_fichiers(id, numero, requete, reponse);
                        }
                        Ok(AgentVersRelais::Message { message }) => {
                            let identifiant = message.id.clone();
                            if crate::messages::recevoir(relais, id, adresse, message)
                                && !envoyer(socket, &RelaisVersAgent::MessageRecu { id: identifiant }).await
                            {
                                return;
                            }
                        }
                        Ok(AgentVersRelais::Detail { demande, detail, erreur }) => {
                            let reponse = match detail {
                                Some(texte) => Ok(texte),
                                None => Err(erreur.unwrap_or_else(|| "détail indisponible".into())),
                            };
                            relais.registre.reponse_detail(id, numero, demande, reponse);
                        }
                        Ok(AgentVersRelais::ReponseRecue { id: message }) => {
                            if let Err(erreur) = relais.base.reponse_livree(id, &message) {
                                tracing::error!("accusé de réponse : {erreur:#}");
                            }
                        }
                        Ok(AgentVersRelais::EnteteAppli { requete, statut, entetes }) => {
                            if !transmettre_appli(relais, id, numero, requete, EvenementAppli::Entete { statut, entetes }).await
                                && !envoyer(socket, &RelaisVersAgent::AnnulerAppli { requete }).await
                            {
                                return;
                            }
                        }
                        Ok(AgentVersRelais::CorpsAppli { requete, donnees }) => {
                            if !transmettre_appli(relais, id, numero, requete, EvenementAppli::Corps(donnees)).await
                                && !envoyer(socket, &RelaisVersAgent::AnnulerAppli { requete }).await
                            {
                                return;
                            }
                        }
                        Ok(AgentVersRelais::FinAppli { requete, erreur }) => {
                            if let Some(file) = relais.registre.fin_appli(id, numero, requete) {
                                let _ = tokio::time::timeout(DELAI_TERMINAL, file.send(EvenementAppli::Fin(erreur))).await;
                            }
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

/// Transmet un évènement d'une application au navigateur. `false` : le navigateur ne suit plus
/// (parti, ou trop lent) et la requête est oubliée ; l'agent est alors à prévenir. Une requête
/// déjà oubliée (`AnnulerAppli` envoyé) ne donne pas lieu à un second ordre.
async fn transmettre_appli(
    relais: &Relais,
    id: &str,
    numero: u64,
    requete: ricochet_protocole::NumeroRequete,
    evenement: EvenementAppli,
) -> bool {
    let Some(file) = relais.registre.sortie_appli(id, numero, requete) else {
        return true;
    };
    let envoi = file.send(evenement);
    if matches!(
        tokio::time::timeout(DELAI_TERMINAL, envoi).await,
        Ok(Ok(()))
    ) {
        return true;
    }
    relais.registre.oublier_appli(requete).is_none()
}
