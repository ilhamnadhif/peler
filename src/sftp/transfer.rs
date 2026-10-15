//! Background uploads and downloads with progress, cancellation and recursive folders.
//!
//! Each transfer runs as a tokio task; the UI polls the atomics for progress. Files are written
//! under a temporary name next to the destination and renamed into place only when complete,
//! so a failed or cancelled transfer never damages an existing file. Entries that cannot be
//! transferred inside a folder (unreadable folders, symlinked folders) are skipped and reported
//! instead of aborting the whole transfer.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use russh_sftp::protocol::FileType;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Semaphore;

use super::entry::{remote_join, remote_name, remote_parent};
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
    /// Entries inside a folder that were skipped, with the reason.
    pub skipped: Mutex<Vec<String>>,
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
            skipped: Mutex::new(Vec::new()),
            cancel: AtomicBool::new(false),
            started: Mutex::new(None),
            finished: Mutex::new(None),
        }
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
        // A transfer still waiting for its turn is cancelled right away.
        let mut state = self.state.lock();
        if *state == TransferState::Queued {
            *state = TransferState::Cancelled;
            *self.finished.lock() = Some(Instant::now());
        }
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    pub fn state(&self) -> TransferState {
        self.state.lock().clone()
    }

    pub fn is_finished(&self) -> bool {
        matches!(
            self.state(),
            TransferState::Done | TransferState::Failed(_) | TransferState::Cancelled
        )
    }

    pub fn fraction(&self) -> f32 {
        let total = self.total.load(Ordering::Relaxed);
        if total == 0 {
            return if self.state() == TransferState::Done {
                1.0
            } else {
                0.0
            };
        }
        (self.done.load(Ordering::Relaxed) as f64 / total as f64).min(1.0) as f32
    }

    /// Average bytes per second since the transfer started.
    pub fn speed(&self) -> f64 {
        let Some(start) = *self.started.lock() else {
            return 0.0;
        };
        let end = self.finished.lock().unwrap_or_else(Instant::now);
        let secs = end.duration_since(start).as_secs_f64().max(0.001);
        self.done.load(Ordering::Relaxed) as f64 / secs
    }

    fn skip(&self, what: String) {
        self.skipped.lock().push(what);
    }

    fn finish(&self, result: Result<(), String>) {
        *self.finished.lock() = Some(Instant::now());
        *self.state.lock() = match result {
            Ok(()) => TransferState::Done,
            Err(e) if e == CANCELLED => TransferState::Cancelled,
            Err(e) => TransferState::Failed(e),
        };
    }

    /// Temporary name for a file being written by this transfer.
    fn partial_name(&self, name: &str) -> String {
        format!(".{name}.peler-part-{}", self.id)
    }
}

/// Overall progress of a batch: the transfers started since nothing was running.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Summary {
    pub batch: u64,
    pub uploads: usize,
    pub downloads: usize,
    /// Transfers not finished yet (running or waiting).
    pub active: usize,
    pub done: usize,
    pub failed: usize,
    pub cancelled: usize,
    pub files_done: u64,
    pub files_total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    /// Bytes per second over the whole batch so far.
    pub speed: f64,
}

impl Summary {
    pub fn running(&self) -> bool {
        self.active > 0
    }

    pub fn fraction(&self) -> f32 {
        if self.bytes_total == 0 {
            return if self.running() { 0.0 } else { 1.0 };
        }
        (self.bytes_done as f64 / self.bytes_total as f64).min(1.0) as f32
    }

    /// Estimated time left, once there is enough to go on.
    pub fn eta(&self) -> Option<Duration> {
        if !self.running() || self.speed < 1.0 || self.bytes_done == 0 {
            return None;
        }
        let left = self.bytes_total.saturating_sub(self.bytes_done) as f64;
        Some(Duration::from_secs_f64(left / self.speed))
    }
}

/// Runs transfers with a limit on how many run at once.
pub struct Transfers {
    pub items: Vec<Arc<Transfer>>,
    limiter: Arc<Semaphore>,
    next_id: u64,
    /// First transfer id of the current batch, and when the batch started.
    batch_first: u64,
    batch_started: Option<Instant>,
}

impl Default for Transfers {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            limiter: Arc::new(Semaphore::new(2)),
            next_id: 1,
            batch_first: 1,
            batch_started: None,
        }
    }
}

impl Transfers {
    /// Progress of the current batch (`None` before the first transfer).
    pub fn summary(&self) -> Option<Summary> {
        let started = self.batch_started?;
        let mut s = Summary {
            batch: self.batch_first,
            ..Default::default()
        };
        let mut last_finish = None;
        for t in self.items.iter().filter(|t| t.id >= self.batch_first) {
            match t.direction {
                Direction::Upload => s.uploads += 1,
                Direction::Download => s.downloads += 1,
            }
            let state = t.state();
            let files = t.files_total.load(Ordering::Relaxed).max(1);
            s.files_total += files;
            match state {
                TransferState::Done => {
                    s.done += 1;
                    s.files_done += files;
                }
                TransferState::Failed(_) => s.failed += 1,
                TransferState::Cancelled => s.cancelled += 1,
                TransferState::Queued | TransferState::Running => {
                    s.active += 1;
                    s.files_done += t.files_done.load(Ordering::Relaxed);
                }
            }
            if !matches!(state, TransferState::Cancelled) {
                s.bytes_total += t.total.load(Ordering::Relaxed);
                s.bytes_done += t.done.load(Ordering::Relaxed);
            }
            if let Some(f) = *t.finished.lock() {
                last_finish = Some(last_finish.map_or(f, |l: Instant| l.max(f)));
            }
        }
        if s.uploads + s.downloads == 0 {
            return None;
        }
        let end = if s.running() {
            Instant::now()
        } else {
            last_finish.unwrap_or_else(Instant::now)
        };
        s.speed = s.bytes_done as f64 / end.duration_since(started).as_secs_f64().max(0.001);
        Some(s)
    }

    /// Cancels every unfinished transfer.
    pub fn cancel_all(&self) {
        for t in &self.items {
            if !t.is_finished() {
                t.cancel();
            }
        }
    }

    pub fn any_running(&self) -> bool {
        self.items.iter().any(|t| !t.is_finished())
    }

    pub fn clear_finished(&mut self) {
        self.items.retain(|t| !t.is_finished());
    }

    /// Whether an unfinished transfer already writes to this destination.
    fn busy(&self, direction: Direction, local: &Path, remote: &str) -> bool {
        self.items.iter().any(|t| {
            !t.is_finished()
                && t.direction == direction
                && match direction {
                    Direction::Upload => t.remote == remote,
                    Direction::Download => t.local == local,
                }
        })
    }

    fn start(
        &mut self,
        rt: &tokio::runtime::Handle,
        sftp: Sftp,
        t: Transfer,
        on_done: impl Fn() + Send + Sync + 'static,
    ) -> Arc<Transfer> {
        let t = Arc::new(t);
        self.next_id += 1;
        // A transfer started while nothing else runs begins a new batch.
        if !self.any_running() {
            self.batch_first = t.id;
            self.batch_started = Some(Instant::now());
        }
        self.items.push(t.clone());
        let limiter = self.limiter.clone();
        let task = t.clone();
        rt.spawn(async move {
            let _permit = limiter.acquire_owned().await;
            if task.cancelled() {
                // Cancelled while queued: leave every destination untouched.
                task.finish(Err(CANCELLED.into()));
                on_done();
                return;
            }
            *task.started.lock() = Some(Instant::now());
            *task.state.lock() = TransferState::Running;
            let result = match task.direction {
                Direction::Upload => run_upload(&task, &sftp).await,
                Direction::Download => run_download(&task, &sftp).await,
            };
            task.finish(result);
            on_done();
        });
        t
    }

    /// Uploads a local file or folder into `remote_dir`. Refused while another transfer is
    /// writing to the same destination.
    pub fn upload(
        &mut self,
        rt: &tokio::runtime::Handle,
        sftp: Sftp,
        local: PathBuf,
        remote_dir: &str,
        on_done: impl Fn() + Send + Sync + 'static,
    ) -> Result<Arc<Transfer>, String> {
        let name = local
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let dest = remote_join(remote_dir, &name);
        if self.busy(Direction::Upload, &local, &dest) {
            return Err(dest);
        }
        let t = Transfer::new(self.next_id, Direction::Upload, name, local, dest);
        Ok(self.start(rt, sftp, t, on_done))
    }

    /// Downloads a remote file or folder into `local_dir`. Refused while another transfer is
    /// writing to the same destination.
    pub fn download(
        &mut self,
        rt: &tokio::runtime::Handle,
        sftp: Sftp,
        remote: String,
        local_dir: &Path,
        on_done: impl Fn() + Send + Sync + 'static,
    ) -> Result<Arc<Transfer>, String> {
        let name = remote_name(&remote).to_string();
        let dest = local_dir.join(&name);
        if self.busy(Direction::Download, &dest, &remote) {
            return Err(dest.display().to_string());
        }
        let t = Transfer::new(self.next_id, Direction::Download, name, dest, remote);
        Ok(self.start(rt, sftp, t, on_done))
    }
}

// ----- upload ---------------------------------------------------------------------------------

struct Plan {
    dirs: Vec<String>,
    files: Vec<(PathBuf, String, u64)>,
}

/// Walks a local file or folder. Symlinked folders and unreadable folders are skipped.
fn plan_upload(t: &Transfer, local: &Path, dest: &str) -> Result<Plan, String> {
    let meta = std::fs::metadata(local).map_err(|e| format!("{}: {e}", local.display()))?;
    let mut plan = Plan {
        dirs: Vec::new(),
        files: Vec::new(),
    };
    if !meta.is_dir() {
        plan.files
            .push((local.to_path_buf(), dest.to_string(), meta.len()));
        return Ok(plan);
    }
    let mut stack = vec![(local.to_path_buf(), dest.to_string())];
    while let Some((dir, remote_dir)) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if dir != local => {
                t.skip(format!("{}: {e}", dir.display()));
                continue;
            }
            Err(e) => return Err(format!("{}: {e}", dir.display())),
        };
        plan.dirs.push(remote_dir.clone());
        let mut entries: Vec<_> = entries.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(ft) = entry.file_type() else { continue };
            let target = remote_join(&remote_dir, &name);
            if ft.is_dir() {
                stack.push((path, target));
            } else if ft.is_symlink() && path.is_dir() {
                t.skip(format!("{} (symlinked folder)", path.display()));
            } else if ft.is_file() || (ft.is_symlink() && path.is_file()) {
                let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                plan.files.push((path, target, size));
            }
        }
    }
    Ok(plan)
}

async fn run_upload(t: &Arc<Transfer>, sftp: &Sftp) -> Result<(), String> {
    let (local, dest) = (t.local.clone(), t.remote.clone());
    let task = t.clone();
    let plan = tokio::task::spawn_blocking(move || plan_upload(&task, &local, &dest))
        .await
        .map_err(|e| e.to_string())??;
    t.total
        .store(plan.files.iter().map(|f| f.2).sum(), Ordering::Relaxed);
    t.files_total
        .store(plan.files.len() as u64, Ordering::Relaxed);
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

async fn upload_file(
    t: &Transfer,
    sftp: &Sftp,
    local: &Path,
    remote_path: &str,
    size: u64,
) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    if t.cancelled() {
        return Err(CANCELLED.into());
    }
    let mut src = tokio::fs::File::open(local)
        .await
        .map_err(|e| format!("{}: {e}", local.display()))?;
    let local_mode = src
        .metadata()
        .await
        .map(|m| m.permissions().mode() & 0o777)
        .ok();
    let partial = remote_join(
        &remote_parent(remote_path),
        &t.partial_name(remote_name(remote_path)),
    );
    let mut dst = sftp
        .create(partial.clone())
        .await
        .map_err(|e| format!("{remote_path}: {}", describe(&e)))?;
    let mut buf = vec![0u8; CHUNK];
    let result: Result<(), String> = async {
        loop {
            if t.cancelled() {
                return Err(CANCELLED.to_string());
            }
            let n = src
                .read(&mut buf)
                .await
                .map_err(|e| format!("{}: {e}", local.display()))?;
            if n == 0 {
                break;
            }
            dst.write_all(&buf[..n])
                .await
                .map_err(|e| format!("{remote_path}: {e}"))?;
            t.done.fetch_add(n as u64, Ordering::Relaxed);
        }
        Ok(())
    }
    .await;
    let closed = dst.close().await.map_err(|e| format!("{remote_path}: {e}"));
    let result = result.and(closed);
    let result = match result {
        Ok(()) => match sftp.metadata(partial.clone()).await {
            Ok(m) if m.size == Some(size) => Ok(()),
            Ok(m) => Err(format!(
                "{remote_path}: size check failed ({} of {size} bytes)",
                m.size.unwrap_or(0)
            )),
            Err(e) => Err(format!("{remote_path}: {}", describe(&e))),
        },
        Err(e) => Err(e),
    };
    if let Err(e) = result {
        let _ = sftp.remove_file(partial).await;
        return Err(e);
    }
    // Replacing a file keeps its permissions; a new file gets the local file's.
    let existing = sftp.metadata(remote_path).await.ok();
    let mode = existing
        .as_ref()
        .and_then(|m| m.permissions)
        .map(|p| p & 0o7777)
        .or(local_mode);
    if let Some(mode) = mode {
        let _ = remote::chmod(sftp, &partial, mode).await;
    }
    if existing.is_some() {
        // SFTP v3 rename refuses to replace: remove the old file at the last moment.
        if let Err(e) = sftp.remove_file(remote_path).await {
            let _ = sftp.remove_file(partial).await;
            return Err(format!("{remote_path}: {}", describe(&e)));
        }
    }
    if let Err(e) = remote::rename(sftp, &partial, remote_path).await {
        let _ = sftp.remove_file(partial).await;
        return Err(format!("{remote_path}: {e}"));
    }
    Ok(())
}

// ----- download -------------------------------------------------------------------------------

async fn run_download(t: &Transfer, sftp: &Sftp) -> Result<(), String> {
    let root = sftp
        .metadata(&t.remote)
        .await
        .map_err(|e| format!("{}: {}", t.remote, describe(&e)))?;
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut files: Vec<(String, PathBuf, u64, Option<u32>)> = Vec::new();
    if root.file_type() == FileType::Dir {
        let mut stack = vec![(t.remote.clone(), t.local.clone())];
        while let Some((remote_dir, local_dir)) = stack.pop() {
            if t.cancelled() {
                return Err(CANCELLED.into());
            }
            let entries = match sftp.read_dir(&remote_dir).await {
                Ok(e) => e,
                Err(e) if remote_dir != t.remote => {
                    t.skip(format!("{remote_dir}: {}", describe(&e)));
                    continue;
                }
                Err(e) => return Err(format!("{remote_dir}: {}", describe(&e))),
            };
            dirs.push(local_dir.clone());
            let mut entries: Vec<_> = entries.collect();
            entries.sort_by_key(|e| e.file_name());
            for e in entries {
                let name = e.file_name();
                if name == "." || name == ".." || name.contains('/') {
                    continue;
                }
                let child = remote_join(&remote_dir, &name);
                let mut attrs = e.metadata();
                if attrs.file_type() == FileType::Symlink {
                    // Follow links to files; links to folders are skipped (they can loop).
                    match sftp.metadata(&child).await {
                        Ok(target) if target.file_type() == FileType::File => attrs = target,
                        _ => {
                            t.skip(format!("{child} (symlink)"));
                            continue;
                        }
                    }
                }
                match attrs.file_type() {
                    FileType::Dir => stack.push((child, local_dir.join(&name))),
                    FileType::File => files.push((
                        child,
                        local_dir.join(&name),
                        attrs.size.unwrap_or(0),
                        attrs.permissions,
                    )),
                    _ => t.skip(format!("{child} (special file)")),
                }
            }
        }
    } else {
        files.push((
            t.remote.clone(),
            t.local.clone(),
            root.size.unwrap_or(0),
            root.permissions,
        ));
    }
    t.total
        .store(files.iter().map(|f| f.2).sum(), Ordering::Relaxed);
    t.files_total.store(files.len() as u64, Ordering::Relaxed);
    for dir in &dirs {
        tokio::fs::create_dir_all(dir)
            .await
            .map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    for (remote_path, local, size, mode) in &files {
        download_file(t, sftp, remote_path, local, *size, *mode).await?;
        t.files_done.fetch_add(1, Ordering::Relaxed);
    }
    Ok(())
}

async fn download_file(
    t: &Transfer,
    sftp: &Sftp,
    remote_path: &str,
    local: &Path,
    size: u64,
    mode: Option<u32>,
) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    if t.cancelled() {
        return Err(CANCELLED.into());
    }
    let mut src = sftp
        .open(remote_path)
        .await
        .map_err(|e| format!("{remote_path}: {}", describe(&e)))?;
    let file_name = local
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let partial = local.with_file_name(t.partial_name(&file_name));
    let mut dst = tokio::fs::File::create(&partial)
        .await
        .map_err(|e| format!("{}: {e}", local.display()))?;
    let mut buf = vec![0u8; CHUNK];
    let mut written = 0u64;
    let result: Result<(), String> = async {
        loop {
            if t.cancelled() {
                return Err(CANCELLED.to_string());
            }
            let n = src
                .read(&mut buf)
                .await
                .map_err(|e| format!("{remote_path}: {e}"))?;
            if n == 0 {
                break;
            }
            dst.write_all(&buf[..n])
                .await
                .map_err(|e| format!("{}: {e}", local.display()))?;
            written += n as u64;
            t.done.fetch_add(n as u64, Ordering::Relaxed);
        }
        dst.flush()
            .await
            .map_err(|e| format!("{}: {e}", local.display()))?;
        Ok(())
    }
    .await;
    drop(src);
    drop(dst);
    // A file that grew while being read (a log) is fine; one that came up short is not.
    let result = result.and_then(|()| {
        if written >= size {
            Ok(())
        } else {
            Err(format!(
                "{}: size check failed ({written} of {size} bytes)",
                local.display()
            ))
        }
    });
    if let Err(e) = result {
        let _ = tokio::fs::remove_file(&partial).await;
        return Err(e);
    }
    if let Some(mode) = mode {
        let _ = tokio::fs::set_permissions(&partial, std::fs::Permissions::from_mode(mode & 0o777))
            .await;
    }
    tokio::fs::rename(&partial, local).await.map_err(|e| {
        let _ = std::fs::remove_file(&partial);
        format!("{}: {e}", local.display())
    })
}
