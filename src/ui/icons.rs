//! Small vector icons drawn with the painter (no icon font needed, crisp at any scale).

use eframe::egui::{
    self, Color32, CornerRadius, Painter, Pos2, Rect, Stroke, StrokeKind, Ui, pos2, vec2,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Folder,
    File,
    Link,
}

pub fn folder_color(dark: bool) -> Color32 {
    if dark {
        Color32::from_rgb(0x6c, 0xa8, 0xf0)
    } else {
        Color32::from_rgb(0x3b, 0x82, 0xe0)
    }
}

/// Paints an icon centered at `c` with the given height.
pub fn paint(painter: &Painter, icon: Icon, c: Pos2, h: f32, color: Color32) {
    match icon {
        Icon::Folder => {
            let w = h * 1.25;
            let body = Rect::from_center_size(c + vec2(0.0, h * 0.06), vec2(w, h * 0.78));
            let tab = Rect::from_min_size(
                pos2(body.left(), body.top() - h * 0.16),
                vec2(w * 0.42, h * 0.3),
            );
            painter.rect_filled(tab, CornerRadius::same(2), color.gamma_multiply(0.85));
            painter.rect_filled(body, CornerRadius::same(2), color);
        }
        Icon::File => {
            let r = Rect::from_center_size(c, vec2(h * 0.78, h));
            painter.rect_stroke(
                r,
                CornerRadius::same(2),
                Stroke::new(1.2, color),
                StrokeKind::Inside,
            );
            for i in 0..3 {
                let y = r.top() + h * (0.32 + i as f32 * 0.2);
                painter.hline(
                    r.left() + h * 0.18..=r.right() - h * 0.18,
                    y,
                    Stroke::new(1.0, color.gamma_multiply(0.7)),
                );
            }
        }
        Icon::Link => {
            paint(painter, Icon::File, c, h, color);
            let a = pos2(c.x - h * 0.2, c.y + h * 0.25);
            let b = pos2(c.x + h * 0.25, c.y - h * 0.2);
            painter.line_segment([a, b], Stroke::new(1.4, color));
            painter.line_segment([b, pos2(b.x - h * 0.22, b.y)], Stroke::new(1.4, color));
            painter.line_segment([b, pos2(b.x, b.y + h * 0.22)], Stroke::new(1.4, color));
        }
    }
}

/// An icon followed by a (truncated) label, as one row element.
pub fn icon_label(ui: &mut Ui, icon: Icon, text: &str) {
    ui.horizontal(|ui| {
        let h = 13.0;
        let (rect, _) = ui.allocate_exact_size(vec2(h * 1.3, h), egui::Sense::hover());
        let color = match icon {
            Icon::Folder => folder_color(ui.visuals().dark_mode),
            _ => ui.visuals().weak_text_color(),
        };
        paint(ui.painter(), icon, rect.center(), h, color);
        ui.add(egui::Label::new(text).truncate().selectable(false));
    });
}
