//! Connection editor (create and edit). Secrets typed here go to the Keychain on save.

use eframe::egui::{self, RichText};
use uuid::Uuid;

use super::style;
use crate::bookmarks::{Auth, Bookmark, BookmarkStore, Forward, ForwardKind};
use crate::i18n::{t, tf};
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
    /// The keys of an SSH agent.
    Agent,
}

/// A port forward being edited (the numbers as typed).
#[derive(Clone)]
struct ForwardRow {
    kind: ForwardKind,
    listen: String,
    host: String,
    port: String,
}

impl ForwardRow {
    fn from(f: &Forward) -> Self {
        Self {
            kind: f.kind,
            listen: f.listen_port.to_string(),
            host: f.host.clone(),
            port: if f.port == 0 {
                String::new()
            } else {
                f.port.to_string()
            },
        }
    }

    fn to_forward(&self) -> Result<Forward, String> {
        let number = |text: &str| text.trim().parse::<u16>().ok().filter(|p| *p > 0);
        let listen = number(&self.listen).ok_or_else(|| {
            t("Port forwarding: each forward needs a port between 1 and 65535.").to_string()
        })?;
        if self.kind == ForwardKind::Dynamic {
            return Ok(Forward {
                kind: self.kind,
                listen_port: listen,
                host: String::new(),
                port: 0,
            });
        }
        let host = self.host.trim();
        let port = number(&self.port);
        match port {
            Some(port) if !host.is_empty() => Ok(Forward {
                kind: self.kind,
                listen_port: listen,
                host: host.to_string(),
                port,
            }),
            _ => Err(tf(
                "Port forwarding: say where port {} goes (host and port).",
                &[&listen.to_string()],
            )),
        }
    }
}

/// SSH agents found on this Mac: their name and socket.
fn known_agents() -> Vec<(&'static str, String)> {
    [
        (
            "1Password",
            "~/Library/Group Containers/2BUA8C4S2C.com.1password/t/agent.sock",
        ),
        (
            "Secretive",
            "~/Library/Containers/com.maxgoedjen.Secretive.SecretAgent/Data/socket.ssh",
        ),
    ]
    .into_iter()
    .filter(|(_, path)| paths::expand_tilde(path).exists())
    .map(|(name, path)| (name, path.to_string()))
    .collect()
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
    /// Password and passphrase fields show their text (and a saved one, loaded to be read).
    show_password: bool,
    show_passphrase: bool,
    /// The saved pasted key, while it is shown.
    shown_key: Option<String>,
    /// The last checked key text and what it is.
    key_check: Option<(String, KeyCheck)>,
    /// The login method when the editor opened (a saved passphrase belongs to that key).
    original_auth: Auth,
    agent_socket: String,
    agents: Vec<(&'static str, String)>,
    forwards: Vec<ForwardRow>,
    /// Advanced settings start open when one of them is in use.
    advanced_open: bool,
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
        let first_key = key_files.first().cloned().unwrap_or_default();
        let (auth, key_path, agent_socket) = match &bookmark.auth {
            Auth::Password => (AuthKind::Password, first_key, String::new()),
            Auth::Key { path } => (AuthKind::KeyFile, path.clone(), String::new()),
            Auth::KeychainKey => (AuthKind::PastedKey, first_key, String::new()),
            Auth::Agent { socket } => (AuthKind::Agent, first_key, socket.clone()),
        };
        let advanced_open = !bookmark.start_dir.is_empty()
            || !bookmark.local_start_dir.is_empty()
            || !bookmark.startup_command.is_empty()
            || !bookmark.notes.is_empty()
            || bookmark.legacy_algorithms;
        Self {
            original_auth: bookmark.auth.clone(),
            port_text: bookmark.port.to_string(),
            tags_text: bookmark.tags.join(", "),
            forwards: bookmark.forwards.iter().map(ForwardRow::from).collect(),
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
            show_password: false,
            show_passphrase: false,
            shown_key: None,
            key_check: None,
            agent_socket,
            agents: known_agents(),
            advanced_open,
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
        match self.check() {
            Ok(()) => {
                self.error = None;
                true
            }
            Err(e) => {
                self.error = Some(e);
                false
            }
        }
    }

    fn check(&mut self) -> Result<(), String> {
        self.draft.username = self.draft.username.trim().to_string();
        // Accept "user@host:port" (and "[ipv6]:port") typed into the host field.
        let target = crate::bookmarks::model::parse_target(&self.draft.host)?;
        if let Some(user) = target.user {
            self.draft.username = user;
        }
        self.draft.host = target.host;
        if let Some(p) = target.port {
            self.port_text = p.to_string();
        }
        match self.port_text.trim().parse::<u16>() {
            Ok(p) if p > 0 => self.draft.port = p,
            _ => return Err(t("Port must be a number between 1 and 65535.").into()),
        }
        self.draft.auth = match self.auth {
            AuthKind::Password => Auth::Password,
            AuthKind::KeyFile => {
                let path = self.key_path.trim().to_string();
                if path.is_empty() {
                    return Err(t("Choose a private key file.").into());
                }
                if !paths::expand_tilde(&path).is_file() {
                    return Err(t("The private key file does not exist.").into());
                }
                Auth::Key { path }
            }
            AuthKind::PastedKey => {
                if self.new_pasted_key() {
                    match self.pasted_key_check() {
                        None => return Err(t("Paste the private key.").into()),
                        Some(KeyCheck::PublicKey) => return Err(t("This is a public key. Paste the private key instead (the file without .pub).").into()),
                        Some(KeyCheck::Invalid) => {
                            return Err(t("This does not look like a private key.").into());
                        }
                        Some(KeyCheck::Valid(_) | KeyCheck::Encrypted) => {}
                    }
                }
                Auth::KeychainKey
            }
            AuthKind::Agent => Auth::Agent {
                socket: self.agent_socket.trim().to_string(),
            },
        };
        let forwards = self
            .forwards
            .iter()
            .map(ForwardRow::to_forward)
            .collect::<Result<Vec<_>, _>>()?;
        // One port can only be listened on once: here (local, SOCKS) or on the server.
        let mut here = std::collections::HashSet::new();
        let mut there = std::collections::HashSet::new();
        for f in &forwards {
            let fresh = match f.kind {
                ForwardKind::Remote => there.insert(f.listen_port),
                _ => here.insert(f.listen_port),
            };
            if !fresh {
                return Err(tf(
                    "Port forwarding: port {} is used twice.",
                    &[&f.listen_port.to_string()],
                ));
            }
        }
        self.draft.forwards = forwards;
        self.draft.tags = self
            .tags_text
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        Ok(())
    }

    pub fn show(&mut self, ctx: &egui::Context, store: &BookmarkStore) -> EditorResult {
        let mut result = EditorResult::Open;
        let title = if self.is_new {
            t("New Connection")
        } else {
            t("Edit Connection")
        };
        let screen = ctx.content_rect();
        let width = (screen.width() - 80.0).clamp(420.0, 600.0);
        let modal = egui::Modal::new(egui::Id::new("bookmark-editor")).show(ctx, |ui| {
            ui.set_width(width);
            style::form_spacing(ui);
            ui.heading(RichText::new(title).strong());
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.command);
            let max_form = (screen.height() - 200.0).max(240.0);
            egui::ScrollArea::vertical()
                .id_salt("editor-form")
                .max_height(max_form)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    // Room for the scroll bar, so it never covers a field.
                    ui.set_width(ui.available_width() - 10.0);
                    self.connection_section(ui);
                    self.login_section(ui);
                    self.network_section(ui, store);
                    self.organize_section(ui, store);
                    self.advanced_section(ui);
                    ui.add_space(4.0);
                });

            if let Some(e) = &self.error {
                ui.add_space(4.0);
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            ui.add_space(4.0);
            ui.separator();
            style::right_to_left(ui, |ui| {
                let save_connect_button = ui
                    .add(style::primary(t("Save & Connect")))
                    .on_hover_text("⌘↩");
                #[cfg(debug_assertions)]
                crate::devtest::record("editor-save-connect", save_connect_button.rect);
                let save_connect = save_connect_button.clicked() || enter;
                let save = ui.button(t("Save")).clicked();
                if ui.button(t("Cancel")).clicked() {
                    result = EditorResult::Cancel;
                }
                if (save || save_connect) && self.validate() {
                    result = self.saved(save_connect);
                }
            });
        });
        if super::dialogs::escape_only(ctx, &modal) && matches!(result, EditorResult::Open) {
            result = EditorResult::Cancel;
        }
        result
    }

    /// The bookmark and the secrets to store or forget.
    fn saved(&self, connect: bool) -> EditorResult {
        let mut set = SecretSet::default();
        let mut clear = Vec::new();
        if matches!(self.draft.auth, Auth::Password) {
            if !self.password.is_empty() {
                set.password = Some(self.password.clone());
            } else if self.clear_password {
                clear.push(SecretKind::Password);
            }
        } else if self.draft.auth.uses_key() {
            if self.new_pasted_key() {
                set.private_key = Some(keys::normalize_key_text(&self.key_text));
            }
            // A saved passphrase belongs to the old key: a different key without a typed
            // passphrase asks for it when connecting.
            let key_changed = set.private_key.is_some() || self.draft.auth != self.original_auth;
            if !self.passphrase.is_empty() {
                set.passphrase = Some(self.passphrase.clone());
            } else if self.clear_passphrase || key_changed {
                clear.push(SecretKind::Passphrase);
            }
        }
        EditorResult::Save {
            bookmark: self.draft.clone(),
            set,
            clear,
            connect,
        }
    }

    fn connection_section(&mut self, ui: &mut egui::Ui) {
        style::section(ui, t("Connection"));
        style::row(ui, t("Name"), |ui| {
            let r = ui.add(style::input(&mut self.draft.name).hint_text(t("e.g. Production web")));
            #[cfg(debug_assertions)]
            crate::devtest::record("editor-name", r.rect);
            if std::mem::take(&mut self.focus_first) {
                r.request_focus();
            }
        });
        style::row(ui, t("Host"), |ui| {
            // Left to right, so Tab goes from the host to the port.
            ui.horizontal(|ui| {
                const PORT_W: f32 = 64.0;
                let label_w = style::text_width(ui, t("Port"));
                let host_w =
                    ui.available_width() - PORT_W - label_w - 2.0 * ui.spacing().item_spacing.x;
                let _host = ui.add(
                    style::input(&mut self.draft.host)
                        .hint_text("server.example.com")
                        .desired_width(host_w.max(80.0)),
                );
                ui.label(t("Port"));
                let _port = ui.add(style::input(&mut self.port_text).desired_width(PORT_W));
                #[cfg(debug_assertions)]
                {
                    crate::devtest::record("editor-host", _host.rect);
                    crate::devtest::record("editor-port", _port.rect);
                }
            });
        });
        style::row(ui, t("Username"), |ui| {
            let _user = ui.add(
                style::input(&mut self.draft.username)
                    .hint_text(std::env::var("USER").unwrap_or_default()),
            );
            #[cfg(debug_assertions)]
            crate::devtest::record("editor-user", _user.rect);
        });
    }

    fn login_section(&mut self, ui: &mut egui::Ui) {
        style::section(ui, t("Login"));
        style::row(ui, t("Method"), |ui| {
            let key_kind = if self.auth == AuthKind::KeyFile {
                AuthKind::KeyFile
            } else {
                AuthKind::PastedKey
            };
            let mut picked = match self.auth {
                AuthKind::PastedKey | AuthKind::KeyFile => key_kind,
                other => other,
            };
            let _responses = style::segmented(
                ui,
                &mut picked,
                &[
                    (AuthKind::Password, t("Password")),
                    (key_kind, t("Private key")),
                    (AuthKind::Agent, t("SSH agent")),
                ],
            );
            #[cfg(debug_assertions)]
            {
                crate::devtest::record("editor-auth-key", _responses[1].rect);
                crate::devtest::record("editor-auth-agent", _responses[2].rect);
            }
            self.auth = picked;
        });
        match self.auth {
            AuthKind::Password => {
                style::row(ui, t("Password"), |ui| {
                    let saved = self.had_password && !self.clear_password;
                    let hint = if saved {
                        t("Saved in Keychain — type to replace")
                    } else {
                        t("Leave empty to ask when connecting")
                    };
                    secret_input(
                        ui,
                        &mut self.password,
                        &mut self.show_password,
                        hint,
                        saved.then_some((self.draft.id, SecretKind::Password)),
                        &mut self.error,
                        "editor-show-password",
                    );
                    if self.had_password
                        && style::checkbox(
                            ui,
                            &mut self.clear_password,
                            t("Forget the saved password"),
                        )
                        .changed()
                        && self.clear_password
                    {
                        // A shown saved password would otherwise be saved again.
                        self.password.clear();
                        self.show_password = false;
                    }
                });
            }
            AuthKind::PastedKey | AuthKind::KeyFile => {
                style::row(ui, t("Key"), |ui| {
                    style::segmented(
                        ui,
                        &mut self.auth,
                        &[
                            (AuthKind::PastedKey, t("Paste key")),
                            (AuthKind::KeyFile, t("Key file")),
                        ],
                    );
                    if self.auth == AuthKind::KeyFile {
                        self.key_file_ui(ui);
                    } else {
                        self.pasted_key_ui(ui);
                    }
                });
                style::row(ui, t("Passphrase"), |ui| {
                    let saved = self.had_passphrase && !self.clear_passphrase;
                    let hint = if saved {
                        t("Saved in Keychain — type to replace")
                    } else {
                        t("Only if the key is encrypted")
                    };
                    secret_input(
                        ui,
                        &mut self.passphrase,
                        &mut self.show_passphrase,
                        hint,
                        saved.then_some((self.draft.id, SecretKind::Passphrase)),
                        &mut self.error,
                        "editor-show-passphrase",
                    );
                    if self.had_passphrase
                        && style::checkbox(
                            ui,
                            &mut self.clear_passphrase,
                            t("Forget the saved passphrase"),
                        )
                        .changed()
                        && self.clear_passphrase
                    {
                        self.passphrase.clear();
                        self.show_passphrase = false;
                    }
                });
            }
            AuthKind::Agent => {
                style::row(ui, t("Agent"), |ui| self.agent_ui(ui));
            }
        }
    }

    fn key_file_ui(&mut self, ui: &mut egui::Ui) {
        style::right_to_left(ui, |ui| {
            if ui.button(t("Browse…")).clicked()
                && let Some(p) = rfd::FileDialog::new()
                    .set_directory(paths::home_dir().join(".ssh"))
                    .pick_file()
            {
                self.key_path = paths::contract_tilde(&p);
            }
            if !self.key_files.is_empty() {
                egui::ComboBox::from_id_salt("key-files")
                    .selected_text("")
                    .width(28.0)
                    .show_ui(ui, |ui| {
                        for k in &self.key_files {
                            if ui.selectable_label(self.key_path == *k, k).clicked() {
                                self.key_path = k.clone();
                            }
                        }
                    })
                    .response
                    .on_hover_text(t("Keys in ~/.ssh"));
            }
            ui.add(
                style::input(&mut self.key_path)
                    .hint_text("~/.ssh/id_ed25519")
                    .desired_width(ui.available_width()),
            );
        });
    }

    fn pasted_key_ui(&mut self, ui: &mut egui::Ui) {
        if self.had_stored_key && !self.replace_stored_key {
            ui.horizontal(|ui| {
                ui.label(format!(
                    "🔑 {}",
                    t("A private key is saved in the Keychain.")
                ));
                let show = ui.button(if self.shown_key.is_some() {
                    t("Hide")
                } else {
                    t("Show")
                });
                #[cfg(debug_assertions)]
                crate::devtest::record("editor-show-key", show.rect);
                if show.clicked() && self.shown_key.take().is_none() {
                    match secrets::get(self.draft.id, SecretKind::PrivateKey) {
                        Ok(Some(key)) => self.shown_key = Some(key),
                        Ok(None) => {
                            self.error =
                                Some(t("The private key is not in the Keychain any more.").into())
                        }
                        Err(e) => self.error = Some(format!("{e:#}")),
                    }
                }
                if ui.button(t("Replace…")).clicked() {
                    self.replace_stored_key = true;
                    self.shown_key = None;
                }
            });
            // Shown as it is saved, to read or copy; changing it goes through Replace.
            if let Some(key) = &self.shown_key {
                let mut view = key.as_str();
                egui::ScrollArea::vertical()
                    .id_salt("editor-shown-key")
                    .max_height(150.0)
                    .show(ui, |ui| {
                        ui.add(
                            style::input_multiline(&mut view)
                                .font(egui::TextStyle::Monospace)
                                .desired_rows(5),
                        )
                    });
                if ui.button(t("Copy")).clicked() {
                    ui.ctx().copy_text(key.clone());
                }
            }
            return;
        }
        // Long keys (RSA is ~50 lines) scroll inside the box.
        let _key_box = egui::ScrollArea::vertical()
            .id_salt("editor-key-scroll")
            .max_height(150.0)
            .show(ui, |ui| {
                ui.add(
                    style::input_multiline(&mut self.key_text)
                        .font(egui::TextStyle::Monospace)
                        .hint_text(
                            "-----BEGIN OPENSSH PRIVATE KEY-----\n…\n-----END OPENSSH PRIVATE KEY-----",
                        )
                        .desired_rows(5),
                )
            })
            .inner;
        #[cfg(debug_assertions)]
        crate::devtest::record("editor-key-text", _key_box.rect);
        ui.horizontal(|ui| {
            if ui.button(t("Import from file…")).clicked()
                && let Some(p) = rfd::FileDialog::new()
                    .set_directory(paths::home_dir().join(".ssh"))
                    .pick_file()
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
        let error = ui.visuals().error_fg_color;
        match self.pasted_key_check() {
            Some(KeyCheck::Valid(kind)) => {
                ui.colored_label(style::GREEN, tf("✓ {} private key", &[&kind]));
            }
            Some(KeyCheck::Encrypted) => {
                ui.colored_label(
                    style::GREEN,
                    t("✓ Encrypted private key: enter its passphrase below (or when connecting)."),
                );
            }
            Some(KeyCheck::PublicKey) => {
                ui.colored_label(
                    error,
                    t("This is a public key. Paste the private key instead (the file without .pub)."),
                );
            }
            Some(KeyCheck::Invalid) => {
                ui.colored_label(error, t("This does not look like a private key."));
            }
            None => {}
        }
        style::hint(
            ui,
            t("Stored in the macOS Keychain, not in the connections file."),
        );
    }

    fn agent_ui(&mut self, ui: &mut egui::Ui) {
        ui.add(style::input(&mut self.agent_socket).hint_text(t("System agent (SSH_AUTH_SOCK)")));
        if !self.agents.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(t("Use:")).weak());
                for (name, path) in &self.agents {
                    if ui
                        .selectable_label(self.agent_socket == *path, *name)
                        .on_hover_text(path)
                        .clicked()
                    {
                        self.agent_socket = path.clone();
                    }
                }
                if !self.agent_socket.is_empty()
                    && ui.selectable_label(false, t("System agent")).clicked()
                {
                    self.agent_socket.clear();
                }
            });
        }
        let socket = self.agent_socket.trim();
        let (ok, text) = if socket.is_empty() {
            match std::env::var_os("SSH_AUTH_SOCK") {
                Some(_) => (
                    true,
                    t("Uses the macOS SSH agent (keys added with ssh-add).").to_string(),
                ),
                None => (
                    false,
                    t("No system agent is running: choose an agent's socket.").to_string(),
                ),
            }
        } else if paths::expand_tilde(socket).exists() {
            (
                true,
                t("Agent found. It signs for Peler; the keys never leave it.").to_string(),
            )
        } else {
            (false, t("No agent socket at this path.").to_string())
        };
        let color = if ok {
            ui.visuals().weak_text_color()
        } else {
            ui.visuals().error_fg_color
        };
        ui.label(RichText::new(text).small().color(color));
    }

    fn network_section(&mut self, ui: &mut egui::Ui, store: &BookmarkStore) {
        style::section(ui, t("Network"));
        style::row(ui, t("Jump host"), |ui| {
            // Other saved connections that don't already go through this one.
            let own = self.draft.id;
            let mut options: Vec<(String, Uuid)> = store
                .bookmarks
                .iter()
                .filter(|b| b.id != own)
                .filter(|b| {
                    store
                        .jump_chain(b)
                        .is_ok_and(|chain| chain.iter().all(|hop| hop.id != own))
                })
                .map(|b| (format!("{}  —  {}", b.display_name(), b.address()), b.id))
                .collect();
            options.sort();
            let current = self
                .draft
                .jump
                .and_then(|id| options.iter().find(|(_, o)| *o == id))
                .map_or_else(
                    || t("None (connect directly)").to_string(),
                    |(n, _)| n.clone(),
                );
            egui::ComboBox::from_id_salt("jump-host")
                .selected_text(current)
                .width(ui.available_width())
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(self.draft.jump.is_none(), t("None (connect directly)"))
                        .clicked()
                    {
                        self.draft.jump = None;
                    }
                    for (name, id) in options {
                        if ui
                            .selectable_label(self.draft.jump == Some(id), name)
                            .clicked()
                        {
                            self.draft.jump = Some(id);
                        }
                    }
                });
            style::hint(
                ui,
                t("Reach this server through another saved connection (ssh -J)."),
            );
        });
        style::row(ui, t("Port forwarding"), |ui| {
            let mut remove = None;
            for (i, row) in self.forwards.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt(("forward-kind", i))
                        .width(96.0)
                        .selected_text(match row.kind {
                            ForwardKind::Local => t("Local"),
                            ForwardKind::Remote => t("Remote"),
                            ForwardKind::Dynamic => "SOCKS",
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut row.kind, ForwardKind::Local, t("Local (-L)"));
                            ui.selectable_value(
                                &mut row.kind,
                                ForwardKind::Remote,
                                t("Remote (-R)"),
                            );
                            ui.selectable_value(
                                &mut row.kind,
                                ForwardKind::Dynamic,
                                t("SOCKS proxy (-D)"),
                            );
                        });
                    let _listen = ui.add(
                        style::input(&mut row.listen)
                            .hint_text("8080")
                            .desired_width(58.0),
                    );
                    #[cfg(debug_assertions)]
                    crate::devtest::record(format!("forward-listen:{i}"), _listen.rect);
                    if row.kind != ForwardKind::Dynamic {
                        ui.label("→");
                        let _host = ui.add(
                            style::input(&mut row.host)
                                .hint_text("localhost")
                                .desired_width((ui.available_width() - 100.0).max(80.0)),
                        );
                        let _port = ui.add(
                            style::input(&mut row.port)
                                .hint_text("80")
                                .desired_width(52.0),
                        );
                        #[cfg(debug_assertions)]
                        {
                            crate::devtest::record(format!("forward-host:{i}"), _host.rect);
                            crate::devtest::record(format!("forward-port:{i}"), _port.rect);
                        }
                    }
                    style::right_to_left(ui, |ui| {
                        if ui.button("×").on_hover_text(t("Remove")).clicked() {
                            remove = Some(i);
                        }
                    });
                });
                let (listen, host, port) = (row.listen.trim(), row.host.trim(), row.port.trim());
                let listen = if listen.is_empty() { "…" } else { listen };
                let host = if host.is_empty() { "…" } else { host };
                let port = if port.is_empty() { "…" } else { port };
                style::hint(
                    ui,
                    match row.kind {
                        ForwardKind::Local => tf(
                            "localhost:{} on this Mac reaches {}:{} as seen from the server.",
                            &[listen, host, port],
                        ),
                        ForwardKind::Remote => tf(
                            "Port {} on the server reaches {}:{} as seen from this Mac.",
                            &[listen, host, port],
                        ),
                        ForwardKind::Dynamic => tf(
                            "A SOCKS proxy at localhost:{}; its connections leave from the server.",
                            &[listen],
                        ),
                    },
                );
            }
            if let Some(i) = remove {
                self.forwards.remove(i);
            }
            let add = ui.button(format!("+ {}", t("Add Forward")));
            #[cfg(debug_assertions)]
            crate::devtest::record("editor-add-forward", add.rect);
            if add.clicked() {
                self.forwards.push(ForwardRow {
                    kind: ForwardKind::Local,
                    listen: String::new(),
                    host: "localhost".into(),
                    port: String::new(),
                });
            }
        });
    }

    fn organize_section(&mut self, ui: &mut egui::Ui, store: &BookmarkStore) {
        style::section(ui, t("Organize"));
        style::row(ui, t("Group"), |ui| {
            let current = if self.draft.group.is_some() {
                store.group_path(self.draft.group)
            } else {
                t("(none)").to_string()
            };
            egui::ComboBox::from_id_salt("group")
                .selected_text(current)
                .width(ui.available_width())
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(self.draft.group.is_none(), t("(none)"))
                        .clicked()
                    {
                        self.draft.group = None;
                    }
                    let mut groups: Vec<(String, Uuid)> = store
                        .groups
                        .iter()
                        .map(|g| (store.group_path(Some(g.id)), g.id))
                        .collect();
                    groups.sort();
                    for (path, id) in groups {
                        if ui
                            .selectable_label(self.draft.group == Some(id), path)
                            .clicked()
                        {
                            self.draft.group = Some(id);
                        }
                    }
                });
        });
        style::row(ui, t("Tags"), |ui| {
            ui.add(style::input(&mut self.tags_text).hint_text(t("comma separated")));
        });
    }

    fn advanced_section(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        let header = egui::CollapsingHeader::new(RichText::new(t("Advanced")).strong())
            .id_salt("editor-advanced")
            .default_open(self.advanced_open)
            .show(ui, |ui| {
                style::row(ui, t("Remote start folder"), |ui| {
                    ui.add(style::input(&mut self.draft.start_dir).hint_text("/var/www"));
                    style::hint(ui, t("The terminal and the remote side of SFTP open here."));
                });
                style::row(ui, t("Local start folder"), |ui| {
                    style::right_to_left(ui, |ui| {
                        if ui.button(t("Browse…")).clicked() {
                            let start = paths::expand_tilde(self.draft.local_start_dir.trim());
                            let dialog = rfd::FileDialog::new().set_title(t("Local start folder"));
                            let dialog = if start.is_dir() {
                                dialog.set_directory(start)
                            } else {
                                dialog.set_directory(paths::home_dir())
                            };
                            if let Some(dir) = dialog.pick_folder() {
                                self.draft.local_start_dir = paths::contract_tilde(&dir);
                            }
                        }
                        ui.add(
                            style::input(&mut self.draft.local_start_dir)
                                .hint_text(t("Last folder used"))
                                .desired_width(ui.available_width()),
                        );
                    });
                    style::hint(ui, t("The local side of SFTP opens here."));
                });
                style::row(ui, t("Run on connect"), |ui| {
                    ui.add(
                        style::input_multiline(&mut self.draft.startup_command)
                            .hint_text(t("One command per line"))
                            .desired_rows(2)
                            .font(egui::TextStyle::Monospace),
                    );
                });
                style::row(ui, t("Notes"), |ui| {
                    ui.add(style::input_multiline(&mut self.draft.notes).desired_rows(2));
                });
                style::row(ui, "", |ui| {
                    style::checkbox(ui,
                        &mut self.draft.legacy_algorithms,
                        t("Allow legacy algorithms (old servers)"),
                    )
                    .on_hover_text(t("Also offer ssh-rsa/SHA-1 key exchange and CBC ciphers. Only enable this for servers that need it."));
                });
            });
        let _ = header;
    }
}

/// A password field with a Show/Hide button after it. Showing a value that is saved (`saved`)
/// and not typed over loads it from the Keychain into the field, to be read or copied; saving it
/// unchanged stores the same value.
fn secret_input(
    ui: &mut egui::Ui,
    text: &mut String,
    shown: &mut bool,
    hint: &str,
    saved: Option<(Uuid, SecretKind)>,
    error: &mut Option<String>,
    _record: &str,
) -> egui::Response {
    // Left to right with the button after the field, so Tab goes from the field to it.
    ui.horizontal(|ui| {
        let label = if *shown { t("Hide") } else { t("Show") };
        let button_w = style::text_width(ui, label) + 2.0 * ui.spacing().button_padding.x;
        let field_w = ui.available_width() - button_w - ui.spacing().item_spacing.x;
        let field = ui.add(
            style::input(text)
                .password(!*shown)
                .hint_text(hint)
                .desired_width(field_w.max(80.0)),
        );
        let button = ui.button(label);
        #[cfg(debug_assertions)]
        crate::devtest::record(_record, button.rect);
        if button.clicked() {
            *shown = !*shown;
            if *shown
                && text.is_empty()
                && let Some((id, kind)) = saved
            {
                match secrets::get(id, kind) {
                    Ok(Some(value)) => *text = value,
                    Ok(None) => {}
                    Err(e) => *error = Some(format!("{e:#}")),
                }
            }
        }
        field
    })
    .inner
}
