//! Codes à usage unique basés sur le temps (TOTP, RFC 6238) : HMAC-SHA1, 6 chiffres, 30 s.
//! C'est le réglage que comprennent toutes les applications d'authentification.

use data_encoding::BASE32_NOPAD;
use hmac::{Hmac, KeyInit, Mac};
use sha1::Sha1;

/// Durée d'un pas de temps, en secondes.
pub const PAS_S: u64 = 30;
/// Nombre de pas acceptés avant et après le pas courant (décalage d'horloge du téléphone).
const TOLERANCE: u64 = 1;

/// Nouveau secret de 160 bits, encodé en base32 (la forme que l'on tape dans l'application).
pub fn nouveau_secret() -> std::io::Result<String> {
    Ok(BASE32_NOPAD.encode(&crate::jeton::octets_aleatoires::<20>()?))
}

/// Lien `otpauth://` à transformer en QR code pour l'application d'authentification.
pub fn lien(secret: &str, compte: &str) -> String {
    let compte: String = compte
        .bytes()
        .map(|o| {
            if o.is_ascii_alphanumeric() || b"-_.".contains(&o) {
                (o as char).to_string()
            } else {
                format!("%{o:02X}")
            }
        })
        .collect();
    format!("otpauth://totp/Ricochet:{compte}?secret={secret}&issuer=Ricochet")
}

/// Code du pas de temps `pas`.
fn code(cle: &[u8], pas: u64) -> u32 {
    let mut mac = Hmac::<Sha1>::new_from_slice(cle).expect("HMAC accepte toute longueur de clé");
    mac.update(&pas.to_be_bytes());
    let empreinte = mac.finalize().into_bytes();
    let decalage = (empreinte[19] & 0x0f) as usize;
    let valeur = u32::from_be_bytes(empreinte[decalage..decalage + 4].try_into().unwrap());
    (valeur & 0x7fff_ffff) % 1_000_000
}

/// Vérifie `saisi` à l'instant `maintenant` (secondes Unix). Renvoie le pas reconnu, que
/// l'appelant mémorise : un pas déjà utilisé (`<= dernier_pas`) est refusé, ce qui empêche de
/// rejouer un code intercepté.
pub fn verifier(secret: &str, saisi: &str, maintenant: u64, dernier_pas: u64) -> Option<u64> {
    let saisi = saisi.trim();
    if saisi.len() != 6 || !saisi.bytes().all(|o| o.is_ascii_digit()) {
        return None;
    }
    let saisi: u32 = saisi.parse().ok()?;
    let cle = BASE32_NOPAD.decode(secret.as_bytes()).ok()?;
    let courant = maintenant / PAS_S;
    (courant.saturating_sub(TOLERANCE)..=courant + TOLERANCE)
        .filter(|&pas| pas > dernier_pas)
        .find(|&pas| code(&cle, pas) == saisi)
}

/// Code attendu à l'instant donné, pour les essais.
#[cfg(test)]
pub(crate) fn code_pour(secret: &str, instant: u64) -> String {
    let cle = BASE32_NOPAD.decode(secret.as_bytes()).unwrap();
    format!("{:06}", code(&cle, instant / PAS_S))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Vecteurs de l'annexe B de la RFC 6238 (SHA-1), ramenés à 6 chiffres.
    #[test]
    fn vecteurs_rfc() {
        let cle = b"12345678901234567890";
        for (instant, attendu) in [
            (59, 287_082),
            (1_111_111_109, 81_804),
            (1_111_111_111, 50_471),
            (1_234_567_890, 5_924),
            (2_000_000_000, 279_037),
        ] {
            assert_eq!(code(cle, instant / PAS_S), attendu, "instant {instant}");
        }
    }

    #[test]
    fn verification_et_rejeu() {
        let secret = BASE32_NOPAD.encode(b"12345678901234567890");
        let t = 1_111_111_109;
        assert_eq!(verifier(&secret, "081804", t, 0), Some(t / PAS_S));
        // Un pas d'écart est toléré, deux non.
        assert!(verifier(&secret, "081804", t + PAS_S, 0).is_some());
        assert!(verifier(&secret, "081804", t + 2 * PAS_S, 0).is_none());
        // Le même code ne sert qu'une fois.
        assert!(verifier(&secret, "081804", t, t / PAS_S).is_none());
        assert!(verifier(&secret, "000000", t, 0).is_none());
        assert!(verifier(&secret, "81804", t, 0).is_none());
        assert!(verifier(&secret, "abcdef", t, 0).is_none());
    }

    #[test]
    fn secret_et_lien() {
        let secret = nouveau_secret().unwrap();
        assert_eq!(secret.len(), 32);
        assert_eq!(
            lien("ABC", "charles m"),
            "otpauth://totp/Ricochet:charles%20m?secret=ABC&issuer=Ricochet"
        );
    }
}
