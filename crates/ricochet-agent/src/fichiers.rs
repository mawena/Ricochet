//! Accès aux fichiers de la machine : lister un dossier, lire et écrire par morceaux.
//!
//! Les ordres sont exécutés l'un après l'autre par un fil dédié. Avec un `utilisateur` dans la
//! configuration, ce fil prend ses droits pour le système de fichiers (fsuid, fsgid et groupes,
//! propres au fil sous Linux) : l'agent reste root, mais ses accès aux fichiers sont ceux de
//! l'utilisateur, et les fichiers reçus lui appartiennent.

use std::{
    ffi::{CString, OsString},
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

use ricochet_protocole::{
    Entree, Genre, MORCEAU_MAX, NumeroRequete, OrdreFichiers, ReponseFichiers,
};
use tokio::sync::mpsc;

use crate::config::ConfigFichiers;

/// Nombre d'entrées au plus dans la liste d'un dossier (la réponse doit tenir dans un message).
const ENTREES_MAX: usize = 4000;
/// Ordres en attente au plus.
const FILE_MAX: usize = 16;
/// Suffixe du fichier en cours de réception.
pub const SUFFIXE_PARTIEL: &str = ".ricochet-partiel";

pub type Reponse = (NumeroRequete, ReponseFichiers);

/// Accès aux fichiers d'une session : les réponses arrivent dans la file donnée par `new`.
pub struct Fichiers {
    ordres: Option<mpsc::Sender<(NumeroRequete, OrdreFichiers)>>,
}

impl Fichiers {
    /// Sans configuration, l'accès est refusé. Le fil s'arrête avec la valeur renvoyée.
    pub fn new(config: Option<ConfigFichiers>) -> (Self, mpsc::Receiver<Reponse>) {
        let (retour, reponses) = mpsc::channel(FILE_MAX);
        let ordres = config.map(|config| {
            let (envoi, reception) = mpsc::channel(FILE_MAX);
            std::thread::spawn(move || travailleur(config, reception, retour));
            envoi
        });
        (Self { ordres }, reponses)
    }

    /// Transmet un ordre au fil ; renvoie de suite une erreur s'il ne peut pas le prendre.
    pub fn demander(&self, requete: NumeroRequete, ordre: OrdreFichiers) -> Option<Reponse> {
        let message = match &self.ordres {
            None => "accès aux fichiers désactivé sur cette machine",
            Some(ordres) => match ordres.try_send((requete, ordre)) {
                Ok(()) => return None,
                Err(_) => "trop de demandes en cours, réessayer",
            },
        };
        Some((
            requete,
            ReponseFichiers::Erreur {
                message: message.into(),
            },
        ))
    }
}

fn travailleur(
    config: ConfigFichiers,
    mut ordres: mpsc::Receiver<(NumeroRequete, OrdreFichiers)>,
    retour: mpsc::Sender<Reponse>,
) {
    let contexte = Contexte::preparer(&config).map_err(|erreur| {
        let message = format!(
            "fichiers indisponibles : utilisateur « {} » ({erreur})",
            config.utilisateur.as_deref().unwrap_or("root")
        );
        tracing::error!("{message}");
        message
    });
    while let Some((requete, ordre)) = ordres.blocking_recv() {
        let reponse = match &contexte {
            Ok(contexte) => contexte.executer(ordre),
            Err(message) => ReponseFichiers::Erreur {
                message: message.clone(),
            },
        };
        if retour.blocking_send((requete, reponse)).is_err() {
            break;
        }
    }
}

/// Ce que le fil sait pour exécuter les ordres.
pub struct Contexte {
    /// Dossier personnel, désigné par `~`.
    personnel: PathBuf,
}

impl Contexte {
    /// Prend l'identité de l'utilisateur configuré pour ce fil (à appeler dans le fil dédié).
    pub fn preparer(config: &ConfigFichiers) -> io::Result<Self> {
        let personnel = match &config.utilisateur {
            Some(nom) => prendre_identite(nom)?,
            None => std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| "/".into()),
        };
        Ok(Self { personnel })
    }

    pub fn executer(&self, ordre: OrdreFichiers) -> ReponseFichiers {
        let (chemin, resultat) = match ordre {
            OrdreFichiers::Lister { chemin } => {
                let r = self.resoudre(&chemin).and_then(|c| lister(&c));
                (chemin, r)
            }
            OrdreFichiers::Lire {
                chemin,
                decalage,
                taille,
            } => {
                let r = self
                    .resoudre(&chemin)
                    .and_then(|c| lire(&c, decalage, taille));
                (chemin, r)
            }
            OrdreFichiers::Ecrire {
                chemin,
                decalage,
                donnees,
                fin,
            } => {
                let r = self
                    .resoudre(&chemin)
                    .and_then(|c| ecrire(&c, decalage, &donnees, fin));
                (chemin, r)
            }
            OrdreFichiers::Partiel { chemin } => {
                let r = self.resoudre(&chemin).and_then(|c| partiel(&c));
                (chemin, r)
            }
        };
        resultat.unwrap_or_else(|erreur| ReponseFichiers::Erreur {
            message: format!("{chemin} : {erreur}"),
        })
    }

    fn resoudre(&self, chemin: &str) -> io::Result<PathBuf> {
        if chemin.is_empty() || chemin == "~" {
            return Ok(self.personnel.clone());
        }
        if let Some(reste) = chemin.strip_prefix("~/") {
            return Ok(self.personnel.join(reste));
        }
        if !Path::new(chemin).is_absolute() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "chemin absolu attendu",
            ));
        }
        Ok(chemin.into())
    }
}

fn lister(chemin: &Path) -> io::Result<ReponseFichiers> {
    let dossier = fs::canonicalize(chemin)?;
    let mut entrees = Vec::new();
    let mut tronque = false;
    for entree in fs::read_dir(&dossier)? {
        if entrees.len() == ENTREES_MAX {
            tronque = true;
            break;
        }
        let entree = entree?;
        let Ok(propre) = entree.metadata() else {
            continue;
        };
        let lien = propre.is_symlink();
        let meta = if lien {
            fs::metadata(entree.path()).unwrap_or(propre)
        } else {
            propre
        };
        let genre = if meta.is_dir() {
            Genre::Dossier
        } else if meta.is_file() {
            Genre::Fichier
        } else {
            Genre::Autre
        };
        entrees.push(Entree {
            nom: entree.file_name().to_string_lossy().into_owned(),
            genre,
            lien,
            taille: meta.len(),
            modifie_le: meta
                .modified()
                .ok()
                .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs()),
        });
    }
    entrees.sort_by(|a, b| {
        (a.genre != Genre::Dossier, a.nom.to_lowercase())
            .cmp(&(b.genre != Genre::Dossier, b.nom.to_lowercase()))
    });
    Ok(ReponseFichiers::Dossier {
        chemin: dossier.to_string_lossy().into_owned(),
        entrees,
        tronque,
    })
}

fn lire(chemin: &Path, decalage: u64, taille: u32) -> io::Result<ReponseFichiers> {
    let mut fichier = File::open(chemin)?;
    let meta = fichier.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "pas un fichier",
        ));
    }
    fichier.seek(SeekFrom::Start(decalage))?;
    let mut donnees = Vec::new();
    fichier
        .take(u64::from(taille.min(MORCEAU_MAX)))
        .read_to_end(&mut donnees)?;
    Ok(ReponseFichiers::Morceau {
        donnees,
        taille_totale: meta.len(),
    })
}

fn chemin_partiel(chemin: &Path) -> PathBuf {
    let mut nom = OsString::from(chemin.as_os_str());
    nom.push(SUFFIXE_PARTIEL);
    nom.into()
}

fn ecrire(chemin: &Path, decalage: u64, donnees: &[u8], fin: bool) -> io::Result<ReponseFichiers> {
    if chemin.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "c'est un dossier",
        ));
    }
    let partiel = chemin_partiel(chemin);
    let mut fichier = if decalage == 0 {
        File::create(&partiel)?
    } else {
        OpenOptions::new().append(true).open(&partiel)?
    };
    let taille = fichier.metadata()?.len();
    if taille != decalage {
        // Le morceau ne suit pas : l'envoyeur reprend à `taille`.
        return Ok(ReponseFichiers::Ecrit { taille });
    }
    fichier.write_all(donnees)?;
    let taille = taille + donnees.len() as u64;
    if fin {
        fichier.sync_all()?;
        fs::rename(&partiel, chemin)?;
    }
    Ok(ReponseFichiers::Ecrit { taille })
}

fn partiel(chemin: &Path) -> io::Result<ReponseFichiers> {
    let taille = match fs::metadata(chemin_partiel(chemin)) {
        Ok(meta) => meta.len(),
        Err(erreur) if erreur.kind() == io::ErrorKind::NotFound => 0,
        Err(erreur) => return Err(erreur),
    };
    Ok(ReponseFichiers::Ecrit { taille })
}

/// Donne au fil courant les droits de l'utilisateur sur le système de fichiers ; renvoie son
/// dossier personnel.
fn prendre_identite(nom: &str) -> io::Result<PathBuf> {
    let introuvable = || io::Error::new(io::ErrorKind::NotFound, "utilisateur inconnu");
    let c_nom = CString::new(nom).map_err(|_| introuvable())?;
    // SAFETY : tampons possédés ici, de la taille annoncée aux fonctions de la libc.
    unsafe {
        let mut entree: libc::passwd = std::mem::zeroed();
        let mut tampon = vec![0 as libc::c_char; 16 * 1024];
        let mut trouve = std::ptr::null_mut();
        let code = libc::getpwnam_r(
            c_nom.as_ptr(),
            &mut entree,
            tampon.as_mut_ptr(),
            tampon.len(),
            &mut trouve,
        );
        if code != 0 || trouve.is_null() {
            return Err(introuvable());
        }
        let (uid, gid) = (entree.pw_uid, entree.pw_gid);
        let personnel = PathBuf::from(
            std::ffi::CStr::from_ptr(entree.pw_dir)
                .to_string_lossy()
                .into_owned(),
        );

        let mut groupes = vec![0 as libc::gid_t; 64];
        let mut nombre = groupes.len() as libc::c_int;
        if libc::getgrouplist(c_nom.as_ptr(), gid, groupes.as_mut_ptr(), &mut nombre) < 0 {
            groupes.resize(nombre.max(0) as usize, 0);
            if libc::getgrouplist(c_nom.as_ptr(), gid, groupes.as_mut_ptr(), &mut nombre) < 0 {
                return Err(io::Error::other("groupes illisibles"));
            }
        }
        groupes.truncate(nombre.max(0) as usize);
        // Appel système direct : la fonction de la libc change les groupes de tous les fils.
        if libc::syscall(libc::SYS_setgroups, groupes.len(), groupes.as_ptr()) != 0 {
            return Err(io::Error::last_os_error());
        }
        libc::setfsgid(gid);
        libc::setfsuid(uid);
        // Un identifiant invalide ne change rien et renvoie la valeur en place.
        if libc::setfsuid(libc::uid_t::MAX) as libc::uid_t != uid
            || libc::setfsgid(libc::gid_t::MAX) as libc::gid_t != gid
        {
            return Err(io::Error::from(io::ErrorKind::PermissionDenied));
        }
        Ok(personnel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dossier_essai(nom: &str) -> PathBuf {
        let dossier = std::env::temp_dir().join(format!("ricochet-{nom}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dossier);
        fs::create_dir_all(&dossier).unwrap();
        dossier
    }

    fn contexte() -> Contexte {
        Contexte::preparer(&ConfigFichiers::default()).unwrap()
    }

    fn texte(chemin: &Path) -> String {
        chemin.to_str().unwrap().to_owned()
    }

    #[test]
    fn ecrire_par_morceaux_puis_lire() {
        let dossier = dossier_essai("ecrire");
        let cible = dossier.join("a.bin");
        let c = contexte();
        let ecrire = |decalage, donnees: &[u8], fin| {
            c.executer(OrdreFichiers::Ecrire {
                chemin: texte(&cible),
                decalage,
                donnees: donnees.to_vec(),
                fin,
            })
        };
        assert_eq!(
            ecrire(0, b"abc", false),
            ReponseFichiers::Ecrit { taille: 3 }
        );
        // Un morceau qui ne suit pas n'est pas écrit : l'envoyeur apprend où reprendre.
        assert_eq!(
            ecrire(10, b"zzz", false),
            ReponseFichiers::Ecrit { taille: 3 }
        );
        let partiel = c.executer(OrdreFichiers::Partiel {
            chemin: texte(&cible),
        });
        assert_eq!(partiel, ReponseFichiers::Ecrit { taille: 3 });
        assert!(!cible.exists(), "le fichier n'apparaît qu'à la fin");
        assert_eq!(ecrire(3, b"de", true), ReponseFichiers::Ecrit { taille: 5 });
        assert_eq!(fs::read(&cible).unwrap(), b"abcde");
        assert!(!chemin_partiel(&cible).exists());

        let lu = c.executer(OrdreFichiers::Lire {
            chemin: texte(&cible),
            decalage: 1,
            taille: 3,
        });
        assert_eq!(
            lu,
            ReponseFichiers::Morceau {
                donnees: b"bcd".to_vec(),
                taille_totale: 5
            }
        );
        let fin = c.executer(OrdreFichiers::Lire {
            chemin: texte(&cible),
            decalage: 5,
            taille: 3,
        });
        assert!(matches!(fin, ReponseFichiers::Morceau { donnees, .. } if donnees.is_empty()));
        fs::remove_dir_all(dossier).unwrap();
    }

    #[test]
    fn lister_un_dossier() {
        let dossier = dossier_essai("lister");
        fs::write(dossier.join("b.txt"), "x").unwrap();
        fs::create_dir(dossier.join("z")).unwrap();
        std::os::unix::fs::symlink(dossier.join("z"), dossier.join("a")).unwrap();
        let reponse = contexte().executer(OrdreFichiers::Lister {
            chemin: format!("{}/z/..", texte(&dossier)),
        });
        let ReponseFichiers::Dossier {
            chemin,
            entrees,
            tronque,
        } = reponse
        else {
            panic!("{reponse:?}");
        };
        assert_eq!(chemin, texte(&fs::canonicalize(&dossier).unwrap()));
        assert!(!tronque);
        let noms: Vec<_> = entrees
            .iter()
            .map(|e| (e.nom.as_str(), e.genre, e.lien))
            .collect();
        assert_eq!(
            noms,
            [
                ("a", Genre::Dossier, true),
                ("z", Genre::Dossier, false),
                ("b.txt", Genre::Fichier, false)
            ]
        );
        fs::remove_dir_all(dossier).unwrap();
    }

    #[test]
    fn erreurs() {
        let c = contexte();
        let relatif = c.executer(OrdreFichiers::Lister {
            chemin: "etc".into(),
        });
        assert!(
            matches!(relatif, ReponseFichiers::Erreur { message } if message.contains("absolu"))
        );
        let absent = c.executer(OrdreFichiers::Lire {
            chemin: "/nexiste/pas".into(),
            decalage: 0,
            taille: 1,
        });
        assert!(matches!(absent, ReponseFichiers::Erreur { .. }));
        let dossier = c.executer(OrdreFichiers::Lire {
            chemin: "/".into(),
            decalage: 0,
            taille: 1,
        });
        assert!(matches!(dossier, ReponseFichiers::Erreur { .. }));
    }

    #[tokio::test]
    async fn fil_dedie() {
        let (sans, mut reponses) = Fichiers::new(None);
        let (requete, reponse) = sans
            .demander(1, OrdreFichiers::Lister { chemin: "/".into() })
            .unwrap();
        assert_eq!(requete, 1);
        assert!(matches!(reponse, ReponseFichiers::Erreur { .. }));
        drop(sans);
        assert!(reponses.recv().await.is_none());

        let (avec, mut reponses) = Fichiers::new(Some(ConfigFichiers::default()));
        assert!(
            avec.demander(2, OrdreFichiers::Lister { chemin: "/".into() })
                .is_none()
        );
        let (requete, reponse) = reponses.recv().await.unwrap();
        assert_eq!(requete, 2);
        assert!(matches!(reponse, ReponseFichiers::Dossier { chemin, .. } if chemin == "/"));

        // Un utilisateur inconnu rend les fichiers indisponibles, sans arrêter l'agent.
        let config = ConfigFichiers {
            utilisateur: Some("utilisateur-qui-n-existe-pas".into()),
        };
        let (inconnu, mut reponses) = Fichiers::new(Some(config));
        assert!(
            inconnu
                .demander(3, OrdreFichiers::Lister { chemin: "/".into() })
                .is_none()
        );
        let (_, reponse) = reponses.recv().await.unwrap();
        assert!(
            matches!(reponse, ReponseFichiers::Erreur { message } if message.contains("inconnu"))
        );
    }
}
