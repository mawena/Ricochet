use std::{
    io::{BufRead, IsTerminal, Write},
    path::PathBuf,
};

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use ricochet_serveur::{Base, Config, comptes, jeton, registre::maintenant, totp};
use tracing_subscriber::EnvFilter;

const CONFIG_PAR_DEFAUT: &str = "/etc/ricochet/serveur.toml";

/// Relais Ricochet : reçoit les connexions des agents et sert l'interface de contrôle.
#[derive(Parser)]
#[command(version)]
struct Arguments {
    /// Fichier de configuration.
    #[arg(long, short, global = true, default_value = CONFIG_PAR_DEFAUT)]
    config: PathBuf,
    #[command(subcommand)]
    commande: Option<Commande>,
}

#[derive(Subcommand)]
enum Commande {
    /// Lance le relais (commande par défaut).
    Servir,
    /// Gère les comptes opérateur (mot de passe + code TOTP).
    #[command(subcommand)]
    Compte(CommandeCompte),
    /// Crée un jeton pour les scripts : le jeton reste chez vous, l'empreinte va dans
    /// `jeton_operateur_sha256` de la configuration.
    Jeton,
}

#[derive(Subcommand)]
enum CommandeCompte {
    /// Crée un compte : demande le mot de passe, puis affiche le secret TOTP à enregistrer
    /// dans l'application d'authentification du téléphone.
    Ajouter { nom: String },
    /// Liste les comptes.
    Liste,
    /// Supprime un compte et ferme ses sessions.
    Supprimer { nom: String },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let arguments = Arguments::parse();
    match arguments.commande {
        Some(Commande::Jeton) => {
            let nouveau = jeton::nouveau()?;
            println!("jeton     : {nouveau}");
            println!("empreinte : {}", jeton::empreinte(&nouveau));
            Ok(())
        }
        Some(Commande::Compte(commande)) => {
            let config = Config::lire(&arguments.config)?;
            let base = Base::ouvrir(&config.base)?;
            gerer_compte(&base, commande)
        }
        Some(Commande::Servir) | None => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
                )
                .init();
            ricochet_serveur::servir(Config::lire(&arguments.config)?).await
        }
    }
}

fn gerer_compte(base: &Base, commande: CommandeCompte) -> anyhow::Result<()> {
    match commande {
        CommandeCompte::Ajouter { nom } => ajouter_compte(base, &nom),
        CommandeCompte::Liste => {
            for (nom, cree_le) in base.comptes()? {
                println!("{nom}\tcréé le {} (UTC)", date_utc(cree_le));
            }
            Ok(())
        }
        CommandeCompte::Supprimer { nom } => {
            if !base.supprimer_compte(&nom)? {
                bail!("compte « {nom} » inconnu");
            }
            base.noter(maintenant(), None, "console", "compte_supprime", &nom)?;
            println!("compte « {nom} » supprimé, ses sessions sont fermées");
            Ok(())
        }
    }
}

/// Lit une ligne : sans écho si l'entrée est un terminal, sinon telle quelle (scripts).
fn lire(invite: &str, secret: bool) -> anyhow::Result<String> {
    if std::io::stdin().is_terminal() {
        if secret {
            return Ok(rpassword::prompt_password(invite)?);
        }
        print!("{invite}");
        std::io::stdout().flush()?;
    }
    let mut ligne = String::new();
    std::io::stdin().lock().read_line(&mut ligne)?;
    Ok(ligne.trim_end_matches(['\r', '\n']).to_owned())
}

fn ajouter_compte(base: &Base, nom: &str) -> anyhow::Result<()> {
    if nom.is_empty() || nom.len() > 64 || nom.chars().any(char::is_control) {
        bail!("nom de compte invalide (1 à 64 caractères)");
    }
    if base.compte(nom)?.is_some() {
        bail!("le compte « {nom} » existe déjà");
    }
    let mot_de_passe = lire("Mot de passe : ", true)?;
    if mot_de_passe.chars().count() < comptes::LONGUEUR_MIN {
        bail!(
            "mot de passe trop court ({} caractères au moins)",
            comptes::LONGUEUR_MIN
        );
    }
    if lire("Encore une fois : ", true)? != mot_de_passe {
        bail!("les deux mots de passe diffèrent");
    }

    let secret = totp::nouveau_secret()?;
    let lien = totp::lien(&secret, nom);
    println!("\nAjoutez ce compte dans l'application d'authentification du téléphone :");
    println!("{}", qr_code(&lien)?);
    println!("ou saisissez le secret : {secret}");
    println!("(lien : {lien})\n");

    let code = lire("Code à 6 chiffres affiché par l'application : ", false)?;
    let instant = maintenant();
    let Some(pas) = totp::verifier(&secret, &code, instant, 0) else {
        bail!("code refusé : vérifiez l'heure du téléphone, puis recommencez");
    };

    let empreinte = comptes::hacher(&mot_de_passe)?;
    if !base.creer_compte(nom, &empreinte, &secret, instant)? {
        bail!("le compte « {nom} » existe déjà");
    }
    base.noter_pas_totp(nom, pas)?;
    base.noter(instant, None, "console", "compte_cree", nom)?;
    println!("compte « {nom} » créé");
    Ok(())
}

/// QR code en caractères (deux modules par caractère en hauteur), lisible dans un terminal.
fn qr_code(texte: &str) -> anyhow::Result<String> {
    let code = qrcode::QrCode::new(texte.as_bytes()).context("QR code")?;
    let largeur = code.width();
    let sombre = |x: isize, y: isize| {
        x >= 0
            && y >= 0
            && (x as usize) < largeur
            && (y as usize) < largeur
            && code[(x as usize, y as usize)] == qrcode::Color::Dark
    };
    // Marge de 2 modules ; fond clair, modules sombres, pour tout thème de terminal.
    let mut sortie = String::new();
    let marge = 2isize;
    let fin = largeur as isize + marge;
    let mut y = -marge;
    while y < fin {
        for x in -marge..fin {
            sortie.push(match (sombre(x, y), sombre(x, y + 1)) {
                (false, false) => '█',
                (true, false) => '▄',
                (false, true) => '▀',
                (true, true) => ' ',
            });
        }
        sortie.push('\n');
        y += 2;
    }
    Ok(sortie)
}

/// Date `AAAA-MM-JJ HH:MM` en UTC d'un instant Unix (algorithme « civil from days »).
fn date_utc(instant: u64) -> String {
    let jours = (instant / 86_400) as i64;
    let secondes = instant % 86_400;
    let z = jours + 719_468;
    let ere = z.div_euclid(146_097);
    let jour_ere = z.rem_euclid(146_097);
    let annee_ere = (jour_ere - jour_ere / 1460 + jour_ere / 36_524 - jour_ere / 146_096) / 365;
    let jour_annee = jour_ere - (365 * annee_ere + annee_ere / 4 - annee_ere / 100);
    let m = (5 * jour_annee + 2) / 153;
    let jour = jour_annee - (153 * m + 2) / 5 + 1;
    let mois = if m < 10 { m + 3 } else { m - 9 };
    let annee = annee_ere + ere * 400 + i64::from(mois <= 2);
    format!(
        "{annee:04}-{mois:02}-{jour:02} {:02}:{:02}",
        secondes / 3600,
        secondes % 3600 / 60
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn date_utc() {
        assert_eq!(super::date_utc(0), "1970-01-01 00:00");
        assert_eq!(super::date_utc(951_782_400), "2000-02-29 00:00");
        assert_eq!(super::date_utc(1_794_825_395), "2026-11-16 10:36");
    }
}
