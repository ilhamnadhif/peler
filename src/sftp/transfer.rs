//! Background uploads and downloads with progress, cancellation and recursive folders.
//!
//! Each transfer runs as a tokio task; the UI polls the atomics for progress. Partial files are
//! removed when a transfer is cancelled or fails, and every file's size is checked after it has
//! been written.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use parking_lot::Mutex;
use russh_sftp::protocol::FileType;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Semaphore;

use super::entry::{remote_join, remote_name};
use super::remote::{self, Sftp, describe};

/// Read/write chunk size; the SFTP client pipelines it into several in-flight requests.
const CHUNK: usize = 512 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Upload,
    Download,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransferState {
    Queued,
    Running,
    Done,
    Failed(String),
    Cancelled,
}

pub struct Transfer {
    pub id: u64,
    pub direction: Direction,
    /// Display name (file or folder name).
    pub name: String,
    /// Local source (upload) or destination (download).
    pub local: PathBuf,
    /// Remote destination (upload) or source (download).
    pub remote: String,
    pub total: AtomicU64,
    pub done: AtomicU64,
    pub files_total: AtomicU64,
    pub files_done: AtomicU64,
    pub state: Mutex<TransferState>,
    cancel: AtomicBool,
    started: Mutex<Option<Instant>>,
    finished: Mutex<Option<Instant>>,
}

const CANCELLED: &str = "\0cancelled";

impl Transfer {
    fn new(id: u64, direction: Direction, name: String, local: PathBuf, remote: String) -> Self {
        Self {
            id,
            direction,
            name,
            local,
            remote,
            total: AtomicU64::new(0),
            done: AtomicU64::new(0),
            files_total: AtomicU64::new(0),
            files_done: AtomicU64::new(0),
            state: Mutex::new(TransferState::Queued),
            cancel: AtomicBool::new(false),
            started: Mutex::new(None),
            finished: Mutex::new(None),
        }
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    pub fn state(&self) -> TransferState {
        self.state.lock().clone()
    }

    pub fn is_finished(&self) -> bool {
        matches!(self.state(), TransferState::Done | TransferState::Failed(_) | TransferState::Cancelled)
    }

    pub fn fraction(&self) -> f32 {
        let total = self.total.load(Ordering::Relaxed);
        if total == 0 {
            return if self.state() == TransferState::Done { 1.0 } else { 0.0 };
        }
        (self.done.load(Ordering::Relaxed) as f64 / total as f64).min(1.0) as f32
    }

    /// Average bytes per second since the transfer started.
    pub fn speed(&self) -> f64 {
        let Some(start) = *self.started.lock() else { return 0.0 };
        let end = self.finished.lock().unwrap_or_else(Instant::now);
        let secs = end.duration_since(start).as_secs_f64().max(0.001);
        self.done.load(Ordering::Relaxed) as f64 / secs
    }

    fn finish(&self, result: Result<(), String>) {
        *self.finished.lock() = Some(Instant::now());
        *self.state.lock() = match result {
            Ok(()) => TransferState::Done,
            Err(e) if e == CANCELLED => TransferState::Cancelled,
            Err(e) => TransferState::Failed(e),
        };
    }
}

/// Runs transfers with a limit on how many run at once.
pub struct Transfers {
    pub items: Vec<Arc<Transfer>>,
    limiter: Arc<Semaphore>,
    next_id: u64,
}

impl Default for Transfers {
    fn default() -> Self {
        Self { items: Vec::new(), limiter: Arc::new(Semaphore::new(2)), next_id: 1 }
    }
}

impl Transfers {
    pub fn any_running(&self) -> bool {
        self.items.iter().any(|t| !t.is_finished())
    }

    pub fn clear_finished(&mut self) {
        self.items.retain(|t| !t.is_finished());
    }

    /// Uploads a local file or folder into `remote_dir`.
    pub fn upload(
        &mut self,
        rt: &tokio::runtime::Handle,
        sftp: Sftp,
        local: PathBuf,
        remote_dir: &str,
        on_done: impl Fn() + Send + Sync + 'static,
    ) -> Arc<Transfer> {
        let name = local.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let dest = remote_join(remote_dir, &name);
        let t = Arc::new(Transfer::new(self.next_id, Direction::Upload, name, local, dest));
        self.next_id += 1;
        self.items.push(t.clone());
        let limiter = self.limiter.clone();
        let task = t.clone();
        rt.spawn(async move {
            let _permit = limiter.acquire_owned().await;
            *task.started.lock() = Some(Instant::now());
            *task.state.lock() = TransferState::Running;
            let result = run_upload(&task, &sftp).await;
            task.finish(result);
            on_done();
        });
        t
    }

    /// Downloads a remote file or folder into `local_dir`.
    pub fn download(
        &mut self,
        rt: &tokio::runtime::Handle,
        sftp: Sftp,
        remote: String,
        local_dir: &Path,
        on_done: impl Fn() + Send + Sync + 'static,
    ) -> Arc<Transfer> {
        let name = remote_name(&remote).to_string();
        let dest = local_dir.join(&name);
        let t = Arc::new(Transfer::new(self.next_id, Direction::Download, name, dest, remote));
        self.next_id += 1;
        self.items.push(t.clone());
        let limiter = self.limiter.clone();
        let task = t.clone();
        rt.spawn(async move {
            let _permit = limiter.acquire_owned().await;
            *task.started.lock() = Some(Instant::now());
            *task.state.lock() = TransferState::Running;
            let result = run_download(&task, &sftp).await;
            task.finish(result);
            on_done();
        });
        t
    }
}

// ----- upload ---------------------------------------------------------------------------------

struct Plan {
    dirs: Vec<String>,
    files: Vec<(PathBuf, String, u64)>,
}

/// Walks a local file or folder (without following symlinked folders).
fn plan_upload(local: &Path, dest: &str) -> Result<Plan, String> {
    let meta = std::fs::metadata(local).map_err(|e| format!("{}: {e}", local.display()))?;
    let mut plan = Plan { dirs: Vec::new(), files: Vec::new() };
    if !meta.is_dir() {
        plan.files.push((local.to_path_buf(), dest.to_string(), meta.len()));
        return Ok(plan);
    }
    let mut stack = vec![(local.to_path_buf(), dest.to_string())];
    while let Some((dir, remote_dir)) = stack.pop() {
        plan.dirs.push(remote_dir.clone());
        let mut entries: Vec<_> = std::fs::read_dir(&dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?
            .flatten()
            .collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(ft) = entry.file_type() else { continue };
            let target = remote_join(&remote_dir, &name);
            if ft.is_dir() {
                stack.push((path, target));
            } else if ft.is_file() || (ft.is_symlink() && path.is_file()) {
                let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                plan.files.push((path, target, size));
            }
        }
    }
    Ok(plan)
}

async fn run_upload(t: &Transfer, sftp: &Sftp) -> Result<(), String> {
    let (local, dest) = (t.local.clone(), t.remote.clone());
    let plan = tokio::task::spawn_blocking(move || plan_upload(&local, &dest)).await.map_err(|e| e.to_string())??;
    t.total.store(plan.files.iter().map(|f| f.2).sum(), Ordering::Relaxed);
    t.files_total.store(plan.files.len() as u64, Ordering::Relaxed);
    for dir in &plan.dirs {
        if t.cancelled() {
            return Err(CANCELLED.into());
        }
        remote::ensure_dir(sftp, dir).await?;
    }
    for (local, remote_path, size) in &plan.files {
        upload_file(t, sftp, local, remote_path, *size).await?;
        t.files_done.fetch_add(1, Ordering::Relaxed);
    }
    Ok(())
}

async fn upload_file(t: &Transfer, sftp: &Sftp, local: &Path, remote_path: &str, size: u64) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mut src = tokio::fs::File::open(local).await.map_err(|e| format!("{}: {e}", local.display()))?;
    let mode = src.metadata().await.map(|m| m.permissions().mode() & 0o777).ok();
    let mut dst = sftp.create(remote_path).await.map_err(|e| format!("{remote_path}: {}", describe(&e)))?;
    let mut buf = vec![0u8; CHUNK];
    let result: Result<(), String> = async {
        loop {
            if t.cancelled() {
                return Err(CANCELLED.to_string());
            }
            let n = src.read(&mut buf).await.map_err(|e| format!("{}: {e}", local.display()))?;
            if n == 0 {
                break;
            }
            dst.write_all(&buf[..n]).await.map_err(|e| format!("{remote_path}: {e}"))?;
            t.done.fetch_add(n as u64, Ordering::Relaxed);
        }
        Ok(())
    }
    .await;
    let closed = dst.close().await.map_err(|e| format!("{remote_path}: {e}"));
    if let Err(e) = result.and(closed) {
        let _ = sftp.remove_file(remote_path).await;
        return Err(e);
    }
    if let Some(mode) = mode {
        let _ = remote::chmod(sftp, remote_path, mode).await;
    }
    let written = sftp.metadata(remote_path).await.ok().and_then(|m| m.size);
    if written.is_some_and(|w| w != size) {
        return Err(format!("{remote_path}: size check failed ({} of {size} bytes)", written.unwrap_or(0)));
    }
    Ok(())
}

// ----- download -------------------------------------------------------------------------------

async fn run_download(t: &Transfer, sftp: &Sftp) -> Result<(), String> {
    let root = sftp.metadata(&t.remote).await.map_err(|e| format!("{}: {}", t.remote, describe(&e)))?;
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut files: Vec<(String, PathBuf, u64, Option<u32>)> = Vec::new();
    if root.file_type() == FileType::Dir {
        let mut stack = vec![(t.remote.clone(), t.local.clone())];
        while let Some((remote_dir, local_dir)) = stack.pop() {
            if t.cancelled() {
                return Err(CANCELLED.into());
            }
            dirs.push(local_dir.clone());
            let mut entries: Vec<_> =
                sftp.read_dir(&remote_dir).await.map_err(|e| format!("{remote_dir}: {}", describe(&e)))?.collect();
            entries.sort_by_key(|e| e.file_name());
            for e in entries {
                let name = e.file_name();
                if name == "." || name == ".." || name.contains('/') {
                    continue;
                }
                let attrs = e.metadata();
                let child = remote_join(&remote_dir, &name);
                match attrs.file_type() {
                    FileType::Dir => stack.push((child, local_dir.join(&name))),
                    FileType::File => files.push((child, local_dir.join(&name), attrs.size.unwrap_or(0), attrs.permissions)),
                    _ => {}
                }
            }
        }
    } else {
        files.push((t.remote.clone(), t.local.clone(), root.size.unwrap_or(0), root.permissions));
    }
    t.total.store(files.iter().map(|f| f.2).sum(), Ordering::Relaxed);
    t.files_total.store(files.len() as u64, Ordering::Relaxed);
    for dir in &dirs {
        tokio::fs::create_dir_all(dir).await.map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    for (remote_path, local, size, mode) in &files {
        download_file(t, sftp, remote_path, local, *size, *mode).await?;
        t.files_done.fetch_add(1, Ordering::Relaxed);
    }
    Ok(())
}

async fn download_file(t: &Transfer, sftp: &Sftp, remote_path: &str, local: &Path, size: u64, mode: Option<u32>) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mut src = sftp.open(remote_path).await.map_err(|e| format!("{remote_path}: {}", describe(&e)))?;
    let mut dst = tokio::fs::File::create(local).await.map_err(|e| format!("{}: {e}", local.display()))?;
    let mut buf = vec![0u8; CHUNK];
    let mut written = 0u64;
    let result: Result<(), String> = async {
        loop {
            if t.cancelled() {
                return Err(CANCELLED.to_string());
            }
            let n = src.read(&mut buf).await.map_err(|e| format!("{remote_path}: {e}"))?;
            if n == 0 {
                break;
            }
            dst.write_all(&buf[..n]).await.map_err(|e| format!("{}: {e}", local.display()))?;
            written += n as u64;
            t.done.fetch_add(n as u64, Ordering::Relaxed);
        }
        dst.flush().await.map_err(|e| format!("{}: {e}", local.display()))?;
        Ok(())
    }
    .await;
    drop(src);
    if let Err(e) = result {
        drop(dst);
        let _ = tokio::fs::remove_file(local).await;
        return Err(e);
    }
    if written != size {
        return Err(format!("{}: size check failed ({written} of {size} bytes)", local.display()));
    }
    if let Some(mode) = mode {
        let _ = tokio::fs::set_permissions(local, std::fs::Permissions::from_mode(mode & 0o777)).await;
    }
    Ok(())
}
