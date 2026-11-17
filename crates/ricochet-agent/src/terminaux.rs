//! Terminaux à distance : chaque terminal est un shell dans un pseudo-terminal (PTY).
//!
//! Les lectures et écritures du PTY sont bloquantes : chaque terminal a un fil pour lire ce que
//! le shell affiche et un autre pour lui transmettre ce que l'opérateur tape. Ce qui est lu
//! remonte par une file bornée : si le relais n'avance pas, le shell attend.

use std::{
    collections::HashMap,
    io::{Read, Write},
    sync::mpsc as std_mpsc,
};

use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use ricochet_protocole::NumeroTerminal;
use tokio::sync::mpsc;

use crate::config::ConfigTerminal;

/// Taille d'une lecture du PTY.
const TAILLE_LECTURE: usize = 16 * 1024;
/// Bornes de la taille demandée, pour ne pas passer n'importe quoi au noyau.
const COTE_MAX: u16 = 1000;

/// Ce que les terminaux ont à dire au relais.
#[derive(Debug, PartialEq, Eq)]
pub enum Evenement {
    Sortie(NumeroTerminal, Vec<u8>),
    Ferme(NumeroTerminal, String),
}

struct Terminal {
    maitre: Box<dyn MasterPty + Send>,
    entree: std_mpsc::Sender<Vec<u8>>,
    tueur: Box<dyn ChildKiller + Send + Sync>,
}

/// Les terminaux ouverts pendant une session avec le relais ; tous sont fermés avec elle.
pub struct Terminaux {
    config: Option<ConfigTerminal>,
    ouverts: HashMap<NumeroTerminal, Terminal>,
    evenements: mpsc::Sender<Evenement>,
}

fn taille(colonnes: u16, lignes: u16) -> PtySize {
    PtySize {
        rows: lignes.clamp(1, COTE_MAX),
        cols: colonnes.clamp(1, COTE_MAX),
        pixel_width: 0,
        pixel_height: 0,
    }
}

impl Terminaux {
    pub fn new(config: Option<ConfigTerminal>) -> (Self, mpsc::Receiver<Evenement>) {
        let (evenements, reception) = mpsc::channel(64);
        let terminaux = Self {
            config,
            ouverts: HashMap::new(),
            evenements,
        };
        (terminaux, reception)
    }

    /// Ouvre un terminal ; en cas d'échec, la raison est à renvoyer au relais.
    pub fn ouvrir(
        &mut self,
        numero: NumeroTerminal,
        colonnes: u16,
        lignes: u16,
    ) -> Result<(), String> {
        let Some(config) = &self.config else {
            return Err("terminal désactivé sur cette machine".into());
        };
        if self.ouverts.contains_key(&numero) {
            return Err("terminal déjà ouvert".into());
        }
        if self.ouverts.len() >= config.maximum {
            return Err(format!("{} terminaux déjà ouverts", config.maximum));
        }
        let paire = native_pty_system()
            .openpty(taille(colonnes, lignes))
            .map_err(|e| format!("ouverture du pseudo-terminal : {e}"))?;
        let mut commande = CommandBuilder::new(&config.commande[0]);
        commande.args(&config.commande[1..]);
        commande.env("TERM", "xterm-256color");
        commande.env("COLORTERM", "truecolor");
        let mut enfant = paire
            .slave
            .spawn_command(commande)
            .map_err(|e| format!("lancement de {} : {e}", config.commande[0]))?;
        // Le shell garde seul le côté esclave : sa fin fermera le PTY et terminera la lecture.
        drop(paire.slave);
        let tueur = enfant.clone_killer();
        let mut lecteur = paire
            .master
            .try_clone_reader()
            .map_err(|e| format!("lecture du pseudo-terminal : {e}"))?;
        let mut ecrivain = paire
            .master
            .take_writer()
            .map_err(|e| format!("écriture du pseudo-terminal : {e}"))?;

        let evenements = self.evenements.clone();
        std::thread::spawn(move || {
            let mut tampon = vec![0; TAILLE_LECTURE];
            loop {
                match lecteur.read(&mut tampon) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let sortie = Evenement::Sortie(numero, tampon[..n].to_vec());
                        if evenements.blocking_send(sortie).is_err() {
                            break;
                        }
                    }
                }
            }
            let raison = match enfant.wait() {
                Ok(statut) if statut.success() => "fin du shell".to_owned(),
                Ok(statut) => format!("fin du shell (code {})", statut.exit_code()),
                Err(e) => format!("fin du shell ({e})"),
            };
            let _ = evenements.blocking_send(Evenement::Ferme(numero, raison));
        });

        let (entree, file) = std_mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            for donnees in file {
                if ecrivain
                    .write_all(&donnees)
                    .and_then(|_| ecrivain.flush())
                    .is_err()
                {
                    break;
                }
            }
        });

        self.ouverts.insert(
            numero,
            Terminal {
                maitre: paire.master,
                entree,
                tueur,
            },
        );
        Ok(())
    }

    pub fn ecrire(&mut self, numero: NumeroTerminal, donnees: Vec<u8>) {
        if let Some(terminal) = self.ouverts.get(&numero) {
            let _ = terminal.entree.send(donnees);
        }
    }

    pub fn redimensionner(&mut self, numero: NumeroTerminal, colonnes: u16, lignes: u16) {
        if let Some(terminal) = self.ouverts.get(&numero)
            && let Err(erreur) = terminal.maitre.resize(taille(colonnes, lignes))
        {
            tracing::warn!("terminal {numero} : redimensionnement impossible ({erreur})");
        }
    }

    /// Arrête le shell à la demande du relais.
    pub fn fermer(&mut self, numero: NumeroTerminal) {
        if let Some(mut terminal) = self.ouverts.remove(&numero) {
            let _ = terminal.tueur.kill();
        }
    }

    /// Oublie un terminal dont le shell s'est terminé.
    pub fn oublier(&mut self, numero: NumeroTerminal) {
        self.ouverts.remove(&numero);
    }

    /// La file des événements, que les tunnels partagent avec les terminaux.
    pub fn evenements(&self) -> mpsc::Sender<Evenement> {
        self.evenements.clone()
    }

    pub fn nombre(&self) -> usize {
        self.ouverts.len()
    }
}

impl Drop for Terminaux {
    fn drop(&mut self) {
        for (_, mut terminal) in self.ouverts.drain() {
            let _ = terminal.tueur.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn config(commande: &[&str]) -> Option<ConfigTerminal> {
        Some(ConfigTerminal {
            commande: commande.iter().map(|s| s.to_string()).collect(),
            maximum: 2,
        })
    }

    /// Lit les événements jusqu'à la fermeture du terminal ; renvoie la sortie et la raison.
    async fn jusqu_a_la_fin(
        reception: &mut mpsc::Receiver<Evenement>,
        terminaux: &mut Terminaux,
    ) -> (String, String) {
        let mut sortie = Vec::new();
        loop {
            let evenement = tokio::time::timeout(Duration::from_secs(10), reception.recv())
                .await
                .expect("le terminal doit répondre")
                .unwrap();
            match evenement {
                Evenement::Sortie(_, donnees) => sortie.extend(donnees),
                Evenement::Ferme(numero, raison) => {
                    terminaux.oublier(numero);
                    return (String::from_utf8_lossy(&sortie).into_owned(), raison);
                }
            }
        }
    }

    #[tokio::test]
    async fn shell_interactif() {
        let (mut terminaux, mut reception) = Terminaux::new(config(&["/bin/sh"]));
        terminaux.ouvrir(1, 80, 24).unwrap();
        terminaux.ecrire(1, b"stty size; echo fin-$((6*7)); exit 3\n".to_vec());
        let (sortie, raison) = jusqu_a_la_fin(&mut reception, &mut terminaux).await;
        assert!(sortie.contains("24 80"), "{sortie}");
        assert!(sortie.contains("fin-42"), "{sortie}");
        assert_eq!(raison, "fin du shell (code 3)");
        assert_eq!(terminaux.nombre(), 0);
    }

    #[tokio::test]
    async fn redimensionnement() {
        let (mut terminaux, mut reception) = Terminaux::new(config(&["/bin/sh"]));
        terminaux.ouvrir(1, 80, 24).unwrap();
        terminaux.redimensionner(1, 132, 50);
        terminaux.ecrire(1, b"stty size; exit\n".to_vec());
        let (sortie, _) = jusqu_a_la_fin(&mut reception, &mut terminaux).await;
        assert!(sortie.contains("50 132"), "{sortie}");
    }

    #[tokio::test]
    async fn fermeture_par_le_relais() {
        let (mut terminaux, mut reception) = Terminaux::new(config(&["/bin/sh"]));
        terminaux.ouvrir(7, 80, 24).unwrap();
        terminaux.fermer(7);
        let (_, raison) = jusqu_a_la_fin(&mut reception, &mut terminaux).await;
        assert!(raison.starts_with("fin du shell"), "{raison}");
    }

    #[test]
    fn limites() {
        let (mut sans, _r) = Terminaux::new(None);
        assert_eq!(
            sans.ouvrir(1, 80, 24).unwrap_err(),
            "terminal désactivé sur cette machine"
        );

        let (mut terminaux, _r) = Terminaux::new(config(&["/bin/sh"]));
        terminaux.ouvrir(1, 80, 24).unwrap();
        assert!(terminaux.ouvrir(1, 80, 24).is_err(), "même numéro");
        terminaux.ouvrir(2, 0, 0).unwrap();
        assert!(terminaux.ouvrir(3, 80, 24).is_err(), "maximum atteint");

        let (mut absent, _r) = Terminaux::new(config(&["/chemin/inexistant"]));
        assert!(absent.ouvrir(1, 80, 24).is_err());
    }
}
