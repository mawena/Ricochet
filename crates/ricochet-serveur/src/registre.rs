//! Registre des machines : qui est connecté, depuis quand, et son dernier état connu.

use std::{
    collections::HashMap,
    sync::{
        Mutex,
        atomic::{AtomicU32, AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use ricochet_protocole::{EtatMachine, InfoMachine, NumeroTerminal, RelaisVersAgent};
use serde::Serialize;
use tokio::sync::mpsc;

use crate::base::MachineDeclaree;

/// Ordre transmis à la tâche qui tient la connexion d'un agent.
#[derive(Debug)]
pub enum Commande {
    /// Envoyer ce message à l'agent.
    Envoyer(RelaisVersAgent),
    /// Une nouvelle connexion du même agent remplace celle-ci : la fermer.
    Remplacer,
}

/// Nombre de terminaux ouverts en même temps sur une machine, côté relais.
pub const TERMINAUX_MAX: usize = 8;

/// Ce que l'agent renvoie pour un terminal, transmis au navigateur de l'opérateur.
#[derive(Debug, PartialEq, Eq)]
pub enum EvenementTerminal {
    Sortie(Vec<u8>),
    Ferme(String),
}

#[derive(Default)]
pub struct Registre {
    machines: Mutex<HashMap<String, Machine>>,
    compteur: AtomicU64,
    terminaux: Mutex<HashMap<NumeroTerminal, TerminalOuvert>>,
    compteur_terminaux: AtomicU32,
}

/// Un terminal appartient à une session d'agent : seule celle-ci peut y écrire.
struct TerminalOuvert {
    machine: String,
    session: u64,
    sortie: mpsc::Sender<EvenementTerminal>,
}

#[derive(Default)]
struct Machine {
    info: Option<InfoMachine>,
    etat: Option<EtatMachine>,
    etat_recu_le: Option<u64>,
    vue_le: Option<u64>,
    session: Option<Session>,
}

struct Session {
    numero: u64,
    connectee_le: u64,
    adresse: String,
    commandes: mpsc::Sender<Commande>,
}

/// Une machine telle que l'API la montre.
#[derive(Debug, Clone, Serialize)]
pub struct VueMachine {
    pub id: String,
    pub nom: Option<String>,
    pub connectee: bool,
    /// Instants en secondes depuis l'époque Unix.
    pub connectee_le: Option<u64>,
    pub vue_le: Option<u64>,
    pub adresse: Option<String>,
    pub info: Option<InfoMachine>,
    pub etat: Option<EtatMachine>,
    pub etat_recu_le: Option<u64>,
}

/// Erreur d'envoi d'une commande à une machine.
#[derive(Debug, PartialEq, Eq)]
pub enum ErreurEnvoi {
    Deconnectee,
    Saturee,
    /// L'agent n'accepte pas les terminaux.
    SansTerminal,
    /// Trop de terminaux ouverts sur cette machine.
    TropDeTerminaux,
}

pub fn maintenant() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Registre {
    /// Ouvre la session d'un agent ; une session précédente du même agent est fermée.
    /// Renvoie le numéro de la session et la file de ses commandes.
    pub fn ouvrir_session(
        &self,
        id: &str,
        info: InfoMachine,
        adresse: String,
    ) -> (u64, mpsc::Receiver<Commande>) {
        let numero = self.compteur.fetch_add(1, Ordering::Relaxed) + 1;
        let (envoi, reception) = mpsc::channel(32);
        let mut machines = self.machines.lock().unwrap();
        let machine = machines.entry(id.to_owned()).or_default();
        if let Some(ancienne) = machine.session.take() {
            let _ = ancienne.commandes.try_send(Commande::Remplacer);
        }
        let instant = maintenant();
        machine.info = Some(info);
        machine.vue_le = Some(instant);
        machine.session = Some(Session {
            numero,
            connectee_le: instant,
            adresse,
            commandes: envoi,
        });
        (numero, reception)
    }

    /// Note qu'un message vient d'arriver de la session.
    pub fn toucher(&self, id: &str, numero: u64) {
        self.modifier(id, numero, |machine| machine.vue_le = Some(maintenant()));
    }

    pub fn mettre_a_jour_etat(&self, id: &str, numero: u64, etat: EtatMachine) {
        self.modifier(id, numero, |machine| {
            machine.etat = Some(etat);
            machine.etat_recu_le = Some(maintenant());
        });
    }

    /// Ferme la session, si c'est toujours la session courante de l'agent, et les terminaux
    /// qu'elle portait.
    pub fn fermer_session(&self, id: &str, numero: u64) {
        self.modifier(id, numero, |machine| machine.session = None);
        self.terminaux
            .lock()
            .unwrap()
            .retain(|_, t| !(t.machine == id && t.session == numero));
    }

    fn modifier(&self, id: &str, numero: u64, action: impl FnOnce(&mut Machine)) {
        let mut machines = self.machines.lock().unwrap();
        if let Some(machine) = machines.get_mut(id)
            && machine.session.as_ref().is_some_and(|s| s.numero == numero)
        {
            action(machine);
        }
    }

    /// Envoie une commande à la session courante d'un agent.
    pub fn envoyer(&self, id: &str, commande: Commande) -> Result<(), ErreurEnvoi> {
        let machines = self.machines.lock().unwrap();
        let session = machines
            .get(id)
            .and_then(|m| m.session.as_ref())
            .ok_or(ErreurEnvoi::Deconnectee)?;
        session.commandes.try_send(commande).map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => ErreurEnvoi::Saturee,
            mpsc::error::TrySendError::Closed(_) => ErreurEnvoi::Deconnectee,
        })
    }

    /// File des commandes de la session courante, pour un envoi qui attend s'il le faut.
    pub fn file(&self, id: &str) -> Option<mpsc::Sender<Commande>> {
        let machines = self.machines.lock().unwrap();
        Some(machines.get(id)?.session.as_ref()?.commandes.clone())
    }

    /// Oublie une machine révoquée et ferme sa connexion et ses terminaux.
    pub fn retirer(&self, id: &str) {
        let machine = self.machines.lock().unwrap().remove(id);
        if let Some(session) = machine.and_then(|m| m.session) {
            let _ = session.commandes.try_send(Commande::Remplacer);
        }
        self.terminaux
            .lock()
            .unwrap()
            .retain(|_, t| t.machine != id);
    }

    /// Ouvre un terminal sur la machine : l'ordre part à l'agent, et sa sortie arrivera dans
    /// la file renvoyée.
    pub fn ouvrir_terminal(
        &self,
        id: &str,
        colonnes: u16,
        lignes: u16,
    ) -> Result<(NumeroTerminal, mpsc::Receiver<EvenementTerminal>), ErreurEnvoi> {
        let machines = self.machines.lock().unwrap();
        let machine = machines.get(id).ok_or(ErreurEnvoi::Deconnectee)?;
        let session = machine.session.as_ref().ok_or(ErreurEnvoi::Deconnectee)?;
        if !machine.info.as_ref().is_some_and(|i| i.terminal) {
            return Err(ErreurEnvoi::SansTerminal);
        }
        let mut terminaux = self.terminaux.lock().unwrap();
        if terminaux.values().filter(|t| t.machine == id).count() >= TERMINAUX_MAX {
            return Err(ErreurEnvoi::TropDeTerminaux);
        }
        let terminal = self.compteur_terminaux.fetch_add(1, Ordering::Relaxed) + 1;
        session
            .commandes
            .try_send(Commande::Envoyer(RelaisVersAgent::OuvrirTerminal {
                terminal,
                colonnes,
                lignes,
            }))
            .map_err(|e| match e {
                mpsc::error::TrySendError::Full(_) => ErreurEnvoi::Saturee,
                mpsc::error::TrySendError::Closed(_) => ErreurEnvoi::Deconnectee,
            })?;
        let (envoi, reception) = mpsc::channel(64);
        terminaux.insert(
            terminal,
            TerminalOuvert {
                machine: id.to_owned(),
                session: session.numero,
                sortie: envoi,
            },
        );
        Ok((terminal, reception))
    }

    /// Où transmettre la sortie d'un terminal, si la session de l'agent en est bien la
    /// propriétaire (un agent ne peut pas écrire dans le terminal d'une autre machine).
    pub fn sortie_terminal(
        &self,
        id: &str,
        session: u64,
        terminal: NumeroTerminal,
    ) -> Option<mpsc::Sender<EvenementTerminal>> {
        let terminaux = self.terminaux.lock().unwrap();
        let ouvert = terminaux.get(&terminal)?;
        (ouvert.machine == id && ouvert.session == session).then(|| ouvert.sortie.clone())
    }

    /// L'agent signale la fin d'un terminal : prévenir le navigateur.
    pub fn terminal_ferme(&self, id: &str, session: u64, terminal: NumeroTerminal, raison: String) {
        let mut terminaux = self.terminaux.lock().unwrap();
        if terminaux
            .get(&terminal)
            .is_some_and(|t| t.machine == id && t.session == session)
            && let Some(ouvert) = terminaux.remove(&terminal)
        {
            let _ = ouvert.sortie.try_send(EvenementTerminal::Ferme(raison));
        }
    }

    /// Oublie un terminal ; renvoie sa machine s'il était encore ouvert (l'agent est alors à
    /// prévenir).
    pub fn fermer_terminal(&self, terminal: NumeroTerminal) -> Option<String> {
        let ouvert = self.terminaux.lock().unwrap().remove(&terminal)?;
        Some(ouvert.machine)
    }

    /// Les machines déclarées, dans l'ordre donné, connectées ou non.
    pub fn liste(&self, declarees: &[MachineDeclaree]) -> Vec<VueMachine> {
        let machines = self.machines.lock().unwrap();
        declarees
            .iter()
            .map(|agent| {
                let machine = machines.get(&agent.id);
                let session = machine.and_then(|m| m.session.as_ref());
                VueMachine {
                    id: agent.id.clone(),
                    nom: agent.nom.clone(),
                    connectee: session.is_some(),
                    connectee_le: session.map(|s| s.connectee_le),
                    vue_le: machine.and_then(|m| m.vue_le),
                    adresse: session.map(|s| s.adresse.clone()),
                    info: machine.and_then(|m| m.info.clone()),
                    etat: machine.and_then(|m| m.etat.clone()),
                    etat_recu_le: machine.and_then(|m| m.etat_recu_le),
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> InfoMachine {
        InfoMachine {
            nom_hote: "h".into(),
            systeme: "s".into(),
            noyau: "n".into(),
            architecture: "a".into(),
            version_agent: "v".into(),
            terminal: true,
        }
    }

    fn config() -> Vec<MachineDeclaree> {
        ["pc", "vps"]
            .map(|id| MachineDeclaree {
                id: id.into(),
                nom: None,
                creee_le: 0,
            })
            .to_vec()
    }

    #[test]
    fn session_puis_fermeture() {
        let registre = Registre::default();
        let config = config();
        assert!(registre.liste(&config).iter().all(|m| !m.connectee));

        let (numero, _file) = registre.ouvrir_session("pc", info(), "1.2.3.4".into());
        let liste = registre.liste(&config);
        assert!(liste[0].connectee);
        assert_eq!(liste[0].adresse.as_deref(), Some("1.2.3.4"));
        assert!(!liste[1].connectee);

        registre.fermer_session("pc", numero);
        let liste = registre.liste(&config);
        assert!(!liste[0].connectee);
        assert!(
            liste[0].info.is_some(),
            "les infos restent après la déconnexion"
        );
    }

    #[test]
    fn nouvelle_session_remplace_ancienne() {
        let registre = Registre::default();
        let (premiere, mut file) = registre.ouvrir_session("pc", info(), "a".into());
        let (_seconde, _file2) = registre.ouvrir_session("pc", info(), "b".into());
        assert!(matches!(file.try_recv(), Ok(Commande::Remplacer)));

        // L'ancienne session ne doit plus toucher à l'état de la machine.
        registre.fermer_session("pc", premiere);
        assert!(registre.liste(&config())[0].connectee);
    }

    #[test]
    fn retrait_ferme_la_connexion() {
        let registre = Registre::default();
        let (_n, mut file) = registre.ouvrir_session("pc", info(), "a".into());
        registre.retirer("pc");
        assert!(matches!(file.try_recv(), Ok(Commande::Remplacer)));
        assert!(!registre.liste(&config())[0].connectee);
        assert!(registre.liste(&config())[0].info.is_none());
    }

    #[test]
    fn terminaux() {
        let registre = Registre::default();
        assert_eq!(
            registre.ouvrir_terminal("pc", 80, 24).unwrap_err(),
            ErreurEnvoi::Deconnectee
        );
        let (session, mut file) = registre.ouvrir_session("pc", info(), "a".into());
        let (terminal, mut sortie) = registre.ouvrir_terminal("pc", 80, 24).unwrap();
        assert!(matches!(
            file.try_recv(),
            Ok(Commande::Envoyer(RelaisVersAgent::OuvrirTerminal {
                colonnes: 80,
                lignes: 24,
                ..
            }))
        ));

        // Seule la session de l'agent qui porte le terminal peut y écrire.
        let (autre, _f) = registre.ouvrir_session("vps", info(), "b".into());
        assert!(registre.sortie_terminal("vps", autre, terminal).is_none());
        assert!(
            registre
                .sortie_terminal("pc", session + 99, terminal)
                .is_none()
        );
        let envoi = registre.sortie_terminal("pc", session, terminal).unwrap();
        envoi
            .try_send(EvenementTerminal::Sortie(b"x".to_vec()))
            .unwrap();
        assert_eq!(
            sortie.try_recv(),
            Ok(EvenementTerminal::Sortie(b"x".to_vec()))
        );
        drop(envoi);

        registre.terminal_ferme("vps", autre, terminal, "non".into());
        assert!(
            sortie.try_recv().is_err(),
            "une autre machine ne ferme rien"
        );
        registre.terminal_ferme("pc", session, terminal, "fin".into());
        assert_eq!(
            sortie.try_recv(),
            Ok(EvenementTerminal::Ferme("fin".into()))
        );
        assert_eq!(registre.fermer_terminal(terminal), None);

        // La fin de la session de l'agent ferme ses terminaux.
        let (t2, mut sortie2) = registre.ouvrir_terminal("pc", 80, 24).unwrap();
        registre.fermer_session("pc", session);
        assert!(sortie2.try_recv().is_err());
        assert_eq!(registre.fermer_terminal(t2), None);
    }

    #[test]
    fn terminaux_limites() {
        let registre = Registre::default();
        let mut sans = info();
        sans.terminal = false;
        let (_s, _f) = registre.ouvrir_session("pc", sans, "a".into());
        assert_eq!(
            registre.ouvrir_terminal("pc", 80, 24).unwrap_err(),
            ErreurEnvoi::SansTerminal
        );
        let (_s, mut file) = registre.ouvrir_session("vps", info(), "b".into());
        let mut ouverts = Vec::new();
        for _ in 0..TERMINAUX_MAX {
            ouverts.push(registre.ouvrir_terminal("vps", 80, 24).unwrap());
            file.try_recv().unwrap();
        }
        assert_eq!(
            registre.ouvrir_terminal("vps", 80, 24).unwrap_err(),
            ErreurEnvoi::TropDeTerminaux
        );
        registre.retirer("vps");
        assert_eq!(registre.fermer_terminal(ouverts[0].0), None);
    }

    #[test]
    fn envoi_a_une_machine() {
        let registre = Registre::default();
        assert_eq!(
            registre.envoyer("pc", Commande::Envoyer(RelaisVersAgent::DemandeEtat)),
            Err(ErreurEnvoi::Deconnectee)
        );
        let (_n, mut file) = registre.ouvrir_session("pc", info(), "a".into());
        registre
            .envoyer("pc", Commande::Envoyer(RelaisVersAgent::DemandeEtat))
            .unwrap();
        assert!(matches!(
            file.try_recv(),
            Ok(Commande::Envoyer(RelaisVersAgent::DemandeEtat))
        ));
    }
}
