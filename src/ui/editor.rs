//! Bookmark editor (create and edit). Secrets typed here go to the Keychain on save.

use eframe::egui::{self, RichText};

use crate::bookmarks::{Auth, Bookmark, BookmarkStore, DEFAULT_PORT};
use crate::i18n::t;
use crate::paths;
use crate::secrets::{self, SecretKind, SecretSet};
use crate::ssh::keys::{self, KeyCheck};

/// How the connection logs in, as chosen in the editor.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AuthKind {
    Password,
    /// A private key pasted into the editor, kept in the Keychain.
    PastedKey,
    /// A private key file on this Mac.
    KeyFile,
}

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
    auth: AuthKind,
    key_path: String,
    key_text: String,
    /// A pasted key is already in the Keychain (editing an existing connection).
    had_stored_key: bool,
    replace_stored_key: bool,
    /// The last checked key text and what it is.
    key_check: Option<(String, KeyCheck)>,
    /// The login method when the editor opened (a saved passphrase belongs to that key).
    original_auth: Auth,
}

#[allow(clippy::large_enum_variant)] // short-lived, returned once per frame
pub enum EditorResult {
    Open,
    Cancel,
    Save {
        bookmark: Bookmark,
        set: SecretSet,
        clear: Vec<SecretKind>,
        connect: bool,
    },
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
                && !matches!(
                    name.as_str(),
                    "known_hosts" | "known_hosts.old" | "config" | "authorized_keys"
                )
                && path.is_file()
                && std::fs::read_to_string(&path)
                    .map(|s| s.contains("PRIVATE KEY-----"))
                    .unwrap_or(false);
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
        let had_stored_key = !is_new
            && matches!(bookmark.auth, Auth::KeychainKey)
            && secrets::has(bookmark.id, SecretKind::PrivateKey);
        let key_files = ssh_keys();
        let (auth, key_path) = match &bookmark.auth {
            Auth::Password => (
                AuthKind::Password,
                key_files.first().cloned().unwrap_or_default(),
            ),
            Auth::Key { path } => (AuthKind::KeyFile, path.clone()),
            Auth::KeychainKey => (
                AuthKind::PastedKey,
                key_files.first().cloned().unwrap_or_default(),
            ),
        };
        Self {
            original_auth: bookmark.auth.clone(),
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
            key_files,
            auth,
            key_path,
            key_text: String::new(),
            had_stored_key,
            replace_stored_key: false,
            key_check: None,
        }
    }

    /// Shows why saving failed; the editor stays open with everything typed.
    pub fn set_error(&mut self, error: String) {
        self.error = Some(error);
    }

    /// What the pasted key text is (re-checked only when it changes).
    fn pasted_key_check(&mut self) -> Option<KeyCheck> {
        if self.key_text.trim().is_empty() {
            return None;
        }
        if self
            .key_check
            .as_ref()
            .is_none_or(|(text, _)| *text != self.key_text)
        {
            self.key_check = Some((self.key_text.clone(), keys::check_key_text(&self.key_text)));
        }
        self.key_check.as_ref().map(|(_, c)| c.clone())
    }

    /// A new pasted key will be saved (not keeping the one already in the Keychain).
    fn new_pasted_key(&self) -> bool {
        self.auth == AuthKind::PastedKey && (!self.had_stored_key || self.replace_stored_key)
    }

    fn validate(&mut self) -> bool {
        self.draft.username = self.draft.username.trim().to_string();
        // Accept "user@host:port" (and "[ipv6]:port") typed into the host field.
        match crate::bookmarks::model::parse_target(&self.draft.host) {
            Ok(target) => {
                if let Some(user) = target.user {
                    self.draft.username = user;
                }
                self.draft.host = target.host;
                if let Some(p) = target.port {
                    self.port_text = p.to_string();
                }
            }
            Err(e) => {
                self.error = Some(e);
                return false;
            }
        }
        match self.port_text.trim().parse::<u16>() {
            Ok(p) if p > 0 => self.draft.port = p,
            _ => {
                self.error = Some(t("Port must be a number between 1 and 65535.").into());
                return false;
            }
        }
        match self.auth {
            AuthKind::Password => self.draft.auth = Auth::Password,
            AuthKind::KeyFile => {
                let path = self.key_path.trim().to_string();
                if path.is_empty() {
                    self.error = Some(t("Choose a private key file.").into());
                    return false;
                }
                if !paths::expand_tilde(&path).is_file() {
                    self.error = Some(t("The private key file does not exist.").into());
                    return false;
                }
                self.draft.auth = Auth::Key { path };
            }
            AuthKind::PastedKey => {
                if self.new_pasted_key() {
                    match self.pasted_key_check() {
                        None => {
                            self.error = Some(t("Paste the private key.").into());
                            return false;
                        }
                        Some(KeyCheck::PublicKey) => {
                            self.error = Some(t("This is a public key. Paste the private key instead (the file without .pub).").into());
                            return false;
                        }
                        Some(KeyCheck::Invalid) => {
                            self.error = Some(t("This does not look like a private key.").into());
                            return false;
                        }
                        Some(KeyCheck::Valid(_) | KeyCheck::Encrypted) => {}
                    }
                }
                self.draft.auth = Auth::KeychainKey;
            }
        }
        self.draft.tags = self
            .tags_text
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        self.error = None;
        true
    }

    pub fn show(&mut self, ctx: &egui::Context, store: &BookmarkStore) -> EditorResult {
        let mut result = EditorResult::Open;
        let title = if self.is_new {
            t("New Connection")
        } else {
            t("Edit Connection")
        };
        let modal = egui::Modal::new(egui::Id::new("bookmark-editor")).show(ctx, |ui| {
            ui.set_width(460.0);
            ui.heading(title);
            ui.add_space(6.0);
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.command);
            let max_form = (ui.ctx().content_rect().height() - 190.0).max(240.0);
            egui::ScrollArea::vertical().id_salt("editor-form").max_height(max_form).show(ui, |ui| {
            egui::Grid::new("editor-grid").num_columns(2).spacing([10.0, 8.0]).show(ui, |ui| {
                ui.label(t("Name"));
                let r = ui.add(egui::TextEdit::singleline(&mut self.draft.name).hint_text(t("e.g. Production web")).desired_width(f32::INFINITY));
                #[cfg(debug_assertions)]
                crate::devtest::record("editor-name", r.rect);
                if std::mem::take(&mut self.focus_first) {
                    r.request_focus();
                }
                ui.end_row();

                ui.label(t("Host"));
                ui.horizontal(|ui| {
                    let _host = ui.add(egui::TextEdit::singleline(&mut self.draft.host).hint_text("server.example.com").desired_width(260.0));
                    ui.label(t("Port"));
                    let _port = ui.add(egui::TextEdit::singleline(&mut self.port_text).desired_width(52.0));
                    #[cfg(debug_assertions)]
                    {
                        crate::devtest::record("editor-host", _host.rect);
                        crate::devtest::record("editor-port", _port.rect);
                    }
                });
                ui.end_row();

                ui.label(t("Username"));
                let _user = ui.add(egui::TextEdit::singleline(&mut self.draft.username).hint_text(std::env::var("USER").unwrap_or_default()).desired_width(f32::INFINITY));
                #[cfg(debug_assertions)]
                crate::devtest::record("editor-user", _user.rect);
                ui.end_row();

                ui.label(t("Authentication"));
                ui.horizontal(|ui| {
                    if ui.radio(self.auth == AuthKind::Password, t("Password")).clicked() {
                        self.auth = AuthKind::Password;
                    }
                    let is_key = self.auth != AuthKind::Password;
                    let _key_radio = ui.radio(is_key, t("Private key"));
                    #[cfg(debug_assertions)]
                    crate::devtest::record("editor-auth-key", _key_radio.rect);
                    if _key_radio.clicked() && !is_key {
                        // Pasting is the default; a key file stays one click away.
                        self.auth = AuthKind::PastedKey;
                    }
                });
                ui.end_row();

                match self.auth {
                    AuthKind::Password => {
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
                    AuthKind::PastedKey | AuthKind::KeyFile => {
                        ui.label("");
                        ui.horizontal(|ui| {
                            ui.selectable_value(&mut self.auth, AuthKind::PastedKey, t("Paste key"));
                            ui.selectable_value(&mut self.auth, AuthKind::KeyFile, t("Key file"));
                        });
                        ui.end_row();
                    }
                }
                match self.auth {
                    AuthKind::Password => {}
                    AuthKind::KeyFile => {
                        ui.label(t("Key file"));
                        ui.horizontal(|ui| {
                            let path = &mut self.key_path;
                            ui.add(egui::TextEdit::singleline(path).hint_text("~/.ssh/id_ed25519").desired_width(250.0));
                            if !self.key_files.is_empty() {
                                egui::ComboBox::from_id_salt("key-files").selected_text("").width(24.0).show_ui(ui, |ui| {
                                    for k in &self.key_files {
                                        if ui.selectable_label(path == k, k).clicked() {
                                            *path = k.clone();
                                        }
                                    }
                                });
                            }
                            if ui.button(t("Browse…")).clicked()
                                && let Some(p) = rfd::FileDialog::new().set_directory(paths::home_dir().join(".ssh")).pick_file() {
                                    *path = paths::contract_tilde(&p);
                                }
                        });
                        ui.end_row();
                    }
                    AuthKind::PastedKey => {
                        ui.label(t("Private key"));
                        ui.vertical(|ui| {
                            if self.had_stored_key && !self.replace_stored_key {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(format!("🔑 {}", t("A private key is saved in the Keychain."))));
                                    if ui.button(t("Replace…")).clicked() {
                                        self.replace_stored_key = true;
                                    }
                                });
                            } else {
                                // Long keys (RSA is ~50 lines) scroll inside the box.
                                let _key_box = egui::ScrollArea::vertical()
                                    .id_salt("editor-key-scroll")
                                    .max_height(150.0)
                                    .show(ui, |ui| {
                                        ui.add(
                                            egui::TextEdit::multiline(&mut self.key_text)
                                                .font(egui::TextStyle::Monospace)
                                                .hint_text("-----BEGIN OPENSSH PRIVATE KEY-----\n…\n-----END OPENSSH PRIVATE KEY-----")
                                                .desired_rows(5)
                                                .desired_width(f32::INFINITY),
                                        )
                                    })
                                    .inner;
                                #[cfg(debug_assertions)]
                                crate::devtest::record("editor-key-text", _key_box.rect);
                                ui.horizontal(|ui| {
                                    if ui.button(t("Import from file…")).clicked()
                                        && let Some(p) = rfd::FileDialog::new().set_directory(paths::home_dir().join(".ssh")).pick_file()
                                    {
                                        match std::fs::read(&p) {
                                            Ok(data) if data.len() <= keys::MAX_KEY_TEXT => {
                                                self.key_text = String::from_utf8_lossy(&data).into_owned();
                                            }
                                            Ok(_) => self.error = Some(t("This does not look like a private key.").into()),
                                            Err(e) => self.error = Some(format!("{}: {e}", p.display())),
                                        }
                                    }
                                    if self.had_stored_key && ui.button(t("Keep the saved key")).clicked() {
                                        self.replace_stored_key = false;
                                        self.key_text.clear();
                                    }
                                });
                                let green = egui::Color32::from_rgb(0x3f, 0xb9, 0x50);
                                match self.pasted_key_check() {
                                    Some(KeyCheck::Valid(kind)) => {
                                        ui.colored_label(green, crate::i18n::tf("✓ {} private key", &[&kind]));
                                    }
                                    Some(KeyCheck::Encrypted) => {
                                        ui.colored_label(green, t("✓ Encrypted private key: enter its passphrase below (or when connecting)."));
                                    }
                                    Some(KeyCheck::PublicKey) => {
                                        ui.colored_label(ui.visuals().error_fg_color, t("This is a public key. Paste the private key instead (the file without .pub)."));
                                    }
                                    Some(KeyCheck::Invalid) => {
                                        ui.colored_label(ui.visuals().error_fg_color, t("This does not look like a private key."));
                                    }
                                    None => {}
                                }
                                ui.label(RichText::new(t("Stored in the macOS Keychain, not in the connections file.")).weak().small());
                            }
                        });
                        ui.end_row();
                    }
                }
                if self.auth != AuthKind::Password {
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

                ui.label(t("Remote start folder"));
                ui.add(egui::TextEdit::singleline(&mut self.draft.start_dir).hint_text("/var/www").desired_width(f32::INFINITY))
                    .on_hover_text(t("The terminal and the remote side of SFTP open here."));
                ui.end_row();

                ui.label(t("Local start folder"));
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.draft.local_start_dir)
                            .hint_text(t("Last folder used"))
                            .desired_width(ui.available_width() - 70.0),
                    )
                    .on_hover_text(t("The local side of SFTP opens here."));
                    if ui.button(t("Browse…")).clicked() {
                        let start = paths::expand_tilde(self.draft.local_start_dir.trim());
                        let dialog = rfd::FileDialog::new().set_title(t("Local start folder"));
                        let dialog = if start.is_dir() { dialog.set_directory(start) } else { dialog.set_directory(paths::home_dir()) };
                        if let Some(dir) = dialog.pick_folder() {
                            self.draft.local_start_dir = paths::contract_tilde(&dir);
                        }
                    }
                });
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
            });

            if let Some(e) = &self.error {
                ui.add_space(4.0);
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let save_connect_button = ui.add(egui::Button::new(RichText::new(t("Save & Connect")).strong()));
                #[cfg(debug_assertions)]
                crate::devtest::record("editor-save-connect", save_connect_button.rect);
                let save_connect = save_connect_button.clicked() || enter;
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
                        if self.new_pasted_key() {
                            set.private_key = Some(keys::normalize_key_text(&self.key_text));
                        }
                        // A saved passphrase belongs to the old key: a different key without a
                        // typed passphrase asks for it when connecting.
                        let key_changed = set.private_key.is_some() || self.draft.auth != self.original_auth;
                        if !self.passphrase.is_empty() {
                            set.passphrase = Some(self.passphrase.clone());
                        } else if self.clear_passphrase || key_changed {
                            clear.push(SecretKind::Passphrase);
                        }
                    }
                    result = EditorResult::Save { bookmark: self.draft.clone(), set, clear, connect: save_connect };
                }
            });
        });
        if super::dialogs::escape_only(ctx, &modal) && matches!(result, EditorResult::Open) {
            result = EditorResult::Cancel;
        }
        let _ = DEFAULT_PORT;
        result
    }
}
