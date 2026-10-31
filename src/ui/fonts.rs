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
        faces: [
            Some((None, 0)),
            Some((None, 1)),
            Some((None, 2)),
            Some((None, 3)),
        ],
    },
    FontPreset {
        name: "SF Mono",
        path: Some("/System/Library/Fonts/SFNSMono.ttf"),
        faces: [
            Some((None, 0)),
            None,
            Some((Some("/System/Library/Fonts/SFNSMonoItalic.ttf"), 0)),
            None,
        ],
    },
    FontPreset {
        name: "Monaco",
        path: Some("/System/Library/Fonts/Monaco.ttf"),
        faces: [Some((None, 0)), None, None, None],
    },
    FontPreset {
        name: "Courier New",
        path: Some("/System/Library/Fonts/Supplemental/Courier New.ttf"),
        faces: [
            Some((None, 0)),
            Some((
                Some("/System/Library/Fonts/Supplemental/Courier New Bold.ttf"),
                0,
            )),
            Some((
                Some("/System/Library/Fonts/Supplemental/Courier New Italic.ttf"),
                0,
            )),
            Some((
                Some("/System/Library/Fonts/Supplemental/Courier New Bold Italic.ttf"),
                0,
            )),
        ],
    },
    FontPreset {
        name: "Andale Mono",
        path: Some("/System/Library/Fonts/Supplemental/Andale Mono.ttf"),
        faces: [Some((None, 0)), None, None, None],
    },
    FontPreset {
        name: "Hack (built-in)",
        path: None,
        faces: [None, None, None, None],
    },
];

const CJK_FALLBACKS: &[(&str, &str)] = &[
    ("cjk-ja", "/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc"),
    ("cjk-zh", "/System/Library/Fonts/Hiragino Sans GB.ttc"),
    ("cjk-ko", "/System/Library/Fonts/AppleSDGothicNeo.ttc"),
];
const SYMBOLS: (&str, &str) = ("symbols", "/System/Library/Fonts/Apple Symbols.ttf");

/// The UI fonts, bundled: Inter in three weights and the Phosphor icons.
const INTER: &[u8] = include_bytes!("../../assets/fonts/Inter-Regular.ttf");
const INTER_MEDIUM: &[u8] = include_bytes!("../../assets/fonts/Inter-Medium.ttf");
const INTER_SEMIBOLD: &[u8] = include_bytes!("../../assets/fonts/Inter-SemiBold.ttf");
const PHOSPHOR: &[u8] = include_bytes!("../../assets/fonts/Phosphor.ttf");

/// Font families of the UI beyond egui's two: Inter Medium and SemiBold, and the icons alone.
pub const MEDIUM: &str = "ui-medium";
pub const SEMIBOLD: &str = "ui-semibold";
pub const ICONS: &str = "ui-icons";

/// Largest font file loaded.
const MAX_FONT_BYTES: u64 = 64 * 1024 * 1024;

/// Loads a font file once for the life of the process. System fonts are memory-mapped (they
/// don't change while the app runs); a font the user picked is read into memory, since
/// replacing a mapped file under the app would crash it.
fn mapped(path: &Path) -> Option<&'static [u8]> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, &'static [u8]>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut cache = cache.lock();
    if let Some(bytes) = cache.get(path) {
        return Some(bytes);
    }
    let file = std::fs::File::open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > MAX_FONT_BYTES {
        return None;
    }
    let bytes: &'static [u8] = if path.starts_with("/System/Library/") {
        // SAFETY: system font files are read-only (sealed system volume); the mapping lives
        // for the rest of the process.
        let map = unsafe { memmap2::Mmap::map(&file) }.ok()?;
        Box::leak(Box::new(map))
    } else {
        let mut data = Vec::new();
        std::io::Read::read_to_end(&mut std::io::Read::take(&file, MAX_FONT_BYTES), &mut data)
            .ok()?;
        Box::leak(data.into_boxed_slice())
    };
    cache.insert(path.to_path_buf(), bytes);
    Some(bytes)
}

/// Whether `bytes` holds a usable font at `index` (egui panics on a font it can't parse, so
/// every face is checked before it is handed over).
fn is_usable_font(bytes: &[u8], index: u32) -> bool {
    use skrifa::MetadataProvider;
    skrifa::FontRef::from_index(bytes, index).is_ok_and(|f| f.charmap().map('M').is_some())
}

fn add_face(defs: &mut FontDefinitions, key: &str, path: &Path, index: u32) -> bool {
    let Some(bytes) = mapped(path) else {
        return false;
    };
    if !is_usable_font(bytes, index) {
        return false;
    }
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
    let emoji: Vec<String> = ["NotoEmoji-Regular", "emoji-icon-font"]
        .iter()
        .map(|s| s.to_string())
        .collect();

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
        (None, Some(path))
            if add_face(
                &mut defs,
                TERM_REGULAR,
                &crate::paths::expand_tilde(path),
                choice.index,
            ) =>
        {
            faces[0] = Some(TERM_REGULAR.to_string());
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
        defs.families
            .insert(FontFamily::Name((*key).into()), family);
    }
    let mut mono = vec![regular, "Hack".to_string()];
    mono.extend(fallbacks.iter().cloned());
    mono.dedup();
    defs.families.insert(FontFamily::Monospace, mono);

    // UI: Inter, with the icon font right behind it so icons can sit inside ordinary text.
    // The icons live in the Private Use Area, like the Powerline and Nerd Font glyphs that
    // remote prompts print, so they stay out of the terminal and monospace families above.
    let font = |bytes: &'static [u8]| Arc::new(FontData::from_static(bytes));
    defs.font_data.insert("Inter".into(), font(INTER));
    defs.font_data.insert("Inter-Medium".into(), font(INTER_MEDIUM));
    defs.font_data.insert("Inter-SemiBold".into(), font(INTER_SEMIBOLD));
    let mut phosphor = FontData::from_static(PHOSPHOR);
    // Icons sit a little high next to Inter's x-height.
    phosphor.tweak.y_offset_factor = 0.08;
    defs.font_data.insert("Phosphor".into(), Arc::new(phosphor));
    let ui_family = |first: &str| {
        let mut family = vec![first.to_string(), "Phosphor".to_string()];
        family.extend(fallbacks.iter().cloned());
        family
    };
    defs.families
        .insert(FontFamily::Proportional, ui_family("Inter"));
    defs.families
        .insert(FontFamily::Name(MEDIUM.into()), ui_family("Inter-Medium"));
    defs.families
        .insert(FontFamily::Name(SEMIBOLD.into()), ui_family("Inter-SemiBold"));
    defs.families.insert(
        FontFamily::Name(ICONS.into()),
        vec!["Phosphor".into(), "Inter".into()],
    );
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

/// Installs the fonts. Returns false when a font file the user picked could not be used (the
/// built-in Hack font is used instead).
pub fn apply(ctx: &egui::Context, choice: &FontChoice, load_cjk: bool) -> bool {
    let defs = definitions(choice, load_cjk);
    let ok = preset(choice).is_some() || defs.font_data.contains_key(TERM_REGULAR);
    ctx.set_fonts(defs);
    ok
}

/// The saved font, or Menlo when nothing is chosen yet.
pub fn effective_choice(choice: &FontChoice) -> FontChoice {
    if choice.name.is_empty() && choice.path.is_none() {
        FontChoice {
            name: "Menlo".into(),
            path: None,
            index: 0,
        }
    } else {
        choice.clone()
    }
}
