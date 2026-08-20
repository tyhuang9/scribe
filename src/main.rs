mod app;
mod audio;
mod benchmark;
mod compatibility_bridge;
mod config;
mod core;
mod diagnostics;
mod disk_space;
mod embedded_runtime;
mod history;
mod history_playback;
mod hotkey;
mod huggingface_catalog;
mod installations;
mod installed_manifest;
mod managed_downloads;
mod model_catalog;
mod models;
// This delivery unit intentionally exposes only private service hooks; a later
// coordinator branch may call its install APIs without widening product types.
#[allow(dead_code)]
mod onnx_model_bundles;
mod onnx_worker;
mod overlay;
mod prepared_audio;
mod runtime_catalog;
mod runtime_router;
mod silero_vad_native;
mod streaming;
mod stt;
mod support_assets;
mod text_output;
mod transcription;
mod tray;
mod ui;

#[cfg(test)]
mod architecture_guard;

use eframe::egui;

#[cfg(any(target_os = "linux", test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LinuxDisplayBackend {
    Auto,
    X11,
    Wayland,
}

fn main() -> eframe::Result<()> {
    if let Some(exit_code) = onnx_worker::maybe_run_worker() {
        std::process::exit(exit_code);
    }
    if let Some(exit_code) = transcription::maybe_run_installation_smoke_helper() {
        std::process::exit(exit_code);
    }
    if let Some(exit_code) = benchmark::maybe_run_local_command() {
        std::process::exit(exit_code);
    }
    if let Err(error) = support_assets::materialize_bundled_support_assets() {
        eprintln!("Scribe support assets are unavailable: {error:#}");
    }
    configure_graphics_environment();

    let result = eframe::run_native(
        "Scribe",
        native_options(),
        Box::new(|cc| {
            #[cfg(all(feature = "ui-harness", debug_assertions))]
            if let Some(fixture) = ui::fixture_from_env() {
                return Box::new(ui::UiHarnessApp::new(cc, fixture));
            }
            Box::new(app::LocalTranscriberApp::new(cc))
        }),
    );
    if let Err(err) = &result {
        eprintln!("Scribe failed to start: {err}");
        print_linux_display_help(err);
    }
    result
}

fn native_options() -> eframe::NativeOptions {
    let inner_size = initial_window_size();
    eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size(inner_size)
            .with_min_inner_size([960.0, 680.0])
            .with_resizable(true)
            .with_transparent(root_viewport_requests_transparency(cfg!(
                target_os = "windows"
            ))),
        follow_system_theme: true,
        default_theme: eframe::Theme::Light,
        event_loop_builder: Some(Box::new(configure_event_loop_backend)),
        ..Default::default()
    }
}

const fn root_viewport_requests_transparency(target_is_windows: bool) -> bool {
    target_is_windows
}

fn initial_window_size() -> [f32; 2] {
    #[cfg(all(feature = "ui-harness", debug_assertions))]
    if ui::fixture_from_env().is_some()
        && let Some(size) = std::env::var("SCRIBE_UI_HARNESS_VIEWPORT")
            .ok()
            .and_then(|value| parse_harness_viewport(&value))
    {
        return size;
    }
    [1180.0, 815.0]
}

#[cfg(all(feature = "ui-harness", debug_assertions))]
fn parse_harness_viewport(value: &str) -> Option<[f32; 2]> {
    let (width, height) = value.trim().split_once('x')?;
    let width = width.trim().parse::<f32>().ok()?;
    let height = height.trim().parse::<f32>().ok()?;
    (width.is_finite() && height.is_finite() && width >= 960.0 && height >= 680.0)
        .then_some([width, height])
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
    fn native_window_is_resizable_and_minimum_sized() {
        let options = native_options();

        assert_eq!(options.viewport.inner_size, Some(egui::vec2(1180.0, 815.0)));
        assert_eq!(
            options.viewport.min_inner_size,
            Some(egui::vec2(960.0, 680.0))
        );
        assert_eq!(options.viewport.resizable, Some(true));
        assert_eq!(
            options.viewport.transparent,
            Some(cfg!(target_os = "windows"))
        );
        assert!(options.follow_system_theme);
    }

    #[test]
    fn root_requests_alpha_capable_config_only_on_windows() {
        assert!(root_viewport_requests_transparency(true));
        assert!(!root_viewport_requests_transparency(false));
    }

    #[cfg(all(feature = "ui-harness", debug_assertions))]
    #[test]
    fn harness_viewport_parser_accepts_supported_sizes_and_rejects_invalid_input() {
        assert_eq!(parse_harness_viewport("960x680"), Some([960.0, 680.0]));
        assert_eq!(
            parse_harness_viewport(" 1180 x 815 "),
            Some([1180.0, 815.0])
        );
        assert_eq!(parse_harness_viewport("959x680"), None);
        assert_eq!(parse_harness_viewport("960x679"), None);
        assert_eq!(parse_harness_viewport("wide"), None);
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
