use anyhow::{Context, Result, anyhow};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

const MENU_SHOW: &str = "scribe-show";
const MENU_HIDE: &str = "scribe-hide";
const MENU_TOGGLE_RECORDING: &str = "scribe-toggle-recording";
const MENU_COPY_LAST: &str = "scribe-copy-last";
const MENU_QUIT: &str = "scribe-quit";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrayCommand {
    Show,
    Hide,
    ToggleRecording,
    CopyLastTranscript,
    Quit,
}

pub struct TrayService {
    _tray_icon: TrayIcon,
    toggle_recording_item: MenuItem,
    copy_last_item: MenuItem,
    _show_item: MenuItem,
    _hide_item: MenuItem,
    _quit_item: MenuItem,
    _separator: PredefinedMenuItem,
}

impl TrayService {
    pub fn new(is_recording: bool, has_transcript: bool) -> Result<Self> {
        if std::env::var_os("SCRIBE_DISABLE_TRAY").is_some() {
            return Err(anyhow!("system tray disabled by SCRIBE_DISABLE_TRAY"));
        }
        if running_in_wsl() && std::env::var_os("SCRIBE_ENABLE_TRAY").is_none() {
            return Err(anyhow!(
                "system tray disabled by default under WSL; set SCRIBE_ENABLE_TRAY=1 to enable it"
            ));
        }
        ensure_tray_runtime_available()?;
        catch_tray_init_panic(|| Self::build(is_recording, has_transcript))?
    }

    fn build(is_recording: bool, has_transcript: bool) -> Result<Self> {
        let show_item = MenuItem::with_id(MENU_SHOW, "Show Scribe", true, None);
        let hide_item = MenuItem::with_id(MENU_HIDE, "Hide Window", true, None);
        let toggle_recording_item = MenuItem::with_id(
            MENU_TOGGLE_RECORDING,
            recording_label(is_recording),
            true,
            None,
        );
        let copy_last_item =
            MenuItem::with_id(MENU_COPY_LAST, "Copy Last Transcript", has_transcript, None);
        let separator = PredefinedMenuItem::separator();
        let quit_item = MenuItem::with_id(MENU_QUIT, "Quit Scribe", true, None);

        let menu = Menu::new();
        menu.append_items(&[
            &show_item,
            &hide_item,
            &toggle_recording_item,
            &copy_last_item,
            &separator,
            &quit_item,
        ])
        .context("failed to build tray menu")?;

        let tray_icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("Scribe")
            .with_icon(scribe_icon()?)
            .build()
            .context("failed to create tray icon")?;

        Ok(Self {
            _tray_icon: tray_icon,
            toggle_recording_item,
            copy_last_item,
            _show_item: show_item,
            _hide_item: hide_item,
            _quit_item: quit_item,
            _separator: separator,
        })
    }

    pub fn set_recording(&self, is_recording: bool) {
        self.toggle_recording_item
            .set_text(recording_label(is_recording));
    }

    pub fn set_has_transcript(&self, has_transcript: bool) {
        self.copy_last_item.set_enabled(has_transcript);
    }

    pub fn poll_command(&self) -> Option<TrayCommand> {
        let mut command = None;
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if let Some(menu_command) = command_from_menu_id(event.id().as_ref()) {
                command = Some(menu_command);
            }
        }
        if command.is_some() {
            while TrayIconEvent::receiver().try_recv().is_ok() {}
            return command;
        }
        while let Ok(event) = TrayIconEvent::receiver().try_recv() {
            if tray_event_should_show(&event) {
                command = Some(TrayCommand::Show);
            }
        }
        command
    }
}

#[cfg(target_os = "linux")]
fn ensure_tray_runtime_available() -> Result<()> {
    let candidates = [
        "libayatana-appindicator3.so.1",
        "libappindicator3.so.1",
        "libayatana-appindicator3.so",
        "libappindicator3.so",
    ];

    if candidates
        .iter()
        .any(|name| unsafe { libloading::Library::new(name).is_ok() })
    {
        Ok(())
    } else {
        Err(anyhow!(
            "system tray unavailable: install libayatana-appindicator3-1 or libappindicator3-1"
        ))
    }
}

#[cfg(not(target_os = "linux"))]
fn ensure_tray_runtime_available() -> Result<()> {
    Ok(())
}

#[cfg(target_os = "linux")]
fn running_in_wsl() -> bool {
    std::env::var_os("WSL_DISTRO_NAME").is_some()
        || std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .map(|release| {
                let release = release.to_ascii_lowercase();
                release.contains("microsoft") || release.contains("wsl")
            })
            .unwrap_or(false)
}

#[cfg(not(target_os = "linux"))]
fn running_in_wsl() -> bool {
    false
}

fn catch_tray_init_panic<T>(init: impl FnOnce() -> T + std::panic::UnwindSafe) -> Result<T> {
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let result = std::panic::catch_unwind(init);
    std::panic::set_hook(previous_hook);

    result.map_err(|payload| {
        anyhow!(
            "system tray unavailable: {}",
            panic_payload_message(payload)
        )
    })
}

fn panic_payload_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "tray initialization panicked".to_owned()
    }
}

fn command_from_menu_id(id: &str) -> Option<TrayCommand> {
    match id {
        MENU_SHOW => Some(TrayCommand::Show),
        MENU_HIDE => Some(TrayCommand::Hide),
        MENU_TOGGLE_RECORDING => Some(TrayCommand::ToggleRecording),
        MENU_COPY_LAST => Some(TrayCommand::CopyLastTranscript),
        MENU_QUIT => Some(TrayCommand::Quit),
        _ => None,
    }
}

fn tray_event_should_show(event: &TrayIconEvent) -> bool {
    matches!(
        event,
        TrayIconEvent::DoubleClick {
            button: MouseButton::Left,
            ..
        } | TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        }
    )
}

fn recording_label(is_recording: bool) -> &'static str {
    if is_recording {
        "Stop Recording"
    } else {
        "Start Recording"
    }
}

fn scribe_icon() -> Result<Icon> {
    const SIZE: u32 = 16;
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let inside = (2..=13).contains(&x) && (2..=13).contains(&y);
            let mic_stem = (7..=8).contains(&x) && (5..=12).contains(&y);
            let mic_head = (5..=10).contains(&x) && (3..=8).contains(&y);
            let base = (4..=11).contains(&x) && y == 13;
            let (r, g, b, a) = if !inside {
                (0, 0, 0, 0)
            } else if mic_head || mic_stem || base {
                (255, 255, 255, 255)
            } else {
                (24, 102, 196, 255)
            };
            rgba.extend([r, g, b, a]);
        }
    }

    Icon::from_rgba(rgba, SIZE, SIZE).context("failed to create tray icon bitmap")
}
