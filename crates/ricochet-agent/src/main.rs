use std::path::PathBuf;

use clap::{Parser, Subcommand};
use ricochet_agent::{Config, systeme};
use tracing_subscriber::EnvFilter;

/// Agent Ricochet : relie cette machine au relais par une connexion sortante.
#[derive(Parser)]
#[command(version)]
struct Arguments {
    #[command(subcommand)]
    commande: Option<Commande>,
}

#[derive(Subcommand)]
enum Commande {
    /// Se connecte au relais et y reste (commande par défaut).
    Executer {
        /// Fichier de configuration.
        #[arg(long, short, default_value = "/etc/ricochet/agent.toml")]
        config: PathBuf,
    },
    /// Affiche l'état de cette machine tel que l'agent l'envoie, en JSON.
    Etat,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    match Arguments::parse().commande {
        Some(Commande::Etat) => {
            let mut collecteur = systeme::Collecteur::new()?;
            // Deux mesures espacées : la première ne connaît pas encore l'usage du processeur.
            collecteur.etat();
            tokio::time::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL).await;
            let sortie = serde_json::json!({
                "info": systeme::info_machine(false, false),
                "etat": collecteur.etat(),
            });
            println!("{}", serde_json::to_string_pretty(&sortie)?);
            Ok(())
        }
        commande => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
                )
                .init();
            let chemin = match commande {
                Some(Commande::Executer { config }) => config,
                _ => PathBuf::from("/etc/ricochet/agent.toml"),
            };
            match ricochet_agent::executer(Config::lire(&chemin)?).await? {}
        }
    }
}
