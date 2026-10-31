//! A small editor for remote text files (configs, scripts): opens over SFTP, saves back with
//! ⌘S, keeps the file's line endings, and warns when the file changed on the server meanwhile.

use std::sync::mpsc;

use eframe::egui::text::{CCursor, CCursorRange, CharIndex, LayoutJob};
use eframe::egui::text_edit::TextEditState;
use eframe::egui::{self, Color32, Event, FontId, Id, Key, Modifiers, RichText, TextStyle, vec2};

use super::style;
use crate::i18n::{t, tf};
use crate::sftp::edit::{self, LoadError, Loaded, SaveError, Stamp};
use crate::sftp::entry::{remote_name, remote_parent};
use crate::sftp::remote::Sftp;

enum State {
    Loading,
    Ready,
    Failed(LoadError),
}

enum Msg {
    Loaded(Result<Loaded, LoadError>),
    Saved(Result<Stamp, SaveError>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Confirm {
    /// Close with unsaved changes?
    Discard,
    /// The file changed on the server: overwrite it?
    Overwrite,
    /// The file was deleted on the server: create it again?
    Recreate,
}

pub enum EditorOutput {
    Open,
    /// Closed; `saved` when the file was written at least once (the listing should refresh).
    Close {
        saved: bool,
    },
    /// The file can't be edited here; the user asked to download it.
    Download,
}

enum Status {
    Saving,
    Saved,
    Info(String),
    Error(String),
}

pub struct FileEditor {
    pub path: String,
    state: State,
    text: String,
    /// The text as last loaded or saved.
    saved_text: String,
    /// The text being written right now.
    saving_text: Option<String>,
    crlf: bool,
    bom: bool,
    stamp: Option<Stamp>,
    pending: Option<mpsc::Receiver<Msg>>,
    saved_once: bool,
    status: Option<Status>,
    confirm: Option<Confirm>,
    focus: bool,
    /// Line and column of the cursor (1-based), for the status bar.
    cursor: (usize, usize),
}

impl FileEditor {
    pub fn open(
        rt: &tokio::runtime::Handle,
        ctx: &egui::Context,
        sftp: Sftp,
        path: String,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let (ctx2, p) = (ctx.clone(), path.clone());
        rt.spawn(async move {
            let _ = tx.send(Msg::Loaded(edit::load(&sftp, &p).await));
            ctx2.request_repaint();
        });
        Self {
            path,
            state: State::Loading,
            text: String::new(),
            saved_text: String::new(),
            saving_text: None,
            crlf: false,
            bom: false,
            stamp: None,
            pending: Some(rx),
            saved_once: false,
            status: None,
            confirm: None,
            focus: true,
            cursor: (1, 1),
        }
    }

    pub fn name(&self) -> &str {
        remote_name(&self.path)
    }

    pub fn is_dirty(&self) -> bool {
        matches!(self.state, State::Ready) && self.text != self.saved_text
    }

    /// Closes now if nothing would be lost; otherwise asks first. Returns true when closed.
    pub fn request_close(&mut self) -> bool {
        if self.is_dirty() {
            self.confirm = Some(Confirm::Discard);
            false
        } else {
            true
        }
    }

    fn poll(&mut self) {
        let Some(rx) = &self.pending else { return };
        let Ok(msg) = rx.try_recv() else { return };
        self.pending = None;
        match msg {
            Msg::Loaded(Ok(l)) => {
                self.text = l.text.clone();
                self.saved_text = l.text;
                self.crlf = l.crlf;
                self.bom = l.bom;
                self.stamp = Some(l.stamp);
                self.state = State::Ready;
                self.focus = true;
            }
            Msg::Loaded(Err(e)) => self.state = State::Failed(e),
            Msg::Saved(result) => {
                let written = self.saving_text.take();
                match result {
                    Ok(stamp) => {
                        self.stamp = Some(stamp);
                        self.saved_once = true;
                        if let Some(text) = written {
                            self.saved_text = text;
                        }
                        self.status = Some(Status::Saved);
                    }
                    Err(SaveError::Changed) => {
                        self.status = None;
                        self.confirm = Some(Confirm::Overwrite);
                    }
                    Err(SaveError::Deleted) => {
                        self.status = None;
                        self.confirm = Some(Confirm::Recreate);
                    }
                    // The text stays in the editor, so nothing typed is lost.
                    Err(SaveError::Failed(e)) => {
                        self.status = Some(Status::Error(tf("Could not save: {}", &[&e])))
                    }
                }
            }
        }
    }

    fn save(
        &mut self,
        rt: &tokio::runtime::Handle,
        ctx: &egui::Context,
        sftp: Option<Sftp>,
        overwrite: bool,
    ) {
        if self.saving_text.is_some() || !matches!(self.state, State::Ready) {
            return;
        }
        let Some(sftp) = sftp else {
            self.status = Some(Status::Error(t("Not connected.").into()));
            return;
        };
        let data = edit::encode(&self.text, self.crlf, self.bom);
        let expected = if overwrite { None } else { self.stamp };
        let (tx, rx) = mpsc::channel();
        let (ctx2, path) = (ctx.clone(), self.path.clone());
        rt.spawn(async move {
            let _ = tx.send(Msg::Saved(edit::save(&sftp, &path, &data, expected).await));
            ctx2.request_repaint();
        });
        self.saving_text = Some(self.text.clone());
        self.pending = Some(rx);
        self.status = Some(Status::Saving);
    }

    /// Indentation inserted by Tab: the file's own style (tabs if it indents with tabs).
    fn indent_unit(&self) -> &'static str {
        if self.text.lines().any(|l| l.starts_with('\t')) {
            "\t"
        } else if self.path.ends_with(".py") {
            "    "
        } else {
            "  "
        }
    }

    /// Tab and Enter, handled before the text field sees them: Tab indents with spaces (YAML
    /// forbids tabs), Enter keeps the current line's indentation. Each key press becomes the
    /// text it stands for, in place, so fast typing keeps its order.
    fn handle_keys(&mut self, ctx: &egui::Context, id: Id) {
        if !ctx.memory(|m| m.has_focus(id)) {
            return;
        }
        let pressed = |e: &Event, key: Key| matches!(e, Event::Key { key: k, pressed: true, modifiers, .. } if *k == key && modifiers.is_none());
        if !ctx.input(|i| {
            i.events
                .iter()
                .any(|e| pressed(e, Key::Tab) || pressed(e, Key::Enter))
        }) {
            return;
        }
        let cursor = TextEditState::load(ctx, id)
            .and_then(|s| s.cursor.char_range())
            .map_or(self.text.chars().count(), |r| {
                r.as_sorted_char_range().start.0
            });
        let start = self
            .text
            .char_indices()
            .nth(cursor)
            .map_or(self.text.len(), |(b, _)| b);
        let line_start = self.text[..start].rfind('\n').map_or(0, |i| i + 1);
        let indent: String = self.text[line_start..start]
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        let unit = self.indent_unit();
        ctx.input_mut(|i| {
            for e in &mut i.events {
                if pressed(e, Key::Tab) {
                    *e = Event::Text(unit.to_string());
                } else if pressed(e, Key::Enter) {
                    *e = Event::Text(format!("\n{indent}"));
                }
            }
        });
    }

    /// Writes the text to a file on this Mac (when the server can't take it).
    fn save_copy(&mut self) {
        let Some(dest) = rfd::FileDialog::new()
            .set_title(t("Save a Copy"))
            .set_file_name(self.name())
            .save_file()
        else {
            return;
        };
        let data = edit::encode(&self.text, self.crlf, self.bom);
        self.status = Some(match std::fs::write(&dest, data) {
            Ok(()) => Status::Info(tf("Saved a copy to {}.", &[&dest.display().to_string()])),
            Err(e) => Status::Error(tf("Could not save: {}", &[&e.to_string()])),
        });
    }

    pub fn show(
        &mut self,
        ctx: &egui::Context,
        rt: &tokio::runtime::Handle,
        sftp: Option<Sftp>,
    ) -> EditorOutput {
        self.poll();
        let mut out = EditorOutput::Open;
        let screen = ctx.content_rect();
        let size = vec2(
            (screen.width() - 80.0).clamp(420.0, 1200.0),
            (screen.height() - 120.0).clamp(240.0, 1000.0),
        );
        let text_id = Id::new(("file-editor-text", &self.path));
        let dirty = self.is_dirty();
        let mut save_clicked = false;
        let mut close_clicked = false;
        let mut copy_clicked = false;

        if self.confirm.is_none() {
            let (save_key, escape) = ctx.input_mut(|i| {
                (
                    i.consume_key(Modifiers::COMMAND, Key::S),
                    i.key_pressed(Key::Escape),
                )
            });
            save_clicked |= save_key;
            close_clicked |= escape;
            if matches!(self.state, State::Ready) {
                self.handle_keys(ctx, text_id);
            }
        }

        egui::Modal::new(Id::new(("file-editor", &self.path))).show(ctx, |ui| {
            ui.set_width(size.x);
            let p = style::pal(ui);
            ui.horizontal(|ui| {
                ui.set_height(style::CONTROL_H);
                let (icon, color) = super::icons::file_icon(self.name(), false, false, &p);
                super::kit::icon(ui, icon, 16.0, color);
                ui.label(RichText::new(self.name()).font(style::semibold(14.0)).color(p.text));
                if self.is_dirty() {
                    super::kit::dot(ui, p.accent, 3.5).on_hover_text(t("Unsaved changes"));
                }
                ui.label(RichText::new(remote_parent(&self.path)).small().color(p.text_faint));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(t("Close")).clicked() {
                        close_clicked = true;
                    }
                    let saving = self.saving_text.is_some();
                    let can_save = matches!(self.state, State::Ready) && !saving;
                    if ui
                        .add_enabled_ui(can_save, |ui| {
                            super::kit::button_ex(
                                ui,
                                super::kit::Kind::Primary,
                                Some(super::icons::FLOPPY_DISK),
                                &format!("{}   ⌘S", t("Save")),
                                false,
                            )
                        })
                        .inner
                        .clicked()
                    {
                        save_clicked = true;
                    }
                    if saving {
                        ui.spinner();
                    }
                    if matches!(self.state, State::Ready)
                        && ui
                            .button(t("Save a Copy…"))
                            .on_hover_text(t("Save the text to a file on this Mac"))
                            .clicked()
                    {
                        copy_clicked = true;
                    }
                });
            });
            ui.separator();
            let body_h = size.y - 70.0;
            match &self.state {
                State::Loading => {
                    ui.allocate_ui(vec2(size.x, body_h), |ui| {
                        ui.centered_and_justified(|ui| {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label(t("Opening…"));
                            });
                        });
                    });
                }
                State::Failed(e) => {
                    let can_download = matches!(e, LoadError::TooLarge(_) | LoadError::NotText);
                    let message = e.message();
                    ui.add_space(12.0);
                    ui.colored_label(ui.visuals().error_fg_color, message);
                    ui.add_space(12.0);
                    if can_download && ui.button(t("Download")).clicked() {
                        out = EditorOutput::Download;
                    }
                    ui.add_space(12.0);
                }
                State::Ready => {
                    let font = FontId::new(13.0, egui::FontFamily::Monospace);
                    let color = ui.visuals().text_color();
                    egui::ScrollArea::both()
                        .id_salt(("file-editor-scroll", &self.path))
                        .auto_shrink([false, false])
                        .min_scrolled_height(body_h)
                        .max_height(body_h)
                        .show(ui, |ui| {
                            ui.horizontal_top(|ui| {
                                // Line numbers, in the same font as the text so rows line up.
                                let lines = self.text.split('\n').count();
                                let digits = lines.to_string().len().max(2);
                                let numbers: Vec<String> =
                                    (1..=lines).map(|n| format!("{n:>digits$}")).collect();
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(numbers.join("\n"))
                                            .font(font.clone())
                                            .color(ui.visuals().weak_text_color()),
                                    )
                                    .selectable(false)
                                    .wrap_mode(egui::TextWrapMode::Extend),
                                );
                                // No soft wrapping: line numbers stay aligned with their lines.
                                let mut layouter =
                                    |ui: &egui::Ui, buf: &dyn egui::TextBuffer, _wrap: f32| {
                                        let job = LayoutJob::simple(
                                            buf.as_str().to_owned(),
                                            font.clone(),
                                            color,
                                            f32::INFINITY,
                                        );
                                        ui.fonts_mut(|f| f.layout_job(job))
                                    };
                                let output = egui::TextEdit::multiline(&mut self.text)
                                    .id(text_id)
                                    .font(TextStyle::Monospace)
                                    .code_editor()
                                    .frame(egui::Frame::NONE)
                                    .margin(egui::Margin::ZERO)
                                    .desired_width(f32::INFINITY)
                                    .desired_rows(1)
                                    .layouter(&mut layouter)
                                    .show(ui);
                                if self.focus {
                                    // Start at the top of the file, like any editor.
                                    let mut state = output.state.clone();
                                    state.cursor.set_char_range(Some(CCursorRange::one(
                                        CCursor::new(CharIndex(0)),
                                    )));
                                    state.store(ui.ctx(), text_id);
                                    output.response.request_focus();
                                    self.focus = false;
                                }
                                if let Some(range) = output.cursor_range {
                                    let at = range.primary.index.0;
                                    let before: String = self.text.chars().take(at).collect();
                                    let line = before.matches('\n').count() + 1;
                                    let col =
                                        before.rsplit('\n').next().map_or(0, |l| l.chars().count())
                                            + 1;
                                    self.cursor = (line, col);
                                }
                            });
                        });
                }
            }
            ui.separator();
            ui.horizontal(|ui| {
                if matches!(self.state, State::Ready) {
                    ui.label(
                        RichText::new(format!(
                            "{} {}, {} {}  ·  UTF-8{}  ·  {}",
                            t("Ln"),
                            self.cursor.0,
                            t("Col"),
                            self.cursor.1,
                            if self.bom { " BOM" } else { "" },
                            if self.crlf { "CRLF" } else { "LF" },
                        ))
                        .weak()
                        .small(),
                    );
                }
                let status = match &self.status {
                    Some(Status::Saving) => {
                        Some((t("Saving…").to_string(), ui.visuals().weak_text_color()))
                    }
                    Some(Status::Saved) => {
                        Some((t("Saved.").to_string(), Color32::from_rgb(0x3f, 0xb9, 0x50)))
                    }
                    Some(Status::Info(m)) => Some((m.clone(), ui.visuals().text_color())),
                    Some(Status::Error(m)) => Some((m.clone(), ui.visuals().error_fg_color)),
                    None => None,
                };
                if let Some((msg, color)) = status {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.colored_label(color, msg);
                    });
                }
            });
        });

        if copy_clicked {
            self.save_copy();
        }
        if save_clicked {
            if dirty {
                self.save(rt, ctx, sftp.clone(), false);
            } else if matches!(self.state, State::Ready) {
                self.status = Some(Status::Info(t("No changes to save.").into()));
            }
        }
        // "Saved." only while it is true.
        if self.is_dirty() && matches!(self.status, Some(Status::Saved | Status::Info(_))) {
            self.status = None;
        }

        match self.confirm {
            Some(Confirm::Discard) => {
                let mut choice = None;
                egui::Modal::new(Id::new(("file-editor-discard", &self.path))).show(ctx, |ui| {
                    ui.set_width(380.0);
                    ui.heading(t("Discard changes?"));
                    ui.label(tf(
                        "Your changes to {} have not been saved.",
                        &[self.name()],
                    ));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui
                            .button(RichText::new(t("Keep Editing")).strong())
                            .clicked()
                        {
                            choice = Some(false);
                        }
                        if super::kit::danger(ui, t("Discard")).clicked() {
                            choice = Some(true);
                        }
                    });
                });
                if ctx.input(|i| i.key_pressed(Key::Escape)) {
                    choice = Some(false);
                }
                match choice {
                    Some(true) => {
                        return EditorOutput::Close {
                            saved: self.saved_once,
                        };
                    }
                    Some(false) => self.confirm = None,
                    None => {}
                }
            }
            Some(confirm @ (Confirm::Overwrite | Confirm::Recreate)) => {
                let mut choice = None;
                let (heading, message, button) = if confirm == Confirm::Overwrite {
                    (
                        t("The file changed on the server"),
                        tf(
                            "{} was changed by someone else after you opened it. Saving replaces those changes.",
                            &[self.name()],
                        ),
                        t("Overwrite"),
                    )
                } else {
                    (
                        t("The file was deleted on the server"),
                        tf(
                            "{} was deleted after you opened it. Saving creates it again.",
                            &[self.name()],
                        ),
                        t("Save"),
                    )
                };
                egui::Modal::new(Id::new(("file-editor-overwrite", &self.path))).show(ctx, |ui| {
                    ui.set_width(420.0);
                    ui.heading(heading);
                    ui.label(message);
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button(RichText::new(t("Cancel")).strong()).clicked() {
                            choice = Some(false);
                        }
                        if super::kit::danger(ui, button).clicked() {
                            choice = Some(true);
                        }
                    });
                });
                if ctx.input(|i| i.key_pressed(Key::Escape)) {
                    choice = Some(false);
                }
                match choice {
                    Some(true) => {
                        self.confirm = None;
                        self.save(rt, ctx, sftp, true);
                    }
                    Some(false) => self.confirm = None,
                    None => {}
                }
            }
            None => {
                if close_clicked && self.request_close() {
                    return EditorOutput::Close {
                        saved: self.saved_once,
                    };
                }
            }
        }
        out
    }
}
