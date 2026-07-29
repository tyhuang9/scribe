use anyhow::{Context, Result, anyhow};
use crossbeam_channel::{Receiver, Sender, unbounded};
use eframe::egui;
use std::sync::{Arc, OnceLock, RwLock};
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
    command_rx: Receiver<TrayCommand>,
    toggle_recording_item: MenuItem,
    copy_last_item: MenuItem,
    _show_item: MenuItem,
    _hide_item: MenuItem,
    _quit_item: MenuItem,
    _separator: PredefinedMenuItem,
}

impl TrayService {
    pub fn new(ctx: &egui::Context, is_recording: bool, has_transcript: bool) -> Result<Self> {
        if std::env::var_os("SCRIBE_DISABLE_TRAY").is_some() {
            return Err(anyhow!("system tray disabled by SCRIBE_DISABLE_TRAY"));
        }
        if running_in_wsl() && std::env::var_os("SCRIBE_ENABLE_TRAY").is_none() {
            return Err(anyhow!(
                "system tray disabled by default under WSL; set SCRIBE_ENABLE_TRAY=1 to enable it"
            ));
        }
        ensure_tray_runtime_available()?;
        let command_rx = install_event_handlers(ctx);
        catch_tray_init_panic(|| Self::build(command_rx, is_recording, has_transcript))?
    }

    fn build(
        command_rx: Receiver<TrayCommand>,
        is_recording: bool,
        has_transcript: bool,
    ) -> Result<Self> {
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
            .with_menu_on_left_click(false)
            .with_menu_on_right_click(true)
            .with_tooltip("Scribe")
            .with_icon(scribe_icon()?)
            .build()
            .context("failed to create tray icon")?;

        Ok(Self {
            _tray_icon: tray_icon,
            command_rx,
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

    pub fn drain_commands(&self) -> Vec<TrayCommand> {
        drain_commands(&self.command_rx)
    }
}

#[derive(Clone)]
struct EventBridge {
    sender: Sender<TrayCommand>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

struct EventHandlerState {
    receiver: Receiver<TrayCommand>,
    wake_ctx: Arc<RwLock<egui::Context>>,
}

impl EventBridge {
    fn send(&self, command: TrayCommand) {
        let _ = self.sender.send(command);
        (self.wake)();
    }
}

fn install_event_handlers(ctx: &egui::Context) -> Receiver<TrayCommand> {
    static STATE: OnceLock<EventHandlerState> = OnceLock::new();

    let state = STATE.get_or_init(|| {
        let (sender, receiver) = unbounded();
        let wake_ctx = Arc::new(RwLock::new(ctx.clone()));
        let wake_context = wake_ctx.clone();
        let bridge = EventBridge {
            sender,
            wake: Arc::new(move || {
                let ctx = wake_context
                    .read()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone();
                ctx.request_repaint();
            }),
        };

        let menu_bridge = bridge.clone();
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if let Some(command) = command_from_menu_id(event.id().as_ref()) {
                menu_bridge.send(command);
            }
        }));

        TrayIconEvent::set_event_handler(Some(move |event| {
            if tray_event_should_show(&event) {
                bridge.send(TrayCommand::Show);
            }
        }));

        EventHandlerState { receiver, wake_ctx }
    });

    // tray-icon handlers are process-wide and cannot be replaced. Refresh the
    // egui context so a reconstructed app still receives their wake requests.
    *state
        .wake_ctx
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = ctx.clone();
    state.receiver.clone()
}

fn drain_commands(receiver: &Receiver<TrayCommand>) -> Vec<TrayCommand> {
    receiver.try_iter().collect()
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

#[cfg(test)]
mod tests {
    use tray_icon::{Rect, TrayIconId, dpi::PhysicalPosition};

    use super::*;

    fn click_event(button: MouseButton, button_state: MouseButtonState) -> TrayIconEvent {
        TrayIconEvent::Click {
            id: TrayIconId::new("scribe-test"),
            position: PhysicalPosition::new(0.0, 0.0),
            rect: Rect::default(),
            button,
            button_state,
        }
    }

    #[test]
    fn tray_menu_ids_map_to_expected_commands() {
        assert_eq!(command_from_menu_id(MENU_SHOW), Some(TrayCommand::Show));
        assert_eq!(command_from_menu_id(MENU_HIDE), Some(TrayCommand::Hide));
        assert_eq!(
            command_from_menu_id(MENU_TOGGLE_RECORDING),
            Some(TrayCommand::ToggleRecording)
        );
        assert_eq!(
            command_from_menu_id(MENU_COPY_LAST),
            Some(TrayCommand::CopyLastTranscript)
        );
        assert_eq!(command_from_menu_id(MENU_QUIT), Some(TrayCommand::Quit));
        assert_eq!(command_from_menu_id("unknown"), None);
    }

    #[test]
    fn only_a_completed_left_click_restores_the_window() {
        assert!(tray_event_should_show(&click_event(
            MouseButton::Left,
            MouseButtonState::Up
        )));
        assert!(!tray_event_should_show(&click_event(
            MouseButton::Left,
            MouseButtonState::Down
        )));
        assert!(!tray_event_should_show(&click_event(
            MouseButton::Right,
            MouseButtonState::Up
        )));
    }

    #[test]
    fn event_bridge_delivers_commands_and_requests_a_wake() {
        let (sender, receiver) = unbounded();
        let wake_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let wake_counter = wake_count.clone();
        let bridge = EventBridge {
            sender,
            wake: Arc::new(move || {
                wake_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }),
        };

        bridge.send(TrayCommand::Show);
        bridge.send(TrayCommand::ToggleRecording);

        assert_eq!(
            drain_commands(&receiver),
            vec![TrayCommand::Show, TrayCommand::ToggleRecording]
        );
        assert_eq!(wake_count.load(std::sync::atomic::Ordering::Relaxed), 2);
    }

    #[test]
    fn recording_menu_label_tracks_recording_state() {
        assert_eq!(recording_label(false), "Start Recording");
        assert_eq!(recording_label(true), "Stop Recording");
    }
}
