//! Private key loading with a clear distinction between "needs a passphrase", "wrong
//! passphrase" and unsupported formats; plus our own known_hosts maintenance.

use std::path::Path;

use russh::keys::{self, PrivateKey, ssh_key};

use crate::i18n::{t, tf};

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
            KeyLoadError::NeedsPassphrase => {
                f.write_str(t("the key is encrypted and needs a passphrase"))
            }
            KeyLoadError::WrongPassphrase => f.write_str(t("wrong passphrase")),
            KeyLoadError::Unsupported(e) => f.write_str(&tf("unsupported key format ({})", &[e])),
            KeyLoadError::Io(e) => f.write_str(e),
        }
    }
}

/// Whether key text is passphrase-protected (OpenSSH, PEM `Proc-Type` and PKCS#8).
pub fn is_encrypted(text: &str) -> bool {
    if text.contains("-----BEGIN OPENSSH PRIVATE KEY-----") {
        return ssh_key::PrivateKey::from_openssh(text)
            .map(|k| k.is_encrypted())
            .unwrap_or(false);
    }
    text.contains("-----BEGIN ENCRYPTED PRIVATE KEY-----")
        || text.contains("Proc-Type: 4,ENCRYPTED")
}

/// What pasted key text is, for the connection editor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyCheck {
    /// A usable private key of this type ("Ed25519", "ECDSA", "RSA").
    Valid(String),
    /// A passphrase-protected private key (checked when connecting).
    Encrypted,
    /// A public key (`ssh-ed25519 AAAA…`) was pasted instead of the private one.
    PublicKey,
    Invalid,
}

/// Largest key text accepted from the editor (real keys are a few KB).
pub const MAX_KEY_TEXT: usize = 64 * 1024;

/// Cleans up pasted key text: no surrounding blank space or indentation, LF line endings, one
/// final newline. Blank lines inside the key (legacy PEM headers) are kept.
pub fn normalize_key_text(text: &str) -> String {
    let lines: Vec<&str> = text.trim().lines().map(str::trim).collect();
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

pub fn check_key_text(text: &str) -> KeyCheck {
    let text = normalize_key_text(text);
    if text.len() > MAX_KEY_TEXT {
        return KeyCheck::Invalid;
    }
    let first = text.split_whitespace().next().unwrap_or("");
    if first.starts_with("ssh-") || first.starts_with("ecdsa-") || first.starts_with("sk-") {
        return KeyCheck::PublicKey;
    }
    match decode_key(&text, None) {
        Ok(key) => {
            let kind = match key.algorithm() {
                ssh_key::Algorithm::Ed25519 => "Ed25519",
                ssh_key::Algorithm::Ecdsa { .. } => "ECDSA",
                ssh_key::Algorithm::Rsa { .. } => "RSA",
                _ => "SSH",
            };
            KeyCheck::Valid(kind.to_string())
        }
        Err(KeyLoadError::NeedsPassphrase) => KeyCheck::Encrypted,
        Err(_) => KeyCheck::Invalid,
    }
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
            if encrypted
                && (s.contains("DecryptFailed") || s.contains("Pkcs1(") || s.contains("Der("))
            {
                Err(KeyLoadError::WrongPassphrase)
            } else {
                Err(KeyLoadError::Unsupported(e.to_string()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pasted_key_text() {
        let dir = tempfile::tempdir().unwrap();
        let plain = keygen(dir.path(), "plain", "ed25519", "");
        let enc = keygen(dir.path(), "enc", "rsa", "secret");
        // Indented and with CRLF, as pasted from a chat or web page.
        let messy: String = plain.lines().map(|l| format!("   {l}\r\n")).collect();
        assert_eq!(normalize_key_text(&messy), plain);
        assert_eq!(check_key_text(&messy), KeyCheck::Valid("Ed25519".into()));
        assert_eq!(check_key_text(&enc), KeyCheck::Encrypted);
        assert_eq!(
            check_key_text("ssh-ed25519 AAAAC3Nza me@laptop"),
            KeyCheck::PublicKey
        );
        assert_eq!(check_key_text("hello"), KeyCheck::Invalid);

        // The classic PEM format ("BEGIN RSA PRIVATE KEY"), as Electerm and older tools show it.
        let pem_path = dir.path().join("pem");
        assert!(
            std::process::Command::new("ssh-keygen")
                .args(["-q", "-t", "rsa", "-b", "2048", "-m", "PEM", "-N", "", "-f"])
                .arg(&pem_path)
                .status()
                .unwrap()
                .success()
        );
        let pem = std::fs::read_to_string(&pem_path).unwrap();
        assert!(pem.starts_with("-----BEGIN RSA PRIVATE KEY-----"));
        assert_eq!(check_key_text(&pem), KeyCheck::Valid("RSA".into()));
    }

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
        assert_eq!(
            decode_key(&plain, Some("ignored"))
                .unwrap()
                .algorithm()
                .as_str(),
            "ssh-ed25519"
        );

        let ecdsa = keygen(dir.path(), "ec", "ecdsa", "");
        assert!(
            decode_key(&ecdsa, None)
                .unwrap()
                .algorithm()
                .as_str()
                .starts_with("ecdsa-sha2")
        );
        let rsa = keygen(dir.path(), "rsa", "rsa", "");
        assert!(decode_key(&rsa, None).unwrap().algorithm().is_rsa());

        let enc = keygen(dir.path(), "enc", "ed25519", "secret");
        assert!(is_encrypted(&enc));
        assert_eq!(
            decode_key(&enc, None).unwrap_err(),
            KeyLoadError::NeedsPassphrase
        );
        assert_eq!(
            decode_key(&enc, Some("nope")).unwrap_err(),
            KeyLoadError::WrongPassphrase
        );
        assert!(decode_key(&enc, Some("secret")).is_ok());
        assert!(matches!(
            decode_key("garbage", None),
            Err(KeyLoadError::Unsupported(_))
        ));
    }
}
