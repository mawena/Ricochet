//! Messages des programmes de la machine vers l'opérateur.
//!
//! Un programme local (CofiTicket, CCS…) écrit une requête JSON sur un socket Unix ; l'agent
//! la valide, la garde sur disque puis la transmet au relais, qui la pousse en notification sur
//! les appareils de l'opérateur. Le détail (texte libre, parfois des données clients) reste sur
//! la machine : le relais ne le demande que lorsque l'opérateur l'ouvre. Les réponses aux
//! boutons reviennent par le relais et attendent ici que le programme les lise.
//!
//! La file d'attente est sur disque (dossier 0700, fichiers 0600) : un redémarrage de l'agent
//! ou une coupure du relais ne perd rien. Un message est renvoyé à chaque connexion tant que le
//! relais ne l'a pas accepté ; tout est purgé après `conservation_jours`.

use std::{
    collections::{HashMap, HashSet},
    ffi::CString,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use ricochet_protocole::{BoutonMessage, GenreMessage, MessageMachine};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::{Notify, Semaphore},
};

use crate::config::ConfigMessages;

/// Taille maximale d'une requête (une ligne JSON).
pub const REQUETE_MAX: usize = 96 * 1024;
/// Taille maximale du détail d'un message.
pub const DETAIL_MAX: usize = 64 * 1024;
/// Messages gardés au plus sur la machine (la file ne doit pas remplir le disque).
const MESSAGES_MAX: usize = 1000;
/// Temps laissé à un client pour envoyer sa requête.
const DELAI_LECTURE: Duration = Duration::from_secs(10);
/// Connexions traitées en même temps.
const CONNEXIONS_MAX: usize = 32;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ReponseLocale {
    bouton: String,
    par: String,
    quand: u64,
}

/// Ce que l'agent garde d'un message : un fichier JSON par message.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Enregistrement {
    message: MessageMachine,
    cle: Option<String>,
    /// Le relais a accepté le message : inutile de le renvoyer.
    accepte: bool,
    reponse: Option<ReponseLocale>,
    /// Le programme a lu la réponse.
    reponse_lue: bool,
}

/// La file d'attente persistante et ce qui en dépend.
pub struct Messagerie {
    dossier: PathBuf,
    conservation_s: u64,
    utilisateurs: Vec<String>,
    etat: Mutex<HashMap<String, Enregistrement>>,
    connecte: AtomicBool,
    reveil: Notify,
}

/// Une demande d'envoi, telle que le programme la donne.
#[derive(Debug, Default, Deserialize)]
pub struct Requete {
    #[serde(default)]
    pub op: String,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub cle: Option<String>,
    #[serde(default)]
    pub titre: Option<String>,
    #[serde(default)]
    pub texte: Option<String>,
    #[serde(default)]
    pub genre: Option<String>,
    #[serde(default)]
    pub boutons: Option<Vec<BoutonMessage>>,
    #[serde(default)]
    pub detail: Option<String>,
}

pub fn maintenant() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Date au format `2026-11-21T10:00:00Z`.
pub fn iso_utc(instant: u64) -> String {
    let jours = (instant / 86400) as i64;
    let reste = instant % 86400;
    // Jours depuis 1970 vers année, mois, jour (calendrier grégorien).
    let z = jours + 719_468;
    let ere = z.div_euclid(146_097);
    let jour_ere = z.rem_euclid(146_097);
    let annee_ere = (jour_ere - jour_ere / 1460 + jour_ere / 36_524 - jour_ere / 146_096) / 365;
    let jour_annee = jour_ere - (365 * annee_ere + annee_ere / 4 - annee_ere / 100);
    let mp = (5 * jour_annee + 2) / 153;
    let jour = jour_annee - (153 * mp + 2) / 5 + 1;
    let mois = if mp < 10 { mp + 3 } else { mp - 9 };
    let annee = annee_ere + ere * 400 + i64::from(mois <= 2);
    format!(
        "{annee:04}-{mois:02}-{jour:02}T{:02}:{:02}:{:02}Z",
        reste / 3600,
        reste % 3600 / 60,
        reste % 60
    )
}

fn source_valide(source: &str) -> bool {
    (1..=32).contains(&source.len())
        && source
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

fn identifiant_bouton_valide(id: &str) -> bool {
    (1..=32).contains(&id.len())
        && id
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_' || c == b'-')
}

/// Identifiant d'un message : 128 bits tirés de /dev/urandom, en hexadécimal.
fn nouvel_id() -> io::Result<String> {
    let mut octets = [0u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut octets)?;
    Ok(octets.iter().map(|o| format!("{o:02x}")).collect())
}

/// Un identifiant de message tel que l'agent les crée : 32 caractères hexadécimaux. Les noms
/// de fichiers en dérivent : rien d'autre n'est accepté.
pub fn id_valide(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|c| c.is_ascii_hexdigit())
}

fn creer_dossier_prive(chemin: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(chemin)
}

/// Écrit un fichier de droits 0600 : d'abord à côté, puis renommé, pour ne jamais laisser un
/// fichier à moitié écrit.
fn ecrire_prive(chemin: &Path, contenu: &[u8]) -> io::Result<()> {
    let mut temporaire = chemin.as_os_str().to_owned();
    temporaire.push(".tmp");
    let temporaire = PathBuf::from(temporaire);
    let mut fichier = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporaire)?;
    // Un fichier laissé par une version précédente garde ses droits : les resserrer.
    fichier.set_permissions(fs::Permissions::from_mode(0o600))?;
    fichier.write_all(contenu)?;
    fichier.sync_all()?;
    fs::rename(&temporaire, chemin)
}

fn erreur(raison: &str) -> Value {
    json!({ "ok": false, "erreur": raison })
}

impl Messagerie {
    /// Ouvre la file d'attente dans le dossier d'état et relit ce qu'elle contient.
    pub fn ouvrir(config: &ConfigMessages) -> io::Result<Arc<Self>> {
        creer_dossier_prive(&config.dossier_etat.join("messages"))?;
        creer_dossier_prive(&config.dossier_etat.join("details"))?;
        // Le dossier d'état lui-même peut avoir été créé avec d'autres droits.
        fs::set_permissions(&config.dossier_etat, fs::Permissions::from_mode(0o700))?;
        let mut etat = HashMap::new();
        for entree in fs::read_dir(config.dossier_etat.join("messages"))? {
            let chemin = entree?.path();
            let nom = chemin.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if nom.ends_with(".tmp") {
                let _ = fs::remove_file(&chemin);
                continue;
            }
            let Some(id) = nom.strip_suffix(".json").filter(|i| id_valide(i)) else {
                continue;
            };
            match fs::read(&chemin).map_err(|e| e.to_string()).and_then(|o| {
                serde_json::from_slice::<Enregistrement>(&o).map_err(|e| e.to_string())
            }) {
                Ok(enregistrement) if enregistrement.message.id == id => {
                    etat.insert(id.to_owned(), enregistrement);
                }
                Ok(_) => tracing::warn!("message {nom} ignoré : identifiant différent du nom"),
                Err(raison) => tracing::warn!("message {nom} illisible ({raison})"),
            }
        }
        Ok(Arc::new(Self {
            dossier: config.dossier_etat.clone(),
            conservation_s: config.conservation_jours.saturating_mul(86400),
            utilisateurs: config.utilisateurs.clone(),
            etat: Mutex::new(etat),
            connecte: AtomicBool::new(false),
            reveil: Notify::new(),
        }))
    }

    fn fichier(&self, id: &str) -> PathBuf {
        self.dossier.join("messages").join(format!("{id}.json"))
    }

    fn fichier_detail(&self, id: &str) -> PathBuf {
        self.dossier.join("details").join(format!("{id}.txt"))
    }

    fn verrou(&self) -> std::sync::MutexGuard<'_, HashMap<String, Enregistrement>> {
        self.etat.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn enregistrer(&self, enregistrement: &Enregistrement) -> io::Result<()> {
        let octets = serde_json::to_vec(enregistrement).map_err(io::Error::other)?;
        ecrire_prive(&self.fichier(&enregistrement.message.id), &octets)
    }

    /// Dit si la session avec le relais est ouverte (pour `etat`).
    pub fn relais_connecte(&self, connecte: bool) {
        self.connecte.store(connecte, Ordering::Relaxed);
    }

    /// Attend qu'il y ait quelque chose de nouveau à envoyer au relais.
    pub async fn attendre(&self) {
        self.reveil.notified().await;
    }

    /// Messages à envoyer au relais : ceux qu'il n'a pas encore acceptés et que cette session
    /// n'a pas déjà envoyés (`deja` en garde la liste).
    pub fn a_envoyer(&self, deja: &mut HashSet<String>) -> Vec<MessageMachine> {
        let mut liste: Vec<MessageMachine> = self
            .verrou()
            .values()
            .filter(|e| !e.accepte && !deja.contains(&e.message.id))
            .map(|e| e.message.clone())
            .collect();
        liste.sort_by(|a, b| (a.date, &a.id).cmp(&(b.date, &b.id)));
        deja.extend(liste.iter().map(|m| m.id.clone()));
        liste
    }

    /// Le relais a enregistré le message.
    pub fn accepter(&self, id: &str) {
        let mut etat = self.verrou();
        if let Some(enregistrement) = etat.get_mut(id)
            && !enregistrement.accepte
        {
            enregistrement.accepte = true;
            if let Err(erreur) = self.enregistrer(enregistrement) {
                tracing::error!("message {id} : écriture impossible ({erreur})");
            }
        }
    }

    /// Garde la réponse de l'opérateur à un bouton. Un message n'accepte qu'une réponse ; une
    /// réponse répétée ou à un message inconnu est ignorée (le relais est de toute façon
    /// prévenu qu'elle est arrivée).
    pub fn enregistrer_reponse(&self, id: &str, bouton: &str, par: &str, quand: u64) {
        let mut etat = self.verrou();
        let Some(enregistrement) = etat.get_mut(id) else {
            tracing::warn!("réponse à un message inconnu ({id}) ignorée");
            return;
        };
        if enregistrement.reponse.is_some()
            || !enregistrement
                .message
                .boutons
                .iter()
                .any(|b| b.id == bouton)
        {
            return;
        }
        enregistrement.accepte = true;
        enregistrement.reponse = Some(ReponseLocale {
            bouton: bouton.to_owned(),
            par: par.to_owned(),
            quand,
        });
        if let Err(erreur) = self.enregistrer(enregistrement) {
            tracing::error!("message {id} : écriture impossible ({erreur})");
        }
    }

    /// Détail d'un message, pour le relais.
    pub fn detail(&self, id: &str) -> Result<String, String> {
        if !id_valide(id) || !self.verrou().contains_key(id) {
            return Err("message inconnu sur la machine (purgé ?)".into());
        }
        match fs::read(self.fichier_detail(id)) {
            Ok(octets) => Ok(String::from_utf8_lossy(&octets).into_owned()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                Err("ce message n'a pas de détail".into())
            }
            Err(e) => Err(format!("lecture du détail impossible ({e})")),
        }
    }

    /// Supprime ce qui a plus de `conservation_jours` ; renvoie le nombre de messages purgés.
    pub fn purger(&self, instant: u64) -> usize {
        let mut etat = self.verrou();
        let vieux: Vec<String> = etat
            .values()
            .filter(|e| e.message.date.saturating_add(self.conservation_s) <= instant)
            .map(|e| e.message.id.clone())
            .collect();
        for id in &vieux {
            etat.remove(id);
            let _ = fs::remove_file(self.fichier(id));
            let _ = fs::remove_file(self.fichier_detail(id));
        }
        vieux.len()
    }

    /// `etat` : le relais est-il connecté, et combien de messages attendent d'être acceptés.
    pub fn etat(&self) -> Value {
        let en_attente = self.verrou().values().filter(|e| !e.accepte).count();
        json!({
            "ok": true,
            "relais_connecte": self.connecte.load(Ordering::Relaxed),
            "en_attente": en_attente,
        })
    }

    /// `reponses` : les réponses pas encore lues d'une source ; chacune n'est rendue qu'une fois.
    pub fn reponses(&self, source: &str) -> Value {
        let mut etat = self.verrou();
        let mut ids: Vec<String> = etat
            .values()
            .filter(|e| e.message.source == source && e.reponse.is_some() && !e.reponse_lue)
            .map(|e| e.message.id.clone())
            .collect();
        ids.sort();
        let mut rendues = Vec::new();
        for id in ids {
            let enregistrement = etat.get_mut(&id).expect("id tiré de la table");
            let reponse = enregistrement.reponse.clone().expect("filtré plus haut");
            enregistrement.reponse_lue = true;
            if let Err(erreur) = self.enregistrer(enregistrement) {
                // Ne pas rendre une réponse qu'on ne sait pas marquer comme lue.
                tracing::error!("message {id} : écriture impossible ({erreur})");
                enregistrement.reponse_lue = false;
                continue;
            }
            rendues.push(json!({
                "id": id,
                "bouton": reponse.bouton,
                "par": reponse.par,
                "quand": iso_utc(reponse.quand),
            }));
        }
        json!({ "ok": true, "reponses": rendues })
    }

    /// `envoyer` : valide, enregistre et programme l'envoi au relais. Renvoie l'identifiant.
    pub fn envoyer(&self, requete: Requete, instant: u64) -> Result<String, String> {
        let source = requete.source.ok_or("source manquante")?;
        if !source_valide(&source) {
            return Err("source invalide : [a-z0-9-], 1 à 32 caractères".into());
        }
        let cle = match requete.cle {
            Some(cle) if cle.is_empty() => None,
            Some(cle) if cle.chars().count() > 128 => {
                return Err("cle trop longue (128 caractères au plus)".into());
            }
            autre => autre,
        };
        let titre = requete.titre.ok_or("titre manquant")?;
        if titre.trim().is_empty() {
            return Err("titre vide".into());
        }
        if titre.chars().count() > 80 {
            return Err("titre trop long (80 caractères au plus)".into());
        }
        let texte = requete.texte.unwrap_or_default();
        if texte.chars().count() > 200 {
            return Err("texte trop long (200 caractères au plus)".into());
        }
        let genre = match requete.genre.as_deref() {
            None => GenreMessage::Info,
            Some(g) => {
                GenreMessage::depuis_texte(g).ok_or("genre invalide : info, action ou alerte")?
            }
        };
        let boutons = requete.boutons.unwrap_or_default();
        if boutons.len() > 3 {
            return Err("3 boutons au plus".into());
        }
        let mut vus = HashSet::new();
        for bouton in &boutons {
            if !identifiant_bouton_valide(&bouton.id) {
                return Err("id de bouton invalide : [a-z0-9_-], 1 à 32 caractères".into());
            }
            if bouton.libelle.trim().is_empty() || bouton.libelle.chars().count() > 24 {
                return Err("libelle de bouton invalide (1 à 24 caractères)".into());
            }
            if !vus.insert(bouton.id.as_str()) {
                return Err(format!("bouton « {} » en double", bouton.id));
            }
        }
        let detail = requete.detail.filter(|d| !d.is_empty());
        if detail.as_ref().is_some_and(|d| d.len() > DETAIL_MAX) {
            return Err("detail trop long (64 Kio au plus)".into());
        }

        let mut etat = self.verrou();
        if let Some(cle) = &cle
            && let Some(existant) = etat
                .values()
                .find(|e| e.message.source == source && e.cle.as_deref() == Some(cle))
        {
            return Ok(existant.message.id.clone());
        }
        if etat.len() >= MESSAGES_MAX {
            return Err("file d'attente pleine : trop de messages gardés sur la machine".into());
        }
        let id = nouvel_id().map_err(|e| format!("identifiant impossible ({e})"))?;
        let enregistrement = Enregistrement {
            message: MessageMachine {
                id: id.clone(),
                source,
                titre,
                texte,
                genre,
                boutons,
                date: instant,
                detail: detail.is_some(),
            },
            cle,
            accepte: false,
            reponse: None,
            reponse_lue: false,
        };
        // Le détail d'abord : un message ne doit jamais annoncer un détail absent.
        if let Some(detail) = &detail {
            ecrire_prive(&self.fichier_detail(&id), detail.as_bytes())
                .map_err(|e| format!("écriture impossible ({e})"))?;
        }
        if let Err(e) = self.enregistrer(&enregistrement) {
            let _ = fs::remove_file(self.fichier_detail(&id));
            return Err(format!("écriture impossible ({e})"));
        }
        etat.insert(id.clone(), enregistrement);
        drop(etat);
        self.reveil.notify_one();
        Ok(id)
    }

    /// Traite une ligne de requête et renvoie la réponse.
    pub fn traiter(&self, ligne: &str, instant: u64) -> Value {
        let requete: Requete = match serde_json::from_str(ligne) {
            Ok(requete) => requete,
            Err(e) => return erreur(&format!("requête illisible ({e})")),
        };
        match requete.op.as_str() {
            "envoyer" => match self.envoyer(requete, instant) {
                Ok(id) => json!({ "ok": true, "id": id }),
                Err(raison) => erreur(&raison),
            },
            "reponses" => match requete.source.as_deref() {
                Some(source) if source_valide(source) => self.reponses(source),
                Some(_) => erreur("source invalide : [a-z0-9-], 1 à 32 caractères"),
                None => erreur("source manquante"),
            },
            "etat" => self.etat(),
            "" => erreur("op manquante"),
            autre => erreur(&format!("op inconnue : {autre}")),
        }
    }

    /// Ce compte du système a-t-il le droit de parler au socket ? Root toujours ; les autres
    /// seulement s'ils figurent dans `utilisateurs`.
    pub fn uid_autorise(&self, uid: u32) -> bool {
        uid == 0 || self.utilisateurs.iter().any(|nom| uid_de(nom) == Some(uid))
    }
}

/// Numéro d'utilisateur d'un compte du système.
fn uid_de(nom: &str) -> Option<u32> {
    let nom = CString::new(nom).ok()?;
    // SAFETY : getpwnam_r écrit dans les tampons fournis (tailles passées) et dans `resultat`.
    unsafe {
        let mut entree: libc::passwd = std::mem::zeroed();
        let mut tampon = vec![0u8; 16 * 1024];
        let mut resultat: *mut libc::passwd = std::ptr::null_mut();
        let code = libc::getpwnam_r(
            nom.as_ptr(),
            &mut entree,
            tampon.as_mut_ptr().cast(),
            tampon.len(),
            &mut resultat,
        );
        (code == 0 && !resultat.is_null()).then_some(entree.pw_uid)
    }
}

/// Écoute sur le socket jusqu'à ce que la tâche soit arrêtée. Le socket est en 0666 : la liste
/// `utilisateurs`, vérifiée par `SO_PEERCRED` à chaque connexion, fait foi.
pub async fn servir(messagerie: Arc<Messagerie>, socket: PathBuf) -> io::Result<()> {
    if let Some(parent) = socket.parent() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o755)
            .create(parent)?;
    }
    // Un socket resté d'une exécution précédente empêcherait d'écouter.
    if fs::symlink_metadata(&socket).is_ok_and(|m| {
        use std::os::unix::fs::FileTypeExt;
        m.file_type().is_socket()
    }) {
        fs::remove_file(&socket)?;
    }
    let ecoute = UnixListener::bind(&socket)?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o666))?;
    tracing::info!("messages : socket {}", socket.display());
    let places = Arc::new(Semaphore::new(CONNEXIONS_MAX));
    loop {
        let (flux, _) = ecoute.accept().await?;
        let Ok(place) = places.clone().try_acquire_owned() else {
            // Trop de connexions : le client réessaiera.
            drop(flux);
            continue;
        };
        let messagerie = messagerie.clone();
        tokio::spawn(async move {
            connexion(&messagerie, flux).await;
            drop(place);
        });
    }
}

/// Répond puis ferme. Ce que le client a encore à envoyer est lu et jeté (un instant) : fermer
/// avec des données non lues ferait réinitialiser la connexion, et le client perdrait la réponse.
async fn repondre(flux: &mut UnixStream, reponse: &Value) {
    let mut ligne = reponse.to_string();
    ligne.push('\n');
    let _ = flux.write_all(ligne.as_bytes()).await;
    let _ = flux.shutdown().await;
    let jeter = async {
        let mut poubelle = [0u8; 8192];
        let mut total = 0usize;
        while total < 4 * REQUETE_MAX {
            match flux.read(&mut poubelle).await {
                Ok(0) | Err(_) => break,
                Ok(n) => total += n,
            }
        }
    };
    let _ = tokio::time::timeout(Duration::from_secs(1), jeter).await;
}

/// Une connexion : contrôle de l'identité, une requête, une réponse, fermeture.
async fn connexion(messagerie: &Messagerie, mut flux: UnixStream) {
    let autorise = flux
        .peer_cred()
        .is_ok_and(|identite| messagerie.uid_autorise(identite.uid()));
    if !autorise {
        tracing::warn!("messages : connexion refusée (compte non autorisé)");
        return repondre(&mut flux, &erreur("accès refusé : compte non autorisé")).await;
    }
    let mut ligne = Vec::new();
    let lecture = {
        let mut lecteur = BufReader::new((&mut flux).take(REQUETE_MAX as u64 + 1));
        tokio::time::timeout(DELAI_LECTURE, lecteur.read_until(b'\n', &mut ligne)).await
    };
    let reponse = match lecture {
        Err(_) => erreur("requête trop lente"),
        Ok(Err(e)) => erreur(&format!("lecture impossible ({e})")),
        Ok(Ok(_)) if ligne.len() > REQUETE_MAX => erreur("requête trop grande (96 Kio au plus)"),
        Ok(Ok(_)) => match std::str::from_utf8(&ligne) {
            Ok(texte) => messagerie.traiter(texte.trim_end_matches(['\n', '\r']), maintenant()),
            Err(_) => erreur("requête illisible (UTF-8 attendu)"),
        },
    };
    repondre(&mut flux, &reponse).await;
}

/// Purge chaque heure, et une fois au démarrage.
pub async fn purger_periodiquement(messagerie: Arc<Messagerie>) {
    let mut periode = tokio::time::interval(Duration::from_secs(3600));
    loop {
        periode.tick().await;
        let n = messagerie.purger(maintenant());
        if n > 0 {
            tracing::info!("messages : {n} message(s) purgé(s)");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(dossier: &Path) -> ConfigMessages {
        ConfigMessages {
            utilisateurs: vec!["nobody".into()],
            socket: dossier.join("messages.sock"),
            conservation_jours: 30,
            dossier_etat: dossier.join("etat"),
        }
    }

    fn dossier_temporaire(nom: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ricochet-{nom}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn envoi(titre: &str) -> Requete {
        Requete {
            op: "envoyer".into(),
            source: Some("cofiticket".into()),
            titre: Some(titre.into()),
            ..Default::default()
        }
    }

    #[test]
    fn dates_iso() {
        assert_eq!(iso_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(iso_utc(1_794_825_395), "2026-11-16T10:36:35Z");
    }

    #[test]
    fn validations() {
        let d = dossier_temporaire("valid");
        let m = Messagerie::ouvrir(&config(&d)).unwrap();
        let ko = |r: Requete| m.envoyer(r, 1).unwrap_err();
        assert!(ko(Requete::default()).contains("source"));
        let mut r = envoi("t");
        r.source = Some("Mauvaise Source".into());
        assert!(ko(r).contains("source invalide"));
        let mut r = envoi("t");
        r.source = Some("a".repeat(33));
        assert!(ko(r).contains("source invalide"));
        assert!(ko(envoi(&"x".repeat(81))).contains("titre trop long"));
        assert!(ko(envoi("  ")).contains("titre vide"));
        let mut r = envoi("t");
        r.texte = Some("é".repeat(201));
        assert!(ko(r).contains("texte trop long"));
        let mut r = envoi("t");
        r.genre = Some("urgent".into());
        assert!(ko(r).contains("genre"));
        let bouton = |id: &str, l: &str| BoutonMessage {
            id: id.into(),
            libelle: l.into(),
        };
        let mut r = envoi("t");
        r.boutons = Some((0..4).map(|i| bouton(&format!("b{i}"), "x")).collect());
        assert!(ko(r).contains("3 boutons"));
        let mut r = envoi("t");
        r.boutons = Some(vec![bouton("Grand", "x")]);
        assert!(ko(r).contains("id de bouton"));
        let mut r = envoi("t");
        r.boutons = Some(vec![bouton("ok", &"x".repeat(25))]);
        assert!(ko(r).contains("libelle"));
        let mut r = envoi("t");
        r.boutons = Some(vec![bouton("ok", "a"), bouton("ok", "b")]);
        assert!(ko(r).contains("double"));
        let mut r = envoi("t");
        r.cle = Some("k".repeat(129));
        assert!(ko(r).contains("cle"));
        let mut r = envoi("t");
        r.detail = Some("d".repeat(DETAIL_MAX + 1));
        assert!(ko(r).contains("detail"));
        // Limites acceptées pile.
        let mut r = envoi(&"é".repeat(80));
        r.texte = Some("x".repeat(200));
        r.detail = Some("d".repeat(DETAIL_MAX));
        r.boutons = Some(vec![bouton("a", &"é".repeat(24)), bouton("b", "B")]);
        assert!(m.envoyer(r, 1).is_ok());
        assert_eq!(m.traiter("pas du json", 1)["ok"], false);
        assert_eq!(m.traiter(r#"{"op":"inconnue"}"#, 1)["ok"], false);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn dedoublonnage_par_source_et_cle() {
        let d = dossier_temporaire("dedup");
        let m = Messagerie::ouvrir(&config(&d)).unwrap();
        let mut r = envoi("premier");
        r.cle = Some("R-1:attente".into());
        let id = m.envoyer(r, 1).unwrap();
        let mut r = envoi("second");
        r.cle = Some("R-1:attente".into());
        assert_eq!(m.envoyer(r, 2).unwrap(), id);
        // Autre source, ou autre clé : nouveau message.
        let mut r = envoi("autre source");
        r.cle = Some("R-1:attente".into());
        r.source = Some("ccs".into());
        assert_ne!(m.envoyer(r, 3).unwrap(), id);
        let mut r = envoi("autre cle");
        r.cle = Some("R-2:attente".into());
        assert_ne!(m.envoyer(r, 4).unwrap(), id);
        // Sans clé : jamais dédoublonné.
        assert_ne!(
            m.envoyer(envoi("a"), 5).unwrap(),
            m.envoyer(envoi("a"), 5).unwrap()
        );
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn persistance_et_droits() {
        let d = dossier_temporaire("persist");
        let config = config(&d);
        let (id, accepte) = {
            let m = Messagerie::ouvrir(&config).unwrap();
            let mut r = envoi("à garder");
            r.detail = Some("secret client".into());
            r.boutons = Some(vec![BoutonMessage {
                id: "ok".into(),
                libelle: "OK".into(),
            }]);
            r.cle = Some("k".into());
            let id = m.envoyer(r, 100).unwrap();
            let accepte = m.envoyer(envoi("accepté"), 101).unwrap();
            m.accepter(&accepte);
            m.enregistrer_reponse(&id, "ok", "mawena", 200);
            (id, accepte)
        };
        // Droits : dossiers 0700, fichiers 0600.
        let mode = |c: PathBuf| fs::metadata(c).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(config.dossier_etat.clone()), 0o700);
        assert_eq!(mode(config.dossier_etat.join("messages")), 0o700);
        assert_eq!(mode(config.dossier_etat.join("details")), 0o700);
        assert_eq!(
            mode(config.dossier_etat.join(format!("messages/{id}.json"))),
            0o600
        );
        assert_eq!(
            mode(config.dossier_etat.join(format!("details/{id}.txt"))),
            0o600
        );

        // Redémarrage : tout est relu.
        let m = Messagerie::ouvrir(&config).unwrap();
        assert_eq!(m.detail(&id).unwrap(), "secret client");
        assert!(m.detail(&accepte).unwrap_err().contains("pas de détail"));
        assert!(m.detail("../../etc/passwd").is_err());
        assert_eq!(
            m.etat()["en_attente"],
            0,
            "tout est accepté (une réponse implique l'acceptation)"
        );
        let mut deja = HashSet::new();
        assert!(m.a_envoyer(&mut deja).is_empty(), "plus rien à envoyer");
        // Doublon après redémarrage.
        let mut r = envoi("répété");
        r.cle = Some("k".into());
        assert_eq!(m.envoyer(r, 300).unwrap(), id);
        // Les réponses survivent et ne sont rendues qu'une fois.
        let reponses = m.reponses("cofiticket");
        assert_eq!(reponses["reponses"][0]["id"], id.as_str());
        assert_eq!(reponses["reponses"][0]["bouton"], "ok");
        assert_eq!(reponses["reponses"][0]["par"], "mawena");
        assert_eq!(reponses["reponses"][0]["quand"], iso_utc(200));
        assert!(
            m.reponses("cofiticket")["reponses"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let m = Messagerie::ouvrir(&config).unwrap();
        assert!(
            m.reponses("cofiticket")["reponses"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        // Une autre source ne voit pas les réponses.
        assert!(m.reponses("ccs")["reponses"].as_array().unwrap().is_empty());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn reponse_unique_et_renvoi() {
        let d = dossier_temporaire("reponse");
        let m = Messagerie::ouvrir(&config(&d)).unwrap();
        let mut r = envoi("t");
        r.boutons = Some(vec![
            BoutonMessage {
                id: "a".into(),
                libelle: "A".into(),
            },
            BoutonMessage {
                id: "b".into(),
                libelle: "B".into(),
            },
        ]);
        let id = m.envoyer(r, 1).unwrap();
        // Messages à renvoyer : une fois par session, tant qu'ils ne sont pas acceptés.
        let mut session1 = HashSet::new();
        assert_eq!(m.a_envoyer(&mut session1).len(), 1);
        assert!(m.a_envoyer(&mut session1).is_empty());
        assert_eq!(
            m.a_envoyer(&mut HashSet::new()).len(),
            1,
            "nouvelle session : renvoi"
        );
        m.accepter(&id);
        assert!(m.a_envoyer(&mut HashSet::new()).is_empty());
        // Bouton inconnu ignoré ; la première réponse gagne.
        m.enregistrer_reponse(&id, "zz", "x", 5);
        assert!(
            m.reponses("cofiticket")["reponses"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        m.enregistrer_reponse(&id, "a", "mawena", 6);
        m.enregistrer_reponse(&id, "b", "autre", 7);
        let r = m.reponses("cofiticket");
        assert_eq!(r["reponses"][0]["bouton"], "a");
        m.enregistrer_reponse(&id, "b", "autre", 8);
        assert!(
            m.reponses("cofiticket")["reponses"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn purge_apres_conservation() {
        let d = dossier_temporaire("purge");
        let config = config(&d);
        let m = Messagerie::ouvrir(&config).unwrap();
        let mut r = envoi("vieux");
        r.detail = Some("d".into());
        let vieux = m.envoyer(r, 1000).unwrap();
        let recent = m.envoyer(envoi("récent"), 1000 + 29 * 86400).unwrap();
        assert_eq!(m.purger(1000 + 29 * 86400), 0);
        assert_eq!(m.purger(1000 + 30 * 86400), 1);
        assert!(m.detail(&vieux).is_err());
        assert!(
            !config
                .dossier_etat
                .join(format!("details/{vieux}.txt"))
                .exists()
        );
        assert!(
            !config
                .dossier_etat
                .join(format!("messages/{vieux}.json"))
                .exists()
        );
        assert!(m.detail(&recent).is_err(), "sans détail");
        assert_eq!(m.etat()["en_attente"], 1);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn utilisateurs_autorises() {
        let d = dossier_temporaire("uid");
        let m = Messagerie::ouvrir(&config(&d)).unwrap();
        assert!(m.uid_autorise(0), "root toujours accepté");
        let nobody = uid_de("nobody").expect("le compte nobody existe");
        assert!(m.uid_autorise(nobody));
        assert!(!m.uid_autorise(nobody + 1), "uid non listé");
        assert_eq!(uid_de("compte-qui-n-existe-pas"), None);
        let _ = fs::remove_dir_all(&d);
    }
}
