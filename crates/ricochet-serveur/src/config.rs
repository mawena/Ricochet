//! Configuration du relais, lue dans un fichier TOML.

use std::{net::SocketAddr, path::Path};

use anyhow::{Context, bail};
use serde::Deserialize;

/// Configuration complète du relais.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Adresse d'écoute, normalement locale : nginx s'occupe du HTTPS devant.
    #[serde(default = "ecoute_par_defaut")]
    pub ecoute: SocketAddr,
    /// Empreinte SHA-256 (hexadécimale) du jeton de l'opérateur.
    pub jeton_operateur_sha256: String,
    /// Période d'envoi de l'état demandée aux agents, en secondes.
    #[serde(default = "intervalle_par_defaut")]
    pub intervalle_etat_s: u64,
    /// Agents autorisés.
    #[serde(default)]
    pub agents: Vec<AgentAutorise>,
}

/// Un agent autorisé à se connecter.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentAutorise {
    pub id: String,
    /// Empreinte SHA-256 (hexadécimale) du jeton de l'agent.
    pub jeton_sha256: String,
    /// Nom lisible, affiché dans l'interface.
    #[serde(default)]
    pub nom: Option<String>,
}

fn ecoute_par_defaut() -> SocketAddr {
    "127.0.0.1:7870".parse().unwrap()
}

fn intervalle_par_defaut() -> u64 {
    30
}

impl Config {
    pub fn lire(chemin: &Path) -> anyhow::Result<Self> {
        let texte = std::fs::read_to_string(chemin)
            .with_context(|| format!("lecture de {}", chemin.display()))?;
        Self::depuis_texte(&texte).with_context(|| format!("configuration {}", chemin.display()))
    }

    pub fn depuis_texte(texte: &str) -> anyhow::Result<Self> {
        let config: Config = toml::from_str(texte)?;
        config.verifier()?;
        Ok(config)
    }

    fn verifier(&self) -> anyhow::Result<()> {
        verifier_empreinte("jeton_operateur_sha256", &self.jeton_operateur_sha256)?;
        if self.intervalle_etat_s == 0 {
            bail!("intervalle_etat_s doit être supérieur à 0");
        }
        let mut vus = std::collections::HashSet::new();
        for agent in &self.agents {
            if agent.id.is_empty()
                || !agent
                    .id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                bail!(
                    "identifiant d'agent invalide : « {} » (lettres, chiffres, - et _)",
                    agent.id
                );
            }
            if !vus.insert(agent.id.as_str()) {
                bail!("agent « {} » déclaré deux fois", agent.id);
            }
            verifier_empreinte(
                &format!("jeton_sha256 de « {} »", agent.id),
                &agent.jeton_sha256,
            )?;
        }
        Ok(())
    }

    pub fn agent(&self, id: &str) -> Option<&AgentAutorise> {
        self.agents.iter().find(|a| a.id == id)
    }
}

fn verifier_empreinte(champ: &str, valeur: &str) -> anyhow::Result<()> {
    if valeur.len() != 64 || !valeur.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("{champ} doit être une empreinte SHA-256 de 64 caractères hexadécimaux");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[test]
    fn config_minimale() {
        let config = Config::depuis_texte(&format!("jeton_operateur_sha256 = \"{H}\"")).unwrap();
        assert_eq!(config.ecoute.to_string(), "127.0.0.1:7870");
        assert_eq!(config.intervalle_etat_s, 30);
        assert!(config.agents.is_empty());
    }

    #[test]
    fn agents_valides() {
        let texte = format!(
            "jeton_operateur_sha256 = \"{H}\"\n[[agents]]\nid = \"pc\"\njeton_sha256 = \"{H}\"\nnom = \"PC\"\n"
        );
        let config = Config::depuis_texte(&texte).unwrap();
        assert_eq!(config.agent("pc").unwrap().nom.as_deref(), Some("PC"));
        assert!(config.agent("autre").is_none());
    }

    #[test]
    fn erreurs_detectees() {
        assert!(Config::depuis_texte("jeton_operateur_sha256 = \"court\"").is_err());
        let double = format!(
            "jeton_operateur_sha256 = \"{H}\"\n[[agents]]\nid = \"pc\"\njeton_sha256 = \"{H}\"\n[[agents]]\nid = \"pc\"\njeton_sha256 = \"{H}\"\n"
        );
        assert!(Config::depuis_texte(&double).is_err());
        let mauvais_id = format!(
            "jeton_operateur_sha256 = \"{H}\"\n[[agents]]\nid = \"a b\"\njeton_sha256 = \"{H}\"\n"
        );
        assert!(Config::depuis_texte(&mauvais_id).is_err());
        let inconnu = format!("jeton_operateur_sha256 = \"{H}\"\nautre = 1\n");
        assert!(Config::depuis_texte(&inconnu).is_err());
    }
}
