//! Connexion au relais : présentation, envoi de l'état et reconnexion automatique.

use std::{collections::HashSet, sync::Arc, time::Duration};

use anyhow::{Context, bail};
use futures_util::{SinkExt, StreamExt};
use ricochet_protocole::{
    AgentVersRelais, Refus, RelaisVersAgent, VERSION_PROTOCOLE, decoder, encoder,
};
use tokio::time::{Instant, sleep, sleep_until};
use tokio_tungstenite::tungstenite::Message;

use crate::{
    Config,
    fichiers::Fichiers,
    messages::{self, Messagerie},
    systeme::{Collecteur, info_machine},
    terminaux::{Evenement, Terminaux},
    tunnels::Tunnels,
};

/// Délai laissé au relais pour répondre au bonjour.
const DELAI_ACCUEIL: Duration = Duration::from_secs(10);
/// Le relais envoie un ping toutes les 20 s : sans nouvelles pendant ce temps, il est injoignable.
const DELAI_SILENCE: Duration = Duration::from_secs(90);
/// Attente maximale entre deux essais de connexion.
const ATTENTE_MAX: Duration = Duration::from_secs(60);
/// Attente après un refus : la configuration doit changer, inutile d'insister.
const ATTENTE_REFUS: Duration = Duration::from_secs(300);

/// Comment une session s'est terminée.
#[derive(Debug, PartialEq, Eq)]
pub enum Fin {
    /// La connexion a été établie puis fermée.
    Fermee,
    /// Le relais a refusé l'agent.
    Refusee(Refus),
}

/// Attente avant le n-ième nouvel essai : 1, 2, 4… secondes, jusqu'à une minute.
pub fn attente(echecs: u32) -> Duration {
    Duration::from_secs(1u64 << echecs.min(6)).min(ATTENTE_MAX)
}

/// Reste connecté au relais pour toujours, en se reconnectant après chaque coupure.
pub async fn executer(config: Config) -> anyhow::Result<std::convert::Infallible> {
    let mut collecteur = Collecteur::new().context("lecture des informations système")?;
    let messagerie = demarrer_messages(&config);
    let mut echecs = 0;
    loop {
        let delai = match session_avec(&config, &mut collecteur, messagerie.as_ref()).await {
            Ok(Fin::Fermee) => {
                tracing::warn!("connexion au relais fermée, reconnexion");
                echecs = 0;
                attente(0)
            }
            Ok(Fin::Refusee(raison)) => {
                tracing::error!("le relais refuse cet agent : {raison}");
                ATTENTE_REFUS
            }
            Err(erreur) => {
                let delai = attente(echecs);
                echecs += 1;
                tracing::warn!("relais injoignable ({erreur:#}), nouvel essai dans {delai:?}");
                delai
            }
        };
        sleep(delai).await;
    }
}

/// Ouvre la file des messages et le socket si la configuration le demande. Un échec est signalé
/// sans empêcher l'agent de servir le reste (terminal, fichiers…) : la capacité n'est alors pas
/// annoncée au relais.
fn demarrer_messages(config: &Config) -> Option<Arc<Messagerie>> {
    let reglage = config.messages.as_ref()?;
    let messagerie = match Messagerie::ouvrir(reglage) {
        Ok(messagerie) => messagerie,
        Err(erreur) => {
            tracing::error!(
                "messages désactivés : dossier d'état {} inutilisable ({erreur})",
                reglage.dossier_etat.display()
            );
            return None;
        }
    };
    tokio::spawn(messages::purger_periodiquement(messagerie.clone()));
    let socket = reglage.socket.clone();
    let pour_socket = messagerie.clone();
    tokio::spawn(async move {
        if let Err(erreur) = messages::servir(pour_socket, socket.clone()).await {
            tracing::error!("messages : socket {} : {erreur}", socket.display());
        }
    });
    Some(messagerie)
}

/// Dit à la messagerie si la session avec le relais est ouverte, jusqu'à sa fin.
struct GardeSession(Arc<Messagerie>);

impl GardeSession {
    fn new(messagerie: &Arc<Messagerie>) -> Self {
        messagerie.relais_connecte(true);
        Self(messagerie.clone())
    }
}

impl Drop for GardeSession {
    fn drop(&mut self) {
        self.0.relais_connecte(false);
    }
}

fn texte(message: &AgentVersRelais) -> Message {
    Message::text(encoder(message))
}

/// Une session complète : connexion, présentation, puis échanges jusqu'à la coupure.
pub async fn session(config: &Config, collecteur: &mut Collecteur) -> anyhow::Result<Fin> {
    session_avec(config, collecteur, None).await
}

/// Comme `session`, avec la file des messages des programmes de la machine.
pub async fn session_avec(
    config: &Config,
    collecteur: &mut Collecteur,
    messagerie: Option<&Arc<Messagerie>>,
) -> anyhow::Result<Fin> {
    // Sans fournisseur installé, la première connexion wss:// panique. Déjà fait : sans effet.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (mut ws, _) = tokio_tungstenite::connect_async(config.relais.as_str())
        .await
        .with_context(|| format!("connexion à {}", config.relais))?;
    ws.send(texte(&AgentVersRelais::Bonjour {
        version_protocole: VERSION_PROTOCOLE,
        id: config.id.clone(),
        jeton: config.jeton.clone(),
        info: info_machine(
            config.terminal.is_some(),
            config.fichiers.is_some(),
            config
                .tunnel
                .as_ref()
                .map(|t| t.ports.clone())
                .unwrap_or_default(),
            messagerie.is_some(),
        ),
    }))
    .await?;

    let intervalle = loop {
        let message = tokio::time::timeout(DELAI_ACCUEIL, ws.next())
            .await
            .context("pas de réponse du relais")?;
        match message {
            Some(Ok(Message::Text(t))) => match decoder::<RelaisVersAgent>(&t)? {
                RelaisVersAgent::Bienvenue { intervalle_etat_s } => break intervalle_etat_s,
                RelaisVersAgent::Refus { raison } => return Ok(Fin::Refusee(raison)),
                autre => bail!("message inattendu avant l'accueil : {autre:?}"),
            },
            Some(Ok(Message::Close(_))) | None => bail!("le relais a fermé la connexion"),
            Some(Ok(_)) => continue,
            Some(Err(erreur)) => return Err(erreur.into()),
        }
    };
    tracing::info!("connecté au relais en tant que « {} »", config.id);

    // Les terminaux appartiennent à la session : ils sont fermés quand elle se termine.
    let (mut terminaux, mut evenements) = Terminaux::new(config.terminal.clone());
    let mut tunnels = Tunnels::new(config.tunnel.clone(), terminaux.evenements());
    let (fichiers, mut reponses_fichiers) = Fichiers::new(config.fichiers.clone());
    let mut minuterie = tokio::time::interval(Duration::from_secs(intervalle.max(1)));
    let mut dernier_message = Instant::now();
    // Messages envoyés pendant cette session : un nouvel envoi à chaque connexion tant que le
    // relais ne les a pas acceptés.
    let mut envoyes = HashSet::new();
    let _garde = messagerie.map(GardeSession::new);
    if let Some(messagerie) = messagerie {
        envoyer_messages(&mut ws, messagerie, &mut envoyes).await?;
    }
    loop {
        tokio::select! {
            _ = async {
                match messagerie {
                    Some(m) => m.attendre().await,
                    None => std::future::pending().await,
                }
            } => {
                if let Some(messagerie) = messagerie {
                    envoyer_messages(&mut ws, messagerie, &mut envoyes).await?;
                }
            }
            _ = minuterie.tick() => {
                ws.send(texte(&AgentVersRelais::Etat { etat: collecteur.etat() })).await?;
            }
            _ = sleep_until(dernier_message + DELAI_SILENCE) => bail!("relais silencieux"),
            Some(evenement) = evenements.recv() => {
                let message = match evenement {
                    Evenement::Sortie(terminal, donnees) => {
                        AgentVersRelais::Sortie { terminal, donnees }
                    }
                    Evenement::Ferme(terminal, raison) => {
                        terminaux.oublier(terminal);
                        tunnels.oublier(terminal);
                        tracing::info!("terminal ou tunnel {terminal} fermé : {raison}");
                        AgentVersRelais::TerminalFerme { terminal, raison }
                    }
                };
                ws.send(texte(&message)).await?;
            }
            Some((requete, reponse)) = reponses_fichiers.recv() => {
                ws.send(texte(&AgentVersRelais::Fichiers { requete, reponse })).await?;
            }
            message = ws.next() => {
                dernier_message = Instant::now();
                match message {
                    Some(Ok(Message::Text(t))) => match decoder::<RelaisVersAgent>(&t) {
                        Ok(message) => {
                            if let Some(reponse) = traiter(message, collecteur, &mut terminaux, &mut tunnels, &fichiers, messagerie) {
                                ws.send(texte(&reponse)).await?;
                            }
                        }
                        Err(erreur) => tracing::warn!("message illisible du relais ({erreur})"),
                    },
                    Some(Ok(Message::Close(_))) | None => return Ok(Fin::Fermee),
                    Some(Ok(_)) => {}
                    Some(Err(erreur)) => return Err(erreur.into()),
                }
            }
        }
    }
}

/// Envoie au relais les messages qu'il n'a pas encore acceptés.
async fn envoyer_messages<S>(
    ws: &mut S,
    messagerie: &Messagerie,
    envoyes: &mut HashSet<String>,
) -> anyhow::Result<()>
where
    S: futures_util::Sink<Message, Error = tokio_tungstenite::tungstenite::Error> + Unpin,
{
    for message in messagerie.a_envoyer(envoyes) {
        ws.send(texte(&AgentVersRelais::Message { message }))
            .await?;
    }
    Ok(())
}

/// Exécute un ordre du relais ; renvoie la réponse éventuelle.
fn traiter(
    message: RelaisVersAgent,
    collecteur: &mut Collecteur,
    terminaux: &mut Terminaux,
    tunnels: &mut Tunnels,
    fichiers: &Fichiers,
    messagerie: Option<&Arc<Messagerie>>,
) -> Option<AgentVersRelais> {
    match message {
        RelaisVersAgent::DemandeEtat => Some(AgentVersRelais::Etat {
            etat: collecteur.etat(),
        }),
        RelaisVersAgent::OuvrirTerminal {
            terminal,
            colonnes,
            lignes,
        } => match terminaux.ouvrir(terminal, colonnes, lignes) {
            Ok(()) => {
                tracing::info!("terminal {terminal} ouvert");
                None
            }
            Err(raison) => {
                tracing::warn!("terminal {terminal} refusé : {raison}");
                Some(AgentVersRelais::TerminalFerme { terminal, raison })
            }
        },
        RelaisVersAgent::OuvrirTunnel { tunnel, port } => match tunnels.ouvrir(tunnel, port) {
            Ok(()) => {
                tracing::info!("tunnel {tunnel} ouvert vers le port {port}");
                None
            }
            Err(raison) => {
                tracing::warn!("tunnel {tunnel} refusé : {raison}");
                Some(AgentVersRelais::TerminalFerme {
                    terminal: tunnel,
                    raison,
                })
            }
        },
        RelaisVersAgent::Entree { terminal, donnees } => {
            if let Some(donnees) = tunnels.ecrire(terminal, donnees) {
                terminaux.ecrire(terminal, donnees);
            }
            None
        }
        RelaisVersAgent::Redimensionner {
            terminal,
            colonnes,
            lignes,
        } => {
            terminaux.redimensionner(terminal, colonnes, lignes);
            None
        }
        RelaisVersAgent::FermerTerminal { terminal } => {
            terminaux.fermer(terminal);
            tunnels.fermer(terminal);
            None
        }
        RelaisVersAgent::Fichiers { requete, ordre } => fichiers
            .demander(requete, ordre)
            .map(|(requete, reponse)| AgentVersRelais::Fichiers { requete, reponse }),
        RelaisVersAgent::MessageRecu { id } => {
            if let Some(messagerie) = messagerie {
                messagerie.accepter(&id);
            }
            None
        }
        RelaisVersAgent::DemandeDetail { demande, id } => {
            let resultat = match messagerie {
                Some(messagerie) => messagerie.detail(&id),
                None => Err("messages désactivés sur cette machine".into()),
            };
            Some(match resultat {
                Ok(detail) => AgentVersRelais::Detail {
                    demande,
                    detail: Some(detail),
                    erreur: None,
                },
                Err(erreur) => AgentVersRelais::Detail {
                    demande,
                    detail: None,
                    erreur: Some(erreur),
                },
            })
        }
        RelaisVersAgent::Reponse {
            id,
            bouton,
            par,
            quand,
        } => messagerie.map(|messagerie| {
            messagerie.enregistrer_reponse(&id, &bouton, &par, quand);
            AgentVersRelais::ReponseRecue { id }
        }),
        autre => {
            tracing::warn!("message inattendu du relais : {autre:?}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attente_croissante_et_bornee() {
        assert_eq!(attente(0), Duration::from_secs(1));
        assert_eq!(attente(3), Duration::from_secs(8));
        assert_eq!(attente(6), ATTENTE_MAX);
        assert_eq!(attente(100), ATTENTE_MAX);
    }

    /// Le relais public est en wss:// : la poignée de main TLS doit échouer proprement face à
    /// un pair qui n'en parle pas, sans paniquer faute de fournisseur de chiffrement.
    #[tokio::test]
    async fn wss_sans_panique() {
        let ecoute = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = ecoute.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((flux, _)) = ecoute.accept().await {
                drop(flux);
            }
        });
        let config = Config {
            relais: format!("wss://127.0.0.1:{port}/agent"),
            id: "essai".into(),
            jeton: "x".into(),
            terminal: None,
            fichiers: None,
            tunnel: None,
            messages: None,
        };
        let mut collecteur = Collecteur::new().unwrap();
        assert!(session(&config, &mut collecteur).await.is_err());
    }
}
