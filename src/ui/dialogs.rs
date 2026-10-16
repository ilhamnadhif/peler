//! Modal dialogs: command palette, settings, export, import preview, confirmations, toasts.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Color32, Key, RichText, vec2};
use uuid::Uuid;

use super::fonts;
use crate::bookmarks::transfer::{
    ExportScope, ImportPlan, ItemStatus, MatchedBy, Resolution, SourceKind,
};
use crate::bookmarks::{BookmarkStore, search};
use crate::i18n::{Language, t, tf};
use crate::settings::{CursorStyle, FontChoice, QuickCommand, Settings, ThemeMode};
use crate::term::scheme::SCHEMES;

// ----- command palette ------------------------------------------------------------------------

#[derive(Default)]
pub struct Palette {
    query: String,
    index: usize,
    focused: bool,
}

pub enum PaletteResult {
    Open,
    Close,
    Connect(Uuid),
    /// An address typed in (`user@host[:port]`) that matches no bookmark.
    ConnectTo(String),
}

/// Most results listed in the palette.
const PALETTE_ROWS: usize = 50;

impl Palette {
    pub fn show(&mut self, ctx: &egui::Context, store: &BookmarkStore) -> PaletteResult {
        let mut result = PaletteResult::Open;
        let mut hits = search::search(store, &self.query);
        hits.truncate(PALETTE_ROWS);
        // With no matching bookmark, something that looks like an address connects directly.
        let address = (hits.is_empty() && self.query.contains(['@', '.', ':']))
            .then(|| crate::bookmarks::model::parse_target(&self.query).ok())
            .flatten()
            .map(|_| self.query.trim().to_string());
        let area = egui::Area::new(egui::Id::new("palette-area"))
            .anchor(Align2::CENTER_TOP, vec2(0.0, 90.0));
        let modal = egui::Modal::new(egui::Id::new("palette"))
            .area(area)
            .show(ctx, |ui| {
                ui.set_width(520.0);
                let edit = egui::TextEdit::singleline(&mut self.query)
                    .hint_text(t("Connect to… (type to search connections)"))
                    .font(egui::TextStyle::Heading)
                    .desired_width(f32::INFINITY);
                let r = ui.add(edit);
                if !self.focused {
                    r.request_focus();
                    self.focused = true;
                }
                if r.changed() {
                    self.index = 0;
                }
                let (down, up, enter) = ui.input_mut(|i| {
                    (
                        i.consume_key(egui::Modifiers::NONE, Key::ArrowDown),
                        i.consume_key(egui::Modifiers::NONE, Key::ArrowUp),
                        i.key_pressed(Key::Enter),
                    )
                });
                if down {
                    self.index = (self.index + 1).min(hits.len().saturating_sub(1));
                }
                if up {
                    self.index = self.index.saturating_sub(1);
                }
                if r.has_focus() {
                    ui.memory_mut(|m| {
                        m.set_focus_lock_filter(
                            r.id,
                            egui::EventFilter {
                                vertical_arrows: true,
                                ..Default::default()
                            },
                        )
                    });
                }
                if enter {
                    result = match (hits.get(self.index), &address) {
                        (Some(id), _) => PaletteResult::Connect(*id),
                        (None, Some(a)) => PaletteResult::ConnectTo(a.clone()),
                        (None, None) => PaletteResult::Close,
                    };
                }
                ui.add_space(6.0);
                if let Some(a) = &address {
                    let resp = ui.add(
                        egui::Button::selectable(true, RichText::new(tf("Connect to {}", &[a])))
                            .min_size(vec2(ui.available_width(), 26.0)),
                    );
                    if resp.clicked() {
                        result = PaletteResult::ConnectTo(a.clone());
                    }
                } else if hits.is_empty() {
                    ui.label(
                        RichText::new(if store.bookmarks.is_empty() {
                            t("No connections yet")
                        } else {
                            t("No matches")
                        })
                        .weak(),
                    );
                }
                egui::ScrollArea::vertical()
                    .max_height(360.0)
                    .show(ui, |ui| {
                        for (i, id) in hits.iter().enumerate() {
                            let Some(b) = store.bookmark(*id) else {
                                continue;
                            };
                            let selected = i == self.index;
                            let path = store.group_path(b.group);
                            let resp = ui.add(
                                egui::Button::selectable(
                                    selected,
                                    egui::RichText::new(format!(
                                        "{}    {}",
                                        b.display_name(),
                                        b.address()
                                    )),
                                )
                                .right_text(RichText::new(path).weak())
                                .min_size(vec2(ui.available_width(), 26.0)),
                            );
                            if selected && (down || up) {
                                resp.scroll_to_me(None);
                            }
                            if resp.clicked() {
                                result = PaletteResult::Connect(*id);
                            }
                        }
                    });
            });
        if modal.should_close() && matches!(result, PaletteResult::Open) {
            result = PaletteResult::Close;
        }
        result
    }
}

// ----- simple dialogs -------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum ConfirmAction {
    DeleteBookmark(Uuid),
    DeleteGroup(Uuid),
    /// Tab id.
    CloseTab(u64),
    /// Tab id.
    Reconnect(u64),
    /// Quit so the downloaded update can install.
    RestartForUpdate,
    Quit,
}

pub enum GroupTarget {
    New { parent: Option<Uuid> },
    Rename(Uuid),
}

pub enum Dialog {
    Confirm {
        title: String,
        message: String,
        button: String,
        action: ConfirmAction,
    },
    GroupName {
        target: GroupTarget,
        name: String,
        focus: bool,
    },
    Export(ExportDialog),
    Import(ImportDialog),
    Settings(SettingsDialog),
    Shortcuts,
}

pub enum DialogResult {
    Open,
    Close,
    Confirmed(ConfirmAction),
    GroupName(GroupTarget, String),
    Export {
        scope: ExportScope,
        passphrase: Option<String>,
    },
    Import(Box<ImportPlan>),
}

/// For dialogs holding typed input: only Esc closes them, not a click beside the dialog.
pub fn escape_only<T>(ctx: &egui::Context, modal: &egui::ModalResponse<T>) -> bool {
    modal.should_close() && ctx.input(|i| i.key_pressed(Key::Escape))
}

pub fn confirm(title: &str, message: String, button: &str, action: ConfirmAction) -> Dialog {
    Dialog::Confirm {
        title: title.into(),
        message,
        button: button.into(),
        action,
    }
}

impl Dialog {
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        store: &BookmarkStore,
        settings: &mut Settings,
    ) -> DialogResult {
        match self {
            Dialog::Confirm {
                title,
                message,
                button,
                action,
            } => {
                let mut result = DialogResult::Open;
                let modal = egui::Modal::new(egui::Id::new("confirm")).show(ctx, |ui| {
                    ui.set_width(380.0);
                    ui.heading(title.as_str());
                    ui.add_space(4.0);
                    ui.label(message.as_str());
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button(RichText::new(button.as_str()).strong()).clicked()
                            || ui.input(|i| i.key_pressed(Key::Enter))
                        {
                            result = DialogResult::Confirmed(action.clone());
                        }
                        if ui.button(t("Cancel")).clicked() {
                            result = DialogResult::Close;
                        }
                    });
                });
                if modal.should_close() && matches!(result, DialogResult::Open) {
                    result = DialogResult::Close;
                }
                result
            }
            Dialog::GroupName {
                target,
                name,
                focus,
            } => {
                let mut result = DialogResult::Open;
                let mut submit = false;
                let modal = egui::Modal::new(egui::Id::new("group-name")).show(ctx, |ui| {
                    ui.set_width(320.0);
                    ui.heading(match target {
                        GroupTarget::New { .. } => t("New Group"),
                        GroupTarget::Rename(_) => t("Rename Group"),
                    });
                    let r = ui.add(
                        egui::TextEdit::singleline(name)
                            .hint_text(t("Group name"))
                            .desired_width(f32::INFINITY),
                    );
                    if std::mem::take(focus) {
                        r.request_focus();
                    }
                    if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        submit = true;
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button(t("OK")).clicked() {
                            submit = true;
                        }
                        if ui.button(t("Cancel")).clicked() {
                            result = DialogResult::Close;
                        }
                    });
                });
                if submit && !name.trim().is_empty() {
                    let target = std::mem::replace(target, GroupTarget::New { parent: None });
                    return DialogResult::GroupName(target, name.trim().to_string());
                }
                if modal.should_close() && matches!(result, DialogResult::Open) {
                    result = DialogResult::Close;
                }
                result
            }
            Dialog::Export(d) => d.show(ctx, store),
            Dialog::Import(d) => d.show(ctx),
            Dialog::Settings(d) => d.show(ctx, settings),
            Dialog::Shortcuts => shortcuts(ctx),
        }
    }
}

fn shortcuts(ctx: &egui::Context) -> DialogResult {
    let mut close = false;
    let modal = egui::Modal::new(egui::Id::new("shortcuts")).show(ctx, |ui| {
        ui.set_width(420.0);
        ui.heading(t("Keyboard Shortcuts"));
        ui.add_space(6.0);
        egui::Grid::new("shortcuts-grid")
            .num_columns(2)
            .striped(true)
            .spacing([24.0, 6.0])
            .show(ui, |ui| {
                for (keys, what) in [
                    ("⌘K", t("Search connections and connect")),
                    ("⌘T", t("New tab (same server)")),
                    ("⌘W", t("Close pane or tab")),
                    ("⌘1 … ⌘9", t("Switch to tab")),
                    ("⇧⌘[  ⇧⌘]", t("Previous / next tab")),
                    ("⌘R", t("Reconnect")),
                    ("⌘D  ⇧⌘D", t("Split right / down")),
                    ("⌘[  ⌘]", t("Previous / next pane")),
                    ("⌘B", t("Show/Hide Sidebar")),
                    ("⇧⌘F", t("SFTP Panel")),
                    ("⌘N", t("New Connection…")),
                    ("⌘=  ⌘-  ⌘0", t("Text size")),
                    ("⌥⌘K", t("Clear Scrollback")),
                    ("⇧PageUp / ⇧PageDown", t("Scroll back")),
                    ("⌘,", t("Settings…")),
                ] {
                    ui.monospace(keys);
                    ui.label(what);
                    ui.end_row();
                }
            });
        ui.add_space(8.0);
        if ui.button(t("Close")).clicked() {
            close = true;
        }
    });
    if close || modal.should_close() {
        DialogResult::Close
    } else {
        DialogResult::Open
    }
}

// ----- export ---------------------------------------------------------------------------------

pub struct ExportDialog {
    pub scope: ExportScope,
    include_credentials: bool,
    pass1: String,
    pass2: String,
    error: Option<String>,
    picked: HashSet<Uuid>,
}

impl ExportDialog {
    pub fn new(scope: ExportScope) -> Self {
        let picked = match &scope {
            ExportScope::Selected(ids) => ids.iter().copied().collect(),
            _ => HashSet::new(),
        };
        Self {
            scope,
            include_credentials: false,
            pass1: String::new(),
            pass2: String::new(),
            error: None,
            picked,
        }
    }

    fn show(&mut self, ctx: &egui::Context, store: &BookmarkStore) -> DialogResult {
        let mut result = DialogResult::Open;
        let modal = egui::Modal::new(egui::Id::new("export")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.heading(t("Export Connections"));
            ui.add_space(6.0);
            ui.label(RichText::new(t("What to export")).strong());
            let is_all = self.scope == ExportScope::All;
            if ui.radio(is_all, tf("All connections ({})", &[&store.bookmarks.len().to_string()])).clicked() {
                self.scope = ExportScope::All;
            }
            let group = match &self.scope {
                ExportScope::Group(g) => Some(*g),
                _ => None,
            };
            ui.horizontal(|ui| {
                if ui.radio(group.is_some(), t("One group:")).clicked() && group.is_none()
                    && let Some(g) = store.groups.first() {
                        self.scope = ExportScope::Group(g.id);
                    }
                ui.add_enabled_ui(!store.groups.is_empty(), |ui| {
                    let label = group.map(|g| store.group_path(Some(g))).unwrap_or_else(|| t("(choose)").into());
                    egui::ComboBox::from_id_salt("export-group").selected_text(label).width(260.0).show_ui(ui, |ui| {
                        for g in &store.groups {
                            if ui.selectable_label(group == Some(g.id), store.group_path(Some(g.id))).clicked() {
                                self.scope = ExportScope::Group(g.id);
                            }
                        }
                    });
                });
            });
            let selected_mode = matches!(self.scope, ExportScope::Selected(_));
            if ui.radio(selected_mode, t("Selected connections")).clicked() && !selected_mode {
                self.scope = ExportScope::Selected(self.picked.iter().copied().collect());
            }
            if selected_mode {
                egui::ScrollArea::vertical().max_height(160.0).show(ui, |ui| {
                    for b in &store.bookmarks {
                        let mut on = self.picked.contains(&b.id);
                        if ui.checkbox(&mut on, format!("{}  ({})", b.display_name(), b.address())).changed() {
                            if on {
                                self.picked.insert(b.id);
                            } else {
                                self.picked.remove(&b.id);
                            }
                        }
                    }
                });
                self.scope = ExportScope::Selected(store.bookmarks.iter().filter(|b| self.picked.contains(&b.id)).map(|b| b.id).collect());
            }

            ui.add_space(8.0);
            ui.checkbox(&mut self.include_credentials, t("Include saved passwords and passphrases (encrypted)"));
            if self.include_credentials {
                ui.label(RichText::new(t("Credentials are encrypted with Argon2id + XChaCha20-Poly1305. You need this passphrase to import them.")).weak().small());
                egui::Grid::new("export-pass").num_columns(2).show(ui, |ui| {
                    ui.label(t("Passphrase"));
                    ui.add(egui::TextEdit::singleline(&mut self.pass1).password(true).desired_width(240.0));
                    ui.end_row();
                    ui.label(t("Repeat"));
                    ui.add(egui::TextEdit::singleline(&mut self.pass2).password(true).desired_width(240.0));
                    ui.end_row();
                });
            } else {
                ui.label(RichText::new(t("Private key files are referenced by path only; their contents are never exported.")).weak().small());
            }
            if let Some(e) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button(RichText::new(t("Export…")).strong()).clicked() {
                    if matches!(&self.scope, ExportScope::Selected(ids) if ids.is_empty()) {
                        self.error = Some(t("Select at least one connection.").into());
                    } else if self.include_credentials && self.pass1.len() < 8 {
                        self.error = Some(t("Use a passphrase of at least 8 characters.").into());
                    } else if self.include_credentials && self.pass1 != self.pass2 {
                        self.error = Some(t("The passphrases do not match.").into());
                    } else {
                        result = DialogResult::Export {
                            scope: self.scope.clone(),
                            passphrase: self.include_credentials.then(|| self.pass1.clone()),
                        };
                    }
                }
                if ui.button(t("Cancel")).clicked() {
                    result = DialogResult::Close;
                }
            });
        });
        if escape_only(ctx, &modal) && matches!(result, DialogResult::Open) {
            result = DialogResult::Close;
        }
        result
    }
}

// ----- import ---------------------------------------------------------------------------------

pub struct ImportDialog {
    pub plan: ImportPlan,
    pub source: String,
    passphrase: String,
    unlock_message: Option<(String, bool)>,
    /// Key derivation runs on a worker thread (it takes a moment by design).
    unlocking: Option<std::sync::mpsc::Receiver<anyhow::Result<Vec<u8>>>>,
}

impl ImportDialog {
    pub fn new(plan: ImportPlan, source: String) -> Self {
        Self {
            plan,
            source,
            passphrase: String::new(),
            unlock_message: None,
            unlocking: None,
        }
    }

    fn start_unlock(&mut self, ctx: &egui::Context) {
        let Some(blob) = self.plan.encrypted_credentials.clone() else {
            return;
        };
        let passphrase = self.passphrase.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(crate::crypto::decrypt(&passphrase, &blob));
            ctx.request_repaint();
        });
        self.unlocking = Some(rx);
        self.unlock_message = None;
    }

    fn poll_unlock(&mut self) {
        let Some(rx) = &self.unlocking else {
            return;
        };
        let result = match rx.try_recv() {
            Ok(r) => r,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err(anyhow::anyhow!("{}", t("Decryption failed.")))
            }
        };
        self.unlocking = None;
        self.unlock_message = Some(
            match result.and_then(|plain| self.plan.attach_credentials(&plain)) {
                Ok(n) => (
                    tf(
                        "Unlocked credentials for {} connection(s).",
                        &[&n.to_string()],
                    ),
                    false,
                ),
                Err(e) => (format!("{e:#}"), true),
            },
        );
    }

    fn show(&mut self, ctx: &egui::Context) -> DialogResult {
        self.poll_unlock();
        let mut result = DialogResult::Open;
        let modal = egui::Modal::new(egui::Id::new("import")).show(ctx, |ui| {
            ui.set_width(640.0);
            let kind = match self.plan.kind {
                SourceKind::Peler => "peler",
                SourceKind::Electerm => "Electerm",
                SourceKind::SshConfig => "SSH config",
            };
            ui.heading(t("Import Connections"));
            ui.label(RichText::new(format!("{} · {kind}", self.source)).weak());
            for w in &self.plan.warnings {
                ui.label(RichText::new(format!("⚠ {w}")).color(Color32::from_rgb(0xd0, 0x9a, 0x2a)));
            }
            if self.plan.encrypted_credentials.is_some() {
                ui.add_space(4.0);
                ui.group(|ui| {
                    ui.label(t("This file contains encrypted passwords. Enter the export passphrase to import them, or continue without them."));
                    ui.horizontal(|ui| {
                        let busy = self.unlocking.is_some();
                        let r = ui.add_enabled(!busy, egui::TextEdit::singleline(&mut self.passphrase).password(true).hint_text(t("Passphrase")).desired_width(240.0));
                        let go = ui.add_enabled(!busy, egui::Button::new(t("Unlock"))).clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)));
                        if busy {
                            ui.spinner();
                        } else if go && !self.passphrase.is_empty() {
                            self.start_unlock(ui.ctx());
                        }
                    });
                });
            }
            if let Some((msg, err)) = &self.unlock_message {
                let color = if *err { ui.visuals().error_fg_color } else { Color32::from_rgb(0x3f, 0xb9, 0x50) };
                ui.colored_label(color, msg);
            }

            let commands = self.plan.startup_command_count();
            if commands > 0 {
                ui.add_space(4.0);
                ui.group(|ui| {
                    ui.checkbox(
                        &mut self.plan.import_startup_commands,
                        tf("Import startup commands ({} connection(s))", &[&commands.to_string()]),
                    );
                    ui.label(RichText::new(t("Startup commands run on the server every time the connection opens. Only import them from a file you trust.")).weak().small());
                });
            }

            let (add, change, skip) = self.plan.summary();
            ui.add_space(6.0);
            ui.label(RichText::new(tf("{} to add · {} to update · {} to skip", &[&add.to_string(), &change.to_string(), &skip.to_string()])).strong());
            if self.plan.conflicts() > 0 {
                ui.horizontal(|ui| {
                    ui.label(tf("{} conflict(s). For all:", &[&self.plan.conflicts().to_string()]));
                    if ui.small_button(t("Skip")).clicked() {
                        self.plan.resolve_all_conflicts(Resolution::Skip);
                    }
                    if ui.small_button(t("Overwrite")).clicked() {
                        self.plan.resolve_all_conflicts(Resolution::Overwrite);
                    }
                    if ui.small_button(t("Keep both")).clicked() {
                        self.plan.resolve_all_conflicts(Resolution::KeepBoth);
                    }
                });
            }
            ui.add_space(4.0);
            egui::ScrollArea::vertical().max_height(320.0).auto_shrink([false, true]).show(ui, |ui| {
                egui::Grid::new("import-items").num_columns(4).striped(true).spacing([12.0, 4.0]).show(ui, |ui| {
                    ui.label(RichText::new(t("Connection")).strong());
                    ui.label(RichText::new(t("Address")).strong());
                    ui.label(RichText::new(t("Group")).strong());
                    ui.label(RichText::new(t("Action")).strong());
                    ui.end_row();
                    for (i, item) in self.plan.items.iter_mut().enumerate() {
                        let mut name = item.incoming.display_name();
                        if !item.secrets.is_empty() {
                            name.push_str("  🔑");
                        }
                        ui.label(name);
                        match (&item.status, &item.existing_address) {
                            (ItemStatus::Conflict { .. }, Some(old)) => {
                                ui.label(RichText::new(format!("{old} → {}", item.incoming.address())).color(Color32::from_rgb(0xd0, 0x9a, 0x2a)))
                                    .on_hover_text(t("Overwriting changes the address; the credentials saved for the old address are removed."));
                            }
                            _ => {
                                ui.label(RichText::new(item.incoming.address()).weak());
                            }
                        }
                        ui.label(RichText::new(&item.group_path).weak());
                        match item.status {
                            ItemStatus::New => {
                                ui.label(RichText::new(t("Add")).color(Color32::from_rgb(0x3f, 0xb9, 0x50)));
                            }
                            ItemStatus::Identical { .. } => {
                                ui.label(RichText::new(t("Already present")).weak());
                            }
                            ItemStatus::Conflict { matched_by, .. } => {
                                let why = match matched_by {
                                    MatchedBy::Id => t("same connection, changed"),
                                    MatchedBy::Endpoint => t("same host, port and user"),
                                };
                                ui.horizontal(|ui| {
                                    egui::ComboBox::from_id_salt(("res", i))
                                        .selected_text(match item.resolution {
                                            Resolution::Skip => t("Skip"),
                                            Resolution::Overwrite => t("Overwrite"),
                                            Resolution::KeepBoth => t("Keep both"),
                                            Resolution::Add => t("Add"),
                                        })
                                        .width(110.0)
                                        .show_ui(ui, |ui| {
                                            ui.selectable_value(&mut item.resolution, Resolution::Skip, t("Skip"));
                                            ui.selectable_value(&mut item.resolution, Resolution::Overwrite, t("Overwrite"));
                                            ui.selectable_value(&mut item.resolution, Resolution::KeepBoth, t("Keep both"));
                                        });
                                    ui.label(RichText::new(why).weak().small());
                                });
                            }
                        }
                        ui.end_row();
                    }
                });
            });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let (add, change, _) = self.plan.summary();
                if ui.add_enabled(add + change > 0 && self.unlocking.is_none(), egui::Button::new(RichText::new(t("Import")).strong())).clicked() {
                    result = DialogResult::Import(Box::new(self.plan.clone()));
                }
                if ui.button(t("Cancel")).clicked() {
                    result = DialogResult::Close;
                }
            });
        });
        if escape_only(ctx, &modal) && matches!(result, DialogResult::Open) {
            result = DialogResult::Close;
        }
        result
    }
}

// ----- settings -------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum SettingsTab {
    #[default]
    General,
    Terminal,
    QuickCommands,
}

#[derive(Default)]
pub struct SettingsDialog {
    tab: SettingsTab,
}

impl SettingsDialog {
    fn show(&mut self, ctx: &egui::Context, s: &mut Settings) -> DialogResult {
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::new("settings")).show(ctx, |ui| {
            ui.set_width(560.0);
            ui.horizontal(|ui| {
                ui.heading(t("Settings"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(t("Done")).clicked() {
                        close = true;
                    }
                });
            });
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, SettingsTab::General, t("General"));
                ui.selectable_value(&mut self.tab, SettingsTab::Terminal, t("Terminal"));
                ui.selectable_value(
                    &mut self.tab,
                    SettingsTab::QuickCommands,
                    t("Quick Commands"),
                );
            });
            ui.separator();
            egui::ScrollArea::vertical()
                .max_height(460.0)
                .show(ui, |ui| match self.tab {
                    SettingsTab::General => general(ui, s),
                    SettingsTab::Terminal => terminal(ui, s),
                    SettingsTab::QuickCommands => quick_commands(ui, s),
                });
        });
        if close || modal.should_close() {
            DialogResult::Close
        } else {
            DialogResult::Open
        }
    }
}

fn general(ui: &mut egui::Ui, s: &mut Settings) {
    egui::Grid::new("settings-general")
        .num_columns(2)
        .spacing([16.0, 10.0])
        .show(ui, |ui| {
            ui.label(t("Language"));
            egui::ComboBox::from_id_salt("lang")
                .selected_text(match s.language {
                    Language::System => t("System"),
                    Language::English => "English",
                    Language::Indonesian => "Bahasa Indonesia",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut s.language, Language::System, t("System"));
                    ui.selectable_value(&mut s.language, Language::English, "English");
                    ui.selectable_value(&mut s.language, Language::Indonesian, "Bahasa Indonesia");
                });
            ui.end_row();

            ui.label(t("Appearance"));
            ui.horizontal(|ui| {
                ui.selectable_value(&mut s.theme, ThemeMode::System, t("System"));
                ui.selectable_value(&mut s.theme, ThemeMode::Light, t("Light"));
                ui.selectable_value(&mut s.theme, ThemeMode::Dark, t("Dark"));
            });
            ui.end_row();

            ui.label(t("Keepalive"));
            ui.horizontal(|ui| {
                ui.add(
                    egui::DragValue::new(&mut s.keepalive_secs)
                        .range(0..=600)
                        .update_while_editing(false)
                        .suffix(" s"),
                );
                ui.label(RichText::new(t("0 = off. Applies to new connections.")).weak());
            });
            ui.end_row();

            ui.label(t("Closing tabs"));
            ui.checkbox(
                &mut s.confirm_close_connected,
                t("Ask before closing a connected tab"),
            );
            ui.end_row();

            ui.label(t("Updates"));
            ui.checkbox(&mut s.check_updates, t("Check for updates automatically"))
                .on_hover_text(t(
                    "Peler asks GitHub for the latest release at launch and once a day.",
                ));
            ui.end_row();

            ui.label(t("Data folder"));
            if ui.button(t("Show in Finder")).clicked() {
                let _ = std::process::Command::new("open")
                    .arg(crate::paths::data_dir())
                    .spawn();
            }
            ui.end_row();
        });
}

fn terminal(ui: &mut egui::Ui, s: &mut Settings) {
    egui::Grid::new("settings-terminal")
        .num_columns(2)
        .spacing([16.0, 10.0])
        .show(ui, |ui| {
            ui.label(t("Font"));
            ui.horizontal(|ui| {
                let current = fonts::effective_choice(&s.font);
                egui::ComboBox::from_id_salt("font")
                    .selected_text(current.name.clone())
                    .width(180.0)
                    .show_ui(ui, |ui| {
                        for p in fonts::PRESETS {
                            let available = p
                                .path
                                .is_none_or(|path| std::path::Path::new(path).exists());
                            if available
                                && ui
                                    .selectable_label(current.name == p.name, p.name)
                                    .clicked()
                            {
                                s.font = FontChoice {
                                    name: p.name.into(),
                                    path: None,
                                    index: 0,
                                };
                            }
                        }
                    });
                if ui.button(t("Other font file…")).clicked()
                    && let Some(p) = rfd::FileDialog::new()
                        .add_filter(t("Fonts"), &["ttf", "otf", "ttc"])
                        .pick_file()
                {
                    let name = p
                        .file_stem()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    s.font = FontChoice {
                        name,
                        path: Some(crate::paths::contract_tilde(&p)),
                        index: 0,
                    };
                }
            });
            ui.end_row();

            ui.label(t("Size"));
            ui.add(
                egui::Slider::new(&mut s.font_size, Settings::MIN_FONT..=Settings::MAX_FONT)
                    .step_by(0.5)
                    .suffix(" pt"),
            );
            ui.end_row();

            for (label, value, dark) in [
                (t("Colors (dark)"), &mut s.scheme_dark, true),
                (t("Colors (light)"), &mut s.scheme_light, false),
            ] {
                ui.label(label);
                egui::ComboBox::from_id_salt(("scheme", dark))
                    .selected_text(value.clone())
                    .width(180.0)
                    .show_ui(ui, |ui| {
                        for sc in SCHEMES.iter().filter(|sc| sc.dark == dark) {
                            ui.horizontal(|ui| {
                                let (r, _) =
                                    ui.allocate_exact_size(vec2(42.0, 14.0), egui::Sense::hover());
                                ui.painter().rect_filled(r, 2.0, sc.background);
                                for (i, c) in sc.ansi[1..7].iter().enumerate() {
                                    let x = r.left() + 3.0 + i as f32 * 6.0;
                                    ui.painter().rect_filled(
                                        egui::Rect::from_min_size(
                                            egui::pos2(x, r.top() + 3.0),
                                            vec2(5.0, 8.0),
                                        ),
                                        1.0,
                                        *c,
                                    );
                                }
                                if ui.selectable_label(*value == sc.name, sc.name).clicked() {
                                    *value = sc.name.to_string();
                                }
                            });
                        }
                    });
                ui.end_row();
            }

            ui.label(t("Scrollback"));
            ui.horizontal(|ui| {
                ui.add(
                    egui::DragValue::new(&mut s.scrollback)
                        .range(0..=Settings::MAX_SCROLLBACK)
                        .speed(100)
                        // Applied when typing ends: each change resizes every open terminal's
                        // history, and "5" on the way to "50000" would cut it to 5 lines.
                        .update_while_editing(false)
                        .suffix(t(" lines")),
                );
            });
            ui.end_row();

            ui.label(t("Cursor"));
            ui.horizontal(|ui| {
                ui.selectable_value(&mut s.cursor_style, CursorStyle::Block, t("Block"));
                ui.selectable_value(&mut s.cursor_style, CursorStyle::Beam, t("Bar"));
                ui.selectable_value(&mut s.cursor_style, CursorStyle::Underline, t("Underline"));
                ui.checkbox(&mut s.cursor_blink, t("Blink"));
            });
            ui.end_row();

            ui.label(t("Keyboard"));
            ui.checkbox(&mut s.option_as_meta, t("Use Option as Meta key"));
            ui.end_row();

            ui.label(t("Selection"));
            ui.checkbox(&mut s.copy_on_select, t("Copy text when selected"));
            ui.end_row();

            ui.label(t("Fonts"));
            ui.checkbox(
                &mut s.load_cjk_font,
                t("Chinese/Japanese/Korean fallback fonts"),
            );
            ui.end_row();
        });
}

fn quick_commands(ui: &mut egui::Ui, s: &mut Settings) {
    ui.label(
        RichText::new(t(
            "Commands you can send to the active terminal from the toolbar.",
        ))
        .weak(),
    );
    ui.add_space(6.0);
    let mut remove = None;
    let mut swap = None;
    let n = s.quick_commands.len();
    egui::Grid::new("qc")
        .num_columns(5)
        .spacing([8.0, 6.0])
        .show(ui, |ui| {
            ui.label(RichText::new(t("Name")).strong());
            ui.label(RichText::new(t("Command")).strong());
            ui.label(RichText::new(t("Run")).strong());
            ui.label("");
            ui.label("");
            ui.end_row();
            for (i, qc) in s.quick_commands.iter_mut().enumerate() {
                ui.add(egui::TextEdit::singleline(&mut qc.name).desired_width(130.0));
                ui.add(
                    egui::TextEdit::singleline(&mut qc.command)
                        .desired_width(240.0)
                        .font(egui::TextStyle::Monospace),
                );
                ui.checkbox(&mut qc.run, "")
                    .on_hover_text(t("Press Enter after sending"));
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(i > 0, egui::Button::new("▲").small())
                        .clicked()
                    {
                        swap = Some((i, i - 1));
                    }
                    if ui
                        .add_enabled(i + 1 < n, egui::Button::new("▼").small())
                        .clicked()
                    {
                        swap = Some((i, i + 1));
                    }
                });
                if ui.small_button("×").clicked() {
                    remove = Some(i);
                }
                ui.end_row();
            }
        });
    if let Some(i) = remove {
        s.quick_commands.remove(i);
    }
    if let Some((a, b)) = swap {
        s.quick_commands.swap(a, b);
    }
    if ui.button(t("Add command")).clicked() {
        s.quick_commands.push(QuickCommand::new("", ""));
    }
}

// ----- toasts ---------------------------------------------------------------------------------

#[derive(Default)]
pub struct Toasts {
    items: Vec<(String, bool, Instant)>,
}

impl Toasts {
    pub fn info(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        #[cfg(debug_assertions)]
        eprintln!("DEVTEST LOG: toast info: {msg}");
        self.items.push((msg, false, Instant::now()));
    }

    pub fn error(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        #[cfg(debug_assertions)]
        eprintln!("DEVTEST LOG: toast error: {msg}");
        self.items.push((msg, true, Instant::now()));
    }

    pub fn show(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        self.items.retain(|(_, err, at)| {
            now.duration_since(*at) < Duration::from_secs(if *err { 9 } else { 4 })
        });
        if self.items.is_empty() {
            return;
        }
        ctx.request_repaint_after(Duration::from_millis(500));
        egui::Area::new(egui::Id::new("toasts"))
            .anchor(Align2::RIGHT_BOTTOM, vec2(-16.0, -16.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                let mut dismiss = None;
                for (i, (msg, err, _)) in self.items.iter().enumerate() {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_max_width(360.0);
                        ui.horizontal(|ui| {
                            let color = if *err {
                                ui.visuals().error_fg_color
                            } else {
                                ui.visuals().text_color()
                            };
                            ui.label(RichText::new(if *err { "⚠" } else { "✓" }).color(color));
                            ui.label(RichText::new(msg).color(color));
                            if ui.small_button("×").clicked() {
                                dismiss = Some(i);
                            }
                        });
                    });
                    ui.add_space(4.0);
                }
                if let Some(i) = dismiss {
                    self.items.remove(i);
                }
            });
    }
}
