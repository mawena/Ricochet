//! Jetons secrets : création, empreinte et comparaison à temps constant.

use std::io::Read;

use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// Crée un jeton aléatoire de 256 bits, en hexadécimal.
pub fn nouveau() -> std::io::Result<String> {
    let mut octets = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut octets)?;
    Ok(hex::encode(octets))
}

/// Empreinte SHA-256 d'un jeton, en hexadécimal : c'est elle qu'on écrit dans la configuration.
pub fn empreinte(jeton: &str) -> String {
    hex::encode(Sha256::digest(jeton.as_bytes()))
}

/// Vérifie qu'un jeton correspond à une empreinte, sans fuite d'information par le temps.
pub fn verifier(jeton: &str, empreinte_attendue: &str) -> bool {
    let calculee = empreinte(jeton);
    calculee
        .as_bytes()
        .ct_eq(empreinte_attendue.to_ascii_lowercase().as_bytes())
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empreinte_connue() {
        assert_eq!(
            empreinte("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn verification() {
        let jeton = nouveau().unwrap();
        assert_eq!(jeton.len(), 64);
        assert_ne!(jeton, nouveau().unwrap());
        assert!(verifier(&jeton, &empreinte(&jeton)));
        assert!(verifier(&jeton, &empreinte(&jeton).to_uppercase()));
        assert!(!verifier("autre", &empreinte(&jeton)));
    }
}
