#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;

fn main() -> eframe::Result {
    peler::app::mark_process_start();
    install_crash_log();
    // Window position and size from the last run (the app checks it is on a screen).
    let saved = peler::settings::Settings::load(&peler::paths::settings_file())
        .ok()
        .and_then(|s| s.window_frame());
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Peler")
        .with_app_id("dev.peler.app")
        .with_inner_size([1180.0, 760.0])
        .with_min_inner_size([640.0, 400.0])
        // The .app bundle provides the Dock icon; this stops eframe from setting its own.
        .with_icon(egui::IconData::default());
    if let Some([x, y, w, h]) = saved {
        viewport = viewport.with_position([x, y]).with_inner_size([w, h]);
    }
    let options = eframe::NativeOptions {
        viewport,
        event_loop_builder: Some(Box::new(|builder| {
            use winit::platform::macos::EventLoopBuilderExtMacOS as _;
            builder.with_default_menu(false);
        })),
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    // Arguments: bookmark names or user@host[:port] to open on launch.
    let targets = peler::instance::targets(std::env::args().skip(1));
    // A second launch hands its arguments to the running app instead of opening another
    // window on the same bookmarks file.
    let (lock, listener) = match peler::instance::acquire(&targets) {
        peler::instance::Instance::Forwarded => return Ok(()),
        peler::instance::Instance::Primary { lock, listener } => (lock, listener),
    };
    let result = eframe::run_native(
        "Peler",
        options,
        Box::new(|cc| Ok(Box::new(peler::app::PelerApp::new(cc, targets, listener)))),
    );
    drop(lock);
    result
}

/// Writes a report to `~/Library/Logs/Peler/` when something panics (including inside a
/// connection task, which the app survives), then runs the default hook.
fn install_crash_log() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let dir = peler::paths::home_dir().join("Library/Logs/Peler");
        if std::fs::create_dir_all(&dir).is_ok() {
            let thread = std::thread::current()
                .name()
                .unwrap_or("unnamed")
                .to_string();
            let report = format!(
                "Peler {} panicked on thread '{thread}' at {}\n{info}\n\n{}\n",
                env!("CARGO_PKG_VERSION"),
                chrono::Local::now().to_rfc3339(),
                std::backtrace::Backtrace::force_capture(),
            );
            let name = format!("panic-{}.log", chrono::Local::now().format("%Y%m%d-%H%M%S"));
            let _ = std::fs::write(dir.join(name), report);
        }
        default(info);
    }));
}
