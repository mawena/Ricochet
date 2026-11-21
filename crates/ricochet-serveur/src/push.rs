//! Notifications Web Push : abonnements des appareils de l'opérateur, envoi, routes de l'API.
//!
//! - `GET    /api/push/cle` : clé publique VAPID, pour `pushManager.subscribe` ;
//! - `GET    /api/push/abonnements`, `POST` (ajout, par le navigateur), `DELETE …/{id}` ;
//! - `POST   /api/push/essai` : notification d'essai à tous les appareils.
//!
//! Le relais ne parle qu'aux services de push listés (`[push] hotes_autorises`) : l'adresse d'un
//! abonnement vient du navigateur, elle ne doit pas permettre de joindre une adresse interne.
//! Ce qui est poussé se limite à l'identifiant, au titre, au texte, au nom de la machine et au
//! genre ; le détail n'est jamais poussé.

use std::{
    net::{IpAddr, SocketAddr},
    sync::{Arc, OnceLock},
    time::Duration,
};

use anyhow::{Context, bail};
use axum::{
    Extension, Json,
    extract::{Path, State},
    http::{StatusCode, Uri},
    response::{IntoResponse, Response},
};
use ricochet_protocole::GenreMessage;
use rustls::pki_types::ServerName;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
    task::JoinSet,
};
use tokio_rustls::TlsConnector;

use crate::{
    Adresse, Base, Relais,
    api::{Erreur, Operateur},
    base::Abonnement,
    config,
    registre::maintenant,
    webpush::{self, Vapid},
};

/// Réglage de la base qui garde la clé privée VAPID (hexadécimal).
const REGLAGE_VAPID: &str = "vapid_prive";
/// Délai total d'un envoi à un service de push.
const DELAI_ENVOI: Duration = Duration::from_secs(20);
/// Appareils abonnés au plus (c'est un outil personnel).
const ABONNEMENTS_MAX: usize = 20;
const ENDPOINT_MAX: usize = 2048;

type Resultat = Result<Response, Erreur>;

/// Les clés VAPID et la configuration TLS du client de push.
pub struct Push {
    pub vapid: Vapid,
    tls: OnceLock<TlsConnector>,
}

impl Push {
    /// Charge la clé VAPID de la base, ou la crée au premier démarrage.
    pub fn charger(base: &Base) -> anyhow::Result<Self> {
        let existante = base.reglage(REGLAGE_VAPID)?;
        let vapid = match existante {
            Some(hex) => Vapid::depuis_octets(&decoder_hex(&hex)?)?,
            None => {
                let neuve = Vapid::generer();
                base.definir_reglage_si_absent(REGLAGE_VAPID, &hex::encode(neuve.secret_octets()))?;
                // Un autre processus a pu en créer une entre-temps : c'est celle de la base qui compte.
                let gardee = base
                    .reglage(REGLAGE_VAPID)?
                    .context("clé VAPID introuvable après sa création")?;
                Vapid::depuis_octets(&decoder_hex(&gardee)?)?
            }
        };
        Ok(Self {
            vapid,
            tls: OnceLock::new(),
        })
    }

    fn connecteur(&self) -> &TlsConnector {
        self.tls.get_or_init(|| {
            let mut racines = rustls::RootCertStore::empty();
            racines.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            let config = rustls::ClientConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .expect("versions TLS par défaut")
            .with_root_certificates(racines)
            .with_no_client_auth();
            TlsConnector::from(Arc::new(config))
        })
    }
}

fn decoder_hex(texte: &str) -> anyhow::Result<Vec<u8>> {
    hex::decode(texte.trim()).context("clé VAPID illisible dans la base")
}

// --- Hôtes autorisés ---

/// L'hôte correspond-il à l'un des motifs (`hote.exemple.org` ou `*.exemple.org`) ?
pub fn hote_autorise(hote: &str, motifs: &[String]) -> bool {
    let hote = hote.to_ascii_lowercase();
    motifs.iter().any(|motif| {
        let motif = motif.trim().to_ascii_lowercase();
        match motif.strip_prefix("*.") {
            Some(domaine) => hote
                .strip_suffix(domaine)
                .is_some_and(|debut| debut.len() > 1 && debut.ends_with('.')),
            None => hote == motif,
        }
    })
}

/// Adresse joignable depuis Internet ; pas une adresse locale, privée ou réservée.
pub fn adresse_publique(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_documentation()
                || o[0] == 0
                || (o[0] == 100 && (64..128).contains(&o[1])) // CGNAT
                || o[0] >= 240)
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return adresse_publique(IpAddr::V4(v4));
            }
            let premier = v6.segments()[0];
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (premier & 0xfe00) == 0xfc00 // fc00::/7
                || (premier & 0xffc0) == 0xfe80 // fe80::/10
                || (premier == 0x2001 && v6.segments()[1] == 0x0db8))
        }
    }
}

/// Où envoyer : un abonnement validé.
#[derive(Debug, PartialEq, Eq)]
pub struct Cible {
    pub https: bool,
    pub hote: String,
    pub port: u16,
    /// Chemin et paramètres de l'adresse.
    pub chemin: String,
}

impl Cible {
    /// Origine du service (audience du jeton VAPID).
    pub fn origine(&self) -> String {
        let (schema, defaut) = if self.https { ("https", 443) } else { ("http", 80) };
        if self.port == defaut {
            format!("{schema}://{}", self.hote)
        } else {
            format!("{schema}://{}:{}", self.hote, self.port)
        }
    }
}

/// Vérifie l'adresse d'un abonnement : https (http seulement en mode d'essai), sans identifiants,
/// vers un hôte autorisé.
pub fn analyser_endpoint(adresse: &str, reglage: &config::Push) -> Result<Cible, String> {
    if adresse.len() > ENDPOINT_MAX {
        return Err("adresse d'abonnement trop longue".into());
    }
    let uri: Uri = adresse
        .parse()
        .map_err(|_| "adresse d'abonnement invalide".to_owned())?;
    let https = match uri.scheme_str() {
        Some("https") => true,
        Some("http") if reglage.autoriser_http => false,
        _ => return Err("l'adresse d'abonnement doit être en https".into()),
    };
    let autorite = uri.authority().ok_or("adresse d'abonnement invalide")?;
    if autorite.as_str().contains('@') {
        return Err("adresse d'abonnement invalide".into());
    }
    let hote = uri.host().ok_or("adresse d'abonnement invalide")?.to_ascii_lowercase();
    if !hote_autorise(&hote, &reglage.hotes_autorises) {
        return Err(format!("service de push non autorisé : {hote}"));
    }
    Ok(Cible {
        https,
        port: uri.port_u16().unwrap_or(if https { 443 } else { 80 }),
        hote,
        chemin: uri
            .path_and_query()
            .map_or_else(|| "/".to_owned(), |p| p.as_str().to_owned()),
    })
}

// --- Envoi ---

/// Ce qui est poussé : de quoi afficher la notification, et rien d'autre.
#[derive(Debug, Clone, Serialize)]
pub struct Charge {
    pub id: String,
    pub titre: String,
    pub texte: String,
    pub machine: String,
    pub genre: GenreMessage,
}

impl Charge {
    /// Urgence du message (RFC 8030) : une alerte ou une action ne doit pas attendre.
    fn urgence(&self) -> &'static str {
        match self.genre {
            GenreMessage::Alerte => "high",
            GenreMessage::Action => "normal",
            GenreMessage::Info => "low",
        }
    }
}

/// Ce qui est arrivé à un envoi.
#[derive(Debug, PartialEq, Eq)]
pub enum Issue {
    Livre,
    /// Le service dit que l'abonnement n'existe plus (404, 410) : il a été supprimé.
    Perime,
    Echec(String),
}

/// Compte rendu d'une diffusion.
#[derive(Debug, Default, Serialize, PartialEq, Eq)]
pub struct Bilan {
    pub appareils: usize,
    pub envoyes: usize,
    pub supprimes: usize,
    pub echecs: usize,
}

/// Une requête POST en HTTP/1.1 sur un flux déjà ouvert ; ne lit que la ligne de statut.
async fn echanger<S: AsyncRead + AsyncWrite + Unpin>(
    flux: &mut S,
    tete: &str,
    corps: &[u8],
) -> anyhow::Result<u16> {
    flux.write_all(tete.as_bytes()).await?;
    flux.write_all(corps).await?;
    flux.flush().await?;
    let mut recu = Vec::new();
    let mut tampon = [0u8; 1024];
    loop {
        if let Some(fin) = recu.windows(2).position(|f| f == b"\r\n") {
            let ligne = String::from_utf8_lossy(&recu[..fin]);
            let mut mots = ligne.split_whitespace();
            let version = mots.next().unwrap_or("");
            if !version.starts_with("HTTP/1.") {
                bail!("réponse HTTP illisible");
            }
            return mots
                .next()
                .and_then(|s| s.parse().ok())
                .context("statut HTTP illisible");
        }
        if recu.len() > 8192 {
            bail!("réponse HTTP trop longue");
        }
        let n = flux.read(&mut tampon).await?;
        if n == 0 {
            bail!("connexion fermée avant la réponse");
        }
        recu.extend_from_slice(&tampon[..n]);
    }
}

impl Push {
    /// Envoie un contenu déjà chiffré à un service de push ; renvoie le statut HTTP.
    async fn poster(
        &self,
        reglage: &config::Push,
        cible: &Cible,
        entetes: &[(&str, String)],
        corps: &[u8],
    ) -> anyhow::Result<u16> {
        // L'hôte est résolu ici et chaque adresse vérifiée : un nom autorisé ne doit pas
        // mener à une adresse interne.
        let adresses: Vec<SocketAddr> = tokio::net::lookup_host((cible.hote.as_str(), cible.port))
            .await
            .with_context(|| format!("résolution de {}", cible.hote))?
            .filter(|a| reglage.autoriser_http || adresse_publique(a.ip()))
            .collect();
        if adresses.is_empty() {
            bail!("{} ne mène à aucune adresse publique", cible.hote);
        }
        let flux = TcpStream::connect(adresses.as_slice())
            .await
            .with_context(|| format!("connexion à {}", cible.hote))?;
        let mut tete = format!(
            "POST {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: ricochet\r\nConnection: close\r\n\
             Content-Length: {}\r\n",
            cible.chemin,
            cible
                .origine()
                .split_once("://")
                .map_or("", |(_, hote)| hote),
            corps.len()
        );
        for (nom, valeur) in entetes {
            tete.push_str(&format!("{nom}: {valeur}\r\n"));
        }
        tete.push_str("\r\n");
        if cible.https {
            let nom = ServerName::try_from(cible.hote.clone()).context("nom d'hôte invalide")?;
            let mut tls = self.connecteur().connect(nom, flux).await.context("TLS")?;
            echanger(&mut tls, &tete, corps).await
        } else {
            let mut flux = flux;
            echanger(&mut flux, &tete, corps).await
        }
    }
}

/// Envoie la charge à un appareil.
async fn envoyer_a(relais: &Relais, abonnement: &Abonnement, contenu: &[u8], charge: &Charge) -> Issue {
    let reglage = &relais.config.push;
    let Some(sujet) = reglage.sujet() else {
        return Issue::Echec("[push] url_publique absente de la configuration".into());
    };
    let resultat = async {
        let cible = analyser_endpoint(&abonnement.endpoint, reglage).map_err(anyhow::Error::msg)?;
        let corps = webpush::chiffrer(
            contenu,
            &webpush::decoder_base64url(&abonnement.p256dh)?,
            &webpush::decoder_base64url(&abonnement.auth)?,
        )?;
        let entetes = [
            (
                "Authorization",
                relais
                    .push
                    .vapid
                    .autorisation(&cible.origine(), sujet, maintenant()),
            ),
            ("TTL", reglage.ttl_s.to_string()),
            ("Urgency", charge.urgence().to_owned()),
            ("Content-Encoding", "aes128gcm".to_owned()),
            ("Content-Type", "application/octet-stream".to_owned()),
        ];
        tokio::time::timeout(
            DELAI_ENVOI,
            relais.push.poster(reglage, &cible, &entetes, &corps),
        )
        .await
        .context("délai dépassé")?
    }
    .await;
    match resultat {
        Ok(200..=299) => Issue::Livre,
        Ok(404 | 410) => Issue::Perime,
        Ok(statut) => Issue::Echec(format!("statut {statut}")),
        Err(erreur) => Issue::Echec(format!("{erreur:#}")),
    }
}

/// Pousse la charge vers tous les appareils abonnés ; supprime ceux que le service dit périmés.
pub async fn diffuser(relais: &Arc<Relais>, charge: Charge) -> Bilan {
    let abonnements = match relais.base.abonnements() {
        Ok(liste) => liste,
        Err(erreur) => {
            tracing::error!("abonnements push : {erreur:#}");
            return Bilan::default();
        }
    };
    let mut bilan = Bilan {
        appareils: abonnements.len(),
        ..Default::default()
    };
    if relais.config.push.sujet().is_none() {
        if !abonnements.is_empty() {
            tracing::warn!("notification non envoyée : [push] url_publique absente");
        }
        bilan.echecs = abonnements.len();
        return bilan;
    }
    let contenu = serde_json::to_vec(&charge).expect("la charge est sérialisable");
    let charge = Arc::new(charge);
    let mut envois = JoinSet::new();
    for abonnement in abonnements {
        let (relais, contenu, charge) = (relais.clone(), contenu.clone(), charge.clone());
        envois.spawn(async move {
            let issue = envoyer_a(&relais, &abonnement, &contenu, &charge).await;
            (abonnement, issue)
        });
    }
    while let Some(fini) = envois.join_next().await {
        let Ok((abonnement, issue)) = fini else {
            bilan.echecs += 1;
            continue;
        };
        match issue {
            Issue::Livre => bilan.envoyes += 1,
            Issue::Perime => {
                bilan.supprimes += 1;
                if let Err(erreur) = relais.base.supprimer_abonnement(abonnement.id) {
                    tracing::error!("suppression d'un abonnement périmé : {erreur:#}");
                }
                relais.noter(
                    None,
                    "-",
                    "push_abonnement_retire",
                    &format!("{} (périmé)", abonnement.appareil),
                );
            }
            Issue::Echec(raison) => {
                bilan.echecs += 1;
                tracing::warn!("push vers « {} » : {raison}", abonnement.appareil);
            }
        }
    }
    bilan
}

// --- Routes ---

fn refus(statut: StatusCode, texte: &str) -> Response {
    (statut, texte.to_owned()).into_response()
}

/// Clé publique VAPID : le navigateur s'en sert pour s'abonner.
pub(crate) async fn cle(State(relais): State<Arc<Relais>>) -> Response {
    if relais.config.push.sujet().is_none() {
        return refus(
            StatusCode::SERVICE_UNAVAILABLE,
            "notifications non configurées sur le relais (section [push], url_publique)",
        );
    }
    Json(json!({ "cle": relais.push.vapid.publique_base64url() })).into_response()
}

fn hote_de(endpoint: &str) -> String {
    endpoint
        .parse::<Uri>()
        .ok()
        .and_then(|u| u.host().map(str::to_owned))
        .unwrap_or_default()
}

pub(crate) async fn liste(State(relais): State<Arc<Relais>>) -> Resultat {
    let vues: Vec<_> = relais
        .base
        .abonnements()?
        .into_iter()
        .map(|a| {
            json!({
                "id": a.id,
                "compte": a.compte,
                "appareil": a.appareil,
                "cree_le": a.cree_le,
                "hote": hote_de(&a.endpoint),
                // Le navigateur calcule la même empreinte pour reconnaître son propre abonnement.
                "empreinte": crate::jeton::empreinte(&a.endpoint),
            })
        })
        .collect();
    Ok(Json(vues).into_response())
}

#[derive(Deserialize)]
pub(crate) struct Cles {
    p256dh: String,
    auth: String,
}

/// Une `PushSubscription` du navigateur, au format de `toJSON()`.
#[derive(Deserialize)]
pub(crate) struct NouvelAbonnement {
    endpoint: String,
    keys: Cles,
    #[serde(default)]
    appareil: Option<String>,
}

pub(crate) async fn ajouter(
    State(relais): State<Arc<Relais>>,
    Extension(operateur): Extension<Operateur>,
    Adresse(adresse): Adresse,
    Json(nouveau): Json<NouvelAbonnement>,
) -> Resultat {
    if let Err(raison) = analyser_endpoint(&nouveau.endpoint, &relais.config.push) {
        return Ok(refus(StatusCode::BAD_REQUEST, &raison));
    }
    let cle_valide = webpush::decoder_base64url(&nouveau.keys.p256dh)
        .ok()
        .is_some_and(|k| k.len() == 65 && p256::PublicKey::from_sec1_bytes(&k).is_ok());
    let auth_valide = webpush::decoder_base64url(&nouveau.keys.auth)
        .ok()
        .is_some_and(|a| a.len() == 16);
    if !cle_valide || !auth_valide {
        return Ok(refus(StatusCode::BAD_REQUEST, "clés d'abonnement invalides"));
    }
    let existants = relais.base.abonnements()?;
    if existants.len() >= ABONNEMENTS_MAX && !existants.iter().any(|a| a.endpoint == nouveau.endpoint) {
        return Ok(refus(
            StatusCode::CONFLICT,
            "trop d'appareils abonnés : en retirer avant d'en ajouter",
        ));
    }
    let appareil: String = nouveau
        .appareil
        .as_deref()
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .unwrap_or("appareil")
        .chars()
        .filter(|c| !c.is_control())
        .take(100)
        .collect();
    let id = relais.base.ajouter_abonnement(
        &operateur.compte,
        &nouveau.endpoint,
        nouveau.keys.p256dh.trim_end_matches('='),
        nouveau.keys.auth.trim_end_matches('='),
        &appareil,
        maintenant(),
    )?;
    relais.noter(
        Some(&operateur.compte),
        &adresse,
        "push_abonnement_ajoute",
        &format!("{appareil} ({})", hote_de(&nouveau.endpoint)),
    );
    Ok((StatusCode::CREATED, Json(json!({ "id": id }))).into_response())
}

pub(crate) async fn retirer(
    State(relais): State<Arc<Relais>>,
    Extension(operateur): Extension<Operateur>,
    Adresse(adresse): Adresse,
    Path(id): Path<i64>,
) -> Resultat {
    let Some(appareil) = relais.base.supprimer_abonnement(id)? else {
        return Ok(refus(StatusCode::NOT_FOUND, "abonnement inconnu"));
    };
    relais.noter(
        Some(&operateur.compte),
        &adresse,
        "push_abonnement_retire",
        &appareil,
    );
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Notification d'essai à tous les appareils : le compte rendu dit ce qui est arrivé.
pub(crate) async fn essai(
    State(relais): State<Arc<Relais>>,
    Extension(operateur): Extension<Operateur>,
    Adresse(adresse): Adresse,
) -> Resultat {
    if relais.config.push.sujet().is_none() {
        return Ok(refus(
            StatusCode::SERVICE_UNAVAILABLE,
            "notifications non configurées sur le relais (section [push], url_publique)",
        ));
    }
    let bilan = diffuser(
        &relais,
        Charge {
            id: "essai".into(),
            titre: "Notification d'essai".into(),
            texte: "Ricochet peut vous prévenir sur cet appareil.".into(),
            machine: "ricochet".into(),
            genre: GenreMessage::Info,
        },
    )
    .await;
    relais.noter(
        Some(&operateur.compte),
        &adresse,
        "push_essai",
        &format!(
            "{} appareil(s), {} envoyé(s), {} échec(s)",
            bilan.appareils, bilan.envoyes, bilan.echecs
        ),
    );
    Ok(Json(bilan).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn motifs() -> Vec<String> {
        config::HOTES_PUSH.iter().map(|h| (*h).to_owned()).collect()
    }

    #[test]
    fn hotes_de_push() {
        let m = motifs();
        for bon in [
            "fcm.googleapis.com",
            "web.push.apple.com",
            "WEB.PUSH.APPLE.COM",
            "updates.push.services.mozilla.com",
            "wns2-par02p.notify.windows.com",
            "a.b.push.apple.com",
        ] {
            assert!(hote_autorise(bon, &m), "{bon}");
        }
        for mauvais in [
            "localhost",
            "127.0.0.1",
            "evil.example",
            "fcm.googleapis.com.evil.example",
            "evilfcm.googleapis.com",
            "push.apple.com",
            "xpush.apple.com",
            "apple.com",
            "169.254.169.254",
            "[::1]",
        ] {
            assert!(!hote_autorise(mauvais, &m), "{mauvais}");
        }
    }

    #[test]
    fn adresses_de_abonnement() {
        let reglage = config::Push::default();
        let cible = analyser_endpoint("https://fcm.googleapis.com/fcm/send/abc?x=1", &reglage).unwrap();
        assert_eq!(cible.hote, "fcm.googleapis.com");
        assert_eq!((cible.port, cible.https), (443, true));
        assert_eq!(cible.chemin, "/fcm/send/abc?x=1");
        assert_eq!(cible.origine(), "https://fcm.googleapis.com");
        for refusee in [
            "http://fcm.googleapis.com/x",
            "https://127.0.0.1/x",
            "https://localhost:7870/x",
            "https://metadata.internal/x",
            "https://user:pass@fcm.googleapis.com/x",
            "https://evil.example@fcm.googleapis.com.evil.example/x",
            "ftp://fcm.googleapis.com/x",
            "/relatif",
            "pas une adresse",
        ] {
            assert!(analyser_endpoint(refusee, &reglage).is_err(), "{refusee}");
        }
        assert!(analyser_endpoint(&format!("https://web.push.apple.com/{}", "a".repeat(2100)), &reglage).is_err());
        // Mode d'essai : http et hôte local, mais seulement si l'hôte est listé.
        let essai = config::Push {
            autoriser_http: true,
            hotes_autorises: vec!["127.0.0.1".into()],
            ..Default::default()
        };
        let cible = analyser_endpoint("http://127.0.0.1:9999/push/1", &essai).unwrap();
        assert_eq!((cible.port, cible.https), (9999, false));
        assert_eq!(cible.origine(), "http://127.0.0.1:9999");
        assert!(analyser_endpoint("http://localhost:9999/x", &essai).is_err());
    }

    #[test]
    fn adresses_publiques() {
        for locale in [
            "127.0.0.1", "10.0.0.1", "192.168.1.1", "172.16.0.1", "169.254.169.254", "0.0.0.0",
            "100.64.0.1", "224.0.0.1", "::1", "::", "fc00::1", "fd12::1", "fe80::1",
            "::ffff:127.0.0.1", "::ffff:10.0.0.1", "ff02::1",
        ] {
            assert!(!adresse_publique(locale.parse().unwrap()), "{locale}");
        }
        for publique in ["8.8.8.8", "17.253.0.1", "142.250.0.1", "2a00:1450::1", "::ffff:8.8.8.8"] {
            assert!(adresse_publique(publique.parse().unwrap()), "{publique}");
        }
    }

    #[test]
    fn urgence_selon_le_genre() {
        let charge = |genre| Charge {
            id: "1".into(),
            titre: "t".into(),
            texte: "x".into(),
            machine: "m".into(),
            genre,
        };
        assert_eq!(charge(GenreMessage::Alerte).urgence(), "high");
        assert_eq!(charge(GenreMessage::Action).urgence(), "normal");
        assert_eq!(charge(GenreMessage::Info).urgence(), "low");
        // Rien d'autre que ces cinq champs n'est poussé.
        let json = serde_json::to_value(charge(GenreMessage::Info)).unwrap();
        let mut champs: Vec<_> = json.as_object().unwrap().keys().cloned().collect();
        champs.sort();
        assert_eq!(champs, ["genre", "id", "machine", "texte", "titre"]);
    }

    #[test]
    fn cle_vapid_gardee_en_base() {
        let base = Base::en_memoire();
        let premiere = Push::charger(&base).unwrap();
        let seconde = Push::charger(&base).unwrap();
        assert_eq!(premiere.vapid.publique(), seconde.vapid.publique());
    }
}
