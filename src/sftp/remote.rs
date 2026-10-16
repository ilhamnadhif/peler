//! Remote file operations over an `SftpSession`.

use std::sync::Arc;

use russh_sftp::client::SftpSession;
use russh_sftp::client::error::Error as SftpError;
use russh_sftp::protocol::{FileAttributes, FileType, StatusCode};

use super::entry::{FileEntry, remote_join, remote_name, remote_parent};

pub type Sftp = Arc<SftpSession>;

/// A readable message for an SFTP error.
pub fn describe(e: &SftpError) -> String {
    match e {
        SftpError::Status(st) => {
            let what = match st.status_code {
                StatusCode::NoSuchFile => "no such file or directory",
                StatusCode::PermissionDenied => "permission denied",
                StatusCode::Failure if !st.error_message.is_empty() => st.error_message.as_str(),
                StatusCode::Failure => {
                    "operation failed (the target may already exist or not be empty)"
                }
                StatusCode::OpUnsupported => "not supported by the server",
                StatusCode::NoConnection | StatusCode::ConnectionLost => "connection lost",
                _ => st.error_message.as_str(),
            };
            what.to_string()
        }
        SftpError::Timeout => "the server did not answer in time".into(),
        other => other.to_string(),
    }
}

fn entry_from(
    name: String,
    attrs: &FileAttributes,
    link_target: Option<&FileAttributes>,
) -> FileEntry {
    let is_symlink = attrs.file_type() == FileType::Symlink;
    // A symlink shows (and chmods) what it points to; a broken link has no usable mode.
    let effective = if is_symlink { link_target } else { Some(attrs) };
    let is_dir = effective.is_some_and(|a| a.file_type() == FileType::Dir);
    FileEntry {
        name,
        is_dir,
        is_symlink,
        size: if is_dir {
            0
        } else {
            effective.and_then(|a| a.size).unwrap_or(0)
        },
        modified: attrs.mtime.map(i64::from),
        mode: effective.and_then(|a| a.permissions).map(|p| p & 0o7777),
    }
}

/// Lists a remote directory. Symlinks are resolved (several at a time) so linked folders can
/// be opened and show their target's details.
pub async fn list(sftp: &Sftp, path: &str) -> Result<Vec<FileEntry>, String> {
    let entries = sftp.read_dir(path).await.map_err(|e| describe(&e))?;
    let mut raw = Vec::new();
    let mut links = tokio::task::JoinSet::new();
    let limit = Arc::new(tokio::sync::Semaphore::new(16));
    for e in entries {
        let name = e.file_name();
        if name == "." || name == ".." {
            continue;
        }
        let attrs = e.metadata();
        if attrs.file_type() == FileType::Symlink {
            let (sftp, limit, full, idx) = (
                sftp.clone(),
                limit.clone(),
                remote_join(path, &name),
                raw.len(),
            );
            links.spawn(async move {
                let _permit = limit.acquire_owned().await;
                (idx, sftp.metadata(full).await.ok())
            });
        }
        raw.push((name, attrs, None));
    }
    while let Some(Ok((idx, target))) = links.join_next().await {
        raw[idx].2 = target;
    }
    Ok(raw
        .into_iter()
        .map(|(name, attrs, target)| entry_from(name, &attrs, target.as_ref()))
        .collect())
}

pub async fn home(sftp: &SftpSession) -> Result<String, String> {
    sftp.canonicalize(".").await.map_err(|e| describe(&e))
}

pub async fn canonicalize(sftp: &SftpSession, path: &str) -> Result<String, String> {
    sftp.canonicalize(path).await.map_err(|e| describe(&e))
}

pub async fn is_dir(sftp: &SftpSession, path: &str) -> Result<bool, String> {
    sftp.metadata(path)
        .await
        .map(|m| m.file_type() == FileType::Dir)
        .map_err(|e| describe(&e))
}

pub async fn exists(sftp: &SftpSession, path: &str) -> bool {
    sftp.symlink_metadata(path).await.is_ok()
}

pub async fn mkdir(sftp: &SftpSession, path: &str) -> Result<(), String> {
    sftp.create_dir(path).await.map_err(|e| describe(&e))
}

/// Creates a directory unless it already exists as a directory.
pub async fn ensure_dir(sftp: &SftpSession, path: &str) -> Result<(), String> {
    match sftp.metadata(path).await {
        Ok(m) if m.file_type() == FileType::Dir => Ok(()),
        Ok(_) => Err(format!("{path} exists and is not a folder")),
        Err(_) => mkdir(sftp, path).await,
    }
}

pub async fn rename(sftp: &SftpSession, from: &str, to: &str) -> Result<(), String> {
    sftp.rename(from, to).await.map_err(|e| describe(&e))
}

/// A short random string for temporary names next to a user's files.
pub fn random_suffix() -> String {
    let mut b = [0u8; 6];
    let _ = getrandom::fill(&mut b);
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Puts the finished file `tmp` in the place of the file at `path`. SFTP renames don't replace
/// files, so the original is moved aside first and deleted only once the new file is in place;
/// if that fails, the original is put back. `Ok(false)` when the original can't be moved
/// (nothing was changed).
pub async fn replace_file(sftp: &SftpSession, tmp: &str, path: &str) -> Result<bool, String> {
    let aside = remote_join(
        &remote_parent(path),
        &format!(".{}.peler-old-{}", remote_name(path), random_suffix()),
    );
    if sftp.rename(path, aside.clone()).await.is_err() {
        return Ok(false);
    }
    if let Err(e) = sftp.rename(tmp, path).await {
        if sftp.rename(aside.as_str(), path).await.is_err() {
            return Err(format!(
                "{}; the original file was kept as {aside}",
                describe(&e)
            ));
        }
        return Err(describe(&e));
    }
    let _ = sftp.remove_file(aside).await;
    Ok(true)
}

/// The file a symlink at `path` points to (replacing that keeps the link); `path` itself
/// otherwise.
pub async fn link_target(sftp: &SftpSession, path: &str) -> String {
    let is_link = sftp
        .symlink_metadata(path)
        .await
        .is_ok_and(|m| m.file_type() == FileType::Symlink);
    let to_file = is_link
        && sftp
            .metadata(path)
            .await
            .is_ok_and(|m| m.file_type() == FileType::File);
    if to_file && let Ok(real) = canonicalize(sftp, path).await {
        return real;
    }
    path.to_string()
}

/// Whether a file we just created has the same owner and group as `original` (replacing
/// `original` with it would otherwise change who owns the file).
pub fn same_owner(created: Option<&FileAttributes>, original: &FileAttributes) -> bool {
    match (created, original.uid, original.gid) {
        (Some(c), Some(uid), Some(gid)) => c.uid == Some(uid) && c.gid == Some(gid),
        _ => false,
    }
}

pub async fn chmod(sftp: &SftpSession, path: &str, mode: u32) -> Result<(), String> {
    sftp.set_metadata(
        path,
        FileAttributes {
            permissions: Some(mode),
            ..FileAttributes::empty()
        },
    )
    .await
    .map_err(|e| describe(&e))
}

/// Deletes a file, or a folder with everything in it.
pub async fn remove(sftp: &SftpSession, path: &str) -> Result<(), String> {
    let meta = sftp
        .symlink_metadata(path)
        .await
        .map_err(|e| describe(&e))?;
    if meta.file_type() != FileType::Dir {
        return sftp.remove_file(path).await.map_err(|e| describe(&e));
    }
    // Iterative depth-first delete (async recursion would need boxing).
    let mut stack = vec![(path.to_string(), false)];
    while let Some((dir, visited)) = stack.pop() {
        if visited {
            sftp.remove_dir(&dir)
                .await
                .map_err(|e| format!("{dir}: {}", describe(&e)))?;
            continue;
        }
        stack.push((dir.clone(), true));
        for e in sftp.read_dir(&dir).await.map_err(|e| describe(&e))? {
            let name = e.file_name();
            if name == "." || name == ".." {
                continue;
            }
            let child = remote_join(&dir, &name);
            if e.metadata().file_type() == FileType::Dir {
                stack.push((child, false));
            } else {
                sftp.remove_file(&child)
                    .await
                    .map_err(|e| format!("{child}: {}", describe(&e)))?;
            }
        }
    }
    Ok(())
}
