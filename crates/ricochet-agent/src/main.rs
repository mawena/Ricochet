use std::path::PathBuf;

use clap::{Parser, Subcommand};
use ricochet_agent::{
    Config, systeme,
    tunnel_client::{self, ConfigClient},
};
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
    /// Relie l'entrée et la sortie standard à un port d'une machine, par le relais. Pour SSH :
    /// `ssh -o ProxyCommand="ricochet-agent tunnel %h" utilisateur@machine`.
    Tunnel {
        /// Identifiant de la machine dans Ricochet.
        machine: String,
        /// Port de la machine à joindre (il doit être autorisé dans son `[tunnel]`).
        #[arg(long, short, default_value_t = 22)]
        port: u16,
        /// Fichier du client (relais et jeton) ; par défaut `~/.config/ricochet/client.toml`.
        #[arg(long, short)]
        config: Option<PathBuf>,
    },
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
                "info": systeme::info_machine(false, false, Vec::new()),
                "etat": collecteur.etat(),
            });
            println!("{}", serde_json::to_string_pretty(&sortie)?);
            Ok(())
        }
        Some(Commande::Tunnel {
            machine,
            port,
            config,
        }) => {
            let chemin = config.unwrap_or_else(ConfigClient::chemin_par_defaut);
            let resultat = match ConfigClient::lire(&chemin) {
                Ok(config) => tunnel_client::executer(&config, &machine, port).await,
                Err(erreur) => Err(erreur),
            };
            // Sortie directe : une lecture de l'entrée standard encore en cours bloquerait
            // l'arrêt normal du programme.
            std::process::exit(match resultat {
                Ok(raison) if raison.is_empty() => 0,
                Ok(raison) => {
                    eprintln!("ricochet : tunnel fermé ({raison})");
                    1
                }
                Err(erreur) => {
                    eprintln!("ricochet : {erreur:#}");
                    1
                }
            })
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
