//! One terminal: an alacritty `Term` shared between the SSH reader task (which feeds it bytes)
//! and the UI (which draws it and sends input).
//!
//! Events raised by the parser are queued through a channel and handled by the reader right
//! after `advance` (outside the lock): replies such as device attributes, cursor position and
//! colour queries go straight back to the remote even when the window is not repainting.

use std::sync::Arc;
use std::sync::mpsc;
use std::time::Instant;

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::{CursorShape, CursorStyle, Processor, Rgb};
use eframe::egui::Color32;
use parking_lot::Mutex;

use super::scheme::Scheme;
use crate::settings::CursorStyle as CursorPref;

pub const MIN_COLS: usize = 2;
pub const MIN_ROWS: usize = 1;

#[derive(Clone)]
pub struct Listener(mpsc::Sender<Event>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        // Never touch the Term here: `advance` holds its lock while emitting events.
        let _ = self.0.send(event);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridSize {
    pub cols: usize,
    pub rows: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// State the UI reads that the parser produces (title, bell, OSC 52 copies).
#[derive(Default)]
pub struct UiNotes {
    pub title: Option<String>,
    pub bell_at: Option<Instant>,
    pub clipboard: Option<String>,
    /// Set whenever new output arrived; the UI uses it to mark inactive tabs.
    pub activity: bool,
    /// "Clear Scrollback" was chosen on the alternate screen: clear when it is left.
    pub clear_history_pending: bool,
}

/// Colours used to answer OSC 4/10/11 queries (kept in sync with the active scheme).
#[derive(Clone)]
pub struct QueryPalette {
    pub ansi: [Color32; 16],
    pub fg: Color32,
    pub bg: Color32,
    pub cursor: Color32,
}

impl From<&Scheme> for QueryPalette {
    fn from(s: &Scheme) -> Self {
        Self {
            ansi: s.ansi,
            fg: s.foreground,
            bg: s.background,
            cursor: s.cursor,
        }
    }
}

pub struct TermShared {
    pub term: FairMutex<Term<Listener>>,
    pub notes: Mutex<UiNotes>,
    /// Cell size in pixels and grid size, for CSI 14 t and the SSH window-change request.
    pub window: Mutex<WindowSize>,
    pub palette: Mutex<QueryPalette>,
}

pub type SharedTerm = Arc<TermShared>;

pub fn term_config(scrollback: usize, cursor: CursorPref, blink: bool) -> Config {
    let shape = match cursor {
        CursorPref::Block => CursorShape::Block,
        CursorPref::Beam => CursorShape::Beam,
        CursorPref::Underline => CursorShape::Underline,
    };
    Config {
        scrolling_history: scrollback,
        default_cursor_style: CursorStyle {
            shape,
            blinking: blink,
        },
        ..Config::default()
    }
}

/// Creates a terminal and the receiving end of its event queue (owned by the reader).
pub fn new_terminal(
    config: Config,
    size: GridSize,
    palette: QueryPalette,
) -> (SharedTerm, mpsc::Receiver<Event>) {
    let (tx, rx) = mpsc::channel();
    let term = Term::new(config, &size, Listener(tx));
    let shared = Arc::new(TermShared {
        term: FairMutex::new(term),
        notes: Mutex::new(UiNotes::default()),
        window: Mutex::new(WindowSize {
            num_lines: size.rows as u16,
            num_cols: size.cols as u16,
            cell_width: 8,
            cell_height: 16,
        }),
        palette: Mutex::new(palette),
    });
    (shared, rx)
}

fn rgb(c: Color32) -> Rgb {
    Rgb {
        r: c.r(),
        g: c.g(),
        b: c.b(),
    }
}

/// Feeds remote output into a terminal and answers the parser's queries.
pub struct Feeder {
    pub shared: SharedTerm,
    parser: Processor,
    events: mpsc::Receiver<Event>,
}

impl Feeder {
    pub fn new(shared: SharedTerm, events: mpsc::Receiver<Event>) -> Self {
        Self {
            shared,
            parser: Processor::new(),
            events,
        }
    }

    /// Parses `bytes`; returns the replies to send to the remote and whether to repaint.
    pub fn feed(&mut self, bytes: &[u8]) -> (Vec<u8>, bool) {
        {
            let mut term = self.shared.term.lock();
            self.parser.advance(&mut *term, bytes);
            if !term
                .mode()
                .contains(alacritty_terminal::term::TermMode::ALT_SCREEN)
            {
                let mut notes = self.shared.notes.lock();
                if std::mem::take(&mut notes.clear_history_pending) {
                    term.grid_mut().clear_history();
                }
            }
        }
        let repaint = self.parser.sync_bytes_count() < bytes.len();
        if repaint {
            self.shared.notes.lock().activity = true;
        }
        (self.drain_events(), repaint)
    }

    /// Deadline of a pending synchronized update (DEC 2026), if any.
    pub fn sync_deadline(&self) -> Option<Instant> {
        self.parser.sync_timeout().sync_timeout()
    }

    /// Flushes a synchronized update whose deadline passed.
    pub fn flush_sync(&mut self) -> Vec<u8> {
        {
            let mut term = self.shared.term.lock();
            self.parser.stop_sync(&mut *term);
        }
        self.drain_events()
    }

    fn drain_events(&mut self) -> Vec<u8> {
        let mut reply = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::PtyWrite(s) => reply.extend_from_slice(s.as_bytes()),
                Event::ColorRequest(index, format) => {
                    let custom = self.shared.term.lock().colors()[index];
                    let color = custom.unwrap_or_else(|| {
                        let p = self.shared.palette.lock();
                        rgb(match index {
                            0..=15 => p.ansi[index],
                            16..=255 => super::scheme::xterm_256(index as u8),
                            256 => p.fg,
                            257 => p.bg,
                            _ => p.cursor,
                        })
                    });
                    reply.extend_from_slice(format(color).as_bytes());
                }
                Event::TextAreaSizeRequest(format) => {
                    let ws = *self.shared.window.lock();
                    reply.extend_from_slice(format(ws).as_bytes());
                }
                Event::Title(t) => self.shared.notes.lock().title = Some(t),
                Event::ResetTitle => self.shared.notes.lock().title = None,
                Event::Bell => self.shared.notes.lock().bell_at = Some(Instant::now()),
                Event::ClipboardStore(_, text) => self.shared.notes.lock().clipboard = Some(text),
                _ => {}
            }
        }
        reply
    }
}

/// Resizes the grid (clamped to the minimum) and records the pixel size. Returns whether the
/// grid size changed, i.e. the remote must be told.
pub fn resize(shared: &TermShared, size: GridSize, cell_w: f32, cell_h: f32) -> bool {
    let size = GridSize {
        cols: size.cols.max(MIN_COLS),
        rows: size.rows.max(MIN_ROWS),
    };
    let mut window = shared.window.lock();
    window.cell_width = cell_w.round() as u16;
    window.cell_height = cell_h.round() as u16;
    let mut term = shared.term.lock();
    if term.columns() == size.cols && term.screen_lines() == size.rows {
        return false;
    }
    term.resize(size);
    window.num_cols = size.cols as u16;
    window.num_lines = size.rows as u16;
    true
}

/// Writes a local status line (e.g. "connection closed") into the terminal in dim text.
pub fn write_status(feeder: &mut Feeder, text: &str) {
    let line = format!("\r\n\x1b[0;2m{text}\x1b[0m\r\n");
    let _ = feeder.feed(line.as_bytes());
}

/// Ends a session in this terminal before showing `text`: shows any pending synchronized
/// output, drops half-received escape sequences, and switches off the modes a full-screen
/// program may have left on (alternate screen, mouse and focus reporting, bracketed paste,
/// application cursor keys, hidden cursor, scroll region). The last output stays visible and a
/// reconnect starts from a clean terminal.
pub fn end_session(feeder: &mut Feeder, text: &str) {
    {
        let mut term = feeder.shared.term.lock();
        feeder.parser.stop_sync(&mut *term);
    }
    feeder.parser = Processor::new();
    const RESET: &str = "\x1b[?1049l\x1b[r\x1b[?6l\x1b[4l\x1b(B\x1b[?1l\x1b>\x1b[?25h\x1b[?7h\
        \x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l\x1b[?1004l\x1b[?2004l\x1b[0m";
    let _ = feeder.feed(RESET.as_bytes());
    write_status(feeder, text);
    let _ = feeder.drain_events();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::scheme::SCHEMES;
    use alacritty_terminal::index::{Column, Line};

    fn make() -> Feeder {
        let (shared, rx) = new_terminal(
            Config::default(),
            GridSize { cols: 40, rows: 10 },
            (&SCHEMES[0]).into(),
        );
        Feeder::new(shared, rx)
    }

    fn row_text(f: &Feeder, line: i32) -> String {
        let term = f.shared.term.lock();
        let row = &term.grid()[Line(line)];
        (0..term.columns())
            .map(|c| row[Column(c)].c)
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    #[test]
    fn feeds_text_and_answers_queries() {
        let mut f = make();
        let (reply, repaint) = f.feed(b"\x1b]0;my title\x07hello\x1b[6n\x1b[c\x1b]11;?\x07");
        assert!(repaint);
        assert_eq!(row_text(&f, 0), "hello");
        let reply = String::from_utf8(reply).unwrap();
        assert!(reply.contains("\x1b[1;6R"), "{reply:?}");
        assert!(reply.contains("\x1b[?6c"));
        assert!(reply.contains("\x1b]11;rgb:1717/1919/1e1e"), "{reply:?}");
        assert_eq!(f.shared.notes.lock().title.as_deref(), Some("my title"));
    }

    #[test]
    fn resize_and_sync_updates() {
        let mut f = make();
        assert!(resize(&f.shared, GridSize { cols: 20, rows: 5 }, 8.0, 16.0));
        assert!(!resize(
            &f.shared,
            GridSize { cols: 20, rows: 5 },
            8.0,
            16.0
        ));
        let (reply, _) = f.feed(b"\x1b[14t");
        assert_eq!(reply, b"\x1b[4;80;160t");
        f.feed(b"\x1b[?2026h");
        let (_, repaint) = f.feed(b"synced");
        assert!(!repaint, "buffered by a synchronized update");
        assert!(f.sync_deadline().is_some());
        f.flush_sync();
        assert_eq!(row_text(&f, 0), "synced");
    }

    #[test]
    fn end_session_resets_modes() {
        use alacritty_terminal::term::TermMode;
        let mut f = make();
        // A full-screen app with mouse reporting, cut off in the middle of an OSC sequence.
        f.feed(
            b"\x1b[?1049h\x1b[?1000h\x1b[?1006h\x1b[?1004h\x1b[?2004h\x1b[?25l\x1b[?1h\x1b]0;half",
        );
        end_session(&mut f, "closed");
        let mode = *f.shared.term.lock().mode();
        assert!(!mode.contains(TermMode::ALT_SCREEN));
        assert!(!mode.intersects(TermMode::MOUSE_MODE | TermMode::FOCUS_IN_OUT));
        assert!(!mode.contains(TermMode::BRACKETED_PASTE | TermMode::APP_CURSOR));
        assert!(mode.contains(TermMode::SHOW_CURSOR));
        // The status line is not swallowed by the unfinished OSC.
        let term = f.shared.term.lock();
        let text: String = (0..term.screen_lines() as i32)
            .map(|l| {
                let row = &term.grid()[Line(l)];
                (0..term.columns())
                    .map(|c| row[Column(c)].c)
                    .collect::<String>()
            })
            .collect();
        assert!(text.contains("closed"), "{text:?}");
    }
}
