//! Split-pane layout of a tab: a binary tree whose leaves are terminal panes.

use eframe::egui::{Pos2, Rect, pos2};

pub type PaneId = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplitDir {
    /// Side by side (divider is vertical).
    Horizontal,
    /// Stacked (divider is horizontal).
    Vertical,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Layout {
    Pane(PaneId),
    Split {
        dir: SplitDir,
        ratio: f32,
        a: Box<Layout>,
        b: Box<Layout>,
    },
}

/// A draggable divider; `path` addresses the split node (false = a, true = b at each level).
#[derive(Clone, Debug, PartialEq)]
pub struct Divider {
    pub path: Vec<bool>,
    pub dir: SplitDir,
    pub rect: Rect,
    /// The rect of the whole split, to convert a drag position into a ratio.
    pub parent: Rect,
}

pub const DIVIDER: f32 = 4.0;

impl Layout {
    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect(&self, out: &mut Vec<PaneId>) {
        match self {
            Layout::Pane(id) => out.push(*id),
            Layout::Split { a, b, .. } => {
                a.collect(out);
                b.collect(out);
            }
        }
    }

    pub fn contains(&self, id: PaneId) -> bool {
        self.panes().contains(&id)
    }

    /// Splits pane `target`, putting `new` after it. Returns whether `target` was found.
    pub fn split(&mut self, target: PaneId, new: PaneId, dir: SplitDir) -> bool {
        match self {
            Layout::Pane(id) if *id == target => {
                *self = Layout::Split {
                    dir,
                    ratio: 0.5,
                    a: Box::new(Layout::Pane(target)),
                    b: Box::new(Layout::Pane(new)),
                };
                true
            }
            Layout::Pane(_) => false,
            Layout::Split { a, b, .. } => a.split(target, new, dir) || b.split(target, new, dir),
        }
    }

    /// Removes a pane, letting its sibling take its space. Returns false if it is the last pane
    /// (the caller closes the tab instead) or was not found.
    pub fn remove(&mut self, target: PaneId) -> bool {
        let Layout::Split { a, b, .. } = self else {
            return false;
        };
        if matches!(**a, Layout::Pane(id) if id == target) {
            *self = std::mem::replace(&mut **b, Layout::Pane(0));
            return true;
        }
        if matches!(**b, Layout::Pane(id) if id == target) {
            *self = std::mem::replace(&mut **a, Layout::Pane(0));
            return true;
        }
        a.remove(target) || b.remove(target)
    }

    /// Pane rectangles and dividers for the given area.
    pub fn arrange(&self, rect: Rect) -> (Vec<(PaneId, Rect)>, Vec<Divider>) {
        let mut panes = Vec::new();
        let mut dividers = Vec::new();
        self.arrange_into(rect, &mut Vec::new(), &mut panes, &mut dividers);
        (panes, dividers)
    }

    fn arrange_into(
        &self,
        rect: Rect,
        path: &mut Vec<bool>,
        panes: &mut Vec<(PaneId, Rect)>,
        dividers: &mut Vec<Divider>,
    ) {
        match self {
            Layout::Pane(id) => panes.push((*id, rect)),
            Layout::Split { dir, ratio, a, b } => {
                let (ra, div, rb) = split_rect(rect, *dir, *ratio);
                dividers.push(Divider {
                    path: path.clone(),
                    dir: *dir,
                    rect: div,
                    parent: rect,
                });
                path.push(false);
                a.arrange_into(ra, path, panes, dividers);
                path.pop();
                path.push(true);
                b.arrange_into(rb, path, panes, dividers);
                path.pop();
            }
        }
    }

    /// Sets the ratio of the split at `path`.
    pub fn set_ratio(&mut self, path: &[bool], value: f32) {
        let mut node = self;
        for &side in path {
            match node {
                Layout::Split { a, b, .. } => node = if side { b } else { a },
                Layout::Pane(_) => return,
            }
        }
        if let Layout::Split { ratio, .. } = node {
            *ratio = value.clamp(0.1, 0.9);
        }
    }

    /// The pane after (or before) `current` in reading order, wrapping around.
    pub fn neighbor(&self, current: PaneId, forward: bool) -> PaneId {
        let panes = self.panes();
        let idx = panes.iter().position(|p| *p == current).unwrap_or(0);
        let n = panes.len();
        panes[if forward {
            (idx + 1) % n
        } else {
            (idx + n - 1) % n
        }]
    }
}

fn split_rect(rect: Rect, dir: SplitDir, ratio: f32) -> (Rect, Rect, Rect) {
    let half = DIVIDER / 2.0;
    match dir {
        SplitDir::Horizontal => {
            let x = (rect.left() + rect.width() * ratio).round();
            (
                Rect::from_min_max(rect.min, pos2(x - half, rect.bottom())),
                Rect::from_min_max(pos2(x - half, rect.top()), pos2(x + half, rect.bottom())),
                Rect::from_min_max(pos2(x + half, rect.top()), rect.max),
            )
        }
        SplitDir::Vertical => {
            let y = (rect.top() + rect.height() * ratio).round();
            (
                Rect::from_min_max(rect.min, pos2(rect.right(), y - half)),
                Rect::from_min_max(pos2(rect.left(), y - half), pos2(rect.right(), y + half)),
                Rect::from_min_max(pos2(rect.left(), y + half), rect.max),
            )
        }
    }
}

/// Ratio for a divider dragged to `pos`.
pub fn ratio_at(divider: &Divider, pos: Pos2) -> f32 {
    let p = divider.parent;
    match divider.dir {
        SplitDir::Horizontal => (pos.x - p.left()) / p.width().max(1.0),
        SplitDir::Vertical => (pos.y - p.top()) / p.height().max(1.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_remove_and_arrange() {
        let mut l = Layout::Pane(1);
        assert!(l.split(1, 2, SplitDir::Horizontal));
        assert!(l.split(2, 3, SplitDir::Vertical));
        assert_eq!(l.panes(), [1, 2, 3]);
        let (panes, dividers) = l.arrange(Rect::from_min_max(pos2(0.0, 0.0), pos2(100.0, 100.0)));
        assert_eq!(panes.len(), 3);
        assert_eq!(dividers.len(), 2);
        assert!(panes[0].1.right() <= 48.0 && panes[1].1.left() >= 52.0);
        assert!(panes[1].1.bottom() <= 48.0 && panes[2].1.top() >= 52.0);

        l.set_ratio(&dividers[1].path, 0.25);
        let (panes, _) = l.arrange(Rect::from_min_max(pos2(0.0, 0.0), pos2(100.0, 100.0)));
        assert!(panes[1].1.bottom() < 25.0);

        assert_eq!(l.neighbor(3, true), 1);
        assert_eq!(l.neighbor(1, false), 3);
        assert!(l.remove(2));
        assert_eq!(l.panes(), [1, 3]);
        assert!(l.remove(1));
        assert_eq!(l, Layout::Pane(3));
        assert!(!l.remove(3), "last pane is not removed");
    }
}
