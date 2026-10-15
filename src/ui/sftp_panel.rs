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
use crate::sftp::transfer::{Direction, TransferState, Transfers};
use crate::ui::file_editor::{EditorOutput, FileEditor};
use crate::ui::icons::{self, Icon};

enum Session {
    Opening(oneshot::Receiver<Result<SftpSession, String>>),
    Ready(Sftp),
    Failed(String),
}

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
}

pub struct SftpPanel {
    session: Session,
    events_tx: mpsc::Sender<Event>,
    events_rx: mpsc::Receiver<Event>,
    local: SideState,
    remote: SideState,
    show_hidden: bool,
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
    ToggleHidden,
}

impl SftpPanel {
    pub fn new(
        open: oneshot::Receiver<Result<SftpSession, String>>,
        start_dir: &str,
        local_dir: Option<&str>,
    ) -> Self {
        let (events_tx, events_rx) = mpsc::channel();
        let local_cwd = local_dir
            .map(PathBuf::from)
            .filter(|p| p.is_dir())
            .unwrap_or_else(paths::home_dir)
            .display()
            .to_string();
        let mut panel = Self {
            session: Session::Opening(open),
            events_tx,
            events_rx,
            local: SideState::default(),
            remote: SideState::default(),
            show_hidden: false,
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
        if let Err(dest) = started {
            self.error(tf("{} is already being transferred.", &[&dest]));
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
        if let Err(dest) = started {
            self.error(tf("{} is already being transferred.", &[&dest]));
        }
    }

    fn poll(&mut self, rt: &tokio::runtime::Handle, ctx: &egui::Context) {
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
                ui.centered_and_justified(|ui| {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(t("Opening SFTP…"));
                    });
                });
                return output;
            }
            Session::Failed(e) => {
                let e = e.clone();
                ui.centered_and_justified(|ui| {
                    ui.colored_label(
                        ui.visuals().error_fg_color,
                        tf("SFTP unavailable: {}", &[&e]),
                    );
                });
                return output;
            }
            Session::Ready(_) => {}
        }

        let transfers_height = if self.transfers.items.is_empty() {
            0.0
        } else {
            (self.transfers.items.len().min(4) as f32) * 26.0 + 34.0
        };
        let message_height = if self.message.is_some() { 26.0 } else { 0.0 };
        let lists_height =
            (ui.available_height() - transfers_height - message_height - 4.0).max(80.0);
        let total_width = ui.available_width();
        let half = ((total_width - 28.0) / 2.0).max(120.0);

        let mut local_actions = Vec::new();
        let mut remote_actions = Vec::new();
        ui.horizontal_top(|ui| {
            ui.allocate_ui(egui::vec2(half, lists_height), |ui| {
                local_actions = self.side_ui(ui, false, input_enabled);
            });
            ui.separator();
            ui.allocate_ui(egui::vec2(half, lists_height), |ui| {
                remote_actions = self.side_ui(ui, true, input_enabled);
            });
        });
        for a in local_actions.into_iter().chain(remote_actions) {
            if let Some(out) = self.apply(a, rt, &ctx) {
                output = out;
            }
        }

        if let Some((msg, is_err)) = self.message.clone() {
            let color = if is_err {
                ui.visuals().error_fg_color
            } else {
                ui.visuals().text_color()
            };
            ui.horizontal(|ui| {
                ui.colored_label(color, msg);
                if ui.small_button("×").clicked() {
                    self.message = None;
                }
            });
        }
        if !self.transfers.items.is_empty() {
            ui.separator();
            self.transfers_ui(ui);
        }
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
        let show_hidden = self.show_hidden;
        let menu_target = self.menu_target.clone().filter(|m| m.remote == is_remote);
        let side = if is_remote {
            &mut self.remote
        } else {
            &mut self.local
        };
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(if is_remote { t("Remote") } else { t("Local") }).strong());
                if ui
                    .small_button("⬆")
                    .on_hover_text(t("Parent folder"))
                    .clicked()
                {
                    actions.push(SideAction::Up(is_remote));
                }
                if ui
                    .small_button("⌂")
                    .on_hover_text(t("Home folder"))
                    .clicked()
                {
                    actions.push(SideAction::Home(is_remote));
                }
                if ui.small_button("⟳").on_hover_text(t("Refresh")).clicked() {
                    actions.push(SideAction::Refresh(is_remote));
                }
                if ui
                    .small_button("+")
                    .on_hover_text(t("New folder"))
                    .clicked()
                {
                    actions.push(SideAction::Mkdir(is_remote));
                }
                if ui
                    .add(egui::Button::new(RichText::new(".*").small()).selected(show_hidden))
                    .on_hover_text(t("Show hidden files"))
                    .clicked()
                {
                    actions.push(SideAction::ToggleHidden);
                }
                if is_remote && side.loading {
                    ui.spinner();
                }
                let edit = ui.add(
                    egui::TextEdit::singleline(&mut side.path_edit).desired_width(f32::INFINITY),
                );
                if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    actions.push(SideAction::Cd(is_remote, side.path_edit.clone()));
                }
            });
            if let Some(err) = &side.error {
                ui.colored_label(ui.visuals().error_fg_color, err);
            }

            let visible: Vec<usize> = side
                .entries
                .iter()
                .enumerate()
                .filter(|(_, e)| show_hidden || !e.is_hidden())
                .map(|(i, _)| i)
                .collect();
            let row_h = 20.0;
            let (_, dropped) = ui.dnd_drop_zone::<DragFiles, _>(egui::Frame::NONE, |ui| {
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
                    .striped(true)
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
                        if side.descending { " ▼" } else { " ▲" }
                    } else {
                        ""
                    }
                };
                let header_button =
                    |ui: &mut Ui, label: &str, key: SortKey, clicked: &mut Option<SortKey>| {
                        if ui
                            .add(egui::Button::new(format!("{label}{}", arrow(key))).frame(false))
                            .clicked()
                        {
                            *clicked = Some(key);
                        }
                    };
                table
                    .header(row_h, |mut header| {
                        header.col(|ui| {
                            header_button(ui, t("Name"), SortKey::Name, &mut sort_clicked)
                        });
                        header.col(|ui| {
                            header_button(ui, t("Size"), SortKey::Size, &mut sort_clicked)
                        });
                        header.col(|ui| {
                            header_button(ui, t("Modified"), SortKey::Modified, &mut sort_clicked)
                        });
                        if is_remote {
                            header.col(|ui| {
                                // Same look and baseline as the sortable headers.
                                ui.add(
                                    egui::Button::new(t("Permissions"))
                                        .frame(false)
                                        .sense(Sense::hover()),
                                );
                            });
                        }
                    })
                    .body(|body| {
                        body.rows(row_h, visible.len(), |mut row| {
                            let e = &side.entries[visible[row.index()]];
                            row.set_selected(side.selected.contains(&e.name));
                            row.col(|ui| {
                                let icon = if e.is_dir {
                                    Icon::Folder
                                } else if e.is_symlink {
                                    Icon::Link
                                } else {
                                    Icon::File
                                };
                                icons::icon_label(ui, icon, &e.name);
                            });
                            row.col(|ui| {
                                if !e.is_dir {
                                    ui.label(entry::human_size(e.size));
                                }
                            });
                            row.col(|ui| {
                                ui.label(e.modified.map(entry::format_time).unwrap_or_default());
                            });
                            if is_remote {
                                row.col(|ui| {
                                    ui.monospace(
                                        e.mode.map(entry::mode_string).unwrap_or_default(),
                                    );
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
                                let target = menu_target.clone().unwrap_or_else(|| MenuTarget {
                                    remote: is_remote,
                                    dir: side.cwd.clone(),
                                    names: names_here(),
                                    entry: e.clone(),
                                });
                                if is_remote
                                    && !target.entry.is_dir
                                    && target.names.len() == 1
                                    && ui.button(t("Edit")).clicked()
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
                                if ui.button(label).clicked() {
                                    actions.push(SideAction::Transfer {
                                        from_remote: is_remote,
                                        dir: target.dir.clone(),
                                        names: target.names.clone(),
                                    });
                                    ui.close();
                                }
                                if target.entry.is_dir && ui.button(t("Open")).clicked() {
                                    actions.push(SideAction::Open {
                                        remote: is_remote,
                                        dir: target.dir.clone(),
                                        entry: target.entry.clone(),
                                    });
                                    ui.close();
                                }
                                ui.separator();
                                if ui
                                    .add_enabled(
                                        target.names.len() == 1,
                                        egui::Button::new(t("Rename…")),
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
                                            egui::Button::new(t("Permissions…")),
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
                                if ui.button(t("Delete…")).clicked() {
                                    actions.push(SideAction::Delete {
                                        remote: is_remote,
                                        dir: target.dir.clone(),
                                        names: target.names.clone(),
                                    });
                                    ui.close();
                                }
                                ui.separator();
                                if ui.button(t("Copy Path")).clicked() {
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
                                if !is_remote && ui.button(t("Show in Finder")).clicked() {
                                    let p = Path::new(&target.dir).join(&target.entry.name);
                                    let _ =
                                        std::process::Command::new("open").arg("-R").arg(p).spawn();
                                    ui.close();
                                }
                            });
                        });
                    });
                if let Some(k) = sort_clicked {
                    actions.push(SideAction::Sort(is_remote, k));
                }
            });
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
            SideAction::ToggleHidden => {
                self.show_hidden = !self.show_hidden;
                if !self.show_hidden {
                    self.local.selected.retain(|n| !n.starts_with('.'));
                    self.remote.selected.retain(|n| !n.starts_with('.'));
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
                let show_hidden = self.show_hidden;
                let side = if remote {
                    &mut self.remote
                } else {
                    &mut self.local
                };
                let names: Vec<String> =
                    side.visible(show_hidden).map(|e| e.name.clone()).collect();
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
        let galley = painter.layout_no_wrap(
            label.clone(),
            egui::FontId::proportional(13.0),
            visuals.strong_text_color(),
        );
        let r = egui::Rect::from_min_size(
            pos + egui::vec2(14.0, 6.0),
            galley.size() + egui::vec2(14.0, 8.0),
        );
        painter.rect_filled(r, egui::CornerRadius::same(5), visuals.window_fill);
        painter.rect_stroke(
            r,
            egui::CornerRadius::same(5),
            visuals.window_stroke,
            egui::StrokeKind::Inside,
        );
        painter.galley(
            r.min + egui::vec2(7.0, 4.0),
            galley,
            visuals.strong_text_color(),
        );
    }

    fn transfers_ui(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new(t("Transfers")).strong());
            if ui.small_button(t("Clear finished")).clicked() {
                self.transfers.clear_finished();
            }
        });
        egui::ScrollArea::vertical()
            .id_salt("sftp-transfers")
            .max_height(110.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                let mut remove = None;
                for tr in self.transfers.items.iter().rev() {
                    ui.horizontal(|ui| {
                        ui.label(if tr.direction == Direction::Upload {
                            "⬆"
                        } else {
                            "⬇"
                        });
                        ui.add_sized([180.0, 18.0], egui::Label::new(&tr.name).truncate());
                        let done = tr.done.load(std::sync::atomic::Ordering::Relaxed);
                        let total = tr.total.load(std::sync::atomic::Ordering::Relaxed);
                        let state = tr.state();
                        let skipped = tr.skipped.lock().clone();
                        let mut text = match &state {
                            TransferState::Queued => t("Waiting…").to_string(),
                            TransferState::Running => format!(
                                "{} / {} · {}/s",
                                entry::human_size(done),
                                entry::human_size(total),
                                entry::human_size(tr.speed() as u64)
                            ),
                            TransferState::Done => {
                                format!(
                                    "{} · {}/s",
                                    entry::human_size(total),
                                    entry::human_size(tr.speed() as u64)
                                )
                            }
                            TransferState::Failed(e) => e.clone(),
                            TransferState::Cancelled => t("Cancelled").to_string(),
                        };
                        if !skipped.is_empty() {
                            text.push_str(&format!(
                                " · {}",
                                tf("{} skipped", &[&skipped.len().to_string()])
                            ));
                        }
                        let bar = egui::ProgressBar::new(tr.fraction())
                            .desired_width(220.0)
                            .text(match state {
                                TransferState::Done => t("Done"),
                                TransferState::Failed(_) => t("Failed"),
                                TransferState::Cancelled => t("Cancelled"),
                                _ => "",
                            });
                        ui.add(bar);
                        let color =
                            if matches!(state, TransferState::Failed(_)) || !skipped.is_empty() {
                                ui.visuals().warn_fg_color
                            } else {
                                ui.visuals().weak_text_color()
                            };
                        let label = ui.add(
                            egui::Label::new(RichText::new(text).color(color).small()).truncate(),
                        );
                        if !skipped.is_empty() {
                            label.on_hover_text(
                                skipped
                                    .iter()
                                    .take(20)
                                    .cloned()
                                    .collect::<Vec<_>>()
                                    .join("\n"),
                            );
                        }
                        if !tr.is_finished() {
                            if ui.small_button("×").on_hover_text(t("Cancel")).clicked() {
                                tr.cancel();
                            }
                        } else if ui.small_button("×").on_hover_text(t("Remove")).clicked() {
                            remove = Some(tr.id);
                        }
                    });
                }
                if let Some(id) = remove {
                    self.transfers.items.retain(|t| t.id != id);
                }
            });
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
                    let r = ui.add(egui::TextEdit::singleline(name).hint_text(t("Folder name")).desired_width(f32::INFINITY));
                    if focus {
                        r.request_focus();
                    }
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        confirmed = true;
                    }
                }
                Dialog::Rename { to, .. } => {
                    ui.heading(t("Rename"));
                    let r = ui.add(egui::TextEdit::singleline(to).desired_width(f32::INFINITY));
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
                                if ui.checkbox(&mut on, "").changed() {
                                    bits = if on { bits | mask } else { bits & !mask };
                                    *mode = format!("{:o}", bits);
                                }
                            }
                            ui.end_row();
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label(t("Octal"));
                        ui.add(egui::TextEdit::singleline(mode).desired_width(60.0));
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
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let ok_label = match dialog {
                    Dialog::Delete { .. } => t("Delete"),
                    Dialog::Overwrite { .. } => t("Replace"),
                    Dialog::Mkdir { .. } => t("Create"),
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
            _ => {}
        }
    }
}

/// (toggle, range) selection modifiers for a click.
fn ui_mods(resp: &egui::Response) -> (bool, bool) {
    let m = resp.ctx.input(|i| i.modifiers);
    (m.command, m.shift)
}
