//! Sidebar: search plus the bookmark tree (nested groups, drag-and-drop reordering).

use std::collections::{HashMap, HashSet};

use eframe::egui::{
    self, Color32, CornerRadius, FontId, Key, LayerId, Order, RichText, Sense, Stroke, Ui, pos2,
    vec2,
};
use uuid::Uuid;

use super::actions::Action;
use crate::bookmarks::transfer::ExportScope;
use crate::bookmarks::{BookmarkStore, search};
use crate::i18n::{t, tf};

const ROW_H: f32 = 24.0;
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
    collapsed: HashSet<Uuid>,
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
    pub fn show(
        &mut self,
        ui: &mut Ui,
        store: &BookmarkStore,
        live: &HashMap<Uuid, Live>,
    ) -> (Vec<Action>, Option<TreeMove>) {
        let mut cx = Ctx {
            store,
            live,
            actions: Vec::new(),
            moved: None,
        };

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let selected_group = self.selected_group(store);
                ui.menu_button("+", |ui| {
                    if ui.button(t("New Bookmark…")).clicked() {
                        cx.actions.push(Action::NewBookmark {
                            group: selected_group,
                        });
                        ui.close();
                    }
                    if ui.button(t("New Group")).clicked() {
                        cx.actions.push(Action::NewGroup { parent: None });
                        ui.close();
                    }
                    ui.separator();
                    if ui.button(t("Import Bookmarks…")).clicked() {
                        cx.actions.push(Action::Import);
                        ui.close();
                    }
                    if ui.button(t("Import from ~/.ssh/config")).clicked() {
                        cx.actions.push(Action::ImportSshConfig);
                        ui.close();
                    }
                    if ui.button(t("Export Bookmarks…")).clicked() {
                        cx.actions.push(Action::Export(ExportScope::All));
                        ui.close();
                    }
                });
                let search_id = egui::Id::new("sidebar-search");
                let edit = egui::TextEdit::singleline(&mut self.search)
                    .id(search_id)
                    .hint_text(t("Search bookmarks"))
                    .desired_width(ui.available_width());
                let resp = ui.add(edit);
                if std::mem::take(&mut self.focus_search) {
                    resp.request_focus();
                }
                if resp.changed() {
                    self.search_index = 0;
                }
                if resp.has_focus() {
                    let (down, up) =
                        ui.input(|i| (i.key_pressed(Key::ArrowDown), i.key_pressed(Key::ArrowUp)));
                    if down {
                        self.search_index += 1;
                    }
                    if up {
                        self.search_index = self.search_index.saturating_sub(1);
                    }
                }
                if resp.lost_focus()
                    && ui.input(|i| i.key_pressed(Key::Enter))
                    && !self.search.trim().is_empty()
                {
                    let hits = search::search(store, &self.search);
                    if let Some(id) = hits.get(self.search_index.min(hits.len().saturating_sub(1)))
                    {
                        cx.actions.push(Action::Connect(*id));
                    }
                }
                if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Escape)) {
                    self.search.clear();
                }
            })
        });
        ui.add_space(4.0);

        egui::ScrollArea::vertical()
            .id_salt("sidebar-tree")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                if self.search.trim().is_empty() {
                    if store.bookmarks.is_empty() && store.groups.is_empty() {
                        ui.add_space(20.0);
                        ui.vertical_centered(|ui| {
                            ui.label(RichText::new(t("No bookmarks yet")).weak());
                            ui.add_space(6.0);
                            if ui.button(t("New Bookmark…")).clicked() {
                                cx.actions.push(Action::NewBookmark { group: None });
                            }
                            if ui.button(t("Import from ~/.ssh/config")).clicked() {
                                cx.actions.push(Action::ImportSshConfig);
                            }
                            if ui.button(t("Import Bookmarks…")).clicked() {
                                cx.actions.push(Action::Import);
                            }
                        });
                    }
                    self.tree(ui, &mut cx, None, 0);
                    // Empty space at the bottom: drop here to move to the top level.
                    let (rect, resp) = ui.allocate_exact_size(
                        vec2(ui.available_width(), ui.available_height().max(ROW_H * 2.0)),
                        Sense::click(),
                    );
                    if let Some(item) = resp.dnd_hover_payload::<DragItem>() {
                        let _ = item;
                        ui.painter().rect_stroke(
                            rect.shrink(2.0),
                            CornerRadius::same(4),
                            Stroke::new(1.0, ui.visuals().selection.bg_fill),
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
                        if ui.button(t("New Bookmark…")).clicked() {
                            cx.actions.push(Action::NewBookmark { group: None });
                            ui.close();
                        }
                        if ui.button(t("New Group")).clicked() {
                            cx.actions.push(Action::NewGroup { parent: None });
                            ui.close();
                        }
                    });
                } else {
                    let hits = search::search(store, &self.search);
                    if hits.is_empty() {
                        ui.add_space(12.0);
                        ui.vertical_centered(|ui| ui.label(RichText::new(t("No matches")).weak()));
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
            if !self.collapsed.contains(&gid) {
                self.tree(ui, cx, Some(gid), depth + 1);
            }
        }
        let bookmarks: Vec<Uuid> = cx.store.bookmarks_in(parent).map(|b| b.id).collect();
        for id in bookmarks {
            self.bookmark_row(ui, cx, id, depth, None, false);
        }
    }

    fn row_frame(&self, ui: &mut Ui, id: egui::Id, selected: bool) -> (egui::Rect, egui::Response) {
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::hover());
        let resp = ui.interact(rect, id, Sense::click_and_drag());
        let visuals = ui.visuals();
        if selected {
            ui.painter().rect_filled(
                rect,
                CornerRadius::same(5),
                visuals.selection.bg_fill.gamma_multiply(0.55),
            );
        } else if resp.hovered() {
            ui.painter().rect_filled(
                rect,
                CornerRadius::same(5),
                visuals.widgets.hovered.weak_bg_fill,
            );
        }
        (rect, resp)
    }

    fn drag_preview(ui: &Ui, resp: &egui::Response, label: &str) {
        if resp.dragged()
            && let Some(p) = ui.ctx().pointer_interact_pos()
        {
            let painter = ui
                .ctx()
                .layer_painter(LayerId::new(Order::Tooltip, egui::Id::new("sidebar-drag")));
            let galley = painter.layout_no_wrap(
                label.to_string(),
                FontId::proportional(13.0),
                ui.visuals().strong_text_color(),
            );
            let r = egui::Rect::from_min_size(p + vec2(12.0, 4.0), galley.size() + vec2(12.0, 6.0));
            painter.rect_filled(r, CornerRadius::same(5), ui.visuals().window_fill);
            painter.rect_stroke(
                r,
                CornerRadius::same(5),
                ui.visuals().window_stroke,
                egui::StrokeKind::Inside,
            );
            painter.galley(
                r.min + vec2(6.0, 3.0),
                galley,
                ui.visuals().strong_text_color(),
            );
        }
    }

    fn group_row(&mut self, ui: &mut Ui, cx: &mut Ctx, gid: Uuid, depth: usize) {
        let Some(group) = cx.store.group(gid) else {
            return;
        };
        let selected = self.selected == Some(gid);
        let (rect, resp) = self.row_frame(ui, egui::Id::new(("group-row", gid)), selected);
        let expanded = !self.collapsed.contains(&gid);
        let x = rect.left() + 6.0 + depth as f32 * INDENT;
        let color = ui.visuals().text_color();
        let painter = ui.painter();
        painter.text(
            pos2(x, rect.center().y),
            egui::Align2::LEFT_CENTER,
            if expanded { "▾" } else { "▸" },
            FontId::proportional(12.0),
            color,
        );
        crate::ui::icons::paint(
            painter,
            crate::ui::icons::Icon::Folder,
            pos2(x + 22.0, rect.center().y),
            12.0,
            crate::ui::icons::folder_color(ui.visuals().dark_mode),
        );
        let count = cx.store.bookmarks_in(Some(gid)).count();
        let name_galley = painter.layout_no_wrap(
            group.name.clone(),
            FontId::proportional(13.5),
            ui.visuals().strong_text_color(),
        );
        let name_w = name_galley.size().x;
        painter.galley(
            pos2(x + 34.0, rect.center().y - name_galley.size().y / 2.0),
            name_galley,
            color,
        );
        if count > 0 {
            painter.text(
                pos2(x + 40.0 + name_w, rect.center().y),
                egui::Align2::LEFT_CENTER,
                count.to_string(),
                FontId::proportional(11.0),
                ui.visuals().weak_text_color(),
            );
        }

        if resp.clicked() {
            self.selected = Some(gid);
            if !self.collapsed.remove(&gid) {
                self.collapsed.insert(gid);
            }
        }
        resp.dnd_set_drag_payload(DragItem::Group(gid));
        Self::drag_preview(ui, &resp, &group.name);
        if let (Some(item), Some(p)) = (
            resp.dnd_hover_payload::<DragItem>(),
            ui.ctx().pointer_interact_pos(),
        ) {
            let into = matches!(*item, DragItem::Bookmark(_)) || p.y > rect.center().y;
            let accent = ui.visuals().selection.bg_fill;
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
                self.collapsed.remove(&gid);
            }
        }
        resp.context_menu(|ui| {
            if ui.button(t("New Bookmark…")).clicked() {
                cx.actions.push(Action::NewBookmark { group: Some(gid) });
                ui.close();
            }
            if ui.button(t("New Subgroup")).clicked() {
                cx.actions.push(Action::NewGroup { parent: Some(gid) });
                ui.close();
            }
            ui.separator();
            if ui.button(t("Rename…")).clicked() {
                cx.actions.push(Action::RenameGroup(gid));
                ui.close();
            }
            if ui.button(t("Export Group…")).clicked() {
                cx.actions.push(Action::Export(ExportScope::Group(gid)));
                ui.close();
            }
            ui.separator();
            if ui.button(t("Delete Group…")).clicked() {
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
        let height = if subtitle.is_some() {
            ROW_H + 14.0
        } else {
            ROW_H
        };
        let (rect, resp) = {
            let (rect, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
            let resp = ui.interact(
                rect,
                egui::Id::new(("bookmark-row", id)),
                Sense::click_and_drag(),
            );
            let visuals = ui.visuals();
            if selected {
                ui.painter().rect_filled(
                    rect,
                    CornerRadius::same(5),
                    visuals.selection.bg_fill.gamma_multiply(0.55),
                );
            } else if resp.hovered() {
                ui.painter().rect_filled(
                    rect,
                    CornerRadius::same(5),
                    visuals.widgets.hovered.weak_bg_fill,
                );
            }
            (rect, resp)
        };
        let x = rect.left() + 20.0 + depth as f32 * INDENT;
        let painter = ui.painter();
        let top_line_y = if subtitle.is_some() {
            rect.top() + ROW_H / 2.0
        } else {
            rect.center().y
        };
        let dot = match cx.live.get(&id) {
            Some(Live::Connected) => Some(Color32::from_rgb(0x3f, 0xb9, 0x50)),
            Some(Live::Connecting) => Some(Color32::from_rgb(0xe0, 0xa8, 0x2e)),
            Some(Live::Failed) => Some(Color32::from_rgb(0xe0, 0x50, 0x4f)),
            None => None,
        };
        match dot {
            Some(c) => {
                painter.circle_filled(pos2(x + 4.0, top_line_y), 3.5, c);
            }
            None => {
                painter.text(
                    pos2(x - 1.0, top_line_y),
                    egui::Align2::LEFT_CENTER,
                    "›",
                    FontId::proportional(13.0),
                    ui.visuals().weak_text_color(),
                );
            }
        }
        let name = b.display_name();
        let clip = egui::Rect::from_min_max(
            pos2(x + 14.0, rect.top()),
            pos2(rect.right() - 4.0, rect.bottom()),
        );
        let painter = painter.with_clip_rect(clip);
        painter.text(
            pos2(x + 14.0, top_line_y),
            egui::Align2::LEFT_CENTER,
            &name,
            FontId::proportional(13.5),
            ui.visuals().text_color(),
        );
        if let Some(sub) = &subtitle {
            painter.text(
                pos2(x + 14.0, rect.top() + ROW_H - 2.0),
                egui::Align2::LEFT_TOP,
                sub,
                FontId::proportional(11.0),
                ui.visuals().weak_text_color(),
            );
        }

        let resp = resp.on_hover_ui(|ui| {
            ui.label(RichText::new(b.address()).strong());
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
            let accent = ui.visuals().selection.bg_fill;
            if matches!(*item, DragItem::Bookmark(_)) {
                let y = if before { rect.top() } else { rect.bottom() };
                ui.painter()
                    .hline(rect.x_range(), y, Stroke::new(2.0, accent));
            }
            if let Some(item) = resp.dnd_release_payload::<DragItem>() {
                let next = {
                    let ids: Vec<Uuid> = cx.store.bookmarks_in(b.group).map(|x| x.id).collect();
                    let pos = ids.iter().position(|x| *x == id).unwrap_or(0);
                    ids.get(pos + 1).copied()
                };
                cx.moved = Some(match *item {
                    DragItem::Bookmark(dragged) => TreeMove::Bookmark {
                        id: dragged,
                        group: b.group,
                        before: if before {
                            Some(id)
                        } else {
                            next.filter(|n| *n != dragged)
                        },
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
            if ui.button(RichText::new(t("Connect")).strong()).clicked() {
                cx.actions.push(Action::Connect(id));
                ui.close();
            }
            if ui.button(t("Edit…")).clicked() {
                cx.actions.push(Action::EditBookmark(id));
                ui.close();
            }
            if ui.button(t("Duplicate")).clicked() {
                cx.actions.push(Action::DuplicateBookmark(id));
                ui.close();
            }
            if ui.button(t("Copy Address")).clicked() {
                ui.ctx().copy_text(b.address());
                ui.close();
            }
            if ui.button(t("Export…")).clicked() {
                cx.actions
                    .push(Action::Export(ExportScope::Selected(vec![id])));
                ui.close();
            }
            ui.separator();
            if ui.button(t("Delete…")).clicked() {
                cx.actions.push(Action::DeleteBookmark(id));
                ui.close();
            }
        });
    }

    /// Selects and reveals a bookmark (expanding its groups).
    pub fn reveal(&mut self, store: &BookmarkStore, id: Uuid) {
        self.selected = Some(id);
        let mut g = store.bookmark(id).and_then(|b| b.group);
        while let Some(gid) = g {
            self.collapsed.remove(&gid);
            g = store.group(gid).and_then(|x| x.parent);
        }
    }

    pub fn status_line(store: &BookmarkStore) -> String {
        tf("{} bookmarks", &[&store.bookmarks.len().to_string()])
    }
}
