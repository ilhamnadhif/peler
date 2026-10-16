//! Loading and saving remote text files for the built-in editor.
//!
//! Saving never leaves a half-written file behind when it can avoid it: the new content is
//! written to a temporary file next to the original and swapped in by renames (SFTP has no
//! rename-over, so the original is first moved aside and only deleted once the new file is in
//! place). Where that would change the file (a symlink, a file owned by someone else, a folder
//! we can't create files in), the file is rewritten in place instead, as `vim` or an SCP editor
//! would.

use russh_sftp::client::SftpSession;
use russh_sftp::protocol::{FileAttributes, FileType, OpenFlags, StatusCode};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::entry::{remote_join, remote_name, remote_parent};
use super::remote::{describe, random_suffix, replace_file, same_owner};
use crate::i18n::{t, tf};

/// Largest file the editor opens (the editor lays out the whole text).
pub const MAX_EDIT_BYTES: u64 = 2 * 1024 * 1024;

/// What the file looked like on the server, to notice changes made by someone else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    pub size: Option<u64>,
    pub mtime: Option<u32>,
}

impl Stamp {
    fn of(attrs: &FileAttributes) -> Self {
        Self {
            size: attrs.size,
            mtime: attrs.mtime,
        }
    }
}

/// A file opened for editing: text with `\n` line endings, plus how to write it back.
#[derive(Clone, Debug)]
pub struct Loaded {
    pub text: String,
    pub crlf: bool,
    pub bom: bool,
    pub stamp: Stamp,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoadError {
    TooLarge(u64),
    NotText,
    Failed(String),
}

impl LoadError {
    pub fn message(&self) -> String {
        match self {
            LoadError::TooLarge(size) => tf(
                "The file is too large to edit here ({} MB; the limit is {} MB). Download it instead.",
                &[
                    &format!("{:.1}", *size as f64 / 1_048_576.0),
                    &(MAX_EDIT_BYTES / 1_048_576).to_string(),
                ],
            ),
            LoadError::NotText => {
                t("This is not a text file (or not UTF-8). Download it instead.").into()
            }
            LoadError::Failed(e) => e.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SaveError {
    /// The file changed on the server since it was opened (or last saved).
    Changed,
    /// The file is no longer on the server (saving without a stamp creates it again).
    Deleted,
    Failed(String),
}

/// Splits file bytes into editable text (line endings and BOM remembered separately).
pub fn decode(data: &[u8]) -> Result<(String, bool, bool), LoadError> {
    let (bom, data) = match data.strip_prefix(b"\xEF\xBB\xBF") {
        Some(rest) => (true, rest),
        None => (false, data),
    };
    if data.contains(&0) {
        return Err(LoadError::NotText);
    }
    let text = std::str::from_utf8(data).map_err(|_| LoadError::NotText)?;
    let crlf = text.contains("\r\n");
    let text = if crlf {
        text.replace("\r\n", "\n")
    } else {
        text.to_string()
    };
    Ok((text, crlf, bom))
}

/// The bytes to write for edited text, with the file's original line endings and BOM.
pub fn encode(text: &str, crlf: bool, bom: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() + 3);
    if bom {
        out.extend_from_slice(b"\xEF\xBB\xBF");
    }
    if crlf {
        out.extend_from_slice(text.replace("\r\n", "\n").replace('\n', "\r\n").as_bytes());
    } else {
        out.extend_from_slice(text.as_bytes());
    }
    out
}

fn failed(e: russh_sftp::client::error::Error) -> String {
    describe(&e)
}

pub async fn load(sftp: &SftpSession, path: &str) -> Result<Loaded, LoadError> {
    let attrs = sftp
        .metadata(path)
        .await
        .map_err(|e| LoadError::Failed(failed(e)))?;
    if attrs.file_type() == FileType::Dir {
        return Err(LoadError::Failed(t("This is a folder.").into()));
    }
    let size = attrs.size.unwrap_or(0);
    if size > MAX_EDIT_BYTES {
        return Err(LoadError::TooLarge(size));
    }
    let mut file = sftp
        .open(path)
        .await
        .map_err(|e| LoadError::Failed(failed(e)))?;
    // Read at most one byte past the limit, in case the file grew since the stat.
    let mut data = Vec::with_capacity(size as usize);
    (&mut file)
        .take(MAX_EDIT_BYTES + 1)
        .read_to_end(&mut data)
        .await
        .map_err(|e| LoadError::Failed(e.to_string()))?;
    let _ = file.shutdown().await;
    if data.len() as u64 > MAX_EDIT_BYTES {
        return Err(LoadError::TooLarge(data.len() as u64));
    }
    let (text, crlf, bom) = decode(&data)?;
    Ok(Loaded {
        text,
        crlf,
        bom,
        stamp: Stamp::of(&attrs),
    })
}

pub async fn stamp(sftp: &SftpSession, path: &str) -> Result<Stamp, String> {
    sftp.metadata(path)
        .await
        .map(|a| Stamp::of(&a))
        .map_err(failed)
}

/// Writes `data` to `path`. Unless `expected` is `None` (the user chose to overwrite), refuses
/// when the file no longer matches `expected`. Returns the file's new stamp.
pub async fn save(
    sftp: &SftpSession,
    path: &str,
    data: &[u8],
    expected: Option<Stamp>,
) -> Result<Stamp, SaveError> {
    let attrs = match sftp.metadata(path).await {
        Ok(a) => a,
        Err(russh_sftp::client::error::Error::Status(st))
            if st.status_code == StatusCode::NoSuchFile =>
        {
            if expected.is_some() {
                return Err(SaveError::Deleted);
            }
            create_new(sftp, path, data)
                .await
                .map_err(SaveError::Failed)?;
            return stamp(sftp, path).await.map_err(SaveError::Failed);
        }
        Err(e) => return Err(SaveError::Failed(failed(e))),
    };
    if expected.is_some_and(|s| s != Stamp::of(&attrs)) {
        return Err(SaveError::Changed);
    }
    let is_link = sftp
        .symlink_metadata(path)
        .await
        .is_ok_and(|l| l.file_type() == FileType::Symlink);
    if !is_link
        && replace_via_temp(sftp, path, data, &attrs)
            .await
            .map_err(SaveError::Failed)?
    {
        return stamp(sftp, path).await.map_err(SaveError::Failed);
    }
    write_in_place(sftp, path, data)
        .await
        .map_err(SaveError::Failed)?;
    stamp(sftp, path).await.map_err(SaveError::Failed)
}

/// Writes the new content next to the file and swaps it in. `Ok(false)` when this can't be
/// done without changing the file's owner or needing rights we don't have (nothing was
/// changed; the caller writes in place instead).
async fn replace_via_temp(
    sftp: &SftpSession,
    path: &str,
    data: &[u8],
    original: &FileAttributes,
) -> Result<bool, String> {
    let dir = remote_parent(path);
    let name = remote_name(path).to_string();
    let tmp = remote_join(&dir, &format!(".{name}.peler-edit-{}", random_suffix()));
    let mode = original.permissions.map(|p| p & 0o7777);
    let attrs = FileAttributes {
        permissions: mode,
        ..Default::default()
    };
    let Ok(mut file) = sftp
        .open_with_flags_and_attributes(
            tmp.clone(),
            OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE,
            attrs,
        )
        .await
    else {
        return Ok(false); // can't create files here
    };
    // A new file is ours; if the original belongs to someone else, swapping would change that.
    let created = file.metadata().await.ok();
    if !same_owner(created.as_ref(), original) {
        let _ = file.shutdown().await;
        let _ = sftp.remove_file(tmp).await;
        return Ok(false);
    }
    let written = async {
        file.write_all(data).await.map_err(|e| e.to_string())?;
        file.sync_all().await.map_err(failed)?;
        file.shutdown().await.map_err(|e| e.to_string())?;
        // The mode given at creation is reduced by the server's umask.
        if let Some(mode) = mode {
            let attrs = FileAttributes {
                permissions: Some(mode),
                ..Default::default()
            };
            let _ = sftp.set_metadata(tmp.clone(), attrs).await;
        }
        // The temporary file must hold everything before the original is touched.
        match sftp.metadata(tmp.clone()).await {
            Ok(a) if a.size == Some(data.len() as u64) => Ok(()),
            Ok(_) => Err(t("the server stored an incomplete file").to_string()),
            Err(e) => Err(failed(e)),
        }
    }
    .await;
    if let Err(e) = written {
        let _ = sftp.remove_file(tmp).await;
        return Err(e);
    }
    match replace_file(sftp, &tmp, path).await {
        Ok(true) => Ok(true),
        // e.g. a sticky folder: the original is untouched, it is written in place.
        Ok(false) => {
            let _ = sftp.remove_file(tmp).await;
            Ok(false)
        }
        Err(e) => {
            let _ = sftp.remove_file(tmp).await;
            Err(e)
        }
    }
}

/// Writes a file that doesn't exist (any more); refuses to replace one created meanwhile.
async fn create_new(sftp: &SftpSession, path: &str, data: &[u8]) -> Result<(), String> {
    let mut file = sftp
        .open_with_flags(
            path,
            OpenFlags::CREATE | OpenFlags::EXCLUDE | OpenFlags::WRITE,
        )
        .await
        .map_err(failed)?;
    file.write_all(data).await.map_err(|e| e.to_string())?;
    file.sync_all().await.map_err(failed)?;
    file.shutdown().await.map_err(|e| e.to_string())
}

async fn write_in_place(sftp: &SftpSession, path: &str, data: &[u8]) -> Result<(), String> {
    let mut file = sftp
        .open_with_flags(path, OpenFlags::WRITE | OpenFlags::TRUNCATE)
        .await
        .map_err(failed)?;
    file.write_all(data).await.map_err(|e| e.to_string())?;
    file.sync_all().await.map_err(failed)?;
    file.shutdown().await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_round_trip_keeps_line_endings_and_bom() {
        let (text, crlf, bom) = decode(b"\xEF\xBB\xBFa: 1\r\nb: 2\r\n").unwrap();
        assert_eq!((text.as_str(), crlf, bom), ("a: 1\nb: 2\n", true, true));
        assert_eq!(
            encode("a: 1\nb: 3\n", crlf, bom),
            b"\xEF\xBB\xBFa: 1\r\nb: 3\r\n"
        );
        let (text, crlf, bom) = decode(b"x\ny").unwrap();
        assert_eq!(encode(&text, crlf, bom), b"x\ny");
        assert_eq!(decode(b"ELF\0\x01"), Err(LoadError::NotText));
        assert_eq!(decode(b"\xff\xfe"), Err(LoadError::NotText));
    }
}
