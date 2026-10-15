//! Passphrase encryption for exported credentials: Argon2id derives a 256-bit key, and
//! XChaCha20-Poly1305 encrypts and authenticates the payload. All parameters needed to decrypt
//! are stored next to the ciphertext, so they can be raised later without breaking old files.

use anyhow::{Result, anyhow, bail};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use chacha20poly1305::aead::Aead;
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};

pub const KDF_ARGON2ID: &str = "argon2id";
pub const CIPHER_XCHACHA: &str = "xchacha20poly1305";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncryptedBlob {
    pub kdf: String,
    /// Argon2 memory cost in KiB.
    pub m_cost: u32,
    pub t_cost: u32,
    pub p_cost: u32,
    pub salt: String,
    pub cipher: String,
    pub nonce: String,
    pub ciphertext: String,
}

/// 64 MiB, 3 passes: a few hundred milliseconds on a laptop, expensive to brute-force.
const DEFAULT_PARAMS: (u32, u32, u32) = (64 * 1024, 3, 1);

fn random_bytes<const N: usize>() -> Result<[u8; N]> {
    let mut buf = [0u8; N];
    getrandom::fill(&mut buf).map_err(|e| anyhow!("system RNG failed: {e}"))?;
    Ok(buf)
}

fn derive_key(passphrase: &str, salt: &[u8], m: u32, t: u32, p: u32) -> Result<[u8; 32]> {
    let params = Params::new(m, t, p, Some(32)).map_err(|e| anyhow!("argon2 params: {e}"))?;
    let mut key = [0u8; 32];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(passphrase.as_bytes(), salt, &mut key)
        .map_err(|e| anyhow!("argon2: {e}"))?;
    Ok(key)
}

pub fn encrypt(passphrase: &str, plaintext: &[u8]) -> Result<EncryptedBlob> {
    encrypt_with(passphrase, plaintext, DEFAULT_PARAMS)
}

pub fn encrypt_with(
    passphrase: &str,
    plaintext: &[u8],
    (m, t, p): (u32, u32, u32),
) -> Result<EncryptedBlob> {
    if passphrase.is_empty() {
        bail!("{}", crate::i18n::t("The passphrase must not be empty."));
    }
    let salt: [u8; 16] = random_bytes()?;
    let nonce: [u8; 24] = random_bytes()?;
    let key = derive_key(passphrase, &salt, m, t, p)?;
    let cipher = XChaCha20Poly1305::new(&key.into());
    let ciphertext = cipher
        .encrypt(&XNonce::from(nonce), plaintext)
        .map_err(|_| anyhow!("encryption failed"))?;
    Ok(EncryptedBlob {
        kdf: KDF_ARGON2ID.into(),
        m_cost: m,
        t_cost: t,
        p_cost: p,
        salt: B64.encode(salt),
        cipher: CIPHER_XCHACHA.into(),
        nonce: B64.encode(nonce),
        ciphertext: B64.encode(ciphertext),
    })
}

pub fn decrypt(passphrase: &str, blob: &EncryptedBlob) -> Result<Vec<u8>> {
    if blob.kdf != KDF_ARGON2ID || blob.cipher != CIPHER_XCHACHA {
        bail!("Unsupported encryption ({} / {}).", blob.kdf, blob.cipher);
    }
    // Refuse absurd parameters from a crafted file (memory above 1 GiB, too many passes).
    if blob.m_cost > 1024 * 1024 || blob.t_cost > 64 || blob.p_cost > 16 {
        bail!("Encryption parameters in the file are out of range.");
    }
    let salt = B64
        .decode(&blob.salt)
        .map_err(|_| anyhow!("Malformed salt."))?;
    let nonce: [u8; 24] = B64
        .decode(&blob.nonce)
        .ok()
        .and_then(|n| n.try_into().ok())
        .ok_or_else(|| anyhow!("Malformed nonce."))?;
    let ciphertext = B64
        .decode(&blob.ciphertext)
        .map_err(|_| anyhow!("Malformed ciphertext."))?;
    let key = derive_key(passphrase, &salt, blob.m_cost, blob.t_cost, blob.p_cost)?;
    XChaCha20Poly1305::new(&key.into())
        .decrypt(&XNonce::from(nonce), ciphertext.as_slice())
        .map_err(|_| {
            anyhow!(
                "{}",
                crate::i18n::t("Wrong passphrase, or the file has been modified.")
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_decrypt_and_tamper() {
        let blob = encrypt_with("pass", b"secret data", (1024, 1, 1)).unwrap();
        assert_eq!(decrypt("pass", &blob).unwrap(), b"secret data");
        assert!(decrypt("nope", &blob).is_err());
        let mut bad = blob.clone();
        let mut ct = B64.decode(&bad.ciphertext).unwrap();
        ct[0] ^= 1;
        bad.ciphertext = B64.encode(ct);
        assert!(decrypt("pass", &bad).is_err());
        assert!(encrypt("", b"x").is_err());
        let other = encrypt_with("pass", b"secret data", (1024, 1, 1)).unwrap();
        assert_ne!(other.salt, blob.salt);
        assert_ne!(other.nonce, blob.nonce);
    }
}
