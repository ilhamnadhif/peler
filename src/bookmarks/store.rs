//! Bookmark store: the on-disk `bookmarks.json` plus tree operations on groups.
//!
//! Order matters and is the order of the vectors: a group's bookmarks are listed in the order
//! they appear in `bookmarks`, and sibling groups in the order they appear in `groups`.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::model::{Bookmark, Group};
use crate::paths;

/// 2: SSH agent logins, jump hosts and port forwards (older versions would lose them on save).
pub const STORE_SCHEMA_VERSION: u32 = 2;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BookmarkStore {
    #[serde(default)]
    pub groups: Vec<Group>,
    #[serde(default)]
    pub bookmarks: Vec<Bookmark>,
}

/// The bookmarks file was written by a newer version of the app: it must be left alone (not
/// renamed, not overwritten) so that version still finds it.
#[derive(Debug)]
pub struct NewerSchema(pub u32);

impl std::fmt::Display for NewerSchema {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&crate::i18n::tf(
            "The connections were saved by a newer version of Peler (schema {}). Please update the app; until then changes to connections are not saved.",
            &[&self.0.to_string()],
        ))
    }
}

impl std::error::Error for NewerSchema {}

#[derive(Serialize, Deserialize)]
struct StoreFile {
    schema_version: u32,
    #[serde(flatten)]
    store: BookmarkStore,
}

impl BookmarkStore {
    /// Loads the store; a missing file is an empty store.
    pub fn load(path: &Path) -> Result<Self> {
        let data = match std::fs::read(path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => {
                return Err(e).with_context(|| {
                    crate::i18n::tf("Could not read {}", &[&path.display().to_string()])
                });
            }
        };
        // The version is checked first: a newer file may not parse as this version's format.
        #[derive(Deserialize)]
        struct Version {
            schema_version: u32,
        }
        if let Ok(v) = serde_json::from_slice::<Version>(&data)
            && v.schema_version > STORE_SCHEMA_VERSION
        {
            return Err(NewerSchema(v.schema_version).into());
        }
        let file: StoreFile = serde_json::from_slice(&data)
            .with_context(|| crate::i18n::tf("{} is damaged", &[&path.display().to_string()]))?;
        let mut store = file.store;
        store.sanitize();
        Ok(store)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let file = StoreFile {
            schema_version: STORE_SCHEMA_VERSION,
            store: self.clone(),
        };
        let json = serde_json::to_vec_pretty(&file)?;
        paths::write_private(path, &json)
            .with_context(|| crate::i18n::tf("Could not save {}", &[&path.display().to_string()]))
    }

    // ----- lookup -------------------------------------------------------------------------

    pub fn bookmark(&self, id: Uuid) -> Option<&Bookmark> {
        self.bookmarks.iter().find(|b| b.id == id)
    }

    pub fn bookmark_mut(&mut self, id: Uuid) -> Option<&mut Bookmark> {
        self.bookmarks.iter_mut().find(|b| b.id == id)
    }

    pub fn group(&self, id: Uuid) -> Option<&Group> {
        self.groups.iter().find(|g| g.id == id)
    }

    pub fn child_groups(&self, parent: Option<Uuid>) -> impl Iterator<Item = &Group> {
        self.groups.iter().filter(move |g| g.parent == parent)
    }

    pub fn bookmarks_in(&self, group: Option<Uuid>) -> impl Iterator<Item = &Bookmark> {
        self.bookmarks.iter().filter(move |b| b.group == group)
    }

    /// "Parent / Child" path of a group, for display and search.
    pub fn group_path(&self, id: Option<Uuid>) -> String {
        let mut names = Vec::new();
        let mut cur = id;
        let mut seen = HashSet::new();
        while let Some(gid) = cur {
            if !seen.insert(gid) {
                break;
            }
            match self.group(gid) {
                Some(g) => {
                    names.push(g.name.clone());
                    cur = g.parent;
                }
                None => break,
            }
        }
        names.reverse();
        names.join(" / ")
    }

    /// The group itself plus every group nested below it.
    pub fn group_and_descendants(&self, id: Uuid) -> HashSet<Uuid> {
        let mut out = HashSet::from([id]);
        loop {
            let before = out.len();
            for g in &self.groups {
                if let Some(p) = g.parent
                    && out.contains(&p)
                {
                    out.insert(g.id);
                }
            }
            if out.len() == before {
                return out;
            }
        }
    }

    /// Depth of a group (top level = 0).
    pub fn group_depth(&self, id: Uuid) -> usize {
        self.group_path(Some(id)).matches(" / ").count()
    }

    // ----- bookmarks ----------------------------------------------------------------------

    /// Inserts a new bookmark or replaces the one with the same id (keeping its position).
    pub fn upsert_bookmark(&mut self, bookmark: Bookmark) {
        match self.bookmarks.iter_mut().find(|b| b.id == bookmark.id) {
            Some(existing) => *existing = bookmark,
            None => self.bookmarks.push(bookmark),
        }
    }

    pub fn remove_bookmark(&mut self, id: Uuid) -> Option<Bookmark> {
        let idx = self.bookmarks.iter().position(|b| b.id == id)?;
        Some(self.bookmarks.remove(idx))
    }

    /// Duplicates a bookmark, placing the copy right after the original. Returns the new id.
    pub fn duplicate_bookmark(&mut self, id: Uuid, copy_suffix: &str) -> Option<Uuid> {
        let idx = self.bookmarks.iter().position(|b| b.id == id)?;
        let mut copy = self.bookmarks[idx].duplicate();
        copy.name = format!("{} {}", self.bookmarks[idx].display_name(), copy_suffix);
        let new_id = copy.id;
        self.bookmarks.insert(idx + 1, copy);
        Some(new_id)
    }

    /// Moves a bookmark into `group`, before `before` (a bookmark in that group) or at the end.
    pub fn move_bookmark(&mut self, id: Uuid, group: Option<Uuid>, before: Option<Uuid>) {
        if before == Some(id) {
            return;
        }
        if group.is_some_and(|g| self.group(g).is_none()) {
            return;
        }
        let Some(mut b) = self.remove_bookmark(id) else {
            return;
        };
        b.group = group;
        let idx = before
            .and_then(|bid| self.bookmarks.iter().position(|x| x.id == bid))
            .unwrap_or(self.bookmarks.len());
        self.bookmarks.insert(idx, b);
    }

    // ----- groups -------------------------------------------------------------------------

    pub fn add_group(&mut self, name: impl Into<String>, parent: Option<Uuid>) -> Uuid {
        let g = Group::new(name, parent);
        let id = g.id;
        self.groups.push(g);
        id
    }

    pub fn rename_group(&mut self, id: Uuid, name: impl Into<String>) {
        if let Some(g) = self.groups.iter_mut().find(|g| g.id == id) {
            g.name = name.into();
        }
    }

    /// Deletes a group. Its bookmarks and sub-groups move up to the deleted group's parent, so
    /// deleting a group never deletes bookmarks.
    pub fn remove_group(&mut self, id: Uuid) {
        let Some(idx) = self.groups.iter().position(|g| g.id == id) else {
            return;
        };
        let parent = self.groups[idx].parent;
        self.groups.remove(idx);
        for g in self.groups.iter_mut().filter(|g| g.parent == Some(id)) {
            g.parent = parent;
        }
        for b in self.bookmarks.iter_mut().filter(|b| b.group == Some(id)) {
            b.group = parent;
        }
    }

    /// Moves a group under `parent`, before sibling `before` or at the end. Refuses moves that
    /// would create a cycle. Returns whether the move happened.
    pub fn move_group(&mut self, id: Uuid, parent: Option<Uuid>, before: Option<Uuid>) -> bool {
        if before == Some(id) {
            return false;
        }
        if let Some(p) = parent
            && (self.group(p).is_none() || self.group_and_descendants(id).contains(&p))
        {
            return false;
        }
        let Some(idx) = self.groups.iter().position(|g| g.id == id) else {
            return false;
        };
        let mut g = self.groups.remove(idx);
        g.parent = parent;
        let idx = before
            .and_then(|bid| self.groups.iter().position(|x| x.id == bid))
            .unwrap_or(self.groups.len());
        self.groups.insert(idx, g);
        true
    }

    /// Finds a group by name under `parent`, creating it if needed.
    pub fn ensure_group(&mut self, name: &str, parent: Option<Uuid>) -> Uuid {
        if let Some(g) = self
            .groups
            .iter()
            .find(|g| g.parent == parent && g.name == name)
        {
            return g.id;
        }
        self.add_group(name, parent)
    }

    /// Repairs references to missing groups and breaks parent cycles, so a hand-edited or
    /// partially imported file can never make the tree unrenderable.
    pub fn sanitize(&mut self) {
        let mut seen = HashSet::new();
        self.groups.retain(|g| seen.insert(g.id));
        let mut seen = HashSet::new();
        self.bookmarks.retain(|b| seen.insert(b.id));

        let ids: HashSet<Uuid> = self.groups.iter().map(|g| g.id).collect();
        for g in &mut self.groups {
            if g.parent.is_some_and(|p| !ids.contains(&p) || p == g.id) {
                g.parent = None;
            }
        }
        // Break cycles: walk up from each group; if we revisit a node, detach it.
        for i in 0..self.groups.len() {
            let mut visited = HashSet::new();
            let mut cur = self.groups[i].parent;
            visited.insert(self.groups[i].id);
            while let Some(p) = cur {
                if !visited.insert(p) {
                    self.groups[i].parent = None;
                    break;
                }
                cur = self
                    .groups
                    .iter()
                    .find(|g| g.id == p)
                    .and_then(|g| g.parent);
            }
        }
        for b in &mut self.bookmarks {
            if b.group.is_some_and(|g| !ids.contains(&g)) {
                b.group = None;
            }
        }
        // A jump host must be another saved connection, without going round in circles.
        let bookmark_ids: HashSet<Uuid> = self.bookmarks.iter().map(|b| b.id).collect();
        for b in &mut self.bookmarks {
            if b.jump
                .is_some_and(|j| j == b.id || !bookmark_ids.contains(&j))
            {
                b.jump = None;
            }
        }
        for i in 0..self.bookmarks.len() {
            if self.jump_chain(&self.bookmarks[i]).is_err() {
                self.bookmarks[i].jump = None;
            }
        }
    }

    /// The jump hosts to go through to reach `b`, the first one to connect to first. An error
    /// when they loop or go deeper than [`MAX_JUMPS`].
    pub fn jump_chain(&self, b: &Bookmark) -> Result<Vec<&Bookmark>, String> {
        let mut chain = Vec::new();
        let mut seen = HashSet::from([b.id]);
        let mut cur = b.jump;
        while let Some(j) = cur {
            if !seen.insert(j) {
                return Err(crate::i18n::t("The jump hosts go round in a circle.").into());
            }
            let Some(hop) = self.bookmark(j) else {
                return Err(crate::i18n::t("The jump host no longer exists.").into());
            };
            chain.push(hop);
            if chain.len() > MAX_JUMPS {
                return Err(crate::i18n::tf(
                    "More than {} jump hosts in a row.",
                    &[&MAX_JUMPS.to_string()],
                ));
            }
            cur = hop.jump;
        }
        chain.reverse();
        Ok(chain)
    }
}

/// The longest chain of jump hosts followed.
pub const MAX_JUMPS: usize = 4;

#[cfg(test)]
mod tests {
    use super::*;

    fn bm(name: &str, group: Option<Uuid>) -> Bookmark {
        Bookmark {
            name: name.into(),
            host: format!("{name}.example"),
            group,
            ..Default::default()
        }
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bookmarks.json");
        let mut s = BookmarkStore::default();
        let prod = s.add_group("Prod", None);
        let db = s.add_group("DB", Some(prod));
        s.upsert_bookmark(bm("web", Some(prod)));
        s.upsert_bookmark(bm("pg", Some(db)));
        s.save(&path).unwrap();
        let loaded = BookmarkStore::load(&path).unwrap();
        assert_eq!(loaded, s);
        assert_eq!(loaded.group_path(Some(db)), "Prod / DB");
    }

    #[test]
    fn missing_file_is_empty_and_newer_schema_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("none.json");
        assert_eq!(
            BookmarkStore::load(&path).unwrap(),
            BookmarkStore::default()
        );
        std::fs::write(
            &path,
            r#"{"schema_version": 99, "groups": [], "bookmarks": []}"#,
        )
        .unwrap();
        // Checked by type: the message is translated (and another test may switch language).
        let err = BookmarkStore::load(&path).unwrap_err();
        assert!(
            matches!(err.downcast_ref::<NewerSchema>(), Some(NewerSchema(99))),
            "{err}"
        );
    }

    #[test]
    fn move_group_refuses_cycles() {
        let mut s = BookmarkStore::default();
        let a = s.add_group("A", None);
        let b = s.add_group("B", Some(a));
        let c = s.add_group("C", Some(b));
        assert!(!s.move_group(a, Some(c), None));
        assert!(!s.move_group(a, Some(a), None));
        assert!(s.move_group(c, None, Some(a)));
        assert_eq!(s.groups[0].id, c);
        assert_eq!(s.group(c).unwrap().parent, None);
    }

    #[test]
    fn move_and_duplicate_bookmarks_keep_order() {
        let mut s = BookmarkStore::default();
        let g = s.add_group("G", None);
        let a = bm("a", None);
        let b = bm("b", None);
        let (aid, bid) = (a.id, b.id);
        s.upsert_bookmark(a);
        s.upsert_bookmark(b);
        s.move_bookmark(bid, None, Some(aid));
        let order: Vec<_> = s.bookmarks_in(None).map(|b| b.name.clone()).collect();
        assert_eq!(order, ["b", "a"]);
        s.move_bookmark(aid, Some(g), None);
        assert_eq!(s.bookmarks_in(Some(g)).count(), 1);
        let dup = s.duplicate_bookmark(bid, "(copy)").unwrap();
        assert_ne!(dup, bid);
        assert_eq!(s.bookmark(dup).unwrap().name, "b (copy)");
        assert_eq!(s.bookmarks[1].id, dup);
    }

    #[test]
    fn removing_group_reparents_contents() {
        let mut s = BookmarkStore::default();
        let a = s.add_group("A", None);
        let b = s.add_group("B", Some(a));
        let c = s.add_group("C", Some(b));
        s.upsert_bookmark(bm("x", Some(b)));
        s.remove_group(b);
        assert_eq!(s.group(c).unwrap().parent, Some(a));
        assert_eq!(s.bookmarks[0].group, Some(a));
    }

    #[test]
    fn sanitize_fixes_dangling_and_cyclic_refs() {
        let mut s = BookmarkStore::default();
        let a = s.add_group("A", None);
        let b = s.add_group("B", Some(a));
        s.groups[0].parent = Some(b); // A <-> B cycle
        s.upsert_bookmark(bm("x", Some(Uuid::new_v4())));
        s.sanitize();
        assert!(s.groups.iter().any(|g| g.parent.is_none()));
        assert_eq!(s.bookmarks[0].group, None);
        assert_eq!(
            s.group_and_descendants(a).len() + s.group_and_descendants(b).len(),
            3
        );
    }
}
