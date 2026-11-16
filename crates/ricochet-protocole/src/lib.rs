//! Protocole de Ricochet : les messages échangés entre un agent et le relais.
//!
//! L'agent ouvre une connexion WebSocket sortante vers le relais, puis chaque message est un
//! objet JSON envoyé dans une trame texte. Le champ `type` indique la nature du message.

use serde::{Deserialize, Serialize};

/// Version du protocole. Le relais refuse un agent dont la version diffère.
pub const VERSION_PROTOCOLE: u32 = 1;

/// Ce que l'agent dit de sa machine en se présentant ; cela ne change pas pendant la session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InfoMachine {
    pub nom_hote: String,
    pub systeme: String,
    pub noyau: String,
    pub architecture: String,
    pub version_agent: String,
    /// L'agent accepte d'ouvrir des terminaux (option `[terminal]` de sa configuration).
    #[serde(default)]
    pub terminal: bool,
    /// L'agent donne accès à ses fichiers (option `[fichiers]` de sa configuration).
    #[serde(default)]
    pub fichiers: bool,
}

/// Numéro d'un terminal, attribué par le relais.
pub type NumeroTerminal = u32;

/// Octets transportés en base64 dans le JSON.
mod base64 {
    use data_encoding::BASE64;
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(octets: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&BASE64.encode(octets))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let texte = <std::borrow::Cow<'de, str>>::deserialize(d)?;
        BASE64.decode(texte.as_bytes()).map_err(D::Error::custom)
    }
}

/// Numéro d'une requête de fichiers, attribué par le relais et repris dans la réponse.
pub type NumeroRequete = u64;

/// Taille maximale d'un morceau de fichier transporté dans un message.
pub const MORCEAU_MAX: u32 = 512 * 1024;

/// Ordre sur les fichiers de la machine. Les chemins sont absolus ; `~` désigne le dossier
/// personnel de l'utilisateur des fichiers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "ordre", rename_all = "snake_case")]
pub enum OrdreFichiers {
    /// Contenu d'un dossier.
    Lister { chemin: String },
    /// Lire au plus `taille` octets à partir de `decalage`.
    Lire {
        chemin: String,
        decalage: u64,
        taille: u32,
    },
    /// Écrire un morceau dans le fichier partiel `<chemin>.ricochet-partiel`, qui n'est écrit
    /// qu'à la suite de ce qu'il contient déjà (`decalage` 0 le recommence). Avec `fin`, il
    /// prend la place de `chemin`.
    Ecrire {
        chemin: String,
        decalage: u64,
        #[serde(with = "base64")]
        donnees: Vec<u8>,
        fin: bool,
    },
    /// Taille du fichier partiel de `chemin` (0 s'il n'existe pas), pour reprendre un envoi.
    Partiel { chemin: String },
}

/// Réponse de l'agent à un ordre sur les fichiers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reponse", rename_all = "snake_case")]
pub enum ReponseFichiers {
    /// Contenu du dossier `chemin` (chemin complet, liens résolus).
    Dossier {
        chemin: String,
        entrees: Vec<Entree>,
        /// La liste a été coupée : le dossier contient trop d'entrées.
        tronque: bool,
    },
    /// Morceau lu ; vide à la fin du fichier.
    Morceau {
        #[serde(with = "base64")]
        donnees: Vec<u8>,
        taille_totale: u64,
    },
    /// Taille du fichier partiel après l'écriture (ou du fichier final, après `fin`).
    Ecrit {
        taille: u64,
    },
    Erreur {
        message: String,
    },
}

/// Une entrée d'un dossier.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entree {
    pub nom: String,
    pub genre: Genre,
    /// L'entrée est un lien symbolique (`genre` décrit sa cible).
    #[serde(default)]
    pub lien: bool,
    pub taille: u64,
    /// Dernière modification, en secondes depuis l'époque Unix.
    pub modifie_le: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Genre {
    Dossier,
    Fichier,
    Autre,
}

/// Un instantané de l'état de la machine, envoyé régulièrement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EtatMachine {
    /// Utilisation moyenne du processeur, de 0 à 100.
    pub cpu_pourcent: f32,
    pub nombre_coeurs: u32,
    /// Charge moyenne sur 1, 5 et 15 minutes.
    pub charge: [f64; 3],
    /// Mémoire vive, en octets.
    pub memoire_utilisee: u64,
    pub memoire_totale: u64,
    pub disques: Vec<EtatDisque>,
    /// Temps écoulé depuis le démarrage de la machine, en secondes.
    pub allumee_depuis_s: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EtatDisque {
    pub point_montage: String,
    /// Espace occupé et taille totale, en octets.
    pub utilise: u64,
    pub total: u64,
}

/// Messages envoyés par l'agent au relais.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentVersRelais {
    /// Premier message de la session : l'agent se présente et prouve son identité.
    Bonjour {
        version_protocole: u32,
        id: String,
        jeton: String,
        info: InfoMachine,
    },
    /// État courant de la machine.
    Etat { etat: EtatMachine },
    /// Ce que le terminal a affiché.
    Sortie {
        terminal: NumeroTerminal,
        #[serde(with = "base64")]
        donnees: Vec<u8>,
    },
    /// Le terminal s'est fermé (fin du shell) ou n'a pas pu s'ouvrir.
    TerminalFerme {
        terminal: NumeroTerminal,
        raison: String,
    },
    /// Réponse à un ordre sur les fichiers.
    Fichiers {
        requete: NumeroRequete,
        reponse: ReponseFichiers,
    },
}

/// Messages envoyés par le relais à l'agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RelaisVersAgent {
    /// L'agent est accepté ; il enverra son état toutes les `intervalle_etat_s` secondes.
    Bienvenue { intervalle_etat_s: u64 },
    /// L'agent est refusé ; le relais ferme ensuite la connexion.
    Refus { raison: Refus },
    /// Le relais veut un état à jour sans attendre le prochain envoi.
    DemandeEtat,
    /// Ouvrir un terminal de cette taille.
    OuvrirTerminal {
        terminal: NumeroTerminal,
        colonnes: u16,
        lignes: u16,
    },
    /// Ce que l'opérateur tape dans le terminal.
    Entree {
        terminal: NumeroTerminal,
        #[serde(with = "base64")]
        donnees: Vec<u8>,
    },
    /// La fenêtre du terminal a changé de taille.
    Redimensionner {
        terminal: NumeroTerminal,
        colonnes: u16,
        lignes: u16,
    },
    /// L'opérateur ferme le terminal : arrêter le shell.
    FermerTerminal { terminal: NumeroTerminal },
    /// Ordre sur les fichiers ; l'agent répond par `AgentVersRelais::Fichiers`.
    Fichiers {
        requete: NumeroRequete,
        ordre: OrdreFichiers,
    },
}

/// Raison d'un refus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Refus {
    /// Identifiant inconnu ou jeton faux.
    Identification,
    /// Versions du protocole différentes : il faut mettre à jour l'agent ou le relais.
    Version,
    /// Le premier message n'était pas un `Bonjour`.
    Protocole,
}

impl std::fmt::Display for Refus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Refus::Identification => "identifiant ou jeton refusé",
            Refus::Version => "version du protocole différente",
            Refus::Protocole => "message de présentation attendu",
        })
    }
}

/// Encode un message en JSON.
pub fn encoder<T: Serialize>(message: &T) -> String {
    serde_json::to_string(message).expect("les messages du protocole sont toujours sérialisables")
}

/// Décode un message JSON.
pub fn decoder<'a, T: Deserialize<'a>>(texte: &'a str) -> Result<T, serde_json::Error> {
    serde_json::from_str(texte)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> InfoMachine {
        InfoMachine {
            nom_hote: "machine".into(),
            systeme: "Ubuntu 24.04".into(),
            noyau: "6.8.0".into(),
            architecture: "x86_64".into(),
            version_agent: "0.1.0".into(),
            terminal: true,
            fichiers: true,
        }
    }

    #[test]
    fn bonjour_aller_retour() {
        let message = AgentVersRelais::Bonjour {
            version_protocole: VERSION_PROTOCOLE,
            id: "pc".into(),
            jeton: "secret".into(),
            info: info(),
        };
        let texte = encoder(&message);
        assert!(texte.starts_with(r#"{"type":"bonjour""#), "{texte}");
        assert_eq!(decoder::<AgentVersRelais>(&texte).unwrap(), message);
    }

    #[test]
    fn etat_aller_retour() {
        let message = AgentVersRelais::Etat {
            etat: EtatMachine {
                cpu_pourcent: 12.5,
                nombre_coeurs: 6,
                charge: [0.5, 0.4, 0.3],
                memoire_utilisee: 1 << 30,
                memoire_totale: 11 << 30,
                disques: vec![EtatDisque {
                    point_montage: "/".into(),
                    utilise: 100,
                    total: 200,
                }],
                allumee_depuis_s: 3600,
            },
        };
        assert_eq!(
            decoder::<AgentVersRelais>(&encoder(&message)).unwrap(),
            message
        );
    }

    #[test]
    fn messages_du_relais() {
        assert_eq!(
            encoder(&RelaisVersAgent::DemandeEtat),
            r#"{"type":"demande_etat"}"#
        );
        assert_eq!(
            encoder(&RelaisVersAgent::Refus {
                raison: Refus::Identification
            }),
            r#"{"type":"refus","raison":"identification"}"#
        );
        let bienvenue = RelaisVersAgent::Bienvenue {
            intervalle_etat_s: 30,
        };
        assert_eq!(
            decoder::<RelaisVersAgent>(&encoder(&bienvenue)).unwrap(),
            bienvenue
        );
    }

    #[test]
    fn terminal_en_base64() {
        let entree = RelaisVersAgent::Entree {
            terminal: 3,
            donnees: b"ls\r\xff".to_vec(),
        };
        let texte = encoder(&entree);
        assert_eq!(
            texte,
            r#"{"type":"entree","terminal":3,"donnees":"bHMN/w=="}"#
        );
        assert_eq!(decoder::<RelaisVersAgent>(&texte).unwrap(), entree);
        assert!(
            decoder::<AgentVersRelais>(r#"{"type":"sortie","terminal":1,"donnees":"%%"}"#).is_err()
        );
    }

    #[test]
    fn info_sans_terminal() {
        // Un agent qui ne parle pas du terminal ne l'accepte pas.
        let info: InfoMachine = serde_json::from_str(
            r#"{"nom_hote":"h","systeme":"s","noyau":"n","architecture":"a","version_agent":"v"}"#,
        )
        .unwrap();
        assert!(!info.terminal);
        assert!(!info.fichiers);
    }

    #[test]
    fn fichiers_aller_retour() {
        let ordre = RelaisVersAgent::Fichiers {
            requete: 7,
            ordre: OrdreFichiers::Ecrire {
                chemin: "/tmp/a".into(),
                decalage: 0,
                donnees: b"abc".to_vec(),
                fin: true,
            },
        };
        let texte = encoder(&ordre);
        assert_eq!(
            texte,
            r#"{"type":"fichiers","requete":7,"ordre":{"ordre":"ecrire","chemin":"/tmp/a","decalage":0,"donnees":"YWJj","fin":true}}"#
        );
        assert_eq!(decoder::<RelaisVersAgent>(&texte).unwrap(), ordre);
        let reponse = AgentVersRelais::Fichiers {
            requete: 7,
            reponse: ReponseFichiers::Dossier {
                chemin: "/".into(),
                entrees: vec![Entree {
                    nom: "etc".into(),
                    genre: Genre::Dossier,
                    lien: false,
                    taille: 4096,
                    modifie_le: Some(1),
                }],
                tronque: false,
            },
        };
        assert_eq!(
            decoder::<AgentVersRelais>(&encoder(&reponse)).unwrap(),
            reponse
        );
    }

    #[test]
    fn message_inconnu_refuse() {
        assert!(decoder::<AgentVersRelais>(r#"{"type":"autre"}"#).is_err());
    }
}
