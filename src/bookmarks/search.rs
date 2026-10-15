//! Fuzzy bookmark search over name, host, user, tags and group path.

use uuid::Uuid;

use super::model::Bookmark;
use super::store::BookmarkStore;

/// Scores `pattern` as a case-insensitive subsequence of `text`. Higher is better; `None` when
/// the characters don't all appear in order. Consecutive runs and matches at word starts score
/// higher, so "pdb" ranks "prod-db" above "upload-backup".
pub fn fuzzy_score(pattern: &str, text: &str) -> Option<i32> {
    let p: Vec<char> = pattern.chars().flat_map(char::to_lowercase).collect();
    if p.is_empty() {
        return Some(0);
    }
    let t: Vec<char> = text.chars().flat_map(char::to_lowercase).collect();
    if p.len() > t.len() {
        return None;
    }
    // best[j]: best score with the current pattern char matched at text position j.
    let mut best: Vec<Option<i32>> = (0..t.len())
        .map(|j| (t[j] == p[0]).then(|| 16 + boundary_bonus(&t, j) - (j as i32).min(10)))
        .collect();
    for &pc in &p[1..] {
        let mut next = vec![None; t.len()];
        for j in 0..t.len() {
            if t[j] != pc {
                continue;
            }
            let here = 16 + boundary_bonus(&t, j);
            next[j] = (0..j)
                .filter_map(|k| {
                    best[k].map(|s| {
                        let link = if k + 1 == j { 12 } else { -((j - k - 1) as i32).min(8) };
                        s + here + link
                    })
                })
                .max();
        }
        best = next;
    }
    // Prefer shorter texts when everything else is equal.
    best.into_iter().flatten().max().map(|s| s - t.len() as i32 / 8)
}

fn boundary_bonus(t: &[char], i: usize) -> i32 {
    if i == 0 || matches!(t[i - 1], ' ' | '-' | '_' | '.' | '@' | '/' | ':') { 10 } else { 0 }
}

/// Best score of one search term against a bookmark's fields; the name counts most.
fn term_score(term: &str, b: &Bookmark, group_path: &str) -> Option<i32> {
    let fields: [(&str, i32); 4] =
        [(&b.name, 3), (&b.host, 2), (&b.username, 1), (group_path, 1)];
    let mut best: Option<i32> = None;
    for (text, weight) in fields {
        if let Some(s) = fuzzy_score(term, text) {
            best = Some(best.map_or(s * weight, |x| x.max(s * weight)));
        }
    }
    for tag in &b.tags {
        if let Some(s) = fuzzy_score(term, tag) {
            best = Some(best.map_or(s * 2, |x| x.max(s * 2)));
        }
    }
    best
}

/// Bookmarks matching every whitespace-separated term in `query`, best first. An empty query
/// returns all bookmarks in store order.
pub fn search(store: &BookmarkStore, query: &str) -> Vec<Uuid> {
    let terms: Vec<&str> = query.split_whitespace().collect();
    if terms.is_empty() {
        return store.bookmarks.iter().map(|b| b.id).collect();
    }
    let mut hits: Vec<(i32, usize, Uuid)> = Vec::new();
    for (idx, b) in store.bookmarks.iter().enumerate() {
        let path = store.group_path(b.group);
        let mut total = 0;
        let mut all = true;
        for term in &terms {
            match term_score(term, b, &path) {
                Some(s) => total += s,
                None => {
                    all = false;
                    break;
                }
            }
        }
        if all {
            hits.push((total, idx, b.id));
        }
    }
    hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    hits.into_iter().map(|h| h.2).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subsequence_and_ranking() {
        assert!(fuzzy_score("pdb", "prod-db").is_some());
        assert!(fuzzy_score("xyz", "prod-db").is_none());
        assert!(fuzzy_score("pdb", "prod-db") > fuzzy_score("pdb", "upload-backup-db"));
        assert!(fuzzy_score("WEB", "web-01").is_some());
        assert_eq!(fuzzy_score("", "anything"), Some(0));
    }

    #[test]
    fn search_matches_all_terms_across_fields() {
        let mut s = BookmarkStore::default();
        let g = s.add_group("Production", None);
        let mk = |name: &str, host: &str, tags: &[&str], group| Bookmark {
            name: name.into(),
            host: host.into(),
            tags: tags.iter().map(|t| t.to_string()).collect(),
            group,
            ..Default::default()
        };
        s.upsert_bookmark(mk("web 1", "10.0.0.1", &["nginx"], Some(g)));
        s.upsert_bookmark(mk("db main", "db.internal", &["postgres"], Some(g)));
        s.upsert_bookmark(mk("staging web", "stg.example.com", &[], None));

        let names = |q: &str| -> Vec<String> {
            search(&s, q).iter().map(|id| s.bookmark(*id).unwrap().name.clone()).collect()
        };
        assert_eq!(names("postgres"), ["db main"]);
        assert_eq!(names("prod web"), ["web 1"]);
        assert_eq!(names("web")[0], "web 1");
        assert_eq!(names("").len(), 3);
        assert!(names("nomatch").is_empty());
    }
}
