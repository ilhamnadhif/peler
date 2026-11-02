//! Updates from GitHub releases.
//!
//! The app asks GitHub for the latest release, and when it is newer than this build downloads
//! the file that matches how this copy was installed, checks its size and the SHA-256 GitHub
//! published for it, and installs it once TernaryShell has quit:
//!
//! - macOS: `TernaryShell-<version>.zip`, also checked for bundle id, version, code signature and the
//!   same signing team when this build has one; the installed app is replaced and opened again.
//!   When the app is not writable by the user (installed by the .pkg as root), macOS asks for
//!   an administrator password.
//! - Windows, installed with the setup: the new setup runs silently over it (asking for
//!   administrator rights only for an installation for all users) and opens TernaryShell again.
//! - Linux AppImage, or the .tar.gz unpacked where the user can write: the file is replaced and
//!   TernaryShell opens again. Copies from a .deb or .rpm are updated by downloading the new package.
//!
//! The outcome is left in the data folder for the next launch to report. The system's own
//! tools do the work (`curl`, and on macOS `ditto`, `codesign`, `plutil`; `tar` on Linux), so
//! the updater adds no HTTP or TLS code to the app.

#[cfg(target_os = "macos")]
use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::i18n::t;

/// The GitHub repository releases come from.
pub const REPO: &str = "ilhamnadhif/ternaryshell";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
#[cfg(target_os = "macos")]
const BUNDLE_ID: &str = "dev.ternaryshell.app";
/// Name prefix of the old app while the new one is moved in (next to it, so a crash in between
/// never loses it).
#[cfg(target_os = "macos")]
const ASIDE_PREFIX: &str = ".TernaryShell.app.old-";

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
    /// When it was published (`YYYY-MM-DD`).
    pub date: Option<String>,
    /// The release page.
    pub page: String,
    /// The file the updater installs on this system (none: download it by hand).
    pub download: Option<Asset>,
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    html_url: String,
    #[serde(default)]
    published_at: Option<String>,
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
        && let Ok(url) = std::env::var("TERNARYSHELL_UPDATE_FEED")
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

/// A command for one of the system's tools, without a console window flashing up on Windows.
fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// The system's curl (part of Windows 10 and later, macOS and nearly every Linux).
fn curl_path() -> PathBuf {
    if cfg!(windows) {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        return PathBuf::from(root).join("System32").join("curl.exe");
    }
    let usr = Path::new("/usr/bin/curl");
    if usr.exists() {
        usr.to_path_buf()
    } else {
        PathBuf::from("curl")
    }
}

fn curl(args: &[&str]) -> Result<Vec<u8>, String> {
    let out = command(curl_path())
        .args([
            "-fsSL",
            "--proto",
            protocols(),
            "--proto-redir",
            protocols(),
        ])
        .args(["--max-time", "30"])
        .args(["-H", &format!("User-Agent: TernaryShell/{CURRENT}")])
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

/// The release file that updates a copy installed as `kind`.
fn asset_name(kind: &Install, version: &str) -> Option<String> {
    let arch = std::env::consts::ARCH;
    Some(match kind {
        Install::MacApp(_) => format!("TernaryShell-{version}.zip"),
        Install::WindowsSetup { .. } => format!("TernaryShell-{version}-windows-x64-setup.exe"),
        Install::AppImage(_) => format!("TernaryShell-{version}-linux-{arch}.AppImage"),
        Install::LinuxArchive(_) => format!("TernaryShell-{version}-linux-{arch}.tar.gz"),
        Install::Manual => return None,
    })
}

fn parse_release(json: &[u8], kind: &Install) -> Result<Release, String> {
    let r: ApiRelease = serde_json::from_slice(json).map_err(|e| e.to_string())?;
    if r.draft || r.prerelease {
        return Err("not a final release".into());
    }
    let version = r.tag_name.trim_start_matches('v').to_string();
    let wanted = asset_name(kind, &version);
    let download = r
        .assets
        .into_iter()
        .find(|a| Some(&a.name) == wanted.as_ref())
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
        date: r.published_at.and_then(|d| d.get(..10).map(str::to_string)),
        page: r.html_url,
        download,
    })
}

/// The latest release, if it is newer than this build.
pub fn check() -> Result<Option<Release>, String> {
    let json = curl(&["-H", "Accept: application/vnd.github+json", &feed_url()])?;
    let release = parse_release(&json, &Install::current())?;
    Ok(is_newer(&release.version, CURRENT).then_some(release))
}

/// How this copy of TernaryShell was installed, which decides how it updates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Install {
    /// `TernaryShell.app` (macOS).
    MacApp(PathBuf),
    /// By the Windows setup; `all_users` when it went to Program Files.
    WindowsSetup { exe: PathBuf, all_users: bool },
    /// The AppImage at this path (Linux).
    AppImage(PathBuf),
    /// The binary from the .tar.gz, where the user can replace it (Linux).
    LinuxArchive(PathBuf),
    /// Updated by downloading the new version by hand: a .deb or .rpm, the portable Windows
    /// .zip, a development build.
    Manual,
}

impl Install {
    pub fn current() -> Self {
        let Some(exe) = std::env::current_exe()
            .ok()
            .and_then(|e| e.canonicalize().ok())
        else {
            return Install::Manual;
        };
        if cfg!(debug_assertions) && std::env::var_os("TERNARYSHELL_UPDATE_FEED").is_none() {
            return Install::Manual;
        }
        Self::detect(&exe)
    }

    #[cfg(target_os = "macos")]
    fn detect(_exe: &Path) -> Self {
        installed_bundle().map_or(Install::Manual, Install::MacApp)
    }

    #[cfg(windows)]
    fn detect(exe: &Path) -> Self {
        // The setup leaves its uninstaller next to the app; the portable .zip has none.
        let Some(dir) = exe.parent() else {
            return Install::Manual;
        };
        if !dir.join("unins000.exe").exists() {
            return Install::Manual;
        }
        let all_users = ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"]
            .iter()
            .filter_map(std::env::var_os)
            .any(|p| exe.starts_with(PathBuf::from(p)));
        Install::WindowsSetup {
            exe: exe.to_path_buf(),
            all_users,
        }
    }

    #[cfg(target_os = "linux")]
    fn detect(exe: &Path) -> Self {
        if let Some(image) = std::env::var_os("APPIMAGE").map(PathBuf::from)
            && image.is_file()
        {
            return Install::AppImage(image);
        }
        // Installed by a package (or by root): the package manager updates it.
        if exe.starts_with("/usr/") || exe.starts_with("/opt/") {
            return Install::Manual;
        }
        match exe.parent() {
            Some(dir) if writable(dir) => Install::LinuxArchive(exe.to_path_buf()),
            _ => Install::Manual,
        }
    }
}

/// Whether the user can create files in `dir` (and so replace one there).
#[cfg(target_os = "linux")]
fn writable(dir: &Path) -> bool {
    let p = dir.join(format!(".ternaryshell-write-test-{}", std::process::id()));
    let ok = std::fs::File::create(&p).is_ok();
    let _ = std::fs::remove_file(&p);
    ok
}

/// Whether this copy can install `release` by itself.
pub fn installable(release: &Release) -> bool {
    Install::current() != Install::Manual && release.download.is_some()
}

/// Why the update has to be downloaded by hand.
pub fn manual_reason() -> &'static str {
    if cfg!(target_os = "macos") {
        t(
            "Automatic installation needs TernaryShell in the Applications folder. Download the new version from GitHub instead.",
        )
    } else if cfg!(windows) {
        t(
            "This copy of TernaryShell was not installed with its setup. Download the new version from GitHub instead.",
        )
    } else {
        t(
            "TernaryShell was installed from a package (or where you can't change it). Download the new version from GitHub and install it the same way.",
        )
    }
}

/// The installed `TernaryShell.app` this process runs from, when it can be updated in place: not a
/// development build, not a disk image, not a translocated copy macOS made read-only.
#[cfg(target_os = "macos")]
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
    /// The download's private folder.
    pub dir: PathBuf,
    /// What gets installed: `TernaryShell.app` (macOS), the setup (Windows), the new AppImage or
    /// binary (Linux).
    pub file: PathBuf,
}

#[cfg(target_os = "macos")]
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
#[cfg(target_os = "macos")]
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

#[cfg(target_os = "macos")]
fn signature_valid(app: &Path) -> bool {
    Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict"])
        .arg(app)
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// A new folder for one download, private to this user and never shared with another download
/// or another running TernaryShell.
fn new_download_dir() -> Result<PathBuf, String> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    loop {
        let dir = std::env::temp_dir().join(format!(
            "ternaryshell-update-{}-{nanos}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        // `create` (not `create_all`) fails when anything, a link included, is already there.
        #[cfg_attr(not(unix), allow(unused_mut))]
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        match builder.create(&dir) {
            Ok(()) => return Ok(dir),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
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

/// Downloads and checks the release's file for this installation.
pub fn download(release: &Release, stage: &Arc<Mutex<Stage>>) -> Result<Prepared, String> {
    let asset = release
        .download
        .as_ref()
        .ok_or("this release has no file for this system")?;
    let dir = new_download_dir()?;
    let result = download_into(release, asset, &dir, stage);
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&dir);
    }
    result
}

fn download_into(
    release: &Release,
    asset: &Asset,
    dir: &Path,
    stage: &Arc<Mutex<Stage>>,
) -> Result<Prepared, String> {
    let archive = fetch(asset, dir, stage)?;
    prepare(release, &archive, dir)
}

/// Downloads `asset` into `dir` and checks its size and SHA-256. A file without a published
/// checksum is refused.
fn fetch(asset: &Asset, dir: &Path, stage: &Arc<Mutex<Stage>>) -> Result<PathBuf, String> {
    let archive = dir.join(&asset.name);
    *stage.lock() = Stage::Downloading {
        done: 0,
        total: asset.size,
    };
    let mut child = command(curl_path())
        .args([
            "-fsSL",
            "--proto",
            protocols(),
            "--proto-redir",
            protocols(),
        ])
        .args(["--max-time", "1800"])
        .args(["-H", &format!("User-Agent: TernaryShell/{CURRENT}")])
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
    let expected = asset
        .sha256
        .as_ref()
        .ok_or("the download has no published checksum")?;
    let actual = sha256_file(&archive).map_err(|e| e.to_string())?;
    if &actual != expected {
        return Err("the download does not match its published checksum".into());
    }
    Ok(archive)
}

/// macOS: unpacks the app and checks it is the right, intact TernaryShell.
#[cfg(target_os = "macos")]
fn prepare(release: &Release, archive: &Path, dir: &Path) -> Result<Prepared, String> {
    let unpacked = dir.join("app");
    let ok = Command::new("/usr/bin/ditto")
        .args(["-x", "-k"])
        .arg(archive)
        .arg(&unpacked)
        .status()
        .map_err(|e| e.to_string())?
        .success();
    if !ok {
        return Err("the archive could not be unpacked".into());
    }
    let app = unpacked.join("TernaryShell.app");
    if plist_value(&app, "CFBundleIdentifier").as_deref() != Some(BUNDLE_ID) {
        return Err("the download is not TernaryShell".into());
    }
    if plist_value(&app, "CFBundleShortVersionString").as_deref() != Some(&release.version) {
        return Err("the downloaded version does not match the release".into());
    }
    if !signature_valid(&app) {
        return Err("the downloaded app's signature is not valid".into());
    }
    // A build signed by a team only accepts updates signed by the same team.
    if let Some(bundle) = installed_bundle()
        && let Some(team) = team_id(&bundle)
        && team_id(&app).as_deref() != Some(&team)
    {
        return Err("the downloaded app is signed by someone else".into());
    }
    let _ = std::fs::remove_file(archive);
    Ok(Prepared {
        version: release.version.clone(),
        dir: dir.to_path_buf(),
        file: app,
    })
}

/// Windows: the setup is run as it is.
#[cfg(windows)]
fn prepare(release: &Release, setup: &Path, dir: &Path) -> Result<Prepared, String> {
    Ok(Prepared {
        version: release.version.clone(),
        dir: dir.to_path_buf(),
        file: setup.to_path_buf(),
    })
}

/// Linux: an AppImage is used as it is; from the .tar.gz, its `bin/ternaryshell`. Either must run and
/// say it is this version.
#[cfg(target_os = "linux")]
fn prepare(release: &Release, download: &Path, dir: &Path) -> Result<Prepared, String> {
    use std::os::unix::fs::PermissionsExt;
    let file = if download.extension().is_some_and(|e| e == "gz") {
        let unpacked = dir.join("unpacked");
        std::fs::create_dir(&unpacked).map_err(|e| e.to_string())?;
        let ok = command("tar")
            .arg("-xzf")
            .arg(download)
            .arg("-C")
            .arg(&unpacked)
            .status()
            .map_err(|e| format!("tar: {e}"))?
            .success();
        if !ok {
            return Err("the archive could not be unpacked".into());
        }
        let bin = std::fs::read_dir(&unpacked)
            .map_err(|e| e.to_string())?
            .flatten()
            .map(|e| e.path().join("bin/ternaryshell"))
            .find(|p| p.is_file())
            .ok_or("the archive has no TernaryShell in it")?;
        let _ = std::fs::remove_file(download);
        bin
    } else {
        download.to_path_buf()
    };
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| e.to_string())?;
    let out = command(&file)
        .arg("--version")
        .env_remove("APPIMAGE")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("the new version does not run: {e}"))?;
    let said = String::from_utf8_lossy(&out.stdout);
    if !said.trim().ends_with(&release.version) {
        return Err("the downloaded version does not match the release".into());
    }
    Ok(Prepared {
        version: release.version.clone(),
        dir: dir.to_path_buf(),
        file,
    })
}

/// Whether the current user can replace `bundle` without an administrator password.
#[cfg(target_os = "macos")]
fn user_can_replace(bundle: &Path) -> bool {
    let probe = |dir: &Path| {
        let p = dir.join(format!(".ternaryshell-write-test-{}", std::process::id()));
        let ok = std::fs::File::create(&p).is_ok();
        let _ = std::fs::remove_file(&p);
        ok
    };
    // Renaming a folder needs write access to the folder itself, not only to its parent.
    bundle.parent().is_some_and(probe) && probe(bundle) && probe(&bundle.join("Contents"))
}

/// Replaces the installed app (`$2`) with the downloaded one (`$1`), as the user or as root.
/// The download stays writable by the user, so it is first copied into a new folder only this
/// process can write (`/private/tmp` is sticky: nobody else can rename that folder either), and
/// everything is checked on that copy: signature, bundle id (`$5`), version (`$3`) and, when
/// set, signing team (`$4`). As root the copy gets the owner of the app it replaces. The old app
/// is moved aside, and put back when the new one can't be moved in. Only the last line of a
/// failure is meant for the user.
#[cfg(target_os = "macos")]
const SWAP_SCRIPT: &str = r#"
fail() { echo "$1" >&2; exit 1; }
src=$1 app=$2 version=$3 team=$4 id=$5
umask 022
[ -d "$app" ] || fail "the installed app was not found"
stage=$(/usr/bin/mktemp -d /private/tmp/ternaryshell-update.XXXXXXXX) || fail "no folder for the update could be created"
trap '/bin/rm -rf "$stage"' EXIT
new=$stage/TernaryShell.app
/usr/bin/ditto --norsrc --noextattr --noacl "$src" "$new" 2>/dev/null || fail "the update could not be copied"
if [ "$(/usr/bin/id -u)" = 0 ]; then
    owner=$(/usr/bin/stat -f %u:%g "$app") && /usr/sbin/chown -R "$owner" "$new" ||
        fail "the update could not be given the installed app's owner"
fi
# No set-id bits, nothing writable by others.
/bin/chmod -R u+rwX,go+rX,go-w,a-st "$new" || fail "the update's permissions could not be set"
/usr/bin/codesign --verify --deep --strict "$new" 2>/dev/null || fail "the downloaded app's signature is not valid"
plist() { /usr/bin/plutil -extract "$1" raw -o - "$new/Contents/Info.plist" 2>/dev/null; }
[ "$(plist CFBundleIdentifier)" = "$id" ] || fail "the download is not TernaryShell"
[ "$(plist CFBundleShortVersionString)" = "$version" ] || fail "the downloaded version does not match the release"
if [ -n "$team" ]; then
    signer=$(/usr/bin/codesign -dv "$new" 2>&1 | /usr/bin/sed -n 's/^TeamIdentifier=//p')
    [ "$signer" = "$team" ] || fail "the downloaded app is signed by someone else"
fi
dir=$(/usr/bin/dirname "$app")
old=$(/usr/bin/mktemp -u "$dir/.TernaryShell.app.old-XXXXXXXX") || fail "the installed app could not be moved aside"
# -h: never move into a directory a link points to.
/bin/mv -h "$app" "$old" 2>/dev/null || fail "the installed app could not be moved aside"
if ! /bin/mv -h "$new" "$app" 2>/dev/null; then
    /bin/rm -rf "$app"
    /bin/mv -h "$old" "$app" 2>/dev/null || fail "the new version could not be moved into place; the old one is in $old"
    fail "the new version could not be moved into place"
fi
/bin/rm -rf "$old"
# Old copies an interrupted update left behind; the app itself is in place and checked.
for leftover in "$dir"/.TernaryShell.app.old-*; do
    if [ -e "$leftover" ] || [ -L "$leftover" ]; then /bin/rm -rf "$leftover"; fi
done
exit 0
"#;

/// Waits for TernaryShell (`$1`) to quit, runs the command after the first five arguments (the swap,
/// directly or behind the administrator password prompt), records its outcome in `$2` for the
/// next launch, opens the app (`$5`) and deletes the download (`$3`). It runs as the user, and
/// nothing is spliced into the script: every value is an argument.
#[cfg(target_os = "macos")]
const HELPER_SCRIPT: &str = r#"
pid=$1 status=$2 download=$3 version=$4 app=$5
shift 5
while /bin/kill -0 "$pid" 2>/dev/null; do /bin/sleep 0.2; done
out=$("$@" 2>&1)
rc=$?
printf '%s\n%s\n%s\n' "$version" "$rc" "$out" > "$status.tmp" && /bin/mv -f "$status.tmp" "$status"
/usr/bin/open "$app"
/bin/rm -rf "$download"
"#;

/// Runs the swap script (item 2) as root with the remaining items as its arguments, after
/// macOS asked for an administrator password with the prompt in item 1; nothing changes if it
/// is declined. AppleScript quotes every argument, and no file the user can write is run.
#[cfg(target_os = "macos")]
const ADMIN_APPLESCRIPT: &str = r#"on run argv
    set cmd to "/bin/sh -c " & quoted form of (item 2 of argv) & " ternaryshell-update"
    repeat with arg in rest of rest of argv
        set cmd to cmd & " " & quoted form of (contents of arg)
    end repeat
    do shell script cmd with prompt (item 1 of argv) with administrator privileges
end run"#;

/// Where the installer helper leaves the outcome of an update for the next launch.
fn outcome_file() -> PathBuf {
    crate::paths::data_dir().join("update-outcome")
}

/// Starts the installation of a prepared update; the caller then quits the app.
pub fn launch_installer(prepared: &Prepared) -> Result<(), String> {
    match Install::current() {
        #[cfg(target_os = "macos")]
        Install::MacApp(bundle) => launch_mac_installer(prepared, &bundle),
        #[cfg(windows)]
        Install::WindowsSetup { all_users, .. } => launch_windows_setup(prepared, all_users),
        #[cfg(target_os = "linux")]
        Install::AppImage(path) | Install::LinuxArchive(path) => {
            replace_and_relaunch(prepared, &path)
        }
        _ => Err("this copy of TernaryShell can't update itself".into()),
    }
}

/// Windows: runs the new setup silently over the installed version, once this process has quit
/// (the setup closes it, and opens TernaryShell again when done). An installation for all users asks
/// for administrator rights.
#[cfg(windows)]
fn launch_windows_setup(prepared: &Prepared, all_users: bool) -> Result<(), String> {
    record_pending(&prepared.version)?;
    command(&prepared.file)
        .args([
            "/SILENT",
            "/SUPPRESSMSGBOXES",
            "/NORESTART",
            "/NOCANCEL",
            "/update=1",
        ])
        .arg(if all_users {
            "/ALLUSERS"
        } else {
            "/CURRENTUSER"
        })
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Linux: puts the new AppImage or binary in place of the running one (renaming over a running
/// program is fine there), then starts it once this process has quit.
#[cfg(target_os = "linux")]
fn replace_and_relaunch(prepared: &Prepared, target: &Path) -> Result<(), String> {
    use std::os::unix::process::CommandExt;
    let dir = target.parent().ok_or("no folder to update in")?;
    let name = target.file_name().ok_or("no file to update")?;
    // A copy beside the target first, so the final step is one rename on the same disk.
    let staged = dir.join(format!(
        ".{}.update-{}",
        name.to_string_lossy(),
        std::process::id()
    ));
    std::fs::copy(&prepared.file, &staged).map_err(|e| format!("{}: {e}", dir.display()))?;
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755));
    }
    if let Err(e) = std::fs::rename(&staged, target) {
        let _ = std::fs::remove_file(&staged);
        return Err(format!("{}: {e}", target.display()));
    }
    let _ = std::fs::remove_dir_all(&prepared.dir);
    record_pending(&prepared.version)?;
    command("/bin/sh")
        .args([
            "-c",
            r#"while kill -0 "$1" 2>/dev/null; do sleep 0.2; done; exec "$2""#,
            "ternaryshell-update",
        ])
        .arg(std::process::id().to_string())
        .arg(target)
        // The new copy sets these up itself.
        .env_remove("APPIMAGE")
        .env_remove("APPDIR")
        .env_remove("ARGV0")
        .env_remove("OWD")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Windows and Linux: the update about to be installed, so the next launch can tell whether it
/// arrived (the same record the macOS helper leaves, with the outcome decided at that launch).
#[cfg(not(target_os = "macos"))]
fn record_pending(version: &str) -> Result<(), String> {
    crate::paths::write_private(&outcome_file(), format!("{version}\npending\n").as_bytes())
        .map_err(|e| e.to_string())
}

/// Starts a helper that waits for this process to exit, swaps in the new app (asking for an
/// administrator password when needed), and opens it. The caller then quits the app.
#[cfg(target_os = "macos")]
fn launch_mac_installer(prepared: &Prepared, bundle: &Path) -> Result<(), String> {
    // Another copy may have been installed meanwhile (the .pkg, the install script).
    if let Some(installed) = plist_value(bundle, "CFBundleShortVersionString")
        && !is_newer(&prepared.version, &installed)
    {
        return Err(format!(
            "TernaryShell {installed} is already installed; quit TernaryShell and open it again"
        ));
    }
    let status = outcome_file();
    crate::paths::ensure_private_dir(crate::paths::data_dir()).map_err(|e| e.to_string())?;
    let mut swap: Vec<OsString> = if user_can_replace(bundle) {
        ["/bin/sh", "-c", SWAP_SCRIPT, "ternaryshell-update"]
            .map(OsString::from)
            .into()
    } else {
        [
            "/usr/bin/osascript",
            "-e",
            ADMIN_APPLESCRIPT,
            t("TernaryShell wants to install an update."),
            SWAP_SCRIPT,
        ]
        .map(OsString::from)
        .into()
    };
    swap.extend([
        prepared.file.clone().into_os_string(),
        bundle.as_os_str().to_owned(),
        prepared.version.clone().into(),
        team_id(bundle).unwrap_or_default().into(),
        BUNDLE_ID.into(),
    ]);
    Command::new("/bin/sh")
        .args(["-c", HELPER_SCRIPT, "ternaryshell-update"])
        .arg(std::process::id().to_string())
        .arg(&status)
        .arg(&prepared.dir)
        .arg(&prepared.version)
        .arg(bundle)
        .args(&swap)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// How the update started by the previous run went.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// This version was installed.
    Installed(String),
    /// The administrator password prompt was dismissed.
    Cancelled,
    /// Why it was not installed.
    Failed(String),
}

/// The outcome the installer helper recorded, if any. It is reported once: the file is deleted.
pub fn take_outcome() -> Option<Outcome> {
    let path = outcome_file();
    let text = std::fs::read_to_string(&path).ok()?;
    let _ = std::fs::remove_file(&path);
    parse_outcome(&text)
}

/// The helper's record: version, exit status, then what the swap printed. On Windows and Linux
/// the status is `pending`: the update arrived when this is that version.
fn parse_outcome(text: &str) -> Option<Outcome> {
    let mut lines = text.splitn(3, '\n');
    let version = lines.next()?.trim();
    let second = text.lines().nth(1).map(str::trim);
    if second == Some("pending") {
        return Some(if version == CURRENT {
            Outcome::Installed(version.to_string())
        } else {
            Outcome::Failed(t("the installer did not finish").into())
        });
    }
    let code: i32 = lines.next()?.trim().parse().ok()?;
    if code == 0 {
        return Some(Outcome::Installed(version.to_string()));
    }
    let output = lines.next().unwrap_or("");
    // osascript reports "<range>: execution error: <message> (<number>)"; -128 is "User canceled".
    let mut message = output
        .rsplit_once("execution error: ")
        .map_or(output, |(_, m)| m)
        .trim();
    if let Some((rest, number)) = message.rsplit_once(" (")
        && let Some(number) = number.strip_suffix(')')
        && let Ok(number) = number.parse::<i64>()
    {
        if number == -128 {
            return Some(Outcome::Cancelled);
        }
        message = rest;
    }
    // The swap's own message comes last (osascript joins lines with \r).
    let reason = message
        .split(['\r', '\n'])
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .unwrap_or("unknown error");
    Some(Outcome::Failed(reason.chars().take(300).collect()))
}

/// Deletes old copies an interrupted update left next to the app (`.TernaryShell.app.old-…`), but only
/// while the app itself is in place and intact, so the only copy is never removed. Copies owned
/// by root stay until the next update that runs as root removes them.
pub fn remove_leftovers() {
    #[cfg(target_os = "macos")]
    if let Some(bundle) = installed_bundle() {
        remove_leftovers_beside(&bundle, BUNDLE_ID);
    }
}

#[cfg(target_os = "macos")]
fn remove_leftovers_beside(bundle: &Path, id: &str) {
    let Some(Ok(entries)) = bundle.parent().map(std::fs::read_dir) else {
        return;
    };
    let leftovers: Vec<PathBuf> = entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(ASIDE_PREFIX))
        .map(|e| e.path())
        .collect();
    if leftovers.is_empty()
        || plist_value(bundle, "CFBundleIdentifier").as_deref() != Some(id)
        || !signature_valid(bundle)
    {
        return;
    }
    for path in leftovers {
        // A link is removed itself, never followed.
        let _ = match std::fs::symlink_metadata(&path) {
            Ok(m) if m.is_dir() => std::fs::remove_dir_all(&path),
            _ => std::fs::remove_file(&path),
        };
    }
}

/// Opens a web page in the default browser.
pub fn open_page(url: &str) {
    if url.starts_with("https://") {
        crate::platform::open(url);
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
        let json = br#"{"tag_name":"v1.2.0","body":"Notes","html_url":"https://github.com/x/y/releases/tag/v1.2.0","published_at":"2026-10-16T10:08:51Z","draft":false,"prerelease":false,
            "assets":[{"name":"TernaryShell-1.2.0.dmg","browser_download_url":"https://a/dmg","size":10,"digest":"sha256:AA"},
                      {"name":"TernaryShell-1.2.0.zip","browser_download_url":"https://a/zip","size":20,"digest":"sha256:ABCD"}]}"#;
        let mac = Install::MacApp(PathBuf::from("/Applications/TernaryShell.app"));
        let r = parse_release(json, &mac).unwrap();
        assert_eq!(r.version, "1.2.0");
        assert_eq!(r.date.as_deref(), Some("2026-10-16"));
        let a = r.download.unwrap();
        assert_eq!(
            (a.url.as_str(), a.size, a.sha256.as_deref()),
            ("https://a/zip", 20, Some("abcd"))
        );
        let pre = br#"{"tag_name":"v2.0.0-rc1","html_url":"u","prerelease":true,"assets":[]}"#;
        assert!(parse_release(pre, &mac).is_err());
        // Each kind of installation takes its own file; a manual one none.
        let arch = std::env::consts::ARCH;
        let json = format!(
            r#"{{"tag_name":"v1.3.0","html_url":"u","assets":[
                {{"name":"TernaryShell-1.3.0-windows-x64-setup.exe","browser_download_url":"https://a/exe","size":1,"digest":"sha256:01"}},
                {{"name":"TernaryShell-1.3.0-linux-{arch}.AppImage","browser_download_url":"https://a/ai","size":2,"digest":"sha256:02"}},
                {{"name":"TernaryShell-1.3.0-linux-{arch}.tar.gz","browser_download_url":"https://a/tgz","size":3,"digest":"sha256:03"}}]}}"#
        );
        let url = |kind: Install| {
            parse_release(json.as_bytes(), &kind)
                .unwrap()
                .download
                .map(|a| a.url)
        };
        let p = PathBuf::from("/x");
        assert_eq!(
            url(Install::WindowsSetup {
                exe: p.clone(),
                all_users: false
            })
            .as_deref(),
            Some("https://a/exe")
        );
        assert_eq!(
            url(Install::AppImage(p.clone())).as_deref(),
            Some("https://a/ai")
        );
        assert_eq!(
            url(Install::LinuxArchive(p)).as_deref(),
            Some("https://a/tgz")
        );
        assert_eq!(url(Install::Manual), None);
    }

    /// Linux: an update is downloaded (curl, here from a file), checked (checksum, and that it
    /// runs and is the release's version) and put in place of the running file, for an
    /// AppImage and for the binary from the .tar.gz.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_updates_replace_the_running_file() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let script = |version: &str| format!("#!/bin/sh\necho 'TernaryShell {version}'\n");
        let asset = |name: &str, path: &Path| Asset {
            name: name.into(),
            url: format!("file://{}", path.display()),
            size: std::fs::metadata(path).unwrap().len(),
            sha256: Some(sha256_file(path).unwrap()),
        };
        let release = |download: Asset| Release {
            version: "9.9.9".into(),
            notes: String::new(),
            date: None,
            page: String::new(),
            download: Some(download),
        };
        let stage = Arc::new(Mutex::new(Stage::Starting));

        // AppImage.
        let published = tmp.path().join("published.AppImage");
        std::fs::write(&published, script("9.9.9")).unwrap();
        let installed = tmp.path().join("apps/TernaryShell.AppImage");
        std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
        std::fs::write(&installed, script("1.0.0")).unwrap();
        let prepared = download(
            &release(asset("TernaryShell-9.9.9-linux-x.AppImage", &published)),
            &stage,
        )
        .unwrap();
        replace_and_relaunch(&prepared, &installed).unwrap();
        assert_eq!(
            std::fs::read_to_string(&installed).unwrap(),
            script("9.9.9")
        );
        assert_eq!(
            std::fs::metadata(&installed).unwrap().permissions().mode() & 0o777,
            0o755
        );

        // A wrong version or checksum is refused, and nothing changes.
        let other = tmp.path().join("other.AppImage");
        std::fs::write(&other, script("9.9.8")).unwrap();
        let err = download(
            &release(asset("TernaryShell-9.9.9-linux-x.AppImage", &other)),
            &stage,
        )
        .unwrap_err();
        assert!(err.contains("version"), "{err}");
        let mut tampered = asset("TernaryShell-9.9.9-linux-x.AppImage", &published);
        tampered.sha256 = Some("00".into());
        assert!(
            download(&release(tampered), &stage)
                .unwrap_err()
                .contains("checksum")
        );

        // The .tar.gz: its bin/ternaryshell replaces the installed binary.
        let tree = tmp.path().join("tree/ternaryshell-9.9.9/bin");
        std::fs::create_dir_all(&tree).unwrap();
        std::fs::write(tree.join("ternaryshell"), script("9.9.9")).unwrap();
        let tarball = tmp.path().join("published.tar.gz");
        assert!(
            Command::new("tar")
                .arg("-czf")
                .arg(&tarball)
                .arg("-C")
                .arg(tmp.path().join("tree"))
                .arg("ternaryshell-9.9.9")
                .status()
                .unwrap()
                .success()
        );
        let binary = tmp.path().join("local/bin/ternaryshell");
        std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
        std::fs::write(&binary, script("1.0.0")).unwrap();
        let prepared = download(
            &release(asset("TernaryShell-9.9.9-linux-x.tar.gz", &tarball)),
            &stage,
        )
        .unwrap();
        replace_and_relaunch(&prepared, &binary).unwrap();
        assert_eq!(std::fs::read_to_string(&binary).unwrap(), script("9.9.9"));
    }

    #[test]
    fn pending_updates_are_judged_at_the_next_launch() {
        assert_eq!(
            parse_outcome(&format!("{CURRENT}\npending\n")),
            Some(Outcome::Installed(CURRENT.into()))
        );
        assert!(matches!(
            parse_outcome("0.0.1\npending\n"),
            Some(Outcome::Failed(_))
        ));
    }

    #[test]
    fn outcomes() {
        assert_eq!(
            parse_outcome("1.2.0\n0\n\n"),
            Some(Outcome::Installed("1.2.0".into()))
        );
        assert_eq!(
            parse_outcome("1.2.0\n1\n0:301: execution error: User canceled. (-128)\n"),
            Some(Outcome::Cancelled)
        );
        // osascript joins the swap's lines with \r; the last one is the reason.
        assert_eq!(
            parse_outcome(
                "1.2.0\n1\n7:158: execution error: noise\rthe download is not TernaryShell (1)\n"
            ),
            Some(Outcome::Failed("the download is not TernaryShell".into()))
        );
        assert_eq!(
            parse_outcome("1.2.0\n1\nditto: noise\nthe update could not be copied\n"),
            Some(Outcome::Failed("the update could not be copied".into()))
        );
        assert_eq!(
            parse_outcome("1.2.0\n2\n"),
            Some(Outcome::Failed("unknown error".into()))
        );
        assert_eq!(parse_outcome(""), None);
        assert_eq!(parse_outcome("garbage"), None);
    }

    #[test]
    fn download_dirs_are_unique_and_private() {
        let a = new_download_dir().unwrap();
        let b = new_download_dir().unwrap();
        assert_ne!(a, b);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&a).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700);
        }
        std::fs::remove_dir(&a).unwrap();
        std::fs::remove_dir(&b).unwrap();
    }

    #[cfg(target_os = "macos")]
    /// A small ad-hoc signed bundle (its executable is a copy of `true`).
    fn fake_app(parent: &Path, version: &str) -> PathBuf {
        let app = parent.join("TernaryShell.app");
        std::fs::create_dir_all(app.join("Contents/MacOS")).unwrap();
        std::fs::create_dir_all(app.join("Contents/Resources")).unwrap();
        std::fs::copy("/usr/bin/true", app.join("Contents/MacOS/ternaryshell")).unwrap();
        std::fs::write(app.join("Contents/Resources/notes.txt"), version).unwrap();
        std::fs::write(
            app.join("Contents/Info.plist"),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>ternaryshell</string>
<key>CFBundleIdentifier</key><string>dev.ternaryshell.test</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>{version}</string>
</dict></plist>"#
            ),
        )
        .unwrap();
        let signed = Command::new("/usr/bin/codesign")
            .args(["--force", "--sign", "-"])
            .arg(&app)
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(signed.success());
        app
    }

    #[cfg(target_os = "macos")]
    /// Runs `script` the way the helper does for a user-writable app; returns its stderr.
    fn run_swap(script: &str, new: &Path, app: &Path, version: &str, team: &str) -> String {
        let out = Command::new("/bin/sh")
            .args(["-c", script, "ternaryshell-update"])
            .arg(new)
            .arg(app)
            .args([version, team, "dev.ternaryshell.test"])
            .output()
            .unwrap();
        assert_eq!(out.status.success(), out.stderr.is_empty());
        String::from_utf8_lossy(&out.stderr).trim().to_string()
    }

    #[cfg(target_os = "macos")]
    fn leftovers(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(ASIDE_PREFIX))
            .collect()
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn swap_installs_a_checked_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let (apps, download) = (tmp.path().join("Applications"), tmp.path().join("dl"));
        let app = fake_app(&apps, "1.0.0");
        let new = fake_app(&download, "2.0.0");
        // From an update that was interrupted earlier.
        std::fs::create_dir_all(apps.join(".TernaryShell.app.old-1234/Contents")).unwrap();

        assert_eq!(run_swap(SWAP_SCRIPT, &new, &app, "2.0.0", ""), "");
        assert_eq!(
            plist_value(&app, "CFBundleShortVersionString").as_deref(),
            Some("2.0.0")
        );
        assert!(signature_valid(&app));
        assert!(leftovers(&apps).is_empty());
        assert!(new.exists(), "the download is copied, not moved");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn swap_refuses_a_bad_update_and_keeps_the_app() {
        let tmp = tempfile::tempdir().unwrap();
        let (apps, download) = (tmp.path().join("Applications"), tmp.path().join("dl"));
        let app = fake_app(&apps, "1.0.0");
        let new = fake_app(&download, "2.0.0");
        let kept = |app: &Path| {
            assert_eq!(
                plist_value(app, "CFBundleShortVersionString").as_deref(),
                Some("1.0.0")
            );
            assert!(leftovers(&apps).is_empty());
        };

        assert_eq!(
            run_swap(SWAP_SCRIPT, &new, &app, "3.0.0", ""),
            "the downloaded version does not match the release"
        );
        kept(&app);
        assert_eq!(
            run_swap(SWAP_SCRIPT, &new, &app, "2.0.0", "ABCDE12345"),
            "the downloaded app is signed by someone else"
        );
        kept(&app);
        // Changed after it was checked.
        std::fs::write(new.join("Contents/Resources/notes.txt"), "evil").unwrap();
        assert_eq!(
            run_swap(SWAP_SCRIPT, &new, &app, "2.0.0", ""),
            "the downloaded app's signature is not valid"
        );
        kept(&app);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn swap_puts_the_old_app_back_when_the_new_one_cannot_move_in() {
        let tmp = tempfile::tempdir().unwrap();
        let (apps, download) = (tmp.path().join("Applications"), tmp.path().join("dl"));
        let app = fake_app(&apps, "1.0.0");
        let new = fake_app(&download, "2.0.0");
        let move_in = r#"/bin/mv -h "$new" "$app" 2>/dev/null"#;
        assert!(SWAP_SCRIPT.contains(move_in));
        let failing = SWAP_SCRIPT.replace(move_in, "false");

        assert_eq!(
            run_swap(&failing, &new, &app, "2.0.0", ""),
            "the new version could not be moved into place"
        );
        assert_eq!(
            plist_value(&app, "CFBundleShortVersionString").as_deref(),
            Some("1.0.0")
        );
        assert!(signature_valid(&app));
        assert!(leftovers(&apps).is_empty());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn leftovers_go_only_while_the_app_is_intact() {
        let tmp = tempfile::tempdir().unwrap();
        let apps = tmp.path().join("Applications");
        let app = fake_app(&apps, "1.0.0");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("keep.txt"), "x").unwrap();
        std::fs::create_dir_all(apps.join(".TernaryShell.app.old-a/Contents")).unwrap();
        std::os::unix::fs::symlink(&outside, apps.join(".TernaryShell.app.old-link")).unwrap();

        std::fs::write(app.join("Contents/Resources/notes.txt"), "damaged").unwrap();
        remove_leftovers_beside(&app, "dev.ternaryshell.test");
        assert_eq!(leftovers(&apps).len(), 2, "kept: the app is not intact");

        std::fs::remove_dir_all(&app).unwrap();
        let app = fake_app(&apps, "1.0.0");
        remove_leftovers_beside(&app, "dev.ternaryshell.app");
        assert_eq!(leftovers(&apps).len(), 2, "kept: not the expected app");
        remove_leftovers_beside(&app, "dev.ternaryshell.test");
        assert!(leftovers(&apps).is_empty());
        assert!(outside.join("keep.txt").exists(), "links are not followed");
        assert!(app.exists());
    }

    #[cfg(target_os = "macos")]
    /// The administrator variant can't run in a test; the same AppleScript without the password
    /// prompt shows every argument arrives unchanged.
    #[test]
    fn applescript_passes_arguments_verbatim() {
        let privileged = " with prompt (item 1 of argv) with administrator privileges";
        assert!(ADMIN_APPLESCRIPT.contains(privileged));
        let plain = ADMIN_APPLESCRIPT.replace(privileged, "");
        let args = ["it's \"quoted\"", "$HOME `id` \\ ; *", "", "two  spaces"];
        let out = Command::new("/usr/bin/osascript")
            .args(["-e", &plain, "prompt", r#"printf '<%s>' "$@""#])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let expected: String = args.iter().map(|a| format!("<{a}>")).collect();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim_end(), expected);

        // The real one compiles (compiling runs nothing).
        let tmp = tempfile::tempdir().unwrap();
        let compiled = Command::new("/usr/bin/osacompile")
            .args(["-e", ADMIN_APPLESCRIPT, "-o"])
            .arg(tmp.path().join("admin.scpt"))
            .status()
            .unwrap();
        assert!(compiled.success());
    }
}
