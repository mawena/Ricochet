//! Connexion au relais : présentation, envoi de l'état et reconnexion automatique.

use std::time::Duration;

use anyhow::{Context, bail};
use futures_util::{SinkExt, StreamExt};
use ricochet_protocole::{
    AgentVersRelais, Refus, RelaisVersAgent, VERSION_PROTOCOLE, decoder, encoder,
};
use tokio::time::{Instant, sleep, sleep_until};
use tokio_tungstenite::tungstenite::Message;

use crate::{
    Config,
    systeme::{Collecteur, info_machine},
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
    let mut echecs = 0;
    loop {
        let delai = match session(&config, &mut collecteur).await {
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

fn texte(message: &AgentVersRelais) -> Message {
    Message::text(encoder(message))
}

/// Une session complète : connexion, présentation, puis échanges jusqu'à la coupure.
pub async fn session(config: &Config, collecteur: &mut Collecteur) -> anyhow::Result<Fin> {
    let (mut ws, _) = tokio_tungstenite::connect_async(config.relais.as_str())
        .await
        .with_context(|| format!("connexion à {}", config.relais))?;
    ws.send(texte(&AgentVersRelais::Bonjour {
        version_protocole: VERSION_PROTOCOLE,
        id: config.id.clone(),
        jeton: config.jeton.clone(),
        info: info_machine(),
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
                RelaisVersAgent::DemandeEtat => bail!("message inattendu avant l'accueil"),
            },
            Some(Ok(Message::Close(_))) | None => bail!("le relais a fermé la connexion"),
            Some(Ok(_)) => continue,
            Some(Err(erreur)) => return Err(erreur.into()),
        }
    };
    tracing::info!("connecté au relais en tant que « {} »", config.id);

    let mut minuterie = tokio::time::interval(Duration::from_secs(intervalle.max(1)));
    let mut dernier_message = Instant::now();
    loop {
        tokio::select! {
            _ = minuterie.tick() => {
                ws.send(texte(&AgentVersRelais::Etat { etat: collecteur.etat() })).await?;
            }
            _ = sleep_until(dernier_message + DELAI_SILENCE) => bail!("relais silencieux"),
            message = ws.next() => {
                dernier_message = Instant::now();
                match message {
                    Some(Ok(Message::Text(t))) => match decoder::<RelaisVersAgent>(&t) {
                        Ok(RelaisVersAgent::DemandeEtat) => {
                            ws.send(texte(&AgentVersRelais::Etat { etat: collecteur.etat() })).await?;
                        }
                        Ok(autre) => tracing::warn!("message inattendu du relais : {autre:?}"),
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
}
