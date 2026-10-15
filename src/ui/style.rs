//! Visual style: a calm, native-looking theme for both light and dark mode.

use eframe::egui::{self, Color32, CornerRadius, Margin, Stroke, Theme, ThemePreference, vec2};

use crate::settings::ThemeMode;

pub const ACCENT: Color32 = Color32::from_rgb(0x2f, 0x81, 0xf7);
pub const GREEN: Color32 = Color32::from_rgb(0x3f, 0xb9, 0x50);
pub const AMBER: Color32 = Color32::from_rgb(0xe0, 0xa8, 0x2e);
pub const RED: Color32 = Color32::from_rgb(0xe0, 0x50, 0x4f);

pub fn apply(ctx: &egui::Context, mode: ThemeMode) {
    ctx.set_theme(match mode {
        ThemeMode::System => ThemePreference::System,
        ThemeMode::Light => ThemePreference::Light,
        ThemeMode::Dark => ThemePreference::Dark,
    });
    for theme in [Theme::Dark, Theme::Light] {
        ctx.style_mut_of(theme, |style| {
            let dark = theme == Theme::Dark;
            style.spacing.item_spacing = vec2(8.0, 6.0);
            style.spacing.button_padding = vec2(10.0, 4.0);
            style.spacing.interact_size.y = 24.0;
            style.spacing.window_margin = Margin::same(16);
            style.spacing.menu_margin = Margin::same(6);
            let v = &mut style.visuals;
            v.text_cursor.blink = false; // a blinking caret would repaint twice a second
            v.window_corner_radius = CornerRadius::same(10);
            v.menu_corner_radius = CornerRadius::same(8);
            v.selection.bg_fill = if dark {
                Color32::from_rgb(0x26, 0x4f, 0x78)
            } else {
                Color32::from_rgb(0xb4, 0xd5, 0xfe)
            };
            v.selection.stroke = Stroke::new(
                1.0,
                if dark {
                    Color32::from_rgb(0xd0, 0xe4, 0xff)
                } else {
                    Color32::from_rgb(0x10, 0x3d, 0x7a)
                },
            );
            v.hyperlink_color = ACCENT;
            if dark {
                v.panel_fill = Color32::from_rgb(0x1e, 0x20, 0x26);
                v.window_fill = Color32::from_rgb(0x25, 0x28, 0x2f);
                v.extreme_bg_color = Color32::from_rgb(0x16, 0x18, 0x1d);
                v.faint_bg_color = Color32::from_rgb(0x24, 0x27, 0x2e);
            } else {
                v.panel_fill = Color32::from_rgb(0xf4, 0xf5, 0xf7);
                v.window_fill = Color32::from_rgb(0xfc, 0xfc, 0xfd);
                v.extreme_bg_color = Color32::WHITE;
                v.faint_bg_color = Color32::from_rgb(0xec, 0xee, 0xf1);
            }
            for w in [
                &mut v.widgets.inactive,
                &mut v.widgets.hovered,
                &mut v.widgets.active,
                &mut v.widgets.open,
                &mut v.widgets.noninteractive,
            ] {
                w.corner_radius = CornerRadius::same(6);
            }
        });
    }
}

/// Background for the sidebar and tab strip (slightly different from the content area).
pub fn chrome_fill(dark: bool) -> Color32 {
    if dark {
        Color32::from_rgb(0x19, 0x1b, 0x20)
    } else {
        Color32::from_rgb(0xe9, 0xeb, 0xee)
    }
}
