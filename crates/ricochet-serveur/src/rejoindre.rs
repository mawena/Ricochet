//! Lien d'installation à usage unique : `curl -fsSL https://<relais>/rejoindre/<code> | sudo sh`.
//!
//! À l'ajout d'une machine, le relais garde en mémoire (15 minutes, une seule lecture) de quoi
//! écrire le script qui installe l'agent, écrit sa configuration (jeton compris), prépare le
//! serveur SSH et ses clés autorisées si le port 22 est ouvert aux tunnels, puis le démarre.
//! Le jeton n'apparaît donc jamais à l'écran ; le lien lu ou expiré, il est oublié.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};

use crate::{Adresse, Relais, jeton};

/// Durée de validité d'un lien.
pub const VALIDITE: Duration = Duration::from_secs(15 * 60);
/// Ports ouverts aux tunnels, au plus.
pub const PORTS_MAX: usize = 16;
/// Clés SSH autorisées, au plus.
pub const CLES_MAX: usize = 16;

/// Ce qu'il faut pour écrire le script d'une machine.
#[derive(Clone, Debug, PartialEq)]
pub struct Installation {
    pub id: String,
    pub jeton: String,
    pub utilisateur: Option<String>,
    pub ports: Vec<u16>,
    pub cles_ssh: Vec<String>,
}

/// Liens en attente, en mémoire : code → (création, installation).
#[derive(Default)]
pub struct Liens(Mutex<HashMap<String, (Instant, Installation)>>);

impl Liens {
    /// Garde une installation et renvoie son code (13 caractères, 64 bits d'aléa).
    pub fn ajouter(&self, installation: Installation) -> std::io::Result<String> {
        let code = data_encoding::BASE32_NOPAD
            .encode(&jeton::octets_aleatoires::<8>()?)
            .to_ascii_lowercase();
        let mut liens = self.0.lock().unwrap_or_else(|e| e.into_inner());
        liens.retain(|_, (cree, _)| cree.elapsed() < VALIDITE);
        liens.insert(code.clone(), (Instant::now(), installation));
        Ok(code)
    }

    /// Rend l'installation une seule fois, si le lien n'a pas expiré.
    pub fn prendre(&self, code: &str) -> Option<Installation> {
        let mut liens = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let (cree, installation) = liens.remove(code)?;
        (cree.elapsed() < VALIDITE).then_some(installation)
    }

    /// Oublie les liens d'une machine (révoquée).
    pub fn oublier_machine(&self, id: &str) {
        let mut liens = self.0.lock().unwrap_or_else(|e| e.into_inner());
        liens.retain(|_, (_, installation)| installation.id != id);
    }
}

/// Nom d'utilisateur Unix sûr à mettre tel quel dans un script.
pub fn utilisateur_valide(nom: &str) -> bool {
    let mut car = nom.chars();
    nom.len() <= 32
        && car
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && car.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-'))
}

/// Clé publique SSH sur une ligne (`type base64 [commentaire]`), sans caractère dangereux pour le
/// script ni option (`command=…`) devant le type.
pub fn cle_ssh_valide(cle: &str) -> bool {
    let mut morceaux = cle.split(' ');
    let (Some(genre), Some(corps)) = (morceaux.next(), morceaux.next()) else {
        return false;
    };
    let genre_connu = matches!(genre, "ssh-ed25519" | "ssh-rsa")
        || genre.starts_with("ecdsa-sha2-nistp")
        || genre.starts_with("sk-ssh-ed25519@")
        || genre.starts_with("sk-ecdsa-sha2-nistp256@");
    let commentaire: String = morceaux.collect::<Vec<_>>().join(" ");
    cle.len() <= 4096
        && genre_connu
        && corps.len() >= 16
        && corps
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '='))
        && commentaire
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || " @._-:+".contains(c))
}

/// Hôte du relais tel que la machine l'a joint, sûr à écrire dans le script.
fn hote_valide(hote: &str) -> bool {
    !hote.is_empty()
        && hote.len() <= 253
        && hote
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']'))
}

/// Écrit le script d'installation. Tout est dans la fonction `principal`, appelée à la dernière
/// ligne : le shell lit donc le script en entier avant de lancer quoi que ce soit, et les
/// commandes ne peuvent pas avaler la suite sur l'entrée standard.
pub fn script(installation: &Installation, relais: &str) -> String {
    let Installation {
        id,
        jeton,
        utilisateur,
        ports,
        cles_ssh,
    } = installation;
    let mut config = format!("relais = \"{relais}\"\nid = \"{id}\"\njeton = \"{jeton}\"\n");
    if let Some(u) = utilisateur {
        config.push_str(&format!(
            "\n[terminal]\ncommande = [\"runuser\", \"-l\", \"{u}\"]\nmaximum = 8\n\
             \n[fichiers]\nutilisateur = \"{u}\"\n"
        ));
    }
    if !ports.is_empty() {
        let liste: Vec<String> = ports.iter().map(u16::to_string).collect();
        config.push_str(&format!(
            "\n[tunnel]\nports = [{}]\nmaximum = 16\n",
            liste.join(", ")
        ));
    }

    let mut s = String::from(
        "#!/bin/sh\n\
         # Ricochet : ajout de cette machine au relais (lien à usage unique).\n\
         set -eu\n\
         \n\
         dire() { printf '\\033[1m→ %s\\033[0m\\n' \"$*\"; }\n\
         \n\
         principal() {\n\
         \x20 if [ \"$(id -u)\" != 0 ]; then\n\
         \x20   echo \"Ricochet : à lancer avec sudo (curl -fsSL … | sudo sh).\" >&2\n\
         \x20   exit 1\n\
         \x20 fi\n",
    );
    s.push_str(&format!("  dire \"Machine « {id} »\"\n"));
    if let Some(u) = utilisateur {
        s.push_str(&format!(
            "  if ! getent passwd {u} >/dev/null; then\n\
             \x20   echo \"Ricochet : l'utilisateur « {u} » n'existe pas sur cette machine.\" >&2\n\
             \x20   exit 1\n\
             \x20 fi\n"
        ));
    }
    s.push_str(
        "  dire \"Installation de l'agent Ricochet\"\n\
         \x20 curl -fsSL https://apt.mawena.cloud/installer.sh | sh -s -- ricochet-agent </dev/null\n",
    );
    if ports.contains(&22) {
        s.push_str(
            "  if [ ! -x /usr/sbin/sshd ]; then\n\
             \x20   dire \"Installation du serveur SSH\"\n\
             \x20   DEBIAN_FRONTEND=noninteractive apt-get install -y openssh-server </dev/null\n\
             \x20 fi\n\
             \x20 systemctl enable --now ssh >/dev/null 2>&1 || systemctl enable --now sshd >/dev/null 2>&1 || true\n",
        );
        if let (Some(u), false) = (utilisateur, cles_ssh.is_empty()) {
            s.push_str(&format!(
                "  dire \"Clés SSH autorisées pour {u}\"\n\
                 \x20 maison=$(getent passwd {u} | cut -d: -f6)\n\
                 \x20 groupe=$(id -gn {u})\n\
                 \x20 install -d -m 700 -o {u} -g \"$groupe\" \"$maison/.ssh\"\n\
                 \x20 cles=\"$maison/.ssh/authorized_keys\"\n\
                 \x20 [ -e \"$cles\" ] || install -m 600 -o {u} -g \"$groupe\" /dev/null \"$cles\"\n"
            ));
            for cle in cles_ssh {
                s.push_str(&format!(
                    "  grep -qxF '{cle}' \"$cles\" || printf '%s\\n' '{cle}' >>\"$cles\"\n"
                ));
            }
        }
    }
    s.push_str(&format!(
        "  dire \"Configuration de l'agent\"\n\
         \x20 install -d -m 755 /etc/ricochet\n\
         \x20 [ ! -e /etc/ricochet/agent.toml ] || cp -p /etc/ricochet/agent.toml /etc/ricochet/agent.toml.avant\n\
         \x20 install -m 600 /dev/null /etc/ricochet/agent.toml.nouveau\n\
         \x20 cat >/etc/ricochet/agent.toml.nouveau <<'FIN_CONFIG'\n\
         {config}FIN_CONFIG\n\
         \x20 mv /etc/ricochet/agent.toml.nouveau /etc/ricochet/agent.toml\n\
         \x20 systemctl enable ricochet-agent >/dev/null 2>&1 || true\n\
         \x20 systemctl restart ricochet-agent\n\
         \x20 dire \"Terminé : « {id} » apparaît en ligne d'ici quelques secondes.\"\n\
         }}\n\
         \n\
         principal\n"
    ));
    s
}

/// `GET /rejoindre/{code}` : le script, une seule fois, sans connexion.
pub async fn servir(
    State(relais): State<Arc<Relais>>,
    Adresse(adresse): Adresse,
    Path(code): Path<String>,
    entetes: HeaderMap,
) -> Response {
    let hote = entetes
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    if !hote_valide(hote) {
        return (StatusCode::BAD_REQUEST, "hôte invalide\n").into_response();
    }
    let Some(installation) = relais.liens.prendre(&code) else {
        return (
            StatusCode::NOT_FOUND,
            "echo \"Ricochet : lien inconnu, déjà utilisé ou expiré (15 minutes) : \
             recréer la machine.\" >&2; exit 1\n",
        )
            .into_response();
    };
    relais.noter(None, &adresse, "lien_installation_lu", &installation.id);
    let schema = if relais.config.cookie_securise {
        "wss"
    } else {
        "ws"
    };
    (
        [
            (header::CONTENT_TYPE, "text/x-shellscript; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        script(&installation, &format!("{schema}://{hote}/agent")),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn installation() -> Installation {
        Installation {
            id: "pc-salon".into(),
            jeton: "abc123".into(),
            utilisateur: Some("charles".into()),
            ports: vec![22, 7341],
            cles_ssh: vec!["ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIabc ccs@vps".into()],
        }
    }

    #[test]
    fn lien_a_usage_unique() {
        let liens = Liens::default();
        let code = liens.ajouter(installation()).unwrap();
        assert_eq!(code.len(), 13);
        assert!(liens.prendre("inconnu").is_none());
        assert_eq!(liens.prendre(&code), Some(installation()));
        assert!(liens.prendre(&code).is_none());
        let code = liens.ajouter(installation()).unwrap();
        liens.oublier_machine("pc-salon");
        assert!(liens.prendre(&code).is_none());
    }

    #[test]
    fn validations() {
        assert!(utilisateur_valide("charles-gamligo"));
        assert!(!utilisateur_valide("Charles"));
        assert!(!utilisateur_valide("a;b"));
        assert!(!utilisateur_valide(""));
        assert!(cle_ssh_valide(
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIabc ccs@vps"
        ));
        assert!(cle_ssh_valide("ssh-rsa AAAAB3NzaC1yc2EAAAADAQAB"));
        assert!(!cle_ssh_valide("ssh-ed25519 AAAA'; rm -rf / #"));
        assert!(!cle_ssh_valide(
            "command=\"x\" ssh-ed25519 AAAAC3NzaC1lZDI1NTE5"
        ));
        assert!(!cle_ssh_valide(
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA\nssh-rsa x"
        ));
        assert!(!cle_ssh_valide("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5 a$b"));
        assert!(hote_valide("ricochet.mawena.cloud"));
        assert!(!hote_valide("a\"; b"));
    }

    #[test]
    fn script_complet() {
        let s = script(&installation(), "wss://r.exemple/agent");
        assert!(s.contains("relais = \"wss://r.exemple/agent\"\nid = \"pc-salon\""));
        assert!(s.contains("commande = [\"runuser\", \"-l\", \"charles\"]"));
        assert!(s.contains("[tunnel]\nports = [22, 7341]"));
        assert!(s.contains("openssh-server"));
        assert!(s.contains("ccs@vps' >>\"$cles\""));
        assert!(s.trim_end().ends_with("principal"));
    }

    #[test]
    fn script_minimal() {
        let mut i = installation();
        i.utilisateur = None;
        i.ports.clear();
        let s = script(&i, "wss://r/agent");
        assert!(!s.contains("[terminal]"));
        assert!(!s.contains("[tunnel]"));
        assert!(!s.contains("openssh-server"));
        assert!(!s.contains("authorized_keys"));
    }

    #[test]
    fn script_valide_pour_sh() {
        let chemin =
            std::env::temp_dir().join(format!("ricochet-rejoindre-{}.sh", std::process::id()));
        std::fs::write(&chemin, script(&installation(), "wss://r/agent")).unwrap();
        let sortie = std::process::Command::new("sh")
            .arg("-n")
            .arg(&chemin)
            .output()
            .unwrap();
        let _ = std::fs::remove_file(&chemin);
        assert!(
            sortie.status.success(),
            "{}",
            String::from_utf8_lossy(&sortie.stderr)
        );
    }
}
