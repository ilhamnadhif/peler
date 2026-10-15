//! The SFTP panel: local and remote file lists side by side, transfers below.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use eframe::egui::{self, Color32, RichText, Sense, Ui};
use egui_extras::{Column, TableBuilder};
use russh_sftp::client::SftpSession;
use tokio::sync::oneshot;

use crate::i18n::{t, tf};
use crate::paths;
use crate::sftp::entry::{self, FileEntry, SortKey, remote_join, remote_parent};
use crate::sftp::remote::{self, Sftp};
use crate::sftp::transfer::{Direction, TransferState, Transfers};

enum Session {
    Opening(oneshot::Receiver<Result<SftpSession, String>>),
    Ready(Sftp),
    Failed(String),
}

enum Event {
    Listed { remote: bool, path: String, result: Result<Vec<FileEntry>, String> },
    OpDone { result: Result<(), String>, refresh_remote: bool },
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
}

impl SideState {
    fn visible(&self, show_hidden: bool) -> Vec<&FileEntry> {
        self.entries.iter().filter(|e| show_hidden || !e.is_hidden()).collect()
    }
}

/// Files being dragged from one side to the other.
#[derive(Clone)]
struct DragFiles {
    from_remote: bool,
    names: Vec<String>,
}

enum Dialog {
    Mkdir { remote: bool, name: String },
    Rename { remote: bool, from: String, to: String },
    Delete { remote: bool, names: Vec<String> },
    Chmod { path: String, mode: String },
    Overwrite { upload: bool, names: Vec<String>, conflicts: Vec<String> },
}

pub struct SftpPanel {
    session: Session,
    events_tx: mpsc::Sender<Event>,
    events_rx: mpsc::Receiver<Event>,
    local: SideState,
    remote: SideState,
    show_hidden: bool,
    pub transfers: Transfers,
    dialog: Option<Dialog>,
    focus_dialog: bool,
    /// Finder drops received before the session was ready.
    pending_uploads: Vec<PathBuf>,
    start_dir: String,
    message: Option<(String, bool)>,
}

/// Something the app should do after the panel was drawn.
pub enum PanelOutput {
    None,
    /// Remember this local folder for next time.
    LocalDir(String),
}

impl SftpPanel {
    pub fn new(open: oneshot::Receiver<Result<SftpSession, String>>, start_dir: &str, local_dir: Option<&str>) -> Self {
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
            transfers: Transfers::default(),
            dialog: None,
            focus_dialog: false,
            pending_uploads: Vec::new(),
            start_dir: start_dir.trim().to_string(),
            message: None,
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

    pub fn is_ready(&self) -> bool {
        matches!(self.session, Session::Ready(_))
    }

    /// Uploads local files (e.g. dropped from Finder) into the current remote folder.
    pub fn upload_paths(&mut self, rt: &tokio::runtime::Handle, ctx: &egui::Context, paths: Vec<PathBuf>) {
        if !self.is_ready() || self.remote.cwd.is_empty() {
            self.pending_uploads.extend(paths);
            return;
        }
        let names: Vec<String> = paths.iter().filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())).collect();
        let conflicts: Vec<String> = names.iter().filter(|n| self.remote.entries.iter().any(|e| &e.name == *n)).cloned().collect();
        if conflicts.is_empty() {
            for p in paths {
                self.start_upload(rt, ctx, p);
            }
        } else {
            // Finder paths can come from anywhere: keep full paths in `names`.
            let full: Vec<String> = paths.iter().map(|p| p.display().to_string()).collect();
            self.dialog = Some(Dialog::Overwrite { upload: true, names: full, conflicts });
        }
    }

    fn local_cd(&mut self, path: &str) {
        let path = paths::expand_tilde(path.trim());
        match entry::list_local(&path) {
            Ok(mut entries) => {
                entry::sort_entries(&mut entries, self.local.sort, self.local.descending);
                self.local.cwd = path.display().to_string();
                self.local.path_edit = self.local.cwd.clone();
                self.local.entries = entries;
                self.local.error = None;
                self.local.selected.clear();
            }
            Err(e) => self.local.error = Some(format!("{}: {e}", path.display())),
        }
    }

    fn remote_cd(&mut self, rt: &tokio::runtime::Handle, ctx: &egui::Context, path: String) {
        let Some(sftp) = self.sftp() else { return };
        self.remote.loading = true;
        let tx = self.events_tx.clone();
        let ctx = ctx.clone();
        rt.spawn(async move {
            let resolved = remote::canonicalize(&sftp, &path).await.unwrap_or(path);
            let result = remote::list(&sftp, &resolved).await;
            let _ = tx.send(Event::Listed { remote: true, path: resolved, result });
            ctx.request_repaint();
        });
    }

    fn refresh_remote(&mut self, rt: &tokio::runtime::Handle, ctx: &egui::Context) {
        let cwd = self.remote.cwd.clone();
        if !cwd.is_empty() {
            self.remote_cd(rt, ctx, cwd);
        }
    }

    fn run_op(
        &mut self,
        rt: &tokio::runtime::Handle,
        ctx: &egui::Context,
        op: impl FnOnce(Sftp) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>> + Send + 'static,
    ) {
        let Some(sftp) = self.sftp() else { return };
        let tx = self.events_tx.clone();
        let ctx = ctx.clone();
        rt.spawn(async move {
            let result = op(sftp).await;
            let _ = tx.send(Event::OpDone { result, refresh_remote: true });
            ctx.request_repaint();
        });
    }

    fn start_upload(&mut self, rt: &tokio::runtime::Handle, ctx: &egui::Context, local: PathBuf) {
        let Some(sftp) = self.sftp() else { return };
        let tx = self.events_tx.clone();
        let ctx2 = ctx.clone();
        self.transfers.upload(rt, sftp, local, &self.remote.cwd, move || {
            let _ = tx.send(Event::OpDone { result: Ok(()), refresh_remote: true });
            ctx2.request_repaint();
        });
    }

    fn start_download(&mut self, rt: &tokio::runtime::Handle, ctx: &egui::Context, name: &str) {
        let Some(sftp) = self.sftp() else { return };
        let tx = self.events_tx.clone();
        let ctx2 = ctx.clone();
        let remote_path = remote_join(&self.remote.cwd, name);
        let local_dir = PathBuf::from(&self.local.cwd);
        self.transfers.download(rt, sftp, remote_path, &local_dir, move || {
            let _ = tx.send(Event::OpDone { result: Ok(()), refresh_remote: false });
            ctx2.request_repaint();
        });
    }

    /// Uploads the named local entries, asking before replacing existing remote ones.
    fn upload(&mut self, rt: &tokio::runtime::Handle, ctx: &egui::Context, names: Vec<String>) {
        let conflicts: Vec<String> = names.iter().filter(|n| self.remote.entries.iter().any(|e| &e.name == *n)).cloned().collect();
        if conflicts.is_empty() {
            for n in names {
                self.start_upload(rt, ctx, Path::new(&self.local.cwd).join(n));
            }
        } else {
            let full = names.iter().map(|n| Path::new(&self.local.cwd).join(n).display().to_string()).collect();
            self.dialog = Some(Dialog::Overwrite { upload: true, names: full, conflicts });
        }
    }

    fn download(&mut self, rt: &tokio::runtime::Handle, ctx: &egui::Context, names: Vec<String>) {
        let conflicts: Vec<String> = names.iter().filter(|n| Path::new(&self.local.cwd).join(n).exists()).cloned().collect();
        if conflicts.is_empty() {
            for n in names {
                self.start_download(rt, ctx, &n);
            }
        } else {
            self.dialog = Some(Dialog::Overwrite { upload: false, names, conflicts });
        }
    }

    fn poll(&mut self, rt: &tokio::runtime::Handle, ctx: &egui::Context) {
        if let Session::Opening(rx) = &mut self.session {
            match rx.try_recv() {
                Ok(Ok(sftp)) => {
                    self.session = Session::Ready(Arc::new(sftp));
                    let start = if self.start_dir.is_empty() { ".".to_string() } else { self.start_dir.clone() };
                    let start = if start == "~" || start.starts_with("~/") { start.replacen('~', ".", 1) } else { start };
                    self.remote_cd(rt, ctx, start);
                }
                Ok(Err(e)) => self.session = Session::Failed(e),
                Err(oneshot::error::TryRecvError::Empty) => {}
                Err(oneshot::error::TryRecvError::Closed) => self.session = Session::Failed(t("Not connected.").into()),
            }
        }
        while let Ok(ev) = self.events_rx.try_recv() {
            match ev {
                Event::Listed { remote: true, path, result } => {
                    self.remote.loading = false;
                    match result {
                        Ok(mut entries) => {
                            entry::sort_entries(&mut entries, self.remote.sort, self.remote.descending);
                            if self.remote.cwd != path {
                                self.remote.selected.clear();
                            }
                            self.remote.cwd = path.clone();
                            self.remote.path_edit = path;
                            self.remote.entries = entries;
                            self.remote.error = None;
                        }
                        Err(e) => {
                            self.remote.error = Some(e);
                            self.remote.path_edit = self.remote.cwd.clone();
                        }
                    }
                }
                Event::Listed { .. } => {}
                Event::OpDone { result, refresh_remote } => {
                    if let Err(e) = result {
                        self.message = Some((e, true));
                    }
                    if refresh_remote {
                        self.refresh_remote(rt, ctx);
                    }
                    let cwd = self.local.cwd.clone();
                    self.local_cd(&cwd);
                }
            }
        }
        if self.is_ready() && !self.remote.cwd.is_empty() && !self.pending_uploads.is_empty() {
            let paths = std::mem::take(&mut self.pending_uploads);
            self.upload_paths(rt, ctx, paths);
        }
    }

    pub fn show(&mut self, ui: &mut Ui, rt: &tokio::runtime::Handle, input_enabled: bool) -> PanelOutput {
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
                    ui.colored_label(ui.visuals().error_fg_color, tf("SFTP unavailable: {}", &[&e]));
                });
                return output;
            }
            Session::Ready(_) => {}
        }

        let transfers_height = if self.transfers.items.is_empty() { 0.0 } else { (self.transfers.items.len().min(4) as f32) * 26.0 + 34.0 };
        let lists_height = (ui.available_height() - transfers_height - 4.0).max(80.0);
        let total_width = ui.available_width();
        let half = (total_width - 12.0) / 2.0;

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

        if !self.transfers.items.is_empty() {
            ui.separator();
            self.transfers_ui(ui);
        }
        if let Some((msg, is_err)) = self.message.clone() {
            let color = if is_err { ui.visuals().error_fg_color } else { ui.visuals().text_color() };
            ui.horizontal(|ui| {
                ui.colored_label(color, msg);
                if ui.small_button("✕").clicked() {
                    self.message = None;
                }
            });
        }
        self.dialog_ui(&ctx, rt);
        output
    }

    fn side_ui(&mut self, ui: &mut Ui, is_remote: bool, input_enabled: bool) -> Vec<SideAction> {
        let mut actions = Vec::new();
        let show_hidden = self.show_hidden;
        let side = if is_remote { &mut self.remote } else { &mut self.local };
        ui.vertical(|ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(if is_remote { t("Remote") } else { t("Local") }).strong());
                if ui.small_button("⬆").on_hover_text(t("Parent folder")).clicked() {
                    actions.push(SideAction::Up(is_remote));
                }
                if ui.small_button("⌂").on_hover_text(t("Home folder")).clicked() {
                    actions.push(SideAction::Home(is_remote));
                }
                if ui.small_button("⟳").on_hover_text(t("Refresh")).clicked() {
                    actions.push(SideAction::Refresh(is_remote));
                }
                if ui.small_button("+").on_hover_text(t("New folder")).clicked() {
                    actions.push(SideAction::Mkdir(is_remote));
                }
                if is_remote && side.loading {
                    ui.spinner();
                }
                let edit = ui.add(egui::TextEdit::singleline(&mut side.path_edit).desired_width(f32::INFINITY));
                if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    actions.push(SideAction::Cd(is_remote, side.path_edit.clone()));
                }
            });
            if let Some(err) = &side.error {
                ui.colored_label(ui.visuals().error_fg_color, err);
            }

            let entries: Vec<FileEntry> = side.visible(show_hidden).into_iter().cloned().collect();
            let row_h = 20.0;
            let (_, dropped) = ui.dnd_drop_zone::<DragFiles, _>(egui::Frame::NONE, |ui| {
                ui.set_min_size(ui.available_size());
                let mut sort_clicked = None;
                let mut table = TableBuilder::new(ui)
                    .id_salt(if is_remote { "sftp-remote" } else { "sftp-local" })
                    .striped(true)
                    .sense(Sense::click())
                    .resizable(true)
                    .auto_shrink([false, false])
                    .max_scroll_height(f32::INFINITY)
                    .column(Column::remainder().at_least(120.0).clip(true))
                    .column(Column::initial(70.0).at_least(50.0))
                    .column(Column::initial(118.0).at_least(60.0).clip(true));
                if is_remote {
                    table = table.column(Column::initial(80.0).at_least(60.0));
                }
                let arrow = |k: SortKey| if side.sort == k { if side.descending { " ⏷" } else { " ⏶" } } else { "" };
                table
                    .header(row_h, |mut header| {
                        header.col(|ui| {
                            if ui.add(egui::Button::new(format!("{}{}", t("Name"), arrow(SortKey::Name))).frame(false)).clicked() {
                                sort_clicked = Some(SortKey::Name);
                            }
                        });
                        header.col(|ui| {
                            if ui.add(egui::Button::new(format!("{}{}", t("Size"), arrow(SortKey::Size))).frame(false)).clicked() {
                                sort_clicked = Some(SortKey::Size);
                            }
                        });
                        header.col(|ui| {
                            if ui.add(egui::Button::new(format!("{}{}", t("Modified"), arrow(SortKey::Modified))).frame(false)).clicked() {
                                sort_clicked = Some(SortKey::Modified);
                            }
                        });
                        if is_remote {
                            header.col(|ui| {
                                ui.label(t("Permissions"));
                            });
                        }
                    })
                    .body(|body| {
                        body.rows(row_h, entries.len(), |mut row| {
                            let e = &entries[row.index()];
                            row.set_selected(side.selected.contains(&e.name));
                            row.col(|ui| {
                                let icon = if e.is_dir { "📁" } else if e.is_symlink { "🔗" } else { "📄" };
                                let payload = DragFiles {
                                    from_remote: is_remote,
                                    names: if side.selected.contains(&e.name) { side.selected.iter().cloned().collect() } else { vec![e.name.clone()] },
                                };
                                ui.dnd_drag_source(egui::Id::new(("sftp-drag", is_remote, &e.name)), payload, |ui| {
                                    ui.add(egui::Label::new(format!("{icon} {}", e.name)).truncate().selectable(false));
                                });
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
                                    ui.monospace(e.mode.map(entry::mode_string).unwrap_or_default());
                                });
                            }
                            let resp = row.response();
                            if resp.double_clicked() {
                                actions.push(SideAction::Open(is_remote, e.clone()));
                            } else if resp.clicked() {
                                let mods = ui_mods(&resp);
                                actions.push(SideAction::Select(is_remote, e.name.clone(), mods.0, mods.1));
                            }
                            resp.context_menu(|ui| {
                                if !side.selected.contains(&e.name) {
                                    side.selected.clear();
                                    side.selected.insert(e.name.clone());
                                }
                                let names: Vec<String> = side.selected.iter().cloned().collect();
                                let label = if is_remote { t("Download") } else { t("Upload") };
                                if ui.button(label).clicked() {
                                    actions.push(SideAction::Transfer(is_remote, names.clone()));
                                    ui.close();
                                }
                                if e.is_dir && ui.button(t("Open")).clicked() {
                                    actions.push(SideAction::Open(is_remote, e.clone()));
                                    ui.close();
                                }
                                ui.separator();
                                if ui.add_enabled(names.len() == 1, egui::Button::new(t("Rename…"))).clicked() {
                                    actions.push(SideAction::Rename(is_remote, e.name.clone()));
                                    ui.close();
                                }
                                if is_remote && ui.add_enabled(names.len() == 1, egui::Button::new(t("Permissions…"))).clicked() {
                                    actions.push(SideAction::Chmod(e.clone()));
                                    ui.close();
                                }
                                if ui.button(t("Delete…")).clicked() {
                                    actions.push(SideAction::Delete(is_remote, names.clone()));
                                    ui.close();
                                }
                                ui.separator();
                                if ui.button(t("Copy Path")).clicked() {
                                    let p = if is_remote { remote_join(&side.cwd, &e.name) } else { Path::new(&side.cwd).join(&e.name).display().to_string() };
                                    ui.ctx().copy_text(p);
                                    ui.close();
                                }
                                if !is_remote && ui.button(t("Show in Finder")).clicked() {
                                    let p = Path::new(&side.cwd).join(&e.name);
                                    let _ = std::process::Command::new("open").arg("-R").arg(p).spawn();
                                    ui.close();
                                }
                            });
                        });
                    });
                if let Some(k) = sort_clicked {
                    actions.push(SideAction::Sort(is_remote, k));
                }
            });
            if let Some(drop) = dropped {
                if drop.from_remote != is_remote {
                    actions.push(SideAction::Transfer(drop.from_remote, drop.names.clone()));
                }
            }
            let _ = input_enabled;
        });
        actions
    }

    fn apply(&mut self, action: SideAction, rt: &tokio::runtime::Handle, ctx: &egui::Context) -> Option<PanelOutput> {
        match action {
            SideAction::Up(true) => {
                let p = remote_parent(&self.remote.cwd);
                self.remote_cd(rt, ctx, p);
            }
            SideAction::Up(false) => {
                if let Some(p) = Path::new(&self.local.cwd).parent().map(|p| p.display().to_string()) {
                    self.local_cd(&p);
                    return Some(PanelOutput::LocalDir(self.local.cwd.clone()));
                }
            }
            SideAction::Home(true) => self.remote_cd(rt, ctx, ".".into()),
            SideAction::Home(false) => {
                self.local_cd("~");
                return Some(PanelOutput::LocalDir(self.local.cwd.clone()));
            }
            SideAction::Refresh(true) => self.refresh_remote(rt, ctx),
            SideAction::Refresh(false) => {
                let cwd = self.local.cwd.clone();
                self.local_cd(&cwd);
            }
            SideAction::Cd(true, p) => self.remote_cd(rt, ctx, p),
            SideAction::Cd(false, p) => {
                self.local_cd(&p);
                return Some(PanelOutput::LocalDir(self.local.cwd.clone()));
            }
            SideAction::Mkdir(remote) => {
                self.dialog = Some(Dialog::Mkdir { remote, name: String::new() });
                self.focus_dialog = true;
            }
            SideAction::Open(true, e) if e.is_dir => {
                let p = remote_join(&self.remote.cwd, &e.name);
                self.remote_cd(rt, ctx, p);
            }
            SideAction::Open(false, e) if e.is_dir => {
                let p = Path::new(&self.local.cwd).join(&e.name).display().to_string();
                self.local_cd(&p);
                return Some(PanelOutput::LocalDir(self.local.cwd.clone()));
            }
            SideAction::Open(remote, e) => {
                if remote {
                    self.download(rt, ctx, vec![e.name]);
                } else {
                    self.upload(rt, ctx, vec![e.name]);
                }
            }
            SideAction::Select(remote, name, toggle, range) => {
                let show_hidden = self.show_hidden;
                let side = if remote { &mut self.remote } else { &mut self.local };
                if range {
                    if let Some(anchor) = side.anchor.clone() {
                        let names: Vec<String> = side.visible(show_hidden).iter().map(|e| e.name.clone()).collect();
                        let a = names.iter().position(|n| *n == anchor);
                        let b = names.iter().position(|n| *n == name);
                        if let (Some(a), Some(b)) = (a, b) {
                            side.selected = names[a.min(b)..=a.max(b)].iter().cloned().collect();
                        }
                    }
                } else if toggle {
                    if !side.selected.remove(&name) {
                        side.selected.insert(name.clone());
                    }
                    side.anchor = Some(name);
                } else {
                    side.selected = HashSet::from([name.clone()]);
                    side.anchor = Some(name);
                }
            }
            SideAction::Sort(remote, key) => {
                let side = if remote { &mut self.remote } else { &mut self.local };
                if side.sort == key {
                    side.descending = !side.descending;
                } else {
                    side.sort = key;
                    side.descending = false;
                }
                entry::sort_entries(&mut side.entries, side.sort, side.descending);
            }
            SideAction::Transfer(from_remote, names) => {
                if from_remote {
                    self.download(rt, ctx, names);
                } else {
                    self.upload(rt, ctx, names);
                }
            }
            SideAction::Rename(remote, name) => {
                self.dialog = Some(Dialog::Rename { remote, from: name.clone(), to: name });
                self.focus_dialog = true;
            }
            SideAction::Delete(remote, names) => self.dialog = Some(Dialog::Delete { remote, names }),
            SideAction::Chmod(e) => {
                self.dialog = Some(Dialog::Chmod {
                    path: remote_join(&self.remote.cwd, &e.name),
                    mode: format!("{:o}", e.mode.unwrap_or(0o644) & 0o7777),
                });
                self.focus_dialog = true;
            }
        }
        None
    }

    fn transfers_ui(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new(t("Transfers")).strong());
            if ui.small_button(t("Clear finished")).clicked() {
                self.transfers.clear_finished();
            }
        });
        egui::ScrollArea::vertical().id_salt("sftp-transfers").max_height(110.0).auto_shrink([false, true]).show(ui, |ui| {
            let mut remove = None;
            for tr in self.transfers.items.iter().rev() {
                ui.horizontal(|ui| {
                    let arrow = if tr.direction == Direction::Upload { "⬆" } else { "⬇" };
                    ui.label(arrow);
                    ui.add_sized([180.0, 18.0], egui::Label::new(&tr.name).truncate());
                    let done = tr.done.load(std::sync::atomic::Ordering::Relaxed);
                    let total = tr.total.load(std::sync::atomic::Ordering::Relaxed);
                    let state = tr.state();
                    let text = match &state {
                        TransferState::Queued => t("Waiting…").to_string(),
                        TransferState::Running => format!(
                            "{} / {} · {}/s",
                            entry::human_size(done),
                            entry::human_size(total),
                            entry::human_size(tr.speed() as u64)
                        ),
                        TransferState::Done => format!("{} · {}/s", entry::human_size(total), entry::human_size(tr.speed() as u64)),
                        TransferState::Failed(e) => e.clone(),
                        TransferState::Cancelled => t("Cancelled").to_string(),
                    };
                    let bar = egui::ProgressBar::new(tr.fraction()).desired_width(220.0).text(match state {
                        TransferState::Done => t("Done"),
                        TransferState::Failed(_) => t("Failed"),
                        TransferState::Cancelled => t("Cancelled"),
                        _ => "",
                    });
                    ui.add(bar);
                    let color = if matches!(state, TransferState::Failed(_)) { ui.visuals().error_fg_color } else { ui.visuals().weak_text_color() };
                    ui.add(egui::Label::new(RichText::new(text).color(color).small()).truncate());
                    if !tr.is_finished() {
                        if ui.small_button("✕").on_hover_text(t("Cancel")).clicked() {
                            tr.cancel();
                        }
                    } else if ui.small_button("✕").on_hover_text(t("Remove")).clicked() {
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
        let Some(dialog) = &mut self.dialog else { return };
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
                Dialog::Delete { names, remote } => {
                    ui.heading(t("Delete"));
                    let where_ = if *remote { t("on the server") } else { t("on this Mac") };
                    ui.label(tf("Delete {} item(s) {}? Folders are deleted with everything in them. This cannot be undone.", &[&names.len().to_string(), where_]));
                    for n in names.iter().take(8) {
                        ui.label(RichText::new(format!("• {n}")).weak());
                    }
                }
                Dialog::Chmod { path, mode } => {
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
                    ui.label(tf("{} item(s) already exist at the destination and will be replaced:", &[&conflicts.len().to_string()]));
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
            Dialog::Mkdir { remote: true, name } if !name.trim().is_empty() => {
                let path = remote_join(&self.remote.cwd, name.trim());
                self.run_op(rt, ctx, move |s| Box::pin(async move { remote::mkdir(&s, &path).await }));
            }
            Dialog::Mkdir { remote: false, name } if !name.trim().is_empty() => {
                let path = Path::new(&self.local.cwd).join(name.trim());
                if let Err(e) = std::fs::create_dir(&path) {
                    self.message = Some((format!("{}: {e}", path.display()), true));
                }
                let cwd = self.local.cwd.clone();
                self.local_cd(&cwd);
            }
            Dialog::Rename { remote: true, from, to } if !to.trim().is_empty() && to != from => {
                let a = remote_join(&self.remote.cwd, &from);
                let b = remote_join(&self.remote.cwd, to.trim());
                self.run_op(rt, ctx, move |s| Box::pin(async move { remote::rename(&s, &a, &b).await }));
            }
            Dialog::Rename { remote: false, from, to } if !to.trim().is_empty() && to != from => {
                let a = Path::new(&self.local.cwd).join(&from);
                let b = Path::new(&self.local.cwd).join(to.trim());
                if b.exists() {
                    self.message = Some((tf("{} already exists.", &[&b.display().to_string()]), true));
                } else if let Err(e) = std::fs::rename(&a, &b) {
                    self.message = Some((format!("{}: {e}", a.display()), true));
                }
                let cwd = self.local.cwd.clone();
                self.local_cd(&cwd);
            }
            Dialog::Delete { remote: true, names } => {
                let paths: Vec<String> = names.iter().map(|n| remote_join(&self.remote.cwd, n)).collect();
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
            Dialog::Delete { remote: false, names } => {
                for n in names {
                    let p = Path::new(&self.local.cwd).join(&n);
                    let r = if p.is_dir() && !p.is_symlink() { std::fs::remove_dir_all(&p) } else { std::fs::remove_file(&p) };
                    if let Err(e) = r {
                        self.message = Some((format!("{}: {e}", p.display()), true));
                    }
                }
                self.local.selected.clear();
                let cwd = self.local.cwd.clone();
                self.local_cd(&cwd);
            }
            Dialog::Chmod { path, mode } => match entry::parse_mode(&mode) {
                Some(m) => self.run_op(rt, ctx, move |s| Box::pin(async move { remote::chmod(&s, &path, m).await })),
                None => self.message = Some((t("Invalid permissions; use octal like 644 or 755.").into(), true)),
            },
            Dialog::Overwrite { upload: true, names, .. } => {
                for full in names {
                    self.start_upload(rt, ctx, PathBuf::from(full));
                }
            }
            Dialog::Overwrite { upload: false, names, .. } => {
                for n in names {
                    self.start_download(rt, ctx, &n);
                }
            }
            _ => {}
        }
    }
}

enum SideAction {
    Up(bool),
    Home(bool),
    Refresh(bool),
    Cd(bool, String),
    Mkdir(bool),
    Open(bool, FileEntry),
    Select(bool, String, bool, bool),
    Sort(bool, SortKey),
    Transfer(bool, Vec<String>),
    Rename(bool, String),
    Delete(bool, Vec<String>),
    Chmod(FileEntry),
}

/// (toggle, range) selection modifiers for a click.
fn ui_mods(resp: &egui::Response) -> (bool, bool) {
    let m = resp.ctx.input(|i| i.modifiers);
    (m.command, m.shift)
}

#[allow(dead_code)]
fn weak(ui: &Ui) -> Color32 {
    ui.visuals().weak_text_color()
}
