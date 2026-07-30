use anyhow::{Context, Result, anyhow};
use crossbeam_channel::{Receiver, Sender, unbounded};
use eframe::egui;
use std::sync::{Arc, OnceLock, RwLock};
use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

const MENU_SHOW: &str = "scribe-show";
const MENU_HIDE: &str = "scribe-hide";
const MENU_COPY_LAST: &str = "scribe-copy-last";
const MENU_QUIT: &str = "scribe-quit";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrayCommand {
    Show,
    Hide,
    CopyLastTranscript,
    Quit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NativeWake {
    None,
    Restore,
    Message,
}

pub struct TrayService {
    _tray_icon: TrayIcon,
    command_rx: Receiver<TrayCommand>,
    copy_last_item: MenuItem,
    _show_item: MenuItem,
    _hide_item: MenuItem,
    _quit_item: MenuItem,
    _separator: PredefinedMenuItem,
}

impl TrayService {
    pub fn new(ctx: &egui::Context, has_transcript: bool) -> Result<Self> {
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
        catch_tray_init_panic(|| Self::build(command_rx, has_transcript))?
    }

    fn build(command_rx: Receiver<TrayCommand>, has_transcript: bool) -> Result<Self> {
        let show_item = MenuItem::with_id(MENU_SHOW, "Show Scribe", true, None);
        let hide_item = MenuItem::with_id(MENU_HIDE, "Hide Window", true, None);
        let copy_last_item =
            MenuItem::with_id(MENU_COPY_LAST, "Copy Last Transcript", has_transcript, None);
        let separator = PredefinedMenuItem::separator();
        let quit_item = MenuItem::with_id(MENU_QUIT, "Quit Scribe", true, None);

        let menu = Menu::new();
        menu.append_items(&[
            &show_item,
            &hide_item,
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
            copy_last_item,
            _show_item: show_item,
            _hide_item: hide_item,
            _quit_item: quit_item,
            _separator: separator,
        })
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
    native_wake: Arc<dyn Fn(NativeWake) + Send + Sync>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

struct EventHandlerState {
    receiver: Receiver<TrayCommand>,
    wake_ctx: Arc<RwLock<egui::Context>>,
}

impl EventBridge {
    fn send(&self, command: TrayCommand) {
        if self.sender.send(command).is_err() {
            return;
        }
        (self.native_wake)(native_wake_for_command(command));
        (self.wake)();
    }

    fn send_menu_event(&self, id: &MenuId) {
        if let Some(command) = command_from_menu_id(id) {
            self.send(command);
        }
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
            native_wake: Arc::new(post_native_wake),
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
            menu_bridge.send_menu_event(event.id());
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

fn command_from_menu_id(id: &MenuId) -> Option<TrayCommand> {
    match id.as_ref() {
        MENU_SHOW => Some(TrayCommand::Show),
        MENU_HIDE => Some(TrayCommand::Hide),
        MENU_COPY_LAST => Some(TrayCommand::CopyLastTranscript),
        MENU_QUIT => Some(TrayCommand::Quit),
        _ => None,
    }
}

fn native_wake_for_command(command: TrayCommand) -> NativeWake {
    match command {
        TrayCommand::Show => NativeWake::Restore,
        TrayCommand::Hide => NativeWake::None,
        TrayCommand::CopyLastTranscript => NativeWake::Message,
        // A close message alone does not schedule an egui frame for a hidden
        // winit window. Restoring it lets the queued Quit command close cleanly.
        TrayCommand::Quit => NativeWake::Restore,
    }
}

#[cfg(target_os = "windows")]
fn post_native_wake(wake: NativeWake) {
    use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM};
    use windows_sys::Win32::System::Threading::GetCurrentProcessId;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GW_OWNER, GetWindow, GetWindowTextLengthW, GetWindowTextW,
        GetWindowThreadProcessId, IsWindow, PostMessageW, SC_RESTORE, WM_NULL, WM_SYSCOMMAND,
    };

    if wake == NativeWake::None {
        return;
    }

    struct WindowSearch {
        process_id: u32,
        hwnd: HWND,
    }

    unsafe extern "system" fn find_scribe_window(hwnd: HWND, state: LPARAM) -> BOOL {
        let state = unsafe { &mut *(state as *mut WindowSearch) };
        let mut process_id = 0;
        if unsafe { GetWindowThreadProcessId(hwnd, &mut process_id) } == 0
            || process_id != state.process_id
            || !unsafe { GetWindow(hwnd, GW_OWNER) }.is_null()
        {
            return 1;
        }

        let title_length = unsafe { GetWindowTextLengthW(hwnd) };
        if title_length <= 0 {
            return 1;
        }
        let mut title = vec![0_u16; title_length as usize + 1];
        let copied = unsafe { GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32) };
        if copied > 0 && String::from_utf16_lossy(&title[..copied as usize]) == "Scribe" {
            state.hwnd = hwnd;
            return 0;
        }
        1
    }

    let process_id = unsafe { GetCurrentProcessId() };
    let mut search = WindowSearch {
        process_id,
        hwnd: std::ptr::null_mut(),
    };
    unsafe {
        EnumWindows(
            Some(find_scribe_window),
            &mut search as *mut WindowSearch as LPARAM,
        );
    }
    if search.hwnd.is_null() || unsafe { IsWindow(search.hwnd) } == 0 {
        return;
    }

    let mut owner_process_id = 0;
    if unsafe { GetWindowThreadProcessId(search.hwnd, &mut owner_process_id) } == 0
        || owner_process_id != process_id
    {
        return;
    }

    let (message, wparam) = match wake {
        NativeWake::Restore => (WM_SYSCOMMAND, SC_RESTORE as usize),
        NativeWake::Message => (WM_NULL, 0),
        NativeWake::None => return,
    };
    unsafe {
        PostMessageW(search.hwnd, message, wparam, 0);
    }
}

#[cfg(not(target_os = "windows"))]
fn post_native_wake(_wake: NativeWake) {}

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
        assert_eq!(
            command_from_menu_id(&MenuId::new(MENU_SHOW)),
            Some(TrayCommand::Show)
        );
        assert_eq!(
            command_from_menu_id(&MenuId::new(MENU_HIDE)),
            Some(TrayCommand::Hide)
        );
        assert_eq!(
            command_from_menu_id(&MenuId::new(MENU_COPY_LAST)),
            Some(TrayCommand::CopyLastTranscript)
        );
        assert_eq!(
            command_from_menu_id(&MenuId::new(MENU_QUIT)),
            Some(TrayCommand::Quit)
        );
        assert_eq!(command_from_menu_id(&MenuId::new("unknown")), None);
    }

    #[test]
    fn tray_commands_select_the_expected_native_wake() {
        assert_eq!(
            native_wake_for_command(TrayCommand::Show),
            NativeWake::Restore
        );
        assert_eq!(native_wake_for_command(TrayCommand::Hide), NativeWake::None);
        assert_eq!(
            native_wake_for_command(TrayCommand::CopyLastTranscript),
            NativeWake::Message
        );
        assert_eq!(
            native_wake_for_command(TrayCommand::Quit),
            NativeWake::Restore
        );
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
    fn show_and_quit_menu_events_queue_commands_and_wake_the_app() {
        let (sender, receiver) = unbounded();
        let wake_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let wake_counter = wake_count.clone();
        let bridge = EventBridge {
            sender,
            native_wake: Arc::new(|_| {}),
            wake: Arc::new(move || {
                wake_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }),
        };

        bridge.send_menu_event(&MenuId::new(MENU_SHOW));
        bridge.send_menu_event(&MenuId::new(MENU_QUIT));
        bridge.send_menu_event(&MenuId::new("unknown"));

        assert_eq!(
            drain_commands(&receiver),
            vec![TrayCommand::Show, TrayCommand::Quit]
        );
        assert_eq!(wake_count.load(std::sync::atomic::Ordering::Relaxed), 2);
    }

    #[test]
    fn bridge_enqueues_before_native_wake_and_repaints_last() {
        let (sender, receiver) = unbounded();
        let native_receiver = receiver.clone();
        let order = Arc::new(std::sync::Mutex::new(Vec::new()));
        let native_order = order.clone();
        let repaint_order = order.clone();
        let bridge = EventBridge {
            sender,
            native_wake: Arc::new(move |wake| {
                assert_eq!(native_receiver.try_recv(), Ok(TrayCommand::Show));
                assert_eq!(wake, NativeWake::Restore);
                native_order
                    .lock()
                    .expect("native order lock")
                    .push("native");
            }),
            wake: Arc::new(move || {
                repaint_order
                    .lock()
                    .expect("repaint order lock")
                    .push("repaint");
            }),
        };

        bridge.send(TrayCommand::Show);

        assert_eq!(
            *order.lock().expect("final order lock"),
            vec!["native", "repaint"]
        );
    }
}
