//! Registre des machines : qui est connecté, depuis quand, et son dernier état connu.

use std::{
    collections::HashMap,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use ricochet_protocole::{EtatMachine, InfoMachine, RelaisVersAgent};
use serde::Serialize;
use tokio::sync::mpsc;

use crate::config::Config;

/// Ordre transmis à la tâche qui tient la connexion d'un agent.
#[derive(Debug)]
pub enum Commande {
    /// Envoyer ce message à l'agent.
    Envoyer(RelaisVersAgent),
    /// Une nouvelle connexion du même agent remplace celle-ci : la fermer.
    Remplacer,
}

#[derive(Default)]
pub struct Registre {
    machines: Mutex<HashMap<String, Machine>>,
    compteur: AtomicU64,
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

    /// Ferme la session, si c'est toujours la session courante de l'agent.
    pub fn fermer_session(&self, id: &str, numero: u64) {
        self.modifier(id, numero, |machine| machine.session = None);
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

    /// Toutes les machines de la configuration, dans son ordre, connectées ou non.
    pub fn liste(&self, config: &Config) -> Vec<VueMachine> {
        let machines = self.machines.lock().unwrap();
        config
            .agents
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
        }
    }

    fn config() -> Config {
        let h = "0".repeat(64);
        Config::depuis_texte(&format!(
            "jeton_operateur_sha256 = \"{h}\"\n[[agents]]\nid = \"pc\"\njeton_sha256 = \"{h}\"\n[[agents]]\nid = \"vps\"\njeton_sha256 = \"{h}\"\n"
        ))
        .unwrap()
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
