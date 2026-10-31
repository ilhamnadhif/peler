//! What differs between macOS, Windows and Linux for opening things: links in the browser,
//! folders and files in the file manager.

use std::path::Path;
use std::process::Command;

/// Opens a URL, a folder or a file with what the system uses for it.
pub fn open(target: impl AsRef<std::ffi::OsStr>) {
    let target = target.as_ref();
    #[cfg(target_os = "macos")]
    let result = Command::new("/usr/bin/open").arg(target).spawn();
    // explorer opens folders and files, and hands links to the default browser.
    #[cfg(windows)]
    let result = Command::new("explorer.exe").arg(target).spawn();
    #[cfg(not(any(target_os = "macos", windows)))]
    let result = Command::new("xdg-open").arg(target).spawn();
    let _ = result;
}

/// Shows a file in its folder: selected in Finder and Explorer; on Linux the folder opens.
pub fn reveal(path: &Path) {
    #[cfg(target_os = "macos")]
    {
        let _ = Command::new("/usr/bin/open").arg("-R").arg(path).spawn();
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // `/select,` and the path have to arrive as one argument, quoted the way explorer reads.
        let mut arg = std::ffi::OsString::from("/select,\"");
        arg.push(path.as_os_str());
        arg.push("\"");
        let _ = Command::new("explorer.exe").raw_arg(arg).spawn();
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        open(path.parent().unwrap_or(path));
    }
}

/// The label of the menu item that does [`reveal`].
pub fn reveal_label() -> &'static str {
    if cfg!(target_os = "macos") {
        crate::i18n::t("Show in Finder")
    } else if cfg!(windows) {
        crate::i18n::t("Show in Explorer")
    } else {
        crate::i18n::t("Show in Folder")
    }
}

/// Where saved passwords and keys go, as said inside a sentence ("saved in …").
pub fn keychain() -> &'static str {
    if cfg!(target_os = "macos") {
        crate::i18n::t("the Keychain")
    } else if cfg!(windows) {
        crate::i18n::t("Credential Manager")
    } else {
        crate::i18n::t("the system keyring")
    }
}

/// The name of the system's secret store, in error messages.
pub const SECRET_STORE: &str = if cfg!(target_os = "macos") {
    "Keychain"
} else if cfg!(windows) {
    "Credential Manager"
} else {
    "Secret Service"
};

/// This machine, as said inside a sentence ("files on …").
pub fn this_computer() -> &'static str {
    if cfg!(target_os = "macos") {
        crate::i18n::t("this Mac")
    } else {
        crate::i18n::t("this computer")
    }
}
