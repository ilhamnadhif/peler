//! Modal dialogs: command palette, settings, export, import preview, confirmations, toasts.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Color32, Key, RichText, vec2};
use uuid::Uuid;

use super::{fonts, icons, kit, style};
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
            .frame(
                egui::Frame::popup(&ctx.global_style())
                    .inner_margin(egui::Margin::same(10))
                    .corner_radius(egui::CornerRadius::same(12)),
            )
            .show(ctx, |ui| {
                ui.set_width(560.0);
                let p = style::pal(ui);
                let r = ui
                    .horizontal(|ui| {
                        ui.add_space(6.0);
                        kit::icon(ui, icons::MAGNIFYING_GLASS, 18.0, p.text_muted);
                        ui.add(
                            egui::TextEdit::singleline(&mut self.query)
                                .hint_text(
                                    RichText::new(t("Connect to… (type to search connections)"))
                                        .color(p.text_faint),
                                )
                                .font(egui::FontId::proportional(16.0))
                                .frame(egui::Frame::NONE.inner_margin(egui::Margin::symmetric(4, 10)))
                                .desired_width(f32::INFINITY),
                        )
                    })
                    .inner;
                let line_y = r.rect.bottom() + 4.0;
                ui.painter().hline(
                    (ui.max_rect().left() - 10.0)..=(ui.max_rect().right() + 10.0),
                    line_y,
                    egui::Stroke::new(1.0, p.border),
                );
                ui.add_space(10.0);
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
                if let Some(a) = &address {
                    if palette_row(ui, true, icons::ARROW_RIGHT, &tf("Connect to {}", &[a]), "", "")
                        .clicked()
                    {
                        result = PaletteResult::ConnectTo(a.clone());
                    }
                } else if hits.is_empty() {
                    ui.add_space(8.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            RichText::new(if store.bookmarks.is_empty() {
                                t("No connections yet")
                            } else {
                                t("No matches")
                            })
                            .color(p.text_muted),
                        )
                    });
                    ui.add_space(8.0);
                }
                egui::ScrollArea::vertical()
                    .max_height(380.0)
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        for (i, id) in hits.iter().enumerate() {
                            let Some(b) = store.bookmark(*id) else {
                                continue;
                            };
                            let selected = i == self.index;
                            let path = store.group_path(b.group);
                            let resp = palette_row(
                                ui,
                                selected,
                                icons::HARD_DRIVES,
                                &b.display_name(),
                                &b.address(),
                                &path,
                            );
                            if selected && (down || up) {
                                resp.scroll_to_me(None);
                            }
                            if resp.clicked() {
                                result = PaletteResult::Connect(*id);
                            }
                        }
                    });
                // Key hints along the bottom.
                ui.add_space(8.0);
                let hint_top = ui.cursor().top();
                ui.painter().hline(
                    (ui.max_rect().left() - 10.0)..=(ui.max_rect().right() + 10.0),
                    hint_top,
                    egui::Stroke::new(1.0, p.border),
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    for (keys, what) in [("↑↓", t("navigate")), ("↩", t("connect")), ("esc", t("close"))] {
                        kit::kbd(ui, keys);
                        ui.label(RichText::new(what).small().color(p.text_faint));
                        ui.add_space(8.0);
                    }
                });
            });
        if modal.should_close() && matches!(result, PaletteResult::Open) {
            result = PaletteResult::Close;
        }
        result
    }
}

/// One result of the palette: an icon, the name, the address and the group on the right.
fn palette_row(
    ui: &mut egui::Ui,
    selected: bool,
    icon: &str,
    name: &str,
    address: &str,
    group: &str,
) -> egui::Response {
    let p = style::pal(ui);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 38.0), egui::Sense::click());
    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, egui::CornerRadius::same(7), p.accent_soft);
    } else if resp.hovered() {
        painter.rect_filled(rect, egui::CornerRadius::same(7), p.hover);
    }
    let cy = rect.center().y;
    painter.text(
        egui::pos2(rect.left() + 18.0, cy),
        Align2::CENTER_CENTER,
        icon,
        style::icon_font(16.0),
        if selected { p.accent_text } else { p.text_muted },
    );
    let name_g = painter.layout_no_wrap(name.to_string(), style::medium(14.0), p.text);
    let x = rect.left() + 36.0;
    let group_g = painter.layout_no_wrap(group.to_string(), egui::FontId::proportional(12.0), p.text_faint);
    let right = rect.right() - 12.0 - group_g.size().x - 12.0;
    let clip = egui::Rect::from_min_max(rect.min, egui::pos2(right, rect.bottom()));
    let name_w = name_g.size().x;
    painter
        .with_clip_rect(clip)
        .galley(egui::pos2(x, cy - name_g.size().y / 2.0), name_g, p.text);
    if !address.is_empty() {
        painter.with_clip_rect(clip).text(
            egui::pos2(x + name_w + 10.0, cy),
            Align2::LEFT_CENTER,
            address,
            egui::FontId::proportional(12.5),
            p.text_muted,
        );
    }
    painter.galley(
        egui::pos2(rect.right() - 12.0 - group_g.size().x, cy - group_g.size().y / 2.0),
        group_g,
        p.text_faint,
    );
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, true, selected, name));
    resp
}

/// A tinted note with an icon (warnings in dialogs).
fn callout(ui: &mut egui::Ui, icon: &str, color: Color32, text: &str) {
    let dark = ui.visuals().dark_mode;
    egui::Frame::NONE
        .fill(style::soft(color, if dark { 0.12 } else { 0.08 }))
        .stroke(egui::Stroke::new(1.0, style::soft(color, 0.35)))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = 10.0;
                kit::icon(ui, icon, 16.0, color);
                ui.add(egui::Label::new(RichText::new(text).color(style::pal(ui).text)).wrap());
            });
        });
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
        /// When it first appeared (an Enter typed just before must not confirm it).
        shown_at: Option<Instant>,
        /// "Don't ask again" (ticked or not), for closing a connected tab: confirming with it
        /// ticked turns the question off.
        dont_ask: Option<bool>,
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
    Export { scope: ExportScope },
    Import(Box<ImportPlan>),
}

/// For dialogs holding typed input: only Esc closes them, not a click beside the dialog.
/// (The key is checked first: `should_close` consumes it.)
pub fn escape_only<T>(ctx: &egui::Context, modal: &egui::ModalResponse<T>) -> bool {
    ctx.input(|i| i.key_pressed(Key::Escape)) && modal.should_close()
}

pub fn confirm(title: &str, message: String, button: &str, action: ConfirmAction) -> Dialog {
    Dialog::Confirm {
        title: title.into(),
        message,
        button: button.into(),
        action,
        shown_at: None,
        dont_ask: None,
    }
}

/// The question before closing a connected tab, with "Don't ask again".
pub fn confirm_close(title: &str, message: String, button: &str, action: ConfirmAction) -> Dialog {
    Dialog::Confirm {
        title: title.into(),
        message,
        button: button.into(),
        action,
        shown_at: None,
        dont_ask: Some(false),
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
                shown_at,
                dont_ask,
            } => {
                let mut result = DialogResult::Open;
                // Enter confirms only when pressed (not auto-repeated, no modifiers) once the
                // dialog has been up for a moment, and only while it is on top: a key meant for
                // the terminal must not close a tab or quit.
                let settled = shown_at.get_or_insert_with(Instant::now).elapsed()
                    >= Duration::from_millis(300);
                let id = egui::Id::new("confirm");
                let on_top = ctx.memory(|m| m.top_modal_layer().is_none_or(|l| l.id == id));
                let enter = settled
                    && on_top
                    && ctx.input(|i| {
                        i.events.iter().any(|e| {
                            matches!(e, egui::Event::Key { key: Key::Enter, pressed: true, repeat: false, modifiers, .. } if modifiers.is_none())
                        })
                    });
                let modal = egui::Modal::new(id).show(ctx, |ui| {
                    ui.set_width(400.0);
                    let p = style::pal(ui);
                    let destructive = matches!(
                        action,
                        ConfirmAction::DeleteBookmark(_) | ConfirmAction::DeleteGroup(_)
                    );
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 12.0;
                        let (icon, color) = if destructive {
                            (icons::TRASH, p.err)
                        } else {
                            (icons::QUESTION, p.accent)
                        };
                        kit::icon_badge(ui, icon, color, 40.0);
                        ui.add(egui::Label::new(RichText::new(title.as_str()).font(style::semibold(17.0))).wrap());
                    });
                    ui.add_space(6.0);
                    ui.label(RichText::new(message.as_str()).color(p.text_muted));
                    if let Some(ticked) = dont_ask {
                        ui.add_space(6.0);
                        let r = style::checkbox(ui, ticked, t("Don't ask again"))
                            .on_hover_text(t("You can turn it back on in Settings."));
                        #[cfg(debug_assertions)]
                        crate::devtest::record("confirm-dont-ask", r.rect);
                        if r.changed() {
                            // Enter then confirms instead of toggling the box back.
                            r.surrender_focus();
                        }
                    }
                    ui.add_space(10.0);
                    style::right_to_left(ui, |ui| {
                        let ok = if destructive {
                            kit::danger(ui, button.as_str())
                        } else {
                            ui.add(style::primary(button.as_str()))
                        };
                        #[cfg(debug_assertions)]
                        crate::devtest::record("confirm-ok", ok.rect);
                        if ok.clicked() || enter {
                            result = DialogResult::Confirmed(action.clone());
                        }
                        let cancel = ui.button(t("Cancel"));
                        #[cfg(debug_assertions)]
                        crate::devtest::record("confirm-cancel", cancel.rect);
                        if cancel.clicked() {
                            result = DialogResult::Close;
                        }
                    });
                });
                if modal.should_close() && matches!(result, DialogResult::Open) {
                    result = DialogResult::Close;
                }
                if matches!(result, DialogResult::Confirmed(_)) && *dont_ask == Some(true) {
                    settings.confirm_close_connected = false;
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
                    ui.set_width(340.0);
                    ui.heading(match target {
                        GroupTarget::New { .. } => t("New Group"),
                        GroupTarget::Rename(_) => t("Rename Group"),
                    });
                    style::form_spacing(ui);
                    let r = ui.add(style::input(name).hint_text(t("Group name")));
                    if std::mem::take(focus) {
                        r.request_focus();
                    }
                    if r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                        submit = true;
                    }
                    style::right_to_left(ui, |ui| {
                        if ui.add(style::primary(t("OK"))).clicked() {
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
        ui.set_width(440.0);
        ui.heading(t("Keyboard Shortcuts"));
        ui.add_space(8.0);
        egui::Grid::new("shortcuts-grid")
            .num_columns(2)
            .spacing([24.0, 8.0])
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
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 4.0;
                        for key in keys.split("  ") {
                            kit::kbd(ui, key);
                        }
                    });
                    ui.label(what);
                    ui.end_row();
                }
            });
        ui.add_space(12.0);
        style::right_to_left(ui, |ui| {
            if ui.button(t("Close")).clicked() {
                close = true;
            }
        });
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
            ui.label(kit::section_title(t("What to export")));
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
                    egui::ComboBox::from_id_salt("export-group").icon(super::kit::combo_caret).selected_text(label).width(260.0).show_ui(ui, |ui| {
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
                        if style::checkbox(ui, &mut on, format!("{}  ({})", b.display_name(), b.address())).changed() {
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

            // Everything a connection needs goes along, as it is, so the file opens on another
            // Mac as it was here.
            ui.add_space(8.0);
            ui.label(kit::section_title(t("Passwords and keys")));
            callout(ui, icons::WARNING, style::pal(ui).warn, t("Saved passwords, passphrases and keys (key files too) are written into the file as plain text. Anyone who gets the file can log in to these servers: keep it safe, and delete it once it is imported."));
            if let Some(e) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            ui.add_space(10.0);
            style::right_to_left(ui, |ui| {
                if ui.add(style::primary(t("Export…"))).clicked() {
                    if matches!(&self.scope, ExportScope::Selected(ids) if ids.is_empty()) {
                        self.error = Some(t("Select at least one connection.").into());
                    } else {
                        result = DialogResult::Export {
                            scope: self.scope.clone(),
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
            ui.label(RichText::new(format!("{} · {kind}", self.source)).color(style::pal(ui).text_muted));
            for w in &self.plan.warnings {
                callout(ui, icons::WARNING, style::pal(ui).warn, w);
            }
            if self.plan.encrypted_credentials.is_some() {
                ui.add_space(4.0);
                ui.group(|ui| {
                    ui.label(t("This file contains encrypted passwords and keys. Enter the export passphrase to import them, or continue without them."));
                    ui.horizontal(|ui| {
                        let busy = self.unlocking.is_some();
                        let r = ui.add_enabled(!busy, style::input(&mut self.passphrase).password(true).hint_text(t("Passphrase")).desired_width(240.0));
                        #[cfg(debug_assertions)]
                        crate::devtest::record("import-passphrase", r.rect);
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
                let color = if *err { style::pal(ui).err } else { style::pal(ui).ok };
                ui.colored_label(color, msg);
            }
            let missing = self.plan.keys_missing();
            if missing > 0 {
                callout(ui, icons::WARNING, style::pal(ui).warn, &tf("{} connection(s) log in with a pasted key that this file doesn't include. Paste the key again in Edit Connection before connecting (or choose a key file).", &[&missing.to_string()]));
            }

            let commands = self.plan.startup_command_count();
            if commands > 0 {
                ui.add_space(4.0);
                ui.group(|ui| {
                    style::checkbox(ui,
                        &mut self.plan.import_startup_commands,
                        tf("Import startup commands ({} connection(s))", &[&commands.to_string()]),
                    );
                    ui.label(RichText::new(t("Startup commands run on the server every time the connection opens. Only import them from a file you trust.")).weak().small());
                });
            }

            let (add, change, skip) = self.plan.summary();
            ui.add_space(6.0);
            ui.label(RichText::new(tf("{} to add · {} to update · {} to skip", &[&add.to_string(), &change.to_string(), &skip.to_string()])).font(style::medium(13.5)));
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
                let pal = style::pal(ui);
                egui::Grid::new("import-items").num_columns(4).striped(true).spacing([12.0, 6.0]).show(ui, |ui| {
                    for head in [t("Connection"), t("Address"), t("Group"), t("Action")] {
                        ui.label(RichText::new(head).font(style::medium(12.0)).color(pal.text_muted));
                    }
                    ui.end_row();
                    for (i, item) in self.plan.items.iter_mut().enumerate() {
                        let mut name = item.incoming.display_name();
                        if !item.secrets.is_empty() {
                            name.push_str(&format!("  {}", icons::KEY));
                        }
                        ui.label(name);
                        match (&item.status, &item.existing_address) {
                            (ItemStatus::Conflict { .. }, Some(old)) => {
                                ui.label(RichText::new(format!("{old} → {}", item.incoming.address())).color(pal.warn))
                                    .on_hover_text(t("Overwriting changes the address; the credentials saved for the old address are removed."));
                            }
                            _ => {
                                ui.label(RichText::new(item.incoming.address()).weak());
                            }
                        }
                        ui.label(RichText::new(&item.group_path).weak());
                        match item.status {
                            ItemStatus::New => {
                                ui.label(RichText::new(t("Add")).color(pal.ok));
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
                                    egui::ComboBox::from_id_salt(("res", i)).icon(super::kit::combo_caret)
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
            style::right_to_left(ui, |ui| {
                let (add, change, _) = self.plan.summary();
                let import = ui.add_enabled(add + change > 0 && self.unlocking.is_none(), style::primary(t("Import")));
                #[cfg(debug_assertions)]
                crate::devtest::record("import-ok", import.rect);
                if import.clicked() {
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
            ui.set_width(600.0);
            style::form_spacing(ui);
            ui.horizontal(|ui| {
                ui.heading(t("Settings"));
                style::right_to_left(ui, |ui| {
                    if ui.add(style::primary(t("Done"))).clicked() {
                        close = true;
                    }
                });
            });
            let _tabs = kit::tab_strip(
                ui,
                &mut self.tab,
                &[
                    (SettingsTab::General, t("General")),
                    (SettingsTab::Terminal, t("Terminal")),
                    (SettingsTab::QuickCommands, t("Quick Commands")),
                ],
            );
            #[cfg(debug_assertions)]
            {
                crate::devtest::record("settings-terminal", _tabs[1].rect);
                crate::devtest::record("settings-commands", _tabs[2].rect);
            }
            ui.add_space(4.0);
            egui::ScrollArea::vertical()
                .max_height((ctx.content_rect().height() - 160.0).clamp(240.0, 640.0))
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.set_width(ui.available_width() - 10.0);
                    match self.tab {
                        SettingsTab::General => general(ui, s),
                        SettingsTab::Terminal => terminal(ui, s),
                        SettingsTab::QuickCommands => quick_commands(ui, s),
                    }
                    ui.add_space(4.0);
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
    style::section(ui, t("Appearance"));
    style::row(ui, t("Language"), |ui| {
        egui::ComboBox::from_id_salt("lang").icon(super::kit::combo_caret)
            .width(220.0)
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
    });
    style::row(ui, t("Theme"), |ui| {
        style::segmented(
            ui,
            &mut s.theme,
            &[
                (ThemeMode::System, t("System")),
                (ThemeMode::Light, t("Light")),
                (ThemeMode::Dark, t("Dark")),
            ],
        );
    });

    style::section(ui, t("Connections"));
    style::row(ui, t("Keepalive"), |ui| {
        ui.horizontal(|ui| {
            ui.add(
                egui::DragValue::new(&mut s.keepalive_secs)
                    .range(0..=600)
                    .update_while_editing(false)
                    .suffix(" s"),
            );
            ui.label(RichText::new(t("0 = off. Applies to new connections.")).weak());
        });
    });
    style::row(ui, t("Closing tabs"), |ui| {
        style::checkbox(
            ui,
            &mut s.confirm_close_connected,
            t("Ask before closing a connected tab"),
        );
    });

    style::section(ui, t("Updates"));
    style::row(ui, "", |ui| {
        style::checkbox(
            ui,
            &mut s.check_updates,
            t("Check for updates automatically"),
        )
        .on_hover_text(t(
            "Peler asks GitHub for the latest release when it starts and every few hours.",
        ));
    });
    style::row(ui, t("Data folder"), |ui| {
        if ui.button(t("Show in Finder")).clicked() {
            let _ = std::process::Command::new("open")
                .arg(crate::paths::data_dir())
                .spawn();
        }
    });
}

fn terminal(ui: &mut egui::Ui, s: &mut Settings) {
    style::section(ui, t("Text"));
    style::row(ui, t("Font"), |ui| {
        ui.horizontal(|ui| {
            let current = fonts::effective_choice(&s.font);
            egui::ComboBox::from_id_salt("font").icon(super::kit::combo_caret)
                .selected_text(current.name.clone())
                .width(220.0)
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
    });
    style::row(ui, t("Size"), |ui| {
        ui.add(
            egui::Slider::new(&mut s.font_size, Settings::MIN_FONT..=Settings::MAX_FONT)
                .step_by(0.5)
                .suffix(" pt"),
        );
    });
    style::row(ui, t("Fonts"), |ui| {
        style::checkbox(
            ui,
            &mut s.load_cjk_font,
            t("Chinese/Japanese/Korean fallback fonts"),
        );
    });

    style::section(ui, t("Colors"));
    for (label, value, dark) in [
        (t("Colors (dark)"), &mut s.scheme_dark, true),
        (t("Colors (light)"), &mut s.scheme_light, false),
    ] {
        style::row(ui, label, |ui| {
            egui::ComboBox::from_id_salt(("scheme", dark)).icon(super::kit::combo_caret)
                .selected_text(value.clone())
                .width(220.0)
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
        });
    }

    style::section(ui, t("Behavior"));
    style::row(ui, t("Cursor"), |ui| {
        ui.horizontal(|ui| {
            style::segmented(
                ui,
                &mut s.cursor_style,
                &[
                    (CursorStyle::Block, t("Block")),
                    (CursorStyle::Beam, t("Bar")),
                    (CursorStyle::Underline, t("Underline")),
                ],
            );
            style::checkbox(ui, &mut s.cursor_blink, t("Blink"));
        });
    });
    style::row(ui, t("Scrollback"), |ui| {
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
    style::row(ui, t("Keyboard"), |ui| {
        style::checkbox(ui, &mut s.option_as_meta, t("Use Option as Meta key"));
    });
    style::row(ui, t("Selection"), |ui| {
        style::checkbox(ui, &mut s.copy_on_select, t("Copy text when selected"));
    });
}

fn quick_commands(ui: &mut egui::Ui, s: &mut Settings) {
    ui.add_space(8.0);
    ui.label(
        RichText::new(t(
            "Commands you can send to the active terminal from the toolbar.",
        ))
        .color(style::pal(ui).text_muted),
    );
    ui.add_space(6.0);
    let mut remove = None;
    let mut swap = None;
    let n = s.quick_commands.len();
    egui::Grid::new("qc")
        .num_columns(5)
        .spacing([8.0, 6.0])
        .show(ui, |ui| {
            let muted = style::pal(ui).text_muted;
            ui.label(RichText::new(t("Name")).font(style::medium(12.0)).color(muted));
            ui.label(RichText::new(t("Command")).font(style::medium(12.0)).color(muted));
            ui.label(RichText::new(t("Run")).font(style::medium(12.0)).color(muted));
            ui.label("");
            ui.label("");
            ui.end_row();
            for (i, qc) in s.quick_commands.iter_mut().enumerate() {
                ui.add(style::input(&mut qc.name).desired_width(130.0));
                ui.add(
                    style::input(&mut qc.command)
                        .desired_width(260.0)
                        .font(egui::TextStyle::Monospace),
                );
                style::checkbox(ui, &mut qc.run, "").on_hover_text(t("Press Enter after sending"));
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    let up = ui
                        .add_enabled_ui(i > 0, |ui| kit::icon_button_ex(ui, icons::CARET_UP, t("Move up"), false, 24.0))
                        .inner;
                    if up.clicked() {
                        swap = Some((i, i - 1));
                    }
                    let down = ui
                        .add_enabled_ui(i + 1 < n, |ui| {
                            kit::icon_button_ex(ui, icons::CARET_DOWN, t("Move down"), false, 24.0)
                        })
                        .inner;
                    if down.clicked() {
                        swap = Some((i, i + 1));
                    }
                });
                if kit::icon_button_ex(ui, icons::TRASH, t("Remove"), false, 24.0).clicked() {
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
    ui.add_space(4.0);
    if kit::secondary(ui, Some(icons::PLUS), t("Add command")).clicked() {
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
            // Above the status bar.
            .anchor(Align2::RIGHT_BOTTOM, vec2(-16.0, -40.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                let mut dismiss = None;
                let p = style::pal(ui);
                for (i, (msg, err, _)) in self.items.iter().enumerate() {
                    egui::Frame::popup(ui.style())
                        .corner_radius(egui::CornerRadius::same(10))
                        .inner_margin(egui::Margin::symmetric(12, 10))
                        .show(ui, |ui| {
                            ui.set_max_width(380.0);
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 10.0;
                                let (icon, color) = if *err {
                                    (icons::WARNING_CIRCLE, p.err)
                                } else {
                                    (icons::CHECK_CIRCLE, p.ok)
                                };
                                kit::icon(ui, icon, 17.0, color);
                                ui.add(egui::Label::new(RichText::new(msg).color(p.text)).wrap());
                                if kit::icon_button_ex(ui, icons::X, t("Dismiss"), false, 20.0).clicked() {
                                    dismiss = Some(i);
                                }
                            });
                        });
                    ui.add_space(6.0);
                }
                if let Some(i) = dismiss {
                    self.items.remove(i);
                }
            });
    }
}
