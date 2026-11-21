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
    /// Empreinte SHA-256 d'un jeton qui ne permet que de lister les machines et d'ouvrir des
    /// tunnels (pour un outil comme CCS) : ni terminal, ni fichiers, ni gestion des machines.
    #[serde(default)]
    pub jeton_tunnel_sha256: Option<String>,
    /// Durée de vie d'une session de l'interface, en heures.
    #[serde(default = "duree_session_par_defaut")]
    pub duree_session_h: u64,
    /// Cookie de session réservé au HTTPS. À désactiver seulement pour un essai local en HTTP.
    #[serde(default = "vrai")]
    pub cookie_securise: bool,
    /// Période d'envoi de l'état demandée aux agents, en secondes.
    #[serde(default = "intervalle_par_defaut")]
    pub intervalle_etat_s: u64,
    /// Mode Portier de l'interface web (désactivé par défaut).
    #[serde(default)]
    pub portier: Portier,
    /// Messages des machines (conservation, débit).
    #[serde(default)]
    pub messages: Messages,
    /// Notifications Web Push vers les appareils de l'opérateur.
    #[serde(default)]
    pub push: Push,
}

/// Section `[messages]` : les messages que les programmes des machines envoient à l'opérateur.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Messages {
    /// Les messages sont supprimés du relais après ce nombre de jours.
    #[serde(default = "conservation_par_defaut")]
    pub conservation_jours: u64,
    /// Messages acceptés par minute et par machine ; au-delà, ils sont gardés mais ne sont plus
    /// poussés en notification (une machine déréglée ne doit pas inonder les téléphones).
    #[serde(default = "debit_par_defaut")]
    pub limite_par_minute: usize,
    /// Attente maximale du détail d'un message auprès de l'agent, en secondes.
    #[serde(default = "delai_detail_par_defaut")]
    pub delai_detail_s: u64,
}

impl Default for Messages {
    fn default() -> Self {
        Self {
            conservation_jours: conservation_par_defaut(),
            limite_par_minute: debit_par_defaut(),
            delai_detail_s: delai_detail_par_defaut(),
        }
    }
}

fn conservation_par_defaut() -> u64 {
    30
}

fn debit_par_defaut() -> usize {
    20
}

fn delai_detail_par_defaut() -> u64 {
    15
}

/// Services de push des navigateurs : seuls leurs hôtes reçoivent des requêtes du relais.
pub const HOTES_PUSH: [&str; 5] = [
    "fcm.googleapis.com",
    "*.push.apple.com",
    "*.push.services.mozilla.com",
    "*.notify.windows.com",
    "*.push.microsoft.com",
];

/// Section `[push]` : Web Push (RFC 8030, 8291, 8292).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Push {
    /// Adresse publique du relais (`https://ricochet.mawena.cloud`) : elle va dans la
    /// revendication `sub` des jetons VAPID, à la place d'une adresse e-mail. Sans elle, le
    /// relais ne pousse rien.
    #[serde(default)]
    pub url_publique: Option<String>,
    /// Hôtes des services de push joignables (`*.exemple.org` pour tout un domaine). Un
    /// abonnement vers un autre hôte est refusé : il ne doit pas servir à atteindre une adresse
    /// interne.
    #[serde(default = "hotes_par_defaut")]
    pub hotes_autorises: Vec<String>,
    /// Réservé aux essais : accepte `http://` et les adresses locales pour les hôtes listés.
    #[serde(default)]
    pub autoriser_http: bool,
    /// Durée pendant laquelle le service de push garde une notification pour un appareil éteint.
    #[serde(default = "ttl_par_defaut")]
    pub ttl_s: u64,
}

impl Default for Push {
    fn default() -> Self {
        Self {
            url_publique: None,
            hotes_autorises: hotes_par_defaut(),
            autoriser_http: false,
            ttl_s: ttl_par_defaut(),
        }
    }
}

fn hotes_par_defaut() -> Vec<String> {
    HOTES_PUSH.iter().map(|h| (*h).to_owned()).collect()
}

fn ttl_par_defaut() -> u64 {
    86400
}

impl Push {
    /// Adresse publique sans `/` final.
    pub fn sujet(&self) -> Option<&str> {
        self.url_publique
            .as_deref()
            .map(|u| u.trim_end_matches('/'))
    }

    fn verifier(&self) -> anyhow::Result<()> {
        if let Some(url) = &self.url_publique {
            let https = url.starts_with("https://");
            if !(https || self.autoriser_http && url.starts_with("http://")) || !adresse_web(url) {
                bail!("[push] url_publique doit être une adresse https://… sans espace");
            }
        }
        if self.hotes_autorises.iter().any(|h| h.trim().is_empty()) {
            bail!("[push] hotes_autorises ne peut pas contenir d'hôte vide");
        }
        if self.ttl_s == 0 || self.ttl_s > 28 * 86400 {
            bail!("[push] ttl_s doit être entre 1 seconde et 28 jours");
        }
        Ok(())
    }
}

/// Section `[portier]` : l'interface web fait confiance à l'en-tête `X-Portier-Utilisateur`
/// posé par nginx après la vérification de Portier (connexion unique de mawena.cloud). Ne
/// concerne jamais les agents ni l'API par jeton.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Portier {
    /// Désactivé par défaut : l'en-tête est alors ignoré.
    #[serde(default)]
    pub actif: bool,
    /// Noms acceptés (comptes vérifiés par Portier).
    #[serde(default)]
    pub utilisateurs: Vec<String>,
    /// Adresse de Portier, par exemple `https://portier.mawena.cloud`.
    #[serde(default)]
    pub url: String,
    /// Où renvoyer après la déconnexion (par défaut `<url>/`).
    #[serde(default)]
    pub deconnexion: Option<String>,
}

impl Portier {
    /// Adresse de Portier sans `/` final.
    pub fn base(&self) -> &str {
        self.url.trim_end_matches('/')
    }

    /// Adresse vers laquelle renvoie la déconnexion.
    pub fn adresse_deconnexion(&self) -> String {
        match &self.deconnexion {
            Some(adresse) => adresse.clone(),
            None => format!("{}/", self.base()),
        }
    }

    fn verifier(&self) -> anyhow::Result<()> {
        if !self.actif {
            return Ok(());
        }
        if !adresse_web(&self.url) {
            bail!("[portier] url doit commencer par https:// ou http://");
        }
        if let Some(adresse) = &self.deconnexion
            && !adresse_web(adresse)
        {
            bail!("[portier] deconnexion doit commencer par https:// ou http://");
        }
        if self.utilisateurs.is_empty() || self.utilisateurs.iter().any(|u| u.trim().is_empty()) {
            bail!("[portier] utilisateurs doit donner au moins un nom, sans nom vide");
        }
        Ok(())
    }
}

fn adresse_web(adresse: &str) -> bool {
    let reste = adresse
        .strip_prefix("https://")
        .or_else(|| adresse.strip_prefix("http://"));
    reste.is_some_and(|r| !r.is_empty() && !r.chars().any(|c| c.is_whitespace() || c.is_control()))
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
        if let Some(empreinte) = &self.jeton_tunnel_sha256 {
            verifier_empreinte("jeton_tunnel_sha256", empreinte)?;
        }
        if self.intervalle_etat_s == 0 {
            bail!("intervalle_etat_s doit être supérieur à 0");
        }
        if self.duree_session_h == 0 {
            bail!("duree_session_h doit être supérieure à 0");
        }
        if self.messages.conservation_jours == 0 {
            bail!("[messages] conservation_jours doit être supérieur à 0");
        }
        if self.messages.limite_par_minute == 0 {
            bail!("[messages] limite_par_minute doit être supérieur à 0");
        }
        if self.messages.delai_detail_s == 0 {
            bail!("[messages] delai_detail_s doit être supérieur à 0");
        }
        self.push.verifier()?;
        self.portier.verifier()
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
        assert!(!config.portier.actif);
    }

    #[test]
    fn section_portier() {
        let texte = "[portier]\nactif = true\nutilisateurs = [\"charles-gamligo\"]\n\
                     url = \"https://portier.mawena.cloud/\"\n";
        let config = Config::depuis_texte(texte).unwrap();
        assert!(config.portier.actif);
        assert_eq!(config.portier.base(), "https://portier.mawena.cloud");
        assert_eq!(
            config.portier.adresse_deconnexion(),
            "https://portier.mawena.cloud/"
        );
        let autre = format!("{texte}deconnexion = \"https://portier.mawena.cloud/sortie\"\n");
        assert_eq!(
            Config::depuis_texte(&autre)
                .unwrap()
                .portier
                .adresse_deconnexion(),
            "https://portier.mawena.cloud/sortie"
        );
        // Activé, il faut une adresse et au moins un nom.
        assert!(Config::depuis_texte("[portier]\nactif = true\nurl = \"https://p\"").is_err());
        assert!(
            Config::depuis_texte("[portier]\nactif = true\nutilisateurs = [\"a\"]\nurl = \"p\"")
                .is_err()
        );
        // Désactivé, la section peut rester incomplète ; une clé inconnue est refusée.
        assert!(Config::depuis_texte("[portier]\nactif = false").is_ok());
        assert!(Config::depuis_texte("[portier]\nautre = 1").is_err());
    }

    #[test]
    fn messages_et_push() {
        let config = Config::depuis_texte("").unwrap();
        assert_eq!(config.messages.conservation_jours, 30);
        assert_eq!(config.messages.limite_par_minute, 20);
        assert_eq!(config.push.url_publique, None);
        assert_eq!(config.push.ttl_s, 86400);
        assert!(
            config
                .push
                .hotes_autorises
                .iter()
                .any(|h| h == "fcm.googleapis.com")
        );
        assert!(!config.push.autoriser_http);
        let config = Config::depuis_texte(
            "[messages]\nconservation_jours = 7\nlimite_par_minute = 5\n\
             [push]\nurl_publique = \"https://ricochet.mawena.cloud/\"\nhotes_autorises = [\"*.push.apple.com\"]\nttl_s = 3600",
        )
        .unwrap();
        assert_eq!(config.messages.conservation_jours, 7);
        assert_eq!(config.push.sujet(), Some("https://ricochet.mawena.cloud"));
        assert_eq!(config.push.hotes_autorises, ["*.push.apple.com"]);
        assert!(Config::depuis_texte("[push]\nurl_publique = \"http://x\"").is_err());
        assert!(
            Config::depuis_texte("[push]\nautoriser_http = true\nurl_publique = \"http://x\"")
                .is_ok()
        );
        assert!(Config::depuis_texte("[push]\nurl_publique = \"mailto:a@b.c\"").is_err());
        assert!(Config::depuis_texte("[push]\nttl_s = 0").is_err());
        assert!(Config::depuis_texte("[push]\nhotes_autorises = [\"\"]").is_err());
        assert!(Config::depuis_texte("[messages]\nconservation_jours = 0").is_err());
        assert!(Config::depuis_texte("[messages]\nlimite_par_minute = 0").is_err());
        assert!(Config::depuis_texte("[push]\nautre = 1").is_err());
    }

    #[test]
    fn erreurs_detectees() {
        assert!(Config::depuis_texte(&format!("jeton_operateur_sha256 = \"{H}\"")).is_ok());
        assert!(Config::depuis_texte("jeton_operateur_sha256 = \"court\"").is_err());
        assert!(Config::depuis_texte(&format!("jeton_tunnel_sha256 = \"{H}\"")).is_ok());
        assert!(Config::depuis_texte("jeton_tunnel_sha256 = \"court\"").is_err());
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
