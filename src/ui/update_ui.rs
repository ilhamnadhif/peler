//! "Update available": background checks, the toolbar button and the update dialog.

use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use eframe::egui::{self, Id, RichText};
use parking_lot::Mutex;

use super::style;
use crate::i18n::{t, tf};
use crate::settings::Settings;
use crate::sftp::entry::human_size;
use crate::update::{self, CURRENT, Prepared, Release, Stage};

/// Automatic checks: a few seconds after launch (if the last one is old enough), then daily.
const FIRST_CHECK_AFTER: Duration = Duration::from_secs(4);
const CHECK_EVERY_SECS: i64 = 20 * 3600;

enum State {
    Idle,
    Checking,
    UpToDate,
    Error(String),
    Available(Release),
    Downloading(Release),
    Ready(Release, Prepared),
}

enum Msg {
    Checked {
        manual: bool,
        result: Result<Option<Release>, String>,
    },
    Downloaded(Result<Prepared, String>),
}

enum DialogAction {
    Install(Release),
    Skip(String),
}

/// What the app should do after drawing the dialog.
pub enum UpdateEvent {
    None,
    /// The update is ready: quit (after the usual confirmation) and let it install.
    Restart,
}

pub struct Updater {
    state: State,
    rx: Option<mpsc::Receiver<Msg>>,
    stage: Arc<Mutex<Stage>>,
    pub dialog_open: bool,
    launched: Instant,
    /// The version already announced with a toast.
    announced: Option<String>,
}

impl Default for Updater {
    fn default() -> Self {
        Self {
            state: State::Idle,
            rx: None,
            stage: Arc::default(),
            dialog_open: false,
            launched: Instant::now(),
            announced: None,
        }
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

impl Updater {
    fn busy(&self) -> bool {
        self.rx.is_some()
    }

    fn start_check(&mut self, ctx: &egui::Context, manual: bool) {
        if self.busy() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Checked {
                manual,
                result: update::check(),
            });
            ctx.request_repaint();
        });
        self.rx = Some(rx);
        if manual || matches!(self.state, State::Idle | State::UpToDate | State::Error(_)) {
            self.state = State::Checking;
        }
    }

    /// "Check for Updates…" from the menu.
    pub fn check_now(&mut self, ctx: &egui::Context) {
        self.dialog_open = true;
        match self.state {
            // Already found or downloading: just show it.
            State::Available(_) | State::Downloading(_) | State::Ready(..) => {}
            _ => self.start_check(ctx, true),
        }
    }

    /// Runs every frame: schedules automatic checks and collects results. Returns a message to
    /// show as a toast.
    pub fn tick(&mut self, ctx: &egui::Context, settings: &mut Settings) -> Option<String> {
        let mut toast = None;
        if let Some(rx) = &self.rx
            && let Ok(msg) = rx.try_recv()
        {
            self.rx = None;
            match msg {
                Msg::Checked { manual, result } => {
                    settings.last_update_check = Some(now_secs());
                    match result {
                        Ok(Some(release)) => {
                            let skipped = settings.skipped_version.as_deref()
                                == Some(release.version.as_str());
                            if manual || !skipped {
                                if self.announced.as_deref() != Some(&release.version) && !manual {
                                    toast = Some(tf(
                                        "Peler {} is available. Use the Update button to install it.",
                                        &[&release.version],
                                    ));
                                }
                                self.announced = Some(release.version.clone());
                                self.state = State::Available(release);
                            } else {
                                self.state = State::Idle;
                            }
                        }
                        Ok(None) => self.state = if manual { State::UpToDate } else { State::Idle },
                        Err(e) => {
                            self.state = if manual { State::Error(e) } else { State::Idle };
                        }
                    }
                }
                Msg::Downloaded(result) => {
                    match (std::mem::replace(&mut self.state, State::Idle), result) {
                        (State::Downloading(release), Ok(prepared)) => {
                            self.state = State::Ready(release, prepared);
                            self.dialog_open = true;
                        }
                        (_, Err(e)) => {
                            self.state =
                                State::Error(tf("The update could not be installed: {}", &[&e]));
                            self.dialog_open = true;
                        }
                        (other, Ok(_)) => self.state = other,
                    }
                }
            }
        }
        // Automatic checks.
        let due = settings
            .last_update_check
            .is_none_or(|last| now_secs() - last >= CHECK_EVERY_SECS);
        if settings.check_updates
            && due
            && !self.busy()
            && matches!(self.state, State::Idle)
            && self.launched.elapsed() >= FIRST_CHECK_AFTER
        {
            self.start_check(ctx, false);
        }
        if matches!(self.state, State::Downloading(_)) {
            ctx.request_repaint_after(Duration::from_millis(200));
        } else if settings.check_updates && !self.busy() {
            // Wake up for the next scheduled check even when nothing else happens.
            ctx.request_repaint_after(Duration::from_secs(600));
        }
        toast
    }

    /// The toolbar button, while an update is available. Returns true when clicked.
    pub fn toolbar_button(&mut self, ui: &mut egui::Ui) -> bool {
        let label = match &self.state {
            State::Available(r) => format!("⬆ {} {}", t("Update"), r.version),
            State::Downloading(_) => match *self.stage.lock() {
                Stage::Downloading { done, total } if total > 0 => {
                    format!("⬆ {}%", (done as f64 / total as f64 * 100.0).floor())
                }
                _ => format!("⬆ {}", t("Update")),
            },
            State::Ready(..) => format!("⬆ {}", t("Restart to Update")),
            _ => return false,
        };
        let resp = ui
            .add(
                egui::Button::new(RichText::new(label).color(egui::Color32::WHITE))
                    .fill(style::ACCENT),
            )
            .on_hover_text(t("A new version of Peler is available"));
        #[cfg(debug_assertions)]
        crate::devtest::record("update-button", resp.rect);
        let clicked = resp.clicked();
        if clicked {
            self.dialog_open = true;
        }
        clicked
    }

    fn start_download(&mut self, ctx: &egui::Context, release: Release) {
        let (tx, rx) = mpsc::channel();
        let stage = self.stage.clone();
        *stage.lock() = Stage::Starting;
        let ctx = ctx.clone();
        let r = release.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Downloaded(update::download(&r, &stage)));
            ctx.request_repaint();
        });
        self.rx = Some(rx);
        self.state = State::Downloading(release);
    }

    /// Hands the downloaded app to the installer helper; the app must quit right after.
    pub fn launch_installer(&self) -> Result<(), String> {
        let State::Ready(_, prepared) = &self.state else {
            return Err("no update is ready".into());
        };
        let bundle = update::installed_bundle().ok_or("Peler is not installed as an app")?;
        update::launch_installer(prepared, &bundle)
    }

    pub fn show_dialog(&mut self, ctx: &egui::Context, settings: &mut Settings) -> UpdateEvent {
        if !self.dialog_open {
            return UpdateEvent::None;
        }
        let mut event = UpdateEvent::None;
        let mut close = false;
        let mut action = None;
        let modal = egui::Modal::new(Id::new("update-dialog")).show(ctx, |ui| {
            ui.set_width(460.0);
            match &self.state {
                State::Idle | State::Checking => {
                    ui.heading(t("Software Update"));
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(t("Checking for updates…"));
                    });
                    ui.add_space(8.0);
                    if ui.button(t("Cancel")).clicked() {
                        close = true;
                    }
                }
                State::UpToDate => {
                    ui.heading(t("Peler is up to date"));
                    ui.label(tf("Version {} is the latest version.", &[CURRENT]));
                    ui.add_space(8.0);
                    if ui.button(RichText::new(t("OK")).strong()).clicked() {
                        close = true;
                    }
                }
                State::Error(e) => {
                    ui.heading(t("Software Update"));
                    ui.colored_label(ui.visuals().error_fg_color, e.as_str());
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button(RichText::new(t("OK")).strong()).clicked() {
                            close = true;
                        }
                        if ui.button(t("Open Releases Page")).clicked() {
                            update::open_page(&format!(
                                "https://github.com/{}/releases/latest",
                                update::REPO
                            ));
                        }
                    });
                }
                State::Available(release) => {
                    ui.heading(tf("Peler {} is available", &[&release.version]));
                    ui.label(RichText::new(tf("You have version {}.", &[CURRENT])).weak());
                    ui.add_space(6.0);
                    release_notes(ui, &release.notes);
                    ui.add_space(8.0);
                    let installable =
                        update::installed_bundle().is_some() && release.archive.is_some();
                    if !installable {
                        ui.label(
                            RichText::new(t(
                                "Automatic installation needs Peler in the Applications folder. Download the new version from GitHub instead.",
                            ))
                            .weak()
                            .small(),
                        );
                    }
                    ui.horizontal(|ui| {
                        if installable {
                            let install = ui.add(
                                egui::Button::new(
                                    RichText::new(t("Install and Restart")).color(egui::Color32::WHITE),
                                )
                                .fill(style::ACCENT),
                            );
                            #[cfg(debug_assertions)]
                            crate::devtest::record("update-install", install.rect);
                            if install.clicked() {
                                action = Some(DialogAction::Install(release.clone()));
                            }
                        } else if ui.button(RichText::new(t("Download")).strong()).clicked() {
                            update::open_page(&release.page);
                            close = true;
                        }
                        if ui.button(t("Later")).clicked() {
                            close = true;
                        }
                        if ui.button(t("Skip This Version")).clicked() {
                            action = Some(DialogAction::Skip(release.version.clone()));
                        }
                    });
                    if ui.link(t("Release notes on GitHub")).clicked() {
                        update::open_page(&release.page);
                    }
                }
                State::Downloading(release) => {
                    ui.heading(tf("Updating to Peler {}", &[&release.version]));
                    ui.add_space(6.0);
                    let stage = self.stage.lock().clone();
                    let (fraction, text) = match stage {
                        Stage::Starting => (0.0, t("Starting…").to_string()),
                        Stage::Downloading { done, total } => (
                            if total > 0 { done as f32 / total as f32 } else { 0.0 },
                            tf(
                                "Downloading… {} of {}",
                                &[&human_size(done), &human_size(total)],
                            ),
                        ),
                        Stage::Verifying => (1.0, t("Verifying…").to_string()),
                    };
                    ui.add(egui::ProgressBar::new(fraction).fill(style::ACCENT));
                    ui.label(RichText::new(text).weak());
                    ui.add_space(8.0);
                    if ui.button(t("Hide")).clicked() {
                        close = true;
                    }
                }
                State::Ready(release, _) => {
                    ui.heading(tf("Peler {} is ready to install", &[&release.version]));
                    ui.label(t(
                        "Peler will quit, install the new version and open again. Open sessions are closed.",
                    ));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        let restart = ui.add(
                            egui::Button::new(
                                RichText::new(t("Restart Now")).color(egui::Color32::WHITE),
                            )
                            .fill(style::ACCENT),
                        );
                        #[cfg(debug_assertions)]
                        crate::devtest::record("update-restart", restart.rect);
                        if restart.clicked() {
                            event = UpdateEvent::Restart;
                            close = true;
                        }
                        if ui.button(t("Later")).clicked() {
                            close = true;
                        }
                    });
                }
            }
        });
        if super::dialogs::escape_only(ctx, &modal) {
            close = true;
        }
        match action {
            Some(DialogAction::Install(release)) => self.start_download(ctx, release),
            Some(DialogAction::Skip(version)) => {
                settings.skipped_version = Some(version);
                self.state = State::Idle;
                close = true;
            }
            None => {}
        }
        if close {
            self.dialog_open = false;
            if matches!(
                self.state,
                State::UpToDate | State::Error(_) | State::Checking
            ) {
                self.state = State::Idle;
            }
        }
        event
    }
}

/// Release notes from GitHub (Markdown), shown as simple text.
fn release_notes(ui: &mut egui::Ui, notes: &str) {
    if notes.trim().is_empty() {
        return;
    }
    egui::Frame::NONE
        .fill(ui.visuals().faint_bg_color)
        .inner_margin(egui::Margin::same(8))
        .corner_radius(6)
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .max_height(220.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    let mut in_code = false;
                    for line in notes.lines() {
                        let trimmed = line.trim_end();
                        if trimmed.starts_with("```") {
                            in_code = !in_code;
                            continue;
                        }
                        if in_code {
                            ui.label(RichText::new(trimmed).monospace().small());
                        } else if let Some(h) = trimmed.trim_start_matches('#').strip_prefix(' ')
                            && trimmed.starts_with('#')
                        {
                            ui.add_space(4.0);
                            ui.label(RichText::new(h).strong());
                        } else if trimmed.is_empty() {
                            ui.add_space(4.0);
                        } else {
                            let text = trimmed.replace("**", "").replace('`', "");
                            let text = match text.trim_start().strip_prefix("- ") {
                                Some(rest) => format!("•  {rest}"),
                                None => text,
                            };
                            ui.label(text);
                        }
                    }
                });
        });
}
