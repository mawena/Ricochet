//! Base de données du relais (SQLite) : comptes opérateur, sessions, machines, journal d'audit.
//!
//! Les requêtes sont courtes ; une seule connexion protégée par un verrou suffit. La commande
//! `ricochet-serveur compte …` ouvre la même base pendant que le relais tourne (mode WAL).

use std::{path::Path, sync::Mutex};

use anyhow::Context;
use ricochet_protocole::{BoutonMessage, GenreMessage, MessageMachine};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS comptes (
    nom TEXT PRIMARY KEY,
    mot_de_passe TEXT NOT NULL,
    totp_secret TEXT NOT NULL,
    totp_dernier_pas INTEGER NOT NULL DEFAULT 0,
    cree_le INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS sessions (
    empreinte TEXT PRIMARY KEY,
    compte TEXT NOT NULL REFERENCES comptes(nom) ON DELETE CASCADE,
    creee_le INTEGER NOT NULL,
    expire_le INTEGER NOT NULL,
    adresse TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS machines (
    id TEXT PRIMARY KEY,
    nom TEXT,
    jeton_sha256 TEXT NOT NULL,
    creee_le INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS reglages (
    cle TEXT PRIMARY KEY,
    valeur TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS messages (
    id TEXT PRIMARY KEY,
    machine TEXT NOT NULL,
    source TEXT NOT NULL,
    titre TEXT NOT NULL,
    texte TEXT NOT NULL,
    genre TEXT NOT NULL,
    boutons TEXT NOT NULL,
    detail INTEGER NOT NULL,
    cree_le INTEGER NOT NULL,
    recu_le INTEGER NOT NULL,
    lu_le INTEGER,
    reponse_bouton TEXT,
    reponse_par TEXT,
    reponse_le INTEGER,
    reponse_livree INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS messages_recu ON messages(recu_le);
CREATE TABLE IF NOT EXISTS abonnements_push (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    compte TEXT NOT NULL,
    endpoint TEXT NOT NULL UNIQUE,
    p256dh TEXT NOT NULL,
    auth TEXT NOT NULL,
    appareil TEXT NOT NULL,
    cree_le INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS audit (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    instant INTEGER NOT NULL,
    compte TEXT,
    adresse TEXT NOT NULL,
    action TEXT NOT NULL,
    detail TEXT NOT NULL
);
";

pub struct Base {
    connexion: Mutex<Connection>,
}

/// Un compte opérateur, avec ses secrets : ne jamais l'envoyer au navigateur.
#[derive(Debug, Clone)]
pub struct Compte {
    pub nom: String,
    pub mot_de_passe: String,
    pub totp_secret: String,
    pub totp_dernier_pas: u64,
}

/// Une machine déclarée (autorisée à se connecter).
#[derive(Debug, Clone, Serialize)]
pub struct MachineDeclaree {
    pub id: String,
    pub nom: Option<String>,
    pub creee_le: u64,
}

/// Une ligne du journal d'audit.
#[derive(Debug, Clone, Serialize)]
pub struct LigneAudit {
    pub instant: u64,
    pub compte: Option<String>,
    pub adresse: String,
    pub action: String,
    pub detail: String,
}

/// Réponse de l'opérateur à un bouton.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReponseBouton {
    pub bouton: String,
    pub par: String,
    pub quand: u64,
    /// L'agent a accusé réception : le relais n'a plus à la renvoyer.
    pub livree: bool,
}

/// Un message de machine, sans son détail (qui reste sur la machine).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LigneMessage {
    pub id: String,
    pub machine: String,
    pub source: String,
    pub titre: String,
    pub texte: String,
    pub genre: GenreMessage,
    pub boutons: Vec<BoutonMessage>,
    /// Un détail est gardé sur la machine.
    pub detail: bool,
    pub cree_le: u64,
    pub recu_le: u64,
    pub lu_le: Option<u64>,
    pub reponse: Option<ReponseBouton>,
}

/// Résultat de l'enregistrement d'un message reçu d'un agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Enregistre {
    Nouveau,
    /// Déjà connu (l'agent le renvoie tant qu'il n'a pas l'accusé).
    Deja,
    /// Cet identifiant appartient à une autre machine : refusé.
    AutreMachine,
}

/// Filtre de la liste des messages.
#[derive(Debug, Default, Clone)]
pub struct FiltreMessages {
    pub machine: Option<String>,
    pub source: Option<String>,
    pub non_lus: bool,
}

/// Un appareil abonné aux notifications (une `PushSubscription` du navigateur).
#[derive(Debug, Clone, Serialize)]
pub struct Abonnement {
    pub id: i64,
    pub compte: String,
    #[serde(skip)]
    pub endpoint: String,
    #[serde(skip)]
    pub p256dh: String,
    #[serde(skip)]
    pub auth: String,
    pub appareil: String,
    pub cree_le: u64,
}

const COLONNES_MESSAGE: &str = "id, machine, source, titre, texte, genre, boutons, detail, \
    cree_le, recu_le, lu_le, reponse_bouton, reponse_par, reponse_le, reponse_livree";

fn ligne_message(l: &rusqlite::Row<'_>) -> rusqlite::Result<LigneMessage> {
    let invalide = |e: serde_json::Error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    };
    let genre: String = l.get(5)?;
    let boutons: String = l.get(6)?;
    let reponse = match l.get::<_, Option<String>>(11)? {
        Some(bouton) => Some(ReponseBouton {
            bouton,
            par: l.get(12)?,
            quand: l.get(13)?,
            livree: l.get::<_, i64>(14)? != 0,
        }),
        None => None,
    };
    Ok(LigneMessage {
        id: l.get(0)?,
        machine: l.get(1)?,
        source: l.get(2)?,
        titre: l.get(3)?,
        texte: l.get(4)?,
        genre: serde_json::from_str(&format!("\"{genre}\"")).map_err(invalide)?,
        boutons: serde_json::from_str(&boutons).map_err(invalide)?,
        detail: l.get::<_, i64>(7)? != 0,
        cree_le: l.get(8)?,
        recu_le: l.get(9)?,
        lu_le: l.get(10)?,
        reponse,
    })
}

impl Base {
    /// Ouvre (ou crée) la base. `:memory:` donne une base en mémoire, pour les essais.
    pub fn ouvrir(chemin: &Path) -> anyhow::Result<Self> {
        let connexion = Connection::open(chemin)
            .with_context(|| format!("ouverture de la base {}", chemin.display()))?;
        connexion.busy_timeout(std::time::Duration::from_secs(5))?;
        connexion.pragma_update(None, "journal_mode", "WAL")?;
        connexion.pragma_update(None, "foreign_keys", "ON")?;
        connexion.execute_batch(SCHEMA)?;
        if chemin != Path::new(":memory:") {
            use std::os::unix::fs::PermissionsExt;
            // La base contient les secrets TOTP : lisible par le seul propriétaire.
            std::fs::set_permissions(chemin, std::fs::Permissions::from_mode(0o600))
                .with_context(|| format!("droits de {}", chemin.display()))?;
        }
        Ok(Self {
            connexion: Mutex::new(connexion),
        })
    }

    pub fn en_memoire() -> Self {
        Self::ouvrir(Path::new(":memory:")).expect("base en mémoire")
    }

    fn avec<T>(
        &self,
        action: impl FnOnce(&Connection) -> rusqlite::Result<T>,
    ) -> anyhow::Result<T> {
        let connexion = self.connexion.lock().unwrap_or_else(|e| e.into_inner());
        Ok(action(&connexion)?)
    }

    // --- Comptes ---

    /// Crée un compte ; `false` s'il existe déjà.
    pub fn creer_compte(
        &self,
        nom: &str,
        mot_de_passe: &str,
        totp_secret: &str,
        instant: u64,
    ) -> anyhow::Result<bool> {
        self.avec(|c| {
            c.execute(
                "INSERT OR IGNORE INTO comptes (nom, mot_de_passe, totp_secret, cree_le)
                 VALUES (?1, ?2, ?3, ?4)",
                params![nom, mot_de_passe, totp_secret, instant],
            )
            .map(|n| n == 1)
        })
    }

    pub fn compte(&self, nom: &str) -> anyhow::Result<Option<Compte>> {
        self.avec(|c| {
            c.query_row(
                "SELECT nom, mot_de_passe, totp_secret, totp_dernier_pas FROM comptes WHERE nom = ?1",
                [nom],
                |l| {
                    Ok(Compte {
                        nom: l.get(0)?,
                        mot_de_passe: l.get(1)?,
                        totp_secret: l.get(2)?,
                        totp_dernier_pas: l.get(3)?,
                    })
                },
            )
            .optional()
        })
    }

    /// Mémorise le pas TOTP utilisé ; `false` s'il l'a déjà été (deux connexions simultanées
    /// avec le même code : une seule passe).
    pub fn noter_pas_totp(&self, nom: &str, pas: u64) -> anyhow::Result<bool> {
        self.avec(|c| {
            c.execute(
                "UPDATE comptes SET totp_dernier_pas = ?2 WHERE nom = ?1 AND totp_dernier_pas < ?2",
                params![nom, pas],
            )
            .map(|n| n == 1)
        })
    }

    pub fn comptes(&self) -> anyhow::Result<Vec<(String, u64)>> {
        self.avec(|c| {
            c.prepare("SELECT nom, cree_le FROM comptes ORDER BY nom")?
                .query_map([], |l| Ok((l.get(0)?, l.get(1)?)))?
                .collect()
        })
    }

    /// Supprime un compte et ses sessions ; `false` s'il n'existait pas.
    pub fn supprimer_compte(&self, nom: &str) -> anyhow::Result<bool> {
        self.avec(|c| {
            c.execute("DELETE FROM comptes WHERE nom = ?1", [nom])
                .map(|n| n == 1)
        })
    }

    // --- Sessions ---

    pub fn creer_session(
        &self,
        empreinte: &str,
        compte: &str,
        instant: u64,
        expire_le: u64,
        adresse: &str,
    ) -> anyhow::Result<()> {
        self.avec(|c| {
            c.execute(
                "INSERT INTO sessions (empreinte, compte, creee_le, expire_le, adresse)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![empreinte, compte, instant, expire_le, adresse],
            )
            .map(drop)
        })
    }

    /// Compte de la session si elle est encore valable.
    pub fn session(&self, empreinte: &str, instant: u64) -> anyhow::Result<Option<String>> {
        self.avec(|c| {
            c.query_row(
                "SELECT compte FROM sessions WHERE empreinte = ?1 AND expire_le > ?2",
                params![empreinte, instant],
                |l| l.get(0),
            )
            .optional()
        })
    }

    pub fn supprimer_session(&self, empreinte: &str) -> anyhow::Result<()> {
        self.avec(|c| {
            c.execute("DELETE FROM sessions WHERE empreinte = ?1", [empreinte])
                .map(drop)
        })
    }

    pub fn purger_sessions(&self, instant: u64) -> anyhow::Result<usize> {
        self.avec(|c| c.execute("DELETE FROM sessions WHERE expire_le <= ?1", [instant]))
    }

    // --- Machines ---

    /// Déclare une machine ; `false` si l'identifiant est déjà pris.
    pub fn ajouter_machine(
        &self,
        id: &str,
        nom: Option<&str>,
        jeton_sha256: &str,
        instant: u64,
    ) -> anyhow::Result<bool> {
        self.avec(|c| {
            c.execute(
                "INSERT OR IGNORE INTO machines (id, nom, jeton_sha256, creee_le)
                 VALUES (?1, ?2, ?3, ?4)",
                params![id, nom, jeton_sha256, instant],
            )
            .map(|n| n == 1)
        })
    }

    pub fn machines(&self) -> anyhow::Result<Vec<MachineDeclaree>> {
        self.avec(|c| {
            c.prepare("SELECT id, nom, creee_le FROM machines ORDER BY creee_le, id")?
                .query_map([], |l| {
                    Ok(MachineDeclaree {
                        id: l.get(0)?,
                        nom: l.get(1)?,
                        creee_le: l.get(2)?,
                    })
                })?
                .collect()
        })
    }

    pub fn empreinte_machine(&self, id: &str) -> anyhow::Result<Option<String>> {
        self.avec(|c| {
            c.query_row(
                "SELECT jeton_sha256 FROM machines WHERE id = ?1",
                [id],
                |l| l.get(0),
            )
            .optional()
        })
    }

    /// Révoque une machine ; `false` si elle n'existait pas.
    pub fn supprimer_machine(&self, id: &str) -> anyhow::Result<bool> {
        self.avec(|c| {
            c.execute("DELETE FROM machines WHERE id = ?1", [id])
                .map(|n| n == 1)
        })
    }

    // --- Réglages internes (clés VAPID…) ---

    pub fn reglage(&self, cle: &str) -> anyhow::Result<Option<String>> {
        self.avec(|c| {
            c.query_row("SELECT valeur FROM reglages WHERE cle = ?1", [cle], |l| {
                l.get(0)
            })
            .optional()
        })
    }

    /// Définit un réglage seulement s'il n'existe pas encore ; `false` s'il existait.
    pub fn definir_reglage_si_absent(&self, cle: &str, valeur: &str) -> anyhow::Result<bool> {
        self.avec(|c| {
            c.execute(
                "INSERT OR IGNORE INTO reglages (cle, valeur) VALUES (?1, ?2)",
                params![cle, valeur],
            )
            .map(|n| n == 1)
        })
    }

    // --- Messages des machines ---

    /// Enregistre un message venu d'un agent (sans son détail).
    pub fn enregistrer_message(
        &self,
        machine: &str,
        message: &MessageMachine,
        recu_le: u64,
    ) -> anyhow::Result<Enregistre> {
        let boutons = serde_json::to_string(&message.boutons)?;
        let genre = message.genre.texte();
        self.avec(|c| {
            let ajoutes = c.execute(
                "INSERT OR IGNORE INTO messages (id, machine, source, titre, texte, genre, boutons,
                     detail, cree_le, recu_le) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    message.id,
                    machine,
                    message.source,
                    message.titre,
                    message.texte,
                    genre,
                    boutons,
                    message.detail,
                    message.date,
                    recu_le
                ],
            )?;
            if ajoutes == 1 {
                return Ok(Enregistre::Nouveau);
            }
            let proprietaire: String = c.query_row(
                "SELECT machine FROM messages WHERE id = ?1",
                [&message.id],
                |l| l.get(0),
            )?;
            Ok(if proprietaire == machine {
                Enregistre::Deja
            } else {
                Enregistre::AutreMachine
            })
        })
    }

    pub fn message(&self, id: &str) -> anyhow::Result<Option<LigneMessage>> {
        self.avec(|c| {
            c.query_row(
                &format!("SELECT {COLONNES_MESSAGE} FROM messages WHERE id = ?1"),
                [id],
                ligne_message,
            )
            .optional()
        })
    }

    /// Les messages les plus récents d'abord, et le nombre total de messages non lus.
    pub fn messages(
        &self,
        filtre: &FiltreMessages,
        limite: u32,
    ) -> anyhow::Result<(Vec<LigneMessage>, u64)> {
        self.avec(|c| {
            let mut liste = c
                .prepare(&format!(
                    "SELECT {COLONNES_MESSAGE} FROM messages
                     WHERE (?1 IS NULL OR machine = ?1) AND (?2 IS NULL OR source = ?2)
                       AND (?3 = 0 OR lu_le IS NULL)
                     ORDER BY recu_le DESC, rowid DESC LIMIT ?4"
                ))?
                .query_map(
                    params![filtre.machine, filtre.source, filtre.non_lus, limite],
                    ligne_message,
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            liste.shrink_to_fit();
            let non_lus = c.query_row(
                "SELECT COUNT(*) FROM messages WHERE lu_le IS NULL",
                [],
                |l| l.get(0),
            )?;
            Ok((liste, non_lus))
        })
    }

    pub fn marquer_lu(&self, id: &str, instant: u64) -> anyhow::Result<()> {
        self.avec(|c| {
            c.execute(
                "UPDATE messages SET lu_le = ?2 WHERE id = ?1 AND lu_le IS NULL",
                params![id, instant],
            )
            .map(drop)
        })
    }

    /// Note la réponse à un bouton ; `false` si le message en a déjà une (une seule est permise).
    pub fn repondre_message(
        &self,
        id: &str,
        bouton: &str,
        par: &str,
        instant: u64,
    ) -> anyhow::Result<bool> {
        self.avec(|c| {
            c.execute(
                "UPDATE messages SET reponse_bouton = ?2, reponse_par = ?3, reponse_le = ?4,
                     reponse_livree = 0, lu_le = COALESCE(lu_le, ?4)
                 WHERE id = ?1 AND reponse_bouton IS NULL",
                params![id, bouton, par, instant],
            )
            .map(|n| n == 1)
        })
    }

    /// L'agent a accusé réception de la réponse.
    pub fn reponse_livree(&self, machine: &str, id: &str) -> anyhow::Result<()> {
        self.avec(|c| {
            c.execute(
                "UPDATE messages SET reponse_livree = 1 WHERE id = ?1 AND machine = ?2",
                params![id, machine],
            )
            .map(drop)
        })
    }

    /// Réponses que l'agent de cette machine n'a pas encore accusées : (id, bouton, par, quand).
    pub fn reponses_a_livrer(
        &self,
        machine: &str,
    ) -> anyhow::Result<Vec<(String, String, String, u64)>> {
        self.avec(|c| {
            c.prepare(
                "SELECT id, reponse_bouton, reponse_par, reponse_le FROM messages
                 WHERE machine = ?1 AND reponse_bouton IS NOT NULL AND reponse_livree = 0
                 ORDER BY reponse_le, id",
            )?
            .query_map([machine], |l| Ok((l.get(0)?, l.get(1)?, l.get(2)?, l.get(3)?)))?
            .collect()
        })
    }

    /// Supprime les messages reçus avant `instant` ; renvoie leur nombre.
    pub fn purger_messages(&self, instant: u64) -> anyhow::Result<usize> {
        self.avec(|c| c.execute("DELETE FROM messages WHERE recu_le < ?1", [instant]))
    }

    // --- Abonnements Web Push ---

    /// Ajoute un appareil, ou met à jour ses clés s'il est déjà abonné. Renvoie son numéro.
    pub fn ajouter_abonnement(
        &self,
        compte: &str,
        endpoint: &str,
        p256dh: &str,
        auth: &str,
        appareil: &str,
        instant: u64,
    ) -> anyhow::Result<i64> {
        self.avec(|c| {
            c.execute(
                "INSERT INTO abonnements_push (compte, endpoint, p256dh, auth, appareil, cree_le)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(endpoint) DO UPDATE SET compte = ?1, p256dh = ?3, auth = ?4,
                     appareil = ?5",
                params![compte, endpoint, p256dh, auth, appareil, instant],
            )?;
            c.query_row(
                "SELECT id FROM abonnements_push WHERE endpoint = ?1",
                [endpoint],
                |l| l.get(0),
            )
        })
    }

    pub fn abonnements(&self) -> anyhow::Result<Vec<Abonnement>> {
        self.avec(|c| {
            c.prepare(
                "SELECT id, compte, endpoint, p256dh, auth, appareil, cree_le
                 FROM abonnements_push ORDER BY id",
            )?
            .query_map([], |l| {
                Ok(Abonnement {
                    id: l.get(0)?,
                    compte: l.get(1)?,
                    endpoint: l.get(2)?,
                    p256dh: l.get(3)?,
                    auth: l.get(4)?,
                    appareil: l.get(5)?,
                    cree_le: l.get(6)?,
                })
            })?
            .collect()
        })
    }

    /// Retire un abonnement ; renvoie son appareil s'il existait.
    pub fn supprimer_abonnement(&self, id: i64) -> anyhow::Result<Option<String>> {
        self.avec(|c| {
            let appareil = c
                .query_row(
                    "SELECT appareil FROM abonnements_push WHERE id = ?1",
                    [id],
                    |l| l.get(0),
                )
                .optional()?;
            c.execute("DELETE FROM abonnements_push WHERE id = ?1", [id])?;
            Ok(appareil)
        })
    }

    // --- Journal d'audit ---

    pub fn noter(
        &self,
        instant: u64,
        compte: Option<&str>,
        adresse: &str,
        action: &str,
        detail: &str,
    ) -> anyhow::Result<()> {
        self.avec(|c| {
            c.execute(
                "INSERT INTO audit (instant, compte, adresse, action, detail)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![instant, compte, adresse, action, detail],
            )
            .map(drop)
        })
    }

    /// Les `limite` dernières lignes, de la plus récente à la plus ancienne.
    pub fn journal(&self, limite: u32) -> anyhow::Result<Vec<LigneAudit>> {
        self.avec(|c| {
            c.prepare(
                "SELECT instant, compte, adresse, action, detail FROM audit
                 ORDER BY id DESC LIMIT ?1",
            )?
            .query_map([limite], |l| {
                Ok(LigneAudit {
                    instant: l.get(0)?,
                    compte: l.get(1)?,
                    adresse: l.get(2)?,
                    action: l.get(3)?,
                    detail: l.get(4)?,
                })
            })?
            .collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comptes_et_sessions() {
        let base = Base::en_memoire();
        assert!(base.creer_compte("charles", "h", "S", 1).unwrap());
        assert!(!base.creer_compte("charles", "h2", "S2", 2).unwrap());
        assert_eq!(base.compte("charles").unwrap().unwrap().mot_de_passe, "h");

        assert!(base.noter_pas_totp("charles", 10).unwrap());
        assert!(!base.noter_pas_totp("charles", 10).unwrap());
        assert!(!base.noter_pas_totp("charles", 9).unwrap());

        base.creer_session("e", "charles", 100, 200, "1.2.3.4")
            .unwrap();
        assert_eq!(base.session("e", 150).unwrap().as_deref(), Some("charles"));
        assert_eq!(base.session("e", 200).unwrap(), None, "session expirée");
        assert_eq!(base.purger_sessions(200).unwrap(), 1);

        base.creer_session("f", "charles", 100, 200, "1.2.3.4")
            .unwrap();
        assert!(base.supprimer_compte("charles").unwrap());
        assert_eq!(
            base.session("f", 150).unwrap(),
            None,
            "sessions supprimées avec le compte"
        );
    }

    fn message(id: &str, genre: GenreMessage) -> MessageMachine {
        MessageMachine {
            id: id.into(),
            source: "cofiticket".into(),
            titre: "titre".into(),
            texte: "texte".into(),
            genre,
            boutons: vec![BoutonMessage {
                id: "ok".into(),
                libelle: "OK".into(),
            }],
            date: 50,
            detail: true,
        }
    }

    #[test]
    fn messages_et_reponses() {
        let base = Base::en_memoire();
        assert_eq!(
            base.enregistrer_message("pc", &message("a", GenreMessage::Action), 100).unwrap(),
            Enregistre::Nouveau
        );
        assert_eq!(
            base.enregistrer_message("pc", &message("a", GenreMessage::Action), 101).unwrap(),
            Enregistre::Deja
        );
        assert_eq!(
            base.enregistrer_message("autre", &message("a", GenreMessage::Info), 101).unwrap(),
            Enregistre::AutreMachine
        );
        base.enregistrer_message("pc", &message("b", GenreMessage::Alerte), 200).unwrap();
        let (liste, non_lus) = base.messages(&FiltreMessages::default(), 10).unwrap();
        assert_eq!((liste[0].id.as_str(), liste[1].id.as_str(), non_lus), ("b", "a", 2));
        assert_eq!(liste[1].genre, GenreMessage::Action);
        assert_eq!(liste[1].boutons[0].libelle, "OK");
        assert!(liste[1].detail && liste[1].reponse.is_none());

        base.marquer_lu("a", 300).unwrap();
        let (non_lus_seuls, total) = base
            .messages(&FiltreMessages { non_lus: true, ..Default::default() }, 10)
            .unwrap();
        assert_eq!((non_lus_seuls.len(), total), (1, 1));
        let filtre = FiltreMessages { source: Some("ccs".into()), ..Default::default() };
        assert!(base.messages(&filtre, 10).unwrap().0.is_empty());

        // Une seule réponse ; livrée seulement après l'accusé de l'agent.
        assert!(base.repondre_message("a", "ok", "mawena", 400).unwrap());
        assert!(!base.repondre_message("a", "ok", "autre", 401).unwrap());
        assert_eq!(base.reponses_a_livrer("pc").unwrap(), [("a".into(), "ok".into(), "mawena".into(), 400)]);
        assert!(base.reponses_a_livrer("autre").unwrap().is_empty());
        base.reponse_livree("autre", "a").unwrap();
        assert_eq!(base.reponses_a_livrer("pc").unwrap().len(), 1, "accusé d'une autre machine ignoré");
        base.reponse_livree("pc", "a").unwrap();
        assert!(base.reponses_a_livrer("pc").unwrap().is_empty());
        let reponse = base.message("a").unwrap().unwrap().reponse.unwrap();
        assert_eq!((reponse.par.as_str(), reponse.livree), ("mawena", true));

        assert_eq!(base.purger_messages(150).unwrap(), 1);
        assert!(base.message("a").unwrap().is_none());
        assert!(base.message("b").unwrap().is_some());
    }

    #[test]
    fn abonnements_et_reglages() {
        let base = Base::en_memoire();
        let a = base.ajouter_abonnement("mawena", "https://x/1", "k", "s", "iPhone", 1).unwrap();
        let b = base.ajouter_abonnement("mawena", "https://x/2", "k", "s", "Chrome", 2).unwrap();
        assert_ne!(a, b);
        // Même appareil : mise à jour, pas de doublon.
        assert_eq!(base.ajouter_abonnement("mawena", "https://x/1", "k2", "s2", "iPhone 2", 3).unwrap(), a);
        let liste = base.abonnements().unwrap();
        assert_eq!(liste.len(), 2);
        assert_eq!((liste[0].p256dh.as_str(), liste[0].appareil.as_str()), ("k2", "iPhone 2"));
        assert_eq!(base.supprimer_abonnement(a).unwrap().as_deref(), Some("iPhone 2"));
        assert_eq!(base.supprimer_abonnement(a).unwrap(), None);
        assert_eq!(base.abonnements().unwrap().len(), 1);

        assert!(base.definir_reglage_si_absent("cle", "1").unwrap());
        assert!(!base.definir_reglage_si_absent("cle", "2").unwrap());
        assert_eq!(base.reglage("cle").unwrap().as_deref(), Some("1"));
        assert_eq!(base.reglage("autre").unwrap(), None);
    }

    #[test]
    fn machines_et_journal() {
        let base = Base::en_memoire();
        assert!(base.ajouter_machine("pc", Some("PC"), "h", 1).unwrap());
        assert!(!base.ajouter_machine("pc", None, "h", 2).unwrap());
        assert_eq!(base.empreinte_machine("pc").unwrap().as_deref(), Some("h"));
        assert_eq!(base.machines().unwrap()[0].nom.as_deref(), Some("PC"));
        assert!(base.supprimer_machine("pc").unwrap());
        assert!(base.empreinte_machine("pc").unwrap().is_none());

        base.noter(1, None, "a", "premier", "").unwrap();
        base.noter(2, Some("charles"), "a", "second", "x").unwrap();
        let journal = base.journal(10).unwrap();
        assert_eq!(journal[0].action, "second");
        assert_eq!(base.journal(1).unwrap().len(), 1);
    }
}
