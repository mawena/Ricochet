//! Agent de Ricochet.
//!
//! Il ouvre une connexion WebSocket sortante vers le relais, ce qui marche derrière une box ou un
//! NAT sans ouvrir de port, puis il la garde ouverte et se reconnecte seul si elle tombe.

pub mod config;
pub mod connexion;
pub mod fichiers;
pub mod systeme;
pub mod terminaux;
pub mod tunnel_client;
pub mod tunnels;

pub use config::Config;
pub use connexion::executer;
