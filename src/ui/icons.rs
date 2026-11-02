//! Icons: the bundled Phosphor glyphs (`icon_codes.rs`, used inside ordinary text such as
//! `format!("{} Refresh", icons::ARROWS_CLOCKWISE)`), which file gets which icon, and the app
//! logo drawn with the painter.

use eframe::egui::{self, Color32, Painter, Rect, Stroke, Ui, vec2};

pub use super::icon_codes::*;
use super::style::{self, Palette};

/// The icon and its color for an entry of a file list.
pub fn file_icon(name: &str, is_dir: bool, is_link: bool, p: &Palette) -> (&'static str, Color32) {
    if is_dir {
        return (FOLDER, p.folder);
    }
    if is_link {
        return (LINK_SIMPLE, p.text_muted);
    }
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    let icon = match ext.as_str() {
        "rs" | "go" | "py" | "js" | "ts" | "jsx" | "tsx" | "c" | "h" | "cc" | "cpp" | "hpp"
        | "java" | "kt" | "rb" | "php" | "sh" | "bash" | "zsh" | "fish" | "lua" | "swift"
        | "json" | "yaml" | "yml" | "toml" | "xml" | "html" | "css" | "scss" | "sql" | "ini"
        | "conf" | "cfg" | "env" | "dockerfile" | "mk" => FILE_CODE,
        "zip" | "gz" | "tgz" | "xz" | "bz2" | "zst" | "tar" | "7z" | "rar" | "deb" | "rpm"
        | "jar" | "war" => FILE_ZIP,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" | "ico" | "bmp" | "tiff" | "heic" => {
            FILE_IMAGE
        }
        "pdf" => FILE_PDF,
        "txt" | "md" | "log" | "csv" | "rst" | "out" => FILE_TEXT,
        _ => FILE,
    };
    (icon, p.text_muted)
}

/// An icon followed by a (truncated) label, as one row element.
pub fn icon_label(ui: &mut Ui, icon: &str, color: Color32, text: &str) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 7.0;
        ui.label(
            egui::RichText::new(icon)
                .font(style::icon_font(15.0))
                .color(color),
        );
        ui.add(egui::Label::new(text).truncate().selectable(false));
    });
}

// ---------------------------------------------------------------------------------------------
// The logo: the app icon (assets/icon.svg) without its margin, for the welcome screen.
// Coordinates are those of the 1024 px icon; its tile spans 72..952.
// ---------------------------------------------------------------------------------------------

/// The tile, and the ink of the lattice and the prompt.
pub const LOGO_TILE: Color32 = Color32::from_rgb(0x4b, 0x42, 0xc4);
const LOGO_INK: Color32 = Color32::from_rgb(0xff, 0xf6, 0xea);

/// Draws the logo filling `rect` (a square): the 3 x 3 lattice of the Ternary family, with
/// the prompt `>_` running through it.
pub fn paint_logo(painter: &Painter, rect: Rect) {
    let s = rect.width() / 880.0;
    let point = |c: usize, r: usize| {
        rect.min
            + vec2(
                (240.0 + 200.0 * c as f32) * s,
                (240.0 + 200.0 * r as f32) * s,
            )
    };
    painter.rect_filled(rect, 196.0 * s, LOGO_TILE);
    let stroke = Stroke::new(72.0 * s, LOGO_INK);
    painter.line_segment([point(0, 0), point(1, 1)], stroke);
    painter.line_segment([point(1, 1), point(0, 2)], stroke);
    painter.line_segment([point(1, 2), point(2, 2)], stroke);
    let on = [(0, 0), (1, 1), (0, 2), (1, 2), (2, 2)];
    for r in 0..3 {
        for c in 0..3 {
            if on.contains(&(c, r)) {
                painter.circle_filled(point(c, r), 52.0 * s, LOGO_INK);
            } else {
                painter.circle_filled(point(c, r), 30.0 * s, LOGO_INK.gamma_multiply(0.32));
            }
        }
    }
}
