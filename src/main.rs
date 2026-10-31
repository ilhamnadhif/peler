#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;

/// Metal (through wgpu) on macOS; OpenGL elsewhere, which opens a window faster and keeps the
/// binary small.
#[cfg(target_os = "macos")]
const RENDERER: eframe::Renderer = eframe::Renderer::Wgpu;
#[cfg(not(target_os = "macos"))]
const RENDERER: eframe::Renderer = eframe::Renderer::Glow;

fn main() -> eframe::Result {
    // For installers, package checks and the updater, which runs a download to see it works.
    if std::env::args()
        .skip(1)
        .any(|a| a == "--version" || a == "-V")
    {
        println!("Peler {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    peler::app::mark_process_start();
    install_crash_log();
    // Window position and size from the last run (the app checks it is on a screen).
    let saved = peler::settings::Settings::load(&peler::paths::settings_file())
        .ok()
        .and_then(|s| s.window_frame());
    // Arguments: bookmark names or user@host[:port] to open on launch.
    let targets = peler::instance::targets(std::env::args().skip(1));
    // A second launch hands its arguments to the running app instead of opening another
    // window on the same bookmarks file.
    let (lock, inbox) = match peler::instance::acquire(&targets) {
        peler::instance::Instance::Forwarded => return Ok(()),
        peler::instance::Instance::Primary { lock, inbox } => (lock, inbox),
    };
    // The app is created by whichever attempt opens a window.
    let start = std::rc::Rc::new(std::cell::RefCell::new(Some((targets, inbox))));
    let attempt = |renderer: eframe::Renderer| {
        let start = start.clone();
        eframe::run_native(
            "Peler",
            native_options(saved, renderer),
            Box::new(move |cc| {
                let (targets, inbox) = start.borrow_mut().take().expect("the app starts once");
                Ok(Box::new(peler::app::PelerApp::new(cc, targets, inbox)))
            }),
        )
    };
    let result = attempt(RENDERER);
    // Windows without OpenGL 2 (a virtual machine, Remote Desktop, no graphics driver): Direct3D
    // 12, in software when there is no graphics card.
    #[cfg(windows)]
    let result = match result {
        Err(_) if start.borrow().is_some() => attempt(eframe::Renderer::Wgpu),
        other => other,
    };
    drop(lock);
    if let Err(e) = &result {
        report_start_failure(e);
    }
    result
}

fn native_options(saved: Option<[f32; 4]>, renderer: eframe::Renderer) -> eframe::NativeOptions {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Peler")
        // On Linux the id ties the window to peler.desktop (its name and icon in the dock).
        .with_app_id(if cfg!(target_os = "linux") {
            "peler"
        } else {
            "dev.peler.app"
        })
        .with_inner_size([1180.0, 760.0])
        .with_min_inner_size([640.0, 400.0])
        .with_icon(window_icon());
    if let Some([x, y, w, h]) = saved {
        viewport = viewport.with_position([x, y]).with_inner_size([w, h]);
    }
    eframe::NativeOptions {
        viewport,
        // The app installs its own menu bar on macOS.
        #[cfg(target_os = "macos")]
        event_loop_builder: Some(Box::new(|builder| {
            use winit::platform::macos::EventLoopBuilderExtMacOS as _;
            builder.with_default_menu(false);
        })),
        renderer,
        ..Default::default()
    }
}

/// A window that can't open is said in a dialog: on Windows and Linux nobody sees the console.
fn report_start_failure(error: &eframe::Error) {
    eprintln!("Peler could not open its window: {error}");
    #[cfg(not(target_os = "macos"))]
    {
        let _ = rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title("Peler")
            .set_description(format!("Peler could not open its window:\n\n{error}"))
            .show();
    }
}

/// The window and taskbar icon. On macOS the .app bundle provides the Dock icon, and an empty
/// one stops eframe from setting its own.
fn window_icon() -> egui::IconData {
    if cfg!(target_os = "macos") {
        return egui::IconData::default();
    }
    eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon_256.png")).unwrap_or_default()
}

/// Writes a report to the logs folder (`peler::paths::logs_dir`) when something panics
/// (including inside a connection task, which the app survives), then runs the default hook.
fn install_crash_log() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let dir = peler::paths::logs_dir();
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
