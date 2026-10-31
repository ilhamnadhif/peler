//! Renders the app icon: `cargo run --example make_icon -- <out-dir>`.
//! Writes `icon_1024.png` and a macOS `.iconset` folder (turn it into `.icns` with `iconutil`).

#![allow(clippy::field_reassign_with_default)]

use std::path::PathBuf;

use tiny_skia::{
    Color, FillRule, GradientStop, LineCap, LineJoin, LinearGradient, Paint, PathBuilder, Pixmap,
    Point, Shader, SpreadMode, Stroke, Transform,
};

fn rounded_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> tiny_skia::Path {
    // Continuous-ish corners: cubic approximation with a slightly larger handle than a circle.
    let k = r * 0.62;
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - r + k, y, x + w, y + r - k, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - r + k, x + w - r + k, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r - k, y + h, x, y + h - r + k, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r - k, x + r - k, y, x + r, y);
    pb.close();
    pb.finish().unwrap()
}

fn gradient(p0: (f32, f32), p1: (f32, f32), stops: &[(f32, Color)]) -> Shader<'static> {
    LinearGradient::new(
        Point::from_xy(p0.0, p0.1),
        Point::from_xy(p1.0, p1.1),
        stops
            .iter()
            .map(|(pos, c)| GradientStop::new(*pos, *c))
            .collect(),
        SpreadMode::Pad,
        Transform::identity(),
    )
    .unwrap()
}

/// How much larger than its nominal geometry the glyph is drawn (also in `paint_logo`).
const GLYPH_SCALE: f32 = 1.08;

fn fill(pm: &mut Pixmap, path: &tiny_skia::Path, paint: &Paint, t: Transform) {
    pm.fill_path(path, paint, FillRule::Winding, t, None);
}

fn solid(r: u8, g: u8, b: u8, a: u8) -> Paint<'static> {
    let mut paint = Paint::default();
    paint.anti_alias = true;
    paint.set_color(Color::from_rgba8(r, g, b, a));
    paint
}

fn circle(x: f32, y: f32, r: f32) -> tiny_skia::Path {
    PathBuilder::from_circle(x, y, r).unwrap()
}

/// The icon, in the style of the other apps of the family (Merpati, dbiji): a rounded tile with
/// the app's indigo gradient and one white glyph, here two server units with a shell prompt,
/// and a single accent (the green status lights). Geometry is in 1024 px units; the tile spans
/// 100..924 (the macOS icon grid). `src/ui/icons.rs::paint_logo` draws the same picture.
fn render(size: u32) -> Pixmap {
    let s = size as f32 / 1024.0;
    let mut pm = Pixmap::new(size, size).unwrap();
    let t = Transform::from_scale(s, s);
    // Small icons drop the details that would turn into noise.
    let small = size <= 32;

    // Soft shadow, mostly below the tile.
    for i in 0..10 {
        let spread = 1.0 + i as f32 * 2.4;
        let body = rounded_rect(
            100.0 - spread,
            118.0 - spread,
            824.0 + 2.0 * spread,
            824.0 + 2.0 * spread,
            185.0 + spread,
        );
        fill(&mut pm, &body, &solid(0, 0, 0, 7), t);
    }

    // Tile: the indigo gradient, lighter at the top.
    let tile = rounded_rect(100.0, 100.0, 824.0, 824.0, 185.0);
    let mut paint = Paint::default();
    paint.anti_alias = true;
    paint.shader = gradient(
        (512.0, 100.0),
        (512.0, 924.0),
        &[
            (0.0, Color::from_rgba8(0x7c, 0x6c, 0xf0, 255)),
            (1.0, Color::from_rgba8(0x4b, 0x3b, 0xc4, 255)),
        ],
    );
    fill(&mut pm, &tile, &paint, t);

    // The glyph is drawn a little larger than its nominal geometry, around the center.
    let k = GLYPH_SCALE;
    let tg = t.pre_concat(Transform::from_row(k, 0.0, 0.0, k, 512.0 * (1.0 - k), 512.0 * (1.0 - k)));
    let ink = (0x55, 0x45, 0xd2);
    for (i, top) in [297.0f32, 537.0].into_iter().enumerate() {
        let cy = top + 95.0;
        // A soft shadow lifts the unit off the tile.
        for k in 0..6 {
            let g = 4.0 + k as f32 * 3.0;
            let sh = rounded_rect(232.0 - g / 2.0, top + 10.0 - g / 2.0, 560.0 + g, 190.0 + g, 48.0 + g / 2.0);
            fill(&mut pm, &sh, &solid(0x22, 0x16, 0x80, 12), tg);
        }
        // The unit: white, a touch of lavender at the bottom.
        let unit = rounded_rect(232.0, top, 560.0, 190.0, 48.0);
        let mut body = Paint::default();
        body.anti_alias = true;
        body.shader = gradient(
            (512.0, top),
            (512.0, top + 190.0),
            &[
                (0.0, Color::from_rgba8(255, 255, 255, 255)),
                (1.0, Color::from_rgba8(0xec, 0xea, 0xfb, 255)),
            ],
        );
        fill(&mut pm, &unit, &body, tg);

        // Status light: green, with a faint glow.
        let led_r = if small { 26.0 } else { 19.0 };
        if !small {
            fill(&mut pm, &circle(730.0, cy, 34.0), &solid(0x4a, 0xde, 0x80, 50), tg);
            fill(&mut pm, &circle(672.0, cy, 15.0), &solid(0xd4, 0xd0, 0xf2, 255), tg);
        }
        fill(&mut pm, &circle(730.0, cy, led_r), &solid(0x4a, 0xde, 0x80, 255), tg);

        if i == 0 {
            // The prompt: ">_".
            let mut pb = PathBuilder::new();
            pb.move_to(300.0, cy - 42.0);
            pb.line_to(352.0, cy);
            pb.line_to(300.0, cy + 42.0);
            let chevron = pb.finish().unwrap();
            let stroke = Stroke {
                width: if small { 40.0 } else { 28.0 },
                line_cap: LineCap::Round,
                line_join: LineJoin::Round,
                ..Stroke::default()
            };
            pm.stroke_path(&chevron, &solid(ink.0, ink.1, ink.2, 255), &stroke, tg, None);
            let bar = if small {
                rounded_rect(384.0, cy + 18.0, 96.0, 34.0, 17.0)
            } else {
                rounded_rect(380.0, cy + 26.0, 74.0, 24.0, 12.0)
            };
            fill(&mut pm, &bar, &solid(ink.0, ink.1, ink.2, 255), tg);
        } else if !small {
            // A drive slot.
            let slot = rounded_rect(300.0, cy - 11.0, 200.0, 22.0, 11.0);
            fill(&mut pm, &slot, &solid(0xd4, 0xd0, 0xf2, 255), tg);
        }
    }
    pm
}

fn main() {
    let out = PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| "assets".into()));
    std::fs::create_dir_all(&out).unwrap();
    render(1024).save_png(out.join("icon_1024.png")).unwrap();
    render(256).save_png(out.join("icon_256.png")).unwrap();
    let set = out.join("AppIcon.iconset");
    std::fs::create_dir_all(&set).unwrap();
    for (size, name) in [
        (16, "icon_16x16.png"),
        (32, "icon_16x16@2x.png"),
        (32, "icon_32x32.png"),
        (64, "icon_32x32@2x.png"),
        (128, "icon_128x128.png"),
        (256, "icon_128x128@2x.png"),
        (256, "icon_256x256.png"),
        (512, "icon_256x256@2x.png"),
        (512, "icon_512x512.png"),
        (1024, "icon_512x512@2x.png"),
    ] {
        render(size).save_png(set.join(name)).unwrap();
    }
    println!("wrote {}", out.display());
}
