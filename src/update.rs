//! Updates from GitHub releases.
//!
//! The app asks GitHub for the latest release, and when it is newer than this build downloads
//! its `Peler-<version>.zip`, checks it (size, SHA-256 published by GitHub, bundle id, version,
//! code signature, and the same signing team when this build has one), and replaces the
//! installed app once Peler has quit, then opens the new version. When the app is not writable
//! by the user (installed by the .pkg as root), macOS asks for an administrator password.
//!
//! macOS's own tools do the work (`curl`, `ditto`, `codesign`, `plutil`), so the updater adds
//! no HTTP or TLS code to the app.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// The GitHub repository releases come from.
pub const REPO: &str = "ilhamnadhif/peler";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
const BUNDLE_ID: &str = "dev.peler.app";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
    /// Hex SHA-256, when GitHub published one.
    pub sha256: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Release {
    /// Version without the leading `v`.
    pub version: String,
    pub notes: String,
    /// The release page.
    pub page: String,
    /// The app archive the updater installs.
    pub archive: Option<Asset>,
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<ApiAsset>,
}

#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
    size: u64,
    #[serde(default)]
    digest: Option<String>,
}

/// Where to look for the latest release. Debug builds can point it elsewhere (tests).
fn feed_url() -> String {
    if cfg!(debug_assertions)
        && let Ok(url) = std::env::var("PELER_UPDATE_FEED")
    {
        return url;
    }
    format!("https://api.github.com/repos/{REPO}/releases/latest")
}

/// Allowed URL schemes: HTTPS only (debug builds also read local files, for tests).
fn protocols() -> &'static str {
    if cfg!(debug_assertions) {
        "=https,file"
    } else {
        "=https"
    }
}

fn curl(args: &[&str]) -> Result<Vec<u8>, String> {
    let out = Command::new("/usr/bin/curl")
        .args([
            "-fsSL",
            "--proto",
            protocols(),
            "--proto-redir",
            protocols(),
        ])
        .args(["--max-time", "30"])
        .args(["-H", &format!("User-Agent: Peler/{CURRENT}")])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// `1.10.0` > `1.9.3`; anything after `-` (pre-release) is ignored.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    fn parts(v: &str) -> Vec<u64> {
        v.trim()
            .trim_start_matches('v')
            .split('-')
            .next()
            .unwrap_or("")
            .split('.')
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    }
    let (mut a, mut b) = (parts(candidate), parts(current));
    let n = a.len().max(b.len());
    a.resize(n, 0);
    b.resize(n, 0);
    a > b
}

fn parse_release(json: &[u8]) -> Result<Release, String> {
    let r: ApiRelease = serde_json::from_slice(json).map_err(|e| e.to_string())?;
    if r.draft || r.prerelease {
        return Err("not a final release".into());
    }
    let version = r.tag_name.trim_start_matches('v').to_string();
    let archive_name = format!("Peler-{version}.zip");
    let archive = r
        .assets
        .into_iter()
        .find(|a| a.name == archive_name)
        .map(|a| Asset {
            sha256: a
                .digest
                .and_then(|d| d.strip_prefix("sha256:").map(str::to_ascii_lowercase)),
            name: a.name,
            url: a.browser_download_url,
            size: a.size,
        });
    Ok(Release {
        version,
        notes: r.body.unwrap_or_default(),
        page: r.html_url,
        archive,
    })
}

/// The latest release, if it is newer than this build.
pub fn check() -> Result<Option<Release>, String> {
    let json = curl(&["-H", "Accept: application/vnd.github+json", &feed_url()])?;
    let release = parse_release(&json)?;
    Ok(is_newer(&release.version, CURRENT).then_some(release))
}

/// The installed `Peler.app` this process runs from, when it can be updated in place: not a
/// development build, not a disk image, not a translocated copy macOS made read-only.
pub fn installed_bundle() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let bundle = exe.parent()?.parent()?.parent()?.to_path_buf();
    let is_app =
        bundle.extension().is_some_and(|e| e == "app") && exe.parent()?.ends_with("Contents/MacOS");
    let path = bundle.to_string_lossy();
    let movable = !path.contains("/AppTranslocation/") && !path.starts_with("/Volumes/");
    (is_app && movable).then_some(bundle)
}

/// Progress of a download, for the UI.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Stage {
    #[default]
    Starting,
    Downloading {
        done: u64,
        total: u64,
    },
    Verifying,
}

/// A downloaded and checked update, ready to replace the installed app.
#[derive(Debug)]
pub struct Prepared {
    pub version: String,
    pub dir: PathBuf,
    pub app: PathBuf,
}

fn plist_value(app: &Path, key: &str) -> Option<String> {
    let out = Command::new("/usr/bin/plutil")
        .args(["-extract", key, "raw", "-o", "-"])
        .arg(app.join("Contents/Info.plist"))
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The signing team of an app (`None` for ad-hoc or unsigned builds).
fn team_id(app: &Path) -> Option<String> {
    let out = Command::new("/usr/bin/codesign")
        .args(["-dv"])
        .arg(app)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stderr);
    text.lines()
        .find_map(|l| l.strip_prefix("TeamIdentifier="))
        .map(str::trim)
        .filter(|t| *t != "not set")
        .map(str::to_string)
}

fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Downloads and checks the release's app archive.
pub fn download(release: &Release, stage: &Arc<Mutex<Stage>>) -> Result<Prepared, String> {
    let asset = release
        .archive
        .as_ref()
        .ok_or("this release has no app archive")?;
    let dir = std::env::temp_dir().join(format!("peler-update-{}", release.version));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let archive = dir.join(&asset.name);

    *stage.lock() = Stage::Downloading {
        done: 0,
        total: asset.size,
    };
    let mut child = Command::new("/usr/bin/curl")
        .args([
            "-fsSL",
            "--proto",
            protocols(),
            "--proto-redir",
            protocols(),
        ])
        .args(["--max-time", "1800"])
        .args(["-H", &format!("User-Agent: Peler/{CURRENT}")])
        .arg("-o")
        .arg(&archive)
        .arg(&asset.url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        let done = std::fs::metadata(&archive).map(|m| m.len()).unwrap_or(0);
        *stage.lock() = Stage::Downloading {
            done,
            total: asset.size,
        };
        std::thread::sleep(Duration::from_millis(150));
    };
    if !status.success() {
        let mut err = String::new();
        if let Some(mut e) = child.stderr.take() {
            let _ = e.read_to_string(&mut err);
        }
        return Err(format!("download failed: {}", err.trim()));
    }

    *stage.lock() = Stage::Verifying;
    let size = std::fs::metadata(&archive).map(|m| m.len()).unwrap_or(0);
    if size != asset.size {
        return Err(format!(
            "download incomplete ({size} of {} bytes)",
            asset.size
        ));
    }
    if let Some(expected) = &asset.sha256 {
        let actual = sha256_file(&archive).map_err(|e| e.to_string())?;
        if &actual != expected {
            return Err("the download does not match its published checksum".into());
        }
    }
    let unpacked = dir.join("app");
    let ok = Command::new("/usr/bin/ditto")
        .args(["-x", "-k"])
        .arg(&archive)
        .arg(&unpacked)
        .status()
        .map_err(|e| e.to_string())?
        .success();
    if !ok {
        return Err("the archive could not be unpacked".into());
    }
    let app = unpacked.join("Peler.app");
    if plist_value(&app, "CFBundleIdentifier").as_deref() != Some(BUNDLE_ID) {
        return Err("the download is not Peler".into());
    }
    if plist_value(&app, "CFBundleShortVersionString").as_deref() != Some(&release.version) {
        return Err("the downloaded version does not match the release".into());
    }
    let signed = Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(&app)
        .status()
        .map_err(|e| e.to_string())?
        .success();
    if !signed {
        return Err("the downloaded app's signature is not valid".into());
    }
    // A build signed by a team only accepts updates signed by the same team.
    if let Some(bundle) = installed_bundle()
        && let Some(team) = team_id(&bundle)
        && team_id(&app).as_deref() != Some(&team)
    {
        return Err("the downloaded app is signed by someone else".into());
    }
    let _ = std::fs::remove_file(&archive);
    Ok(Prepared {
        version: release.version.clone(),
        dir,
        app,
    })
}

/// Whether the current user can replace `bundle` without an administrator password.
fn user_can_replace(bundle: &Path) -> bool {
    let probe = |dir: &Path| {
        let p = dir.join(format!(".peler-write-test-{}", std::process::id()));
        let ok = std::fs::File::create(&p).is_ok();
        let _ = std::fs::remove_file(&p);
        ok
    };
    bundle.parent().is_some_and(probe) && probe(&bundle.join("Contents"))
}

fn sh_quote(p: &Path) -> String {
    format!("'{}'", p.to_string_lossy().replace('\'', r"'\''"))
}

/// Starts a helper that waits for this process to exit, swaps in the new app (asking for an
/// administrator password when needed), and opens it. The caller then quits the app.
pub fn launch_installer(prepared: &Prepared, bundle: &Path) -> Result<(), String> {
    let old = bundle.with_file_name(format!(".Peler.app.old-{}", std::process::id()));
    let swap = prepared.dir.join("swap.sh");
    let swap_script = format!(
        "#!/bin/sh\n\
         set -e\n\
         mv {app} {old}\n\
         if mv {new} {app}; then rm -rf {old}; else mv {old} {app}; exit 1; fi\n\
         xattr -dr com.apple.quarantine {app} 2>/dev/null || true\n",
        app = sh_quote(bundle),
        old = sh_quote(&old),
        new = sh_quote(&prepared.app),
    );
    std::fs::write(&swap, swap_script).map_err(|e| e.to_string())?;
    let run_swap = if user_can_replace(bundle) {
        format!("/bin/sh {}", sh_quote(&swap))
    } else {
        // AppleScript asks for an administrator password; nothing changes if it is declined.
        let script = format!(
            "do shell script \"/bin/sh \" & quoted form of \"{}\" with administrator privileges",
            swap.to_string_lossy()
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
        );
        format!("/usr/bin/osascript -e {}", sh_quote(Path::new(&script)))
    };
    let installer = prepared.dir.join("install.sh");
    let script = format!(
        "#!/bin/sh\n\
         while kill -0 {pid} 2>/dev/null; do sleep 0.2; done\n\
         {run_swap} || true\n\
         /usr/bin/open {app}\n\
         rm -rf {dir}\n",
        pid = std::process::id(),
        app = sh_quote(bundle),
        dir = sh_quote(&prepared.dir),
    );
    std::fs::write(&installer, script).map_err(|e| e.to_string())?;
    Command::new("/bin/sh")
        .arg(&installer)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Opens a web page in the default browser.
pub fn open_page(url: &str) {
    if url.starts_with("https://") {
        let _ = Command::new("/usr/bin/open").arg(url).spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(is_newer("1.0.1", "1.0.0"));
        assert!(is_newer("v1.10.0", "1.9.3"));
        assert!(is_newer("2.0", "1.99.99"));
        assert!(!is_newer("1.0.0", "1.0.0"));
        assert!(!is_newer("0.9.9", "1.0.0"));
        assert!(!is_newer("1.0.0-beta.1", "1.0.0"));
    }

    #[test]
    fn release_json() {
        let json = br#"{"tag_name":"v1.2.0","body":"Notes","html_url":"https://github.com/x/y/releases/tag/v1.2.0","draft":false,"prerelease":false,
            "assets":[{"name":"Peler-1.2.0.dmg","browser_download_url":"https://a/dmg","size":10,"digest":"sha256:AA"},
                      {"name":"Peler-1.2.0.zip","browser_download_url":"https://a/zip","size":20,"digest":"sha256:ABCD"}]}"#;
        let r = parse_release(json).unwrap();
        assert_eq!(r.version, "1.2.0");
        let a = r.archive.unwrap();
        assert_eq!(
            (a.url.as_str(), a.size, a.sha256.as_deref()),
            ("https://a/zip", 20, Some("abcd"))
        );
        let pre = br#"{"tag_name":"v2.0.0-rc1","html_url":"u","prerelease":true,"assets":[]}"#;
        assert!(parse_release(pre).is_err());
    }
}
