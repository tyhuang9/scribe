use anyhow::{Context, Result, anyhow};
use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};
use std::sync::{Arc, Mutex, Once};
use std::time::Duration;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

const MENU_SHOW: &str = "scribe-show";
const MENU_HIDE: &str = "scribe-hide";
const MENU_TOGGLE_RECORDING: &str = "scribe-toggle-recording";
const MENU_COPY_LAST: &str = "scribe-copy-last";
const MENU_QUIT: &str = "scribe-quit";
const COMMAND_QUEUE_CAPACITY: usize = 32;
#[cfg(target_os = "windows")]
const HIDDEN_REPAINT_TIMER_ID: usize = 0x5343_5242;

type WakeEventLoop = Arc<dyn Fn() + Send + Sync + 'static>;

static EVENT_HANDLERS: Once = Once::new();
static EVENT_TARGET: Mutex<Option<Arc<TrayCommandPublisher>>> = Mutex::new(None);

struct TrayCommandPublisher {
    sender: Sender<TrayCommand>,
    discard_receiver: Receiver<TrayCommand>,
    publish_lock: Mutex<()>,
    wake_event_loop: WakeEventLoop,
    main_window_handle: Option<isize>,
}

impl TrayCommandPublisher {
    fn publish(&self, command: TrayCommand) {
        let _publish_guard = self
            .publish_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Err(TrySendError::Full(command)) = self.sender.try_send(command) {
            // Human-generated tray input should never saturate this queue. If it does,
            // retain the newest intent instead of leaving the app stuck on an old one.
            let _ = self.discard_receiver.try_recv();
            let _ = self.sender.try_send(command);
        }
        (self.wake_event_loop)();
        wake_hidden_main_window(self.main_window_handle);
    }
}

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
    command_receiver: Receiver<TrayCommand>,
    command_publisher: Option<Arc<TrayCommandPublisher>>,
    main_window_handle: Option<isize>,
    toggle_recording_item: MenuItem,
    copy_last_item: MenuItem,
    _show_item: MenuItem,
    _hide_item: MenuItem,
    _quit_item: MenuItem,
    _separator: PredefinedMenuItem,
}

impl TrayService {
    pub fn new(
        is_recording: bool,
        has_transcript: bool,
        main_window_handle: Option<isize>,
        wake_event_loop: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self> {
        if std::env::var_os("SCRIBE_DISABLE_TRAY").is_some() {
            return Err(anyhow!("system tray disabled by SCRIBE_DISABLE_TRAY"));
        }
        if running_in_wsl() && std::env::var_os("SCRIBE_ENABLE_TRAY").is_none() {
            return Err(anyhow!(
                "system tray disabled by default under WSL; set SCRIBE_ENABLE_TRAY=1 to enable it"
            ));
        }
        ensure_tray_runtime_available()?;
        install_event_handlers();
        let (mut service, command_sender) = catch_tray_init_panic(|| {
            Self::build(is_recording, has_transcript, main_window_handle)
        })??;
        let command_publisher = Arc::new(TrayCommandPublisher {
            sender: command_sender,
            discard_receiver: service.command_receiver.clone(),
            publish_lock: Mutex::new(()),
            wake_event_loop: Arc::new(wake_event_loop),
            main_window_handle: service.main_window_handle,
        });
        service.command_publisher = Some(Arc::clone(&command_publisher));
        *EVENT_TARGET
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(command_publisher);
        Ok(service)
    }

    fn build(
        is_recording: bool,
        has_transcript: bool,
        main_window_handle: Option<isize>,
    ) -> Result<(Self, Sender<TrayCommand>)> {
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

        let (command_sender, command_receiver) = bounded(COMMAND_QUEUE_CAPACITY);

        Ok((
            Self {
                _tray_icon: tray_icon,
                command_receiver,
                command_publisher: None,
                main_window_handle: valid_main_window_handle(main_window_handle),
                toggle_recording_item,
                copy_last_item,
                _show_item: show_item,
                _hide_item: hide_item,
                _quit_item: quit_item,
                _separator: separator,
            },
            command_sender,
        ))
    }

    pub fn set_recording(&self, is_recording: bool) {
        self.toggle_recording_item
            .set_text(recording_label(is_recording));
    }

    pub fn set_has_transcript(&self, has_transcript: bool) {
        self.copy_last_item.set_enabled(has_transcript);
    }

    pub fn poll_command(&self) -> Option<TrayCommand> {
        self.command_receiver.try_recv().ok()
    }

    pub fn schedule_hidden_repaint(&self, delay: Duration) -> Result<bool> {
        #[cfg(target_os = "windows")]
        {
            schedule_hidden_main_window_repaint(self.main_window_handle, delay)?;
            Ok(true)
        }

        #[cfg(not(target_os = "windows"))]
        {
            Ok(false)
        }
    }

    pub fn cancel_hidden_repaint(&self) {
        #[cfg(target_os = "windows")]
        {
            cancel_hidden_main_window_repaint(self.main_window_handle);
        }
    }
}

impl Drop for TrayService {
    fn drop(&mut self) {
        self.cancel_hidden_repaint();
        let mut target = EVENT_TARGET
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.command_publisher.as_ref().is_some_and(|owned| {
            target
                .as_ref()
                .is_some_and(|publisher| Arc::ptr_eq(publisher, owned))
        }) {
            *target = None;
        }
    }
}

fn install_event_handlers() {
    EVENT_HANDLERS.call_once(|| {
        MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
            if let Some(command) = command_from_menu_id(event.id().as_ref()) {
                publish_event(command);
            }
        }));

        TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
            if tray_event_should_show(&event) {
                publish_event(TrayCommand::Show);
            }
        }));
    });
}

fn publish_event(command: TrayCommand) {
    let publisher = EVENT_TARGET
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    if let Some(publisher) = publisher {
        publisher.publish(command);
    }
}

#[cfg(target_os = "windows")]
fn valid_main_window_handle(handle: Option<isize>) -> Option<isize> {
    use windows_sys::Win32::UI::WindowsAndMessaging::IsWindow;

    let handle = handle.filter(|handle| *handle != 0)?;
    let window = handle as windows_sys::Win32::Foundation::HWND;
    (unsafe { IsWindow(window) } != 0).then_some(handle)
}

#[cfg(not(target_os = "windows"))]
fn valid_main_window_handle(_handle: Option<isize>) -> Option<isize> {
    None
}

#[cfg(target_os = "windows")]
fn wake_hidden_main_window(handle: Option<isize>) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_PAINT};

    let Some(handle) = handle else {
        return;
    };
    unsafe {
        PostMessageW(
            handle as windows_sys::Win32::Foundation::HWND,
            WM_PAINT,
            0,
            0,
        );
    }
}

#[cfg(not(target_os = "windows"))]
fn wake_hidden_main_window(_handle: Option<isize>) {}

#[cfg(target_os = "windows")]
fn schedule_hidden_main_window_repaint(handle: Option<isize>, delay: Duration) -> Result<()> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{IsWindow, SetTimer};

    let handle = handle.ok_or_else(|| anyhow!("the native Scribe window handle is unavailable"))?;
    let window = handle as windows_sys::Win32::Foundation::HWND;
    if unsafe { IsWindow(window) } == 0 {
        return Err(anyhow!("the native Scribe window no longer exists"));
    }
    let timer = unsafe {
        SetTimer(
            window,
            HIDDEN_REPAINT_TIMER_ID,
            hidden_repaint_delay_millis(delay),
            Some(hidden_repaint_timer),
        )
    };
    if timer == 0 {
        return Err(anyhow!(
            "Windows could not schedule hidden tray processing: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn cancel_hidden_main_window_repaint(handle: Option<isize>) {
    use windows_sys::Win32::UI::WindowsAndMessaging::KillTimer;

    if let Some(handle) = handle {
        unsafe {
            KillTimer(
                handle as windows_sys::Win32::Foundation::HWND,
                HIDDEN_REPAINT_TIMER_ID,
            );
        }
    }
}

#[cfg(target_os = "windows")]
unsafe extern "system" fn hidden_repaint_timer(
    window: windows_sys::Win32::Foundation::HWND,
    _message: u32,
    timer_id: usize,
    _elapsed: u32,
) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{KillTimer, PostMessageW, WM_PAINT};

    unsafe {
        KillTimer(window, timer_id);
        PostMessageW(window, WM_PAINT, 0, 0);
    }
}

#[cfg(target_os = "windows")]
fn hidden_repaint_delay_millis(delay: Duration) -> u32 {
    delay.as_millis().clamp(10, u32::MAX as u128) as u32
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
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn publisher(
        capacity: usize,
        wake_count: Arc<AtomicUsize>,
    ) -> (TrayCommandPublisher, Receiver<TrayCommand>) {
        let (sender, receiver) = bounded(capacity);
        let discard_receiver = receiver.clone();
        (
            TrayCommandPublisher {
                sender,
                discard_receiver,
                publish_lock: Mutex::new(()),
                wake_event_loop: Arc::new(move || {
                    wake_count.fetch_add(1, Ordering::Relaxed);
                }),
                main_window_handle: None,
            },
            receiver,
        )
    }

    #[test]
    fn enqueued_command_wakes_event_loop_and_is_received() {
        let wake_count = Arc::new(AtomicUsize::new(0));
        let (publisher, receiver) = publisher(1, Arc::clone(&wake_count));

        publisher.publish(TrayCommand::Show);

        assert_eq!(receiver.try_recv(), Ok(TrayCommand::Show));
        assert_eq!(wake_count.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn full_command_queue_keeps_newest_intent_and_wakes_event_loop() {
        let wake_count = Arc::new(AtomicUsize::new(0));
        let (publisher, receiver) = publisher(1, Arc::clone(&wake_count));
        publisher.publish(TrayCommand::Hide);
        publisher.publish(TrayCommand::Quit);

        assert_eq!(receiver.try_recv(), Ok(TrayCommand::Quit));
        assert_eq!(wake_count.load(Ordering::Relaxed), 2);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn hidden_repaint_timer_delay_is_bounded_for_win32() {
        assert_eq!(hidden_repaint_delay_millis(Duration::ZERO), 10);
        assert_eq!(hidden_repaint_delay_millis(Duration::from_millis(40)), 40);
        assert_eq!(hidden_repaint_delay_millis(Duration::from_millis(500)), 500);
        assert_eq!(hidden_repaint_delay_millis(Duration::MAX), u32::MAX);
    }

    #[test]
    fn menu_ids_map_to_runtime_neutral_commands() {
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
}
