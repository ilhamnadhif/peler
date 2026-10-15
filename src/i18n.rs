//! English / Indonesian UI strings.
//!
//! UI code writes English text through `t("...")`; when the active language is Indonesian the
//! string is looked up in `ID` and falls back to English if a translation is missing. A unit
//! test checks that every literal passed to `t(...)` in the source has a translation.

use std::collections::HashMap;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    /// Follow the macOS preferred language.
    #[default]
    System,
    English,
    Indonesian,
}

static INDONESIAN: AtomicBool = AtomicBool::new(false);

/// Applies a language preference for all subsequent `t` calls.
pub fn set_language(lang: Language) {
    let id = match lang {
        Language::English => false,
        Language::Indonesian => true,
        Language::System => system_prefers_indonesian(),
    };
    INDONESIAN.store(id, Ordering::Relaxed);
}

pub fn is_indonesian() -> bool {
    INDONESIAN.load(Ordering::Relaxed)
}

fn system_prefers_indonesian() -> bool {
    sys_locale::get_locales()
        .next()
        .is_some_and(|l| l.starts_with("id") || l.starts_with("in"))
}

/// Translates an English UI string into the active language.
pub fn t(en: &'static str) -> &'static str {
    if !is_indonesian() {
        return en;
    }
    table().get(en).copied().unwrap_or(en)
}

/// `t` followed by replacing each `{}` with the next argument, in order.
pub fn tf(en: &'static str, args: &[&str]) -> String {
    let mut out = String::new();
    let mut rest = t(en);
    let mut args = args.iter();
    while let Some(pos) = rest.find("{}") {
        out.push_str(&rest[..pos]);
        out.push_str(args.next().copied().unwrap_or(""));
        rest = &rest[pos + 2..];
    }
    out.push_str(rest);
    out
}

fn table() -> &'static HashMap<&'static str, &'static str> {
    static TABLE: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    TABLE.get_or_init(|| ID.iter().copied().collect())
}

/// English → Indonesian.
const ID: &[(&str, &str)] = &[
    ("(copy)", "(salinan)"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_and_format() {
        INDONESIAN.store(true, Ordering::Relaxed);
        assert_eq!(t("(copy)"), "(salinan)");
        assert_eq!(t("__missing__"), "__missing__");
        INDONESIAN.store(false, Ordering::Relaxed);
        assert_eq!(tf("{} of {}", &["1", "2"]), "1 of 2");
    }

    /// Every `t("...")` / `tf("...")` literal in the source must have an Indonesian entry.
    #[test]
    fn every_ui_string_is_translated() {
        let mut missing = Vec::new();
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut stack = vec![src];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") || path.ends_with("i18n.rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap();
                for lit in extract_literals(&text) {
                    if !table().contains_key(lit.as_str()) && !missing.contains(&lit) {
                        missing.push(lit);
                    }
                }
            }
        }
        missing.sort();
        assert!(missing.is_empty(), "untranslated strings:\n{}", missing.join("\n"));
    }

    /// Finds string literals passed directly to `t(` or `tf(`.
    fn extract_literals(text: &str) -> Vec<String> {
        let mut out = Vec::new();
        for pat in ["t(\"", "tf(\""] {
            let mut rest = text;
            while let Some(pos) = rest.find(pat) {
                let prev = rest[..pos].chars().last();
                rest = &rest[pos + pat.len()..];
                if prev.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                    continue; // e.g. `print("` or `contains_key("`
                }
                let mut lit = String::new();
                let mut chars = rest.chars();
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => match chars.next() {
                            Some('n') => lit.push('\n'),
                            Some(other) => lit.push(other),
                            None => break,
                        },
                        '"' => break,
                        c => lit.push(c),
                    }
                }
                out.push(lit);
            }
        }
        out
    }
}
