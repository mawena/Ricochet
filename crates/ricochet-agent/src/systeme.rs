//! Informations et état de la machine.

use ricochet_protocole::{EtatDisque, EtatMachine, InfoMachine};
use sysinfo::{Disks, System};

/// Systèmes de fichiers sans intérêt pour l'espace disque (images montées, mémoire…).
const SYSTEMES_IGNORES: &[&str] = &["squashfs", "overlay", "tmpfs", "devtmpfs", "efivarfs"];

/// `terminal` : l'agent accepte d'ouvrir des terminaux.
pub fn info_machine(terminal: bool, fichiers: bool) -> InfoMachine {
    InfoMachine {
        nom_hote: System::host_name().unwrap_or_default(),
        systeme: System::long_os_version().unwrap_or_default(),
        noyau: System::kernel_version().unwrap_or_default(),
        architecture: System::cpu_arch(),
        version_agent: env!("CARGO_PKG_VERSION").to_owned(),
        terminal,
        fichiers,
    }
}

/// Mesure l'état de la machine ; le garder d'une mesure à l'autre rend l'usage CPU exact.
pub struct Collecteur {
    systeme: System,
    disques: Disks,
}

impl Collecteur {
    pub fn new() -> Result<Self, sysinfo::Error> {
        let mut systeme = System::new()?;
        systeme.refresh_cpu_all();
        Ok(Self {
            systeme,
            disques: Disks::new()?,
        })
    }

    pub fn etat(&mut self) -> EtatMachine {
        self.systeme.refresh_cpu_all();
        self.systeme.refresh_memory();
        self.disques.refresh(true);
        let charge = System::load_average()
            .map(|c| [c.one, c.five, c.fifteen])
            .unwrap_or_default();
        let mut disques: Vec<EtatDisque> = self
            .disques
            .iter()
            .filter(|d| {
                let fs = d.file_system().to_string_lossy();
                !SYSTEMES_IGNORES.contains(&fs.as_ref()) && d.total_space() > 0
            })
            .map(|d| EtatDisque {
                point_montage: d.mount_point().to_string_lossy().into_owned(),
                utilise: d.total_space().saturating_sub(d.available_space()),
                total: d.total_space(),
            })
            .collect();
        disques.sort_by(|a, b| a.point_montage.cmp(&b.point_montage));
        disques.dedup_by(|a, b| a.point_montage == b.point_montage);
        EtatMachine {
            cpu_pourcent: self.systeme.global_cpu_usage(),
            nombre_coeurs: self.systeme.cpus().len() as u32,
            charge,
            memoire_utilisee: self.systeme.used_memory(),
            memoire_totale: self.systeme.total_memory(),
            disques,
            allumee_depuis_s: System::uptime().unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mesures_plausibles() {
        let info = info_machine(true, true);
        assert!(info.terminal);
        assert!(!info.nom_hote.is_empty());
        let etat = Collecteur::new().unwrap().etat();
        assert!(etat.nombre_coeurs > 0);
        assert!(etat.memoire_totale > 0 && etat.memoire_utilisee <= etat.memoire_totale);
        assert!(
            etat.disques.iter().any(|d| d.point_montage == "/"),
            "{:?}",
            etat.disques
        );
        assert!(etat.allumee_depuis_s > 0);
    }
}
