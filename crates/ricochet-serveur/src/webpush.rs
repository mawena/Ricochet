//! Web Push : chiffrement du contenu (RFC 8291, `aes128gcm`) et identification du relais par
//! VAPID (RFC 8292, jeton JWT signé ES256). Uniquement des bibliothèques RustCrypto, pas
//! d'OpenSSL.
//!
//! Le relais pousse seulement de quoi afficher la notification (identifiant, titre, texte,
//! machine, genre) : le navigateur ouvre ensuite l'interface, qui demande le reste.

use aes_gcm::{Aes128Gcm, KeyInit, aead::Aead};
use anyhow::{Context, anyhow, bail};
use data_encoding::BASE64URL_NOPAD;
use hkdf::Hkdf;
use p256::{
    PublicKey, SecretKey,
    ecdh::diffie_hellman,
    ecdsa::{Signature, SigningKey, signature::Signer},
    elliptic_curve::{rand_core::{OsRng, RngCore}, sec1::ToEncodedPoint},
};
use sha2_010::Sha256;

/// Taille d'un enregistrement chiffré (un seul est émis).
const TAILLE_ENREGISTREMENT: u32 = 4096;
/// Un enregistrement porte l'en-tête (16 + 4 + 1 + 65 octets), le délimiteur et l'étiquette
/// GCM (16 octets) : voilà ce qui reste pour le contenu.
pub const CONTENU_MAX: usize = TAILLE_ENREGISTREMENT as usize - (16 + 4 + 1 + 65) - 1 - 16;
/// Durée de validité du jeton VAPID : bien moins que les 24 h permises.
pub const VALIDITE_JWT_S: u64 = 12 * 3600;

pub fn base64url(octets: &[u8]) -> String {
    BASE64URL_NOPAD.encode(octets)
}

/// Décode du base64url, avec ou sans remplissage `=` (les navigateurs n'en mettent pas).
pub fn decoder_base64url(texte: &str) -> anyhow::Result<Vec<u8>> {
    BASE64URL_NOPAD
        .decode(texte.trim_end_matches('=').as_bytes())
        .map_err(|e| anyhow!("base64url invalide ({e})"))
}

/// Chiffre `contenu` pour l'abonné dont la clé publique (`p256dh`, 65 octets) et le secret
/// d'authentification (`auth`, 16 octets) sont donnés. `cle_relais` est la clé éphémère de ce
/// message et `sel` son sel : tirés au hasard par `chiffrer`, fixés ici pour le vecteur de test.
pub fn chiffrer_avec(
    contenu: &[u8],
    p256dh: &[u8],
    auth: &[u8],
    cle_relais: &SecretKey,
    sel: &[u8; 16],
) -> anyhow::Result<Vec<u8>> {
    if contenu.len() > CONTENU_MAX {
        bail!("contenu trop long pour une notification ({} octets)", contenu.len());
    }
    if p256dh.len() != 65 || auth.len() != 16 {
        bail!("clés d'abonnement de taille inattendue");
    }
    let cle_abonne = PublicKey::from_sec1_bytes(p256dh).context("clé p256dh invalide")?;
    let publique_relais = cle_relais.public_key().to_encoded_point(false);
    let secret = diffie_hellman(cle_relais.to_nonzero_scalar(), cle_abonne.as_affine());

    // RFC 8291 §3.4 : la clé d'entrée mêle le secret ECDH, le secret d'authentification et les
    // deux clés publiques.
    let mut info_cle = b"WebPush: info\0".to_vec();
    info_cle.extend_from_slice(p256dh);
    info_cle.extend_from_slice(publique_relais.as_bytes());
    let mut entree = [0u8; 32];
    Hkdf::<Sha256>::new(Some(auth), secret.raw_secret_bytes())
        .expand(&info_cle, &mut entree)
        .map_err(|_| anyhow!("dérivation de la clé d'entrée"))?;

    // RFC 8188 : clé de chiffrement et nonce, dérivés avec le sel de l'enregistrement.
    let derivation = Hkdf::<Sha256>::new(Some(sel), &entree);
    let mut cle = [0u8; 16];
    derivation
        .expand(b"Content-Encoding: aes128gcm\0", &mut cle)
        .map_err(|_| anyhow!("dérivation de la clé"))?;
    let mut nonce = [0u8; 12];
    derivation
        .expand(b"Content-Encoding: nonce\0", &mut nonce)
        .map_err(|_| anyhow!("dérivation du nonce"))?;

    // Un seul enregistrement : le contenu suivi du délimiteur 0x02 (dernier enregistrement).
    let mut clair = contenu.to_vec();
    clair.push(2);
    let chiffre = Aes128Gcm::new_from_slice(&cle)
        .map_err(|_| anyhow!("clé AES invalide"))?
        .encrypt((&nonce).into(), clair.as_slice())
        .map_err(|_| anyhow!("chiffrement impossible"))?;

    let mut corps = Vec::with_capacity(16 + 4 + 1 + 65 + chiffre.len());
    corps.extend_from_slice(sel);
    corps.extend_from_slice(&TAILLE_ENREGISTREMENT.to_be_bytes());
    corps.push(65);
    corps.extend_from_slice(publique_relais.as_bytes());
    corps.extend_from_slice(&chiffre);
    Ok(corps)
}

/// Chiffre `contenu` avec une clé éphémère et un sel neufs.
pub fn chiffrer(contenu: &[u8], p256dh: &[u8], auth: &[u8]) -> anyhow::Result<Vec<u8>> {
    let cle = SecretKey::random(&mut OsRng);
    let mut sel = [0u8; 16];
    OsRng.fill_bytes(&mut sel);
    chiffrer_avec(contenu, p256dh, auth, &cle, &sel)
}

/// Déchiffre un corps `aes128gcm` comme le ferait le navigateur de l'abonné (clé privée
/// `ua_privee`, clé publique `ua_publique`, secret `auth`). Sert aux essais : le relais, lui,
/// ne fait que chiffrer.
#[doc(hidden)]
pub fn dechiffrer(
    corps: &[u8],
    ua_privee: &[u8],
    ua_publique: &[u8],
    auth: &[u8],
) -> anyhow::Result<Vec<u8>> {
    if corps.len() < 16 + 4 + 1 + 65 + 17 || corps[20] != 65 {
        bail!("corps aes128gcm invalide");
    }
    let sel = &corps[..16];
    let publique_relais = &corps[21..86];
    let secret = diffie_hellman(
        SecretKey::from_slice(ua_privee)
            .map_err(|_| anyhow!("clé privée invalide"))?
            .to_nonzero_scalar(),
        PublicKey::from_sec1_bytes(publique_relais)
            .map_err(|_| anyhow!("clé du relais invalide"))?
            .as_affine(),
    );
    let mut info = b"WebPush: info\0".to_vec();
    info.extend_from_slice(ua_publique);
    info.extend_from_slice(publique_relais);
    let mut entree = [0u8; 32];
    Hkdf::<Sha256>::new(Some(auth), secret.raw_secret_bytes())
        .expand(&info, &mut entree)
        .map_err(|_| anyhow!("dérivation"))?;
    let derivation = Hkdf::<Sha256>::new(Some(sel), &entree);
    let mut cle = [0u8; 16];
    derivation
        .expand(b"Content-Encoding: aes128gcm\0", &mut cle)
        .map_err(|_| anyhow!("dérivation"))?;
    let mut nonce = [0u8; 12];
    derivation
        .expand(b"Content-Encoding: nonce\0", &mut nonce)
        .map_err(|_| anyhow!("dérivation"))?;
    let mut clair = Aes128Gcm::new_from_slice(&cle)
        .map_err(|_| anyhow!("clé AES invalide"))?
        .decrypt((&nonce).into(), &corps[86..])
        .map_err(|_| anyhow!("déchiffrement impossible"))?;
    if clair.pop() != Some(2) {
        bail!("délimiteur du dernier enregistrement absent");
    }
    Ok(clair)
}

/// Clés VAPID du relais : elles l'identifient auprès des services de push, et lient chaque
/// abonnement à ce relais (un autre serveur ne peut pas pousser vers lui).
pub struct Vapid {
    signature: SigningKey,
    publique: Vec<u8>,
}

impl Vapid {
    pub fn generer() -> Self {
        Self::depuis_secret(SecretKey::random(&mut OsRng))
    }

    fn depuis_secret(secret: SecretKey) -> Self {
        let publique = secret.public_key().to_encoded_point(false).as_bytes().to_vec();
        Self {
            signature: SigningKey::from(&secret),
            publique,
        }
    }

    /// Relit la clé privée (32 octets) gardée par `secret_octets`.
    pub fn depuis_octets(octets: &[u8]) -> anyhow::Result<Self> {
        let secret = SecretKey::from_slice(octets).map_err(|_| anyhow!("clé VAPID invalide"))?;
        Ok(Self::depuis_secret(secret))
    }

    pub fn secret_octets(&self) -> Vec<u8> {
        self.signature.to_bytes().to_vec()
    }

    /// Clé publique (65 octets, point non compressé) : ce que le navigateur reçoit pour
    /// s'abonner (`applicationServerKey`).
    pub fn publique(&self) -> &[u8] {
        &self.publique
    }

    pub fn publique_base64url(&self) -> String {
        base64url(&self.publique)
    }

    /// Jeton JWT ES256 pour un service de push (`audience` : son origine), valable jusqu'à
    /// `expire` ; `sujet` : adresse du relais (`https://…`, pas d'adresse e-mail).
    pub fn jwt(&self, audience: &str, sujet: &str, expire: u64) -> String {
        let entete = base64url(br#"{"typ":"JWT","alg":"ES256"}"#);
        let revendications = base64url(
            serde_json::json!({ "aud": audience, "exp": expire, "sub": sujet })
                .to_string()
                .as_bytes(),
        );
        let signe = format!("{entete}.{revendications}");
        let signature: Signature = self.signature.sign(signe.as_bytes());
        format!("{signe}.{}", base64url(&signature.to_bytes()))
    }

    /// En-tête `Authorization` d'une requête de push (RFC 8292, schéma `vapid`).
    pub fn autorisation(&self, audience: &str, sujet: &str, maintenant: u64) -> String {
        format!(
            "vapid t={}, k={}",
            self.jwt(audience, sujet, maintenant + VALIDITE_JWT_S),
            self.publique_base64url()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::{VerifyingKey, signature::Verifier};

    /// Annexe A de la RFC 8291 : message « When I grow up, I want to be a watermelon ».
    const AS_PRIVEE: &str = "yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw";
    const AS_PUBLIQUE: &str =
        "BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8";
    const UA_PRIVEE: &str = "q1dXpw3UpT5VOmu_cf_v6ih07Aems3njxI-JWgLcM94";
    const UA_PUBLIQUE: &str =
        "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
    const AUTH: &str = "BTBZMqHH6r4Tts7J_aSIgg";
    const SEL: &str = "DGv6ra1nlYgDCS1FRnbzlw";
    const CORPS: &str = "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN";
    const MESSAGE: &str = "When I grow up, I want to be a watermelon";

    fn b(texte: &str) -> Vec<u8> {
        decoder_base64url(texte).unwrap()
    }

    #[test]
    fn vecteur_de_la_rfc_8291() {
        let cle = SecretKey::from_slice(&b(AS_PRIVEE)).unwrap();
        assert_eq!(
            base64url(cle.public_key().to_encoded_point(false).as_bytes()),
            AS_PUBLIQUE
        );
        let sel: [u8; 16] = b(SEL).try_into().unwrap();
        let corps =
            chiffrer_avec(MESSAGE.as_bytes(), &b(UA_PUBLIQUE), &b(AUTH), &cle, &sel).unwrap();
        assert_eq!(base64url(&corps), CORPS);
    }

    #[test]
    fn dechiffrement_du_vecteur_et_aller_retour() {
        let clair = dechiffrer(&b(CORPS), &b(UA_PRIVEE), &b(UA_PUBLIQUE), &b(AUTH)).unwrap();
        assert_eq!(clair, MESSAGE.as_bytes());

        let corps = chiffrer(b"{\"id\":\"1\"}", &b(UA_PUBLIQUE), &b(AUTH)).unwrap();
        assert_eq!(
            dechiffrer(&corps, &b(UA_PRIVEE), &b(UA_PUBLIQUE), &b(AUTH)).unwrap(),
            b"{\"id\":\"1\"}"
        );
        // Deux chiffrements du même contenu diffèrent (clé éphémère et sel neufs).
        assert_ne!(corps, chiffrer(b"{\"id\":\"1\"}", &b(UA_PUBLIQUE), &b(AUTH)).unwrap());
    }

    #[test]
    fn limites_du_contenu() {
        let plein = vec![b'x'; CONTENU_MAX];
        let corps = chiffrer(&plein, &b(UA_PUBLIQUE), &b(AUTH)).unwrap();
        assert_eq!(corps.len(), TAILLE_ENREGISTREMENT as usize);
        assert!(chiffrer(&vec![b'x'; CONTENU_MAX + 1], &b(UA_PUBLIQUE), &b(AUTH)).is_err());
        assert!(chiffrer(b"x", &b(UA_PUBLIQUE)[..64], &b(AUTH)).is_err());
        assert!(chiffrer(b"x", &b(UA_PUBLIQUE), &b(AUTH)[..15]).is_err());
        // Un point qui n'est pas sur la courbe est refusé.
        let mut faux = b(UA_PUBLIQUE);
        faux[64] ^= 1;
        assert!(chiffrer(b"x", &faux, &b(AUTH)).is_err());
    }

    #[test]
    fn jwt_vapid_verifiable() {
        let vapid = Vapid::generer();
        let jwt = vapid.jwt("https://fcm.googleapis.com", "https://ricochet.example", 1_900_000_000);
        let parties: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parties.len(), 3);
        assert_eq!(b(parties[0]), br#"{"typ":"JWT","alg":"ES256"}"#);
        let revendications: serde_json::Value = serde_json::from_slice(&b(parties[1])).unwrap();
        assert_eq!(revendications["aud"], "https://fcm.googleapis.com");
        assert_eq!(revendications["sub"], "https://ricochet.example");
        assert_eq!(revendications["exp"], 1_900_000_000u64);
        // Signature brute r || s de 64 octets, vérifiable avec la clé publique annoncée.
        let signature = Signature::from_slice(&b(parties[2])).unwrap();
        assert_eq!(b(parties[2]).len(), 64);
        let verification = VerifyingKey::from_sec1_bytes(vapid.publique()).unwrap();
        verification
            .verify(format!("{}.{}", parties[0], parties[1]).as_bytes(), &signature)
            .expect("signature valide");
        assert!(verification.verify(b"autre chose", &signature).is_err());
        assert_eq!(vapid.publique().len(), 65);
        assert_eq!(vapid.publique()[0], 4);

        let en_tete = vapid.autorisation("https://x", "https://y", 1000);
        assert!(en_tete.starts_with("vapid t="));
        assert!(en_tete.ends_with(&format!(", k={}", vapid.publique_base64url())));
        let exp = {
            let jeton = en_tete.trim_start_matches("vapid t=").split(',').next().unwrap();
            let rev: serde_json::Value =
                serde_json::from_slice(&b(jeton.split('.').nth(1).unwrap())).unwrap();
            rev["exp"].as_u64().unwrap()
        };
        assert!(exp - 1000 < 24 * 3600, "expiration sous 24 h");
    }

    #[test]
    fn cle_vapid_persistante() {
        let vapid = Vapid::generer();
        let relue = Vapid::depuis_octets(&vapid.secret_octets()).unwrap();
        assert_eq!(vapid.publique(), relue.publique());
        assert!(Vapid::depuis_octets(&[0u8; 32]).is_err());
        assert!(Vapid::depuis_octets(b"court").is_err());
    }
}
