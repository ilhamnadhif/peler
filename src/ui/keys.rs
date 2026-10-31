//! Keyboard shortcuts.
//!
//! On macOS the menu bar handles them (its key equivalents) and shows them. Windows and Linux
//! have no menu bar here, so the app handles them itself, with Ctrl+Shift (as GNOME Terminal and
//! Windows Terminal do): Ctrl+letter keeps reaching the remote shell, so Ctrl+C, Ctrl+D, Ctrl+R
//! or Ctrl+W work there as in any terminal.

use eframe::egui::{Key, KeyboardShortcut, Modifiers};

use super::actions::Action;

/// A command with a shortcut.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shortcut {
    QuickConnect,
    NewConnection,
    NewTab,
    Close,
    Reconnect,
    Export,
    Copy,
    Paste,
    ClearScrollback,
    Sidebar,
    Sftp,
    SplitRight,
    SplitDown,
    NextPane,
    PrevPane,
    NextTab,
    PrevTab,
    Bigger,
    Smaller,
    ActualSize,
    Settings,
    Quit,
    FullScreen,
    /// The text editor's Save.
    Save,
    /// Save & Connect in the connection editor.
    SaveAndConnect,
}

const CTRL_SHIFT: Modifiers = Modifiers::COMMAND.plus(Modifiers::SHIFT);
const CTRL_ALT_SHIFT: Modifiers = CTRL_SHIFT.plus(Modifiers::ALT);

impl Shortcut {
    /// Every command the app handles itself on Windows and Linux. A shortcut whose keys include
    /// another's comes first (egui ignores extra Shift and Alt when matching).
    pub const HANDLED: [Shortcut; 21] = [
        Shortcut::SplitDown,
        Shortcut::QuickConnect,
        Shortcut::NewConnection,
        Shortcut::NewTab,
        Shortcut::Close,
        Shortcut::Reconnect,
        Shortcut::Export,
        Shortcut::ClearScrollback,
        Shortcut::Sidebar,
        Shortcut::Sftp,
        Shortcut::SplitRight,
        Shortcut::NextPane,
        Shortcut::PrevPane,
        Shortcut::PrevTab,
        Shortcut::NextTab,
        Shortcut::Bigger,
        Shortcut::Smaller,
        Shortcut::ActualSize,
        Shortcut::Settings,
        Shortcut::Quit,
        Shortcut::FullScreen,
    ];

    /// How the shortcut is written on macOS.
    fn mac(self) -> &'static str {
        match self {
            Shortcut::QuickConnect => "⌘K",
            Shortcut::NewConnection => "⌘N",
            Shortcut::NewTab => "⌘T",
            Shortcut::Close => "⌘W",
            Shortcut::Reconnect => "⌘R",
            Shortcut::Export => "⇧⌘E",
            Shortcut::Copy => "⌘C",
            Shortcut::Paste => "⌘V",
            Shortcut::ClearScrollback => "⌥⌘K",
            Shortcut::Sidebar => "⌘B",
            Shortcut::Sftp => "⇧⌘F",
            Shortcut::SplitRight => "⌘D",
            Shortcut::SplitDown => "⇧⌘D",
            Shortcut::NextPane => "⌘]",
            Shortcut::PrevPane => "⌘[",
            Shortcut::NextTab => "⇧⌘]",
            Shortcut::PrevTab => "⇧⌘[",
            Shortcut::Bigger => "⌘=",
            Shortcut::Smaller => "⌘-",
            Shortcut::ActualSize => "⌘0",
            Shortcut::Settings => "⌘,",
            Shortcut::Quit => "⌘Q",
            Shortcut::FullScreen => "⌃⌘F",
            Shortcut::Save => "⌘S",
            Shortcut::SaveAndConnect => "⌘↩",
        }
    }

    /// The keys on Windows and Linux: the first is the one shown, the others also work.
    fn keys(self) -> &'static [(Modifiers, Key)] {
        use Key::*;
        match self {
            Shortcut::QuickConnect => &[(CTRL_SHIFT, K)],
            Shortcut::NewConnection => &[(CTRL_SHIFT, N)],
            Shortcut::NewTab => &[(CTRL_SHIFT, T)],
            Shortcut::Close => &[(CTRL_SHIFT, W)],
            Shortcut::Reconnect => &[(CTRL_SHIFT, R)],
            Shortcut::Export => &[(CTRL_SHIFT, E)],
            Shortcut::Copy => &[(CTRL_SHIFT, C)],
            Shortcut::Paste => &[(CTRL_SHIFT, V)],
            Shortcut::ClearScrollback => &[(CTRL_SHIFT, L)],
            Shortcut::Sidebar => &[(CTRL_SHIFT, B)],
            Shortcut::Sftp => &[(CTRL_SHIFT, F)],
            Shortcut::SplitRight => &[(CTRL_SHIFT, D)],
            Shortcut::SplitDown => &[(CTRL_ALT_SHIFT, D)],
            // With Shift held, the bracket keys type braces on most layouts.
            Shortcut::NextPane => &[(CTRL_SHIFT, CloseBracket), (CTRL_SHIFT, CloseCurlyBracket)],
            Shortcut::PrevPane => &[(CTRL_SHIFT, OpenBracket), (CTRL_SHIFT, OpenCurlyBracket)],
            Shortcut::NextTab => &[(Modifiers::COMMAND, Tab), (Modifiers::COMMAND, PageDown)],
            Shortcut::PrevTab => &[(CTRL_SHIFT, Tab), (Modifiers::COMMAND, PageUp)],
            Shortcut::Bigger => &[(Modifiers::COMMAND, Equals), (Modifiers::COMMAND, Plus)],
            Shortcut::Smaller => &[(Modifiers::COMMAND, Minus)],
            Shortcut::ActualSize => &[(Modifiers::COMMAND, Num0)],
            Shortcut::Settings => &[(Modifiers::COMMAND, Comma)],
            Shortcut::Quit => &[(CTRL_SHIFT, Q)],
            Shortcut::FullScreen => &[(Modifiers::NONE, F11)],
            Shortcut::Save => &[(Modifiers::COMMAND, S)],
            Shortcut::SaveAndConnect => &[(Modifiers::COMMAND, Enter)],
        }
    }

    /// What it does, for the shortcuts the app handles (the rest are handled where they apply).
    pub fn action(self) -> Option<Action> {
        Some(match self {
            Shortcut::QuickConnect => Action::QuickConnect,
            Shortcut::NewConnection => Action::NewBookmark { group: None },
            Shortcut::NewTab => Action::NewTab,
            Shortcut::Close => Action::CloseTabOrPane,
            Shortcut::Reconnect => Action::Reconnect,
            Shortcut::Export => Action::Export(crate::bookmarks::transfer::ExportScope::All),
            Shortcut::Copy => Action::Copy,
            Shortcut::Paste => Action::Paste,
            Shortcut::ClearScrollback => Action::ClearScrollback,
            Shortcut::Sidebar => Action::ToggleSidebar,
            Shortcut::Sftp => Action::ToggleSftp,
            Shortcut::SplitRight => Action::SplitRight,
            Shortcut::SplitDown => Action::SplitDown,
            Shortcut::NextPane => Action::NextPane,
            Shortcut::PrevPane => Action::PrevPane,
            Shortcut::NextTab => Action::NextTab,
            Shortcut::PrevTab => Action::PrevTab,
            Shortcut::Bigger => Action::FontBigger,
            Shortcut::Smaller => Action::FontSmaller,
            Shortcut::ActualSize => Action::FontReset,
            Shortcut::Settings => Action::Settings,
            Shortcut::Quit => Action::Quit,
            Shortcut::FullScreen => Action::ToggleFullScreen,
            Shortcut::Save | Shortcut::SaveAndConnect => return None,
        })
    }

    /// The shortcut as shown to the user: `⇧⌘F` on macOS, `Ctrl+Shift+F` elsewhere.
    pub fn label(self) -> String {
        if cfg!(target_os = "macos") {
            return self.mac().to_string();
        }
        let (mods, key) = self.keys()[0];
        let mut s = String::new();
        if mods.command || mods.ctrl {
            s.push_str("Ctrl+");
        }
        if mods.alt {
            s.push_str("Alt+");
        }
        if mods.shift {
            s.push_str("Shift+");
        }
        s.push_str(match key {
            Key::Enter => "Enter",
            Key::Tab => "Tab",
            Key::Equals => "=",
            Key::Minus => "-",
            Key::Comma => ",",
            Key::OpenBracket => "[",
            Key::CloseBracket => "]",
            other => other.name(),
        });
        s
    }

    /// Whether the shortcut was pressed this frame (Windows and Linux); its key press is
    /// consumed, so the terminal doesn't get it too.
    pub fn consume(self, input: &mut eframe::egui::InputState) -> bool {
        self.keys()
            .iter()
            .any(|(mods, key)| input.consume_shortcut(&KeyboardShortcut::new(*mods, *key)))
    }
}

/// Switching to tab 1–9: ⌘1…⌘9 on macOS, Alt+1…Alt+9 elsewhere (as in GNOME Terminal).
pub fn tab_number_modifiers() -> Modifiers {
    if cfg!(target_os = "macos") {
        Modifiers::COMMAND
    } else {
        Modifiers::ALT
    }
}

/// "⌘1 … ⌘9" or "Alt+1 … Alt+9".
pub fn tab_number_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "⌘1 … ⌘9"
    } else {
        "Alt+1 … Alt+9"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        if cfg!(target_os = "macos") {
            assert_eq!(Shortcut::Sftp.label(), "⇧⌘F");
        } else {
            assert_eq!(Shortcut::Sftp.label(), "Ctrl+Shift+F");
            assert_eq!(Shortcut::SplitDown.label(), "Ctrl+Alt+Shift+D");
            assert_eq!(Shortcut::Settings.label(), "Ctrl+,");
            assert_eq!(Shortcut::FullScreen.label(), "F11");
        }
    }

    /// Each handled shortcut can be reached: no earlier one catches its keys (matching
    /// ignores extra Shift and Alt, so the order matters).
    #[test]
    fn handled_shortcuts_dont_shadow_each_other() {
        let all: Vec<(Shortcut, Modifiers, Key)> = Shortcut::HANDLED
            .iter()
            .flat_map(|s| s.keys().iter().map(move |(m, k)| (*s, *m, *k)))
            .collect();
        for (i, (earlier, me, ke)) in all.iter().enumerate() {
            for (later, ml, kl) in &all[i + 1..] {
                if earlier != later && ke == kl {
                    assert!(!ml.matches_logically(*me), "{earlier:?} catches {later:?}");
                }
            }
        }
    }
}
