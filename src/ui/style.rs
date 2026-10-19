//! Visual style: a calm, native-looking theme for both light and dark mode.

use eframe::egui::{
    self, Align, Color32, CornerRadius, Layout, Margin, RichText, Stroke, Theme, ThemePreference,
    Ui, vec2,
};

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
            // Selectable labels sense clicks and drags and would steal them from the rows,
            // buttons and tables they sit in; text that should be copyable opts in explicitly.
            style.interaction.selectable_labels = false;
            let v = &mut style.visuals;
            v.text_cursor.blink = false; // a blinking caret would repaint twice a second
            v.window_corner_radius = CornerRadius::same(10);
            v.menu_corner_radius = CornerRadius::same(8);
            v.selection.bg_fill = if dark {
                Color32::from_rgb(0x26, 0x4f, 0x78)
            } else {
                Color32::from_rgb(0xb4, 0xd5, 0xfe)
            };
            // The text color of selected rows, items and text. egui also rings focused text
            // fields with it; `Field` paints its own blue ring over that.
            v.selection.stroke = Stroke::new(
                1.0,
                if dark {
                    Color32::WHITE
                } else {
                    Color32::from_rgb(0x10, 0x3d, 0x7a)
                },
            );
            // Near-white text rather than pure white (which glares on a dark background), and a
            // quieter gray for details; both read well above the 4.5:1 contrast WCAG asks for.
            let (text, weak) = if dark {
                (
                    Color32::from_rgb(0xdc, 0xdf, 0xe4),
                    Color32::from_rgb(0x8f, 0x96, 0xa0),
                )
            } else {
                (
                    Color32::from_rgb(0x2b, 0x2f, 0x36),
                    Color32::from_rgb(0x64, 0x6a, 0x73),
                )
            };
            v.widgets.noninteractive.fg_stroke.color = text;
            v.widgets.inactive.fg_stroke.color = text;
            v.weak_text_color = Some(weak);
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
            // Fields, checkboxes and buttons get a hairline border, so a text field doesn't
            // look like plain text; it darkens on hover and turns blue while focused.
            let (border, border_hover) = if dark {
                (
                    Color32::from_rgb(0x3b, 0x40, 0x4a),
                    Color32::from_rgb(0x55, 0x5c, 0x68),
                )
            } else {
                (
                    Color32::from_rgb(0xcc, 0xd0, 0xd6),
                    Color32::from_rgb(0xa9, 0xaf, 0xb8),
                )
            };
            if !dark {
                // White buttons and menus with a hairline border, as on macOS.
                v.widgets.inactive.weak_bg_fill = Color32::WHITE;
                v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0xf3, 0xf4, 0xf6);
                v.widgets.active.weak_bg_fill = Color32::from_rgb(0xe6, 0xe8, 0xeb);
                v.widgets.open.weak_bg_fill = Color32::from_rgb(0xf3, 0xf4, 0xf6);
            }
            // Separators and table lines stay quiet.
            v.widgets.noninteractive.bg_stroke = Stroke::new(
                1.0,
                if dark {
                    Color32::from_rgb(0x2e, 0x32, 0x3a)
                } else {
                    Color32::from_rgb(0xdf, 0xe2, 0xe6)
                },
            );
            v.widgets.inactive.bg_stroke = Stroke::new(1.0, border);
            v.widgets.hovered.bg_stroke = Stroke::new(1.0, border_hover);
            v.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
            v.widgets.open.bg_stroke = Stroke::new(1.0, border_hover);
        });
    }
}

/// Warning text: amber, dark enough to read on a light background.
pub fn warn_text(v: &egui::Visuals) -> Color32 {
    if v.dark_mode {
        AMBER
    } else {
        Color32::from_rgb(0x8f, 0x5b, 0x00)
    }
}

/// The blue ring around a focused text field or checkbox.
pub fn focus_ring(v: &egui::Visuals) -> Stroke {
    Stroke::new(
        1.5,
        if v.dark_mode {
            Color32::from_rgb(0x6c, 0xa8, 0xff)
        } else {
            Color32::from_rgb(0x1d, 0x6f, 0xe0)
        },
    )
}

/// The colors of a file list: it sits a shade apart from the panel, with faint stripes.
pub struct ListColors {
    pub bg: Color32,
    pub stripe: Color32,
    pub hover: Color32,
    /// While files from the other side are dragged over it.
    pub drop: Color32,
}

pub fn list_colors(v: &egui::Visuals) -> ListColors {
    if v.dark_mode {
        ListColors {
            bg: v.extreme_bg_color,
            stripe: Color32::from_rgb(0x1c, 0x1f, 0x25),
            hover: Color32::from_rgb(0x27, 0x2b, 0x33),
            drop: Color32::from_rgb(0x1a, 0x2a, 0x3d),
        }
    } else {
        ListColors {
            bg: v.extreme_bg_color,
            stripe: Color32::from_rgb(0xf5, 0xf6, 0xf8),
            hover: Color32::from_rgb(0xea, 0xee, 0xf3),
            drop: Color32::from_rgb(0xe3, 0xef, 0xfe),
        }
    }
}

/// Width of the label column in forms.
pub const LABEL_W: f32 = 128.0;
/// Height of a form control (text field, button, menu).
pub const CONTROL_H: f32 = 28.0;

/// Roomier spacing for the content of a dialog or form.
pub fn form_spacing(ui: &mut Ui) {
    let s = ui.spacing_mut();
    s.interact_size.y = CONTROL_H;
    s.item_spacing = vec2(8.0, 8.0);
    s.button_padding = vec2(12.0, 5.0);
}

/// A one-line text field with comfortable padding, as wide as there is room.
pub fn input(text: &mut dyn egui::TextBuffer) -> Field<'_> {
    Field(
        egui::TextEdit::singleline(text)
            .margin(Margin::symmetric(8, 6))
            .desired_width(f32::INFINITY),
    )
}

/// A multi-line text field with the same padding.
pub fn input_multiline(text: &mut dyn egui::TextBuffer) -> Field<'_> {
    Field(
        egui::TextEdit::multiline(text)
            .margin(Margin::symmetric(8, 6))
            .desired_width(f32::INFINITY),
    )
}

/// A text field with a blue focus ring. egui rings a focused field with `selection.stroke`,
/// which is also the color of selected text and so has to stay readable on the selection; the
/// blue ring is painted over it.
pub struct Field<'t>(pub egui::TextEdit<'t>);

impl Field<'_> {
    pub fn hint_text(self, text: impl Into<egui::WidgetText>) -> Self {
        Self(self.0.hint_text(text))
    }

    pub fn password(self, password: bool) -> Self {
        Self(self.0.password(password))
    }

    pub fn desired_width(self, width: f32) -> Self {
        Self(self.0.desired_width(width))
    }

    pub fn desired_rows(self, rows: usize) -> Self {
        Self(self.0.desired_rows(rows))
    }

    pub fn font(self, font: impl Into<egui::FontSelection>) -> Self {
        Self(self.0.font(font))
    }
}

impl egui::Widget for Field<'_> {
    fn ui(self, ui: &mut Ui) -> egui::Response {
        let response = ui.add(self.0);
        if response.has_focus() {
            // Where egui drew its ring: a focused field is drawn as active.
            let active = ui.visuals().widgets.active;
            ui.painter().rect_stroke(
                response.rect.expand(active.expansion),
                active.corner_radius,
                focus_ring(ui.visuals()),
                egui::StrokeKind::Inside,
            );
        }
        response
    }
}

/// The main button of a dialog (blue).
pub fn primary(text: impl Into<String>) -> egui::Button<'static> {
    egui::Button::new(RichText::new(text.into()).color(Color32::WHITE)).fill(ACCENT)
}

/// A section title in a form.
pub fn section(ui: &mut Ui, title: &str) {
    ui.add_space(10.0);
    ui.label(
        RichText::new(title.to_uppercase())
            .small()
            .strong()
            .color(ui.visuals().weak_text_color()),
    );
}

/// A form row: the label on the left, lined up with the first control on the right.
pub fn row<R>(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            vec2(LABEL_W, CONTROL_H),
            Layout::left_to_right(Align::Center),
            |ui| {
                ui.set_min_size(vec2(LABEL_W, CONTROL_H));
                ui.label(label);
            },
        );
        ui.vertical(|ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
    })
    .inner
}

/// A segmented control (macOS style): one of `options`, the chosen one raised. Returns each
/// segment's response.
pub fn segmented<T: PartialEq + Copy>(
    ui: &mut Ui,
    value: &mut T,
    options: &[(T, &str)],
) -> Vec<egui::Response> {
    let dark = ui.visuals().dark_mode;
    let (track, raised, border) = if dark {
        (
            Color32::from_rgb(0x1a, 0x1c, 0x21),
            Color32::from_rgb(0x3a, 0x3f, 0x4a),
            Color32::from_rgb(0x4a, 0x50, 0x5b),
        )
    } else {
        (
            Color32::from_rgb(0xe4, 0xe6, 0xea),
            Color32::WHITE,
            Color32::from_rgb(0xc8, 0xcc, 0xd2),
        )
    };
    let (strong, normal) = (ui.visuals().strong_text_color(), ui.visuals().text_color());
    egui::Frame::NONE
        .fill(track)
        .corner_radius(7)
        .inner_margin(Margin::same(2))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                ui.spacing_mut().button_padding = vec2(14.0, 3.0);
                options
                    .iter()
                    .map(|(option, label)| {
                        let selected = *value == *option;
                        let button = egui::Button::new(RichText::new(*label).color(if selected {
                            strong
                        } else {
                            normal
                        }))
                        .fill(if selected {
                            raised
                        } else {
                            Color32::TRANSPARENT
                        })
                        .stroke(if selected {
                            Stroke::new(1.0, border)
                        } else {
                            Stroke::NONE
                        })
                        .corner_radius(5)
                        .min_size(vec2(0.0, CONTROL_H - 4.0));
                        let r = ui.add(button);
                        if r.clicked() {
                            *value = *option;
                        }
                        r
                    })
                    .collect()
            })
            .inner
        })
        .inner
}

/// Lays widgets out from the right, in a band one control high (a plain right-to-left layout
/// in a vertical one would take all the height there is). The last widget added can fill
/// what is left with `ui.available_width()`.
pub fn right_to_left<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.allocate_ui_with_layout(
        vec2(ui.available_width(), CONTROL_H),
        Layout::right_to_left(Align::Center),
        add,
    )
    .inner
}

/// A checkbox drawn as on macOS: a blue box with a white check when on.
pub fn checkbox(
    ui: &mut Ui,
    checked: &mut bool,
    text: impl Into<egui::WidgetText>,
) -> egui::Response {
    const BOX: f32 = 16.0;
    const GAP: f32 = 8.0;
    let galley = text.into().into_galley(
        ui,
        Some(egui::TextWrapMode::Wrap),
        (ui.available_width() - BOX - GAP).max(40.0),
        egui::TextStyle::Body,
    );
    // As tall as the other controls in the row, so labels beside it line up.
    let line = ui.spacing().interact_size.y.max(22.0);
    let size = vec2(
        BOX + GAP + galley.size().x,
        galley.size().y.max(BOX).max(line),
    );
    let (rect, mut response) = ui.allocate_exact_size(size, egui::Sense::click());
    if response.clicked() {
        *checked = !*checked;
        response.mark_changed();
    }
    let on = *checked;
    response.widget_info(|| {
        egui::WidgetInfo::selected(
            egui::WidgetType::Checkbox,
            ui.is_enabled(),
            on,
            galley.text(),
        )
    });
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact(&response);
        let box_rect = egui::Rect::from_center_size(
            egui::pos2(rect.left() + BOX / 2.0, rect.top() + line / 2.0),
            vec2(BOX, BOX),
        );
        let painter = ui.painter();
        if on {
            let fill = if response.hovered() {
                ACCENT.gamma_multiply(0.85)
            } else {
                ACCENT
            };
            painter.rect_filled(box_rect, 4.0, fill);
            let (l, t, r, b) = (
                box_rect.left(),
                box_rect.top(),
                box_rect.right(),
                box_rect.bottom(),
            );
            painter.add(egui::Shape::line(
                vec![
                    egui::pos2(l + 3.8, t + 8.2),
                    egui::pos2(l + 6.8, b - 4.2),
                    egui::pos2(r - 3.6, t + 4.4),
                ],
                Stroke::new(1.8, Color32::WHITE),
            ));
        } else {
            painter.rect(
                box_rect,
                4.0,
                ui.visuals().extreme_bg_color,
                visuals.bg_stroke,
                egui::StrokeKind::Inside,
            );
        }
        if response.has_focus() {
            painter.rect_stroke(
                box_rect.expand(2.0),
                5.0,
                focus_ring(ui.visuals()),
                egui::StrokeKind::Outside,
            );
        }
        let text_pos = egui::pos2(
            box_rect.right() + GAP,
            box_rect.center().y - galley.rows.first().map_or(galley.size().y, |r| r.height()) / 2.0,
        );
        painter.galley(text_pos, galley, visuals.text_color());
    }
    response
}

/// How wide `text` is in the body font.
pub fn text_width(ui: &Ui, text: &str) -> f32 {
    let font = egui::TextStyle::Body.resolve(ui.style());
    ui.painter()
        .layout_no_wrap(text.to_string(), font, Color32::WHITE)
        .size()
        .x
}

/// A small explanation under a control.
pub fn hint(ui: &mut Ui, text: impl Into<String>) {
    ui.label(RichText::new(text.into()).small().weak());
}

/// Background for the sidebar and tab strip (slightly different from the content area).
pub fn chrome_fill(dark: bool) -> Color32 {
    if dark {
        Color32::from_rgb(0x19, 0x1b, 0x20)
    } else {
        Color32::from_rgb(0xe9, 0xeb, 0xee)
    }
}

/// A small ring that fills clockwise with `fraction`.
pub fn progress_ring(
    painter: &egui::Painter,
    center: egui::Pos2,
    radius: f32,
    fraction: f32,
    dark: bool,
) {
    let track = if dark {
        Color32::from_gray(80)
    } else {
        Color32::from_gray(200)
    };
    painter.circle_stroke(center, radius, egui::Stroke::new(2.0, track));
    let steps = (fraction.clamp(0.0, 1.0) * 32.0).ceil() as usize;
    if steps > 0 {
        let points: Vec<egui::Pos2> = (0..=steps)
            .map(|i| {
                let a = -std::f32::consts::FRAC_PI_2
                    + std::f32::consts::TAU * fraction.clamp(0.0, 1.0) * i as f32 / steps as f32;
                center + egui::vec2(a.cos(), a.sin()) * radius
            })
            .collect();
        painter.add(egui::Shape::line(points, egui::Stroke::new(2.0, ACCENT)));
    }
}
