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

fn render(size: u32) -> Pixmap {
    let s = size as f32 / 1024.0;
    let mut pm = Pixmap::new(size, size).unwrap();
    let t = Transform::from_scale(s, s);

    // Soft shadow under the body.
    for i in 0..12 {
        let spread = 2.0 + i as f32 * 3.0;
        let mut paint = Paint::default();
        paint.set_color(Color::from_rgba8(0, 0, 0, 5));
        paint.anti_alias = true;
        let body = rounded_rect(
            100.0 - spread,
            112.0 - spread,
            824.0 + 2.0 * spread,
            824.0 + 2.0 * spread,
            185.0 + spread,
        );
        pm.fill_path(&body, &paint, FillRule::Winding, t, None);
    }

    // Body: deep navy gradient.
    let body = rounded_rect(100.0, 100.0, 824.0, 824.0, 185.0);
    let mut paint = Paint::default();
    paint.anti_alias = true;
    paint.shader = gradient(
        (512.0, 100.0),
        (512.0, 924.0),
        &[
            (0.0, Color::from_rgba8(0x26, 0x33, 0x52, 255)),
            (1.0, Color::from_rgba8(0x0b, 0x10, 0x1d, 255)),
        ],
    );
    pm.fill_path(&body, &paint, FillRule::Winding, t, None);

    // Faint top highlight band.
    let mut hl = Paint::default();
    hl.anti_alias = true;
    hl.shader = gradient(
        (512.0, 100.0),
        (512.0, 420.0),
        &[
            (0.0, Color::from_rgba8(255, 255, 255, 34)),
            (1.0, Color::from_rgba8(255, 255, 255, 0)),
        ],
    );
    pm.fill_path(&body, &hl, FillRule::Winding, t, None);

    // Inner rim.
    let mut rim = Paint::default();
    rim.anti_alias = true;
    rim.set_color(Color::from_rgba8(255, 255, 255, 28));
    let stroke = Stroke {
        width: 6.0,
        ..Stroke::default()
    };
    pm.stroke_path(
        &rounded_rect(103.0, 103.0, 818.0, 818.0, 182.0),
        &rim,
        &stroke,
        t,
        None,
    );

    // Terminal "window" panel.
    let panel = rounded_rect(210.0, 250.0, 604.0, 470.0, 56.0);
    let mut pp = Paint::default();
    pp.anti_alias = true;
    pp.set_color(Color::from_rgba8(0x05, 0x08, 0x10, 150));
    pm.fill_path(&panel, &pp, FillRule::Winding, t, None);
    let mut pr = Paint::default();
    pr.anti_alias = true;
    pr.set_color(Color::from_rgba8(0x5e, 0xea, 0xd4, 60));
    pm.stroke_path(
        &panel,
        &pr,
        &Stroke {
            width: 4.0,
            ..Stroke::default()
        },
        t,
        None,
    );

    // Traffic-light dots.
    for (i, rgb) in [(0xff, 0x5f, 0x57), (0xfe, 0xbc, 0x2e), (0x28, 0xc8, 0x40)]
        .iter()
        .enumerate()
    {
        let mut dot = Paint::default();
        dot.anti_alias = true;
        dot.set_color(Color::from_rgba8(rgb.0, rgb.1, rgb.2, 230));
        let c = PathBuilder::from_circle(262.0 + i as f32 * 42.0, 300.0, 13.0).unwrap();
        pm.fill_path(&c, &dot, FillRule::Winding, t, None);
    }

    // Prompt chevron ">".
    let mut pb = PathBuilder::new();
    pb.move_to(300.0, 410.0);
    pb.line_to(420.0, 500.0);
    pb.line_to(300.0, 590.0);
    let chevron = pb.finish().unwrap();
    let mut cp = Paint::default();
    cp.anti_alias = true;
    cp.shader = gradient(
        (300.0, 410.0),
        (420.0, 590.0),
        &[
            (0.0, Color::from_rgba8(0x5e, 0xea, 0xd4, 255)),
            (1.0, Color::from_rgba8(0x22, 0xb8, 0xee, 255)),
        ],
    );
    let thick = Stroke {
        width: 54.0,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Stroke::default()
    };
    pm.stroke_path(&chevron, &cp, &thick, t, None);

    // Cursor block "_".
    let mut cur = Paint::default();
    cur.anti_alias = true;
    cur.shader = gradient(
        (470.0, 560.0),
        (660.0, 610.0),
        &[
            (0.0, Color::from_rgba8(0xfb, 0xd3, 0x4d, 255)),
            (1.0, Color::from_rgba8(0xf5, 0x9e, 0x0b, 255)),
        ],
    );
    let bar = rounded_rect(470.0, 562.0, 190.0, 50.0, 25.0);
    pm.fill_path(&bar, &cur, FillRule::Winding, t, None);

    // Small upload arrow (SFTP) at the bottom right of the panel.
    let mut ap = Paint::default();
    ap.anti_alias = true;
    ap.set_color(Color::from_rgba8(0x5e, 0xea, 0xd4, 170));
    let mut pb = PathBuilder::new();
    pb.move_to(735.0, 660.0);
    pb.line_to(735.0, 560.0);
    pb.move_to(700.0, 595.0);
    pb.line_to(735.0, 558.0);
    pb.line_to(770.0, 595.0);
    let arrow = pb.finish().unwrap();
    let st = Stroke {
        width: 18.0,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Stroke::default()
    };
    pm.stroke_path(&arrow, &ap, &st, t, None);
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
