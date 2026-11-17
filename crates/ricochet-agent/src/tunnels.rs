//! Tunnels : une connexion TCP vers un port local de la machine, prolongée jusqu'au relais.
//!
//! Un tunnel sert surtout à joindre le serveur SSH d'une machine sans IP publique. Seuls les
//! ports de la section `[tunnel]` sont joignables, et toujours sur 127.0.0.1. Les tunnels
//! partagent les numéros et la file d'événements des terminaux.

use std::collections::HashMap;

use ricochet_protocole::NumeroTerminal;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::mpsc,
    task::JoinHandle,
};

use crate::{config::ConfigTunnel, terminaux::Evenement};

/// Taille d'une lecture de la connexion locale.
const TAILLE_LECTURE: usize = 32 * 1024;

struct Tunnel {
    entree: mpsc::UnboundedSender<Vec<u8>>,
    tache: JoinHandle<()>,
}

/// Les tunnels ouverts pendant une session avec le relais ; tous sont fermés avec elle.
pub struct Tunnels {
    config: Option<ConfigTunnel>,
    ouverts: HashMap<NumeroTerminal, Tunnel>,
    evenements: mpsc::Sender<Evenement>,
}

impl Tunnels {
    pub fn new(config: Option<ConfigTunnel>, evenements: mpsc::Sender<Evenement>) -> Self {
        Self {
            config,
            ouverts: HashMap::new(),
            evenements,
        }
    }

    /// Ouvre un tunnel ; en cas d'échec, la raison est à renvoyer au relais.
    pub fn ouvrir(&mut self, numero: NumeroTerminal, port: u16) -> Result<(), String> {
        let Some(config) = &self.config else {
            return Err("tunnels désactivés sur cette machine".into());
        };
        if !config.ports.contains(&port) {
            return Err(format!("port {port} non autorisé sur cette machine"));
        }
        if self.ouverts.contains_key(&numero) {
            return Err("tunnel déjà ouvert".into());
        }
        if self.ouverts.len() >= config.maximum {
            return Err(format!("{} tunnels déjà ouverts", config.maximum));
        }
        let (entree, file) = mpsc::unbounded_channel();
        let evenements = self.evenements.clone();
        let tache = tokio::spawn(async move {
            let raison = match TcpStream::connect(("127.0.0.1", port)).await {
                Ok(flux) => relayer(flux, numero, file, &evenements).await,
                Err(erreur) => format!("connexion au port {port} : {erreur}"),
            };
            let _ = evenements.send(Evenement::Ferme(numero, raison)).await;
        });
        self.ouverts.insert(numero, Tunnel { entree, tache });
        Ok(())
    }

    /// Envoie des octets dans le tunnel ; les rend si ce numéro n'est pas un tunnel.
    pub fn ecrire(&mut self, numero: NumeroTerminal, donnees: Vec<u8>) -> Option<Vec<u8>> {
        match self.ouverts.get(&numero) {
            Some(tunnel) => {
                let _ = tunnel.entree.send(donnees);
                None
            }
            None => Some(donnees),
        }
    }

    /// Coupe le tunnel à la demande du relais.
    pub fn fermer(&mut self, numero: NumeroTerminal) {
        if let Some(tunnel) = self.ouverts.remove(&numero) {
            tunnel.tache.abort();
        }
    }

    /// Oublie un tunnel dont la connexion s'est terminée.
    pub fn oublier(&mut self, numero: NumeroTerminal) {
        self.ouverts.remove(&numero);
    }

    pub fn nombre(&self) -> usize {
        self.ouverts.len()
    }
}

impl Drop for Tunnels {
    fn drop(&mut self) {
        for (_, tunnel) in self.ouverts.drain() {
            tunnel.tache.abort();
        }
    }
}

/// Échange entre la connexion locale et le relais jusqu'à la fin ; renvoie la raison de la fin.
async fn relayer(
    flux: TcpStream,
    numero: NumeroTerminal,
    mut file: mpsc::UnboundedReceiver<Vec<u8>>,
    evenements: &mpsc::Sender<Evenement>,
) -> String {
    let (mut lecture, mut ecriture) = flux.into_split();
    let mut tampon = vec![0; TAILLE_LECTURE];
    loop {
        tokio::select! {
            lu = lecture.read(&mut tampon) => match lu {
                Ok(0) => return "connexion fermée par la machine".into(),
                Ok(n) => {
                    let sortie = Evenement::Sortie(numero, tampon[..n].to_vec());
                    if evenements.send(sortie).await.is_err() {
                        return "session terminée".into();
                    }
                }
                Err(erreur) => return format!("lecture : {erreur}"),
            },
            donnees = file.recv() => match donnees {
                Some(donnees) => {
                    if let Err(erreur) = ecriture.write_all(&donnees).await {
                        return format!("écriture : {erreur}");
                    }
                }
                None => return "tunnel fermé".into(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::net::TcpListener;

    use super::*;

    async fn suivant(reception: &mut mpsc::Receiver<Evenement>) -> Evenement {
        tokio::time::timeout(Duration::from_secs(10), reception.recv())
            .await
            .expect("le tunnel doit répondre")
            .unwrap()
    }

    /// Un service local qui renvoie ce qu'il reçoit, en majuscules, puis ferme sur « fin ».
    async fn service() -> u16 {
        let ecoute = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = ecoute.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut flux, _)) = ecoute.accept().await {
                tokio::spawn(async move {
                    let mut tampon = [0; 1024];
                    while let Ok(n) = flux.read(&mut tampon).await {
                        if n == 0 || &tampon[..n] == b"fin" {
                            break;
                        }
                        let reponse = tampon[..n].to_ascii_uppercase();
                        if flux.write_all(&reponse).await.is_err() {
                            break;
                        }
                    }
                });
            }
        });
        port
    }

    #[tokio::test]
    async fn aller_retour_puis_fermeture() {
        let port = service().await;
        let (evenements, mut reception) = mpsc::channel(8);
        let config = ConfigTunnel {
            ports: vec![port],
            maximum: 1,
        };
        let mut tunnels = Tunnels::new(Some(config), evenements);
        tunnels.ouvrir(3, port).unwrap();
        assert!(tunnels.ouvrir(4, port).is_err(), "maximum atteint");
        assert_eq!(tunnels.ecrire(9, b"x".to_vec()), Some(b"x".to_vec()));
        assert_eq!(tunnels.ecrire(3, b"bonjour".to_vec()), None);
        assert_eq!(
            suivant(&mut reception).await,
            Evenement::Sortie(3, b"BONJOUR".to_vec())
        );
        tunnels.ecrire(3, b"fin".to_vec());
        assert_eq!(
            suivant(&mut reception).await,
            Evenement::Ferme(3, "connexion fermée par la machine".into())
        );
        tunnels.oublier(3);
        assert_eq!(tunnels.nombre(), 0);
    }

    #[tokio::test]
    async fn refus() {
        let (evenements, mut reception) = mpsc::channel(8);
        let mut sans = Tunnels::new(None, evenements.clone());
        assert_eq!(
            sans.ouvrir(1, 22).unwrap_err(),
            "tunnels désactivés sur cette machine"
        );
        // Un port libre : rien n'y écoute.
        let libre = {
            let ecoute = TcpListener::bind("127.0.0.1:0").await.unwrap();
            ecoute.local_addr().unwrap().port()
        };
        let config = ConfigTunnel {
            ports: vec![libre],
            maximum: 4,
        };
        let mut tunnels = Tunnels::new(Some(config), evenements);
        assert_eq!(
            tunnels.ouvrir(1, 80).unwrap_err(),
            "port 80 non autorisé sur cette machine"
        );
        tunnels.ouvrir(2, libre).unwrap();
        assert!(tunnels.ouvrir(2, libre).is_err(), "même numéro");
        match suivant(&mut reception).await {
            Evenement::Ferme(2, raison) => {
                assert!(raison.starts_with("connexion au port"), "{raison}")
            }
            autre => panic!("{autre:?}"),
        }
    }
}
