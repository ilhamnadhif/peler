//! Reader for Electerm bookmark exports (filled in from the verified schema).

use anyhow::{Result, bail};

use super::transfer::Incoming;

pub fn looks_like_electerm(value: &serde_json::Value) -> bool {
    value.get("bookmarks").is_some_and(|b| b.is_array()) && value.get("schema_version").is_none()
}

pub fn read(_value: &serde_json::Value) -> Result<Incoming> {
    bail!("Electerm import is not implemented yet")
}
