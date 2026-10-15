#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Peler")
            .with_app_id("dev.peler.app")
            .with_inner_size([1180.0, 760.0])
            .with_min_inner_size([640.0, 400.0])
            // The .app bundle provides the Dock icon; this stops eframe from setting its own.
            .with_icon(egui::IconData::default()),
        event_loop_builder: Some(Box::new(|builder| {
            use winit::platform::macos::EventLoopBuilderExtMacOS as _;
            builder.with_default_menu(false);
        })),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };
    // Arguments: bookmark names or user@host[:port] to open on launch. Finder/launchd may pass
    // a `-psn_…` process serial number argument, which is ignored.
    let targets: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with("-psn_")).collect();
    eframe::run_native("Peler", options, Box::new(|cc| Ok(Box::new(peler::app::PelerApp::new(cc, targets)))))
}
