mod app;
mod audio;
mod config;
mod hotkey;
mod models;
mod stt;

use eframe::egui;

fn main() -> eframe::Result<()> {
    configure_graphics_environment();

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

fn configure_graphics_environment() {
    #[cfg(target_os = "linux")]
    {
        if std::env::var_os("LOCAL_TRANSCRIBER_USE_GPU").is_some() {
            return;
        }

        // Set before eframe/winit starts threads or creates the GL context.
        unsafe {
            std::env::set_var("LIBGL_ALWAYS_SOFTWARE", "1");
        }
    }
}
