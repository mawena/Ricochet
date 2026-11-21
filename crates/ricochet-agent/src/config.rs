//! Configuration de l'agent, lue dans un fichier TOML.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

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
    /// Tunnels vers des ports locaux : désactivés si la section `[tunnel]` est absente.
    #[serde(default)]
    pub tunnel: Option<ConfigTunnel>,
    /// Messages des programmes de la machine : désactivés si la section `[messages]` est absente.
    #[serde(default)]
    pub messages: Option<ConfigMessages>,
    /// Applications web locales joignables par le relais (`[applis.<nom>]`) : aucune si la
    /// section est absente.
    #[serde(default)]
    pub applis: BTreeMap<String, ConfigAppli>,
}

/// Section `[applis.<nom>]` : une application web qui écoute sur 127.0.0.1.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigAppli {
    /// Port local (127.0.0.1 seulement) ; aucun autre port n'est joignable.
    pub port: u16,
    /// Libellé affiché dans l'interface (par défaut le nom).
    #[serde(default)]
    pub libelle: Option<String>,
    /// En-têtes ajoutés à chaque requête, après ceux du navigateur et en les remplaçant : un
    /// jeton d'accès (`Cookie = "…"`) reste ainsi sur la machine.
    #[serde(default)]
    pub entetes: BTreeMap<String, String>,
}

/// Un nom d'application : `[a-z0-9-]{1,32}`.
pub fn nom_appli_valide(nom: &str) -> bool {
    (1..=32).contains(&nom.len())
        && nom
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

/// Section `[messages]` : un socket Unix par lequel les programmes locaux envoient des messages
/// à l'opérateur (notification sur ses appareils) et lisent ses réponses.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigMessages {
    /// Comptes du système autorisés à parler au socket (root l'est toujours).
    pub utilisateurs: Vec<String>,
    /// Chemin du socket (dossier 0755 créé par systemd : `RuntimeDirectory=ricochet-agent`).
    #[serde(default = "socket_par_defaut")]
    pub socket: PathBuf,
    /// Durée de garde des messages et de leurs détails sur la machine.
    #[serde(default = "conservation_par_defaut")]
    pub conservation_jours: u64,
    /// Dossier où l'agent garde la file d'attente (0700) : messages pas encore acceptés par le
    /// relais, détails, réponses non lues (`StateDirectory=ricochet-agent`).
    #[serde(default = "dossier_etat_par_defaut")]
    pub dossier_etat: PathBuf,
}

/// Chemin du socket des messages, par défaut.
pub const SOCKET_MESSAGES: &str = "/run/ricochet-agent/messages.sock";

fn socket_par_defaut() -> PathBuf {
    PathBuf::from(SOCKET_MESSAGES)
}

fn conservation_par_defaut() -> u64 {
    30
}

fn dossier_etat_par_defaut() -> PathBuf {
    PathBuf::from("/var/lib/ricochet-agent")
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigTunnel {
    /// Ports de cette machine (127.0.0.1) qu'un tunnel peut atteindre, par exemple `[22]`
    /// pour SSH. Aucun autre port n'est joignable.
    pub ports: Vec<u16>,
    /// Nombre de tunnels ouverts en même temps.
    #[serde(default = "tunnels_par_defaut")]
    pub maximum: usize,
}

fn tunnels_par_defaut() -> usize {
    16
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
        if config.tunnel.as_ref().is_some_and(|t| t.ports.is_empty()) {
            bail!("tunnel.ports ne peut pas être vide");
        }
        if let Some(messages) = &config.messages {
            if messages.utilisateurs.iter().any(|u| u.trim().is_empty()) {
                bail!("messages.utilisateurs ne peut pas contenir de nom vide");
            }
            if messages.conservation_jours == 0 {
                bail!("messages.conservation_jours doit être supérieur à 0");
            }
        }
        for (nom, appli) in &config.applis {
            if !nom_appli_valide(nom) {
                bail!("applis.{nom} : nom invalide (minuscules, chiffres et tirets, 32 au plus)");
            }
            if appli.port == 0 {
                bail!("applis.{nom}.port doit être supérieur à 0");
            }
            for (entete, valeur) in &appli.entetes {
                if http::HeaderName::from_bytes(entete.as_bytes()).is_err()
                    || http::HeaderValue::from_str(valeur).is_err()
                {
                    bail!("applis.{nom}.entetes : en-tête « {entete} » invalide");
                }
            }
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
        assert!(config.tunnel.is_none());
        let base = "relais = \"wss://e.org/agent\"\nid = \"pc\"\njeton = \"x\"\n";
        let tunnel = Config::depuis_texte(&format!("{base}[tunnel]\nports = [22]"))
            .unwrap()
            .tunnel
            .unwrap();
        assert_eq!((tunnel.ports, tunnel.maximum), (vec![22], 16));
        assert!(config.messages.is_none());
        let messages =
            Config::depuis_texte(&format!("{base}[messages]\nutilisateurs = [\"charles\"]"))
                .unwrap()
                .messages
                .unwrap();
        assert_eq!(
            messages.socket,
            PathBuf::from("/run/ricochet-agent/messages.sock")
        );
        assert_eq!(messages.conservation_jours, 30);
        assert_eq!(
            messages.dossier_etat,
            PathBuf::from("/var/lib/ricochet-agent")
        );
        assert!(Config::depuis_texte(&format!("{base}[messages]")).is_err());
        assert!(Config::depuis_texte(&format!("{base}[messages]\nutilisateurs = [\"\"]")).is_err());
        assert!(config.applis.is_empty());
        let applis = Config::depuis_texte(&format!(
            "{base}[applis.cofiticket]\nport = 8790\nlibelle = \"CofiTicket\"\n[applis.cofiticket.entetes]\nCookie = \"cofiticket=x\""
        ))
        .unwrap()
        .applis;
        assert_eq!(applis["cofiticket"].port, 8790);
        assert_eq!(applis["cofiticket"].entetes["Cookie"], "cofiticket=x");
        assert!(Config::depuis_texte(&format!("{base}[applis.Mal]\nport = 1")).is_err());
        assert!(Config::depuis_texte(&format!("{base}[applis.a]\nport = 0")).is_err());
        assert!(Config::depuis_texte(&format!("{base}[applis.a]")).is_err());
        assert!(
            Config::depuis_texte(&format!(
                "{base}[applis.a]\nport = 1\n[applis.a.entetes]\n\"a b\" = \"x\""
            ))
            .is_err()
        );
        assert!(Config::depuis_texte(&format!("{base}[tunnel]\nports = []")).is_err());
        assert!(Config::depuis_texte(&format!("{base}[tunnel]")).is_err());
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
