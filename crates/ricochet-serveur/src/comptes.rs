//! Mots de passe (Argon2id) et limitation des essais de connexion.

use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
};

use argon2::{
    Argon2,
    password_hash::{PasswordHasher, PasswordVerifier, phc::PasswordHash},
};

/// Longueur minimale d'un mot de passe opérateur.
pub const LONGUEUR_MIN: usize = 12;

/// Empreinte Argon2id (format PHC, sel aléatoire) d'un mot de passe. Coûteux : hors des tâches
/// asynchrones (`spawn_blocking`).
pub fn hacher(mot_de_passe: &str) -> anyhow::Result<String> {
    Ok(Argon2::default()
        .hash_password(mot_de_passe.as_bytes())
        .map_err(|e| anyhow::anyhow!("hachage du mot de passe : {e}"))?
        .to_string())
}

/// Vérifie un mot de passe ; `empreinte = None` (compte inconnu) fait le même travail pour
/// ne pas révéler par le temps de réponse quels comptes existent.
pub fn verifier(mot_de_passe: &str, empreinte: Option<&str>) -> bool {
    static FACTICE: OnceLock<String> = OnceLock::new();
    let factice = FACTICE.get_or_init(|| hacher("compte-inexistant").unwrap_or_default());
    let Ok(empreinte_lue) = PasswordHash::new(empreinte.unwrap_or(factice)) else {
        return false;
    };
    let bon = Argon2::default()
        .verify_password(mot_de_passe.as_bytes(), &empreinte_lue)
        .is_ok();
    bon && empreinte.is_some()
}

/// Échecs tolérés par fenêtre pour une même adresse.
pub const ECHECS_MAX_ADRESSE: usize = 5;
/// Échecs tolérés par fenêtre pour un même compte, toutes adresses confondues : plus large,
/// pour qu'un inconnu ne puisse pas bloquer l'opérateur trop facilement.
pub const ECHECS_MAX_COMPTE: usize = 20;
/// Fenêtre glissante des échecs, en secondes.
pub const FENETRE_S: u64 = 15 * 60;
/// Au-delà, on oublie les plus anciennes clés (protection de la mémoire).
const CLES_MAX: usize = 10_000;

/// Compte les échecs de connexion récents par clé (adresse ou compte).
pub struct Limiteur {
    max: usize,
    echecs: Mutex<HashMap<String, Vec<u64>>>,
}

impl Limiteur {
    pub fn new(max: usize) -> Self {
        Self {
            max,
            echecs: Mutex::default(),
        }
    }

    /// Vrai si la clé a épuisé ses essais.
    pub fn bloque(&self, cle: &str, instant: u64) -> bool {
        let echecs = self.echecs.lock().unwrap_or_else(|e| e.into_inner());
        echecs.get(cle).is_some_and(|liste| {
            liste.iter().filter(|&&t| t + FENETRE_S > instant).count() >= self.max
        })
    }

    pub fn echec(&self, cle: &str, instant: u64) {
        let mut echecs = self.echecs.lock().unwrap_or_else(|e| e.into_inner());
        if echecs.len() >= CLES_MAX {
            echecs.retain(|_, liste| liste.iter().any(|&t| t + FENETRE_S > instant));
        }
        let liste = echecs.entry(cle.to_owned()).or_default();
        liste.retain(|&t| t + FENETRE_S > instant);
        liste.push(instant);
    }

    pub fn oublier(&self, cle: &str) {
        self.echecs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(cle);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mot_de_passe() {
        let empreinte = hacher("correct horse battery").unwrap();
        assert!(empreinte.starts_with("$argon2id$"));
        assert!(verifier("correct horse battery", Some(&empreinte)));
        assert!(!verifier("autre", Some(&empreinte)));
        assert!(!verifier("compte-inexistant", None));
        assert!(!verifier("x", Some("pas une empreinte")));
    }

    #[test]
    fn limitation() {
        let limiteur = Limiteur::new(ECHECS_MAX_ADRESSE);
        for i in 0..ECHECS_MAX_ADRESSE as u64 {
            assert!(!limiteur.bloque("ip:a", 100 + i));
            limiteur.echec("ip:a", 100 + i);
        }
        assert!(limiteur.bloque("ip:a", 110));
        assert!(!limiteur.bloque("ip:b", 110));
        // Les échecs s'effacent après la fenêtre.
        assert!(!limiteur.bloque("ip:a", 105 + FENETRE_S));
        limiteur.oublier("ip:a");
        assert!(!limiteur.bloque("ip:a", 110));
    }
}
