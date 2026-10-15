//! The terminal widget: draws an alacritty grid with egui's painter and turns keyboard, mouse,
//! clipboard and IME events into bytes for the remote.

use std::time::Duration;

use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{self, TermMode};
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor};
use eframe::egui::emath::GuiRounding as _;
use eframe::egui::{
    self, Color32, CornerRadius, Event, EventFilter, FontFamily, FontId, Id, ImeEvent, Key,
    MouseWheelUnit, PointerButton, Pos2, Rect, Sense, Stroke, StrokeKind, Ui, Vec2, pos2, vec2,
};

use crate::term::core::{self as tcore, GridSize, TermShared};
use crate::term::input::{self, KeyModes, Mods, MouseAction, MouseButton};
use crate::term::scheme::{Scheme, xterm_256};
use crate::ui::fonts;

pub const TERM_FILTER: EventFilter = EventFilter {
    tab: true,
    horizontal_arrows: true,
    vertical_arrows: true,
    escape: true,
};
pub const PAD: f32 = 6.0;
const DIM_FACTOR: f32 = 0.66;

pub struct TermStyle<'a> {
    pub font_size: f32,
    pub cell: Vec2,
    pub scheme: &'a Scheme,
    pub real_bold: bool,
    pub real_italic: bool,
    pub option_as_meta: bool,
    pub copy_on_select: bool,
    pub cursor_blink: bool,
}

/// Per-pane widget state that survives between frames.
#[derive(Default)]
pub struct TermView {
    pub id: Option<Id>,
    preedit: String,
    scroll_accum: f32,
    suppress_text: bool,
    reported_button: Option<MouseButton>,
    last_report_cell: Option<(usize, usize)>,
    pub grid: Option<GridSize>,
    blink_epoch: f64,
    was_focused: bool,
}

impl TermView {
    pub fn new(id: Id) -> Self {
        Self {
            id: Some(id),
            ..Default::default()
        }
    }
}

/// The widget id of a pane's terminal (stable, so focus can be moved to it).
pub fn pane_widget_id(pane: u64) -> Id {
    Id::new(("terminal", pane))
}

#[derive(Default)]
pub struct TermResponse {
    /// Bytes for the remote (keys, paste, mouse reports, focus events).
    pub input: Vec<u8>,
    /// New grid size and pixel size when the pane was resized.
    pub resized: Option<(GridSize, u32, u32)>,
    pub focused: bool,
    /// The user clicked into the pane (make it the active pane).
    pub activated: bool,
    /// Enter pressed in a pane whose session ended.
    pub reconnect: bool,
}

pub fn cell_size(ctx: &egui::Context, font_size: f32) -> Vec2 {
    let font = FontId::new(font_size, FontFamily::Name(fonts::TERM_REGULAR.into()));
    ctx.fonts_mut(|f| vec2(f.glyph_width(&font, 'M'), f.row_height(&font)))
}

fn rgb(c: alacritty_terminal::vte::ansi::Rgb) -> Color32 {
    Color32::from_rgb(c.r, c.g, c.b)
}

fn mul(c: Color32, f: f32) -> Color32 {
    Color32::from_rgb(
        (c.r() as f32 * f) as u8,
        (c.g() as f32 * f) as u8,
        (c.b() as f32 * f) as u8,
    )
}

/// Palette lookup honouring OSC 4/10/11/12 overrides.
fn palette(idx: usize, colors: &Colors, scheme: &Scheme) -> Color32 {
    if let Some(c) = colors[idx] {
        return rgb(c);
    }
    match idx {
        0..=15 => scheme.ansi[idx],
        16..=255 => xterm_256(idx as u8),
        256 | 267 => scheme.foreground,
        257 => scheme.background,
        258 => scheme.cursor,
        259..=266 => mul(palette(idx - 259, colors, scheme), DIM_FACTOR),
        268 => mul(palette(256, colors, scheme), DIM_FACTOR),
        _ => scheme.foreground,
    }
}

fn resolve_fg(color: Color, flags: Flags, colors: &Colors, scheme: &Scheme) -> Color32 {
    let dim = flags.contains(Flags::DIM);
    match color {
        Color::Spec(c) => {
            if dim {
                mul(rgb(c), DIM_FACTOR)
            } else {
                rgb(c)
            }
        }
        Color::Named(n) => palette(if dim { n.to_dim() } else { n } as usize, colors, scheme),
        Color::Indexed(i) => {
            let i = i as usize;
            let idx = if dim && !flags.contains(Flags::BOLD) {
                match i {
                    0..=7 => NamedColor::DimBlack as usize + i,
                    8..=15 => i - 8,
                    _ => i,
                }
            } else {
                i
            };
            palette(idx, colors, scheme)
        }
    }
}

/// `None` means the default background (not painted per cell).
fn resolve_bg(color: Color, colors: &Colors, scheme: &Scheme) -> Option<Color32> {
    match color {
        Color::Named(NamedColor::Background) => None,
        Color::Spec(c) => Some(rgb(c)),
        Color::Named(n) => Some(palette(n as usize, colors, scheme)),
        Color::Indexed(i) => Some(palette(i as usize, colors, scheme)),
    }
}

#[derive(Clone, Copy, PartialEq)]
struct RunStyle {
    fg: Color32,
    bold: bool,
    italic: bool,
    underline: u8, // 0 none, 1 single, 2 double, 3 curly/dotted/dashed
    underline_color: Option<Color32>,
    strike: bool,
}

struct TextRun {
    col: usize,
    text: String,
    style: RunStyle,
}

/// Box-drawing weights per direction: 0 none, 1 light, 2 heavy, 3 double. (up, right, down, left)
fn box_lines(c: char) -> Option<[u8; 4]> {
    Some(match c {
        '─' => [0, 1, 0, 1],
        '━' => [0, 2, 0, 2],
        '│' => [1, 0, 1, 0],
        '┃' => [2, 0, 2, 0],
        '┌' | '╭' => [0, 1, 1, 0],
        '┐' | '╮' => [0, 0, 1, 1],
        '└' | '╰' => [1, 1, 0, 0],
        '┘' | '╯' => [1, 0, 0, 1],
        '├' => [1, 1, 1, 0],
        '┤' => [1, 0, 1, 1],
        '┬' => [0, 1, 1, 1],
        '┴' => [1, 1, 0, 1],
        '┼' => [1, 1, 1, 1],
        '┏' => [0, 2, 2, 0],
        '┓' => [0, 0, 2, 2],
        '┗' => [2, 2, 0, 0],
        '┛' => [2, 0, 0, 2],
        '┣' => [2, 2, 2, 0],
        '┫' => [2, 0, 2, 2],
        '┳' => [0, 2, 2, 2],
        '┻' => [2, 2, 0, 2],
        '╋' => [2, 2, 2, 2],
        '═' => [0, 3, 0, 3],
        '║' => [3, 0, 3, 0],
        '╔' => [0, 3, 3, 0],
        '╗' => [0, 0, 3, 3],
        '╚' => [3, 3, 0, 0],
        '╝' => [3, 0, 0, 3],
        '╠' => [3, 3, 3, 0],
        '╣' => [3, 0, 3, 3],
        '╦' => [0, 3, 3, 3],
        '╩' => [3, 3, 0, 3],
        '╬' => [3, 3, 3, 3],
        '╴' => [0, 0, 0, 1],
        '╵' => [1, 0, 0, 0],
        '╶' => [0, 1, 0, 0],
        '╷' => [0, 0, 1, 0],
        _ => return None,
    })
}

/// Draws box-drawing and block characters as shapes so lines join seamlessly across cells.
/// Returns false for characters it does not handle.
fn draw_special(painter: &egui::Painter, c: char, r: Rect, color: Color32, ppp: f32) -> bool {
    let px = 1.0 / ppp;
    let snap = |v: f32| (v * ppp).round() / ppp;
    if let Some(lines) = box_lines(c) {
        let cx = snap(r.center().x);
        let cy = snap(r.center().y);
        let light = px.max(1.0);
        for (dir, weight) in lines.iter().enumerate() {
            if *weight == 0 {
                continue;
            }
            let w = if *weight == 2 { light * 2.0 } else { light };
            let segs: Vec<f32> = if *weight == 3 {
                vec![-light * 1.5, light * 1.5]
            } else {
                vec![0.0]
            };
            for off in segs {
                let rect = match dir {
                    0 => Rect::from_min_max(
                        pos2(cx + off - w / 2.0, r.top()),
                        pos2(cx + off + w / 2.0, cy + w / 2.0),
                    ),
                    1 => Rect::from_min_max(
                        pos2(cx - w / 2.0, cy + off - w / 2.0),
                        pos2(r.right(), cy + off + w / 2.0),
                    ),
                    2 => Rect::from_min_max(
                        pos2(cx + off - w / 2.0, cy - w / 2.0),
                        pos2(cx + off + w / 2.0, r.bottom()),
                    ),
                    _ => Rect::from_min_max(
                        pos2(r.left(), cy + off - w / 2.0),
                        pos2(cx + w / 2.0, cy + off + w / 2.0),
                    ),
                };
                painter.rect_filled(rect, CornerRadius::ZERO, color);
            }
        }
        return true;
    }
    let frac = |f: f32, vertical: bool| -> Rect {
        if vertical {
            Rect::from_min_max(pos2(r.left(), snap(r.bottom() - r.height() * f)), r.max)
        } else {
            Rect::from_min_max(r.min, pos2(snap(r.left() + r.width() * f), r.bottom()))
        }
    };
    let fill = |rect: Rect, col: Color32| {
        painter.rect_filled(rect, CornerRadius::ZERO, col);
    };
    match c {
        '█' => fill(r, color),
        '▀' => fill(
            Rect::from_min_max(r.min, pos2(r.right(), snap(r.center().y))),
            color,
        ),
        '▄' => fill(frac(0.5, true), color),
        '▌' => fill(frac(0.5, false), color),
        '▐' => fill(
            Rect::from_min_max(pos2(snap(r.center().x), r.top()), r.max),
            color,
        ),
        '░' => fill(r, color.gamma_multiply(0.25)),
        '▒' => fill(r, color.gamma_multiply(0.5)),
        '▓' => fill(r, color.gamma_multiply(0.75)),
        '▁'..='▇' => fill(frac((c as u32 - '▁' as u32 + 1) as f32 / 8.0, true), color),
        '▉'..='▏' => fill(
            frac((8 - (c as u32 - '▉' as u32 + 1)) as f32 / 8.0, false),
            color,
        ),
        _ => return false,
    }
    true
}

fn family(bold: bool, italic: bool) -> FontFamily {
    FontFamily::Name(
        match (bold, italic) {
            (false, false) => fonts::TERM_REGULAR,
            (true, false) => fonts::TERM_BOLD,
            (false, true) => fonts::TERM_ITALIC,
            (true, true) => fonts::TERM_BOLD_ITALIC,
        }
        .into(),
    )
}

/// Viewport cell under a screen position, clamped to the grid.
fn cell_at(pos: Pos2, origin: Pos2, cell: Vec2, size: GridSize) -> (usize, usize, Side) {
    let x = (pos.x - origin.x).max(0.0);
    let y = (pos.y - origin.y).max(0.0);
    let col = ((x / cell.x) as usize).min(size.cols - 1);
    let row = ((y / cell.y) as usize).min(size.rows - 1);
    let side = if x - col as f32 * cell.x > cell.x / 2.0 {
        Side::Right
    } else {
        Side::Left
    };
    (col, row, side)
}

fn grid_point(offset: usize, col: usize, row: usize) -> Point {
    term::viewport_to_point(offset, Point::new(row, Column(col)))
}

fn mods_of(m: egui::Modifiers) -> Mods {
    Mods {
        shift: m.shift,
        alt: m.alt,
        ctrl: m.ctrl,
    }
}

#[allow(clippy::too_many_arguments)]
pub fn show(
    ui: &mut Ui,
    rect: Rect,
    view: &mut TermView,
    shared: &TermShared,
    style: &TermStyle,
    active: bool,
    input_enabled: bool,
    alive: bool,
) -> TermResponse {
    let mut out = TermResponse::default();
    let id = *view.id.get_or_insert_with(|| {
        Id::new((
            "terminal",
            rect.min.x as i64,
            rect.min.y as i64,
            ui.next_auto_id(),
        ))
    });
    let ppp = ui.ctx().pixels_per_point();
    let cell = style.cell;
    let origin = (rect.min + vec2(PAD, PAD)).round_to_pixels(ppp);
    let size = GridSize {
        cols: (((rect.width() - 2.0 * PAD) / cell.x).floor() as usize).max(tcore::MIN_COLS),
        rows: (((rect.height() - 2.0 * PAD) / cell.y).floor() as usize).max(tcore::MIN_ROWS),
    };
    if view.grid != Some(size) {
        view.grid = Some(size);
        tcore::resize(shared, size, cell.x, cell.y);
        out.resized = Some((
            size,
            (size.cols as f32 * cell.x * ppp) as u32,
            (size.rows as f32 * cell.y * ppp) as u32,
        ));
    }

    // ----- focus -------------------------------------------------------------------------
    let response = ui.interact(rect, id, Sense::click_and_drag());
    if response.clicked_by(PointerButton::Primary)
        || response.drag_started_by(PointerButton::Primary)
        || response.clicked_by(PointerButton::Secondary)
        || response.clicked_by(PointerButton::Middle)
    {
        out.activated = true;
        if input_enabled {
            ui.memory_mut(|m| m.request_focus(id));
        }
    }
    if active && input_enabled && ui.memory(|m| m.focused().is_none()) {
        ui.memory_mut(|m| m.request_focus(id));
    }
    let focused = input_enabled && ui.memory(|m| m.has_focus(id));
    let window_focused = ui.input(|i| i.focused);
    out.focused = focused;
    if focused {
        ui.memory_mut(|m| m.set_focus_lock_filter(id, TERM_FILTER));
    }

    let mut term = shared.term.lock();
    let mode = *term.mode();
    term.is_focused = focused && window_focused;

    // Focus reporting (DEC 1004).
    let has_focus_now = focused && window_focused;
    if has_focus_now != view.was_focused {
        if mode.contains(TermMode::FOCUS_IN_OUT) && alive {
            out.input
                .extend_from_slice(if has_focus_now { b"\x1b[I" } else { b"\x1b[O" });
        }
        view.was_focused = has_focus_now;
    }

    // ----- keyboard ----------------------------------------------------------------------
    if focused {
        let events = ui.input(|i| i.filtered_events(&TERM_FILTER));
        let modes = KeyModes {
            app_cursor: mode.contains(TermMode::APP_CURSOR),
            option_as_meta: style.option_as_meta,
        };
        let mut typed = Vec::new();
        for event in events {
            match event {
                Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    if modifiers.command {
                        continue; // app shortcuts
                    }
                    let mods = mods_of(modifiers);
                    if modifiers.shift && !mode.contains(TermMode::ALT_SCREEN) {
                        let scroll = match key {
                            Key::PageUp => Some(Scroll::PageUp),
                            Key::PageDown => Some(Scroll::PageDown),
                            Key::Home => Some(Scroll::Top),
                            Key::End => Some(Scroll::Bottom),
                            _ => None,
                        };
                        if let Some(s) = scroll {
                            term.scroll_display(s);
                            continue;
                        }
                    }
                    if !alive {
                        if key == Key::Enter {
                            out.reconnect = true;
                        }
                        continue;
                    }
                    view.suppress_text = input::key_consumes_text(key, mods, modes);
                    if let Some(bytes) = input::encode_key(key, mods, modes) {
                        typed.extend(bytes);
                    }
                }
                Event::Text(text) => {
                    if std::mem::take(&mut view.suppress_text) || !alive {
                        continue;
                    }
                    typed.extend_from_slice(text.as_bytes());
                }
                Event::Paste(text) if alive => {
                    typed.extend(input::encode_paste(
                        &text,
                        mode.contains(TermMode::BRACKETED_PASTE),
                    ));
                }
                Event::Copy | Event::Cut => {
                    if let Some(text) = term.selection_to_string() {
                        ui.ctx().copy_text(text);
                    }
                }
                Event::Ime(ImeEvent::Preedit { text, .. }) => view.preedit = text,
                Event::Ime(ImeEvent::Commit(text)) => {
                    view.preedit.clear();
                    if alive {
                        typed.extend_from_slice(text.as_bytes());
                    }
                }
                _ => {}
            }
        }
        if !typed.is_empty() {
            term.scroll_display(Scroll::Bottom);
            term.selection = None;
            view.blink_epoch = ui.input(|i| i.time);
            out.input.extend(typed);
        }
    }

    // ----- mouse -------------------------------------------------------------------------
    let offset = term.grid().display_offset();
    let hover = response.hover_pos();
    let mouse_mode = mode.intersects(TermMode::MOUSE_MODE) && alive;
    let shift_held = ui.input(|i| i.modifiers.shift);
    let sgr = mode.contains(TermMode::SGR_MOUSE);
    if mouse_mode && !shift_held && input_enabled {
        let events = ui.input(|i| i.events.clone());
        for event in events {
            match event {
                Event::PointerButton {
                    pos,
                    button,
                    pressed,
                    modifiers,
                } => {
                    let b = match button {
                        PointerButton::Primary => MouseButton::Left,
                        PointerButton::Middle => MouseButton::Middle,
                        PointerButton::Secondary => MouseButton::Right,
                        _ => continue,
                    };
                    // Only presses that land on the terminal itself, not on a menu, popup or
                    // dialog drawn over it.
                    if pressed && (!rect.contains(pos) || !on_terminal_layer(ui, pos)) {
                        continue;
                    }
                    if !pressed && view.reported_button != Some(b) {
                        continue;
                    }
                    let (col, row, _) = cell_at(pos, origin, cell, size);
                    let action = if pressed {
                        MouseAction::Press
                    } else {
                        MouseAction::Release
                    };
                    view.reported_button = pressed.then_some(b);
                    view.last_report_cell = Some((col, row));
                    if let Some(bytes) =
                        input::encode_mouse(b, action, mods_of(modifiers), col, row, sgr)
                    {
                        out.input.extend(bytes);
                    }
                }
                Event::PointerMoved(pos) => {
                    let report = match view.reported_button {
                        Some(_) => mode.intersects(TermMode::MOUSE_DRAG | TermMode::MOUSE_MOTION),
                        None => {
                            mode.contains(TermMode::MOUSE_MOTION)
                                && rect.contains(pos)
                                && on_terminal_layer(ui, pos)
                        }
                    };
                    if !report {
                        continue;
                    }
                    let (col, row, _) = cell_at(pos, origin, cell, size);
                    if view.last_report_cell == Some((col, row)) {
                        continue;
                    }
                    view.last_report_cell = Some((col, row));
                    let b = view.reported_button.unwrap_or(MouseButton::None);
                    let mods = mods_of(ui.input(|i| i.modifiers));
                    if let Some(bytes) =
                        input::encode_mouse(b, MouseAction::Motion, mods, col, row, sgr)
                    {
                        out.input.extend(bytes);
                    }
                }
                _ => {}
            }
        }
    } else if input_enabled {
        let press_origin = ui.input(|i| i.pointer.press_origin());
        if response.drag_started_by(PointerButton::Primary)
            && let Some(p) = press_origin
        {
            let (col, row, side) = cell_at(p, origin, cell, size);
            let ty = if ui.input(|i| i.modifiers.alt) {
                SelectionType::Block
            } else {
                SelectionType::Simple
            };
            term.selection = Some(Selection::new(ty, grid_point(offset, col, row), side));
        }
        if response.dragged_by(PointerButton::Primary)
            && let Some(p) = response.interact_pointer_pos()
        {
            // Auto-scroll while dragging past the edges.
            if p.y < rect.top() {
                term.scroll_display(Scroll::Delta(1));
                ui.ctx().request_repaint_after(Duration::from_millis(30));
            } else if p.y > rect.bottom() {
                term.scroll_display(Scroll::Delta(-1));
                ui.ctx().request_repaint_after(Duration::from_millis(30));
            }
            let offset = term.grid().display_offset();
            let (col, row, side) = cell_at(p, origin, cell, size);
            if let Some(sel) = term.selection.as_mut() {
                sel.update(grid_point(offset, col, row), side);
            }
        }
        if response.drag_stopped()
            && style.copy_on_select
            && let Some(text) = term.selection_to_string()
        {
            ui.ctx().copy_text(text);
        }
        if let Some(p) = hover {
            let (col, row, side) = cell_at(p, origin, cell, size);
            if response.triple_clicked() {
                term.selection = Some(Selection::new(
                    SelectionType::Lines,
                    grid_point(offset, col, row),
                    side,
                ));
            } else if response.double_clicked() {
                term.selection = Some(Selection::new(
                    SelectionType::Semantic,
                    grid_point(offset, col, row),
                    side,
                ));
            } else if response.clicked_by(PointerButton::Primary) {
                term.selection = None;
            }
            if (response.double_clicked() || response.triple_clicked())
                && style.copy_on_select
                && let Some(text) = term.selection_to_string()
            {
                ui.ctx().copy_text(text);
            }
        }
    }

    // Wheel: report, send arrows (alternate screen), or scroll the history.
    if let (Some(hover_pos), true) = (hover, input_enabled) {
        let wheel: f32 = ui.input(|i| {
            i.events
                .iter()
                .map(|e| match e {
                    Event::MouseWheel { unit, delta, .. } => match unit {
                        MouseWheelUnit::Point => delta.y / cell.y,
                        MouseWheelUnit::Line => delta.y,
                        MouseWheelUnit::Page => delta.y * size.rows as f32,
                    },
                    _ => 0.0,
                })
                .sum()
        });
        view.scroll_accum += wheel;
        let lines = view.scroll_accum.trunc() as i32;
        if lines != 0 {
            view.scroll_accum -= lines as f32;
            if mouse_mode && !shift_held {
                let (col, row, _) = cell_at(hover_pos, origin, cell, size);
                let b = if lines > 0 {
                    MouseButton::WheelUp
                } else {
                    MouseButton::WheelDown
                };
                for _ in 0..lines.abs().min(10) {
                    if let Some(bytes) =
                        input::encode_mouse(b, MouseAction::Press, Mods::default(), col, row, sgr)
                    {
                        out.input.extend(bytes);
                    }
                }
            } else if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) && alive {
                let seq: &[u8] = if lines > 0 { b"\x1bOA" } else { b"\x1bOB" };
                for _ in 0..lines.abs().min(10) {
                    out.input.extend_from_slice(seq);
                }
            } else {
                term.scroll_display(Scroll::Delta(lines));
            }
        }
    }
    if hover.is_some() {
        ui.ctx().set_cursor_icon(if mouse_mode && !shift_held {
            egui::CursorIcon::Default
        } else {
            egui::CursorIcon::Text
        });
    }

    // ----- render ------------------------------------------------------------------------
    let painter = ui.painter_at(rect);
    let content = term.renderable_content();
    let colors = content.colors;
    let scheme = style.scheme;
    let default_bg = palette(257, colors, scheme);
    painter.rect_filled(rect, CornerRadius::ZERO, default_bg);

    let offset = content.display_offset;
    let selection = content.selection;
    let cursor = content.cursor;
    let snap = |v: f32| (v * ppp).round() / ppp;
    let cell_rect = |col: usize, row: usize, width: usize| {
        let x0 = snap(origin.x + col as f32 * cell.x);
        let x1 = snap(origin.x + (col + width) as f32 * cell.x);
        let y0 = snap(origin.y + row as f32 * cell.y);
        let y1 = snap(origin.y + (row + 1) as f32 * cell.y);
        Rect::from_min_max(pos2(x0, y0), pos2(x1, y1))
    };

    let mut runs: Vec<(usize, TextRun)> = Vec::new();
    let mut singles: Vec<(usize, usize, String, RunStyle, usize)> = Vec::new(); // row, col, text, style, width
    let mut bg_run: Option<(usize, usize, usize, Color32)> = None; // row, start col, end col, color
    let flush_bg = |run: &mut Option<(usize, usize, usize, Color32)>| {
        if let Some((row, start, end, color)) = run.take() {
            let r = cell_rect(start, row, end - start);
            painter.rect_filled(r, CornerRadius::ZERO, color);
        }
    };
    let mut current: Option<(usize, TextRun)> = None;
    let mut cursor_cell: Option<(usize, usize, String, Color32, Color32, bool)> = None;

    for indexed in content.display_iter {
        let Some(vp) = term::point_to_viewport(offset, indexed.point) else {
            continue;
        };
        let (row, col) = (vp.line, vp.column.0);
        if row >= size.rows || col >= size.cols {
            continue;
        }
        let flags = indexed.cell.flags;
        if flags.contains(Flags::WIDE_CHAR_SPACER) {
            continue;
        }
        let mut fg = resolve_fg(indexed.cell.fg, flags, colors, scheme);
        let mut bg = resolve_bg(indexed.cell.bg, colors, scheme);
        if flags.contains(Flags::INVERSE) {
            let new_fg = bg.unwrap_or(default_bg);
            bg = Some(fg);
            fg = new_fg;
        }
        let selected = selection.is_some_and(|s| s.contains(indexed.point));
        if selected {
            bg = Some(scheme.selection);
        }
        let width = if flags.contains(Flags::WIDE_CHAR) {
            2
        } else {
            1
        };

        // Background runs.
        match (&mut bg_run, bg) {
            (Some((r, _, end, color)), Some(c)) if *r == row && *end == col && *color == c => {
                *end = col + width
            }
            (_, Some(c)) => {
                flush_bg(&mut bg_run);
                bg_run = Some((row, col, col + width, c));
            }
            (_, None) => flush_bg(&mut bg_run),
        }

        let mut ch = indexed.cell.c;
        if ch == '\t' || flags.contains(Flags::LEADING_WIDE_CHAR_SPACER) {
            ch = ' ';
        }
        let hidden = flags.contains(Flags::HIDDEN);
        let underline = if flags.contains(Flags::DOUBLE_UNDERLINE) {
            2
        } else if flags
            .intersects(Flags::UNDERCURL | Flags::DOTTED_UNDERLINE | Flags::DASHED_UNDERLINE)
        {
            3
        } else if flags.contains(Flags::UNDERLINE) {
            1
        } else {
            0
        };
        let run_style = RunStyle {
            fg,
            bold: flags.contains(Flags::BOLD),
            italic: flags.contains(Flags::ITALIC),
            underline,
            underline_color: indexed
                .cell
                .underline_color()
                .map(|c| resolve_fg(c, flags, colors, scheme)),
            strike: flags.contains(Flags::STRIKEOUT),
        };

        let is_cursor = cursor.shape != CursorShape::Hidden
            && cursor.point == indexed.point
            && (cursor.point.line.0 + offset as i32) < size.rows as i32;
        if is_cursor {
            let mut text = ch.to_string();
            if let Some(extra) = indexed.cell.zerowidth() {
                text.extend(extra.iter());
            }
            cursor_cell = Some((row, col, text, fg, bg.unwrap_or(default_bg), width == 2));
        }

        let zerowidth = indexed.cell.zerowidth().is_some_and(|z| !z.is_empty());
        let simple = ch.is_ascii() && width == 1 && !zerowidth;
        let blank = ch == ' ' && underline == 0 && !run_style.strike;
        if hidden || blank {
            if let Some(run) = current.take() {
                runs.push(run);
            }
            continue;
        }
        if simple {
            match &mut current {
                Some((r, run))
                    if *r == row && run.col + run.text.len() == col && run.style == run_style =>
                {
                    run.text.push(ch)
                }
                _ => {
                    if let Some(run) = current.take() {
                        runs.push(run);
                    }
                    current = Some((
                        row,
                        TextRun {
                            col,
                            text: ch.to_string(),
                            style: run_style,
                        },
                    ));
                }
            }
        } else {
            if let Some(run) = current.take() {
                runs.push(run);
            }
            let mut text = ch.to_string();
            if let Some(extra) = indexed.cell.zerowidth() {
                text.extend(extra.iter());
            }
            singles.push((row, col, text, run_style, width));
        }
    }
    flush_bg(&mut bg_run);
    if let Some(run) = current.take() {
        runs.push(run);
    }

    let paint_text =
        |row: usize, col: usize, text: &str, st: &RunStyle, width: usize, color: Color32| {
            let r = cell_rect(col, row, width.max(text.chars().count().min(1)));
            if text.chars().count() == 1
                && draw_special(
                    &painter,
                    text.chars().next().unwrap(),
                    cell_rect(col, row, width),
                    color,
                    ppp,
                )
            {
                return;
            }
            let real_face = (!st.bold || style.real_bold) && (!st.italic || style.real_italic);
            let fam = if real_face {
                family(st.bold, st.italic)
            } else {
                family(st.bold && style.real_bold, st.italic && style.real_italic)
            };
            let font = FontId::new(style.font_size, fam);
            let mut job = egui::text::LayoutJob::default();
            job.append(
                text,
                0.0,
                egui::TextFormat {
                    font_id: font,
                    color,
                    italics: st.italic && !style.real_italic,
                    line_height: Some(cell.y),
                    valign: egui::Align::Center,
                    ..Default::default()
                },
            );
            job.wrap = egui::text::TextWrapping::no_max_width();
            let galley = painter.layout_job(job);
            let pos = pos2(r.left(), r.top());
            if st.bold && !style.real_bold {
                painter.galley(
                    pos + vec2(0.5 / ppp.max(1.0) * 2.0 * 0.5, 0.0),
                    galley.clone(),
                    color,
                );
            }
            painter.galley(pos, galley, color);
        };
    let decorate = |row: usize, col: usize, width: usize, st: &RunStyle| {
        let r = cell_rect(col, row, width);
        let thickness = (1.0 / ppp).max(1.0 / ppp * 1.0).max(0.5);
        let ucolor = st.underline_color.unwrap_or(st.fg);
        if st.underline > 0 {
            let y = snap(r.bottom() - thickness * 1.5);
            painter.hline(r.x_range(), y, Stroke::new(thickness.max(1.0), ucolor));
            if st.underline == 2 {
                painter.hline(
                    r.x_range(),
                    y - 2.0,
                    Stroke::new(thickness.max(1.0), ucolor),
                );
            }
        }
        if st.strike {
            painter.hline(
                r.x_range(),
                snap(r.center().y),
                Stroke::new(thickness.max(1.0), st.fg),
            );
        }
    };

    for (row, run) in &runs {
        paint_text(
            *row,
            run.col,
            &run.text,
            &run.style,
            run.text.len(),
            run.style.fg,
        );
        decorate(*row, run.col, run.text.len(), &run.style);
    }
    for (row, col, text, st, width) in &singles {
        paint_text(*row, *col, text, st, *width, st.fg);
        decorate(*row, *col, *width, st);
    }

    // Cursor.
    let blink_on = if style.cursor_blink && has_focus_now && alive {
        let t = ui.input(|i| i.time) - view.blink_epoch;
        let phase = (t / 0.53).floor() as i64;
        let next = (phase + 1) as f64 * 0.53 - t;
        ui.ctx()
            .request_repaint_after(Duration::from_secs_f64(next.max(0.01)));
        phase % 2 == 0
    } else {
        true
    };
    let cursor_color = colors[NamedColor::Cursor as usize]
        .map(rgb)
        .unwrap_or(scheme.cursor);
    let mut cursor_rect = None;
    if let Some((row, col, text, fg, bg, wide)) = cursor_cell {
        let r = cell_rect(col, row, if wide { 2 } else { 1 });
        cursor_rect = Some(r);
        if !view.preedit.is_empty() {
            // Drawn below.
        } else if !has_focus_now {
            painter.rect_stroke(
                r,
                CornerRadius::ZERO,
                Stroke::new(1.0, cursor_color),
                StrokeKind::Inside,
            );
        } else if blink_on {
            match cursor.shape {
                CursorShape::Beam => {
                    painter.rect_filled(
                        Rect::from_min_size(r.min, vec2(2.0f32.max(1.0 / ppp), r.height())),
                        CornerRadius::ZERO,
                        cursor_color,
                    );
                }
                CursorShape::Underline => {
                    painter.rect_filled(
                        Rect::from_min_max(pos2(r.left(), r.bottom() - 2.0), r.max),
                        CornerRadius::ZERO,
                        cursor_color,
                    );
                }
                CursorShape::HollowBlock => {
                    painter.rect_stroke(
                        r,
                        CornerRadius::ZERO,
                        Stroke::new(1.0, cursor_color),
                        StrokeKind::Inside,
                    );
                }
                _ => {
                    painter.rect_filled(r, CornerRadius::ZERO, cursor_color);
                    let text_color = if bg == cursor_color { fg } else { bg };
                    if text.trim() != "" {
                        let st = RunStyle {
                            fg: text_color,
                            bold: false,
                            italic: false,
                            underline: 0,
                            underline_color: None,
                            strike: false,
                        };
                        paint_text(row, col, &text, &st, if wide { 2 } else { 1 }, text_color);
                    }
                }
            }
        }
        let _ = fg;
    }
    drop(term);

    // IME pre-edit text and candidate window position.
    if focused {
        let anchor = cursor_rect.unwrap_or_else(|| Rect::from_min_size(origin, cell));
        if !view.preedit.is_empty() {
            let font = FontId::new(style.font_size, family(false, false));
            let galley = painter.layout_no_wrap(view.preedit.clone(), font, scheme.foreground);
            let r = Rect::from_min_size(anchor.min, vec2(galley.size().x.max(cell.x), cell.y));
            painter.rect_filled(r, CornerRadius::ZERO, default_bg);
            painter.galley(anchor.min, galley, scheme.foreground);
            painter.hline(
                r.x_range(),
                r.bottom() - 1.0,
                Stroke::new(1.0, scheme.foreground),
            );
        }
        let to_global = ui
            .ctx()
            .layer_transform_to_global(ui.layer_id())
            .unwrap_or_default();
        ui.output_mut(|o| {
            o.ime = Some(egui::output::IMEOutput {
                purpose: egui::IMEPurpose::Terminal,
                rect: to_global * anchor,
                cursor_rect: to_global * anchor,
                should_interrupt_composition: false,
            })
        });
    }

    // Context menu.
    // In mouse-reporting mode the right button belongs to the remote program (Shift+right-click
    // still opens the menu).
    if mouse_mode && !shift_held {
        return out;
    }
    response.context_menu(|ui| {
        let term = shared.term.lock();
        let has_sel = term.selection.as_ref().is_some_and(|s| !s.is_empty());
        drop(term);
        if ui
            .add_enabled(has_sel, egui::Button::new(crate::i18n::t("Copy")))
            .clicked()
        {
            if let Some(text) = shared.term.lock().selection_to_string() {
                ui.ctx().copy_text(text);
            }
            ui.close();
        }
        if ui
            .add_enabled(alive, egui::Button::new(crate::i18n::t("Paste")))
            .clicked()
        {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::RequestPaste);
            ui.memory_mut(|m| m.request_focus(id));
            ui.close();
        }
        if ui.button(crate::i18n::t("Select All")).clicked() {
            select_all(shared);
            ui.close();
        }
        ui.separator();
        if ui.button(crate::i18n::t("Clear Scrollback")).clicked() {
            clear_scrollback(shared);
            ui.close();
        }
    });
    out
}

fn on_terminal_layer(ui: &Ui, pos: Pos2) -> bool {
    ui.ctx()
        .layer_id_at(pos)
        .is_none_or(|layer| layer == ui.layer_id())
}

pub fn select_all(shared: &TermShared) {
    let mut term = shared.term.lock();
    let start = Point::new(term.topmost_line(), Column(0));
    let end = Point::new(term.bottommost_line(), term.last_column());
    let mut sel = Selection::new(SelectionType::Simple, start, Side::Left);
    sel.update(end, Side::Right);
    term.selection = Some(sel);
}

pub fn clear_scrollback(shared: &TermShared) {
    let mut term = shared.term.lock();
    term.grid_mut().clear_history();
    term.selection = None;
}

pub fn copy_selection(ctx: &egui::Context, shared: &TermShared) -> bool {
    match shared.term.lock().selection_to_string() {
        Some(text) => {
            ctx.copy_text(text);
            true
        }
        None => false,
    }
}
