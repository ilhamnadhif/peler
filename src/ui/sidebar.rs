//! Sidebar: search plus the bookmark tree (nested groups, drag-and-drop reordering).

use std::collections::{HashMap, HashSet};

use eframe::egui::{
    self, CornerRadius, FontId, Key, LayerId, Order, RichText, Sense, Stroke, Ui, pos2,
    vec2,
};

use super::{icons, kit, style};
use uuid::Uuid;

use super::actions::Action;
use crate::bookmarks::transfer::ExportScope;
use crate::bookmarks::{BookmarkStore, search};
use crate::i18n::{t, tf};

const ROW_H: f32 = 28.0;
const INDENT: f32 = 14.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DragItem {
    Bookmark(Uuid),
    Group(Uuid),
}

/// A reorder/move requested by drag and drop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreeMove {
    Bookmark {
        id: Uuid,
        group: Option<Uuid>,
        before: Option<Uuid>,
    },
    Group {
        id: Uuid,
        parent: Option<Uuid>,
        before: Option<Uuid>,
    },
}

#[derive(Default)]
pub struct Sidebar {
    pub search: String,
    pub selected: Option<Uuid>,
    pub focus_search: bool,
    /// Groups opened in this run: every group starts closed when Peler opens.
    expanded: HashSet<Uuid>,
    search_index: usize,
}

/// Connection state per bookmark, for the dot next to its name.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Live {
    Connected,
    Connecting,
    Failed,
}

struct Ctx<'a> {
    store: &'a BookmarkStore,
    live: &'a HashMap<Uuid, Live>,
    actions: Vec<Action>,
    moved: Option<TreeMove>,
}

impl Sidebar {
    /// Draws the sidebar. Its header is `header_h` high, lined up with the tab strip beside it.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        store: &BookmarkStore,
        live: &HashMap<Uuid, Live>,
        header_h: f32,
    ) -> (Vec<Action>, Option<TreeMove>) {
        let mut cx = Ctx {
            store,
            live,
            actions: Vec::new(),
            moved: None,
        };
        let p = style::pal(ui);

        // Header: the title and the "new" menu, over the same hairline as the tab strip.
        let (header, _) = ui.allocate_exact_size(vec2(ui.available_width(), header_h), Sense::hover());
        let edge = ui.clip_rect().x_range();
        ui.painter()
            .hline(edge, header.bottom() - 0.5, Stroke::new(1.0, p.border));
        let mut header_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(header)
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        header_ui.spacing_mut().item_spacing.x = 6.0;
        header_ui.label(RichText::new(t("Connections")).font(style::semibold(14.0)).color(p.text));
        if !store.bookmarks.is_empty() {
            header_ui.label(
                RichText::new(store.bookmarks.len().to_string())
                    .small()
                    .color(p.text_faint),
            );
        }
        header_ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let selected_group = self.selected_group(store);
            let new = kit::icon_button(ui, icons::PLUS, t("New Connection…"));
            #[cfg(debug_assertions)]
            crate::devtest::record("sidebar-new", new.rect);
            egui::Popup::menu(&new).show(|ui| {
                ui.set_min_width(230.0);
                if ui.button(kit::with_icon(icons::HARD_DRIVES, t("New Connection…"))).clicked() {
                    cx.actions.push(Action::NewBookmark {
                        group: selected_group,
                    });
                    ui.close();
                }
                if ui.button(kit::with_icon(icons::FOLDER_PLUS, t("New Group"))).clicked() {
                    cx.actions.push(Action::NewGroup { parent: None });
                    ui.close();
                }
                ui.separator();
                if ui.button(kit::with_icon(icons::DOWNLOAD_SIMPLE, t("Import Connections…"))).clicked() {
                    cx.actions.push(Action::Import);
                    ui.close();
                }
                if ui.button(kit::with_icon(icons::FILE_CODE, t("Import from ~/.ssh/config"))).clicked() {
                    cx.actions.push(Action::ImportSshConfig);
                    ui.close();
                }
                if ui.button(kit::with_icon(icons::EXPORT, t("Export Connections…"))).clicked() {
                    cx.actions.push(Action::Export(ExportScope::All));
                    ui.close();
                }
            });
        });
        ui.add_space(10.0);

        let search_id = egui::Id::new("sidebar-search");
        let width = ui.available_width();
        let resp = kit::search_field(ui, search_id, &mut self.search, t("Filter connections"), width);
        if std::mem::take(&mut self.focus_search) {
            resp.request_focus();
        }
        #[cfg(debug_assertions)]
        crate::devtest::record("sidebar-search", resp.rect);
        if resp.changed() {
            self.search_index = 0;
        }
        if resp.has_focus() {
            let (down, up) = ui.input(|i| (i.key_pressed(Key::ArrowDown), i.key_pressed(Key::ArrowUp)));
            if down {
                self.search_index += 1;
            }
            if up {
                self.search_index = self.search_index.saturating_sub(1);
            }
        }
        if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) && !self.search.trim().is_empty() {
            let hits = search::search(store, &self.search);
            if let Some(id) = hits.get(self.search_index.min(hits.len().saturating_sub(1))) {
                cx.actions.push(Action::Connect(*id));
            }
        }
        if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Escape)) {
            self.search.clear();
        }
        ui.add_space(8.0);

        egui::ScrollArea::vertical()
            .id_salt("sidebar-tree")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                if self.search.trim().is_empty() {
                    if store.bookmarks.is_empty() && store.groups.is_empty() {
                        kit::empty_state(
                            ui,
                            icons::HARD_DRIVES,
                            t("No connections yet"),
                            t("Add a server, or bring yours in."),
                            |ui| {
                                ui.spacing_mut().item_spacing.y = 6.0;
                                if kit::primary_icon(ui, icons::PLUS, t("New Connection…")).clicked() {
                                    cx.actions.push(Action::NewBookmark { group: None });
                                }
                                if kit::secondary(ui, Some(icons::FILE_CODE), t("Import from ~/.ssh/config"))
                                    .clicked()
                                {
                                    cx.actions.push(Action::ImportSshConfig);
                                }
                                if kit::ghost(ui, Some(icons::DOWNLOAD_SIMPLE), t("Import Connections…")).clicked() {
                                    cx.actions.push(Action::Import);
                                }
                            },
                        );
                    }
                    self.tree(ui, &mut cx, None, 0);
                    // Empty space at the bottom: drop here to move to the top level.
                    let (rect, resp) = ui.allocate_exact_size(
                        vec2(ui.available_width(), ui.available_height().max(ROW_H * 2.0)),
                        Sense::click(),
                    );
                    if resp.dnd_hover_payload::<DragItem>().is_some() {
                        ui.painter().rect_stroke(
                            rect.shrink(2.0),
                            CornerRadius::same(6),
                            Stroke::new(1.0, p.accent),
                            egui::StrokeKind::Inside,
                        );
                    }
                    if let Some(item) = resp.dnd_release_payload::<DragItem>() {
                        cx.moved = Some(match *item {
                            DragItem::Bookmark(id) => TreeMove::Bookmark {
                                id,
                                group: None,
                                before: None,
                            },
                            DragItem::Group(id) => TreeMove::Group {
                                id,
                                parent: None,
                                before: None,
                            },
                        });
                    }
                    if resp.clicked() {
                        self.selected = None;
                    }
                    resp.context_menu(|ui| {
                        if ui.button(kit::with_icon(icons::HARD_DRIVES, t("New Connection…"))).clicked() {
                            cx.actions.push(Action::NewBookmark { group: None });
                            ui.close();
                        }
                        if ui.button(kit::with_icon(icons::FOLDER_PLUS, t("New Group"))).clicked() {
                            cx.actions.push(Action::NewGroup { parent: None });
                            ui.close();
                        }
                    });
                } else {
                    let hits = search::search(store, &self.search);
                    if hits.is_empty() {
                        ui.add_space(16.0);
                        ui.vertical_centered(|ui| {
                            ui.label(RichText::new(t("No matches")).color(p.text_muted))
                        });
                    }
                    self.search_index = self.search_index.min(hits.len().saturating_sub(1));
                    for (i, id) in hits.iter().enumerate() {
                        let Some(b) = store.bookmark(*id) else {
                            continue;
                        };
                        let path = store.group_path(b.group);
                        let sub = if path.is_empty() {
                            b.address()
                        } else {
                            format!("{} · {}", b.address(), path)
                        };
                        let highlighted = i == self.search_index;
                        self.bookmark_row(ui, &mut cx, *id, 0, Some(sub), highlighted);
                    }
                }
            });
        (cx.actions, cx.moved)
    }

    /// The group the selected item belongs to (for "new bookmark here").
    fn selected_group(&self, store: &BookmarkStore) -> Option<Uuid> {
        let id = self.selected?;
        if store.group(id).is_some() {
            return Some(id);
        }
        store.bookmark(id).and_then(|b| b.group)
    }

    fn tree(&mut self, ui: &mut Ui, cx: &mut Ctx, parent: Option<Uuid>, depth: usize) {
        let groups: Vec<Uuid> = cx.store.child_groups(parent).map(|g| g.id).collect();
        for gid in groups {
            self.group_row(ui, cx, gid, depth);
            if self.expanded.contains(&gid) {
                self.tree(ui, cx, Some(gid), depth + 1);
            }
        }
        let bookmarks: Vec<Uuid> = cx.store.bookmarks_in(parent).map(|b| b.id).collect();
        for id in bookmarks {
            self.bookmark_row(ui, cx, id, depth, None, false);
        }
    }

    fn row_frame(&self, ui: &mut Ui, id: egui::Id, selected: bool, height: f32) -> (egui::Rect, egui::Response) {
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
        let resp = ui.interact(rect, id, Sense::click_and_drag());
        let p = style::pal(ui);
        let fill = if selected {
            Some(p.accent_soft)
        } else if resp.hovered() {
            Some(p.hover)
        } else {
            None
        };
        if let Some(fill) = fill {
            ui.painter()
                .rect_filled(rect.shrink2(vec2(0.0, 1.0)), CornerRadius::same(6), fill);
        }
        (rect, resp)
    }

    /// Thin vertical lines that show which group a row is in.
    fn indent_guides(ui: &Ui, rect: egui::Rect, depth: usize) {
        let color = style::pal(ui).border;
        for d in 1..=depth {
            let x = rect.left() + 4.0 + (d as f32 - 1.0) * INDENT + 9.0;
            ui.painter()
                .vline(x, rect.y_range(), Stroke::new(1.0, color));
        }
    }

    fn drag_preview(ui: &Ui, resp: &egui::Response, label: &str) {
        if resp.dragged()
            && let Some(p) = ui.ctx().pointer_interact_pos()
        {
            let painter = ui
                .ctx()
                .layer_painter(LayerId::new(Order::Tooltip, egui::Id::new("sidebar-drag")));
            let pal = style::pal(ui);
            let galley = painter.layout_no_wrap(label.to_string(), style::medium(13.0), pal.text);
            let r = egui::Rect::from_min_size(p + vec2(12.0, 4.0), galley.size() + vec2(16.0, 10.0));
            painter.add(ui.visuals().popup_shadow.as_shape(r, CornerRadius::same(6)));
            painter.rect(
                r,
                CornerRadius::same(6),
                pal.surface,
                Stroke::new(1.0, pal.border_strong),
                egui::StrokeKind::Inside,
            );
            painter.galley(r.min + vec2(8.0, 5.0), galley, pal.text);
        }
    }

    fn group_row(&mut self, ui: &mut Ui, cx: &mut Ctx, gid: Uuid, depth: usize) {
        let Some(group) = cx.store.group(gid) else {
            return;
        };
        let selected = self.selected == Some(gid);
        let (rect, resp) = self.row_frame(ui, egui::Id::new(("group-row", gid)), selected, ROW_H);
        Self::indent_guides(ui, rect, depth);
        let expanded = self.expanded.contains(&gid);
        let p = style::pal(ui);
        let x = rect.left() + 4.0 + depth as f32 * INDENT;
        let cy = rect.center().y;
        let painter = ui.painter();
        painter.text(
            pos2(x + 9.0, cy),
            egui::Align2::CENTER_CENTER,
            if expanded { icons::CARET_DOWN } else { icons::CARET_RIGHT },
            style::icon_font(11.0),
            p.text_faint,
        );
        painter.text(
            pos2(x + 27.0, cy),
            egui::Align2::CENTER_CENTER,
            if expanded { icons::FOLDER_OPEN } else { icons::FOLDER_SIMPLE },
            style::icon_font(15.0),
            p.folder,
        );
        let count = cx.store.bookmarks_in(Some(gid)).count();
        let name_x = x + 42.0;
        let count_galley = (count > 0).then(|| {
            painter.layout_no_wrap(count.to_string(), style::medium(10.5), p.text_muted)
        });
        let count_w = count_galley.as_ref().map_or(0.0, |g| g.size().x + 18.0);
        let name_galley = painter.layout_no_wrap(group.name.clone(), style::medium(13.5), p.text);
        let name_w = name_galley.size().x.min(rect.right() - 6.0 - count_w - name_x);
        painter
            .with_clip_rect(egui::Rect::from_min_max(
                pos2(name_x, rect.top()),
                pos2(name_x + name_w.max(0.0), rect.bottom()),
            ))
            .galley(pos2(name_x, cy - name_galley.size().y / 2.0), name_galley, p.text);
        if let Some(g) = count_galley {
            let pill = egui::Rect::from_min_size(
                pos2(name_x + name_w + 8.0, cy - 8.0),
                vec2(g.size().x + 10.0, 16.0),
            );
            painter.rect_filled(pill, CornerRadius::same(8), if selected { p.surface } else { p.subtle });
            painter.galley(pos2(pill.left() + 5.0, pill.center().y - g.size().y / 2.0), g, p.text_muted);
        }

        if resp.clicked() {
            self.selected = Some(gid);
            if !self.expanded.remove(&gid) {
                self.expanded.insert(gid);
            }
        }
        resp.dnd_set_drag_payload(DragItem::Group(gid));
        Self::drag_preview(ui, &resp, &group.name);
        if let (Some(item), Some(p)) = (
            resp.dnd_hover_payload::<DragItem>(),
            ui.ctx().pointer_interact_pos(),
        ) {
            let into = matches!(*item, DragItem::Bookmark(_)) || p.y > rect.center().y;
            let accent = style::pal(ui).accent;
            if into {
                ui.painter().rect_stroke(
                    rect,
                    CornerRadius::same(5),
                    Stroke::new(1.5, accent),
                    egui::StrokeKind::Inside,
                );
            } else {
                ui.painter()
                    .hline(rect.x_range(), rect.top(), Stroke::new(2.0, accent));
            }
            if let Some(item) = resp.dnd_release_payload::<DragItem>() {
                cx.moved = Some(match *item {
                    DragItem::Bookmark(id) => TreeMove::Bookmark {
                        id,
                        group: Some(gid),
                        before: None,
                    },
                    DragItem::Group(id) if into => TreeMove::Group {
                        id,
                        parent: Some(gid),
                        before: None,
                    },
                    DragItem::Group(id) => TreeMove::Group {
                        id,
                        parent: group.parent,
                        before: Some(gid),
                    },
                });
                if into {
                    self.expanded.insert(gid);
                }
            }
        }
        resp.context_menu(|ui| {
            if ui.button(kit::with_icon(icons::HARD_DRIVES, t("New Connection…"))).clicked() {
                cx.actions.push(Action::NewBookmark { group: Some(gid) });
                ui.close();
            }
            if ui.button(kit::with_icon(icons::FOLDER_PLUS, t("New Subgroup"))).clicked() {
                cx.actions.push(Action::NewGroup { parent: Some(gid) });
                ui.close();
            }
            ui.separator();
            if ui.button(kit::with_icon(icons::PENCIL_SIMPLE, t("Rename…"))).clicked() {
                cx.actions.push(Action::RenameGroup(gid));
                ui.close();
            }
            if ui.button(kit::with_icon(icons::EXPORT, t("Export Group…"))).clicked() {
                cx.actions.push(Action::Export(ExportScope::Group(gid)));
                ui.close();
            }
            ui.separator();
            if ui
                .button(RichText::new(kit::with_icon(icons::TRASH, t("Delete Group…"))).color(style::pal(ui).err))
                .clicked()
            {
                cx.actions.push(Action::DeleteGroup(gid));
                ui.close();
            }
        });
    }

    fn bookmark_row(
        &mut self,
        ui: &mut Ui,
        cx: &mut Ctx,
        id: Uuid,
        depth: usize,
        subtitle: Option<String>,
        highlighted: bool,
    ) {
        let Some(b) = cx.store.bookmark(id) else {
            return;
        };
        let selected = self.selected == Some(id) || highlighted;
        let height = if subtitle.is_some() { ROW_H + 16.0 } else { ROW_H };
        let (rect, resp) = self.row_frame(ui, egui::Id::new(("bookmark-row", id)), selected, height);
        Self::indent_guides(ui, rect, depth);
        let p = style::pal(ui);
        let x = rect.left() + 4.0 + depth as f32 * INDENT;
        let top_line_y = if subtitle.is_some() {
            rect.top() + ROW_H / 2.0 + 1.0
        } else {
            rect.center().y
        };
        let live = cx.live.get(&id).copied();
        let painter = ui.painter();
        painter.text(
            pos2(x + 27.0, top_line_y),
            egui::Align2::CENTER_CENTER,
            icons::HARD_DRIVES,
            style::icon_font(15.0),
            if live == Some(Live::Connected) { p.text } else { p.text_muted },
        );
        let dot = match live {
            Some(Live::Connected) => Some(p.ok),
            Some(Live::Connecting) => Some(p.warn),
            Some(Live::Failed) => Some(p.err),
            None => None,
        };
        let right = match dot {
            Some(c) => {
                painter.circle_filled(pos2(rect.right() - 12.0, top_line_y), 3.5, c);
                rect.right() - 22.0
            }
            None => rect.right() - 6.0,
        };
        let name = b.display_name();
        let clip = egui::Rect::from_min_max(pos2(x + 42.0, rect.top()), pos2(right, rect.bottom()));
        let painter = painter.with_clip_rect(clip);
        painter.text(
            pos2(x + 42.0, top_line_y),
            egui::Align2::LEFT_CENTER,
            &name,
            if selected { style::medium(13.5) } else { FontId::proportional(13.5) },
            p.text,
        );
        if let Some(sub) = &subtitle {
            painter.text(
                pos2(x + 42.0, rect.top() + ROW_H - 1.0),
                egui::Align2::LEFT_TOP,
                sub,
                FontId::proportional(11.0),
                p.text_faint,
            );
        }

        #[cfg(debug_assertions)]
        crate::devtest::record(format!("bookmark:{name}"), rect);
        let resp = resp.on_hover_ui(|ui| {
            ui.label(RichText::new(b.address()).font(style::medium(13.0)));
            if !b.tags.is_empty() {
                ui.label(b.tags.join(", "));
            }
            if !b.notes.trim().is_empty() {
                ui.label(RichText::new(b.notes.trim()).weak());
            }
        });
        if resp.double_clicked() {
            cx.actions.push(Action::Connect(id));
        } else if resp.clicked() {
            self.selected = Some(id);
        }
        resp.dnd_set_drag_payload(DragItem::Bookmark(id));
        Self::drag_preview(ui, &resp, &name);
        if subtitle.is_none()
            && let (Some(item), Some(p)) = (
                resp.dnd_hover_payload::<DragItem>(),
                ui.ctx().pointer_interact_pos(),
            )
        {
            let before = p.y < rect.center().y;
            let accent = style::pal(ui).accent;
            if matches!(*item, DragItem::Bookmark(_)) {
                let y = if before { rect.top() } else { rect.bottom() };
                ui.painter()
                    .hline(rect.x_range(), y, Stroke::new(2.0, accent));
            }
            if let Some(item) = resp.dnd_release_payload::<DragItem>() {
                // "After this row" = before the next row that isn't the dragged one itself.
                let after = |dragged: Uuid| {
                    let ids: Vec<Uuid> = cx.store.bookmarks_in(b.group).map(|x| x.id).collect();
                    let pos = ids.iter().position(|x| *x == id).unwrap_or(0);
                    ids[pos + 1..].iter().copied().find(|x| *x != dragged)
                };
                cx.moved = Some(match *item {
                    DragItem::Bookmark(dragged) => TreeMove::Bookmark {
                        id: dragged,
                        group: b.group,
                        before: if before { Some(id) } else { after(dragged) },
                    },
                    DragItem::Group(g) => TreeMove::Group {
                        id: g,
                        parent: b.group,
                        before: None,
                    },
                });
            }
        }
        resp.context_menu(|ui| {
            self.selected = Some(id);
            if ui.button(kit::with_icon(icons::PLUGS_CONNECTED, t("Connect"))).clicked() {
                cx.actions.push(Action::Connect(id));
                ui.close();
            }
            if ui.button(kit::with_icon(icons::PENCIL_SIMPLE, t("Edit…"))).clicked() {
                cx.actions.push(Action::EditBookmark(id));
                ui.close();
            }
            if ui.button(kit::with_icon(icons::COPY, t("Duplicate"))).clicked() {
                cx.actions.push(Action::DuplicateBookmark(id));
                ui.close();
            }
            if ui.button(kit::with_icon(icons::CLIPBOARD, t("Copy Address"))).clicked() {
                ui.ctx().copy_text(b.address());
                ui.close();
            }
            if ui.button(kit::with_icon(icons::EXPORT, t("Export…"))).clicked() {
                cx.actions
                    .push(Action::Export(ExportScope::Selected(vec![id])));
                ui.close();
            }
            ui.separator();
            if ui
                .button(RichText::new(kit::with_icon(icons::TRASH, t("Delete…"))).color(style::pal(ui).err))
                .clicked()
            {
                cx.actions.push(Action::DeleteBookmark(id));
                ui.close();
            }
        });
    }

    /// Selects and reveals a bookmark or a group (expanding the groups it is in).
    pub fn reveal(&mut self, store: &BookmarkStore, id: Uuid) {
        self.selected = Some(id);
        let mut g = match store.bookmark(id) {
            Some(b) => b.group,
            None => store.group(id).and_then(|x| x.parent),
        };
        while let Some(gid) = g {
            self.expanded.insert(gid);
            g = store.group(gid).and_then(|x| x.parent);
        }
    }

    pub fn status_line(store: &BookmarkStore) -> String {
        tf("{} connections", &[&store.bookmarks.len().to_string()])
    }
}
