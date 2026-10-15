//! User preferences, stored in `settings.json`. Every field has a default so the app works with
//! no settings file and old files keep loading after new fields are added.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::i18n::Language;
use crate::paths;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeMode {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorStyle {
    #[default]
    Block,
    Beam,
    Underline,
}

/// A command that can be sent to the active terminal from the toolbar.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuickCommand {
    pub id: Uuid,
    pub name: String,
    pub command: String,
    /// Send Enter after the command.
    #[serde(default = "yes")]
    pub run: bool,
}

impl QuickCommand {
    pub fn new(name: &str, command: &str) -> Self {
        Self {
            id: Uuid::new_v4(),
            name: name.into(),
            command: command.into(),
            run: true,
        }
    }
}

fn yes() -> bool {
    true
}

/// Monospace font for the terminal. `path == None` means the bundled Hack font.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FontChoice {
    pub name: String,
    pub path: Option<String>,
    #[serde(default)]
    pub index: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub language: Language,
    pub theme: ThemeMode,
    /// Terminal colour scheme used with the dark / light app theme.
    pub scheme_dark: String,
    pub scheme_light: String,
    pub font: FontChoice,
    pub font_size: f32,
    /// Scrollback lines kept per terminal.
    pub scrollback: usize,
    pub cursor_style: CursorStyle,
    pub cursor_blink: bool,
    /// Treat Option as Meta (sends ESC-prefixed keys) instead of typing special characters.
    pub option_as_meta: bool,
    pub copy_on_select: bool,
    /// Seconds between SSH keepalive probes; 0 disables keepalive.
    pub keepalive_secs: u64,
    pub confirm_close_connected: bool,
    pub load_cjk_font: bool,
    pub sidebar_visible: bool,
    pub sidebar_width: f32,
    pub quick_commands: Vec<QuickCommand>,
    pub last_local_dir: Option<String>,
    /// Window position and size in points (x, y, width, height), restored on launch.
    pub window: Option<[f32; 4]>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            language: Language::System,
            theme: ThemeMode::System,
            scheme_dark: "Peler Dark".into(),
            scheme_light: "Peler Light".into(),
            font: FontChoice::default(),
            font_size: 13.0,
            scrollback: 10_000,
            cursor_style: CursorStyle::Block,
            cursor_blink: false,
            option_as_meta: false,
            copy_on_select: false,
            keepalive_secs: 30,
            confirm_close_connected: true,
            load_cjk_font: true,
            sidebar_visible: true,
            sidebar_width: 260.0,
            quick_commands: vec![
                QuickCommand::new("Disk usage", "df -h"),
                QuickCommand::new("Memory", "free -h"),
                QuickCommand::new("Top processes", "top"),
            ],
            last_local_dir: None,
            window: None,
        }
    }
}

impl Settings {
    pub const MIN_FONT: f32 = 8.0;
    pub const MAX_FONT: f32 = 36.0;
    pub const MAX_SCROLLBACK: usize = 200_000;

    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read(path) {
            Ok(data) => {
                let mut s: Settings = serde_json::from_slice(&data).with_context(|| {
                    crate::i18n::tf("{} is damaged", &[&path.display().to_string()])
                })?;
                s.clamp();
                Ok(s)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).with_context(|| {
                crate::i18n::tf("Could not read {}", &[&path.display().to_string()])
            }),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_vec_pretty(self)?;
        paths::write_private(path, &json)
            .with_context(|| crate::i18n::tf("Could not save {}", &[&path.display().to_string()]))
    }

    /// The saved window frame, if it is a usable size.
    pub fn window_frame(&self) -> Option<[f32; 4]> {
        self.window.filter(|[x, y, w, h]| {
            [x, y, w, h].iter().all(|v| v.is_finite())
                && (640.0..=16384.0).contains(w)
                && (400.0..=16384.0).contains(h)
        })
    }

    pub fn clamp(&mut self) {
        self.font_size = self.font_size.clamp(Self::MIN_FONT, Self::MAX_FONT);
        self.scrollback = self.scrollback.min(Self::MAX_SCROLLBACK);
        self.sidebar_width = self.sidebar_width.clamp(160.0, 600.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_file_uses_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, r#"{"font_size": 99, "scrollback": 5000}"#).unwrap();
        let s = Settings::load(&path).unwrap();
        assert_eq!(s.font_size, Settings::MAX_FONT);
        assert_eq!(s.scrollback, 5000);
        assert_eq!(s.keepalive_secs, 30);
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), s);
    }
}
