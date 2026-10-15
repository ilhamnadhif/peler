//! Bookmark editor (create and edit). Secrets typed here go to the Keychain on save.

use eframe::egui::{self, RichText};

use crate::bookmarks::{Auth, Bookmark, BookmarkStore, DEFAULT_PORT};
use crate::i18n::t;
use crate::paths;
use crate::secrets::{self, SecretKind, SecretSet};

pub struct Editor {
    pub draft: Bookmark,
    pub is_new: bool,
    port_text: String,
    tags_text: String,
    password: String,
    passphrase: String,
    had_password: bool,
    had_passphrase: bool,
    clear_password: bool,
    clear_passphrase: bool,
    error: Option<String>,
    focus_first: bool,
    key_files: Vec<String>,
}

pub enum EditorResult {
    Open,
    Cancel,
    Save { bookmark: Bookmark, set: SecretSet, clear: Vec<SecretKind>, connect: bool },
}

fn ssh_keys() -> Vec<String> {
    let dir = paths::home_dir().join(".ssh");
    let mut keys: Vec<String> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let path = e.path();
            let looks_private = !name.ends_with(".pub")
                && !matches!(name.as_str(), "known_hosts" | "known_hosts.old" | "config" | "authorized_keys")
                && path.is_file()
                && std::fs::read_to_string(&path).map(|s| s.contains("PRIVATE KEY-----")).unwrap_or(false);
            looks_private.then(|| format!("~/.ssh/{name}"))
        })
        .collect();
    keys.sort();
    keys
}

impl Editor {
    pub fn new(bookmark: Bookmark, is_new: bool) -> Self {
        let had_password = !is_new && secrets::has(bookmark.id, SecretKind::Password);
        let had_passphrase = !is_new && secrets::has(bookmark.id, SecretKind::Passphrase);
        Self {
            port_text: bookmark.port.to_string(),
            tags_text: bookmark.tags.join(", "),
            draft: bookmark,
            is_new,
            password: String::new(),
            passphrase: String::new(),
            had_password,
            had_passphrase,
            clear_password: false,
            clear_passphrase: false,
            error: None,
            focus_first: true,
            key_files: ssh_keys(),
        }
    }

    fn validate(&mut self) -> bool {
        self.draft.host = self.draft.host.trim().to_string();
        self.draft.username = self.draft.username.trim().to_string();
        // Accept "user@host:port" typed into the host field.
        if let Some((user, rest)) = self.draft.host.clone().split_once('@') {
            if self.draft.username.is_empty() {
                self.draft.username = user.to_string();
            }
            self.draft.host = rest.to_string();
        }
        if let Some((host, port)) = self.draft.host.clone().rsplit_once(':') {
            if !host.contains(':') {
                if let Ok(p) = port.parse::<u16>() {
                    self.draft.host = host.to_string();
                    self.port_text = p.to_string();
                }
            }
        }
        if self.draft.host.is_empty() {
            self.error = Some(t("Host is required.").into());
            return false;
        }
        match self.port_text.trim().parse::<u16>() {
            Ok(p) if p > 0 => self.draft.port = p,
            _ => {
                self.error = Some(t("Port must be a number between 1 and 65535.").into());
                return false;
            }
        }
        if let Auth::Key { path } = &self.draft.auth {
            if path.trim().is_empty() {
                self.error = Some(t("Choose a private key file.").into());
                return false;
            }
            if !paths::expand_tilde(path.trim()).is_file() {
                self.error = Some(t("The private key file does not exist.").into());
                return false;
            }
        }
        self.draft.tags = self.tags_text.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        self.error = None;
        true
    }

    pub fn show(&mut self, ctx: &egui::Context, store: &BookmarkStore) -> EditorResult {
        let mut result = EditorResult::Open;
        let title = if self.is_new { t("New Bookmark") } else { t("Edit Bookmark") };
        let modal = egui::Modal::new(egui::Id::new("bookmark-editor")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.heading(title);
            ui.add_space(6.0);
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.command);
            egui::Grid::new("editor-grid").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                ui.label(t("Name"));
                let r = ui.add(egui::TextEdit::singleline(&mut self.draft.name).hint_text(t("e.g. Production web")).desired_width(f32::INFINITY));
                if std::mem::take(&mut self.focus_first) {
                    r.request_focus();
                }
                ui.end_row();

                ui.label(t("Host"));
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.draft.host).hint_text("server.example.com").desired_width(260.0));
                    ui.label(t("Port"));
                    ui.add(egui::TextEdit::singleline(&mut self.port_text).desired_width(52.0));
                });
                ui.end_row();

                ui.label(t("Username"));
                ui.add(egui::TextEdit::singleline(&mut self.draft.username).hint_text(std::env::var("USER").unwrap_or_default()).desired_width(f32::INFINITY));
                ui.end_row();

                ui.label(t("Authentication"));
                ui.horizontal(|ui| {
                    let is_password = matches!(self.draft.auth, Auth::Password);
                    if ui.radio(is_password, t("Password")).clicked() {
                        self.draft.auth = Auth::Password;
                    }
                    let is_key = matches!(self.draft.auth, Auth::Key { .. });
                    if ui.radio(is_key, t("Private key")).clicked() && !is_key {
                        self.draft.auth = Auth::Key { path: self.key_files.first().cloned().unwrap_or_default() };
                    }
                    if matches!(self.draft.auth, Auth::KeychainKey) {
                        let _ = ui.radio(true, t("Imported key (Keychain)"));
                    }
                });
                ui.end_row();

                match &mut self.draft.auth {
                    Auth::Password => {
                        ui.label(t("Password"));
                        ui.vertical(|ui| {
                            let hint = if self.had_password && !self.clear_password { t("Saved in Keychain — type to replace") } else { t("Leave empty to ask when connecting") };
                            ui.add(egui::TextEdit::singleline(&mut self.password).password(true).hint_text(hint).desired_width(f32::INFINITY));
                            if self.had_password {
                                ui.checkbox(&mut self.clear_password, t("Forget the saved password"));
                            }
                        });
                        ui.end_row();
                    }
                    Auth::Key { path } => {
                        ui.label(t("Key file"));
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(path).hint_text("~/.ssh/id_ed25519").desired_width(250.0));
                            if !self.key_files.is_empty() {
                                egui::ComboBox::from_id_salt("key-files").selected_text("▾").width(30.0).show_ui(ui, |ui| {
                                    for k in &self.key_files {
                                        if ui.selectable_label(path == k, k).clicked() {
                                            *path = k.clone();
                                        }
                                    }
                                });
                            }
                            if ui.button(t("Browse…")).clicked() {
                                if let Some(p) = rfd::FileDialog::new().set_directory(paths::home_dir().join(".ssh")).pick_file() {
                                    *path = paths::contract_tilde(&p);
                                }
                            }
                        });
                        ui.end_row();
                    }
                    Auth::KeychainKey => {}
                }
                if self.draft.auth.uses_key() {
                    ui.label(t("Passphrase"));
                    ui.vertical(|ui| {
                        let hint = if self.had_passphrase && !self.clear_passphrase { t("Saved in Keychain — type to replace") } else { t("Only if the key is encrypted") };
                        ui.add(egui::TextEdit::singleline(&mut self.passphrase).password(true).hint_text(hint).desired_width(f32::INFINITY));
                        if self.had_passphrase {
                            ui.checkbox(&mut self.clear_passphrase, t("Forget the saved passphrase"));
                        }
                    });
                    ui.end_row();
                }

                ui.label(t("Group"));
                let current = if self.draft.group.is_some() { store.group_path(self.draft.group) } else { t("(none)").to_string() };
                egui::ComboBox::from_id_salt("group").selected_text(current).width(300.0).show_ui(ui, |ui| {
                    if ui.selectable_label(self.draft.group.is_none(), t("(none)")).clicked() {
                        self.draft.group = None;
                    }
                    let mut groups: Vec<(String, uuid::Uuid)> = store.groups.iter().map(|g| (store.group_path(Some(g.id)), g.id)).collect();
                    groups.sort();
                    for (path, id) in groups {
                        if ui.selectable_label(self.draft.group == Some(id), path).clicked() {
                            self.draft.group = Some(id);
                        }
                    }
                });
                ui.end_row();

                ui.label(t("Tags"));
                ui.add(egui::TextEdit::singleline(&mut self.tags_text).hint_text(t("comma separated")).desired_width(f32::INFINITY));
                ui.end_row();

                ui.label(t("Start folder"));
                ui.add(egui::TextEdit::singleline(&mut self.draft.start_dir).hint_text("/var/www").desired_width(f32::INFINITY));
                ui.end_row();

                ui.label(t("Run on connect"));
                ui.add(egui::TextEdit::multiline(&mut self.draft.startup_command).hint_text(t("One command per line")).desired_rows(2).desired_width(f32::INFINITY).font(egui::TextStyle::Monospace));
                ui.end_row();

                ui.label(t("Notes"));
                ui.add(egui::TextEdit::multiline(&mut self.draft.notes).desired_rows(2).desired_width(f32::INFINITY));
                ui.end_row();

                ui.label("");
                ui.checkbox(&mut self.draft.legacy_algorithms, t("Allow legacy algorithms (old servers)"))
                    .on_hover_text(t("Also offer ssh-rsa/SHA-1 key exchange and CBC ciphers. Only enable this for servers that need it."));
                ui.end_row();
            });

            if let Some(e) = &self.error {
                ui.add_space(4.0);
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let save_connect = ui.add(egui::Button::new(RichText::new(t("Save & Connect")).strong())).clicked() || enter;
                let save = ui.button(t("Save")).clicked();
                if ui.button(t("Cancel")).clicked() {
                    result = EditorResult::Cancel;
                }
                if (save || save_connect) && self.validate() {
                    let mut set = SecretSet::default();
                    let mut clear = Vec::new();
                    if matches!(self.draft.auth, Auth::Password) {
                        if !self.password.is_empty() {
                            set.password = Some(self.password.clone());
                        } else if self.clear_password {
                            clear.push(SecretKind::Password);
                        }
                    } else {
                        if !self.passphrase.is_empty() {
                            set.passphrase = Some(self.passphrase.clone());
                        } else if self.clear_passphrase {
                            clear.push(SecretKind::Passphrase);
                        }
                    }
                    result = EditorResult::Save { bookmark: self.draft.clone(), set, clear, connect: save_connect };
                }
            });
        });
        if modal.should_close() && matches!(result, EditorResult::Open) {
            result = EditorResult::Cancel;
        }
        let _ = DEFAULT_PORT;
        result
    }
}
