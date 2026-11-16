//! Base de données du relais (SQLite) : comptes opérateur, sessions, machines, journal d'audit.
//!
//! Les requêtes sont courtes ; une seule connexion protégée par un verrou suffit. La commande
//! `ricochet-serveur compte …` ouvre la même base pendant que le relais tourne (mode WAL).

use std::{path::Path, sync::Mutex};

use anyhow::Context;
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
