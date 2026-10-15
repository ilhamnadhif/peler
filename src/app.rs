//! The application: window layout, tabs, sidebar, dialogs and the action dispatcher.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Color32, CornerRadius, Key, KeyboardShortcut, Modifiers, RichText, Sense, Stroke, Theme,
    Ui, vec2,
};
use muda::MenuEvent;
use uuid::Uuid;

use crate::bookmarks::transfer::{self, ExportScope, ImportPlan};
use crate::bookmarks::{Bookmark, BookmarkStore, ssh_config};
use crate::i18n::{self, Language, t, tf};
use crate::paths;
use crate::secrets::{self, SecretKind};
use crate::settings::{FontChoice, Settings, ThemeMode};
use crate::ssh::types::{ConnState, HostKeyStatus, Prompt, SecretAnswer, SecretPromptKind};
use crate::term::core::GridSize;
use crate::term::scheme::{self, Scheme};
use crate::ui::actions::Action;
use crate::ui::dialogs::{
    self, ConfirmAction, Dialog, DialogResult, ExportDialog, GroupTarget, ImportDialog, Palette,
    PaletteResult, SettingsDialog, Toasts,
};
use crate::ui::editor::{Editor, EditorResult};
use crate::ui::layout::{self, SplitDir};
use crate::ui::menu::{self, AppMenu};
use crate::ui::sftp_panel::{PanelOutput, SftpPanel};
use crate::ui::sidebar::{Live, Sidebar, TreeMove};
use crate::ui::tabs::{Env, Tab};
use crate::ui::{fonts, style, terminal};

/// Settings that need work when they change.
#[derive(Clone, PartialEq)]
struct Applied {
    language: Language,
    theme: ThemeMode,
    font: FontChoice,
    cjk: bool,
    option_as_meta: bool,
    terminal: (usize, crate::settings::CursorStyle, bool, String, String),
}

impl Applied {
    fn of(s: &Settings) -> Self {
        Self {
            language: s.language,
            theme: s.theme,
            font: fonts::effective_choice(&s.font),
            cjk: s.load_cjk_font,
            option_as_meta: s.option_as_meta,
            terminal: (
                s.scrollback,
                s.cursor_style,
                s.cursor_blink,
                s.scheme_dark.clone(),
                s.scheme_light.clone(),
            ),
        }
    }
}

/// Snapshot of a pending prompt, copied out of the connection lock for drawing.
enum PromptView {
    HostKey {
        host: String,
        port: u16,
        algorithm: String,
        fingerprint: String,
        changed: bool,
    },
    Secret {
        kind: SecretPromptKind,
        message: String,
        echo: bool,
        can_save: bool,
        retry: bool,
    },
}

pub struct PelerApp {
    rt: tokio::runtime::Runtime,
    store: BookmarkStore,
    settings: Settings,
    settings_saved: Settings,
    settings_changed_at: Option<Instant>,
    applied: Applied,
    tabs: Vec<Tab>,
    active: usize,
    next_id: u64,
    sidebar: Sidebar,
    editor: Option<Editor>,
    palette: Option<Palette>,
    dialog: Option<Dialog>,
    toasts: Toasts,
    menu: Option<AppMenu>,
    menu_rx: Option<mpsc::Receiver<MenuEvent>>,
    actions: Vec<Action>,
    cell: (FontChoice, f32, egui::Vec2),
    allow_close: bool,
    title: String,
    first_frame: bool,
    startup: Vec<String>,
    /// (tab id, pane id) that last received keyboard focus.
    focus_target: Option<(u64, crate::ui::layout::PaneId)>,
    #[cfg(debug_assertions)]
    dev: crate::devtest::Script,
}


static PROCESS_START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

/// Records the launch time (for `PELER_STARTUP_LOG=1`).
pub fn mark_process_start() {
    PROCESS_START.get_or_init(Instant::now);
}

/// A bookmark for `user@host[:port]` typed on the command line (not saved).
fn adhoc_bookmark(target: &str) -> Option<Bookmark> {
    let target = target.trim().trim_start_matches("ssh://");
    let (user, rest) = match target.split_once('@') {
        Some((u, r)) => (u.to_string(), r),
        None => (String::new(), target),
    };
    let (host, port) = match rest.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => (h.to_string(), p.parse().ok()?),
        _ => (rest.to_string(), crate::bookmarks::DEFAULT_PORT),
    };
    if host.is_empty() || host.contains(char::is_whitespace) {
        return None;
    }
    Some(Bookmark {
        host,
        port,
        username: user,
        ..Default::default()
    })
}

const TAB_H: f32 = 34.0;

impl PelerApp {
    /// `targets` are command-line arguments: bookmark names or `user@host[:port]`.
    pub fn new(cc: &eframe::CreationContext<'_>, targets: Vec<String>) -> Self {
        let ctx = &cc.egui_ctx;
        let mut toasts = Toasts::default();
        let settings = Settings::load(&paths::settings_file()).unwrap_or_else(|e| {
            toasts.error(format!("{e:#}"));
            Settings::default()
        });
        i18n::set_language(settings.language);
        let store = match BookmarkStore::load(&paths::bookmarks_file()) {
            Ok(s) => s,
            Err(e) => {
                // Keep the unreadable file instead of overwriting it with an empty store.
                let path = paths::bookmarks_file();
                let backup = path.with_extension(format!(
                    "corrupt-{}.json",
                    chrono::Utc::now().format("%Y%m%d%H%M%S")
                ));
                let _ = std::fs::rename(&path, &backup);
                toasts.error(tf(
                    "Could not read the bookmarks file ({}). It was kept as {}.",
                    &[&format!("{e:#}"), &backup.display().to_string()],
                ));
                BookmarkStore::default()
            }
        };
        fonts::apply(
            ctx,
            &fonts::effective_choice(&settings.font),
            settings.load_cjk_font,
        );
        style::apply(ctx, settings.theme);
        ctx.options_mut(|o| {
            o.zoom_with_keyboard = false;
            o.quit_shortcuts.clear();
        });
        let menu = AppMenu::install();
        let menu_rx = menu::event_channel(ctx);
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("peler-io")
            .enable_all()
            .build()
            .expect("tokio runtime");
        Self {
            rt,
            store,
            applied: Applied::of(&settings),
            settings_saved: settings.clone(),
            settings,
            settings_changed_at: None,
            tabs: Vec::new(),
            active: 0,
            next_id: 1,
            sidebar: Sidebar::default(),
            editor: None,
            palette: None,
            dialog: None,
            toasts,
            menu: Some(menu),
            menu_rx: Some(menu_rx),
            actions: Vec::new(),
            cell: (FontChoice::default(), 0.0, vec2(8.0, 16.0)),
            allow_close: false,
            title: String::new(),
            first_frame: true,
            startup: targets,
            focus_target: None,
            #[cfg(debug_assertions)]
            dev: crate::devtest::Script::from_env(),
        }
    }

    /// Debug builds only: one step of `PELER_DEV_SCRIPT` (see `crate::devtest`).
    #[cfg(debug_assertions)]
    fn run_dev_script(&mut self, ctx: &egui::Context) {
        use crate::devtest;
        let Some(step) = self.dev.next(ctx) else {
            return;
        };
        let (cmd, arg) = step.split_once(' ').unwrap_or((step.as_str(), ""));
        let target = |name: &str| -> Option<egui::Pos2> {
            match name.strip_prefix('@') {
                Some(n) => devtest::rect_of(n).map(|r| r.center()),
                None => {
                    let (x, y) = name.split_once(',')?;
                    Some(egui::pos2(x.trim().parse().ok()?, y.trim().parse().ok()?))
                }
            }
        };
        let screen = |app: &Self| -> String {
            let Some(p) = app.tabs.get(app.active).and_then(|t| t.focused_pane()) else {
                return String::new();
            };
            use alacritty_terminal::grid::Dimensions;
            let term = p.shared.term.lock();
            term.bounds_to_string(
                alacritty_terminal::index::Point::new(term.topmost_line(), alacritty_terminal::index::Column(0)),
                alacritty_terminal::index::Point::new(term.bottommost_line(), term.last_column()),
            )
        };
        match cmd {
            // Input sent straight to the remote (bypasses the keyboard path).
            "send" => {
                if let Some(p) = self.tabs.get(self.active).and_then(|t| t.focused_pane()) {
                    p.send(devtest::unescape(arg).into_bytes());
                }
            }
            // Real keyboard and mouse events.
            "keys" => devtest::type_text(&devtest::unescape(arg)),
            "key" => devtest::key(arg),
            "click" | "dclick" | "rclick" => match target(arg) {
                Some(pos) => devtest::click(pos, if cmd == "dclick" { 2 } else { 1 }, cmd == "rclick"),
                None => self.dev.check(false, &format!("{cmd} {arg}: target not found")),
            },
            "connect" => {
                if let Some(b) = self.store.bookmarks.iter().find(|b| b.display_name() == arg).cloned() {
                    self.open_tab(ctx, b);
                }
            }
            "search" => self.sidebar.search = arg.to_string(),
            "import" => self.import_file(PathBuf::from(arg)),
            "action" => {
                let first = self.store.bookmarks.first().map(|b| b.id);
                let action = match arg {
                    "quick" => Some(Action::QuickConnect),
                    "settings" => Some(Action::Settings),
                    "sftp" => Some(Action::ToggleSftp),
                    "split-right" => Some(Action::SplitRight),
                    "split-down" => Some(Action::SplitDown),
                    "new-bookmark" => Some(Action::NewBookmark { group: None }),
                    "edit" => first.map(Action::EditBookmark),
                    "export" => Some(Action::Export(ExportScope::All)),
                    "import-ssh" => Some(Action::ImportSshConfig),
                    "shortcuts" => Some(Action::Shortcuts),
                    "next-tab" => Some(Action::NextTab),
                    "reconnect" => Some(Action::Reconnect),
                    "bigger" => Some(Action::FontBigger),
                    "sidebar" => Some(Action::ToggleSidebar),
                    "close" => Some(Action::CloseTabOrPane),
                    "dark" => {
                        self.settings.theme = ThemeMode::Dark;
                        None
                    }
                    "light" => {
                        self.settings.theme = ThemeMode::Light;
                        None
                    }
                    "indonesian" => {
                        self.settings.language = Language::Indonesian;
                        None
                    }
                    "english" => {
                        self.settings.language = Language::English;
                        None
                    }
                    _ => None,
                };
                self.actions.extend(action);
            }
            // Checks.
            "expect-screen" => {
                let text = screen(self);
                self.dev.check(text.contains(arg), &format!("screen contains {arg:?}"));
            }
            "expect-not-screen" => {
                let text = screen(self);
                self.dev.check(!text.contains(arg), &format!("screen does not contain {arg:?}"));
            }
            "expect-tabs" => {
                let n = self.tabs.len();
                self.dev.check(arg.parse() == Ok(n), &format!("tabs = {arg} (got {n})"));
            }
            "expect-panes" => {
                let n = self.tabs.get(self.active).map_or(0, |t| t.panes.len());
                self.dev.check(arg.parse() == Ok(n), &format!("panes = {arg} (got {n})"));
            }
            "expect-keyboard-on-pane" => {
                let ok = self.tabs.get(self.active).is_some_and(|t| {
                    let idx: usize = arg.parse().unwrap_or(usize::MAX);
                    let panes = t.layout.panes();
                    panes.get(idx).is_some_and(|id| *id == t.focused)
                        && ctx.memory(|m| m.has_focus(terminal::pane_widget_id(t.focused)))
                });
                self.dev.check(ok, &format!("keyboard focus on pane {arg}"));
            }
            "expect-remote-cwd" => {
                let cwd = self
                    .tabs
                    .get(self.active)
                    .and_then(|t| t.sftp.as_ref())
                    .map(|p| p.remote_cwd().to_string())
                    .unwrap_or_default();
                self.dev.check(cwd == arg, &format!("remote cwd = {arg} (got {cwd})"));
            }
            "expect-local-cwd" => {
                let cwd = self
                    .tabs
                    .get(self.active)
                    .and_then(|t| t.sftp.as_ref())
                    .map(|p| p.local_cwd().to_string())
                    .unwrap_or_default();
                self.dev.check(cwd == arg, &format!("local cwd = {arg} (got {cwd})"));
            }
            "expect-modal" => {
                let open = self.modal_open();
                self.dev.check(open == (arg == "yes"), &format!("modal open = {arg} (got {open})"));
            }
            "expect-bookmark" => {
                let found = self.store.bookmarks.iter().any(|b| b.display_name() == arg);
                self.dev.check(found, &format!("bookmark {arg:?} exists"));
            }
            "expect-theme-dark" => {
                let dark = ctx.theme() == Theme::Dark;
                self.dev.check(dark == (arg == "yes"), &format!("dark theme = {arg} (got {dark})"));
            }
            "log" => eprintln!("DEVTEST LOG: {arg}"),
            "debug-hits" => ctx.all_styles_mut(|s| {
                s.debug.show_widget_hits = true;
                s.debug.show_interactive_widgets = true;
            }),
            "move" => {
                if let Some(pos) = target(arg) {
                    devtest::click(pos, 0, false);
                }
            }
            "quit" => {
                eprintln!("DEVTEST DONE pass={} fail={}", self.dev.passed, self.dev.failed);
                self.allow_close = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            other => self.dev.check(false, &format!("unknown step {other:?}")),
        }
    }

    /// Opens the tabs requested on the command line.
    fn open_startup_targets(&mut self, ctx: &egui::Context) {
        for target in std::mem::take(&mut self.startup) {
            let found = self
                .store
                .bookmarks
                .iter()
                .find(|b| b.display_name().eq_ignore_ascii_case(&target))
                .cloned();
            match found.or_else(|| adhoc_bookmark(&target)) {
                Some(b) => self.open_tab(ctx, b),
                None => self
                    .toasts
                    .error(tf("Unknown bookmark or address: {}", &[&target])),
            }
        }
    }

    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn scheme(&self, ctx: &egui::Context) -> &'static Scheme {
        let dark = ctx.theme() == Theme::Dark;
        scheme::find(
            if dark {
                &self.settings.scheme_dark
            } else {
                &self.settings.scheme_light
            },
            dark,
        )
    }

    fn save_store(&mut self) {
        if let Err(e) = self.store.save(&paths::bookmarks_file()) {
            self.toasts.error(format!("{e:#}"));
        }
    }

    /// Whether a dialog or prompt has the keyboard (the terminal and shortcuts must not).
    fn modal_open(&self) -> bool {
        self.editor.is_some()
            || self.palette.is_some()
            || self.dialog.is_some()
            || self.tabs.get(self.active).is_some_and(|t| {
                t.sftp.as_ref().is_some_and(SftpPanel::dialog_open)
                    || t.conn.shared.lock().prompt.is_some()
            })
    }

    fn live_map(&self) -> HashMap<Uuid, Live> {
        let mut map = HashMap::new();
        for tab in &self.tabs {
            let live = match tab.state() {
                ConnState::Connected => Live::Connected,
                ConnState::Connecting | ConnState::Authenticating => Live::Connecting,
                ConnState::Closed(Some(_)) => Live::Failed,
                ConnState::Closed(None) => continue,
            };
            let entry = map.entry(tab.bookmark.id).or_insert(live);
            if live == Live::Connected {
                *entry = Live::Connected;
            }
        }
        map
    }

    /// Measured every frame: a font change only takes effect on the frame after `set_fonts`,
    /// so a cached measurement could keep the old font's metrics.
    fn cell_size(&mut self, ctx: &egui::Context) -> egui::Vec2 {
        let c = terminal::cell_size(ctx, self.settings.font_size);
        if c.x > 0.0 && c.y > 0.0 {
            self.cell = (
                fonts::effective_choice(&self.settings.font),
                self.settings.font_size,
                c,
            );
        }
        self.cell.2
    }

    // ----- input routing ----------------------------------------------------------------------

    fn handle_menu_events(&mut self) {
        let Some(rx) = &self.menu_rx else { return };
        while let Ok(ev) = rx.try_recv() {
            if let Some(action) = self.menu.as_ref().and_then(|m| m.action(ev.id())) {
                self.actions.push(action);
            }
        }
    }

    /// Shortcuts not covered by the native menu.
    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        if self.modal_open() {
            return;
        }
        let digits = [
            Key::Num1,
            Key::Num2,
            Key::Num3,
            Key::Num4,
            Key::Num5,
            Key::Num6,
            Key::Num7,
            Key::Num8,
            Key::Num9,
        ];
        for (i, key) in digits.iter().enumerate() {
            if ctx.input_mut(|inp| {
                inp.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, *key))
            }) {
                let idx = if i == 8 {
                    self.tabs.len().saturating_sub(1)
                } else {
                    i
                };
                self.actions.push(Action::SelectTab(idx));
            }
        }
    }

    fn handle_close_request(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested()) && !self.allow_close {
            let live = self.tabs.iter().filter(|t| t.is_active()).count();
            if live > 0 && self.settings.confirm_close_connected {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.dialog = Some(dialogs::confirm(
                    t("Quit Peler?"),
                    tf(
                        "{} connection(s) are open and will be closed.",
                        &[&live.to_string()],
                    ),
                    t("Quit"),
                    ConfirmAction::Quit,
                ));
            }
        }
    }

    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        if dropped.is_empty() {
            return;
        }
        let rt = self.rt.handle().clone();
        let Some(tab) = self
            .tabs
            .get_mut(self.active)
            .filter(|t| t.state().is_connected())
        else {
            // Dropping an export file with no session open imports it.
            if dropped.len() == 1 && dropped[0].extension().is_some_and(|e| e == "json") {
                self.import_file(dropped[0].clone());
            } else {
                self.toasts.error(t("Connect to a server to upload files."));
            }
            return;
        };
        if tab.sftp.is_none() {
            tab.sftp = Some(tab.new_sftp_panel(self.settings.last_local_dir.as_deref()));
        }
        tab.show_sftp = true;
        if let Some(panel) = &mut tab.sftp {
            panel.upload_paths(&rt, ctx, dropped);
        }
    }

    fn apply_settings_changes(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        self.settings.clamp();
        let now = Applied::of(&self.settings);
        if now != self.applied || self.first_frame {
            if now.language != self.applied.language {
                i18n::set_language(now.language);
                self.menu = Some(AppMenu::install());
            }
            if now.theme != self.applied.theme {
                style::apply(ctx, now.theme);
            }
            if now.font != self.applied.font || now.cjk != self.applied.cjk {
                fonts::apply(ctx, &now.font, now.cjk);
            }
            if now.option_as_meta != self.applied.option_as_meta || self.first_frame {
                use winit::platform::macos::{OptionAsAlt, WindowExtMacOS};
                if let Some(w) = frame.winit_window() {
                    w.set_option_as_alt(if now.option_as_meta {
                        OptionAsAlt::Both
                    } else {
                        OptionAsAlt::None
                    });
                }
            }
            if now.terminal != self.applied.terminal {
                let scheme = self.scheme(ctx);
                for tab in &self.tabs {
                    tab.apply_settings(&self.settings, scheme);
                }
            }
            self.applied = now;
        }
        // Terminal colours follow the effective (system) theme, which can change at any time.
        let scheme = self.scheme(ctx);
        for tab in &self.tabs {
            if tab
                .panes
                .first()
                .is_some_and(|p| p.shared.palette.lock().bg != scheme.background)
            {
                tab.apply_settings(&self.settings, scheme);
            }
        }
        if self.settings != self.settings_saved {
            let at = *self.settings_changed_at.get_or_insert_with(Instant::now);
            if at.elapsed() > Duration::from_millis(600) {
                if let Err(e) = self.settings.save(&paths::settings_file()) {
                    self.toasts.error(format!("{e:#}"));
                }
                self.settings_saved = self.settings.clone();
                self.settings_changed_at = None;
            } else {
                ctx.request_repaint_after(Duration::from_millis(650));
            }
        }
    }

    // ----- tabs -------------------------------------------------------------------------------

    fn env<'a>(&'a self, ctx: &'a egui::Context) -> Env<'a> {
        Env {
            rt: self.rt.handle(),
            ctx,
            settings: &self.settings,
            scheme: self.scheme(ctx),
            known_hosts: paths::known_hosts_file(),
        }
    }

    fn open_tab(&mut self, ctx: &egui::Context, bookmark: Bookmark) {
        let size = self
            .tabs
            .get(self.active)
            .and_then(|t| t.focused_pane())
            .and_then(|p| p.view.grid)
            .unwrap_or(GridSize {
                cols: 120,
                rows: 34,
            });
        let (tab_id, pane_id) = (self.next_id(), self.next_id());
        let tab = Tab::open(tab_id, pane_id, bookmark, size, &self.env(ctx));
        self.tabs.push(tab);
        self.active = self.tabs.len() - 1;
    }

    fn close_tab(&mut self, idx: usize) {
        if idx < self.tabs.len() {
            let tab = self.tabs.remove(idx);
            tab.disconnect();
            if self.active >= self.tabs.len() {
                self.active = self.tabs.len().saturating_sub(1);
            } else if idx < self.active {
                self.active -= 1;
            }
        }
    }

    fn tab_index(&self, id: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    fn request_close_tab(&mut self, idx: usize) {
        let Some(tab) = self.tabs.get(idx) else {
            return;
        };
        if self.settings.confirm_close_connected && tab.is_active() {
            self.dialog = Some(dialogs::confirm(
                t("Close this tab?"),
                tf(
                    "The connection to {} will be closed.",
                    &[&tab.bookmark.address()],
                ),
                t("Close"),
                ConfirmAction::CloseTab(tab.id),
            ));
        } else {
            self.close_tab(idx);
        }
    }

    // ----- actions ----------------------------------------------------------------------------

    fn process_actions(&mut self, ctx: &egui::Context) {
        let actions = std::mem::take(&mut self.actions);
        for action in actions {
            // Native menu shortcuts arrive even while a dialog is open: only editing commands
            // and Quit pass through, and ⌘W closes the dialog instead of a tab.
            if self.modal_open() {
                match action {
                    Action::Copy | Action::Paste | Action::Cut | Action::Quit => {}
                    Action::CloseTabOrPane => {
                        if self.palette.is_some() {
                            self.palette = None;
                        } else if self.dialog.is_some() {
                            self.dialog = None;
                        } else if self.editor.is_some() {
                            self.editor = None;
                        } else if let Some(panel) =
                            self.tabs.get_mut(self.active).and_then(|t| t.sftp.as_mut())
                        {
                            panel.close_dialog();
                        }
                        continue;
                    }
                    _ => continue,
                }
            }
            self.run_action(ctx, action);
        }
    }

    fn run_action(&mut self, ctx: &egui::Context, action: Action) {
        match action {
            Action::NewBookmark { group } => {
                self.editor = Some(Editor::new(
                    Bookmark {
                        group,
                        ..Default::default()
                    },
                    true,
                ));
            }
            Action::EditBookmark(id) => {
                if let Some(b) = self.store.bookmark(id).cloned() {
                    self.editor = Some(Editor::new(b, false));
                } else if let Some(tab) = self.tabs.iter().find(|t| t.bookmark.id == id) {
                    // An ad-hoc connection (command line): offer to save it as a bookmark.
                    self.editor = Some(Editor::new(tab.bookmark.clone(), true));
                }
            }
            Action::DuplicateBookmark(id) => {
                if let Some(new_id) = self.store.duplicate_bookmark(id, t("(copy)")) {
                    secrets::copy_all(id, new_id);
                    self.save_store();
                    self.sidebar.reveal(&self.store, new_id);
                }
            }
            Action::DeleteBookmark(id) => {
                if let Some(b) = self.store.bookmark(id) {
                    self.dialog = Some(dialogs::confirm(
                        t("Delete bookmark?"),
                        tf(
                            "\"{}\" and its saved password will be removed.",
                            &[&b.display_name()],
                        ),
                        t("Delete"),
                        ConfirmAction::DeleteBookmark(id),
                    ));
                }
            }
            Action::Connect(id) => {
                if let Some(b) = self.store.bookmark(id).cloned() {
                    self.sidebar.selected = Some(id);
                    self.open_tab(ctx, b);
                }
            }
            Action::NewGroup { parent } => {
                self.dialog = Some(Dialog::GroupName {
                    target: GroupTarget::New { parent },
                    name: String::new(),
                    focus: true,
                });
            }
            Action::RenameGroup(id) => {
                let name = self
                    .store
                    .group(id)
                    .map(|g| g.name.clone())
                    .unwrap_or_default();
                self.dialog = Some(Dialog::GroupName {
                    target: GroupTarget::Rename(id),
                    name,
                    focus: true,
                });
            }
            Action::DeleteGroup(id) => {
                if let Some(g) = self.store.group(id) {
                    self.dialog = Some(dialogs::confirm(
                        t("Delete group?"),
                        tf(
                            "The group \"{}\" will be removed. Its bookmarks and sub-groups move up one level.",
                            &[&g.name],
                        ),
                        t("Delete"),
                        ConfirmAction::DeleteGroup(id),
                    ));
                }
            }
            Action::QuickConnect => self.palette = Some(Palette::default()),
            Action::NewTab => match self.tabs.get(self.active) {
                Some(tab) => {
                    let b = self
                        .store
                        .bookmark(tab.bookmark.id)
                        .cloned()
                        .unwrap_or_else(|| tab.bookmark.clone());
                    self.open_tab(ctx, b);
                }
                None => self.palette = Some(Palette::default()),
            },
            Action::DuplicateTab(id) => {
                if let Some(tab) = self.tabs.iter().find(|t| t.id == id) {
                    let b = self
                        .store
                        .bookmark(tab.bookmark.id)
                        .cloned()
                        .unwrap_or_else(|| tab.bookmark.clone());
                    self.open_tab(ctx, b);
                }
            }
            Action::CloseTabOrPane => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    if tab.panes.len() > 1 {
                        let focused = tab.focused;
                        tab.close_pane(focused);
                    } else {
                        self.request_close_tab(self.active);
                    }
                }
            }
            Action::CloseTab(id) => {
                if let Some(i) = self.tab_index(id) {
                    self.request_close_tab(i);
                }
            }
            Action::SelectTab(i) => {
                if i < self.tabs.len() {
                    self.active = i;
                }
            }
            Action::NextTab => {
                if !self.tabs.is_empty() {
                    self.active = (self.active + 1) % self.tabs.len();
                }
            }
            Action::PrevTab => {
                if !self.tabs.is_empty() {
                    self.active = (self.active + self.tabs.len() - 1) % self.tabs.len();
                }
            }
            Action::MoveTab { from, to } => {
                if let (Some(from), Some(to)) = (self.tab_index(from), self.tab_index(to))
                    && from != to
                {
                    let tab = self.tabs.remove(from);
                    self.tabs.insert(to, tab);
                    self.active = to;
                }
            }
            Action::Reconnect => {
                if self.active < self.tabs.len() {
                    let id = self.tabs[self.active].bookmark.id;
                    if let Some(b) = self.store.bookmark(id).cloned() {
                        self.tabs[self.active].bookmark = b;
                    }
                    let env = Env {
                        rt: self.rt.handle(),
                        ctx,
                        settings: &self.settings,
                        scheme: self.scheme(ctx),
                        known_hosts: paths::known_hosts_file(),
                    };
                    self.tabs[self.active].reconnect(&env);
                }
            }
            Action::SplitRight | Action::SplitDown => {
                if self.active < self.tabs.len() {
                    let dir = if action == Action::SplitRight {
                        SplitDir::Horizontal
                    } else {
                        SplitDir::Vertical
                    };
                    let pane_id = self.next_id();
                    let env = Env {
                        rt: self.rt.handle(),
                        ctx,
                        settings: &self.settings,
                        scheme: self.scheme(ctx),
                        known_hosts: paths::known_hosts_file(),
                    };
                    self.tabs[self.active].split(pane_id, dir, &env);
                }
            }
            Action::NextPane | Action::PrevPane => {
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.focused = tab.layout.neighbor(tab.focused, action == Action::NextPane);
                    if let Some(id) = tab.focused_pane().and_then(|p| p.view.id) {
                        ctx.memory_mut(|m| m.request_focus(id));
                    }
                }
            }
            Action::ToggleSidebar => self.settings.sidebar_visible = !self.settings.sidebar_visible,
            Action::ToggleSftp => {
                let last = self.settings.last_local_dir.clone();
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.show_sftp = !tab.show_sftp;
                    if tab.show_sftp && tab.sftp.is_none() && tab.state().is_connected() {
                        tab.sftp = Some(tab.new_sftp_panel(last.as_deref()));
                    }
                }
            }
            Action::FontBigger => {
                self.settings.font_size = (self.settings.font_size + 1.0).min(Settings::MAX_FONT)
            }
            Action::FontSmaller => {
                self.settings.font_size = (self.settings.font_size - 1.0).max(Settings::MIN_FONT)
            }
            Action::FontReset => self.settings.font_size = Settings::default().font_size,
            Action::Copy => ctx.send_viewport_cmd(egui::ViewportCommand::RequestCopy),
            Action::Paste => ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaste),
            Action::Cut => ctx.send_viewport_cmd(egui::ViewportCommand::RequestCut),
            Action::SelectAll => {
                if let Some(p) = self.tabs.get(self.active).and_then(|t| t.focused_pane()) {
                    terminal::select_all(&p.shared);
                }
            }
            Action::ClearScrollback => {
                if let Some(p) = self.tabs.get(self.active).and_then(|t| t.focused_pane()) {
                    terminal::clear_scrollback(&p.shared);
                }
            }
            Action::SendQuickCommand(id) => {
                let Some(qc) = self
                    .settings
                    .quick_commands
                    .iter()
                    .find(|q| q.id == id)
                    .cloned()
                else {
                    return;
                };
                if let Some(p) = self.tabs.get(self.active).and_then(|t| t.focused_pane()) {
                    let mut bytes = qc.command.replace('\n', "\r").into_bytes();
                    if qc.run {
                        bytes.push(b'\r');
                    }
                    p.send(bytes);
                    if let Some(id) = p.view.id {
                        ctx.memory_mut(|m| m.request_focus(id));
                    }
                }
            }
            Action::Import => {
                if let Some(path) = rfd::FileDialog::new()
                    .set_title(t("Import Bookmarks"))
                    .add_filter(t("Bookmark exports"), &["json"])
                    .add_filter(t("All files"), &["*"])
                    .pick_file()
                {
                    self.import_file(path);
                }
            }
            Action::ImportSshConfig => {
                let path = paths::home_dir().join(".ssh/config");
                match ssh_config::parse_file(&path) {
                    Ok(hosts) if hosts.is_empty() => {
                        self.toasts.info(t("No hosts found in ~/.ssh/config."))
                    }
                    Ok(hosts) => {
                        let plan = ImportPlan::new(transfer::from_ssh_config(&hosts), &self.store);
                        self.dialog = Some(Dialog::Import(ImportDialog::new(
                            plan,
                            "~/.ssh/config".into(),
                        )));
                    }
                    Err(e) => self.toasts.error(format!("~/.ssh/config: {e}")),
                }
            }
            Action::Export(scope) => {
                if self.store.bookmarks.is_empty() {
                    self.toasts.info(t("There are no bookmarks to export yet."));
                } else {
                    self.dialog = Some(Dialog::Export(ExportDialog::new(scope)));
                }
            }
            Action::Settings => self.dialog = Some(Dialog::Settings(SettingsDialog::default())),
            Action::About => {}
            Action::Shortcuts => self.dialog = Some(Dialog::Shortcuts),
            Action::Quit => {
                let live = self.tabs.iter().filter(|t| t.is_active()).count();
                if live > 0 && self.settings.confirm_close_connected {
                    self.dialog = Some(dialogs::confirm(
                        t("Quit Peler?"),
                        tf(
                            "{} connection(s) are open and will be closed.",
                            &[&live.to_string()],
                        ),
                        t("Quit"),
                        ConfirmAction::Quit,
                    ));
                } else {
                    self.allow_close = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
    }

    fn import_file(&mut self, path: PathBuf) {
        let data = match std::fs::read(&path) {
            Ok(d) => d,
            Err(e) => {
                self.toasts.error(format!("{}: {e}", path.display()));
                return;
            }
        };
        match transfer::read_import(&data) {
            Ok(incoming) => {
                let plan = ImportPlan::new(incoming, &self.store);
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.dialog = Some(Dialog::Import(ImportDialog::new(plan, name)));
            }
            Err(e) => self.toasts.error(format!("{e:#}")),
        }
    }

    fn do_export(&mut self, scope: ExportScope, passphrase: Option<String>) {
        let default_name = format!(
            "peler-bookmarks-{}.json",
            chrono::Local::now().format("%Y-%m-%d")
        );
        let Some(path) = rfd::FileDialog::new()
            .set_title(t("Export Bookmarks"))
            .set_file_name(default_name)
            .add_filter("JSON", &["json"])
            .save_file()
        else {
            return;
        };
        let result = transfer::export(
            &self.store,
            &scope,
            passphrase.as_deref(),
            secrets::load_set,
        )
        .and_then(|file| Ok(serde_json::to_vec_pretty(&file)?))
        .and_then(|json| Ok(paths::write_private(&path, &json)?));
        match result {
            Ok(()) => self
                .toasts
                .info(tf("Exported to {}", &[&path.display().to_string()])),
            Err(e) => self.toasts.error(format!("{e:#}")),
        }
    }

    fn apply_import(&mut self, plan: ImportPlan) {
        let outcome = plan.apply(&mut self.store);
        let mut failed = 0;
        for (id, set) in &outcome.secrets {
            if secrets::store_set(*id, set).is_err() {
                failed += 1;
            }
        }
        self.save_store();
        self.toasts.info(tf(
            "Import finished: {} added, {} updated, {} skipped.",
            &[
                &outcome.added.to_string(),
                &outcome.updated.to_string(),
                &outcome.skipped.to_string(),
            ],
        ));
        if failed > 0 {
            self.toasts.error(tf(
                "{} credential(s) could not be saved to the Keychain.",
                &[&failed.to_string()],
            ));
        }
    }

    fn apply_move(&mut self, mv: TreeMove) {
        match mv {
            TreeMove::Bookmark { id, group, before } => self.store.move_bookmark(id, group, before),
            TreeMove::Group { id, parent, before } => {
                if !self.store.move_group(id, parent, before) {
                    return;
                }
            }
        }
        self.save_store();
    }

    // ----- drawing ----------------------------------------------------------------------------

    fn tab_bar(&mut self, ui: &mut Ui) {
        let dark = ui.visuals().dark_mode;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let toggle = if self.settings.sidebar_visible {
                "◧"
            } else {
                "▢"
            };
            if ui
                .add(egui::Button::new(toggle).frame(false))
                .on_hover_text(format!("{} (⌘B)", t("Show/Hide Sidebar")))
                .clicked()
            {
                self.actions.push(Action::ToggleSidebar);
            }
            ui.add_space(4.0);
            let mut drag_target = None;
            egui::ScrollArea::horizontal()
                .id_salt("tab-strip")
                .max_width(ui.available_width() - 150.0)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        for i in 0..self.tabs.len() {
                            let active = i == self.active;
                            let tab = &self.tabs[i];
                            let tab_id = tab.id;
                            let (dot, status) = match tab.state() {
                                ConnState::Connected if tab.panes.iter().any(|p| p.is_live()) => {
                                    (style::GREEN, t("Connected"))
                                }
                                ConnState::Connected => (Color32::GRAY, t("Session ended")),
                                ConnState::Connecting | ConnState::Authenticating => {
                                    (style::AMBER, t("Connecting…"))
                                }
                                ConnState::Closed(Some(_)) => (style::RED, t("Disconnected")),
                                ConnState::Closed(None) => (Color32::GRAY, t("Disconnected")),
                            };
                            let waiting = tab.conn.shared.lock().prompt.is_some();
                            let activity = !active && tab.has_activity();
                            let title = tab.title();
                            let width = (ui.fonts_mut(|f| {
                                f.layout_no_wrap(
                                    title.clone(),
                                    egui::FontId::proportional(13.0),
                                    Color32::WHITE,
                                )
                                .size()
                                .x
                            }) + 54.0)
                                .clamp(90.0, 220.0);
                            let (rect, resp) = ui.allocate_exact_size(
                                vec2(width, TAB_H - 6.0),
                                Sense::click_and_drag(),
                            );
                            let painter = ui.painter();
                            let bg = if active {
                                ui.visuals().panel_fill
                            } else if resp.hovered() {
                                ui.visuals().widgets.hovered.weak_bg_fill
                            } else {
                                style::chrome_fill(dark)
                            };
                            painter.rect_filled(
                                rect,
                                CornerRadius {
                                    nw: 7,
                                    ne: 7,
                                    sw: 0,
                                    se: 0,
                                },
                                bg,
                            );
                            painter.circle_filled(
                                egui::pos2(rect.left() + 12.0, rect.center().y),
                                4.0,
                                dot,
                            );
                            let text_color = if active {
                                ui.visuals().strong_text_color()
                            } else {
                                ui.visuals().text_color()
                            };
                            let label = if waiting {
                                format!("{title}  •")
                            } else {
                                title.clone()
                            };
                            let clip = egui::Rect::from_min_max(
                                rect.min,
                                egui::pos2(rect.right() - 22.0, rect.bottom()),
                            );
                            painter.with_clip_rect(clip).text(
                                egui::pos2(rect.left() + 22.0, rect.center().y),
                                egui::Align2::LEFT_CENTER,
                                label,
                                if activity {
                                    egui::FontId::new(13.0, egui::FontFamily::Proportional)
                                } else {
                                    egui::FontId::proportional(13.0)
                                },
                                if activity { style::ACCENT } else { text_color },
                            );
                            if i < 9 {
                                let hint = format!("⌘{}", i + 1);
                                let _ = hint;
                            }
                            // Close button.
                            let close_rect = egui::Rect::from_center_size(
                                egui::pos2(rect.right() - 12.0, rect.center().y),
                                vec2(16.0, 16.0),
                            );
                            let close = ui.interact(
                                close_rect,
                                egui::Id::new(("tab-close", tab.id)),
                                Sense::click(),
                            );
                            if close.hovered() || resp.hovered() || active {
                                let c = if close.hovered() {
                                    ui.visuals().strong_text_color()
                                } else {
                                    ui.visuals().weak_text_color()
                                };
                                if close.hovered() {
                                    ui.painter().rect_filled(
                                        close_rect,
                                        CornerRadius::same(4),
                                        ui.visuals().widgets.hovered.bg_fill,
                                    );
                                }
                                ui.painter().text(
                                    close_rect.center(),
                                    egui::Align2::CENTER_CENTER,
                                    "×",
                                    egui::FontId::proportional(14.0),
                                    c,
                                );
                            }
                            #[cfg(debug_assertions)]
                            {
                                crate::devtest::record(format!("tab:{i}"), rect);
                                crate::devtest::record(format!("tab-close:{i}"), close_rect);
                            }
                            if close.clicked() || resp.middle_clicked() {
                                self.actions.push(Action::CloseTab(tab_id));
                            } else if resp.clicked() {
                                self.actions.push(Action::SelectTab(i));
                            }
                            let resp =
                                resp.on_hover_text(format!("{}\n{status}", tab.bookmark.address()));
                            resp.dnd_set_drag_payload(tab_id);
                            if let Some(from) = resp.dnd_release_payload::<u64>() {
                                drag_target = Some((*from, tab_id));
                            }
                            resp.context_menu(|ui| {
                                if ui.button(t("Duplicate Tab")).clicked() {
                                    self.actions.push(Action::DuplicateTab(tab_id));
                                    ui.close();
                                }
                                if ui.button(t("Reconnect")).clicked() {
                                    self.actions.push(Action::SelectTab(i));
                                    self.actions.push(Action::Reconnect);
                                    ui.close();
                                }
                                if ui.button(t("Edit Bookmark…")).clicked() {
                                    self.actions
                                        .push(Action::EditBookmark(self.tabs[i].bookmark.id));
                                    ui.close();
                                }
                                ui.separator();
                                if ui.button(t("Close Tab")).clicked() {
                                    self.actions.push(Action::CloseTab(tab_id));
                                    ui.close();
                                }
                            });
                        }
                        if ui
                            .add(egui::Button::new(" + ").frame(false))
                            .on_hover_text(format!("{} (⌘K)", t("Quick Connect…")))
                            .clicked()
                        {
                            self.actions.push(Action::QuickConnect);
                        }
                    });
                });
            if let Some((from, to)) = drag_target {
                self.actions.push(Action::MoveTab { from, to });
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let has_tab = !self.tabs.is_empty();
                let sftp_on = self.tabs.get(self.active).is_some_and(|t| t.show_sftp);
                if ui
                    .add_enabled(
                        has_tab,
                        egui::Button::new(RichText::new(t("SFTP"))).selected(sftp_on),
                    )
                    .on_hover_text(format!("{} (⇧⌘F)", t("SFTP Panel")))
                    .clicked()
                {
                    self.actions.push(Action::ToggleSftp);
                }
                self.theme_button(ui);
                ui.add_enabled_ui(has_tab, |ui| {
                    ui.menu_button("⚡", |ui| {
                        if self.settings.quick_commands.is_empty() {
                            ui.label(RichText::new(t("No quick commands")).weak());
                        }
                        for qc in &self.settings.quick_commands {
                            let label = if qc.name.trim().is_empty() {
                                qc.command.clone()
                            } else {
                                qc.name.clone()
                            };
                            if ui
                                .button(label)
                                .on_hover_text(RichText::new(&qc.command).monospace())
                                .clicked()
                            {
                                self.actions.push(Action::SendQuickCommand(qc.id));
                                ui.close();
                            }
                        }
                        ui.separator();
                        if ui.button(t("Edit quick commands…")).clicked() {
                            self.actions.push(Action::Settings);
                            ui.close();
                        }
                    })
                    .response
                    .on_hover_text(t("Quick Commands"));
                    if ui
                        .button("◫")
                        .on_hover_text(format!("{} (⌘D)", t("Split Right")))
                        .clicked()
                    {
                        self.actions.push(Action::SplitRight);
                    }
                });
            });
        });
    }

    /// Toolbar button: click toggles dark/light; right-click offers "follow macOS".
    fn theme_button(&mut self, ui: &mut Ui) {
        let dark = ui.visuals().dark_mode;
        let resp = ui
            .add(egui::Button::new("").min_size(vec2(30.0, 22.0)))
            .on_hover_text(if dark {
                t("Switch to light theme")
            } else {
                t("Switch to dark theme")
            });
        let c = resp.rect.center();
        let color = ui.visuals().strong_text_color();
        let painter = ui.painter();
        if dark {
            // Sun: shows what a click switches to.
            painter.circle_filled(c, 3.6, color);
            for i in 0..8 {
                let a = i as f32 * std::f32::consts::FRAC_PI_4;
                let d = vec2(a.cos(), a.sin());
                painter.line_segment([c + d * 5.6, c + d * 7.6], Stroke::new(1.3, color));
            }
        } else {
            // Crescent moon.
            painter.circle_filled(c, 6.5, color);
            painter.circle_filled(
                c + vec2(3.2, -2.6),
                5.6,
                ui.visuals().widgets.inactive.weak_bg_fill,
            );
        }
        if resp.clicked() {
            self.settings.theme = if dark {
                ThemeMode::Light
            } else {
                ThemeMode::Dark
            };
        }
        resp.context_menu(|ui| {
            ui.radio_value(
                &mut self.settings.theme,
                ThemeMode::System,
                t("Follow macOS"),
            );
            ui.radio_value(&mut self.settings.theme, ThemeMode::Light, t("Light"));
            ui.radio_value(&mut self.settings.theme, ThemeMode::Dark, t("Dark"));
        });
    }

    fn welcome(&mut self, ui: &mut Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space((ui.available_height() * 0.25).max(20.0));
            ui.label(RichText::new("Peler").size(34.0).strong());
            ui.label(RichText::new(t("Lightweight SSH and SFTP client")).weak());
            ui.add_space(18.0);
            if ui
                .add(
                    egui::Button::new(
                        RichText::new(format!("{}   ⌘K", t("Search bookmarks and connect")))
                            .size(15.0),
                    )
                    .min_size(vec2(320.0, 34.0)),
                )
                .clicked()
            {
                self.actions.push(Action::QuickConnect);
            }
            ui.add_space(6.0);
            if ui
                .add(
                    egui::Button::new(format!("{}   ⌘N", t("New Bookmark…")))
                        .min_size(vec2(320.0, 30.0)),
                )
                .clicked()
            {
                self.actions.push(Action::NewBookmark { group: None });
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let w = 320.0;
                ui.add_space((ui.available_width() - w) / 2.0);
                if ui
                    .add(
                        egui::Button::new(t("Import Bookmarks…"))
                            .min_size(vec2(w / 2.0 - 4.0, 28.0)),
                    )
                    .clicked()
                {
                    self.actions.push(Action::Import);
                }
                if ui
                    .add(
                        egui::Button::new(t("Import from ~/.ssh/config"))
                            .min_size(vec2(w / 2.0 - 4.0, 28.0)),
                    )
                    .clicked()
                {
                    self.actions.push(Action::ImportSshConfig);
                }
            });
            ui.add_space(14.0);
            ui.label(
                RichText::new(t(
                    "Tip: double-click a bookmark, or select it in the search and press Enter.",
                ))
                .weak()
                .small(),
            );
        });
    }

    fn central(&mut self, ui: &mut Ui) {
        if self.tabs.is_empty() {
            self.welcome(ui);
            return;
        }
        let ctx = ui.ctx().clone();
        let cell = self.cell_size(&ctx);
        let scheme = self.scheme(&ctx);
        let modal_open = self.modal_open();
        let rt = self.rt.handle().clone();
        let fonts_choice = fonts::effective_choice(&self.settings.font);
        let term_style = terminal::TermStyle {
            font_size: self.settings.font_size,
            cell,
            scheme,
            real_bold: fonts::has_real_face(&fonts_choice, true, false),
            real_italic: fonts::has_real_face(&fonts_choice, false, true),
            option_as_meta: self.settings.option_as_meta,
            copy_on_select: self.settings.copy_on_select,
            cursor_blink: self.settings.cursor_blink,
        };
        let active = self.active;
        let tab = &mut self.tabs[active];
        tab.clear_activity();
        let state = tab.state();
        let has_prompt = tab.conn.shared.lock().prompt.is_some();

        // Status banner.
        let banner = match &state {
            ConnState::Connecting => Some((
                tf("Connecting to {}…", &[&tab.bookmark.address()]),
                style::AMBER,
                false,
            )),
            ConnState::Authenticating if !has_prompt => Some((
                tf("Authenticating as {}…", &[&tab.bookmark.address()]),
                style::AMBER,
                false,
            )),
            ConnState::Closed(Some(e)) => Some((e.message(), style::RED, true)),
            ConnState::Closed(None) => Some((t("Disconnected.").to_string(), Color32::GRAY, true)),
            _ => None,
        };
        if let Some((text, color, offer_reconnect)) = banner {
            egui::Frame::NONE
                .fill(color.gamma_multiply(0.18))
                .inner_margin(egui::Margin::symmetric(12, 6))
                .show(ui, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.horizontal(|ui| {
                        if !offer_reconnect {
                            ui.spinner();
                        }
                        ui.label(RichText::new(text).color(if ui.visuals().dark_mode {
                            Color32::WHITE
                        } else {
                            Color32::BLACK
                        }));
                        if offer_reconnect {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.button(t("Edit Bookmark…")).clicked() {
                                        self.actions.push(Action::EditBookmark(tab.bookmark.id));
                                    }
                                    if ui
                                        .button(
                                            RichText::new(format!("{}  ⌘R", t("Reconnect")))
                                                .strong(),
                                        )
                                        .clicked()
                                    {
                                        self.actions.push(Action::Reconnect);
                                    }
                                },
                            );
                        }
                    });
                });
        }

        // Terminal area and optional SFTP panel.
        let full = ui.available_rect_before_wrap();
        let (term_rect, sftp_rect) = if tab.show_sftp {
            let split_y = (full.top() + full.height() * (1.0 - tab.sftp_split)).round();
            (
                egui::Rect::from_min_max(full.min, egui::pos2(full.right(), split_y - 3.0)),
                Some(egui::Rect::from_min_max(
                    egui::pos2(full.left(), split_y + 3.0),
                    full.max,
                )),
            )
        } else {
            (full, None)
        };

        // A split pane whose shell exited (e.g. `exit`/`logout`) closes by itself; the last
        // pane stays so the session can be reconnected.
        if tab.panes.len() > 1 {
            let ended: Vec<_> = tab
                .panes
                .iter()
                .filter(|p| {
                    matches!(
                        *p.status.lock(),
                        crate::ssh::session::PaneStatus::Exited(Some(_))
                    )
                })
                .map(|p| p.id)
                .collect();
            for id in ended {
                if tab.panes.len() > 1 {
                    tab.close_pane(id);
                }
            }
        }
        // Keyboard focus follows the active pane (after a split, a tab switch, ⌘[ / ⌘]).
        let want_focus = (tab.id, tab.focused);
        if self.focus_target != Some(want_focus) && !modal_open && !has_prompt {
            ctx.memory_mut(|m| m.request_focus(terminal::pane_widget_id(tab.focused)));
            self.focus_target = Some(want_focus);
        }

        let (pane_rects, dividers) = tab.layout.arrange(term_rect);
        let multi = pane_rects.len() > 1;
        let tab_id = tab.id;
        let mut new_focus = None;
        let mut close_pane = None;
        for (pane_index, (pane_id, rect)) in pane_rects.into_iter().enumerate() {
            #[cfg(debug_assertions)]
            crate::devtest::record(format!("pane:{pane_index}"), rect);
            let _ = pane_index;
            let focused_pane = tab.focused == pane_id;
            let Some(pane) = tab.pane_mut(pane_id) else {
                continue;
            };
            let alive = pane.is_live();
            let resp = terminal::show(
                ui,
                rect,
                &mut pane.view,
                &pane.shared,
                &term_style,
                focused_pane,
                !modal_open && !has_prompt,
                alive,
            );
            if !resp.input.is_empty() {
                pane.send(resp.input);
            }
            if let Some((size, px_w, px_h)) = resp.resized {
                let _ = pane.input.send(crate::ssh::session::PaneInput::Resize {
                    cols: size.cols as u32,
                    rows: size.rows as u32,
                    px_w,
                    px_h,
                });
            }
            if let Some(text) = pane.shared.notes.lock().clipboard.take() {
                ctx.copy_text(text);
            }
            if resp.reconnect {
                self.actions.push(Action::Reconnect);
            }
            if resp.activated {
                new_focus = Some(pane_id);
            }
            if multi && focused_pane {
                ui.painter().rect_stroke(
                    rect,
                    CornerRadius::ZERO,
                    Stroke::new(1.0, style::ACCENT.gamma_multiply(0.7)),
                    egui::StrokeKind::Inside,
                );
            }
            // Close button for split panes (a single pane is closed with its tab).
            if multi && !modal_open {
                let close_rect = egui::Rect::from_center_size(
                    egui::pos2(rect.right() - 16.0, rect.top() + 16.0),
                    vec2(20.0, 20.0),
                );
                let close = ui
                    .interact(
                        close_rect,
                        egui::Id::new(("pane-close", tab_id, pane_id)),
                        Sense::click(),
                    )
                    .on_hover_text(format!("{} (⌘W)", t("Close pane")));
                if close.hovered() || ui.rect_contains_pointer(rect) {
                    let bg = if close.hovered() {
                        ui.visuals().widgets.hovered.bg_fill
                    } else {
                        ui.visuals().widgets.inactive.weak_bg_fill
                    };
                    ui.painter().circle_filled(close_rect.center(), 9.0, bg);
                    ui.painter().text(
                        close_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "×",
                        egui::FontId::proportional(15.0),
                        ui.visuals().strong_text_color(),
                    );
                }
                #[cfg(debug_assertions)]
                crate::devtest::record(format!("pane-close:{pane_index}"), close_rect);
                if close.clicked() {
                    close_pane = Some(pane_id);
                }
            }
            let bell = pane.shared.notes.lock().bell_at;
            if let Some(at) = bell {
                let age = at.elapsed().as_secs_f32();
                if age < 0.25 {
                    ui.painter().rect_filled(
                        rect,
                        CornerRadius::ZERO,
                        Color32::from_white_alpha(((0.25 - age) * 120.0) as u8),
                    );
                    ctx.request_repaint_after(Duration::from_millis(30));
                }
            }
        }
        if let Some(f) = new_focus {
            tab.focused = f;
        }
        if let Some(id) = close_pane {
            tab.close_pane(id);
        }
        for (i, d) in dividers.iter().enumerate() {
            let resp = ui.interact(
                d.rect.expand(2.0),
                egui::Id::new(("divider", tab.id, i)),
                Sense::drag(),
            );
            let color = if resp.hovered() || resp.dragged() {
                style::ACCENT
            } else {
                ui.visuals().widgets.noninteractive.bg_stroke.color
            };
            ui.painter().rect_filled(d.rect, CornerRadius::ZERO, color);
            if resp.hovered() || resp.dragged() {
                ctx.set_cursor_icon(match d.dir {
                    SplitDir::Horizontal => egui::CursorIcon::ResizeHorizontal,
                    SplitDir::Vertical => egui::CursorIcon::ResizeVertical,
                });
            }
            if resp.dragged()
                && let Some(p) = resp.interact_pointer_pos()
            {
                tab.layout.set_ratio(&d.path, layout::ratio_at(d, p));
            }
        }

        if let Some(rect) = sftp_rect {
            // Divider between terminal and SFTP panel.
            let div = egui::Rect::from_min_max(
                egui::pos2(full.left(), rect.top() - 6.0),
                egui::pos2(full.right(), rect.top()),
            );
            let resp = ui.interact(div, egui::Id::new(("sftp-divider", tab.id)), Sense::drag());
            ui.painter().rect_filled(
                div.shrink2(vec2(0.0, 2.0)),
                CornerRadius::ZERO,
                if resp.hovered() || resp.dragged() {
                    style::ACCENT
                } else {
                    ui.visuals().widgets.noninteractive.bg_stroke.color
                },
            );
            if resp.hovered() || resp.dragged() {
                ctx.set_cursor_icon(egui::CursorIcon::ResizeVertical);
            }
            if resp.dragged()
                && let Some(p) = resp.interact_pointer_pos()
            {
                tab.sftp_split = (1.0 - (p.y - full.top()) / full.height()).clamp(0.15, 0.85);
            }
            let last = self.settings.last_local_dir.clone();
            if tab.sftp.is_none() && tab.state().is_connected() {
                tab.sftp = Some(tab.new_sftp_panel(last.as_deref()));
            }
            let mut child =
                ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(8.0, 4.0))));
            match &mut tab.sftp {
                Some(panel) => {
                    if let PanelOutput::LocalDir(dir) = panel.show(&mut child, &rt, !modal_open) {
                        self.settings.last_local_dir = Some(dir);
                    }
                }
                None => {
                    child.centered_and_justified(|ui| {
                        ui.label(
                            RichText::new(t("SFTP opens once the connection is ready.")).weak(),
                        )
                    });
                }
            }
        }

        // Files dragged from Finder over the window.
        let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());
        if hovering && state.is_connected() {
            let painter = ui.ctx().layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("drop-overlay"),
            ));
            painter.rect_filled(
                full,
                CornerRadius::same(8),
                style::ACCENT.gamma_multiply(0.15),
            );
            painter.rect_stroke(
                full.shrink(4.0),
                CornerRadius::same(8),
                Stroke::new(2.0, style::ACCENT),
                egui::StrokeKind::Inside,
            );
            painter.text(
                full.center(),
                egui::Align2::CENTER_CENTER,
                t("Drop to upload to the current remote folder"),
                egui::FontId::proportional(18.0),
                style::ACCENT,
            );
        }
    }

    fn prompt_ui(&mut self, ctx: &egui::Context) {
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return;
        };
        let view = {
            let s = tab.conn.shared.lock();
            match &s.prompt {
                Some(Prompt::HostKey {
                    host,
                    port,
                    algorithm,
                    fingerprint,
                    status,
                    ..
                }) => PromptView::HostKey {
                    host: host.clone(),
                    port: *port,
                    algorithm: algorithm.clone(),
                    fingerprint: fingerprint.clone(),
                    changed: *status == HostKeyStatus::Changed,
                },
                Some(Prompt::Secret {
                    kind,
                    message,
                    echo,
                    can_save,
                    retry,
                    ..
                }) => PromptView::Secret {
                    kind: *kind,
                    message: message.clone(),
                    echo: *echo,
                    can_save: *can_save,
                    retry: *retry,
                },
                None => return,
            }
        };
        let mut answer: Option<Option<SecretAnswer>> = None;
        let mut hostkey_answer: Option<bool> = None;
        let modal = egui::Modal::new(egui::Id::new(("prompt", tab.id))).show(ctx, |ui| {
            ui.set_width(440.0);
            match &view {
                PromptView::HostKey { host, port, algorithm, fingerprint, changed } => {
                    if *changed {
                        ui.heading(RichText::new(t("Warning: the host key has changed!")).color(style::RED));
                        ui.label(t("The server presented a different key than the one saved for it. Someone could be intercepting the connection, or the server may have been reinstalled. Only continue if you know why the key changed."));
                    } else {
                        ui.heading(t("Unknown host"));
                        ui.label(t("This server has not been seen before. Check that the fingerprint matches the server's key before trusting it."));
                    }
                    ui.add_space(6.0);
                    egui::Grid::new("hostkey").num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
                        ui.label(t("Host"));
                        ui.label(RichText::new(format!("{host}:{port}")).strong());
                        ui.end_row();
                        ui.label(t("Key type"));
                        ui.label(algorithm.as_str());
                        ui.end_row();
                        ui.label(t("Fingerprint"));
                        ui.add(egui::Label::new(RichText::new(fingerprint.as_str()).monospace()).wrap().selectable(true));
                        ui.end_row();
                    });
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        let accept = if *changed { t("Accept New Key") } else { t("Trust and Connect") };
                        if *changed {
                            if ui.button(RichText::new(t("Cancel")).strong()).clicked() {
                                hostkey_answer = Some(false);
                            }
                            if ui.button(RichText::new(accept).color(style::RED)).clicked() {
                                hostkey_answer = Some(true);
                            }
                        } else {
                            // Enter only counts once the prompt has been on screen for a frame,
                            // so a key press meant for the terminal can't accept a host key.
                            let enter = tab.prompt.armed && ui.input(|i| i.key_pressed(Key::Enter));
                            if ui.button(RichText::new(accept).strong()).clicked() || enter {
                                hostkey_answer = Some(true);
                            }
                            if ui.button(t("Cancel")).clicked() {
                                hostkey_answer = Some(false);
                            }
                        }
                    });
                }
                PromptView::Secret { kind, message, echo, can_save, retry } => {
                    let title = match kind {
                        SecretPromptKind::Password => tf("Password for {}", &[message]),
                        SecretPromptKind::Passphrase => t("Key passphrase").to_string(),
                        SecretPromptKind::Interactive => t("The server asks").to_string(),
                    };
                    ui.heading(title);
                    match kind {
                        SecretPromptKind::Passphrase => {
                            ui.label(tf("The private key ({}) is protected by a passphrase.", &[message]));
                        }
                        SecretPromptKind::Interactive => {
                            ui.label(message.as_str());
                        }
                        SecretPromptKind::Password => {}
                    }
                    if *retry {
                        ui.colored_label(style::RED, if *kind == SecretPromptKind::Passphrase { t("Wrong passphrase, try again.") } else { t("Wrong password, try again.") });
                    }
                    let r = ui.add(egui::TextEdit::singleline(&mut tab.prompt.text).password(!echo).desired_width(f32::INFINITY));
                    if !tab.prompt.focus {
                        r.request_focus();
                        tab.prompt.focus = true;
                    }
                    if *can_save {
                        ui.checkbox(&mut tab.prompt.save, t("Save in Keychain"));
                    }
                    let submit = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button(RichText::new(t("Connect")).strong()).clicked() || submit {
                            answer = Some(Some(SecretAnswer { value: std::mem::take(&mut tab.prompt.text), save: tab.prompt.save }));
                        }
                        if ui.button(t("Cancel")).clicked() {
                            answer = Some(None);
                        }
                    });
                }
            }
        });
        if modal.should_close() && tab.prompt.armed && answer.is_none() && hostkey_answer.is_none()
        {
            match view {
                PromptView::HostKey { .. } => hostkey_answer = Some(false),
                PromptView::Secret { .. } => answer = Some(None),
            }
        }
        if answer.is_some() || hostkey_answer.is_some() {
            let prompt = tab.conn.shared.lock().prompt.take();
            match prompt {
                Some(Prompt::HostKey { reply, .. }) => {
                    let _ = reply.send(hostkey_answer.unwrap_or(false));
                }
                Some(Prompt::Secret { reply, .. }) => {
                    let _ = reply.send(answer.flatten());
                }
                None => {}
            }
            tab.prompt.text.clear();
            tab.prompt.focus = false;
            tab.prompt.armed = false;
            if let Some(id) = tab.focused_pane().and_then(|p| p.view.id) {
                ctx.memory_mut(|m| m.request_focus(id));
            }
        } else {
            tab.prompt.armed = true;
        }
    }

    fn overlays(&mut self, ctx: &egui::Context) {
        if let Some(editor) = &mut self.editor {
            match editor.show(ctx, &self.store) {
                EditorResult::Open => {}
                EditorResult::Cancel => self.editor = None,
                EditorResult::Save {
                    bookmark,
                    set,
                    clear,
                    connect,
                } => {
                    let id = bookmark.id;
                    let mut errors = Vec::new();
                    if let Err(e) = secrets::store_set(id, &set) {
                        errors.push(format!("{e:#}"));
                    }
                    for kind in clear {
                        let _ = secrets::set(id, kind, None);
                    }
                    if !bookmark.auth.uses_key() {
                        let _ = secrets::set(id, SecretKind::Passphrase, None);
                    }
                    self.store.upsert_bookmark(bookmark.clone());
                    self.save_store();
                    self.sidebar.reveal(&self.store, id);
                    self.editor = None;
                    for e in errors {
                        self.toasts.error(e);
                    }
                    if connect {
                        self.open_tab(ctx, bookmark);
                    }
                }
            }
        }
        if let Some(palette) = &mut self.palette {
            match palette.show(ctx, &self.store) {
                PaletteResult::Open => {}
                PaletteResult::Close => self.palette = None,
                PaletteResult::Connect(id) => {
                    self.palette = None;
                    self.actions.push(Action::Connect(id));
                }
            }
        }
        if let Some(dialog) = &mut self.dialog {
            match dialog.show(ctx, &self.store, &mut self.settings) {
                DialogResult::Open => {}
                DialogResult::Close => self.dialog = None,
                DialogResult::Confirmed(action) => {
                    self.dialog = None;
                    match action {
                        ConfirmAction::DeleteBookmark(id) => {
                            self.store.remove_bookmark(id);
                            secrets::delete_all(id);
                            self.save_store();
                        }
                        ConfirmAction::DeleteGroup(id) => {
                            self.store.remove_group(id);
                            self.save_store();
                        }
                        ConfirmAction::CloseTab(id) => {
                            if let Some(i) = self.tab_index(id) {
                                self.close_tab(i);
                            }
                        }
                        ConfirmAction::Quit => {
                            self.allow_close = true;
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    }
                }
                DialogResult::GroupName(target, name) => {
                    self.dialog = None;
                    match target {
                        GroupTarget::New { parent } => {
                            let id = self.store.add_group(name, parent);
                            self.sidebar.selected = Some(id);
                        }
                        GroupTarget::Rename(id) => self.store.rename_group(id, name),
                    }
                    self.save_store();
                }
                DialogResult::Export { scope, passphrase } => {
                    self.dialog = None;
                    self.do_export(scope, passphrase);
                }
                DialogResult::Import(plan) => {
                    self.dialog = None;
                    self.apply_import(*plan);
                }
            }
        }
        self.toasts.show(ctx);
    }

    fn update_title(&mut self, ctx: &egui::Context) {
        let title = match self.tabs.get(self.active) {
            Some(tab) => {
                let remote = tab
                    .focused_pane()
                    .and_then(|p| p.title())
                    .filter(|t| !t.trim().is_empty());
                match remote {
                    Some(r) => format!("{} — {r}", tab.title()),
                    None => format!("{} — Peler", tab.title()),
                }
            }
            None => "Peler".to_string(),
        };
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
    }
}

impl eframe::App for PelerApp {
    fn ui(&mut self, ui: &mut Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.handle_menu_events();
        self.apply_settings_changes(&ctx, frame);
        self.handle_shortcuts(&ctx);
        self.handle_close_request(&ctx);
        self.handle_dropped_files(&ctx);
        if self.first_frame {
            self.open_startup_targets(&ctx);
            if std::env::var_os("PELER_STARTUP_LOG").is_some()
                && let Some(start) = PROCESS_START.get()
            {
                eprintln!("first frame after {} ms", start.elapsed().as_millis());
            }
        }
        #[cfg(debug_assertions)]
        self.run_dev_script(&ctx);
        self.first_frame = false;

        let dark = ui.visuals().dark_mode;
        // Panels are laid out with the root Ui's item spacing between them; keep them flush and
        // restore normal spacing inside each panel.
        let spacing = ui.spacing().item_spacing;
        ui.spacing_mut().item_spacing = egui::Vec2::ZERO;
        egui::Panel::top("tab-bar")
            .frame(
                egui::Frame::NONE
                    .fill(style::chrome_fill(dark))
                    .inner_margin(egui::Margin {
                        left: 8,
                        right: 8,
                        top: 4,
                        bottom: 0,
                    }),
            )
            .exact_size(TAB_H)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = spacing;
                self.tab_bar(ui)
            });

        let mut sidebar_visible = self.settings.sidebar_visible;
        let sidebar = egui::Panel::left("sidebar")
            .resizable(true)
            .default_size(self.settings.sidebar_width)
            .size_range(180.0..=480.0)
            .frame(
                egui::Frame::NONE
                    .fill(style::chrome_fill(dark))
                    .inner_margin(egui::Margin::symmetric(8, 4)),
            )
            .show_collapsible(ui, &mut sidebar_visible, |ui| {
                ui.spacing_mut().item_spacing = spacing;
                let live = self.live_map();
                let (actions, moved) = self.sidebar.show(ui, &self.store, &live);
                self.actions.extend(actions);
                if let Some(mv) = moved {
                    self.apply_move(mv);
                }
            });
        if let Some(r) = sidebar {
            let w = r.response.rect.width();
            if (w - self.settings.sidebar_width).abs() > 1.0 && sidebar_visible {
                self.settings.sidebar_width = w;
            }
        }
        self.settings.sidebar_visible = sidebar_visible;

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(ui.visuals().panel_fill))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = spacing;
                self.central(ui)
            });

        self.prompt_ui(&ctx);
        self.overlays(&ctx);
        self.process_actions(&ctx);
        self.update_title(&ctx);
    }

    fn on_exit(&mut self) {
        for tab in &self.tabs {
            tab.disconnect();
        }
        if self.settings != self.settings_saved {
            let _ = self.settings.save(&paths::settings_file());
        }
    }
}
