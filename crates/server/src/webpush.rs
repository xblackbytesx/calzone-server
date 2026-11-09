//! WebPush message encryption (RFC 8291, `aes128gcm` from RFC 8188), taken
//! from wordwarz-server. The phone's UnifiedPush connector hands over a
//! P-256 public key and an auth secret with its endpoint; messages
//! encrypted to them can be read by that phone only, so the push server
//! relays ciphertext (decision 16). The app drops anything that does not
//! decrypt, so knowing an endpoint is not enough to fake a push.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes128Gcm, Nonce};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hkdf::Hkdf;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use p256::{PublicKey, SecretKey};
use sha2::Sha256;

/// Record size written in the header; one record always fits our messages.
const RECORD_SIZE: u32 = 4096;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum KeyError {
    #[error("the push key is not a P-256 public key")]
    PublicKey,
    #[error("the push auth secret must be 16 bytes")]
    Auth,
}

/// A device's keys, as the connector gives them (base64url, no padding).
#[derive(Clone, Debug)]
pub struct DeviceKeys {
    public: PublicKey,
    auth: [u8; 16],
}

impl DeviceKeys {
    pub fn parse(pub_key: &str, auth: &str) -> Result<DeviceKeys, KeyError> {
        let p = URL_SAFE_NO_PAD.decode(pub_key.trim().trim_end_matches('=')).map_err(|_| KeyError::PublicKey)?;
        if p.len() != 65 {
            return Err(KeyError::PublicKey);
        }
        let public = PublicKey::from_sec1_bytes(&p).map_err(|_| KeyError::PublicKey)?;
        let a = URL_SAFE_NO_PAD.decode(auth.trim().trim_end_matches('=')).map_err(|_| KeyError::Auth)?;
        let auth: [u8; 16] = a.try_into().map_err(|_| KeyError::Auth)?;
        Ok(DeviceKeys { public, auth })
    }
}

fn random<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).expect("OS randomness");
    b
}

/// Encrypts `plaintext` for `keys` with a fresh server key and salt: the
/// body to POST with `Content-Encoding: aes128gcm`.
pub fn encrypt(keys: &DeviceKeys, plaintext: &[u8]) -> Vec<u8> {
    let server = loop {
        if let Ok(k) = SecretKey::from_slice(&random::<32>()) {
            break k;
        }
    };
    encrypt_with(keys, plaintext, &server, random::<16>())
}

/// The deterministic core of [`encrypt`] (the RFC's test vector pins it).
fn encrypt_with(keys: &DeviceKeys, plaintext: &[u8], server: &SecretKey, salt: [u8; 16]) -> Vec<u8> {
    let ua_public = keys.public.to_encoded_point(false);
    let as_public = server.public_key().to_encoded_point(false);
    let shared = p256::ecdh::diffie_hellman(server.to_nonzero_scalar(), keys.public.as_affine());

    // IKM = HKDF(auth_secret, ecdh_secret, "WebPush: info" || 0 || ua_public || as_public)
    let mut key_info = b"WebPush: info\0".to_vec();
    key_info.extend_from_slice(ua_public.as_bytes());
    key_info.extend_from_slice(as_public.as_bytes());
    let mut ikm = [0u8; 32];
    Hkdf::<Sha256>::new(Some(&keys.auth), shared.raw_secret_bytes())
        .expand(&key_info, &mut ikm)
        .expect("32 bytes is a valid HKDF length");

    let prk = Hkdf::<Sha256>::new(Some(&salt), &ikm);
    let mut cek = [0u8; 16];
    let mut nonce = [0u8; 12];
    prk.expand(b"Content-Encoding: aes128gcm\0", &mut cek).expect("valid length");
    prk.expand(b"Content-Encoding: nonce\0", &mut nonce).expect("valid length");

    // One record: the plaintext and the last-record delimiter 0x02.
    let mut record = plaintext.to_vec();
    record.push(2);
    let cipher = Aes128Gcm::new_from_slice(&cek).expect("16-byte key");
    let sealed = cipher.encrypt(Nonce::from_slice(&nonce), record.as_slice()).expect("AES-GCM encrypts");

    let mut body = Vec::with_capacity(16 + 4 + 1 + 65 + sealed.len());
    body.extend_from_slice(&salt);
    body.extend_from_slice(&RECORD_SIZE.to_be_bytes());
    body.push(as_public.as_bytes().len() as u8);
    body.extend_from_slice(as_public.as_bytes());
    body.extend_from_slice(&sealed);
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(s: &str) -> Vec<u8> {
        URL_SAFE_NO_PAD.decode(s.replace([' ', '\n'], "")).unwrap()
    }

    /// RFC 8291, Section 5 and Appendix A: the exact bytes.
    #[test]
    fn rfc8291_test_vector() {
        let keys = DeviceKeys::parse(
            "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4",
            "BTBZMqHH6r4Tts7J_aSIgg",
        )
        .unwrap();
        let server = SecretKey::from_slice(&b64("yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw")).unwrap();
        let salt: [u8; 16] = b64("DGv6ra1nlYgDCS1FRnbzlw").try_into().unwrap();
        let body = encrypt_with(&keys, b"When I grow up, I want to be a watermelon", &server, salt);
        let header = b64(
            "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z 9KsN6nGRTbVYI_c7VJSPQTBtkgcy27ml mlMoZIIgDll6e3vCYLocInmYWAmS6Tlz AC8wEqKK6PBru3jl7A8",
        );
        let cipher = b64("8pfeW0KbunFT06SuDKoJH9Ql87S1QUrd irN6GcG7sFz1y1sqLgVi1VhjVkHsUoEs bI_0LpXMuGvnzQ");
        assert_eq!(header.len(), 86);
        assert_eq!(&body[..86], header.as_slice());
        assert_eq!(&body[86..], cipher.as_slice());
    }

    #[test]
    fn fresh_keys_each_time_and_bad_keys_refused() {
        let keys = DeviceKeys::parse(
            "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4",
            "BTBZMqHH6r4Tts7J_aSIgg",
        )
        .unwrap();
        assert_ne!(encrypt(&keys, b"x"), encrypt(&keys, b"x"));
        assert_eq!(DeviceKeys::parse("AAAA", "BTBZMqHH6r4Tts7J_aSIgg").unwrap_err(), KeyError::PublicKey);
        assert_eq!(
            DeviceKeys::parse(
                "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4",
                "AAAA"
            )
            .unwrap_err(),
            KeyError::Auth
        );
    }
}
