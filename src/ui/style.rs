//! Design tokens (colors, type, spacing), the egui style built from them, and the form helpers
//! every dialog uses. Shared components (buttons, tabs, pills, empty states) are in `kit.rs`.
//!
//! Every screen draws with these tokens: [`Palette`] for colors (light and dark), [`medium`],
//! [`semibold`] and [`icon_font`] for type. The accent is the indigo of the app icon.

use eframe::egui::{
    self, Align, Color32, CornerRadius, FontFamily, FontId, Layout, Margin, Shadow, Stroke, Theme,
    ThemePreference, Ui, vec2,
};

use super::fonts;
use crate::settings::ThemeMode;

/// Colors of one theme.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    /// Content area: the terminal's surroundings, file lists, the welcome screen.
    pub canvas: Color32,
    /// Window chrome: sidebar, tab strip, status bar.
    pub chrome: Color32,
    /// Raised surfaces: inputs, cards, dialogs, menus.
    pub surface: Color32,
    /// Hovered rows and quiet button fills.
    pub hover: Color32,
    /// Pressed state and subtle fills (segmented control track, count pills).
    pub subtle: Color32,
    pub border: Color32,
    pub border_strong: Color32,
    pub text: Color32,
    pub text_muted: Color32,
    pub text_faint: Color32,
    pub accent: Color32,
    pub accent_hover: Color32,
    /// Text on an accent fill.
    pub on_accent: Color32,
    /// Background of selected rows and "on" toggles.
    pub accent_soft: Color32,
    /// Accent-colored text (readable on both the canvas and `accent_soft`).
    pub accent_text: Color32,
    pub ok: Color32,
    pub warn: Color32,
    pub err: Color32,
    pub info: Color32,
    /// Folder icons in file lists and the sidebar.
    pub folder: Color32,
}

const LIGHT: Palette = Palette {
    // The background of the "Peler Light" terminal scheme, so the terminal blends in.
    canvas: Color32::from_rgb(0xfb, 0xfb, 0xfc),
    chrome: Color32::from_rgb(0xf5, 0xf5, 0xf7),
    surface: Color32::from_rgb(0xff, 0xff, 0xff),
    hover: Color32::from_rgb(0xef, 0xef, 0xf3),
    subtle: Color32::from_rgb(0xe8, 0xe8, 0xee),
    border: Color32::from_rgb(0xe3, 0xe3, 0xe9),
    border_strong: Color32::from_rgb(0xcf, 0xcf, 0xd8),
    text: Color32::from_rgb(0x1a, 0x1a, 0x20),
    text_muted: Color32::from_rgb(0x68, 0x68, 0x74),
    text_faint: Color32::from_rgb(0x96, 0x96, 0xa2),
    accent: Color32::from_rgb(0x6d, 0x5a, 0xe6),
    accent_hover: Color32::from_rgb(0x5b, 0x48, 0xd6),
    on_accent: Color32::WHITE,
    accent_soft: Color32::from_rgb(0xee, 0xeb, 0xfd),
    accent_text: Color32::from_rgb(0x56, 0x41, 0xd0),
    ok: Color32::from_rgb(0x16, 0x8c, 0x4e),
    warn: Color32::from_rgb(0xb4, 0x70, 0x00),
    err: Color32::from_rgb(0xd6, 0x30, 0x31),
    info: Color32::from_rgb(0x25, 0x63, 0xdc),
    folder: Color32::from_rgb(0x5b, 0x6e, 0xe8),
};

const DARK: Palette = Palette {
    // The background of the "Peler Dark" terminal scheme, so the terminal blends in.
    canvas: Color32::from_rgb(0x17, 0x19, 0x1e),
    chrome: Color32::from_rgb(0x12, 0x13, 0x17),
    surface: Color32::from_rgb(0x1f, 0x20, 0x26),
    hover: Color32::from_rgb(0x26, 0x27, 0x2e),
    subtle: Color32::from_rgb(0x2c, 0x2d, 0x35),
    border: Color32::from_rgb(0x2b, 0x2c, 0x33),
    border_strong: Color32::from_rgb(0x3d, 0x3f, 0x48),
    text: Color32::from_rgb(0xe8, 0xe8, 0xec),
    text_muted: Color32::from_rgb(0x9e, 0x9f, 0xa8),
    text_faint: Color32::from_rgb(0x6e, 0x6f, 0x78),
    accent: Color32::from_rgb(0x8b, 0x7c, 0xf6),
    accent_hover: Color32::from_rgb(0xa0, 0x94, 0xf8),
    on_accent: Color32::WHITE,
    accent_soft: Color32::from_rgb(0x2a, 0x25, 0x47),
    accent_text: Color32::from_rgb(0xb4, 0xaa, 0xff),
    ok: Color32::from_rgb(0x4a, 0xcd, 0x80),
    warn: Color32::from_rgb(0xf0, 0xb4, 0x48),
    err: Color32::from_rgb(0xf8, 0x68, 0x64),
    info: Color32::from_rgb(0x6e, 0xa0, 0xff),
    folder: Color32::from_rgb(0x8b, 0x9c, 0xf8),
};

impl Palette {
    /// Barely visible fill for hovered list rows.
    pub fn faint_row(&self) -> Color32 {
        soft(self.hover, 0.6)
    }
}

pub fn palette(dark: bool) -> Palette {
    if dark { DARK } else { LIGHT }
}

/// The palette of the theme `ui` is drawn with.
pub fn pal(ui: &Ui) -> Palette {
    palette(ui.visuals().dark_mode)
}

/// A color at `alpha` (0–1) opacity, for soft fills behind colored text and icons.
pub fn soft(c: Color32, alpha: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(
        c.r(),
        c.g(),
        c.b(),
        (alpha.clamp(0.0, 1.0) * 255.0) as u8,
    )
}

// ---------------------------------------------------------------------------------------------
// Type
// ---------------------------------------------------------------------------------------------

/// Body text size of the UI.
pub const BODY: f32 = 13.5;
/// Small text: details, hints, the status bar.
pub const SMALL: f32 = 11.5;

/// Inter Medium: labels that need a little weight (tabs, buttons, row titles).
pub fn medium(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(fonts::MEDIUM.into()))
}

/// Inter SemiBold: titles and headings.
pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(fonts::SEMIBOLD.into()))
}

/// The icon font alone (icons also render inside ordinary text).
pub fn icon_font(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(fonts::ICONS.into()))
}

// ---------------------------------------------------------------------------------------------
// Style
// ---------------------------------------------------------------------------------------------

pub const RADIUS: u8 = 6;
pub const RADIUS_LG: u8 = 10;

pub fn apply(ctx: &egui::Context, mode: ThemeMode) {
    ctx.set_theme(match mode {
        ThemeMode::System => ThemePreference::System,
        ThemeMode::Light => ThemePreference::Light,
        ThemeMode::Dark => ThemePreference::Dark,
    });
    for theme in [Theme::Dark, Theme::Light] {
        ctx.style_mut_of(theme, |style| {
            let dark = theme == Theme::Dark;
            let p = palette(dark);
            let s = &mut style.spacing;
            s.item_spacing = vec2(8.0, 6.0);
            s.button_padding = vec2(10.0, 4.0);
            s.interact_size = vec2(24.0, 26.0);
            s.window_margin = Margin::same(20);
            s.menu_margin = Margin::same(6);
            s.indent = 16.0;
            s.icon_width = 15.0;
            s.icon_width_inner = 9.0;
            s.icon_spacing = 6.0;
            s.combo_height = 320.0;
            s.tooltip_width = 360.0;
            s.menu_width = 220.0;
            s.scroll = egui::style::ScrollStyle::floating();
            s.scroll.bar_width = 8.0;
            s.scroll.floating_width = 3.0;
            s.scroll.floating_allocated_width = 0.0;
            // Selectable labels sense clicks and drags and would steal them from the rows,
            // buttons and tables they sit in; text that should be copyable opts in explicitly.
            style.interaction.selectable_labels = false;

            let v = &mut style.visuals;
            v.text_cursor.blink = false; // a blinking caret would repaint twice a second
            v.text_cursor.stroke = Stroke::new(1.5, p.accent);
            v.panel_fill = p.canvas;
            v.window_fill = p.surface;
            v.extreme_bg_color = p.surface;
            v.text_edit_bg_color = Some(p.surface);
            v.faint_bg_color = if dark {
                Color32::from_rgb(0x1b, 0x1c, 0x21)
            } else {
                Color32::from_rgb(0xf7, 0xf7, 0xf9)
            };
            v.code_bg_color = p.subtle;
            v.hyperlink_color = p.accent_text;
            v.warn_fg_color = p.warn;
            v.error_fg_color = p.err;
            v.override_text_color = None;
            v.weak_text_color = Some(p.text_muted);
            v.selection.bg_fill = soft(p.accent, if dark { 0.30 } else { 0.18 });
            // The text color of selected rows and items, and egui's ring around a focused text
            // field (`Field` paints a softer accent ring over it).
            v.selection.stroke = Stroke::new(1.0, p.accent_text);
            v.window_corner_radius = CornerRadius::same(RADIUS_LG);
            v.menu_corner_radius = CornerRadius::same(8);
            v.window_stroke = Stroke::new(1.0, p.border);
            v.window_shadow = Shadow {
                offset: [0, 10],
                blur: 30,
                spread: 0,
                color: Color32::from_black_alpha(if dark { 120 } else { 38 }),
            };
            v.popup_shadow = Shadow {
                offset: [0, 4],
                blur: 14,
                spread: 0,
                color: Color32::from_black_alpha(if dark { 100 } else { 28 }),
            };
            v.window_highlight_topmost = false;
            v.collapsing_header_frame = false;
            v.indent_has_left_vline = true;
            v.striped = false;
            v.slider_trailing_fill = true;
            v.handle_shape = egui::style::HandleShape::Circle;
            v.disabled_alpha = 0.45;

            let radius = CornerRadius::same(RADIUS);
            let w = &mut v.widgets;
            // Labels, separators, frames.
            w.noninteractive.bg_fill = p.surface;
            w.noninteractive.weak_bg_fill = p.surface;
            w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
            w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
            w.noninteractive.corner_radius = radius;
            // Buttons, inputs, checkboxes at rest: an outlined surface.
            w.inactive.bg_fill = p.subtle;
            w.inactive.weak_bg_fill = p.surface;
            w.inactive.bg_stroke = Stroke::new(1.0, p.border_strong);
            w.inactive.fg_stroke = Stroke::new(1.0, p.text);
            w.inactive.corner_radius = radius;
            w.inactive.expansion = 0.0;
            w.hovered.bg_fill = p.subtle;
            w.hovered.weak_bg_fill = p.hover;
            w.hovered.bg_stroke = Stroke::new(
                1.0,
                if dark {
                    p.border_strong
                } else {
                    Color32::from_rgb(0xbe, 0xbe, 0xc8)
                },
            );
            w.hovered.fg_stroke = Stroke::new(1.0, p.text);
            w.hovered.corner_radius = radius;
            w.hovered.expansion = 0.0;
            w.active.bg_fill = p.subtle;
            w.active.weak_bg_fill = p.subtle;
            w.active.bg_stroke = Stroke::new(1.0, p.accent);
            w.active.fg_stroke = Stroke::new(1.0, p.text);
            w.active.corner_radius = radius;
            w.active.expansion = 0.0;
            w.open.bg_fill = p.subtle;
            w.open.weak_bg_fill = p.hover;
            w.open.bg_stroke = Stroke::new(1.0, p.border_strong);
            w.open.fg_stroke = Stroke::new(1.0, p.text);
            w.open.corner_radius = radius;

            for (ts, font) in style.text_styles.iter_mut() {
                *font = match ts {
                    egui::TextStyle::Heading => semibold(18.0),
                    egui::TextStyle::Small => FontId::proportional(SMALL),
                    egui::TextStyle::Monospace => FontId::monospace(12.5),
                    _ => FontId::proportional(BODY),
                };
            }
        });
    }
}

/// Warning text: amber, dark enough to read on a light background.
pub fn warn_text(v: &egui::Visuals) -> Color32 {
    palette(v.dark_mode).warn
}

/// The accent ring around a focused checkbox.
pub fn focus_ring(v: &egui::Visuals) -> Stroke {
    Stroke::new(1.5, palette(v.dark_mode).accent)
}

/// Focus of an input drawn by hand: a 1 px accent border and a soft accent halo just outside it
/// (calmer than a thick ring, which reads like an error).
pub fn focus_halo(ui: &Ui, rect: egui::Rect, radius: u8) {
    let p = pal(ui);
    let halo = soft(p.accent, if ui.visuals().dark_mode { 0.30 } else { 0.22 });
    let painter = ui.painter();
    painter.rect_stroke(
        rect,
        CornerRadius::same(radius),
        Stroke::new(3.0, halo),
        egui::StrokeKind::Outside,
    );
    painter.rect_stroke(
        rect,
        CornerRadius::same(radius),
        Stroke::new(1.0, p.accent),
        egui::StrokeKind::Inside,
    );
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

/// A text field with a soft accent focus ring painted over egui's own.
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
            focus_halo(ui, response.rect.expand(active.expansion), RADIUS);
        }
        response
    }
}

/// The main button of a dialog (filled accent).
pub fn primary(text: impl Into<String>) -> Primary {
    Primary(text.into())
}

/// See [`primary`]: drawn by `kit::button_ex` once the theme is known.
pub struct Primary(String);

impl egui::Widget for Primary {
    fn ui(self, ui: &mut Ui) -> egui::Response {
        super::kit::button_ex(ui, super::kit::Kind::Primary, None, &self.0, false)
    }
}

/// A section title in a form.
pub fn section(ui: &mut Ui, title: &str) {
    ui.add_space(12.0);
    ui.label(
        egui::RichText::new(title)
            .font(semibold(13.0))
            .color(pal(ui).text),
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
                ui.label(egui::RichText::new(label).color(pal(ui).text_muted));
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

/// A segmented control: one of `options`, the chosen one raised on a quiet track. Returns each
/// segment's response.
pub fn segmented<T: PartialEq + Copy>(
    ui: &mut Ui,
    value: &mut T,
    options: &[(T, &str)],
) -> Vec<egui::Response> {
    let p = pal(ui);
    let font = medium(SMALL + 1.0);
    let galleys: Vec<_> = options
        .iter()
        .map(|(_, label)| {
            ui.painter()
                .layout_no_wrap(label.to_string(), font.clone(), Color32::PLACEHOLDER)
        })
        .collect();
    let seg_w: Vec<f32> = galleys.iter().map(|g| g.size().x + 24.0).collect();
    let total = seg_w.iter().sum::<f32>() + 4.0;
    let (rect, base) = ui.allocate_exact_size(vec2(total, CONTROL_H), egui::Sense::hover());
    ui.painter()
        .rect_filled(rect, CornerRadius::same(RADIUS + 1), p.subtle);
    let mut x = rect.left() + 2.0;
    let mut responses = Vec::with_capacity(options.len());
    for (i, ((option, label), galley)) in options.iter().zip(galleys).enumerate() {
        let seg = egui::Rect::from_min_size(
            egui::pos2(x, rect.top() + 2.0),
            vec2(seg_w[i], rect.height() - 4.0),
        );
        x += seg_w[i];
        let resp = ui.interact(seg, base.id.with(("segment", i)), egui::Sense::click());
        let selected = *value == *option;
        resp.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::RadioButton, true, selected, *label)
        });
        if resp.clicked() {
            *value = *option;
        }
        let painter = ui.painter();
        if selected {
            painter.rect(
                seg,
                CornerRadius::same(RADIUS - 1),
                p.surface,
                Stroke::new(1.0, p.border),
                egui::StrokeKind::Inside,
            );
        } else if resp.hovered() {
            painter.rect_filled(seg, CornerRadius::same(RADIUS - 1), p.hover);
        }
        let color = if selected || resp.hovered() {
            p.text
        } else {
            p.text_muted
        };
        painter.galley(seg.center() - galley.size() / 2.0, galley, color);
        responses.push(resp);
    }
    responses
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

/// A checkbox: an accent box with a white check when on.
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
        let p = pal(ui);
        let box_rect = egui::Rect::from_center_size(
            egui::pos2(rect.left() + BOX / 2.0, rect.top() + line / 2.0),
            vec2(BOX, BOX),
        );
        let painter = ui.painter();
        if on {
            let fill = if response.hovered() {
                p.accent_hover
            } else {
                p.accent
            };
            painter.rect_filled(box_rect, 4.0, fill);
            painter.text(
                box_rect.center(),
                egui::Align2::CENTER_CENTER,
                super::icons::CHECK,
                icon_font(12.0),
                p.on_accent,
            );
        } else {
            painter.rect(
                box_rect,
                4.0,
                p.surface,
                Stroke::new(
                    1.0,
                    if response.hovered() {
                        p.text_faint
                    } else {
                        p.border_strong
                    },
                ),
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
        let text_color = if ui.is_enabled() {
            p.text
        } else {
            p.text_faint
        };
        painter.galley(text_pos, galley, text_color);
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
    ui.label(
        egui::RichText::new(text.into())
            .small()
            .color(pal(ui).text_muted),
    );
}

/// A small ring that fills clockwise with `fraction`.
pub fn progress_ring(
    painter: &egui::Painter,
    center: egui::Pos2,
    radius: f32,
    fraction: f32,
    dark: bool,
) {
    let p = palette(dark);
    painter.circle_stroke(center, radius, Stroke::new(2.0, p.border_strong));
    let steps = (fraction.clamp(0.0, 1.0) * 32.0).ceil() as usize;
    if steps > 0 {
        let points: Vec<egui::Pos2> = (0..=steps)
            .map(|i| {
                let a = -std::f32::consts::FRAC_PI_2
                    + std::f32::consts::TAU * fraction.clamp(0.0, 1.0) * i as f32 / steps as f32;
                center + vec2(a.cos(), a.sin()) * radius
            })
            .collect();
        painter.add(egui::Shape::line(points, Stroke::new(2.0, p.accent)));
    }
}
