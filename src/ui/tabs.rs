//! Tabs: one SSH connection each, with one or more terminal panes (split view) on it.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use eframe::egui;
use parking_lot::Mutex;
use tokio::sync::mpsc;

use super::layout::{Layout, PaneId, SplitDir};
use super::sftp_panel::SftpPanel;
use super::terminal::TermView;
use crate::bookmarks::Bookmark;
use crate::settings::Settings;
use crate::ssh::session::{ConnectSpec, Connection, PaneInput, PaneStart, PaneStatus};
use crate::ssh::types::{ConnError, ConnState, Prompt, SecretAnswer};
use crate::term::core::{self as tcore, Feeder, GridSize, QueryPalette, SharedTerm};
use crate::term::scheme::Scheme;

pub struct Pane {
    pub id: PaneId,
    pub shared: SharedTerm,
    pub feeder: Arc<Mutex<Feeder>>,
    pub input: mpsc::UnboundedSender<PaneInput>,
    pub status: Arc<Mutex<PaneStatus>>,
    pub view: TermView,
    /// Bumped on every reconnect so the previous connection's tasks stop touching the pane.
    pub generation: Arc<AtomicU64>,
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
    /// The saved bookmarks (secrets are only offered for saving for those).
    pub store: Option<&'a crate::bookmarks::BookmarkStore>,
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
    /// When keys were sent with no reply since, and how much had been received then.
    input_watch: Option<(std::time::Instant, u64)>,
    /// Port forward problems already reported.
    forward_errors_seen: Vec<String>,
}

/// Text typed into a password/passphrase prompt (kept until submitted).
#[derive(Default)]
pub struct PromptInput {
    pub text: String,
    pub save: bool,
    pub focus: bool,
    /// The prompt has been drawn at least once (keys from earlier frames can't answer it).
    pub armed: bool,
    /// When the current prompt first appeared.
    pub shown_at: Option<std::time::Instant>,
}

/// How long typed keys may go without any reply before the connection is checked. Shells
/// echo within milliseconds; even keys that aren't echoed (a password) only cost a ping.
const NO_REPLY_AFTER: std::time::Duration = std::time::Duration::from_secs(4);

/// Closes `conn` once the cancelled `pending` transfers have removed their partial files
/// (a few seconds at most).
fn disconnect_after(
    rt: &tokio::runtime::Handle,
    conn: Connection,
    pending: Vec<Arc<crate::sftp::transfer::Transfer>>,
) {
    if pending.is_empty() {
        conn.disconnect();
        return;
    }
    rt.spawn(async move {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while pending.iter().any(|t| !t.is_finished()) && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        conn.disconnect();
    });
}

fn spec(bookmark: &Bookmark, env: &Env) -> ConnectSpec {
    // The jump hosts are saved connections (kept consistent by the store).
    let jumps = env
        .store
        .and_then(|s| s.jump_chain(bookmark).ok())
        .map(|chain| chain.into_iter().cloned().collect())
        .unwrap_or_default();
    ConnectSpec {
        bookmark: bookmark.clone(),
        jumps,
        keepalive_secs: env.settings.keepalive_secs,
        known_hosts: env.known_hosts.clone(),
        save_secrets: env.store.is_some_and(|s| s.bookmark(bookmark.id).is_some()),
    }
}

fn new_pane(id: PaneId, size: GridSize, env: &Env) -> (Pane, mpsc::UnboundedReceiver<PaneInput>) {
    let config = tcore::term_config(
        env.settings.scrollback,
        env.settings.cursor_style,
        env.settings.cursor_blink,
    );
    let (shared, events) = tcore::new_terminal(config, size, QueryPalette::from(env.scheme));
    let feeder = Arc::new(Mutex::new(Feeder::new(shared.clone(), events)));
    let (tx, rx) = mpsc::unbounded_channel();
    let pane = Pane {
        id,
        shared,
        feeder,
        input: tx,
        status: Arc::new(Mutex::new(PaneStatus::Starting)),
        view: TermView::new(super::terminal::pane_widget_id(id)),
        generation: Arc::new(AtomicU64::new(0)),
    };
    (pane, rx)
}

/// The remote working directory from a window title such as `user@host: ~/src` (the title
/// Debian/Ubuntu/Fedora shells set by default), if it looks like one.
pub fn cwd_from_title(title: &str) -> Option<String> {
    let after_at = &title[title.find('@')? + 1..];
    let (_, rest) = after_at.split_once(':')?;
    let path = rest.trim();
    (path == "~" || path.starts_with("~/") || path.starts_with('/')).then(|| path.to_string())
}

fn pane_start(
    pane: &Pane,
    rx: mpsc::UnboundedReceiver<PaneInput>,
    bookmark: &Bookmark,
    first: bool,
    start_dir: Option<String>,
) -> PaneStart {
    let (cols, rows, px_w, px_h) = {
        let w = *pane.shared.window.lock();
        (
            w.num_cols as u32,
            w.num_lines as u32,
            w.num_cols as u32 * w.cell_width as u32,
            w.num_lines as u32 * w.cell_height as u32,
        )
    };
    PaneStart {
        feeder: pane.feeder.clone(),
        input_tx: pane.input.clone(),
        input_rx: rx,
        cols,
        rows,
        px_w,
        px_h,
        start_dir: start_dir.unwrap_or_else(|| bookmark.start_dir.clone()),
        startup_command: if first {
            bookmark.startup_command.clone()
        } else {
            String::new()
        },
        status: pane.status.clone(),
        generation: pane.generation.clone(),
        gen_id: pane.generation.load(Ordering::Relaxed),
    }
}

impl Tab {
    pub fn open(id: u64, pane_id: PaneId, bookmark: Bookmark, size: GridSize, env: &Env) -> Self {
        let conn = Connection::start(env.rt, env.ctx.clone(), spec(&bookmark, env));
        let (pane, rx) = new_pane(pane_id, size, env);
        conn.open_shell(pane_start(&pane, rx, &bookmark, true, None));
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
            input_watch: None,
            forward_errors_seen: Vec::new(),
        }
    }

    /// Forwards that could not start since last asked (a port in use, refused by the server),
    /// each reported once.
    pub fn new_forward_errors(&mut self) -> Vec<String> {
        let failed: Vec<String> = self
            .conn
            .shared
            .lock()
            .forwards
            .iter()
            .filter(|f| !f.listening)
            .filter_map(|f| f.error.clone())
            .collect();
        let new: Vec<String> = failed
            .iter()
            .filter(|e| !self.forward_errors_seen.contains(e))
            .cloned()
            .collect();
        self.forward_errors_seen.extend(new.iter().cloned());
        new
    }

    /// Keys were sent to the server: if nothing comes back for a while, the connection is
    /// checked (a network that went away without a word).
    pub fn note_input(&mut self) {
        if self.input_watch.is_none() && self.state().is_connected() {
            self.input_watch = Some((std::time::Instant::now(), self.conn.received()));
        }
    }

    /// Checks the connection once typed keys got no reply for [`NO_REPLY_AFTER`]. Returns when
    /// to look again.
    pub fn watch_replies(&mut self) -> Option<std::time::Duration> {
        let (since, received) = self.input_watch?;
        if self.conn.received() != received || !self.state().is_connected() {
            self.input_watch = None;
            return None;
        }
        match NO_REPLY_AFTER.checked_sub(since.elapsed()) {
            Some(left) if !left.is_zero() => Some(left),
            _ => {
                self.input_watch = None;
                self.conn.probe(crate::ssh::types::ProbeReason::NoReply);
                None
            }
        }
    }

    /// Opens an SFTP panel on this connection, starting in the bookmark's start folders
    /// (the local side falls back to the last folder used).
    pub fn new_sftp_panel(&self, last_local_dir: Option<&str>) -> SftpPanel {
        let local = self.bookmark.local_start_dir.trim();
        let local = if local.is_empty() {
            last_local_dir.map(str::to_string)
        } else {
            Some(crate::paths::expand_tilde(local).display().to_string())
        };
        SftpPanel::new(
            self.conn.open_sftp(),
            &self.bookmark.start_dir,
            local.as_deref(),
        )
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
        let size = self
            .focused_pane()
            .and_then(|p| p.view.grid)
            .unwrap_or(GridSize { cols: 80, rows: 24 });
        // Open the new shell where the current one is (when the shell reports its folder in
        // the window title), otherwise in the bookmark's start folder.
        let cwd = self
            .focused_pane()
            .and_then(Pane::title)
            .and_then(|t| cwd_from_title(&t));
        let (pane, rx) = new_pane(new_id, size, env);
        if self.state().is_closed() {
            *pane.status.lock() = PaneStatus::Failed(crate::i18n::t("Not connected.").into());
            tcore::end_session(
                &mut pane.feeder.lock(),
                crate::i18n::t("[Connection closed. Press Enter or ⌘R to reconnect.]"),
            );
        } else {
            self.conn
                .open_shell(pane_start(&pane, rx, &self.bookmark, false, cwd));
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
        let pending = self.cancel_transfers();
        let old = std::mem::replace(
            &mut self.conn,
            Connection::start(env.rt, env.ctx.clone(), spec(&self.bookmark, env)),
        );
        disconnect_after(env.rt, old, pending);
        self.sftp = None;
        self.forward_errors_seen.clear();
        let bookmark = self.bookmark.clone();
        for (i, pane) in self.panes.iter_mut().enumerate() {
            let (tx, rx) = mpsc::unbounded_channel();
            pane.input = tx;
            // New generation and a fresh status: the old connection's tasks may still finish
            // and must not mark the reconnected pane as ended.
            pane.generation.fetch_add(1, Ordering::Relaxed);
            pane.status = Arc::new(Mutex::new(PaneStatus::Starting));
            tcore::end_session(
                &mut pane.feeder.lock(),
                &format!("— {} —", crate::i18n::t("reconnecting")),
            );
            self.conn
                .open_shell(pane_start(pane, rx, &bookmark, i == 0, None));
        }
    }

    /// Starts a new shell in an ended pane on the current connection, leaving the other panes
    /// alone. Returns false when the connection is gone (the whole tab must reconnect).
    pub fn restart_pane(&mut self, id: PaneId) -> bool {
        if !self.state().is_connected() {
            return false;
        }
        let first = self.panes.first().is_some_and(|p| p.id == id);
        let bookmark = self.bookmark.clone();
        let Some(pane) = self.panes.iter_mut().find(|p| p.id == id) else {
            return false;
        };
        if pane.is_live() {
            return true;
        }
        let (tx, rx) = mpsc::unbounded_channel();
        pane.input = tx;
        pane.generation.fetch_add(1, Ordering::Relaxed);
        pane.status = Arc::new(Mutex::new(PaneStatus::Starting));
        self.conn
            .open_shell(pane_start(pane, rx, &bookmark, first, None));
        true
    }

    pub fn disconnect(&self) {
        self.conn.disconnect();
    }

    /// Cancels this tab's running transfers; returns the ones still cleaning up.
    pub fn cancel_transfers(&self) -> Vec<Arc<crate::sftp::transfer::Transfer>> {
        let Some(panel) = &self.sftp else {
            return Vec::new();
        };
        panel.transfers.cancel_all();
        panel
            .transfers
            .items
            .iter()
            .filter(|t| !t.is_finished())
            .cloned()
            .collect()
    }

    /// Closes the tab. Running transfers are cancelled first and get a few seconds to remove
    /// their partial files (on the server too) before the connection closes.
    pub fn close(self, rt: &tokio::runtime::Handle) {
        let pending = self.cancel_transfers();
        disconnect_after(rt, self.conn, pending);
    }

    /// Progress of the SFTP transfers in this tab, while any run.
    pub fn transfer_progress(&self) -> Option<crate::sftp::transfer::Summary> {
        self.sftp
            .as_ref()
            .and_then(|p| p.transfer_summary())
            .filter(|s| s.running())
    }

    /// A remote file open in the editor with unsaved changes.
    pub fn unsaved_file(&self) -> Option<String> {
        self.sftp.as_ref().and_then(|p| p.unsaved_file())
    }

    /// Answers the login prompt on screen, if any: whether to trust the host key, or the secret
    /// (`None` cancels). The typed input is cleared either way.
    pub fn answer_prompt(&mut self, trust: bool, secret: Option<SecretAnswer>) {
        let prompt = self.conn.shared.lock().prompt.take();
        match prompt {
            Some(Prompt::HostKey { reply, .. }) => {
                let _ = reply.send(trust);
            }
            Some(Prompt::Secret { reply, .. }) => {
                let _ = reply.send(secret);
            }
            None => {}
        }
        self.prompt = PromptInput::default();
    }

    /// SFTP transfers that have not finished yet.
    pub fn running_transfers(&self) -> usize {
        self.sftp.as_ref().map_or(0, |p| {
            p.transfers
                .items
                .iter()
                .filter(|t| !t.is_finished())
                .count()
        })
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
            let config = tcore::term_config(
                settings.scrollback,
                settings.cursor_style,
                settings.cursor_blink,
            );
            p.shared.term.lock().set_options(config);
            *p.shared.palette.lock() = QueryPalette::from(scheme);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::cwd_from_title;

    #[test]
    fn cwd_from_shell_titles() {
        assert_eq!(
            cwd_from_title("ubuntu@VM-2-193: ~/projects").as_deref(),
            Some("~/projects")
        );
        assert_eq!(
            cwd_from_title("root@web:/var/www").as_deref(),
            Some("/var/www")
        );
        assert_eq!(cwd_from_title("user@host: ~").as_deref(), Some("~"));
        assert_eq!(cwd_from_title("vim notes.txt"), None);
        assert_eq!(cwd_from_title("user@host: htop"), None);
    }
}
