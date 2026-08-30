use std::{
    sync::{
        Arc, OnceLock, RwLock,
        atomic::{AtomicU32, Ordering},
    },
    time::Instant,
};

use anyhow::{Result, anyhow};
use crossbeam_channel::{Receiver, Sender, unbounded};
use eframe::egui;
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

use crate::audio::control::{CaptureControlHandle, HotkeyDispatch};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HotkeyEvent {
    Pressed,
    Released,
}

#[derive(Clone)]
pub struct ObservedHotkeyEvent {
    pub event: HotkeyEvent,
    pub observed_at: Instant,
    pub direct_dispatch: HotkeyDispatch,
}

#[derive(Clone)]
struct QueuedGlobalHotkeyEvent {
    event: GlobalHotKeyEvent,
    observed_at: Instant,
    direct_dispatch: HotkeyDispatch,
}

pub struct HotkeyService {
    manager: Option<GlobalHotKeyManager>,
    hotkey: Option<HotKey>,
    event_rx: Receiver<QueuedGlobalHotkeyEvent>,
    registered_id: Arc<AtomicU32>,
    pub last_error: Option<String>,
}

/// `global-hotkey` owns one process-wide callback. Keep its callback small:
/// enqueue the event first, then wake both a visible egui viewport and a
/// hidden Windows viewport. The app still consumes events through
/// `poll_events`, which keeps recording state single-threaded.
struct EventHandlerState {
    receiver: Receiver<QueuedGlobalHotkeyEvent>,
    wake_context: Arc<RwLock<egui::Context>>,
    capture_control: Arc<RwLock<CaptureControlHandle>>,
    registered_id: Arc<AtomicU32>,
}

#[derive(Clone)]
struct EventBridge {
    sender: Sender<QueuedGlobalHotkeyEvent>,
    native_wake: Arc<dyn Fn() + Send + Sync>,
    repaint_wake: Arc<dyn Fn() + Send + Sync>,
    capture_control: Arc<RwLock<CaptureControlHandle>>,
    registered_id: Arc<AtomicU32>,
}

impl EventBridge {
    fn send(&self, event: GlobalHotKeyEvent) {
        let observed_at = Instant::now();
        let direct_dispatch = if self.registered_id.load(Ordering::Acquire) == event.id() {
            let pressed = event.state() == HotKeyState::Pressed;
            self.capture_control
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .dispatch_hotkey(pressed, observed_at)
        } else {
            HotkeyDispatch::None
        };
        if self
            .sender
            .send(QueuedGlobalHotkeyEvent {
                event,
                observed_at,
                direct_dispatch,
            })
            .is_err()
        {
            return;
        }
        (self.native_wake)();
        (self.repaint_wake)();
    }
}

impl HotkeyService {
    pub fn new(spec: &str, context: &egui::Context, capture_control: CaptureControlHandle) -> Self {
        let (event_rx, registered_id) = install_event_handler(context, capture_control);
        registered_id.store(0, Ordering::Release);
        let mut service = Self {
            manager: None,
            hotkey: None,
            event_rx,
            registered_id,
            last_error: None,
        };
        if let Err(err) = global_hotkey_startup_allowed() {
            service.last_error = Some(err.to_string());
            return service;
        }
        if let Err(err) = service.register(spec) {
            service.last_error = Some(err.to_string());
        }
        service
    }

    pub fn register(&mut self, spec: &str) -> Result<()> {
        global_hotkey_startup_allowed()?;
        let replacement = parse_hotkey(spec)?;
        if self.hotkey == Some(replacement) {
            self.last_error = None;
            return Ok(());
        }

        // Register the replacement through a separate manager before touching
        // the active binding. This keeps the old shortcut effective when the
        // OS rejects a conflicting replacement.
        let replacement_manager = GlobalHotKeyManager::new()?;
        let previous_manager = self.manager.as_ref();
        if let Err(error) = replace_hotkey_registration(
            self.hotkey,
            replacement,
            |candidate| replacement_manager.register(candidate).map_err(Into::into),
            |previous| match previous_manager {
                Some(manager) => manager.unregister(previous).map_err(Into::into),
                None => Ok(()),
            },
            |candidate| {
                replacement_manager
                    .unregister(candidate)
                    .map_err(Into::into)
            },
        ) {
            self.last_error = Some(error.to_string());
            return Err(error);
        }

        self.manager = Some(replacement_manager);
        self.hotkey = Some(replacement);
        self.registered_id
            .store(replacement.id(), Ordering::Release);
        self.last_error = None;
        Ok(())
    }

    pub fn poll_events(&self) -> Vec<ObservedHotkeyEvent> {
        let registered_id = self.hotkey.map(|hotkey| hotkey.id());
        let mut events = Vec::new();
        while let Ok(observed) = self.event_rx.try_recv() {
            if let Some(event) = event_from_global(observed.event, registered_id) {
                events.push(ObservedHotkeyEvent {
                    event,
                    observed_at: observed.observed_at,
                    direct_dispatch: observed.direct_dispatch,
                });
            }
        }
        events
    }
}

fn install_event_handler(
    context: &egui::Context,
    capture_control: CaptureControlHandle,
) -> (Receiver<QueuedGlobalHotkeyEvent>, Arc<AtomicU32>) {
    static STATE: OnceLock<EventHandlerState> = OnceLock::new();

    let state = STATE.get_or_init(|| {
        let (sender, receiver) = unbounded();
        let wake_context = Arc::new(RwLock::new(context.clone()));
        let capture_control = Arc::new(RwLock::new(capture_control.clone()));
        let registered_id = Arc::new(AtomicU32::new(0));
        let repaint_context = Arc::clone(&wake_context);
        let bridge = EventBridge {
            sender,
            native_wake: Arc::new(crate::tray::wake_scribe_app_from_background_event),
            repaint_wake: Arc::new(move || {
                let context = repaint_context
                    .read()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .clone();
                context.request_repaint();
            }),
            capture_control: Arc::clone(&capture_control),
            registered_id: Arc::clone(&registered_id),
        };
        GlobalHotKeyEvent::set_event_handler(Some(move |event| bridge.send(event)));
        EventHandlerState {
            receiver,
            wake_context,
            capture_control,
            registered_id,
        }
    });

    // The global handler cannot be replaced, but test harnesses and eframe
    // restarts can construct a new context in the same process.
    *state
        .wake_context
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = context.clone();
    *state
        .capture_control
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = capture_control;
    (state.receiver.clone(), Arc::clone(&state.registered_id))
}

fn replace_hotkey_registration(
    previous: Option<HotKey>,
    replacement: HotKey,
    register_replacement: impl FnOnce(HotKey) -> Result<()>,
    unregister_previous: impl FnOnce(HotKey) -> Result<()>,
    rollback_replacement: impl FnOnce(HotKey) -> Result<()>,
) -> Result<()> {
    register_replacement(replacement)?;
    if let Some(previous) = previous
        && let Err(error) = unregister_previous(previous)
    {
        return match rollback_replacement(replacement) {
            Ok(()) => Err(anyhow!(
                "could not retire the previous hotkey: {error}; the previous shortcut remains active"
            )),
            Err(rollback_error) => Err(anyhow!(
                "could not retire the previous hotkey: {error}; replacement cleanup also failed: {rollback_error}"
            )),
        };
    }
    Ok(())
}

fn event_from_global(event: GlobalHotKeyEvent, registered_id: Option<u32>) -> Option<HotkeyEvent> {
    if registered_id.is_some_and(|id| id != event.id()) {
        return None;
    }

    match event.state() {
        HotKeyState::Pressed => Some(HotkeyEvent::Pressed),
        HotKeyState::Released => Some(HotkeyEvent::Released),
    }
}

fn global_hotkey_startup_allowed() -> Result<()> {
    if std::env::var_os("SCRIBE_DISABLE_HOTKEY").is_some() {
        return Err(anyhow!("global hotkey disabled by SCRIBE_DISABLE_HOTKEY"));
    }

    #[cfg(target_os = "linux")]
    {
        if std::env::var_os("SCRIBE_ENABLE_GLOBAL_HOTKEY").is_none() {
            return Err(anyhow!(
                "global hotkey disabled on Linux; set SCRIBE_ENABLE_GLOBAL_HOTKEY=1 to enable it"
            ));
        }
    }

    Ok(())
}

pub fn parse_hotkey(spec: &str) -> Result<HotKey> {
    let mut modifiers = Modifiers::empty();
    let mut key = None;

    for part in spec.split('+') {
        let token = part.trim().to_ascii_lowercase();
        match token.as_str() {
            "ctrl" | "control" => modifiers |= Modifiers::CONTROL,
            "shift" => modifiers |= Modifiers::SHIFT,
            "alt" | "option" => modifiers |= Modifiers::ALT,
            "super" | "meta" | "cmd" | "command" | "win" => modifiers |= Modifiers::SUPER,
            "" => {}
            _ => key = Some(parse_key(&token)?),
        }
    }

    let key =
        key.ok_or_else(|| anyhow!("hotkey must include a key, for example Ctrl+Shift+Space"))?;
    Ok(HotKey::new(Some(modifiers), key))
}

/// Validate and serialize a shortcut in the stable persisted presentation
/// used by the capture control. Equivalent input such as `control + space`
/// therefore never creates a duplicate binding or formatting-only setting
/// change.
pub fn normalize_hotkey_spec(spec: &str) -> Result<String> {
    let mut ctrl = false;
    let mut shift = false;
    let mut alt = false;
    let mut super_key = false;
    let mut key = None;

    for part in spec.split('+') {
        let token = part.trim().to_ascii_lowercase();
        match token.as_str() {
            "ctrl" | "control" => ctrl = true,
            "shift" => shift = true,
            "alt" | "option" => alt = true,
            "super" | "meta" | "cmd" | "command" | "win" => super_key = true,
            "" => {}
            _ => {
                parse_key(&token)?;
                key = Some(match token.as_str() {
                    "return" => "Enter".to_owned(),
                    "escape" => "Esc".to_owned(),
                    "arrowup" => "Up".to_owned(),
                    "arrowdown" => "Down".to_owned(),
                    "arrowleft" => "Left".to_owned(),
                    "arrowright" => "Right".to_owned(),
                    _ if token.len() == 1 => token.to_ascii_uppercase(),
                    _ => {
                        let mut chars = token.chars();
                        let first = chars.next().expect("validated nonempty key");
                        first.to_uppercase().collect::<String>() + chars.as_str()
                    }
                });
            }
        }
    }

    let key =
        key.ok_or_else(|| anyhow!("hotkey must include a key, for example Ctrl+Shift+Space"))?;
    let mut parts = Vec::new();
    if ctrl {
        parts.push("Ctrl".to_owned());
    }
    if shift {
        parts.push("Shift".to_owned());
    }
    if alt {
        parts.push("Alt".to_owned());
    }
    if super_key {
        parts.push("Super".to_owned());
    }
    parts.push(key);
    Ok(parts.join("+"))
}

fn parse_key(token: &str) -> Result<Code> {
    match token {
        "space" => Ok(Code::Space),
        "enter" | "return" => Ok(Code::Enter),
        "tab" => Ok(Code::Tab),
        "escape" | "esc" => Ok(Code::Escape),
        "backspace" => Ok(Code::Backspace),
        "delete" => Ok(Code::Delete),
        "up" | "arrowup" => Ok(Code::ArrowUp),
        "down" | "arrowdown" => Ok(Code::ArrowDown),
        "left" | "arrowleft" => Ok(Code::ArrowLeft),
        "right" | "arrowright" => Ok(Code::ArrowRight),
        "f1" => Ok(Code::F1),
        "f2" => Ok(Code::F2),
        "f3" => Ok(Code::F3),
        "f4" => Ok(Code::F4),
        "f5" => Ok(Code::F5),
        "f6" => Ok(Code::F6),
        "f7" => Ok(Code::F7),
        "f8" => Ok(Code::F8),
        "f9" => Ok(Code::F9),
        "f10" => Ok(Code::F10),
        "f11" => Ok(Code::F11),
        "f12" => Ok(Code::F12),
        _ if token.len() == 1 => {
            let ch = token.chars().next().unwrap();
            match ch {
                'a'..='z' => Ok(letter_code(ch)),
                '0' => Ok(Code::Digit0),
                '1' => Ok(Code::Digit1),
                '2' => Ok(Code::Digit2),
                '3' => Ok(Code::Digit3),
                '4' => Ok(Code::Digit4),
                '5' => Ok(Code::Digit5),
                '6' => Ok(Code::Digit6),
                '7' => Ok(Code::Digit7),
                '8' => Ok(Code::Digit8),
                '9' => Ok(Code::Digit9),
                _ => Err(anyhow!("unsupported hotkey key: {token}")),
            }
        }
        _ => Err(anyhow!("unsupported hotkey key: {token}")),
    }
}

fn letter_code(ch: char) -> Code {
    match ch {
        'a' => Code::KeyA,
        'b' => Code::KeyB,
        'c' => Code::KeyC,
        'd' => Code::KeyD,
        'e' => Code::KeyE,
        'f' => Code::KeyF,
        'g' => Code::KeyG,
        'h' => Code::KeyH,
        'i' => Code::KeyI,
        'j' => Code::KeyJ,
        'k' => Code::KeyK,
        'l' => Code::KeyL,
        'm' => Code::KeyM,
        'n' => Code::KeyN,
        'o' => Code::KeyO,
        'p' => Code::KeyP,
        'q' => Code::KeyQ,
        'r' => Code::KeyR,
        's' => Code::KeyS,
        't' => Code::KeyT,
        'u' => Code::KeyU,
        'v' => Code::KeyV,
        'w' => Code::KeyW,
        'x' => Code::KeyX,
        'y' => Code::KeyY,
        'z' => Code::KeyZ,
        _ => Code::Space,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use super::*;

    #[test]
    fn parses_common_modifier_combo() {
        assert!(parse_hotkey("Ctrl+Shift+Space").is_ok());
        assert!(parse_hotkey("command+alt+k").is_ok());
    }

    #[test]
    fn rejects_missing_or_unknown_key() {
        assert!(parse_hotkey("Ctrl+Shift").is_err());
        assert!(parse_hotkey("Ctrl+Mouse1").is_err());
    }

    #[test]
    fn normalizes_equivalent_shortcuts_to_a_single_persisted_format() {
        assert_eq!(
            normalize_hotkey_spec("control + option + k").unwrap(),
            "Ctrl+Alt+K"
        );
        assert_eq!(normalize_hotkey_spec("Ctrl+Escape").unwrap(), "Ctrl+Esc");
    }

    #[test]
    fn failed_replacement_registration_never_retires_the_previous_hotkey() {
        let previous = parse_hotkey("Ctrl+Shift+Space").unwrap();
        let replacement = parse_hotkey("Ctrl+Alt+K").unwrap();
        let calls = std::cell::RefCell::new(Vec::new());

        let error = replace_hotkey_registration(
            Some(previous),
            replacement,
            |_| {
                calls.borrow_mut().push("register replacement");
                Err(anyhow!("shortcut is already in use"))
            },
            |_| {
                calls.borrow_mut().push("unregister previous");
                Ok(())
            },
            |_| {
                calls.borrow_mut().push("rollback replacement");
                Ok(())
            },
        )
        .unwrap_err();

        assert!(error.to_string().contains("already in use"));
        assert_eq!(*calls.borrow(), ["register replacement"]);
    }

    #[test]
    fn failed_previous_retirement_rolls_back_the_registered_replacement() {
        let previous = parse_hotkey("Ctrl+Shift+Space").unwrap();
        let replacement = parse_hotkey("Ctrl+Alt+K").unwrap();
        let calls = std::cell::RefCell::new(Vec::new());

        let error = replace_hotkey_registration(
            Some(previous),
            replacement,
            |_| {
                calls.borrow_mut().push("register replacement");
                Ok(())
            },
            |_| {
                calls.borrow_mut().push("unregister previous");
                Err(anyhow!("previous manager is unavailable"))
            },
            |_| {
                calls.borrow_mut().push("rollback replacement");
                Ok(())
            },
        )
        .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("previous shortcut remains active")
        );
        assert_eq!(
            *calls.borrow(),
            [
                "register replacement",
                "unregister previous",
                "rollback replacement"
            ]
        );
    }

    #[test]
    fn maps_registered_global_hotkey_events_to_press_and_release() {
        let hotkey = parse_hotkey("Ctrl+Shift+Space").unwrap();

        assert_eq!(
            event_from_global(
                GlobalHotKeyEvent {
                    id: hotkey.id(),
                    state: HotKeyState::Pressed,
                },
                Some(hotkey.id())
            ),
            Some(HotkeyEvent::Pressed)
        );
        assert_eq!(
            event_from_global(
                GlobalHotKeyEvent {
                    id: hotkey.id(),
                    state: HotKeyState::Released,
                },
                Some(hotkey.id())
            ),
            Some(HotkeyEvent::Released)
        );
    }

    #[test]
    fn ignores_events_for_other_hotkeys() {
        let hotkey = parse_hotkey("Ctrl+Shift+Space").unwrap();
        let other = parse_hotkey("Ctrl+Alt+K").unwrap();

        assert_eq!(
            event_from_global(
                GlobalHotKeyEvent {
                    id: other.id(),
                    state: HotKeyState::Pressed,
                },
                Some(hotkey.id())
            ),
            None
        );
    }

    #[test]
    fn drains_press_and_release_from_the_app_owned_event_channel() {
        let hotkey = parse_hotkey("Ctrl+Shift+Space").unwrap();
        let (sender, receiver) = unbounded();
        let service = HotkeyService {
            manager: None,
            hotkey: Some(hotkey),
            event_rx: receiver,
            registered_id: Arc::new(AtomicU32::new(hotkey.id())),
            last_error: None,
        };

        sender
            .send(QueuedGlobalHotkeyEvent {
                event: GlobalHotKeyEvent {
                    id: hotkey.id(),
                    state: HotKeyState::Pressed,
                },
                observed_at: Instant::now(),
                direct_dispatch: HotkeyDispatch::None,
            })
            .unwrap();
        sender
            .send(QueuedGlobalHotkeyEvent {
                event: GlobalHotKeyEvent {
                    id: hotkey.id(),
                    state: HotKeyState::Released,
                },
                observed_at: Instant::now(),
                direct_dispatch: HotkeyDispatch::None,
            })
            .unwrap();

        assert_eq!(
            service
                .poll_events()
                .into_iter()
                .map(|event| event.event)
                .collect::<Vec<_>>(),
            vec![HotkeyEvent::Pressed, HotkeyEvent::Released]
        );
    }

    #[test]
    fn event_bridge_dispatches_capture_before_enqueuing_and_waking() {
        let (sender, receiver) = unbounded();
        let native_wakes = Arc::new(AtomicUsize::new(0));
        let repaint_wakes = Arc::new(AtomicUsize::new(0));
        let native_wake_count = Arc::clone(&native_wakes);
        let repaint_wake_count = Arc::clone(&repaint_wakes);
        let controller = crate::audio::control::CaptureController::with_start_capture_for_test(
            Arc::new(|_request, cancellation| {
                while !cancellation.is_cancelled() {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                Err(crate::audio::CaptureError::StartupCancelled)
            }),
        )
        .unwrap();
        controller
            .handle()
            .reconfigure_hotkey(
                true,
                crate::audio::control::CaptureHotkeyMode::HoldToTalk,
                30,
                None,
                crate::audio::CaptureOptions::default(),
            )
            .unwrap();
        let hotkey = parse_hotkey("Ctrl+Shift+Space").unwrap();
        let registered_id = Arc::new(AtomicU32::new(hotkey.id()));
        let bridge = EventBridge {
            sender,
            native_wake: Arc::new(move || {
                native_wake_count.fetch_add(1, Ordering::SeqCst);
            }),
            repaint_wake: Arc::new(move || {
                repaint_wake_count.fetch_add(1, Ordering::SeqCst);
            }),
            capture_control: Arc::new(RwLock::new(controller.handle())),
            registered_id,
        };
        bridge.send(GlobalHotKeyEvent {
            id: hotkey.id(),
            state: HotKeyState::Pressed,
        });

        let queued = receiver.recv().unwrap();
        assert!(matches!(queued.direct_dispatch, HotkeyDispatch::Start(_)));
        assert_eq!(
            controller.handle().owner(),
            Some(crate::audio::control::AudioOwnerKind::Capture)
        );
        assert_eq!(queued.event.id(), hotkey.id());
        assert_eq!(queued.event.state(), HotKeyState::Pressed);
        assert_eq!(native_wakes.load(Ordering::SeqCst), 1);
        assert_eq!(repaint_wakes.load(Ordering::SeqCst), 1);
    }
}
