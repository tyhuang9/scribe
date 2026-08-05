mod app;
mod audio;
mod benchmark;
mod compatibility_bridge;
mod config;
mod core;
mod hotkey;
mod managed_downloads;
mod model_catalog;
mod models;
mod prepared_audio;
mod runtime_catalog;
mod runtime_router;
mod stt;
mod text_output;
mod transcription;
mod tray;

use eframe::egui;

#[cfg(any(target_os = "linux", test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LinuxDisplayBackend {
    Auto,
    X11,
    Wayland,
}

fn main() -> eframe::Result<()> {
    configure_graphics_environment();

    let result = eframe::run_native(
        "Scribe",
        native_options(),
        Box::new(|cc| Box::new(app::LocalTranscriberApp::new(cc))),
    );
    if let Err(err) = &result {
        eprintln!("Scribe failed to start: {err}");
        print_linux_display_help(err);
    }
    result
}

fn native_options() -> eframe::NativeOptions {
    eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 760.0])
            .with_min_inner_size([840.0, 600.0])
            .with_resizable(true)
            .with_transparent(false),
        follow_system_theme: true,
        default_theme: eframe::Theme::Light,
        event_loop_builder: Some(Box::new(configure_event_loop_backend)),
        ..Default::default()
    }
}

fn configure_graphics_environment() {
    #[cfg(target_os = "linux")]
    {
        if std::env::var_os("SCRIBE_USE_GPU").is_some()
            || std::env::var_os("LOCAL_TRANSCRIBER_USE_GPU").is_some()
        {
            return;
        }

        // Set before eframe/winit starts threads or creates the GL context.
        unsafe {
            std::env::set_var("LIBGL_ALWAYS_SOFTWARE", "1");
        }
    }
}

#[cfg(target_os = "linux")]
fn configure_event_loop_backend<T: 'static>(event_loop: &mut eframe::EventLoopBuilder<T>) {
    use winit::platform::wayland::EventLoopBuilderExtWayland;
    use winit::platform::x11::EventLoopBuilderExtX11;

    match linux_display_backend_preference() {
        LinuxDisplayBackend::X11 => {
            event_loop.with_x11();
        }
        LinuxDisplayBackend::Wayland => {
            event_loop.with_wayland();
        }
        LinuxDisplayBackend::Auto => {}
    }
}

#[cfg(not(target_os = "linux"))]
fn configure_event_loop_backend<T: 'static>(_event_loop: &mut eframe::EventLoopBuilder<T>) {}

#[cfg(target_os = "linux")]
fn linux_display_backend_preference() -> LinuxDisplayBackend {
    display_backend_preference(
        std::env::var_os("SCRIBE_FORCE_X11").is_some(),
        std::env::var_os("SCRIBE_FORCE_WAYLAND").is_some(),
        running_under_wsl(),
        std::env::var_os("WAYLAND_DISPLAY").is_some(),
        std::env::var_os("DISPLAY").is_some(),
    )
}

#[cfg(any(target_os = "linux", test))]
fn display_backend_preference(
    force_x11: bool,
    force_wayland: bool,
    under_wsl: bool,
    has_wayland: bool,
    has_x11: bool,
) -> LinuxDisplayBackend {
    if force_x11 {
        return LinuxDisplayBackend::X11;
    }
    if force_wayland {
        return LinuxDisplayBackend::Wayland;
    }
    if under_wsl && has_wayland {
        return LinuxDisplayBackend::Wayland;
    }
    if under_wsl && has_x11 {
        return LinuxDisplayBackend::X11;
    }
    LinuxDisplayBackend::Auto
}

#[cfg(target_os = "linux")]
fn running_under_wsl() -> bool {
    if std::env::var_os("WSL_INTEROP").is_some() || std::env::var_os("WSL_DISTRO_NAME").is_some() {
        return true;
    }

    std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|release| {
            let release = release.to_ascii_lowercase();
            release.contains("microsoft") || release.contains("wsl")
        })
        .unwrap_or(false)
}

fn print_linux_display_help(_err: &eframe::Error) {
    #[cfg(target_os = "linux")]
    {
        let message = _err.to_string();
        let lower_message = message.to_ascii_lowercase();
        if lower_message.contains("nocompositor")
            || lower_message.contains("wayland")
            || lower_message.contains("x connection")
            || lower_message.contains("broken pipe")
            || lower_message.contains("exit failure")
            || lower_message.contains("not supported by winit")
        {
            eprintln!(
                "Linux display diagnostics: DISPLAY={:?}, WAYLAND_DISPLAY={:?}, XDG_RUNTIME_DIR={:?}",
                std::env::var_os("DISPLAY"),
                std::env::var_os("WAYLAND_DISPLAY"),
                std::env::var_os("XDG_RUNTIME_DIR")
            );
            eprintln!(
                "Under WSL this usually means WSLg/Weston crashed or is unreachable. Try `wsl.exe --shutdown` from Windows PowerShell, then reopen WSL and run `cargo run` again. To force X11 for one run, use `SCRIBE_FORCE_X11=1 cargo run`; to force Wayland, use `SCRIBE_FORCE_WAYLAND=1 cargo run`."
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_window_is_resizable_opaque_and_minimum_sized() {
        let options = native_options();

        assert_eq!(options.viewport.inner_size, Some(egui::vec2(1100.0, 760.0)));
        assert_eq!(
            options.viewport.min_inner_size,
            Some(egui::vec2(840.0, 600.0))
        );
        assert_eq!(options.viewport.resizable, Some(true));
        assert_eq!(options.viewport.transparent, Some(false));
        assert!(options.follow_system_theme);
    }

    #[test]
    fn display_backend_force_x11_wins() {
        assert_eq!(
            display_backend_preference(true, true, true, true, true),
            LinuxDisplayBackend::X11
        );
    }

    #[test]
    fn display_backend_force_wayland_when_not_forcing_x11() {
        assert_eq!(
            display_backend_preference(false, true, true, true, true),
            LinuxDisplayBackend::Wayland
        );
    }

    #[test]
    fn display_backend_picks_wayland_under_wsl_when_available() {
        assert_eq!(
            display_backend_preference(false, false, true, true, true),
            LinuxDisplayBackend::Wayland
        );
    }

    #[test]
    fn display_backend_falls_back_to_x11_under_wsl() {
        assert_eq!(
            display_backend_preference(false, false, true, false, true),
            LinuxDisplayBackend::X11
        );
    }

    #[test]
    fn display_backend_keeps_winit_auto_outside_wsl() {
        assert_eq!(
            display_backend_preference(false, false, false, true, true),
            LinuxDisplayBackend::Auto
        );
    }
}
