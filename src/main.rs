mod app;
mod audio;
mod config;
mod hotkey;
mod models;
mod stt;

use eframe::egui;

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 760.0])
            .with_min_inner_size([840.0, 600.0]),
        follow_system_theme: false,
        default_theme: eframe::Theme::Light,
        ..Default::default()
    };

    eframe::run_native(
        "Local Transcriber",
        options,
        Box::new(|cc| Box::new(app::LocalTranscriberApp::new(cc))),
    )
}
