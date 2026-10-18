//! Peler: a lightweight SSH/SFTP client for macOS.

pub mod app;
pub mod bookmarks;
pub mod crypto;
#[cfg(debug_assertions)]
pub mod devtest;
pub mod i18n;
pub mod instance;
pub mod netwatch;
pub mod paths;
pub mod secrets;
pub mod settings;
pub mod sftp;
pub mod ssh;
pub mod term;
pub mod ui;
pub mod update;
