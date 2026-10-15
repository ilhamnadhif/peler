//! Private key loading with a clear distinction between "needs a passphrase", "wrong
//! passphrase" and unsupported formats; plus our own known_hosts maintenance.

use std::path::Path;

use russh::keys::{self, PrivateKey, ssh_key};

#[derive(Debug, PartialEq, Eq)]
pub enum KeyLoadError {
    NeedsPassphrase,
    WrongPassphrase,
    Unsupported(String),
    Io(String),
}

impl std::fmt::Display for KeyLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeyLoadError::NeedsPassphrase => f.write_str("the key is encrypted and needs a passphrase"),
            KeyLoadError::WrongPassphrase => f.write_str("wrong passphrase"),
            KeyLoadError::Unsupported(e) => write!(f, "unsupported key format ({e})"),
            KeyLoadError::Io(e) => f.write_str(e),
        }
    }
}

/// Whether key text is passphrase-protected (OpenSSH, PEM `Proc-Type` and PKCS#8).
pub fn is_encrypted(text: &str) -> bool {
    if text.contains("-----BEGIN OPENSSH PRIVATE KEY-----") {
        return ssh_key::PrivateKey::from_openssh(text).map(|k| k.is_encrypted()).unwrap_or(false);
    }
    text.contains("-----BEGIN ENCRYPTED PRIVATE KEY-----") || text.contains("Proc-Type: 4,ENCRYPTED")
}

pub fn read_key_file(path: &Path) -> Result<String, KeyLoadError> {
    std::fs::read_to_string(path).map_err(|e| KeyLoadError::Io(format!("{}: {e}", path.display())))
}

/// Decodes private key text (OpenSSH, PEM PKCS#1/SEC1/PKCS#8; RSA, ECDSA, Ed25519).
pub fn decode_key(text: &str, passphrase: Option<&str>) -> Result<PrivateKey, KeyLoadError> {
    let encrypted = is_encrypted(text);
    let passphrase = passphrase.filter(|p| !p.is_empty());
    if encrypted && passphrase.is_none() {
        return Err(KeyLoadError::NeedsPassphrase);
    }
    // Passing a passphrase for an unencrypted PKCS#8 key makes decoding fail, so only pass it
    // when the key needs one.
    let pass = if encrypted { passphrase } else { None };
    if encrypted && text.contains("-----BEGIN EC PRIVATE KEY-----") {
        return Err(KeyLoadError::Unsupported(
            "encrypted legacy PEM EC key; convert it with `ssh-keygen -p -f <key>`".into(),
        ));
    }
    match keys::decode_secret_key(text, pass) {
        Ok(k) => Ok(k),
        Err(keys::Error::KeyIsEncrypted) => Err(KeyLoadError::NeedsPassphrase),
        Err(keys::Error::SshKey(ssh_key::Error::Crypto)) | Err(keys::Error::Unpad(_)) => {
            Err(KeyLoadError::WrongPassphrase)
        }
        Err(e) => {
            let s = format!("{e:?}");
            if encrypted && (s.contains("DecryptFailed") || s.contains("Pkcs1(") || s.contains("Der(")) {
                Err(KeyLoadError::WrongPassphrase)
            } else {
                Err(KeyLoadError::Unsupported(e.to_string()))
            }
        }
    }
}

/// Removes line `line` (1-based, as reported by russh's `KeyChanged`) from a known_hosts file.
pub fn forget_known_host_line(path: &Path, line: usize) -> std::io::Result<()> {
    let text = std::fs::read_to_string(path)?;
    let kept: Vec<&str> =
        text.lines().enumerate().filter(|(i, _)| i + 1 != line).map(|(_, l)| l).collect();
    let mut out = kept.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    crate::paths::write_private(path, out.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keygen(dir: &Path, name: &str, kind: &str, pass: &str) -> String {
        let path = dir.join(name);
        let ok = std::process::Command::new("ssh-keygen")
            .args(["-q", "-t", kind, "-N", pass, "-C", "test", "-f"])
            .arg(&path)
            .status()
            .unwrap()
            .success();
        assert!(ok);
        std::fs::read_to_string(path).unwrap()
    }

    #[test]
    fn decodes_keys_and_classifies_passphrase_errors() {
        let dir = tempfile::tempdir().unwrap();
        let plain = keygen(dir.path(), "ed", "ed25519", "");
        assert!(!is_encrypted(&plain));
        assert_eq!(decode_key(&plain, Some("ignored")).unwrap().algorithm().as_str(), "ssh-ed25519");

        let ecdsa = keygen(dir.path(), "ec", "ecdsa", "");
        assert!(decode_key(&ecdsa, None).unwrap().algorithm().as_str().starts_with("ecdsa-sha2"));
        let rsa = keygen(dir.path(), "rsa", "rsa", "");
        assert!(decode_key(&rsa, None).unwrap().algorithm().is_rsa());

        let enc = keygen(dir.path(), "enc", "ed25519", "secret");
        assert!(is_encrypted(&enc));
        assert_eq!(decode_key(&enc, None).unwrap_err(), KeyLoadError::NeedsPassphrase);
        assert_eq!(decode_key(&enc, Some("nope")).unwrap_err(), KeyLoadError::WrongPassphrase);
        assert!(decode_key(&enc, Some("secret")).is_ok());
        assert!(matches!(decode_key("garbage", None), Err(KeyLoadError::Unsupported(_))));
    }

    #[test]
    fn forget_line() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("known_hosts");
        std::fs::write(&p, "a key1\nb key2\nc key3\n").unwrap();
        forget_known_host_line(&p, 2).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "a key1\nc key3\n");
    }
}
