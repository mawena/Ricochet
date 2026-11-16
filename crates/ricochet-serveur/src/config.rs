//! Configuration du relais, lue dans un fichier TOML.

use std::{
    net::SocketAddr,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};
use serde::Deserialize;

/// Configuration complète du relais. Les machines et les comptes sont dans la base de données.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Adresse d'écoute, normalement locale : nginx s'occupe du HTTPS devant.
    #[serde(default = "ecoute_par_defaut")]
    pub ecoute: SocketAddr,
    /// Base de données SQLite (comptes, sessions, machines, journal d'audit).
    #[serde(default = "base_par_defaut")]
    pub base: PathBuf,
    /// Empreinte SHA-256 (hexadécimale) d'un jeton donnant accès à l'API sans session, pour
    /// les scripts. Absent : seules les sessions de l'interface ouvrent l'API.
    #[serde(default)]
    pub jeton_operateur_sha256: Option<String>,
    /// Durée de vie d'une session de l'interface, en heures.
    #[serde(default = "duree_session_par_defaut")]
    pub duree_session_h: u64,
    /// Cookie de session réservé au HTTPS. À désactiver seulement pour un essai local en HTTP.
    #[serde(default = "vrai")]
    pub cookie_securise: bool,
    /// Période d'envoi de l'état demandée aux agents, en secondes.
    #[serde(default = "intervalle_par_defaut")]
    pub intervalle_etat_s: u64,
}

fn ecoute_par_defaut() -> SocketAddr {
    "127.0.0.1:7870".parse().unwrap()
}

fn base_par_defaut() -> PathBuf {
    PathBuf::from("/var/lib/ricochet/ricochet.db")
}

fn duree_session_par_defaut() -> u64 {
    12
}

fn vrai() -> bool {
    true
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
        if let Some(empreinte) = &self.jeton_operateur_sha256 {
            verifier_empreinte("jeton_operateur_sha256", empreinte)?;
        }
        if self.intervalle_etat_s == 0 {
            bail!("intervalle_etat_s doit être supérieur à 0");
        }
        if self.duree_session_h == 0 {
            bail!("duree_session_h doit être supérieure à 0");
        }
        Ok(())
    }
}

/// Identifiant de machine valable : lettres, chiffres, `-` et `_`, 64 caractères au plus.
pub fn identifiant_valide(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
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
        let config = Config::depuis_texte("").unwrap();
        assert_eq!(config.ecoute.to_string(), "127.0.0.1:7870");
        assert_eq!(config.base, PathBuf::from("/var/lib/ricochet/ricochet.db"));
        assert_eq!(config.intervalle_etat_s, 30);
        assert_eq!(config.duree_session_h, 12);
        assert!(config.cookie_securise);
        assert!(config.jeton_operateur_sha256.is_none());
    }

    #[test]
    fn erreurs_detectees() {
        assert!(Config::depuis_texte(&format!("jeton_operateur_sha256 = \"{H}\"")).is_ok());
        assert!(Config::depuis_texte("jeton_operateur_sha256 = \"court\"").is_err());
        assert!(Config::depuis_texte("duree_session_h = 0").is_err());
        assert!(Config::depuis_texte("autre = 1").is_err());
        // Les machines ne se déclarent plus dans la configuration.
        assert!(Config::depuis_texte("[[agents]]\nid = \"pc\"").is_err());
    }

    #[test]
    fn identifiants() {
        assert!(identifiant_valide("pc-de_charles2"));
        assert!(!identifiant_valide(""));
        assert!(!identifiant_valide("a b"));
        assert!(!identifiant_valide("é"));
        assert!(!identifiant_valide(&"a".repeat(65)));
    }
}
