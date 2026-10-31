//! Icons: the bundled Phosphor glyphs (`icon_codes.rs`, used inside ordinary text such as
//! `format!("{} Refresh", icons::ARROWS_CLOCKWISE)`), which file gets which icon, and the app
//! logo drawn with the painter.

use eframe::egui::{self, Color32, Painter, Pos2, Rect, Stroke, Ui, pos2, vec2};

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
// The logo: the app icon (examples/make_icon.rs) without its margin, for the welcome screen.
// Coordinates are those of the 1024 px icon; its tile spans 100..924.
// ---------------------------------------------------------------------------------------------

/// Top and bottom of the tile's gradient.
pub const LOGO_TOP: Color32 = Color32::from_rgb(0x7c, 0x6c, 0xf0);
pub const LOGO_BOTTOM: Color32 = Color32::from_rgb(0x4b, 0x3b, 0xc4);
/// The prompt drawn on the server, and the status lights.
const LOGO_INK: Color32 = Color32::from_rgb(0x55, 0x45, 0xd2);
const LOGO_LED: Color32 = Color32::from_rgb(0x4a, 0xde, 0x80);
const LOGO_SLOT: Color32 = Color32::from_rgb(0xd4, 0xd0, 0xf2);

/// Points around a rounded rectangle (`n` per corner), clockwise from the top left.
fn rounded_points(r: Rect, radius: f32, n: usize) -> Vec<Pos2> {
    let mut pts = Vec::with_capacity(4 * (n + 1));
    let corners = [
        (pos2(r.left() + radius, r.top() + radius), 180.0f32),
        (pos2(r.right() - radius, r.top() + radius), 270.0),
        (pos2(r.right() - radius, r.bottom() - radius), 0.0),
        (pos2(r.left() + radius, r.bottom() - radius), 90.0),
    ];
    for (c, start) in corners {
        for i in 0..=n {
            let a = (start + 90.0 * i as f32 / n as f32).to_radians();
            pts.push(c + vec2(a.cos(), a.sin()) * radius);
        }
    }
    pts
}

/// A convex shape filled with a vertical gradient (a fan from its center).
fn gradient_fill(painter: &Painter, pts: &[Pos2], top: Color32, bottom: Color32) {
    let (y0, y1) = pts
        .iter()
        .fold((f32::MAX, f32::MIN), |(a, b), p| (a.min(p.y), b.max(p.y)));
    let color_at = |y: f32| top.lerp_to_gamma(bottom, ((y - y0) / (y1 - y0).max(1.0)).clamp(0.0, 1.0));
    let center = pts.iter().fold(Pos2::ZERO, |acc, p| acc + p.to_vec2()) / pts.len() as f32;
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(center, color_at(center.y));
    for p in pts {
        mesh.colored_vertex(*p, color_at(p.y));
    }
    let n = pts.len() as u32;
    for i in 0..n {
        mesh.add_triangle(0, 1 + i, 1 + (i + 1) % n);
    }
    painter.add(mesh);
}

/// Draws the logo filling `rect` (a square).
pub fn paint_logo(painter: &Painter, rect: Rect) {
    let s = rect.width() / 824.0;
    // The glyph is drawn 8% larger than its nominal geometry, around the center (as in the icon).
    const K: f32 = 1.08;
    let g = |v: f32| 512.0 + (v - 512.0) * K;
    let at = |x: f32, y: f32| rect.min + vec2((g(x) - 100.0) * s, (g(y) - 100.0) * s);
    let r = |x: f32, y: f32, w: f32, h: f32| Rect::from_min_size(at(x, y), vec2(w * s * K, h * s * K));
    let s_glyph = s * K;
    let small = rect.width() < 40.0;

    gradient_fill(painter, &rounded_points(rect, 185.0 * s, 10), LOGO_TOP, LOGO_BOTTOM);
    for (i, top) in [297.0, 537.0].into_iter().enumerate() {
        let unit = r(232.0, top, 560.0, 190.0);
        gradient_fill(
            painter,
            &rounded_points(unit, 48.0 * s_glyph, 6),
            Color32::WHITE,
            Color32::from_rgb(0xec, 0xea, 0xfb),
        );
        let cy = top + 95.0;
        painter.circle_filled(at(730.0, cy), 19.0 * s_glyph, LOGO_LED);
        if !small {
            painter.circle_filled(at(672.0, cy), 15.0 * s_glyph, LOGO_SLOT);
        }
        if i == 0 {
            // ">_" on the upper unit.
            let w = (28.0 * s_glyph).max(1.2);
            let stroke = Stroke::new(w, LOGO_INK);
            painter.line_segment([at(300.0, cy - 42.0), at(352.0, cy)], stroke);
            painter.line_segment([at(352.0, cy), at(300.0, cy + 42.0)], stroke);
            painter.circle_filled(at(352.0, cy), w / 2.0, LOGO_INK);
            painter.rect_filled(r(380.0, cy + 26.0, 74.0, 24.0), 12.0 * s_glyph, LOGO_INK);
        } else if !small {
            painter.rect_filled(r(300.0, cy - 11.0, 200.0, 22.0), 11.0 * s_glyph, LOGO_SLOT);
        }
    }
}
