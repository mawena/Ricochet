//! Mode Portier de l'interface web.
//!
//! Derrière nginx, Portier (connexion unique de mawena.cloud) vérifie l'identité et nginx
//! transmet le nom vérifié dans l'en-tête `X-Portier-Utilisateur`. Le relais ne fait confiance à
//! cet en-tête que sur la page de l'interface (`GET /`), si l'option est activée, si la
//! connexion vient de 127.0.0.1 ou ::1 et si le nom est dans la liste. Il ouvre alors une
//! session de l'interface (même cookie qu'après mot de passe et TOTP), gardée en mémoire car le
//! nom vient de Portier et non des comptes du relais.
//!
//! L'en-tête n'est jamais lu ailleurs : ni par `/agent`, ni par l'API (cookie ou jeton).

use std::{collections::HashMap, net::SocketAddr, sync::Mutex};

use axum::{
    extract::ConnectInfo,
    http::{HeaderMap, request::Parts},
};

use crate::{Relais, config};

/// En-tête posé par nginx après la vérification de Portier.
pub const ENTETE: &str = "x-portier-utilisateur";

/// Nom vérifié par Portier, s'il faut lui faire confiance pour cette requête.
pub(crate) fn utilisateur(relais: &Relais, parts: &Parts) -> Option<String> {
    let pair = parts
        .extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(pair)| *pair)?;
    utilisateur_de(&relais.config.portier, pair, &parts.headers)
}

fn utilisateur_de(
    portier: &config::Portier,
    pair: SocketAddr,
    entetes: &HeaderMap,
) -> Option<String> {
    if !portier.actif || !pair.ip().to_canonical().is_loopback() {
        return None;
    }
    let mut valeurs = entetes.get_all(ENTETE).iter();
    let valeur = valeurs.next()?;
    if valeurs.next().is_some() {
        // Plusieurs en-têtes : ambigu, ignoré.
        return None;
    }
    let nom = valeur.to_str().ok()?.trim();
    portier
        .utilisateurs
        .iter()
        .any(|u| u == nom)
        .then(|| nom.to_owned())
}

/// Adresse de l'interface telle que le navigateur l'a demandée, reconstruite à partir des
/// en-têtes `Host` et `X-Forwarded-Proto` (par défaut https si le cookie est sécurisé).
pub(crate) fn adresse_interface(entetes: &HeaderMap, cookie_securise: bool) -> String {
    let protocole = match entetes
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("https") => "https",
        Some("http") => "http",
        _ if cookie_securise => "https",
        _ => "http",
    };
    let hote = entetes
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .filter(|h| {
            !h.is_empty()
                && h.chars()
                    .all(|c| c.is_ascii_alphanumeric() || ".-:[]".contains(c))
        })
        .unwrap_or("localhost");
    format!("{protocole}://{hote}/")
}

/// Encodage en pourcentage de tout sauf les caractères non réservés (RFC 3986).
pub(crate) fn encoder(texte: &str) -> String {
    let mut sortie = String::with_capacity(texte.len() * 3);
    for octet in texte.bytes() {
        if octet.is_ascii_alphanumeric() || b"-._~".contains(&octet) {
            sortie.push(octet as char);
        } else {
            sortie.push_str(&format!("%{octet:02X}"));
        }
    }
    sortie
}

/// Adresse du bouton « Se connecter via Portier ».
pub(crate) fn adresse_connexion(portier: &config::Portier, retour: &str) -> String {
    format!("{}/connexion?retour={}", portier.base(), encoder(retour))
}

/// Sessions ouvertes par Portier : empreinte du cookie → (nom, expiration). En mémoire : après
/// un redémarrage du relais, la page suivante en rouvre une.
#[derive(Default)]
pub struct Sessions(Mutex<HashMap<String, (String, u64)>>);

impl Sessions {
    pub fn ouvrir(&self, empreinte: String, nom: &str, expire_le: u64) {
        self.verrou().insert(empreinte, (nom.to_owned(), expire_le));
    }

    pub fn compte(&self, empreinte: &str, instant: u64) -> Option<String> {
        self.verrou()
            .get(empreinte)
            .filter(|(_, expire_le)| *expire_le > instant)
            .map(|(nom, _)| nom.clone())
    }

    /// Vrai si la session avait été ouverte par Portier.
    pub fn supprimer(&self, empreinte: &str) -> bool {
        self.verrou().remove(empreinte).is_some()
    }

    pub fn purger(&self, instant: u64) {
        self.verrou()
            .retain(|_, (_, expire_le)| *expire_le > instant);
    }

    fn verrou(&self) -> std::sync::MutexGuard<'_, HashMap<String, (String, u64)>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn portier(actif: bool) -> config::Portier {
        config::Portier {
            actif,
            utilisateurs: vec!["charles-gamligo".into()],
            url: "https://portier.mawena.cloud".into(),
            deconnexion: None,
        }
    }

    fn entetes(valeurs: &[&str]) -> HeaderMap {
        let mut entetes = HeaderMap::new();
        for valeur in valeurs {
            entetes.append(ENTETE, valeur.parse().unwrap());
        }
        entetes
    }

    #[test]
    fn confiance_dans_l_entete() {
        let local: SocketAddr = "127.0.0.1:4000".parse().unwrap();
        let local6: SocketAddr = "[::1]:4000".parse().unwrap();
        let distant: SocketAddr = "203.0.113.5:4000".parse().unwrap();
        let bon = entetes(&["charles-gamligo"]);
        assert_eq!(
            utilisateur_de(&portier(true), local, &bon).as_deref(),
            Some("charles-gamligo")
        );
        assert!(utilisateur_de(&portier(true), local6, &bon).is_some());
        // IP non locale, option désactivée, nom absent, en-tête vide ou doublé : refusé.
        assert!(utilisateur_de(&portier(true), distant, &bon).is_none());
        assert!(utilisateur_de(&portier(false), local, &bon).is_none());
        assert!(utilisateur_de(&portier(true), local, &entetes(&["intrus"])).is_none());
        assert!(utilisateur_de(&portier(true), local, &entetes(&[""])).is_none());
        assert!(utilisateur_de(&portier(true), local, &HeaderMap::new()).is_none());
        let double = entetes(&["charles-gamligo", "charles-gamligo"]);
        assert!(utilisateur_de(&portier(true), local, &double).is_none());
    }

    #[test]
    fn adresse_de_retour() {
        let mut entetes = HeaderMap::new();
        entetes.insert("host", "ricochet.mawena.cloud".parse().unwrap());
        assert_eq!(
            adresse_interface(&entetes, true),
            "https://ricochet.mawena.cloud/"
        );
        assert_eq!(
            adresse_interface(&entetes, false),
            "http://ricochet.mawena.cloud/"
        );
        entetes.insert("x-forwarded-proto", "https".parse().unwrap());
        let retour = adresse_interface(&entetes, false);
        assert_eq!(retour, "https://ricochet.mawena.cloud/");
        assert_eq!(
            adresse_connexion(&portier(true), &retour),
            "https://portier.mawena.cloud/connexion?retour=https%3A%2F%2Fricochet.mawena.cloud%2F"
        );
        // Un Host fantaisiste n'est pas recopié.
        entetes.insert("host", "a/b?c".parse().unwrap());
        assert_eq!(adresse_interface(&entetes, true), "https://localhost/");
    }

    #[test]
    fn sessions_en_memoire() {
        let sessions = Sessions::default();
        sessions.ouvrir("e".into(), "charles-gamligo", 100);
        assert_eq!(sessions.compte("e", 50).as_deref(), Some("charles-gamligo"));
        assert!(sessions.compte("e", 100).is_none());
        sessions.purger(100);
        assert!(sessions.compte("e", 0).is_none());
        sessions.ouvrir("f".into(), "charles-gamligo", 100);
        sessions.supprimer("f");
        assert!(sessions.compte("f", 0).is_none());
    }
}
