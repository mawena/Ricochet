//! Côté programme : la sous-commande `ricochet-agent message …` parle au socket de l'agent.

use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::Path,
    time::Duration,
};

use anyhow::{Context, bail};
use serde_json::{Value, json};

use crate::messages::{DETAIL_MAX, REQUETE_MAX};

/// Une requête, une réponse, fermeture : renvoie la réponse de l'agent.
pub fn appeler(socket: &Path, requete: &Value) -> anyhow::Result<Value> {
    let mut flux = UnixStream::connect(socket).with_context(|| {
        format!(
            "connexion à {} (l'agent tourne-t-il, avec une section [messages] ?)",
            socket.display()
        )
    })?;
    flux.set_read_timeout(Some(Duration::from_secs(30)))?;
    flux.set_write_timeout(Some(Duration::from_secs(30)))?;
    let mut ligne = requete.to_string();
    if ligne.len() >= REQUETE_MAX {
        bail!("requête trop grande (96 Kio au plus)");
    }
    ligne.push('\n');
    flux.write_all(ligne.as_bytes())?;
    let mut reponse = String::new();
    flux.take(REQUETE_MAX as u64).read_to_string(&mut reponse)?;
    serde_json::from_str(reponse.trim()).context("réponse illisible de l'agent")
}

/// Ce que donne la ligne de commande pour `message envoyer`.
pub struct Envoi {
    pub source: String,
    pub titre: String,
    pub texte: Option<String>,
    pub genre: Option<String>,
    /// `id=Libellé`
    pub boutons: Vec<String>,
    /// Chemin du détail, ou `-` pour l'entrée standard.
    pub detail_fichier: Option<String>,
    pub cle: Option<String>,
}

/// Lit le détail (au plus `DETAIL_MAX` octets, sinon erreur).
fn lire_detail(chemin: &str) -> anyhow::Result<String> {
    let mut octets = Vec::new();
    let limite = DETAIL_MAX as u64 + 1;
    if chemin == "-" {
        std::io::stdin().take(limite).read_to_end(&mut octets)?;
    } else {
        std::fs::File::open(chemin)
            .with_context(|| format!("lecture de {chemin}"))?
            .take(limite)
            .read_to_end(&mut octets)?;
    }
    if octets.len() > DETAIL_MAX {
        bail!("détail trop long (64 Kio au plus)");
    }
    String::from_utf8(octets).context("le détail doit être en UTF-8")
}

pub fn requete_envoi(envoi: &Envoi) -> anyhow::Result<Value> {
    let mut requete = json!({ "op": "envoyer", "source": envoi.source, "titre": envoi.titre });
    if let Some(texte) = &envoi.texte {
        requete["texte"] = json!(texte);
    }
    if let Some(genre) = &envoi.genre {
        requete["genre"] = json!(genre);
    }
    if let Some(cle) = &envoi.cle {
        requete["cle"] = json!(cle);
    }
    if !envoi.boutons.is_empty() {
        let mut boutons = Vec::new();
        for bouton in &envoi.boutons {
            let Some((id, libelle)) = bouton.split_once('=') else {
                bail!("bouton « {bouton} » : écrire id=Libellé");
            };
            boutons.push(json!({ "id": id, "libelle": libelle }));
        }
        requete["boutons"] = json!(boutons);
    }
    if let Some(chemin) = &envoi.detail_fichier {
        requete["detail"] = json!(lire_detail(chemin)?);
    }
    Ok(requete)
}

/// Affiche la réponse et renvoie le code de sortie : 0 si `ok`, 1 sinon.
pub fn terminer(reponse: &Value) -> i32 {
    println!("{reponse}");
    i32::from(reponse["ok"] != true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requete_depuis_la_ligne_de_commande() {
        let envoi = Envoi {
            source: "cofiticket".into(),
            titre: "T".into(),
            texte: Some("x".into()),
            genre: Some("action".into()),
            boutons: vec!["resoudre=Résoudre".into(), "a=b=c".into()],
            detail_fichier: None,
            cle: Some("k".into()),
        };
        let r = requete_envoi(&envoi).unwrap();
        assert_eq!(r["op"], "envoyer");
        assert_eq!(r["boutons"][0], json!({"id": "resoudre", "libelle": "Résoudre"}));
        assert_eq!(r["boutons"][1]["libelle"], "b=c");
        assert_eq!(r["cle"], "k");
        let mauvais = Envoi { boutons: vec!["sans-egal".into()], ..envoi };
        assert!(requete_envoi(&mauvais).is_err());
    }
}
