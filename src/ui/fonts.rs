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

/// A selectable terminal font: file and face indices (regular, bold, italic, bold italic). On
/// macOS files are absolute paths; on Windows and Linux file names, looked up in the system's
/// font folders (`locate`).
#[derive(Clone, Debug)]
pub struct FontPreset {
    pub name: &'static str,
    pub path: Option<&'static str>,
    pub faces: [Option<(Option<&'static str>, u32)>; 4],
}

#[cfg(not(target_os = "macos"))]
const fn four(
    r: &'static str,
    b: &'static str,
    i: &'static str,
    bi: &'static str,
) -> [Option<(Option<&'static str>, u32)>; 4] {
    [
        Some((Some(r), 0)),
        Some((Some(b), 0)),
        Some((Some(i), 0)),
        Some((Some(bi), 0)),
    ]
}

const BUILT_IN: FontPreset = FontPreset {
    name: "Hack (built-in)",
    path: None,
    faces: [None, None, None, None],
};

#[cfg(target_os = "macos")]
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
    BUILT_IN,
];

#[cfg(windows)]
pub const PRESETS: &[FontPreset] = &[
    FontPreset {
        name: "Consolas",
        path: Some("consola.ttf"),
        faces: four(
            "consola.ttf",
            "consolab.ttf",
            "consolai.ttf",
            "consolaz.ttf",
        ),
    },
    FontPreset {
        name: "Cascadia Mono",
        path: Some("CascadiaMono.ttf"),
        faces: [
            Some((None, 0)),
            None,
            Some((Some("CascadiaMonoItalic.ttf"), 0)),
            None,
        ],
    },
    FontPreset {
        name: "Lucida Console",
        path: Some("lucon.ttf"),
        faces: [Some((None, 0)), None, None, None],
    },
    FontPreset {
        name: "Courier New",
        path: Some("cour.ttf"),
        faces: four("cour.ttf", "courbd.ttf", "couri.ttf", "courbi.ttf"),
    },
    BUILT_IN,
];

#[cfg(not(any(target_os = "macos", windows)))]
pub const PRESETS: &[FontPreset] = &[
    BUILT_IN,
    FontPreset {
        name: "DejaVu Sans Mono",
        path: Some("DejaVuSansMono.ttf"),
        faces: four(
            "DejaVuSansMono.ttf",
            "DejaVuSansMono-Bold.ttf",
            "DejaVuSansMono-Oblique.ttf",
            "DejaVuSansMono-BoldOblique.ttf",
        ),
    },
    FontPreset {
        name: "Liberation Mono",
        path: Some("LiberationMono-Regular.ttf"),
        faces: four(
            "LiberationMono-Regular.ttf",
            "LiberationMono-Bold.ttf",
            "LiberationMono-Italic.ttf",
            "LiberationMono-BoldItalic.ttf",
        ),
    },
    FontPreset {
        name: "Ubuntu Mono",
        path: Some("UbuntuMono-R.ttf"),
        faces: four(
            "UbuntuMono-R.ttf",
            "UbuntuMono-B.ttf",
            "UbuntuMono-RI.ttf",
            "UbuntuMono-BI.ttf",
        ),
    },
    FontPreset {
        name: "Noto Sans Mono",
        path: Some("NotoSansMono-Regular.ttf"),
        faces: [
            Some((None, 0)),
            Some((Some("NotoSansMono-Bold.ttf"), 0)),
            None,
            None,
        ],
    },
];

/// Fallbacks for Chinese, Japanese and Korean text, by key and file.
#[cfg(target_os = "macos")]
const CJK_FALLBACKS: &[(&str, &str)] = &[
    ("cjk-ja", "/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc"),
    ("cjk-zh", "/System/Library/Fonts/Hiragino Sans GB.ttc"),
    ("cjk-ko", "/System/Library/Fonts/AppleSDGothicNeo.ttc"),
];
#[cfg(windows)]
const CJK_FALLBACKS: &[(&str, &str)] = &[
    ("cjk-ja", "YuGothM.ttc"),
    ("cjk-zh", "msyh.ttc"),
    ("cjk-ko", "malgun.ttf"),
];
/// Noto CJK covers all three; WenQuanYi where it is the one installed.
#[cfg(not(any(target_os = "macos", windows)))]
const CJK_FALLBACKS: &[(&str, &str)] = &[
    ("cjk", "NotoSansCJK-Regular.ttc"),
    ("cjk-wqy", "wqy-microhei.ttc"),
];

#[cfg(target_os = "macos")]
const SYMBOLS: (&str, &str) = ("symbols", "/System/Library/Fonts/Apple Symbols.ttf");
#[cfg(windows)]
const SYMBOLS: (&str, &str) = ("symbols", "seguisym.ttf");
#[cfg(not(any(target_os = "macos", windows)))]
const SYMBOLS: (&str, &str) = ("symbols", "DejaVuSans.ttf");

/// The terminal font when none was chosen: the platform's usual one.
const DEFAULT_FONT: &str = if cfg!(target_os = "macos") {
    "Menlo"
} else if cfg!(windows) {
    "Consolas"
} else {
    "Hack (built-in)"
};

/// The system's font folders (Windows and Linux).
#[cfg(windows)]
fn font_dirs() -> Vec<PathBuf> {
    let windir = std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into());
    let mut dirs = vec![PathBuf::from(windir).join("Fonts")];
    // Fonts installed for this user only.
    if let Some(local) = dirs::data_local_dir() {
        dirs.push(local.join("Microsoft").join("Windows").join("Fonts"));
    }
    dirs
}

#[cfg(not(any(target_os = "macos", windows)))]
fn font_dirs() -> Vec<PathBuf> {
    let home = crate::paths::home_dir();
    let mut dirs = vec![
        PathBuf::from("/usr/share/fonts"),
        PathBuf::from("/usr/local/share/fonts"),
        home.join(".local/share/fonts"),
        home.join(".fonts"),
    ];
    if let Some(data) = std::env::var_os("XDG_DATA_HOME") {
        dirs.push(PathBuf::from(data).join("fonts"));
    }
    dirs
}

/// Where a font file is: absolute paths as they are (macOS); file names in the system's font
/// folders. On Linux the folders are indexed once, on first use (fonts sit in distribution-
/// specific subfolders); nothing is read for the built-in font.
fn locate(file: &str) -> Option<PathBuf> {
    let path = Path::new(file);
    if path.is_absolute() {
        return Some(path.to_path_buf());
    }
    #[cfg(target_os = "macos")]
    {
        None
    }
    #[cfg(windows)]
    {
        font_dirs()
            .into_iter()
            .map(|d| d.join(file))
            .find(|p| p.is_file())
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        static INDEX: OnceLock<HashMap<String, PathBuf>> = OnceLock::new();
        let index = INDEX.get_or_init(|| {
            let mut index = HashMap::new();
            let mut stack: Vec<(PathBuf, u32)> = font_dirs().into_iter().map(|d| (d, 0)).collect();
            while let Some((dir, depth)) = stack.pop() {
                let Ok(entries) = std::fs::read_dir(&dir) else {
                    continue;
                };
                for entry in entries.flatten() {
                    let p = entry.path();
                    match entry.file_type() {
                        Ok(t) if t.is_dir() && depth < 4 => stack.push((p, depth + 1)),
                        Ok(_) => {
                            let name = entry.file_name().to_string_lossy().into_owned();
                            index.entry(name).or_insert(p);
                        }
                        Err(_) => {}
                    }
                }
            }
            index
        });
        index.get(file).cloned()
    }
}

/// Fonts of the system (read-only while the app runs, so they can be memory-mapped).
fn is_system_font(path: &Path) -> bool {
    if cfg!(target_os = "macos") {
        return path.starts_with("/System/Library/");
    }
    #[cfg(windows)]
    if let Some(windir) = std::env::var_os("WINDIR") {
        return path.starts_with(PathBuf::from(windir).join("Fonts"));
    }
    path.starts_with("/usr/share/fonts")
}

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
    let bytes: &'static [u8] = if is_system_font(path) {
        // SAFETY: system font files are read-only (the sealed system volume on macOS, files
        // owned by the system elsewhere); the mapping lives for the rest of the process.
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

fn add_face(defs: &mut FontDefinitions, key: &str, file: &str, index: u32) -> bool {
    let Some(path) = locate(file) else {
        return false;
    };
    let Some(bytes) = mapped(&path) else {
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

/// Whether a preset's font file is on this computer, so it can be offered: the built-in font
/// always is; the others are looked up where the system keeps fonts (on Windows and Linux the
/// preset names a file, not a path).
pub fn is_available(preset: &FontPreset) -> bool {
    preset
        .path
        .is_none_or(|file| locate(file).is_some_and(|p| p.is_file()))
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
    if add_face(&mut defs, SYMBOLS.0, SYMBOLS.1, 0) {
        fallbacks.push(SYMBOLS.0.into());
    }
    if load_cjk {
        for (key, path) in CJK_FALLBACKS {
            if add_face(&mut defs, key, path, 0) {
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
                    if let Some((file, index)) = face
                        && add_face(&mut defs, keys[i], file.unwrap_or(base), *index)
                    {
                        faces[i] = Some(keys[i].to_string());
                    }
                }
            }
        }
        (None, Some(path))
            if add_face(
                &mut defs,
                TERM_REGULAR,
                &crate::paths::expand_tilde(path).to_string_lossy(),
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
    defs.font_data
        .insert("Inter-Medium".into(), font(INTER_MEDIUM));
    defs.font_data
        .insert("Inter-SemiBold".into(), font(INTER_SEMIBOLD));
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
    defs.families.insert(
        FontFamily::Name(SEMIBOLD.into()),
        ui_family("Inter-SemiBold"),
    );
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

/// The saved font, or the platform's usual one when nothing is chosen yet.
pub fn effective_choice(choice: &FontChoice) -> FontChoice {
    if choice.name.is_empty() && choice.path.is_none() {
        FontChoice {
            name: DEFAULT_FONT.into(),
            path: None,
            index: 0,
        }
    } else {
        choice.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Settings offers the presets whose files are on this computer: the platform's usual font
    /// (Menlo, Consolas; the built-in Hack on Linux), never one whose file is missing.
    #[test]
    fn presets_are_offered_when_their_file_exists() {
        let default = PRESETS.iter().find(|p| p.name == DEFAULT_FONT).unwrap();
        assert!(is_available(default), "{}", default.name);
        assert!(is_available(&BUILT_IN));
        for file in ["ternaryshell-no-such-font.ttf", "/no/such/dir/font.ttf"] {
            let missing = FontPreset {
                name: "Missing",
                path: Some(file),
                faces: [None, None, None, None],
            };
            assert!(!is_available(&missing), "{file}");
        }
    }
}
