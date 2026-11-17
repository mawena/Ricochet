//! Côté client d'un tunnel : relie l'entrée et la sortie standard à un port d'une machine, en
//! passant par le relais. Fait pour `ssh -o ProxyCommand="ricochet-agent tunnel %h"`.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::{
    Message,
    client::IntoClientRequest,
    http::{HeaderValue, header},
};

/// Taille d'une lecture de l'entrée standard (le relais accepte des messages plus gros).
const TAILLE_LECTURE: usize = 32 * 1024;

/// Configuration du client : où est le relais, et le jeton qui ouvre les tunnels.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigClient {
    /// Adresse du relais, par exemple `https://ricochet.mawena.cloud`.
    pub relais: String,
    /// Jeton des tunnels (`jeton_tunnel_sha256` du relais) ou jeton opérateur.
    pub jeton: String,
}

impl ConfigClient {
    /// Le fichier par défaut : `~/.config/ricochet/client.toml`.
    pub fn chemin_par_defaut() -> PathBuf {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .unwrap_or_else(|| PathBuf::from(".config"));
        config.join("ricochet/client.toml")
    }

    pub fn lire(chemin: &Path) -> anyhow::Result<Self> {
        let texte = std::fs::read_to_string(chemin)
            .with_context(|| format!("lecture de {}", chemin.display()))?;
        Self::depuis_texte(&texte).with_context(|| format!("configuration {}", chemin.display()))
    }

    pub fn depuis_texte(texte: &str) -> anyhow::Result<Self> {
        let config: Self = toml::from_str(texte)?;
        if config.jeton.is_empty() {
            bail!("jeton est obligatoire");
        }
        config.adresse("x", 1)?;
        Ok(config)
    }

    /// Adresse WebSocket du tunnel vers `port` de la machine `id`.
    pub fn adresse(&self, id: &str, port: u16) -> anyhow::Result<String> {
        let relais = self.relais.trim_end_matches('/');
        let base = if let Some(reste) = relais.strip_prefix("https://") {
            format!("wss://{reste}")
        } else if let Some(reste) = relais.strip_prefix("http://") {
            format!("ws://{reste}")
        } else {
            bail!("relais doit commencer par https:// (ou http:// pour un essai local)");
        };
        if id.is_empty()
            || !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            bail!("identifiant de machine invalide : « {id} »");
        }
        Ok(format!("{base}/api/machines/{id}/tunnel?port={port}"))
    }
}

/// Ouvre le tunnel et relaie l'entrée et la sortie standard jusqu'à la fin ; renvoie la raison
/// donnée par le relais, s'il en donne une.
pub async fn executer(config: &ConfigClient, id: &str, port: u16) -> anyhow::Result<String> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut requete = config.adresse(id, port)?.into_client_request()?;
    let jeton = HeaderValue::from_str(&format!("Bearer {}", config.jeton))
        .context("jeton illisible dans la configuration")?;
    requete.headers_mut().insert(header::AUTHORIZATION, jeton);
    let (ws, _) = tokio_tungstenite::connect_async(requete)
        .await
        .with_context(|| format!("ouverture du tunnel vers {id} par {}", config.relais))?;
    let (mut envoi, mut reception) = ws.split();
    let mut entree = tokio::io::stdin();
    let mut sortie = tokio::io::stdout();
    let mut tampon = vec![0; TAILLE_LECTURE];
    let mut entree_ouverte = true;
    loop {
        tokio::select! {
            lu = entree.read(&mut tampon), if entree_ouverte => match lu {
                // Fin de l'entrée : le tunnel reste ouvert pour ce qui doit encore revenir.
                Ok(0) | Err(_) => entree_ouverte = false,
                Ok(n) => envoi.send(Message::binary(tampon[..n].to_vec())).await?,
            },
            message = reception.next() => match message {
                Some(Ok(Message::Binary(donnees))) => {
                    sortie.write_all(&donnees).await?;
                    sortie.flush().await?;
                }
                Some(Ok(Message::Close(trame))) => {
                    return Ok(trame.map(|t| t.reason.to_string()).unwrap_or_default());
                }
                Some(Ok(_)) => {}
                Some(Err(erreur)) => return Err(erreur.into()),
                None => return Ok(String::new()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adresses() {
        let config =
            ConfigClient::depuis_texte("relais = \"https://r.exemple.org/\"\njeton = \"j\"")
                .unwrap();
        assert_eq!(
            config.adresse("pc-bureau", 22).unwrap(),
            "wss://r.exemple.org/api/machines/pc-bureau/tunnel?port=22"
        );
        assert!(config.adresse("a/b", 22).is_err());
        assert!(config.adresse("", 22).is_err());
        let local = ConfigClient::depuis_texte("relais = \"http://127.0.0.1:7870\"\njeton = \"j\"")
            .unwrap();
        assert!(
            local
                .adresse("pc", 22)
                .unwrap()
                .starts_with("ws://127.0.0.1:7870/api/")
        );
        assert!(ConfigClient::depuis_texte("relais = \"wss://r\"\njeton = \"j\"").is_err());
        assert!(ConfigClient::depuis_texte("relais = \"https://r\"\njeton = \"\"").is_err());
        assert!(
            ConfigClient::depuis_texte("relais = \"https://r\"\njeton = \"j\"\nx = 1").is_err()
        );
    }
}
