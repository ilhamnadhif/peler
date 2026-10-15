//! Font setup. System font files are memory-mapped rather than read, so a 25 MB CJK fallback
//! costs only the pages actually used for glyphs that appear on screen.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use eframe::egui::{self, FontData, FontDefinitions, FontFamily};
use parking_lot::Mutex;

use crate::settings::FontChoice;

pub const TERM_REGULAR: &str = "term-regular";
pub const TERM_BOLD: &str = "term-bold";
pub const TERM_ITALIC: &str = "term-italic";
pub const TERM_BOLD_ITALIC: &str = "term-bold-italic";

/// A selectable terminal font: file and face indices (regular, bold, italic, bold italic).
#[derive(Clone, Debug)]
pub struct FontPreset {
    pub name: &'static str,
    pub path: Option<&'static str>,
    pub faces: [Option<(Option<&'static str>, u32)>; 4],
}

pub const PRESETS: &[FontPreset] = &[
    FontPreset {
        name: "Menlo",
        path: Some("/System/Library/Fonts/Menlo.ttc"),
        faces: [Some((None, 0)), Some((None, 1)), Some((None, 2)), Some((None, 3))],
    },
    FontPreset {
        name: "SF Mono",
        path: Some("/System/Library/Fonts/SFNSMono.ttf"),
        faces: [Some((None, 0)), None, Some((Some("/System/Library/Fonts/SFNSMonoItalic.ttf"), 0)), None],
    },
    FontPreset { name: "Monaco", path: Some("/System/Library/Fonts/Monaco.ttf"), faces: [Some((None, 0)), None, None, None] },
    FontPreset {
        name: "Courier New",
        path: Some("/System/Library/Fonts/Supplemental/Courier New.ttf"),
        faces: [
            Some((None, 0)),
            Some((Some("/System/Library/Fonts/Supplemental/Courier New Bold.ttf"), 0)),
            Some((Some("/System/Library/Fonts/Supplemental/Courier New Italic.ttf"), 0)),
            Some((Some("/System/Library/Fonts/Supplemental/Courier New Bold Italic.ttf"), 0)),
        ],
    },
    FontPreset { name: "Andale Mono", path: Some("/System/Library/Fonts/Supplemental/Andale Mono.ttf"), faces: [Some((None, 0)), None, None, None] },
    FontPreset { name: "Hack (built-in)", path: None, faces: [None, None, None, None] },
];

const CJK_FALLBACKS: &[(&str, &str)] = &[
    ("cjk-ja", "/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc"),
    ("cjk-zh", "/System/Library/Fonts/Hiragino Sans GB.ttc"),
    ("cjk-ko", "/System/Library/Fonts/AppleSDGothicNeo.ttc"),
];
const SYMBOLS: (&str, &str) = ("symbols", "/System/Library/Fonts/Apple Symbols.ttf");
const UI_FONT: (&str, &str) = ("ui-sf", "/System/Library/Fonts/SFNS.ttf");

/// Memory-maps a font file once for the life of the process.
fn mapped(path: &Path) -> Option<&'static [u8]> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, &'static [u8]>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut cache = cache.lock();
    if let Some(bytes) = cache.get(path) {
        return Some(bytes);
    }
    let file = std::fs::File::open(path).ok()?;
    // SAFETY: system and user font files are not modified while the app runs; the mapping is
    // read-only and lives for the rest of the process.
    let map = unsafe { memmap2::Mmap::map(&file) }.ok()?;
    let bytes: &'static [u8] = Box::leak(Box::new(map));
    cache.insert(path.to_path_buf(), bytes);
    Some(bytes)
}

fn add_face(defs: &mut FontDefinitions, key: &str, path: &Path, index: u32) -> bool {
    let Some(bytes) = mapped(path) else { return false };
    let mut data = FontData::from_static(bytes);
    data.index = index;
    defs.font_data.insert(key.to_string(), Arc::new(data));
    true
}

/// Finds the preset matching a saved choice (by name), if any.
pub fn preset(choice: &FontChoice) -> Option<&'static FontPreset> {
    PRESETS.iter().find(|p| p.name == choice.name)
}

/// Builds the font set: the terminal font (four faces where available), the UI font, and
/// fallbacks for symbols, CJK and emoji.
pub fn definitions(choice: &FontChoice, load_cjk: bool) -> FontDefinitions {
    let mut defs = FontDefinitions::default();
    // egui's bundled fonts: Hack, Ubuntu-Light, NotoEmoji-Regular, emoji-icon-font.
    let emoji: Vec<String> = ["NotoEmoji-Regular", "emoji-icon-font"].iter().map(|s| s.to_string()).collect();

    let mut fallbacks: Vec<String> = Vec::new();
    if add_face(&mut defs, SYMBOLS.0, Path::new(SYMBOLS.1), 0) {
        fallbacks.push(SYMBOLS.0.into());
    }
    if load_cjk {
        for (key, path) in CJK_FALLBACKS {
            if add_face(&mut defs, key, Path::new(path), 0) {
                fallbacks.push(key.to_string());
            }
        }
    }
    fallbacks.extend(emoji.iter().cloned());

    // Terminal faces. Missing faces fall back to the regular one (egui then synthesizes italics
    // and the renderer fakes bold).
    let mut faces: [Option<String>; 4] = [None, None, None, None];
    let keys = [TERM_REGULAR, TERM_BOLD, TERM_ITALIC, TERM_BOLD_ITALIC];
    match (preset(choice), &choice.path) {
        (Some(p), _) => {
            if let Some(base) = p.path {
                for (i, face) in p.faces.iter().enumerate() {
                    if let Some((file, index)) = face {
                        let path = Path::new(file.unwrap_or(base));
                        if add_face(&mut defs, keys[i], path, *index) {
                            faces[i] = Some(keys[i].to_string());
                        }
                    }
                }
            }
        }
        (None, Some(path)) => {
            if add_face(&mut defs, TERM_REGULAR, &crate::paths::expand_tilde(path), choice.index) {
                faces[0] = Some(TERM_REGULAR.to_string());
            }
        }
        _ => {}
    }
    let regular = faces[0].clone().unwrap_or_else(|| "Hack".to_string());
    for (i, key) in keys.iter().enumerate() {
        let primary = faces[i].clone().unwrap_or_else(|| regular.clone());
        let mut family = vec![primary];
        family.push("Hack".into());
        family.extend(fallbacks.iter().cloned());
        family.dedup();
        defs.families.insert(FontFamily::Name((*key).into()), family);
    }
    let mut mono = vec![regular, "Hack".to_string()];
    mono.extend(fallbacks.iter().cloned());
    mono.dedup();
    defs.families.insert(FontFamily::Monospace, mono);

    // UI: the macOS system font when available.
    let mut proportional = Vec::new();
    if add_face(&mut defs, UI_FONT.0, Path::new(UI_FONT.1), 0) {
        proportional.push(UI_FONT.0.to_string());
    }
    proportional.push("Ubuntu-Light".into());
    proportional.extend(fallbacks.iter().cloned());
    defs.families.insert(FontFamily::Proportional, proportional);
    defs
}

/// Whether a terminal style has a real face (otherwise the renderer emulates it).
pub fn has_real_face(choice: &FontChoice, bold: bool, italic: bool) -> bool {
    let idx = match (bold, italic) {
        (false, false) => 0,
        (true, false) => 1,
        (false, true) => 2,
        (true, true) => 3,
    };
    preset(choice).is_some_and(|p| p.path.is_some() && p.faces[idx].is_some())
}

pub fn apply(ctx: &egui::Context, choice: &FontChoice, load_cjk: bool) {
    ctx.set_fonts(definitions(choice, load_cjk));
}

/// The saved font, or Menlo when nothing is chosen yet.
pub fn effective_choice(choice: &FontChoice) -> FontChoice {
    if choice.name.is_empty() && choice.path.is_none() {
        FontChoice { name: "Menlo".into(), path: None, index: 0 }
    } else {
        choice.clone()
    }
}
