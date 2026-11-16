use std::path::PathBuf;

use clap::{Parser, Subcommand};
use ricochet_serveur::{Config, jeton};
use tracing_subscriber::EnvFilter;

/// Relais Ricochet : reçoit les connexions des agents et sert l'API de contrôle.
#[derive(Parser)]
#[command(version)]
struct Arguments {
    #[command(subcommand)]
    commande: Option<Commande>,
}

#[derive(Subcommand)]
enum Commande {
    /// Lance le relais (commande par défaut).
    Servir {
        /// Fichier de configuration.
        #[arg(long, short, default_value = "/etc/ricochet/serveur.toml")]
        config: PathBuf,
    },
    /// Crée un jeton : le jeton va dans l'agent (ou chez l'opérateur), l'empreinte dans la
    /// configuration du relais.
    Jeton,
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
        commande => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
                )
                .init();
            let chemin = match commande {
                Some(Commande::Servir { config }) => config,
                _ => PathBuf::from("/etc/ricochet/serveur.toml"),
            };
            ricochet_serveur::servir(Config::lire(&chemin)?).await
        }
    }
}
