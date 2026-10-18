//! RSA signatures for logging in, made with `ring` (constant time, with blinding) rather than
//! the `rsa` crate, whose private-key operations can leak the key through timing
//! (RUSTSEC-2023-0071, the "Marvin attack"). The `rsa` crate still reads keys and checks
//! servers' signatures, which only use public keys.
//!
//! Two cases are left to the `rsa` crate because `ring` can't do them: SHA-1 signatures
//! (`ssh-rsa`, for servers older than 2016, only with legacy algorithms switched on) and keys
//! shorter than 2048 bits.

use rsa::pkcs1::EncodeRsaPrivateKey;
use russh::keys::agent::AgentIdentity;
use russh::keys::{HashAlg, PrivateKey};

pub struct RingRsa {
    key: ring::signature::RsaKeyPair,
}

#[derive(Debug)]
pub struct SignError(pub String);

impl From<russh::SendError> for SignError {
    fn from(e: russh::SendError) -> Self {
        SignError(e.to_string())
    }
}

impl RingRsa {
    /// `None` when the key is not RSA, or one `ring` doesn't take (under 2048 bits).
    pub fn new(key: &PrivateKey) -> Option<Self> {
        let rsa = key.key_data().rsa()?;
        let private = rsa::RsaPrivateKey::try_from(rsa).ok()?;
        let der = private.to_pkcs1_der().ok()?;
        let key = ring::signature::RsaKeyPair::from_der(der.as_bytes()).ok()?;
        Some(Self { key })
    }

    /// An `rsa-sha2-256` or `rsa-sha2-512` signature of `data`: the algorithm name and the
    /// signature bytes.
    pub fn sign(&self, hash: HashAlg, data: &[u8]) -> Result<(&'static str, Vec<u8>), SignError> {
        let (name, encoding): (_, &'static dyn ring::signature::RsaEncoding) = match hash {
            HashAlg::Sha256 => ("rsa-sha2-256", &ring::signature::RSA_PKCS1_SHA256),
            HashAlg::Sha512 => ("rsa-sha2-512", &ring::signature::RSA_PKCS1_SHA512),
            _ => return Err(SignError(format!("unsupported hash {hash:?}"))),
        };
        let mut signature = vec![0; self.key.public().modulus_len()];
        self.key
            .sign(
                encoding,
                &ring::rand::SystemRandom::new(),
                data,
                &mut signature,
            )
            .map_err(|_| SignError("RSA signing failed".into()))?;
        Ok((name, signature))
    }
}

/// Appends an SSH string (length-prefixed bytes).
fn put_string(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    out.extend_from_slice(bytes);
}

impl russh::Signer for RingRsa {
    type Error = SignError;

    /// russh hands over the data to sign and expects it back with the signature blob appended,
    /// as an agent would answer.
    #[allow(clippy::manual_async_fn)]
    fn auth_sign(
        &mut self,
        _key: &AgentIdentity,
        hash_alg: Option<HashAlg>,
        to_sign: Vec<u8>,
    ) -> impl Future<Output = Result<Vec<u8>, Self::Error>> + Send {
        let result = hash_alg
            .ok_or_else(|| SignError("SHA-1 signatures are not made here".into()))
            .and_then(|hash| self.sign(hash, &to_sign))
            .map(|(name, signature)| {
                let mut blob = Vec::with_capacity(8 + name.len() + signature.len());
                put_string(&mut blob, name.as_bytes());
                put_string(&mut blob, &signature);
                let mut out = to_sign;
                put_string(&mut out, &blob);
                out
            });
        async move { result }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh::keys::ssh_key::{Algorithm, Signature};
    use signature::Verifier;

    fn keygen(dir: &std::path::Path, bits: &str) -> PrivateKey {
        let path = dir.join(format!("rsa{bits}"));
        let ok = std::process::Command::new("ssh-keygen")
            .args(["-q", "-t", "rsa", "-b", bits, "-N", "", "-f"])
            .arg(&path)
            .status()
            .unwrap()
            .success();
        assert!(ok);
        PrivateKey::from_openssh(std::fs::read_to_string(path).unwrap()).unwrap()
    }

    /// Signatures made with ring check out with the key's public half (verified by another
    /// implementation), in the format servers expect.
    #[test]
    fn ring_signatures_verify() {
        let dir = tempfile::tempdir().unwrap();
        let key = keygen(dir.path(), "3072");
        let signer = RingRsa::new(&key).expect("ring takes a 3072-bit key");
        let data = b"session id and userauth request";
        for (hash, name) in [
            (HashAlg::Sha256, "rsa-sha2-256"),
            (HashAlg::Sha512, "rsa-sha2-512"),
        ] {
            let (alg, bytes) = signer.sign(hash, data).unwrap();
            assert_eq!(alg, name);
            let signature = Signature::new(Algorithm::Rsa { hash: Some(hash) }, bytes).unwrap();
            Verifier::verify(key.public_key(), data, &signature).unwrap();
        }
        // Too short for ring: the caller falls back.
        assert!(RingRsa::new(&keygen(dir.path(), "1024")).is_none());
    }
}
