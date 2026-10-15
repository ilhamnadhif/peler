//! Tabs: one SSH connection each, with one or more terminal panes (split view) on it.

use std::path::PathBuf;
use std::sync::Arc;

use eframe::egui;
use parking_lot::Mutex;
use tokio::sync::mpsc;

use super::layout::{Layout, PaneId, SplitDir};
use super::sftp_panel::SftpPanel;
use super::terminal::TermView;
use crate::bookmarks::Bookmark;
use crate::settings::Settings;
use crate::ssh::session::{ConnectSpec, Connection, PaneInput, PaneStart, PaneStatus};
use crate::ssh::types::{ConnError, ConnState};
use crate::term::core::{self as tcore, Feeder, GridSize, QueryPalette, SharedTerm};
use crate::term::scheme::Scheme;

pub struct Pane {
    pub id: PaneId,
    pub shared: SharedTerm,
    pub feeder: Arc<Mutex<Feeder>>,
    pub input: mpsc::UnboundedSender<PaneInput>,
    pub status: Arc<Mutex<PaneStatus>>,
    pub view: TermView,
}

impl Pane {
    pub fn send(&self, bytes: Vec<u8>) {
        if !bytes.is_empty() {
            let _ = self.input.send(PaneInput::Data(bytes));
        }
    }

    pub fn is_live(&self) -> bool {
        self.status.lock().is_live()
    }

    pub fn title(&self) -> Option<String> {
        self.shared.notes.lock().title.clone()
    }
}

/// What tabs need from the app to create connections and panes.
pub struct Env<'a> {
    pub rt: &'a tokio::runtime::Handle,
    pub ctx: &'a egui::Context,
    pub settings: &'a Settings,
    pub scheme: &'a Scheme,
    pub known_hosts: PathBuf,
}

pub struct Tab {
    pub id: u64,
    pub bookmark: Bookmark,
    pub conn: Connection,
    pub panes: Vec<Pane>,
    pub layout: Layout,
    pub focused: PaneId,
    pub sftp: Option<SftpPanel>,
    pub show_sftp: bool,
    /// Height of the SFTP panel as a fraction of the tab.
    pub sftp_split: f32,
    pub prompt: PromptInput,
}

/// Text typed into a password/passphrase prompt (kept until submitted).
#[derive(Default)]
pub struct PromptInput {
    pub text: String,
    pub save: bool,
    pub focus: bool,
}

fn spec(bookmark: &Bookmark, env: &Env) -> ConnectSpec {
    ConnectSpec { bookmark: bookmark.clone(), keepalive_secs: env.settings.keepalive_secs, known_hosts: env.known_hosts.clone() }
}

fn new_pane(id: PaneId, size: GridSize, env: &Env) -> (Pane, mpsc::UnboundedReceiver<PaneInput>) {
    let config = tcore::term_config(env.settings.scrollback, env.settings.cursor_style, env.settings.cursor_blink);
    let (shared, events) = tcore::new_terminal(config, size, QueryPalette::from(env.scheme));
    let feeder = Arc::new(Mutex::new(Feeder::new(shared.clone(), events)));
    let (tx, rx) = mpsc::unbounded_channel();
    let pane = Pane {
        id,
        shared,
        feeder,
        input: tx,
        status: Arc::new(Mutex::new(PaneStatus::Starting)),
        view: TermView::default(),
    };
    (pane, rx)
}

fn pane_start(pane: &Pane, rx: mpsc::UnboundedReceiver<PaneInput>, bookmark: &Bookmark, first: bool) -> PaneStart {
    let (cols, rows, px_w, px_h) = {
        let w = *pane.shared.window.lock();
        (w.num_cols as u32, w.num_lines as u32, w.num_cols as u32 * w.cell_width as u32, w.num_lines as u32 * w.cell_height as u32)
    };
    PaneStart {
        feeder: pane.feeder.clone(),
        input_tx: pane.input.clone(),
        input_rx: rx,
        cols,
        rows,
        px_w,
        px_h,
        start_dir: bookmark.start_dir.clone(),
        startup_command: if first { bookmark.startup_command.clone() } else { String::new() },
        status: pane.status.clone(),
    }
}

impl Tab {
    pub fn open(id: u64, pane_id: PaneId, bookmark: Bookmark, size: GridSize, env: &Env) -> Self {
        let conn = Connection::start(env.rt, env.ctx.clone(), spec(&bookmark, env));
        let (pane, rx) = new_pane(pane_id, size, env);
        conn.open_shell(pane_start(&pane, rx, &bookmark, true));
        Tab {
            id,
            bookmark,
            conn,
            panes: vec![pane],
            layout: Layout::Pane(pane_id),
            focused: pane_id,
            sftp: None,
            show_sftp: false,
            sftp_split: 0.45,
            prompt: PromptInput::default(),
        }
    }

    pub fn state(&self) -> ConnState {
        self.conn.state()
    }

    pub fn pane(&self, id: PaneId) -> Option<&Pane> {
        self.panes.iter().find(|p| p.id == id)
    }

    pub fn pane_mut(&mut self, id: PaneId) -> Option<&mut Pane> {
        self.panes.iter_mut().find(|p| p.id == id)
    }

    pub fn focused_pane(&self) -> Option<&Pane> {
        self.pane(self.focused)
    }

    /// Tab label: the bookmark name.
    pub fn title(&self) -> String {
        self.bookmark.display_name()
    }

    /// Splits the focused pane, opening a new shell on the same connection.
    pub fn split(&mut self, new_id: PaneId, dir: SplitDir, env: &Env) {
        let size = self.focused_pane().and_then(|p| p.view.grid).unwrap_or(GridSize { cols: 80, rows: 24 });
        let (pane, rx) = new_pane(new_id, size, env);
        if self.state().is_connected() || !self.state().is_closed() {
            self.conn.open_shell(pane_start(&pane, rx, &self.bookmark, false));
        }
        self.panes.push(pane);
        self.layout.split(self.focused, new_id, dir);
        self.focused = new_id;
    }

    /// Closes a pane. Returns true when it was the last one (the tab should close).
    pub fn close_pane(&mut self, id: PaneId) -> bool {
        if self.panes.len() <= 1 {
            return true;
        }
        if let Some(p) = self.pane(id) {
            let _ = p.input.send(PaneInput::Close);
        }
        self.layout.remove(id);
        self.panes.retain(|p| p.id != id);
        if !self.layout.contains(self.focused) {
            self.focused = self.layout.panes()[0];
        }
        false
    }

    /// Reconnects, keeping each pane's scrollback.
    pub fn reconnect(&mut self, env: &Env) {
        self.conn.disconnect();
        self.conn = Connection::start(env.rt, env.ctx.clone(), spec(&self.bookmark, env));
        self.sftp = None;
        let bookmark = self.bookmark.clone();
        for (i, pane) in self.panes.iter_mut().enumerate() {
            let (tx, rx) = mpsc::unbounded_channel();
            pane.input = tx;
            *pane.status.lock() = PaneStatus::Starting;
            tcore::write_status(&mut pane.feeder.lock(), &format!("— {} —", crate::i18n::t("reconnecting")));
            self.conn.open_shell(pane_start(pane, rx, &bookmark, i == 0));
        }
    }

    pub fn disconnect(&self) {
        self.conn.disconnect();
    }

    /// Whether any pane is waiting for something or alive (for "close tab?" confirmation).
    pub fn is_active(&self) -> bool {
        self.state().is_connected() && self.panes.iter().any(Pane::is_live)
    }

    /// Error to show for a failed connection, if any.
    pub fn error(&self) -> Option<ConnError> {
        match self.state() {
            ConnState::Closed(Some(e)) => Some(e),
            _ => None,
        }
    }

    pub fn has_activity(&self) -> bool {
        self.panes.iter().any(|p| p.shared.notes.lock().activity)
    }

    pub fn clear_activity(&self) {
        for p in &self.panes {
            p.shared.notes.lock().activity = false;
        }
    }

    /// Applies terminal settings (scrollback, cursor, colours) to every pane.
    pub fn apply_settings(&self, settings: &Settings, scheme: &Scheme) {
        for p in &self.panes {
            let config = tcore::term_config(settings.scrollback, settings.cursor_style, settings.cursor_blink);
            p.shared.term.lock().set_options(config);
            *p.shared.palette.lock() = QueryPalette::from(scheme);
        }
    }
}
