//! Configuration de l'agent, lue dans un fichier TOML.

use std::path::Path;

use anyhow::{Context, bail};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Adresse WebSocket du relais, par exemple `wss://ricochet.mawena.cloud/agent`.
    pub relais: String,
    /// Identifiant de la machine, déclaré dans la configuration du relais.
    pub id: String,
    /// Jeton secret de la machine.
    pub jeton: String,
}

impl Config {
    pub fn lire(chemin: &Path) -> anyhow::Result<Self> {
        let texte = std::fs::read_to_string(chemin)
            .with_context(|| format!("lecture de {}", chemin.display()))?;
        Self::depuis_texte(&texte).with_context(|| format!("configuration {}", chemin.display()))
    }

    pub fn depuis_texte(texte: &str) -> anyhow::Result<Self> {
        let config: Config = toml::from_str(texte)?;
        if !config.relais.starts_with("wss://") && !config.relais.starts_with("ws://") {
            bail!("relais doit commencer par wss:// (ou ws:// pour un essai local)");
        }
        if config.id.is_empty() || config.jeton.is_empty() {
            bail!("id et jeton sont obligatoires");
        }
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lecture() {
        let config = Config::depuis_texte(
            "relais = \"wss://exemple.org/agent\"\nid = \"pc\"\njeton = \"x\"\n",
        )
        .unwrap();
        assert_eq!(config.id, "pc");
        assert!(
            Config::depuis_texte("relais = \"https://exemple.org\"\nid = \"pc\"\njeton = \"x\"")
                .is_err()
        );
        assert!(
            Config::depuis_texte("relais = \"wss://exemple.org\"\nid = \"\"\njeton = \"x\"")
                .is_err()
        );
    }
}
