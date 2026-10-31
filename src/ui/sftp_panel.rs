//! The SFTP panel: local and remote file lists side by side, transfers below.
//!
//! Every operation carries the folder it was started in (dialogs, right-click menus, drags), so
//! a listing that changes in the meantime can never redirect it to another file.

use std::collections::HashSet;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use eframe::egui::{self, RichText, Sense, Ui};
use egui_extras::{Column, TableBuilder};
use russh_sftp::client::SftpSession;
use tokio::sync::oneshot;

use crate::i18n::{t, tf};
use crate::paths;
use crate::sftp::entry::{self, FileEntry, SortKey, remote_join, remote_parent};
use crate::sftp::remote::{self, Sftp};
use crate::sftp::transfer::{Direction, Summary, Transfer, TransferState, Transfers};
use crate::ssh::session::SftpOpener;
use crate::ui::file_editor::{EditorOutput, FileEditor};
use crate::ui::kit::{self, Segment};
use crate::ui::{icons, style};

enum Session {
    Opening(oneshot::Receiver<Result<SftpSession, String>>),
    Ready(Sftp),
    Failed(String),
}

/// A session being opened to switch root on/off: (reply, whether it is the root session, the
/// folder to show once it is ready).
type Reopen = (oneshot::Receiver<Result<SftpSession, String>>, bool, String);

enum Event {
    Listed {
        /// Request number: replies to older requests are ignored.
        seq: u64,
        path: String,
        result: Result<Vec<FileEntry>, String>,
    },
    OpDone {
        result: Result<(), String>,
        refresh_remote: bool,
    },
}

#[derive(Default)]
struct SideState {
    cwd: String,
    path_edit: String,
    entries: Vec<FileEntry>,
    loading: bool,
    error: Option<String>,
    selected: HashSet<String>,
    anchor: Option<String>,
    sort: SortKey,
    descending: bool,
    /// Whether this side shows dot-files; each side has its own toggle.
    show_hidden: bool,
    /// Width the table was laid out for: when it changes, the columns are laid out again.
    table_width: f32,
}

impl SideState {
    fn visible(&self, show_hidden: bool) -> impl Iterator<Item = &FileEntry> {
        self.entries
            .iter()
            .filter(move |e| show_hidden || !e.is_hidden())
    }

    /// Replaces the listing. Within the same folder the selection of entries that still exist
    /// is kept; a different folder starts fresh.
    fn set_listing(&mut self, cwd: String, entries: Vec<FileEntry>) {
        if cwd == self.cwd {
            self.selected
                .retain(|n| entries.iter().any(|e| &e.name == n));
        } else {
            self.selected.clear();
            self.anchor = None;
            self.path_edit = cwd.clone();
        }
        self.cwd = cwd;
        self.entries = entries;
        self.error = None;
    }
}

/// Files being dragged from one side to the other.
#[derive(Clone)]
struct DragFiles {
    from_remote: bool,
    dir: String,
    names: Vec<String>,
}

/// What a right-click menu acts on, captured when the menu opens.
#[derive(Clone)]
struct MenuTarget {
    remote: bool,
    dir: String,
    names: Vec<String>,
    entry: FileEntry,
}

/// Dialogs remember the folder they were opened for.
enum Dialog {
    Mkdir {
        remote: bool,
        dir: String,
        name: String,
    },
    Rename {
        remote: bool,
        dir: String,
        from: String,
        to: String,
    },
    Delete {
        remote: bool,
        dir: String,
        names: Vec<String>,
    },
    Chmod {
        path: String,
        original: u32,
        mode: String,
    },
    /// Upload: `sources` are local paths and `dest` the remote folder. Download: `sources`
    /// are remote paths and `dest` the local folder.
    Overwrite {
        upload: bool,
        sources: Vec<String>,
        dest: String,
        conflicts: Vec<String>,
    },
    /// Switch the remote session to root via sudo; the password is optional (passwordless sudo).
    Sudo { password: String },
}

pub struct SftpPanel {
    session: Session,
    /// Opens new sessions on the same connection, to switch root on or off.
    opener: SftpOpener,
    /// The remote session is running as root (sudo).
    is_root: bool,
    /// A new session being opened to switch root on/off. The working session is kept until this
    /// one is ready, so a failed switch never loses it.
    reopening: Option<Reopen>,
    events_tx: mpsc::Sender<Event>,
    events_rx: mpsc::Receiver<Event>,
    local: SideState,
    remote: SideState,
    remote_seq: u64,
    pub transfers: Transfers,
    dialog: Option<Dialog>,
    focus_dialog: bool,
    /// Finder drops waiting for the session (or for an open dialog to close).
    pending_uploads: Vec<PathBuf>,
    start_dir: String,
    message: Option<(String, bool)>,
    /// Label shown next to the pointer while files are dragged between the two sides.
    drag_label: Option<String>,
    menu_target: Option<MenuTarget>,
    refresh_remote_pending: bool,
    /// A remote text file open in the built-in editor.
    editor: Option<FileEditor>,
    /// The last transfer batch whose completion was announced.
    reported_batch: u64,
    /// Distinguishes this panel's UI state (panel sizes) from other tabs'.
    uid: u64,
    /// The floating transfers/errors popup is open. It opens itself on new activity (a transfer
    /// or an error), and the user can hide (collapse) or close it.
    show_activity: bool,
    /// A finished batch to announce (text, is an error).
    notice: Option<(String, bool)>,
}

/// Something the app should do after the panel was drawn.
pub enum PanelOutput {
    None,
    /// Remember this local folder for next time.
    LocalDir(String),
}

enum SideAction {
    DragStarted(String),
    MenuOpened(MenuTarget),
    Up(bool),
    Home(bool),
    Refresh(bool),
    Cd(bool, String),
    Mkdir(bool),
    Open {
        remote: bool,
        dir: String,
        entry: FileEntry,
    },
    /// Open a remote file in the built-in editor.
    Edit {
        dir: String,
        name: String,
    },
    /// Choose local files (or folders) to upload into the remote folder.
    PickUpload {
        folders: bool,
    },
    SelectAll(bool),
    Select(bool, String, bool, bool),
    Sort(bool, SortKey),
    Transfer {
        from_remote: bool,
        dir: String,
        names: Vec<String>,
    },
    Rename {
        remote: bool,
        dir: String,
        name: String,
    },
    Delete {
        remote: bool,
        dir: String,
        names: Vec<String>,
    },
    Chmod {
        path: String,
        mode: u32,
    },
    ToggleHidden(bool),
    /// Switch the remote session to or from root (sudo).
    Root,
}

impl SftpPanel {
    pub fn new(opener: SftpOpener, start_dir: &str, local_dir: Option<&str>) -> Self {
        let (events_tx, events_rx) = mpsc::channel();
        let local_cwd = local_dir
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .unwrap_or_else(paths::home_dir)
            .display()
            .to_string();
        let mut panel = Self {
            session: Session::Opening(opener.open()),
            opener,
            is_root: false,
            reopening: None,
            events_tx,
            events_rx,
            local: SideState::default(),
            remote: SideState::default(),
            remote_seq: 0,
            transfers: Transfers::default(),
            dialog: None,
            focus_dialog: false,
            pending_uploads: Vec::new(),
            start_dir: start_dir.trim().to_string(),
            message: None,
            drag_label: None,
            menu_target: None,
            refresh_remote_pending: false,
            editor: None,
            reported_batch: 0,
            uid: {
                static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            },
            show_activity: false,
            notice: None,
        };
        panel.local_cd(&local_cwd);
        panel
    }

    pub fn sftp(&self) -> Option<Sftp> {
        match &self.session {
            Session::Ready(s) => Some(s.clone()),
            _ => None,
        }
    }

    pub fn remote_cwd(&self) -> &str {
        &self.remote.cwd
    }

    pub fn local_cwd(&self) -> &str {
        &self.local.cwd
    }

    pub fn dialog_open(&self) -> bool {
        self.dialog.is_some() || self.editor.is_some()
    }

    /// ⌘W: closes a dialog, or the editor (asking first if it has unsaved changes).
    pub fn close_dialog(&mut self) {
        if self.dialog.take().is_some() {
            return;
        }
        if self.editor.as_mut().is_some_and(FileEditor::request_close) {
            self.editor = None;
        }
    }

    /// The file open in the editor with unsaved changes, if any.
    pub fn unsaved_file(&self) -> Option<String> {
        self.editor
            .as_ref()
            .filter(|e| e.is_dirty())
            .map(|e| e.name().to_string())
    }

    pub fn is_ready(&self) -> bool {
        matches!(self.session, Session::Ready(_))
    }

    fn error(&mut self, msg: impl Into<String>) {
        self.message = Some((msg.into(), true));
        self.show_activity = true;
    }

    /// Uploads local files (e.g. dropped from Finder) into the current remote folder.
    pub fn upload_paths(
        &mut self,
        rt: &tokio::runtime::Handle,
        ctx: &egui::Context,
        paths: Vec<PathBuf>,
    ) {
        match &self.session {
            Session::Failed(e) => {
                let e = e.clone();
                self.error(tf("SFTP unavailable: {}", &[&e]));
                return;
            }
            Session::Opening(_) => {
                self.pending_uploads.extend(paths);
                self.message = Some((t("Uploads will start when SFTP is ready.").into(), false));
                self.show_activity = true;
                return;
            }
            Session::Ready(_) => {}
        }
        if self.dialog.is_some() {
            self.pending_uploads.extend(paths);
            self.message = Some((
                t("Uploads will start after the open dialog is closed.").into(),
                false,
            ));
            self.show_activity = true;
            return;
        }
        if self.remote.cwd.is_empty() {
            if self.remote.loading {
                self.pending_uploads.extend(paths);
            } else {
                self.error(t("Open a remote folder first."));
            }
            return;
        }
        let sources: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
        let dest = self.remote.cwd.clone();
        self.upload_sources(rt, ctx, sources, dest);
    }

    /// Starts uploads of local paths into `dest`, asking before replacing existing entries.
    fn upload_sources(
        &mut self,
        rt: &tokio::runtime::Handle,
        ctx: &egui::Context,
        sources: Vec<String>,
        dest: String,
    ) {
        if dest.is_empty() {
            self.error(t("Open a remote folder first."));
            return;
        }
        let conflicts: Vec<String> = sources
            .iter()
            .filter_map(|s| {
                Path::new(s)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
            })
            .filter(|n| dest == self.remote.cwd && self.remote.entries.iter().any(|e| &e.name == n))
            .collect();
        if conflicts.is_empty() {
            for src in sources {
                self.start_upload(rt, ctx, PathBuf::from(src), &dest);
            }
        } else {
            self.dialog = Some(Dialog::Overwrite {
                upload: true,
                sources,
                dest,
                conflicts,
            });
        }
    }

    /// Starts downloads of remote paths into `dest`, asking before replacing existing entries.
    fn download_sources(
        &mut self,
        rt: &tokio::runtime::Handle,
        ctx: &egui::Context,
        sources: Vec<String>,
        dest: String,
    ) {
        let conflicts: Vec<String> = sources
            .iter()
            .map(|s| entry::remote_name(s).to_string())
            .filter(|n| Path::new(&dest).join(n).exists())
            .collect();
        if conflicts.is_empty() {
            for src in sources {
                self.start_download(rt, ctx, src, Path::new(&dest));
            }
        } else {
            self.dialog = Some(Dialog::Overwrite {
                upload: false,
                sources,
                dest,
                conflicts,
            });
        }
    }

    fn local_cd(&mut self, path: &str) {
        let path = paths::expand_tilde(path.trim());
        match entry::list_local(&path) {
            Ok(mut entries) => {
                entry::sort_entries(&mut entries, self.local.sort, self.local.descending);
                self.local.set_listing(path.display().to_string(), entries);
            }
            Err(e) => self.local.error = Some(format!("{}: {e}", path.display())),
        }
    }

    /// A path typed into the local path field: `~` expands, relative paths are relative to
    /// the folder being shown.
    fn resolve_local_input(&self, input: &str) -> String {
        let input = input.trim();
        let p = paths::expand_tilde(input);
        if p.is_absolute() {
            p
        } else {
            Path::new(&self.local.cwd).join(p)
        }
        .display()
        .to_string()
    }

    /// A path typed into the remote path field: `~` means the home folder, relative paths are
    /// relative to the folder being shown.
    fn resolve_remote_input(&self, input: &str) -> String {
        let input = input.trim();
        if input == "~" {
            ".".into()
        } else if let Some(rest) = input.strip_prefix("~/") {
            format!("./{rest}")
        } else if input.starts_with('/') || self.remote.cwd.is_empty() {
            input.to_string()
        } else {
            remote_join(&self.remote.cwd, input)
        }
    }

    fn remote_cd(&mut self, rt: &tokio::runtime::Handle, ctx: &egui::Context, path: String) {
        let Some(sftp) = self.sftp() else { return };
        self.remote.loading = true;
        self.remote_seq += 1;
        let seq = self.remote_seq;
        let tx = self.events_tx.clone();
        let ctx = ctx.clone();
        rt.spawn(async move {
            let resolved = remote::canonicalize(&sftp, &path).await.unwrap_or(path);
            let result = remote::list(&sftp, &resolved).await;
            let _ = tx.send(Event::Listed {
                seq,
                path: resolved,
                result,
            });
            ctx.request_repaint();
        });
    }

    /// Opens a new session on the same connection to switch root on or off. The working session
    /// is kept until the new one is ready, so a failed switch never drops the working one.
    fn reopen(&mut self, to_root: bool, password: Option<String>) {
        if self.reopening.is_some() {
            return;
        }
        let dir = self.remote.cwd.clone();
        let rx = if to_root {
            self.opener.open_root(password)
        } else {
            self.opener.open()
        };
        self.reopening = Some((rx, to_root, dir));
        self.remote.loading = true;
    }

    fn run_op(
        &mut self,
        rt: &tokio::runtime::Handle,
        ctx: &egui::Context,
        op: impl FnOnce(
            Sftp,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<(), String>> + Send>,
        > + Send
        + 'static,
    ) {
        let Some(sftp) = self.sftp() else { return };
        let tx = self.events_tx.clone();
        let ctx = ctx.clone();
        rt.spawn(async move {
            let result = op(sftp).await;
            let _ = tx.send(Event::OpDone {
                result,
                refresh_remote: true,
            });
            ctx.request_repaint();
        });
    }

    fn start_upload(
        &mut self,
        rt: &tokio::runtime::Handle,
        ctx: &egui::Context,
        local: PathBuf,
        remote_dir: &str,
    ) {
        let Some(sftp) = self.sftp() else { return };
        let tx = self.events_tx.clone();
        let ctx2 = ctx.clone();
        let started = self.transfers.upload(rt, sftp, local, remote_dir, move || {
            let _ = tx.send(Event::OpDone {
                result: Ok(()),
                refresh_remote: true,
            });
            ctx2.request_repaint();
        });
        match started {
            Ok(_) => self.show_activity = true,
            Err(dest) => self.error(tf("{} is already being transferred.", &[&dest])),
        }
    }

    fn start_download(
        &mut self,
        rt: &tokio::runtime::Handle,
        ctx: &egui::Context,
        remote_path: String,
        local_dir: &Path,
    ) {
        let Some(sftp) = self.sftp() else { return };
        let tx = self.events_tx.clone();
        let ctx2 = ctx.clone();
        let started = self
            .transfers
            .download(rt, sftp, remote_path, local_dir, move || {
                let _ = tx.send(Event::OpDone {
                    result: Ok(()),
                    refresh_remote: false,
                });
                ctx2.request_repaint();
            });
        match started {
            Ok(_) => self.show_activity = true,
            Err(dest) => self.error(tf("{} is already being transferred.", &[&dest])),
        }
    }

    /// Progress of the current transfer batch.
    pub fn transfer_summary(&self) -> Option<Summary> {
        self.transfers.summary()
    }

    /// A finished transfer batch to announce, once.
    pub fn take_notice(&mut self) -> Option<(String, bool)> {
        self.notice.take()
    }

    /// Processes finished operations and listings; also called while the panel is hidden, so
    /// transfers finishing in the background refresh the lists and are announced.
    pub fn poll(&mut self, rt: &tokio::runtime::Handle, ctx: &egui::Context) {
        if let Session::Opening(rx) = &mut self.session {
            match rx.try_recv() {
                Ok(Ok(sftp)) => {
                    self.session = Session::Ready(Arc::new(sftp));
                    let start = if self.start_dir.is_empty() {
                        ".".to_string()
                    } else {
                        self.start_dir.clone()
                    };
                    let start = if start == "~" || start.starts_with("~/") {
                        start.replacen('~', ".", 1)
                    } else {
                        start
                    };
                    self.remote_cd(rt, ctx, start);
                }
                Ok(Err(e)) => self.session = Session::Failed(e),
                Err(oneshot::error::TryRecvError::Empty) => {}
                Err(oneshot::error::TryRecvError::Closed) => {
                    self.session = Session::Failed(t("Not connected.").into())
                }
            }
        }
        // Switching root on or off: swap to the new session only once it is ready.
        if self.reopening.is_some() {
            let recv = self.reopening.as_mut().unwrap().0.try_recv();
            match recv {
                Ok(result) => {
                    let (_, to_root, dir) = self.reopening.take().unwrap();
                    self.remote.loading = false;
                    match result {
                        Ok(sftp) => {
                            self.session = Session::Ready(Arc::new(sftp));
                            self.is_root = to_root;
                            let dir = if dir.is_empty() { ".".to_string() } else { dir };
                            self.remote_cd(rt, ctx, dir);
                        }
                        Err(e) => self.error(if to_root {
                            tf("Could not switch to root: {}", &[&e])
                        } else {
                            tf("Could not switch back to your user: {}", &[&e])
                        }),
                    }
                }
                Err(oneshot::error::TryRecvError::Empty) => {}
                Err(oneshot::error::TryRecvError::Closed) => {
                    self.reopening = None;
                    self.remote.loading = false;
                    self.error(t("Not connected."));
                }
            }
        }
        if matches!(self.session, Session::Failed(_)) {
            self.pending_uploads.clear();
        }
        let mut refresh_local = false;
        while let Ok(ev) = self.events_rx.try_recv() {
            match ev {
                Event::Listed { seq, path, result } if seq == self.remote_seq => {
                    self.remote.loading = false;
                    match result {
                        Ok(mut entries) => {
                            entry::sort_entries(
                                &mut entries,
                                self.remote.sort,
                                self.remote.descending,
                            );
                            if path != self.remote.cwd {
                                // A menu opened on the old listing must not act on the new one.
                                egui::Popup::close_all(ctx);
                            }
                            self.remote.set_listing(path, entries);
                        }
                        Err(e) if self.remote.cwd.is_empty() && path != "." => {
                            // The start folder can't be opened: fall back to the home folder.
                            self.error(tf(
                                "Could not open {} ({}); showing the home folder instead.",
                                &[&path, &e],
                            ));
                            self.remote_cd(rt, ctx, ".".into());
                        }
                        Err(e) => {
                            self.remote.error = Some(e);
                            self.remote.path_edit = self.remote.cwd.clone();
                        }
                    }
                }
                Event::Listed { .. } => {}
                Event::OpDone {
                    result,
                    refresh_remote,
                } => {
                    if let Err(e) = result {
                        self.error(e);
                    }
                    self.refresh_remote_pending |= refresh_remote;
                    refresh_local = true;
                }
            }
        }
        // One refresh per frame however many operations finished. A navigation in flight
        // already brings a fresh listing, so it is not interrupted.
        if std::mem::take(&mut self.refresh_remote_pending)
            && !self.remote.loading
            && !self.remote.cwd.is_empty()
        {
            let cwd = self.remote.cwd.clone();
            self.remote_cd(rt, ctx, cwd);
        }
        if refresh_local {
            let cwd = self.local.cwd.clone();
            self.local_cd(&cwd);
        }
        if let Some(s) = self.transfers.summary()
            && !s.running()
            && s.batch != self.reported_batch
        {
            self.reported_batch = s.batch;
            self.notice = batch_notice(&s);
        }
        if self.is_ready()
            && self.dialog.is_none()
            && !self.remote.cwd.is_empty()
            && !self.pending_uploads.is_empty()
        {
            let paths = std::mem::take(&mut self.pending_uploads);
            self.upload_paths(rt, ctx, paths);
        }
    }

    pub fn show(
        &mut self,
        ui: &mut Ui,
        rt: &tokio::runtime::Handle,
        input_enabled: bool,
    ) -> PanelOutput {
        let ctx = ui.ctx().clone();
        self.poll(rt, &ctx);
        let mut output = PanelOutput::None;
        if self.transfers.any_running() {
            ctx.request_repaint_after(Duration::from_millis(250));
        }

        match &self.session {
            Session::Opening(_) => {
                let p = style::pal(ui);
                ui.vertical_centered(|ui| {
                    ui.add_space((ui.available_height() * 0.4).max(8.0));
                    ui.add(egui::Spinner::new().size(18.0).color(p.text_muted));
                    ui.add_space(6.0);
                    ui.label(RichText::new(t("Opening SFTP…")).color(p.text_muted));
                });
                return output;
            }
            Session::Failed(e) => {
                let e = e.clone();
                let p = style::pal(ui);
                ui.vertical_centered(|ui| {
                    ui.add_space((ui.available_height() * 0.3).max(8.0));
                    kit::icon_badge(ui, icons::WARNING_CIRCLE, p.err, 40.0);
                    ui.add_space(6.0);
                    ui.label(RichText::new(tf("SFTP unavailable: {}", &[&e])).color(p.text));
                });
                return output;
            }
            Session::Ready(_) => {}
        }

        // The two file lists fill the whole panel; transfers and errors float in a popup over
        // the bottom-right corner, so the lists are never cut short.
        let mut local_actions = Vec::new();
        let mut remote_actions = Vec::new();
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let lists_height = ui.available_height();
                let half = ((ui.available_width() - 9.0 - 2.0 * ui.spacing().item_spacing.x) / 2.0)
                    .max(120.0);
                ui.horizontal_top(|ui| {
                    ui.allocate_ui(egui::vec2(half, lists_height), |ui| {
                        local_actions = self.side_ui(ui, false, input_enabled);
                    });
                    // A separator as tall as the lists (`ui.separator()` would take all the
                    // height left).
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(9.0, lists_height), Sense::hover());
                    ui.painter().vline(
                        rect.center().x,
                        rect.y_range(),
                        egui::Stroke::new(1.0, style::pal(ui).border),
                    );
                    ui.allocate_ui(egui::vec2(half, lists_height), |ui| {
                        remote_actions = self.side_ui(ui, true, input_enabled);
                    });
                });
            });
        for a in local_actions.into_iter().chain(remote_actions) {
            if let Some(out) = self.apply(a, rt, &ctx) {
                output = out;
            }
        }
        self.activity_ui(&ctx);
        self.drag_preview(&ctx);
        self.dialog_ui(&ctx, rt);
        let sftp = self.sftp();
        let editor_out = self
            .editor
            .as_mut()
            .map(|e| (e.show(&ctx, rt, sftp), e.path.clone()));
        match editor_out {
            Some((EditorOutput::Close { saved }, _)) => {
                self.editor = None;
                if saved {
                    self.apply(SideAction::Refresh(true), rt, &ctx);
                }
            }
            Some((EditorOutput::Download, path)) => {
                self.editor = None;
                let dir = remote_parent(&path);
                let name = entry::remote_name(&path).to_string();
                self.apply(
                    SideAction::Transfer {
                        from_remote: true,
                        dir,
                        names: vec![name],
                    },
                    rt,
                    &ctx,
                );
            }
            Some((EditorOutput::Open, _)) | None => {}
        }
        output
    }

    fn side_ui(&mut self, ui: &mut Ui, is_remote: bool, input_enabled: bool) -> Vec<SideAction> {
        let mut actions = Vec::new();
        let uid = self.uid;
        let menu_target = self.menu_target.clone().filter(|m| m.remote == is_remote);
        let is_root_now = self.is_root;
        let reopening = self.reopening.is_some();
        let side = if is_remote {
            &mut self.remote
        } else {
            &mut self.local
        };
        let show_hidden = side.show_hidden;
        let mut path_focused = false;
        ui.vertical(|ui| {
            // Header: which side this is, then the navigation and the folder actions as two
            // grouped controls (bordered, so they read as buttons, without a row of boxes), and
            // on the remote side the Root and Upload buttons at the right.
            let p = style::pal(ui);
            ui.horizontal(|ui| {
                ui.set_height(style::CONTROL_H);
                ui.spacing_mut().item_spacing.x = 6.0;
                kit::icon(
                    ui,
                    if is_remote { icons::HARD_DRIVES } else { icons::LAPTOP },
                    15.0,
                    p.text_muted,
                );
                ui.label(
                    RichText::new(if is_remote { t("Remote") } else { t("Local") })
                        .font(style::semibold(13.0))
                        .color(p.text),
                );
                ui.add_space(4.0);
                let nav = kit::button_group(
                    ui,
                    &[
                        Segment {
                            icon: icons::ARROW_UP,
                            tooltip: t("Parent folder"),
                            on: false,
                        },
                        Segment {
                            icon: icons::HOUSE,
                            tooltip: t("Home folder"),
                            on: false,
                        },
                        Segment {
                            icon: icons::ARROWS_CLOCKWISE,
                            tooltip: t("Refresh"),
                            on: false,
                        },
                    ],
                );
                if nav[0].clicked() {
                    actions.push(SideAction::Up(is_remote));
                }
                if nav[1].clicked() {
                    actions.push(SideAction::Home(is_remote));
                }
                if nav[2].clicked() {
                    actions.push(SideAction::Refresh(is_remote));
                }
                let folder = kit::button_group(
                    ui,
                    &[
                        Segment {
                            icon: icons::FOLDER_PLUS,
                            tooltip: t("New folder"),
                            on: false,
                        },
                        Segment {
                            icon: if show_hidden { icons::EYE } else { icons::EYE_SLASH },
                            tooltip: t("Show hidden files"),
                            on: show_hidden,
                        },
                    ],
                );
                #[cfg(debug_assertions)]
                crate::devtest::record(
                    if is_remote {
                        "remote-hidden"
                    } else {
                        "local-hidden"
                    },
                    folder[1].rect,
                );
                if folder[0].clicked() {
                    actions.push(SideAction::Mkdir(is_remote));
                }
                if folder[1].clicked() {
                    actions.push(SideAction::ToggleHidden(is_remote));
                }
                if side.loading {
                    ui.add(egui::Spinner::new().size(14.0).color(p.text_muted));
                }
                if is_remote {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        let upload = kit::button_ex(
                            ui,
                            kit::Kind::Secondary,
                            Some(icons::UPLOAD_SIMPLE),
                            t("Upload…"),
                            true,
                        )
                        .on_hover_text(t("Choose files on this Mac to upload into this folder"));
                        #[cfg(debug_assertions)]
                        crate::devtest::record("upload-button", upload.rect);
                        egui::Popup::menu(&upload).show(|ui| {
                            if ui.button(kit::with_icon(icons::FILES, t("Files…"))).clicked() {
                                actions.push(SideAction::PickUpload { folders: false });
                                ui.close();
                            }
                            if ui.button(kit::with_icon(icons::FOLDERS, t("Folders…"))).clicked() {
                                actions.push(SideAction::PickUpload { folders: true });
                                ui.close();
                            }
                        });
                        // Browse and transfer as root through sudo.
                        let root = ui
                            .add_enabled_ui(!reopening, |ui| {
                                kit::button_full(
                                    ui,
                                    kit::Kind::Secondary,
                                    Some(icons::SHIELD_CHECK),
                                    t("Root"),
                                    true,
                                    is_root_now,
                                )
                            })
                            .inner
                            .on_hover_text(if is_root_now {
                                t("Running as root — click to go back to your user")
                            } else {
                                t("Browse and transfer as root (sudo)")
                            });
                        #[cfg(debug_assertions)]
                        crate::devtest::record("sftp-root", root.rect);
                        if root.clicked() {
                            actions.push(SideAction::Root);
                        }
                    });
                }
            });
            ui.add_space(2.0);
            // Path: a full-width field with a folder icon inside it.
            let width = ui.available_width();
            let edit = kit::field_with_icon(
                ui,
                egui::Id::new(("sftp-path", uid, is_remote)),
                &mut side.path_edit,
                "",
                width,
                icons::FOLDER_OPEN,
                false,
            );
            if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                actions.push(SideAction::Cd(is_remote, side.path_edit.clone()));
            }
            path_focused = edit.has_focus();
            if let Some(err) = &side.error {
                ui.horizontal(|ui| {
                    kit::icon(ui, icons::WARNING_CIRCLE, 14.0, p.err);
                    ui.add(egui::Label::new(RichText::new(err).color(p.err)).wrap());
                });
            }
            // A quiet line summarising the selection, only while something is selected.
            let sel: Vec<&FileEntry> = side
                .entries
                .iter()
                .filter(|e| side.selected.contains(&e.name) && (show_hidden || !e.is_hidden()))
                .collect();
            if !sel.is_empty() {
                let bytes: u64 = sel.iter().filter(|e| !e.is_dir).map(|e| e.size).sum();
                let folders = sel.iter().filter(|e| e.is_dir).count();
                let mut text = tf("{} selected", &[&sel.len().to_string()]);
                if bytes > 0 {
                    text.push_str(&format!(" · {}", entry::human_size(bytes)));
                }
                if folders > 0 {
                    text.push_str(&format!(
                        " · {}",
                        tf("{} folder(s)", &[&folders.to_string()])
                    ));
                }
                ui.label(RichText::new(text).small().color(p.text_muted));
            }
            ui.add_space(2.0);

            let visible: Vec<usize> = side
                .entries
                .iter()
                .enumerate()
                .filter(|(_, e)| show_hidden || !e.is_hidden())
                .map(|(i, _)| i)
                .collect();
            let row_h = 26.0;
            // The list fills all the height left under the header, right to the bottom.
            let list_size = egui::vec2(ui.available_width(), ui.available_height().max(60.0));
            let list = ui.allocate_ui(list_size, |ui| {
                file_drop_zone(ui, is_remote, |ui| {
                    ui.set_min_size(ui.available_size());
                    let mut sort_clicked = None;
                    // The list always reaches the bottom of the panel, and its columns follow the
                    // panel's width (resized columns would otherwise keep their old widths and leave
                    // a gap, or overflow, after the window or the split changes size).
                    let body_height = (ui.available_height() - row_h - 6.0).max(0.0);
                    let width = ui.available_width();
                    let relayout = (width - side.table_width).abs() > 0.5;
                    side.table_width = width;
                    let mut table = TableBuilder::new(ui)
                        .id_salt(if is_remote {
                            "sftp-remote"
                        } else {
                            "sftp-local"
                        })
                        .striped(false)
                        // Rows take both clicks and drags: a separate drag-only area on top of a
                        // row would swallow its clicks.
                        .sense(Sense::click_and_drag())
                        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                        .resizable(true)
                        .auto_shrink([false, false])
                        .min_scrolled_height(body_height)
                        .max_scroll_height(f32::INFINITY)
                        .column(Column::remainder().at_least(120.0).clip(true))
                        .column(Column::initial(70.0).at_least(50.0))
                        .column(Column::initial(118.0).at_least(60.0).clip(true));
                    if is_remote {
                        table = table.column(Column::initial(80.0).at_least(60.0));
                    }
                    if relayout {
                        table.reset();
                    }
                    let arrow = |k: SortKey| {
                        if side.sort == k {
                            if side.descending {
                                format!("  {}", icons::CARET_DOWN)
                            } else {
                                format!("  {}", icons::CARET_UP)
                            }
                        } else {
                            String::new()
                        }
                    };
                    let header_button =
                        |ui: &mut Ui, label: &str, key: SortKey, clicked: &mut Option<SortKey>| {
                            let color = if side.sort == key { p.text } else { p.text_muted };
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new(format!("{label}{}", arrow(key)))
                                            .font(style::medium(style::SMALL + 0.5))
                                            .color(color),
                                    )
                                    .frame(false),
                                )
                                .clicked()
                            {
                                *clicked = Some(key);
                            }
                        };
                    let mut header_rect = egui::Rect::NOTHING;
                    table
                        .header(24.0, |mut header| {
                            header.col(|ui| {
                                header_button(ui, t("Name"), SortKey::Name, &mut sort_clicked)
                            });
                            header.col(|ui| {
                                header_button(ui, t("Size"), SortKey::Size, &mut sort_clicked)
                            });
                            header.col(|ui| {
                                header_button(
                                    ui,
                                    t("Modified"),
                                    SortKey::Modified,
                                    &mut sort_clicked,
                                )
                            });
                            if is_remote {
                                header.col(|ui| {
                                    // Same look and baseline as the sortable headers.
                                    ui.add(
                                        egui::Button::new(
                                            RichText::new(t("Permissions"))
                                                .font(style::medium(style::SMALL + 0.5))
                                                .color(p.text_muted),
                                        )
                                        .frame(false)
                                        .sense(Sense::hover()),
                                    );
                                });
                            }
                            header_rect = header.response().rect;
                        })
                        .body(|body| {
                            body.rows(row_h, visible.len(), |mut row| {
                                let e = &side.entries[visible[row.index()]];
                                let selected = side.selected.contains(&e.name);
                                row.set_selected(selected);
                                // Names in the text color, details a step quieter (as in
                                // Finder); a selected row is all in the selection's text color.
                                let detail = |_: &Ui| {
                                    if selected { p.accent_text } else { p.text_muted }
                                };
                                row.col(|ui| {
                                    ui.add_space(4.0);
                                    let (icon, color) =
                                        icons::file_icon(&e.name, e.is_dir, e.is_symlink, &p);
                                    let color = if selected && !e.is_dir { p.accent_text } else { color };
                                    icons::icon_label(ui, icon, color, &e.name);
                                });
                                row.col(|ui| {
                                    if !e.is_dir {
                                        let size = entry::human_size(e.size);
                                        ui.label(RichText::new(size).color(detail(ui)));
                                    }
                                });
                                row.col(|ui| {
                                    let time =
                                        e.modified.map(entry::format_time).unwrap_or_default();
                                    ui.label(RichText::new(time).color(detail(ui)));
                                });
                                if is_remote {
                                    row.col(|ui| {
                                        let mode =
                                            e.mode.map(entry::mode_string).unwrap_or_default();
                                        ui.label(RichText::new(mode).monospace().color(detail(ui)));
                                    });
                                }
                                let resp = row.response();
                                #[cfg(debug_assertions)]
                                crate::devtest::record(
                                    format!(
                                        "{}-row:{}",
                                        if is_remote { "remote" } else { "local" },
                                        e.name
                                    ),
                                    resp.rect,
                                );
                                // The names a drag or menu acts on: the selection if this row is
                                // part of it, otherwise just this row.
                                let names_here = || -> Vec<String> {
                                    if side.selected.contains(&e.name) {
                                        let mut v: Vec<String> =
                                            side.selected.iter().cloned().collect();
                                        v.sort();
                                        v
                                    } else {
                                        vec![e.name.clone()]
                                    }
                                };
                                if resp.drag_started() {
                                    let names = names_here();
                                    actions.push(SideAction::DragStarted(if names.len() == 1 {
                                        names[0].clone()
                                    } else {
                                        tf("{} items", &[&names.len().to_string()])
                                    }));
                                    egui::DragAndDrop::set_payload(
                                        &resp.ctx,
                                        DragFiles {
                                            from_remote: is_remote,
                                            dir: side.cwd.clone(),
                                            names,
                                        },
                                    );
                                }
                                if resp.double_clicked() {
                                    // A remote file opens in the editor; folders open, and local
                                    // files upload.
                                    actions.push(if is_remote && !e.is_dir {
                                        SideAction::Edit {
                                            dir: side.cwd.clone(),
                                            name: e.name.clone(),
                                        }
                                    } else {
                                        SideAction::Open {
                                            remote: is_remote,
                                            dir: side.cwd.clone(),
                                            entry: e.clone(),
                                        }
                                    });
                                } else if resp.clicked() {
                                    let mods = ui_mods(&resp);
                                    actions.push(SideAction::Select(
                                        is_remote,
                                        e.name.clone(),
                                        mods.0,
                                        mods.1,
                                    ));
                                }
                                if resp.secondary_clicked() {
                                    actions.push(SideAction::MenuOpened(MenuTarget {
                                        remote: is_remote,
                                        dir: side.cwd.clone(),
                                        names: names_here(),
                                        entry: e.clone(),
                                    }));
                                }
                                resp.context_menu(|ui| {
                                    // Act on what was right-clicked, even if the listing changed since.
                                    let target =
                                        menu_target.clone().unwrap_or_else(|| MenuTarget {
                                            remote: is_remote,
                                            dir: side.cwd.clone(),
                                            names: names_here(),
                                            entry: e.clone(),
                                        });
                                    if is_remote
                                        && !target.entry.is_dir
                                        && target.names.len() == 1
                                        && ui.button(kit::with_icon(icons::PENCIL_SIMPLE, t("Edit"))).clicked()
                                    {
                                        actions.push(SideAction::Edit {
                                            dir: target.dir.clone(),
                                            name: target.entry.name.clone(),
                                        });
                                        ui.close();
                                    }
                                    let label = if is_remote {
                                        t("Download")
                                    } else {
                                        t("Upload")
                                    };
                                    let icon = if is_remote {
                                        icons::DOWNLOAD_SIMPLE
                                    } else {
                                        icons::UPLOAD_SIMPLE
                                    };
                                    if ui.button(kit::with_icon(icon, label)).clicked() {
                                        actions.push(SideAction::Transfer {
                                            from_remote: is_remote,
                                            dir: target.dir.clone(),
                                            names: target.names.clone(),
                                        });
                                        ui.close();
                                    }
                                    if target.entry.is_dir
                                        && ui.button(kit::with_icon(icons::FOLDER_OPEN, t("Open"))).clicked()
                                    {
                                        actions.push(SideAction::Open {
                                            remote: is_remote,
                                            dir: target.dir.clone(),
                                            entry: target.entry.clone(),
                                        });
                                        ui.close();
                                    }
                                    if ui.button(kit::with_icon(icons::LIST, t("Select All"))).clicked() {
                                        actions.push(SideAction::SelectAll(is_remote));
                                        ui.close();
                                    }
                                    ui.separator();
                                    if ui
                                        .add_enabled(
                                            target.names.len() == 1,
                                            egui::Button::new(kit::with_icon(icons::PENCIL_SIMPLE, t("Rename…"))),
                                        )
                                        .clicked()
                                    {
                                        actions.push(SideAction::Rename {
                                            remote: is_remote,
                                            dir: target.dir.clone(),
                                            name: target.entry.name.clone(),
                                        });
                                        ui.close();
                                    }
                                    if is_remote {
                                        let can_chmod =
                                            target.names.len() == 1 && target.entry.mode.is_some();
                                        if ui
                                            .add_enabled(
                                                can_chmod,
                                                egui::Button::new(kit::with_icon(icons::LOCK_KEY, t("Permissions…"))),
                                            )
                                            .clicked()
                                        {
                                            actions.push(SideAction::Chmod {
                                                path: remote_join(&target.dir, &target.entry.name),
                                                mode: target.entry.mode.unwrap_or(0),
                                            });
                                            ui.close();
                                        }
                                    }
                                    if ui
                                        .button(
                                            RichText::new(kit::with_icon(icons::TRASH, t("Delete…")))
                                                .color(p.err),
                                        )
                                        .clicked()
                                    {
                                        actions.push(SideAction::Delete {
                                            remote: is_remote,
                                            dir: target.dir.clone(),
                                            names: target.names.clone(),
                                        });
                                        ui.close();
                                    }
                                    ui.separator();
                                    if ui.button(kit::with_icon(icons::CLIPBOARD, t("Copy Path"))).clicked() {
                                        let p = if is_remote {
                                            remote_join(&target.dir, &target.entry.name)
                                        } else {
                                            Path::new(&target.dir)
                                                .join(&target.entry.name)
                                                .display()
                                                .to_string()
                                        };
                                        ui.ctx().copy_text(p);
                                        ui.close();
                                    }
                                    if !is_remote
                                        && ui
                                            .button(kit::with_icon(icons::ARROW_SQUARE_OUT, t("Show in Finder")))
                                            .clicked()
                                    {
                                        let p = Path::new(&target.dir).join(&target.entry.name);
                                        let _ = std::process::Command::new("open")
                                            .arg("-R")
                                            .arg(p)
                                            .spawn();
                                        ui.close();
                                    }
                                });
                            });
                        });
                    // A hairline between the column titles and the files.
                    if header_rect.is_positive() {
                        let y = header_rect.bottom() + 0.5 * ui.spacing().item_spacing.y;
                        ui.painter().hline(
                            ui.max_rect().x_range(),
                            y,
                            egui::Stroke::new(1.0, p.border),
                        );
                    }
                    if let Some(k) = sort_clicked {
                        actions.push(SideAction::Sort(is_remote, k));
                    }
                })
            });
            let list_hovered = list.response.contains_pointer();
            let (_, dropped) = list.inner;
            // ⌘A over the list selects everything shown.
            if list_hovered
                && !path_focused
                && ui.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::A))
            {
                actions.push(SideAction::SelectAll(is_remote));
            }
            if let Some(drop) = dropped
                && drop.from_remote != is_remote
            {
                actions.push(SideAction::Transfer {
                    from_remote: drop.from_remote,
                    dir: drop.dir.clone(),
                    names: drop.names.clone(),
                });
            }
            let _ = input_enabled;
        });
        actions
    }

    fn apply(
        &mut self,
        action: SideAction,
        rt: &tokio::runtime::Handle,
        ctx: &egui::Context,
    ) -> Option<PanelOutput> {
        match action {
            SideAction::DragStarted(label) => self.drag_label = Some(label),
            SideAction::MenuOpened(target) => {
                let side = if target.remote {
                    &mut self.remote
                } else {
                    &mut self.local
                };
                if !side.selected.contains(&target.entry.name) {
                    side.selected = HashSet::from([target.entry.name.clone()]);
                    side.anchor = Some(target.entry.name.clone());
                }
                self.menu_target = Some(target);
            }
            SideAction::Root => {
                if self.is_root {
                    self.reopen(false, None);
                } else {
                    self.dialog = Some(Dialog::Sudo {
                        password: String::new(),
                    });
                    self.focus_dialog = true;
                }
            }
            SideAction::ToggleHidden(remote) => {
                let side = if remote {
                    &mut self.remote
                } else {
                    &mut self.local
                };
                side.show_hidden = !side.show_hidden;
                if !side.show_hidden {
                    side.selected.retain(|n| !n.starts_with('.'));
                }
            }
            SideAction::Up(true) => {
                if !self.remote.cwd.is_empty() {
                    let p = remote_parent(&self.remote.cwd);
                    self.remote_cd(rt, ctx, p);
                }
            }
            SideAction::Up(false) => {
                if let Some(p) = Path::new(&self.local.cwd)
                    .parent()
                    .map(|p| p.display().to_string())
                {
                    self.local_cd(&p);
                    return Some(PanelOutput::LocalDir(self.local.cwd.clone()));
                }
            }
            SideAction::Home(true) => self.remote_cd(rt, ctx, ".".into()),
            SideAction::Home(false) => {
                self.local_cd("~");
                return Some(PanelOutput::LocalDir(self.local.cwd.clone()));
            }
            SideAction::PickUpload { folders } => {
                let picker = rfd::FileDialog::new()
                    .set_title(tf("Upload to {}", &[&self.remote.cwd]))
                    .set_directory(&self.local.cwd);
                let picked = if folders {
                    picker.pick_folders()
                } else {
                    picker.pick_files()
                };
                if let Some(paths) = picked.filter(|p| !p.is_empty()) {
                    self.upload_paths(rt, ctx, paths);
                }
            }
            SideAction::SelectAll(remote) => {
                let side = if remote {
                    &mut self.remote
                } else {
                    &mut self.local
                };
                side.selected = side
                    .visible(side.show_hidden)
                    .map(|e| e.name.clone())
                    .collect();
                side.anchor = None;
            }
            SideAction::Edit { dir, name } => match self.sftp() {
                Some(sftp) => {
                    self.editor = Some(FileEditor::open(rt, ctx, sftp, remote_join(&dir, &name)));
                }
                None => self.error(t("Not connected.")),
            },
            SideAction::Refresh(true) => {
                if !self.remote.cwd.is_empty() {
                    let cwd = self.remote.cwd.clone();
                    self.remote_cd(rt, ctx, cwd);
                }
            }
            SideAction::Refresh(false) => {
                let cwd = self.local.cwd.clone();
                self.local_cd(&cwd);
            }
            SideAction::Cd(true, p) => {
                let target = self.resolve_remote_input(&p);
                self.remote_cd(rt, ctx, target);
            }
            SideAction::Cd(false, p) => {
                let target = self.resolve_local_input(&p);
                self.local_cd(&target);
                return Some(PanelOutput::LocalDir(self.local.cwd.clone()));
            }
            SideAction::Mkdir(remote) => {
                let dir = if remote {
                    self.remote.cwd.clone()
                } else {
                    self.local.cwd.clone()
                };
                if dir.is_empty() {
                    self.error(t("Open a remote folder first."));
                } else {
                    self.dialog = Some(Dialog::Mkdir {
                        remote,
                        dir,
                        name: String::new(),
                    });
                    self.focus_dialog = true;
                }
            }
            SideAction::Open {
                remote: true,
                dir,
                entry,
            } if entry.is_dir => {
                self.remote_cd(rt, ctx, remote_join(&dir, &entry.name));
            }
            SideAction::Open {
                remote: false,
                dir,
                entry,
            } if entry.is_dir => {
                let p = Path::new(&dir).join(&entry.name).display().to_string();
                self.local_cd(&p);
                return Some(PanelOutput::LocalDir(self.local.cwd.clone()));
            }
            SideAction::Open { remote, dir, entry } => {
                return self.apply(
                    SideAction::Transfer {
                        from_remote: remote,
                        dir,
                        names: vec![entry.name],
                    },
                    rt,
                    ctx,
                );
            }
            SideAction::Select(remote, name, toggle, range) => {
                let side = if remote {
                    &mut self.remote
                } else {
                    &mut self.local
                };
                let names: Vec<String> = side
                    .visible(side.show_hidden)
                    .map(|e| e.name.clone())
                    .collect();
                let anchor_pos = side
                    .anchor
                    .as_ref()
                    .and_then(|a| names.iter().position(|n| n == a));
                match (range, anchor_pos, names.iter().position(|n| *n == name)) {
                    (true, Some(a), Some(b)) => {
                        side.selected = names[a.min(b)..=a.max(b)].iter().cloned().collect();
                    }
                    _ if toggle => {
                        if !side.selected.remove(&name) {
                            side.selected.insert(name.clone());
                        }
                        side.anchor = Some(name);
                    }
                    _ => {
                        side.selected = HashSet::from([name.clone()]);
                        side.anchor = Some(name);
                    }
                }
            }
            SideAction::Sort(remote, key) => {
                let side = if remote {
                    &mut self.remote
                } else {
                    &mut self.local
                };
                if side.sort == key {
                    side.descending = !side.descending;
                } else {
                    side.sort = key;
                    side.descending = false;
                }
                entry::sort_entries(&mut side.entries, side.sort, side.descending);
            }
            SideAction::Transfer {
                from_remote: true,
                dir,
                names,
            } => {
                let sources: Vec<String> = names.iter().map(|n| remote_join(&dir, n)).collect();
                let dest = self.local.cwd.clone();
                self.download_sources(rt, ctx, sources, dest);
            }
            SideAction::Transfer {
                from_remote: false,
                dir,
                names,
            } => {
                let sources: Vec<String> = names
                    .iter()
                    .map(|n| Path::new(&dir).join(n).display().to_string())
                    .collect();
                let dest = self.remote.cwd.clone();
                self.upload_sources(rt, ctx, sources, dest);
            }
            SideAction::Rename { remote, dir, name } => {
                self.dialog = Some(Dialog::Rename {
                    remote,
                    dir,
                    from: name.clone(),
                    to: name,
                });
                self.focus_dialog = true;
            }
            SideAction::Delete { remote, dir, names } => {
                self.dialog = Some(Dialog::Delete { remote, dir, names })
            }
            SideAction::Chmod { path, mode } => {
                self.dialog = Some(Dialog::Chmod {
                    path,
                    original: mode & 0o7777,
                    mode: format!("{:o}", mode & 0o7777),
                });
                self.focus_dialog = true;
            }
        }
        None
    }

    fn drag_preview(&mut self, ctx: &egui::Context) {
        if !egui::DragAndDrop::has_payload_of_type::<DragFiles>(ctx) {
            self.drag_label = None;
            return;
        }
        let (Some(label), Some(pos)) = (&self.drag_label, ctx.pointer_interact_pos()) else {
            return;
        };
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Tooltip,
            egui::Id::new("sftp-drag"),
        ));
        let visuals = ctx.global_style().visuals.clone();
        let p = style::palette(visuals.dark_mode);
        let galley = painter.layout_no_wrap(
            format!("{}  {label}", icons::FILES),
            style::medium(13.0),
            p.text,
        );
        let r = egui::Rect::from_min_size(
            pos + egui::vec2(14.0, 6.0),
            galley.size() + egui::vec2(18.0, 10.0),
        );
        painter.add(visuals.popup_shadow.as_shape(r, egui::CornerRadius::same(6)));
        painter.rect(
            r,
            egui::CornerRadius::same(6),
            p.surface,
            egui::Stroke::new(1.0, p.border_strong),
            egui::StrokeKind::Inside,
        );
        painter.galley(r.min + egui::vec2(9.0, 5.0), galley, p.text);
    }

    /// The floating popup for transfers and errors: it opens on new activity and can be hidden
    /// (collapsed to its title bar) or closed. A closed popup comes back from the pill in the
    /// corner while anything is still going on, or on the next transfer or error.
    fn activity_ui(&mut self, ctx: &egui::Context) {
        let summary = self.transfers.summary();
        let running = summary.as_ref().filter(|s| s.running()).cloned();
        let has_items = !self.transfers.items.is_empty();
        let has_msg = self.message.is_some();
        if !(has_items || has_msg) {
            self.show_activity = false;
            return;
        }
        // Closed, but something is still happening: a small pill to bring the popup back.
        if !self.show_activity {
            let pill = egui::Area::new(egui::Id::new(("sftp-activity-pill", self.uid)))
                .anchor(egui::Align2::RIGHT_BOTTOM, ACTIVITY_OFFSET)
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    let p = style::pal(ui);
                    let (label, color) = match &running {
                        Some(s) => (format!("{}  {}", icons::ARROWS_DOWN_UP, summary_title(s)), p.accent_text),
                        None if has_msg => (format!("{}  {}", icons::WARNING_CIRCLE, t("Details")), p.warn),
                        None => (format!("{}  {}", icons::ARROWS_DOWN_UP, t("Transfers")), p.text),
                    };
                    egui::Frame::popup(ui.style())
                        .corner_radius(egui::CornerRadius::same(16))
                        .inner_margin(egui::Margin::symmetric(10, 4))
                        .show(ui, |ui| {
                            ui.add(
                                egui::Button::new(
                                    RichText::new(label).font(style::medium(12.5)).color(color),
                                )
                                .frame(false),
                            )
                        })
                        .inner
                });
            if pill.inner.clicked() {
                self.show_activity = true;
            }
            return;
        }

        let title = match &running {
            Some(_) => t("Transfers"),
            None => t("Activity"),
        };
        let mut open = true;
        let frame = egui::Frame::window(&ctx.global_style())
            .inner_margin(egui::Margin::symmetric(14, 10));
        egui::Window::new(title)
            .id(egui::Id::new(("sftp-activity", self.uid)))
            .title_bar(false)
            .frame(frame)
            .resizable(true)
            .anchor(egui::Align2::RIGHT_BOTTOM, ACTIVITY_OFFSET)
            .default_width(460.0)
            .min_width(320.0)
            .max_height(340.0)
            .show(ctx, |ui| {
                let p = style::pal(ui);
                // Header: what this is, and a button that hides it (back to the pill).
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let icon = if running.is_some() { icons::ARROWS_DOWN_UP } else { icons::LIST };
                    kit::icon(ui, icon, 15.0, p.text_muted);
                    ui.label(RichText::new(title).font(style::semibold(13.5)).color(p.text));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if kit::icon_button_ex(ui, icons::X, t("Hide"), false, 22.0).clicked() {
                            open = false;
                        }
                    });
                });
                ui.add_space(2.0);
                // The latest error or status, dismissible on its own.
                if let Some((msg, is_err)) = self.message.clone() {
                    let (icon, color) = if is_err {
                        (icons::WARNING_CIRCLE, p.err)
                    } else {
                        (icons::INFO, p.text)
                    };
                    ui.horizontal(|ui| {
                        kit::icon(ui, icon, 15.0, color);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                            if kit::icon_button_ex(ui, icons::X, t("Dismiss"), false, 20.0).clicked() {
                                self.message = None;
                            }
                            ui.add(egui::Label::new(RichText::new(msg).color(color)).wrap());
                        });
                    });
                    if has_items {
                        ui.separator();
                    }
                }
                if !has_items {
                    return;
                }
                // The button goes in first, from the right, so the text can't run under it.
                style::right_to_left(ui, |ui| {
                    if running.is_some() {
                        if kit::ghost(ui, Some(icons::STOP), t("Cancel all")).clicked() {
                            self.transfers.cancel_all();
                        }
                    } else if kit::ghost(ui, Some(icons::BROOM), t("Clear"))
                        .on_hover_text(t("Remove finished transfers from the list"))
                        .clicked()
                    {
                        self.transfers.clear_finished();
                    }
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        match &running {
                            Some(s) => {
                                ui.label(RichText::new(summary_title(s)).font(style::medium(13.0)));
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(summary_detail(s)).small().color(p.text_muted),
                                    )
                                    .truncate(),
                                )
                                .on_hover_text(summary_detail(s));
                            }
                            None => {
                                if let Some(s) = &summary {
                                    ui.label(RichText::new(finished_detail(s)).color(p.text_muted));
                                }
                            }
                        }
                    });
                });
                if let Some(s) = &running {
                    progress_bar(ui, s.fraction(), ui.available_width());
                }
                ui.add_space(2.0);
                let mut remove = None;
                egui::ScrollArea::vertical()
                    .id_salt(("sftp-transfers-list", self.uid))
                    .auto_shrink([false, true])
                    .max_height(240.0)
                    .show(ui, |ui| {
                        for tr in self.transfers.items.iter().rev() {
                            if let Some(id) = transfer_row(ui, tr) {
                                remove = Some(id);
                            }
                        }
                    });
                if let Some(id) = remove {
                    self.transfers.items.retain(|t| t.id != id);
                }
            });
        // The × on the window hides it; the transfers themselves carry on.
        if !open {
            self.show_activity = false;
        }
    }

    fn dialog_ui(&mut self, ctx: &egui::Context, rt: &tokio::runtime::Handle) {
        let Some(dialog) = &mut self.dialog else {
            return;
        };
        let mut close = false;
        let mut confirmed = false;
        let focus = std::mem::take(&mut self.focus_dialog);
        let modal = egui::Modal::new(egui::Id::new("sftp-dialog")).show(ctx, |ui| {
            ui.set_width(360.0);
            match dialog {
                Dialog::Mkdir { name, .. } => {
                    ui.heading(t("New folder"));
                    let r = ui.add(style::input(name).hint_text(t("Folder name")));
                    if focus {
                        r.request_focus();
                    }
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        confirmed = true;
                    }
                }
                Dialog::Rename { to, .. } => {
                    ui.heading(t("Rename"));
                    let r = ui.add(style::input(to));
                    if focus {
                        r.request_focus();
                    }
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        confirmed = true;
                    }
                }
                Dialog::Delete { names, remote, .. } => {
                    ui.heading(t("Delete"));
                    let where_ = if *remote { t("on the server") } else { t("on this Mac") };
                    ui.label(tf(
                        "Delete {} item(s) {}? Folders are deleted with everything in them. This cannot be undone.",
                        &[&names.len().to_string(), where_],
                    ));
                    for n in names.iter().take(8) {
                        ui.label(RichText::new(format!("• {n}")).weak());
                    }
                }
                Dialog::Chmod { path, mode, .. } => {
                    ui.heading(t("Permissions"));
                    ui.label(RichText::new(path.as_str()).weak());
                    let mut bits = entry::parse_mode(mode).unwrap_or(0);
                    egui::Grid::new("chmod").num_columns(4).show(ui, |ui| {
                        ui.label("");
                        ui.label(t("Read"));
                        ui.label(t("Write"));
                        ui.label(t("Execute"));
                        ui.end_row();
                        for (who, shift) in [(t("Owner"), 6), (t("Group"), 3), (t("Others"), 0)] {
                            ui.label(who);
                            for bit in [4u32, 2, 1] {
                                let mask = bit << shift;
                                let mut on = bits & mask != 0;
                                if style::checkbox(ui, &mut on, "").changed() {
                                    bits = if on { bits | mask } else { bits & !mask };
                                    *mode = format!("{:o}", bits);
                                }
                            }
                            ui.end_row();
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label(t("Octal"));
                        ui.add(style::Field(egui::TextEdit::singleline(mode).desired_width(60.0)));
                        ui.monospace(entry::parse_mode(mode).map(entry::mode_string).unwrap_or_else(|| "?".into()));
                    });
                }
                Dialog::Overwrite { conflicts, .. } => {
                    ui.heading(t("Replace existing items?"));
                    ui.label(tf(
                        "{} item(s) already exist at the destination and will be replaced:",
                        &[&conflicts.len().to_string()],
                    ));
                    for n in conflicts.iter().take(8) {
                        ui.label(RichText::new(format!("• {n}")).weak());
                    }
                }
                Dialog::Sudo { password } => {
                    ui.heading(t("Browse as root"));
                    ui.label(t(
                        "Reopen the file panel as root through sudo, to see and transfer files your user has no access to. The terminal stays as your user.",
                    ));
                    ui.add_space(6.0);
                    let r = ui.add(
                        style::input(password)
                            .password(true)
                            .hint_text(t("sudo password (leave empty if not needed)")),
                    );
                    if focus {
                        r.request_focus();
                    }
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        confirmed = true;
                    }
                    ui.add_space(4.0);
                    ui.label(
                        RichText::new(t(
                            "The server must allow sudo for your user. The password is sent only to this server.",
                        ))
                        .weak()
                        .small(),
                    );
                }
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let ok_label = match dialog {
                    Dialog::Delete { .. } => t("Delete"),
                    Dialog::Overwrite { .. } => t("Replace"),
                    Dialog::Mkdir { .. } => t("Create"),
                    Dialog::Sudo { .. } => t("Connect"),
                    _ => t("OK"),
                };
                if ui.button(ok_label).clicked() {
                    confirmed = true;
                }
                if ui.button(t("Cancel")).clicked() {
                    close = true;
                }
            });
        });
        if modal.should_close() {
            close = true;
        }
        if confirmed {
            let dialog = self.dialog.take().unwrap();
            self.confirm(dialog, rt, ctx);
        } else if close {
            self.dialog = None;
        }
    }

    fn confirm(&mut self, dialog: Dialog, rt: &tokio::runtime::Handle, ctx: &egui::Context) {
        match dialog {
            Dialog::Mkdir {
                remote: true,
                dir,
                name,
            } if !name.trim().is_empty() => {
                let path = remote_join(&dir, name.trim());
                self.run_op(rt, ctx, move |s| {
                    Box::pin(async move { remote::mkdir(&s, &path).await })
                });
            }
            Dialog::Mkdir {
                remote: false,
                dir,
                name,
            } if !name.trim().is_empty() => {
                let path = Path::new(&dir).join(name.trim());
                if let Err(e) = std::fs::create_dir(&path) {
                    self.error(format!("{}: {e}", path.display()));
                }
                let cwd = self.local.cwd.clone();
                self.local_cd(&cwd);
            }
            Dialog::Rename {
                remote: true,
                dir,
                from,
                to,
            } if !to.trim().is_empty() && to != from => {
                let a = remote_join(&dir, &from);
                let b = remote_join(&dir, to.trim());
                self.run_op(rt, ctx, move |s| {
                    Box::pin(async move { remote::rename(&s, &a, &b).await })
                });
            }
            Dialog::Rename {
                remote: false,
                dir,
                from,
                to,
            } if !to.trim().is_empty() && to != from => {
                let a = Path::new(&dir).join(&from);
                let b = Path::new(&dir).join(to.trim());
                // On a case-insensitive disk "readme" → "README" is the same file: allowed.
                let same_file = match (std::fs::metadata(&a), std::fs::metadata(&b)) {
                    (Ok(ma), Ok(mb)) => ma.dev() == mb.dev() && ma.ino() == mb.ino(),
                    _ => false,
                };
                if b.exists() && !same_file {
                    self.error(tf("{} already exists.", &[&b.display().to_string()]));
                } else if let Err(e) = std::fs::rename(&a, &b) {
                    self.error(format!("{}: {e}", a.display()));
                }
                let cwd = self.local.cwd.clone();
                self.local_cd(&cwd);
            }
            Dialog::Delete {
                remote: true,
                dir,
                names,
            } => {
                let paths: Vec<String> = names.iter().map(|n| remote_join(&dir, n)).collect();
                self.remote.selected.clear();
                self.run_op(rt, ctx, move |s| {
                    Box::pin(async move {
                        for p in paths {
                            remote::remove(&s, &p).await?;
                        }
                        Ok(())
                    })
                });
            }
            Dialog::Delete {
                remote: false,
                dir,
                names,
            } => {
                for n in names {
                    let p = Path::new(&dir).join(&n);
                    let r = if p.is_dir() && !p.is_symlink() {
                        std::fs::remove_dir_all(&p)
                    } else {
                        std::fs::remove_file(&p)
                    };
                    if let Err(e) = r {
                        self.error(format!("{}: {e}", p.display()));
                    }
                }
                self.local.selected.clear();
                let cwd = self.local.cwd.clone();
                self.local_cd(&cwd);
            }
            Dialog::Chmod {
                path,
                original,
                mode,
            } => match entry::parse_mode(&mode) {
                Some(m) if m == original => {}
                Some(m) => self.run_op(rt, ctx, move |s| {
                    Box::pin(async move { remote::chmod(&s, &path, m).await })
                }),
                None => self.error(t("Invalid permissions; use octal like 644 or 755.")),
            },
            Dialog::Overwrite {
                upload: true,
                sources,
                dest,
                ..
            } => {
                for src in sources {
                    self.start_upload(rt, ctx, PathBuf::from(src), &dest);
                }
            }
            Dialog::Overwrite {
                upload: false,
                sources,
                dest,
                ..
            } => {
                for src in sources {
                    self.start_download(rt, ctx, src, Path::new(&dest));
                }
            }
            Dialog::Sudo { password } => {
                // An empty box means passwordless sudo; a box of spaces is a real password.
                let password = if password.is_empty() {
                    None
                } else {
                    Some(password)
                };
                self.reopen(true, password);
            }
            _ => {}
        }
    }
}

const TRANSFER_ROW_H: f32 = 28.0;
/// Where the transfers popup (and its pill) sit: the bottom right corner, above the status bar.
const ACTIVITY_OFFSET: egui::Vec2 = egui::vec2(-14.0, -40.0);

/// One transfer: direction, name, status (bar while running), details, and cancel/remove.
/// Returns the id to remove when its × was clicked after it finished.
/// A thin accent bar on a quiet track.
fn progress_bar(ui: &mut Ui, fraction: f32, width: f32) {
    let p = style::pal(ui);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 4.0), Sense::hover());
    ui.painter().rect_filled(rect, 2.0, p.subtle);
    let mut done = rect;
    done.set_width(rect.width() * fraction.clamp(0.0, 1.0));
    ui.painter().rect_filled(done, 2.0, p.accent);
}

fn transfer_row(ui: &mut Ui, tr: &Transfer) -> Option<u64> {
    let mut remove = None;
    let state = tr.state();
    let done = tr.done.load(std::sync::atomic::Ordering::Relaxed);
    let total = tr.total.load(std::sync::atomic::Ordering::Relaxed);
    let skipped = tr.skipped.lock().clone();
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), TRANSFER_ROW_H),
        Sense::hover(),
    );
    let p = style::pal(ui);
    if ui.rect_contains_pointer(rect) {
        ui.painter().rect_filled(rect, 6.0, p.faint_row());
    }
    let mut row = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink2(egui::vec2(6.0, 0.0)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    let ui = &mut row;
    let (arrow, arrow_color) = match tr.direction {
        Direction::Upload => (icons::ARROW_UP, p.accent_text),
        Direction::Download => (icons::ARROW_DOWN, p.ok),
    };
    kit::icon(ui, arrow, 14.0, arrow_color);
    // Right part, laid out from the right edge: action, details, status. The details narrow
    // (and finally go, into the name's tooltip) when the popup is too narrow for all of it, so
    // nothing is drawn over the name.
    const STATUS_W: f32 = 120.0;
    const NAME_MIN: f32 = 110.0;
    let gap = ui.spacing().item_spacing.x;
    let avail = ui.available_width();
    let detail_room = avail - 20.0 - STATUS_W - NAME_MIN - 4.0 * gap;
    let detail_w = if detail_room >= 90.0 { detail_room.min(190.0) } else { 0.0 };
    let right_w = 20.0 + STATUS_W + 3.0 * gap + if detail_w > 0.0 { detail_w + gap } else { 0.0 };
    let name_w = (avail - right_w).max(40.0);
    let name = ui
        .allocate_ui_with_layout(
            egui::vec2(name_w, TRANSFER_ROW_H),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.set_width(name_w);
                let text = RichText::new(&tr.name);
                let text = if matches!(state, TransferState::Running) {
                    text.font(style::medium(13.0))
                } else {
                    text
                };
                ui.add(egui::Label::new(text).truncate())
            },
        )
        .inner;
    let (from, to) = match tr.direction {
        Direction::Upload => (tr.local.display().to_string(), tr.remote.clone()),
        Direction::Download => (tr.remote.clone(), tr.local.display().to_string()),
    };
    name.on_hover_text(format!("{from}\n→ {to}"));
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        if !tr.is_finished() {
            if kit::icon_button_ex(ui, icons::X, t("Cancel"), false, 20.0).clicked() {
                tr.cancel();
            }
        } else if kit::icon_button_ex(ui, icons::X, t("Remove"), false, 20.0).clicked() {
            remove = Some(tr.id);
        }
        // Details: sizes and speed.
        let detail = match &state {
            TransferState::Queued => String::new(),
            TransferState::Running => format!(
                "{} / {} · {}/s",
                entry::human_size(done),
                entry::human_size(total),
                entry::human_size(tr.speed() as u64)
            ),
            TransferState::Done => format!(
                "{} · {}/s",
                entry::human_size(total),
                entry::human_size(tr.speed() as u64)
            ),
            TransferState::Failed(_) | TransferState::Cancelled => {
                if total > 0 {
                    format!("{} / {}", entry::human_size(done), entry::human_size(total))
                } else {
                    String::new()
                }
            }
        };
        if detail_w > 0.0 {
            ui.allocate_ui_with_layout(
                egui::vec2(detail_w, TRANSFER_ROW_H),
                egui::Layout::right_to_left(egui::Align::Center),
                |ui| {
                    ui.set_width(detail_w);
                    ui.add(
                        egui::Label::new(RichText::new(detail).small().color(p.text_muted))
                            .truncate(),
                    );
                },
            );
        }
        // Status: a bar while running, a word otherwise.
        ui.allocate_ui_with_layout(
            egui::vec2(STATUS_W, TRANSFER_ROW_H),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.set_width(STATUS_W);
                match &state {
                    TransferState::Running => {
                        progress_bar(ui, tr.fraction(), STATUS_W - 44.0);
                        ui.label(
                            RichText::new(format!("{}%", (tr.fraction() * 100.0).floor()))
                                .small()
                                .color(p.text_muted),
                        );
                    }
                    TransferState::Queued => {
                        ui.label(RichText::new(t("Waiting…")).color(p.text_muted));
                    }
                    TransferState::Done if skipped.is_empty() => {
                        ui.label(
                            RichText::new(format!("{}  {}", icons::CHECK_CIRCLE, t("Done")))
                                .color(p.ok),
                        );
                    }
                    TransferState::Done => {
                        ui.label(
                            RichText::new(format!(
                                "{}  {}",
                                icons::WARNING,
                                tf("{} skipped", &[&skipped.len().to_string()])
                            ))
                            .color(p.warn),
                        )
                        .on_hover_text(
                            skipped
                                .iter()
                                .take(20)
                                .cloned()
                                .collect::<Vec<_>>()
                                .join("\n"),
                        );
                    }
                    TransferState::Failed(e) => {
                        ui.add(
                            egui::Label::new(
                                RichText::new(format!("{}  {}", icons::X_CIRCLE, t("Failed")))
                                    .color(p.err),
                            )
                            .truncate(),
                        )
                        .on_hover_text(e.as_str());
                    }
                    TransferState::Cancelled => {
                        ui.label(RichText::new(t("Cancelled")).color(p.text_muted));
                    }
                }
            },
        );
    });
    remove
}

/// "4 done · 1 failed" for a finished batch.
fn finished_detail(s: &Summary) -> String {
    let mut parts = vec![tf("{} done", &[&s.done.to_string()])];
    if s.failed > 0 {
        parts.push(tf("{} failed", &[&s.failed.to_string()]));
    }
    if s.cancelled > 0 {
        parts.push(tf("{} cancelled", &[&s.cancelled.to_string()]));
    }
    parts.join(" · ")
}

/// "Uploading 7 file(s)" (files inside folders count one by one).
pub fn summary_title(s: &Summary) -> String {
    let n = s.files_total.to_string();
    match (s.uploads > 0, s.downloads > 0) {
        (true, false) => tf("Uploading {} file(s)", &[&n]),
        (false, true) => tf("Downloading {} file(s)", &[&n]),
        _ => tf("Transferring {} file(s)", &[&n]),
    }
}

/// "3 done · 45% · 1.2 GB / 2.6 GB · 12.3 MB/s · about 20s left"
pub fn summary_detail(s: &Summary) -> String {
    let mut text = String::new();
    if s.files_total > 1 {
        text.push_str(&tf("{} done", &[&s.files_done.to_string()]));
        text.push_str(" · ");
    }
    text.push_str(&format!(
        "{}% · {} / {} · {}/s",
        (s.fraction() * 100.0).floor() as u32,
        entry::human_size(s.bytes_done),
        entry::human_size(s.bytes_total),
        entry::human_size(s.speed as u64)
    ));
    if let Some(eta) = s.eta() {
        let secs = eta.as_secs();
        let left = if secs >= 3600 {
            format!("{}h {}m", secs / 3600, secs % 3600 / 60)
        } else if secs >= 60 {
            format!("{}m {}s", secs / 60, secs % 60)
        } else {
            format!("{}s", secs.max(1))
        };
        text.push_str(&format!(" · {}", tf("about {} left", &[&left])));
    }
    text
}

/// What to say when a batch has finished (nothing if it was all cancelled).
fn batch_notice(s: &Summary) -> Option<(String, bool)> {
    let files = s.files_done.to_string();
    let mut text = match (s.uploads > 0, s.downloads > 0) {
        (true, false) => tf("Uploaded {} file(s).", &[&files]),
        (false, true) => tf("Downloaded {} file(s).", &[&files]),
        _ => tf("Transferred {} file(s).", &[&files]),
    };
    if s.failed > 0 {
        text.push(' ');
        text.push_str(&tf("{} transfer(s) failed.", &[&s.failed.to_string()]));
        return Some((text, true));
    }
    (s.done > 0).then_some((text, false))
}

/// The list's surface, which takes files dragged from the other side. `Ui::dnd_drop_zone` would
/// paint it in the button gray, on which the names were hard to read, and light it up for this
/// side's own files too.
fn file_drop_zone<R>(
    ui: &mut Ui,
    is_remote: bool,
    add_contents: impl FnOnce(&mut Ui) -> R,
) -> (R, Option<Arc<DragFiles>>) {
    let p = style::pal(ui);
    let mut frame = egui::Frame::NONE
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(2, 4))
        .begin(ui);
    let v = frame.content_ui.visuals_mut();
    v.widgets.hovered.bg_fill = p.faint_row();
    v.selection.bg_fill = p.accent_soft;
    let inner = add_contents(&mut frame.content_ui);
    let response = frame.allocate_space(ui);
    // `contains_pointer`, not `hovered`: nothing counts as hovered while something is dragged.
    let target = response.contains_pointer()
        && egui::DragAndDrop::payload::<DragFiles>(ui.ctx())
            .is_some_and(|d| d.from_remote != is_remote);
    frame.frame.fill = if target { style::soft(p.accent, 0.10) } else { p.surface };
    frame.frame.stroke = if target {
        egui::Stroke::new(1.5, p.accent)
    } else {
        egui::Stroke::new(1.0, p.border)
    };
    frame.paint(ui);
    (inner, response.dnd_release_payload::<DragFiles>())
}

fn ui_mods(resp: &egui::Response) -> (bool, bool) {
    let m = resp.ctx.input(|i| i.modifiers);
    (m.command, m.shift)
}
