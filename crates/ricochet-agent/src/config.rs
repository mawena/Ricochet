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
    /// Terminal à distance : désactivé si la section `[terminal]` est absente.
    #[serde(default)]
    pub terminal: Option<ConfigTerminal>,
    /// Accès aux fichiers : désactivé si la section `[fichiers]` est absente.
    #[serde(default)]
    pub fichiers: Option<ConfigFichiers>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigFichiers {
    /// Utilisateur dont les droits s'appliquent aux fichiers (lecture, écriture, propriétaire
    /// des fichiers reçus). Absent : root, comme l'agent.
    pub utilisateur: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigTerminal {
    /// Commande lancée dans chaque terminal. L'agent tourne en root : pour un shell d'un autre
    /// utilisateur, `["runuser", "-l", "charles"]`.
    #[serde(default = "shell_par_defaut")]
    pub commande: Vec<String>,
    /// Nombre de terminaux ouverts en même temps.
    #[serde(default = "maximum_par_defaut")]
    pub maximum: usize,
}

fn shell_par_defaut() -> Vec<String> {
    vec!["/bin/bash".into(), "-l".into()]
}

fn maximum_par_defaut() -> usize {
    8
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
        if config
            .terminal
            .as_ref()
            .is_some_and(|t| t.commande.is_empty())
        {
            bail!("terminal.commande ne peut pas être vide");
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
        assert!(config.terminal.is_none());
        assert!(config.fichiers.is_none());
        let avec = Config::depuis_texte(
            "relais = \"wss://e.org/agent\"\nid = \"pc\"\njeton = \"x\"\n[terminal]\n[fichiers]\nutilisateur = \"charles\"\n",
        )
        .unwrap();
        assert_eq!(
            avec.fichiers.unwrap().utilisateur.as_deref(),
            Some("charles")
        );
        let terminal = avec.terminal.unwrap();
        assert_eq!(terminal.commande, ["/bin/bash", "-l"]);
        assert_eq!(terminal.maximum, 8);
        assert!(
            Config::depuis_texte(
                "relais = \"wss://e.org\"\nid = \"pc\"\njeton = \"x\"\n[terminal]\ncommande = []"
            )
            .is_err()
        );
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
