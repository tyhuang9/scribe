use anyhow::{Result, anyhow};
use crossbeam_channel::{Receiver, unbounded};
use eframe::egui;
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use std::sync::{Arc, OnceLock, RwLock};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HotkeyEvent {
    Pressed,
    Released,
}

pub struct HotkeyService {
    manager: Option<GlobalHotKeyManager>,
    hotkey: Option<HotKey>,
    event_rx: Receiver<GlobalHotKeyEvent>,
    pub last_error: Option<String>,
}

struct EventHandlerState {
    receiver: Receiver<GlobalHotKeyEvent>,
    wake_ctx: Arc<RwLock<egui::Context>>,
}

impl HotkeyService {
    pub fn new(spec: &str, ctx: &egui::Context) -> Self {
        let mut service = Self {
            manager: None,
            hotkey: None,
            event_rx: install_event_handler(ctx),
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
        let result: Result<()> = (|| {
            global_hotkey_startup_allowed()?;
            let hotkey = parse_hotkey(spec)?;

            // Keep the existing registration until the replacement is known to work.
            // A rejected system shortcut must not leave the user without recording control.
            if self.manager.is_some() && self.hotkey == Some(hotkey) {
                return Ok(());
            }

            let manager = GlobalHotKeyManager::new()?;
            manager.register(hotkey)?;

            self.manager = Some(manager);
            self.hotkey = Some(hotkey);
            Ok(())
        })();

        self.last_error = result.as_ref().err().map(|error| error.to_string());
        result
    }

    pub fn poll_events(&self) -> Vec<HotkeyEvent> {
        let registered_id = self.hotkey.map(|hotkey| hotkey.id());
        let mut events = Vec::new();
        while let Ok(event) = self.event_rx.try_recv() {
            if let Some(event) = event_from_global(event, registered_id) {
                events.push(event);
            }
        }
        events
    }
}

fn install_event_handler(ctx: &egui::Context) -> Receiver<GlobalHotKeyEvent> {
    static STATE: OnceLock<EventHandlerState> = OnceLock::new();

    let state = STATE.get_or_init(|| {
        let (sender, receiver) = unbounded();
        let wake_ctx = Arc::new(RwLock::new(ctx.clone()));
        let wake_context = wake_ctx.clone();
        GlobalHotKeyEvent::set_event_handler(Some(move |event| {
            let _ = sender.send(event);
            let ctx = wake_context
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone();
            ctx.request_repaint();
        }));
        EventHandlerState { receiver, wake_ctx }
    });

    // global-hotkey handlers are process-wide and cannot be replaced. Refresh
    // the egui context when an app instance is reconstructed in this process.
    *state
        .wake_ctx
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = ctx.clone();
    state.receiver.clone()
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
    let canonical = canonical_hotkey_spec(spec);
    canonical
        .parse::<HotKey>()
        .map_err(|error| anyhow!("invalid hotkey {spec:?}: {error}"))
}

fn canonical_hotkey_spec(spec: &str) -> String {
    spec.split('+')
        .map(|part| match part.trim().to_ascii_lowercase().as_str() {
            "ctrl" | "control" => "Control",
            "alt" | "option" => "Alt",
            "super" | "meta" | "cmd" | "command" | "win" => "Super",
            "esc" | "escape" => "Escape",
            "return" | "enter" => "Enter",
            _ => part.trim(),
        })
        .collect::<Vec<_>>()
        .join("+")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_modifier_combo() {
        assert!(parse_hotkey("Ctrl+Shift+Space").is_ok());
        assert!(parse_hotkey("command+alt+k").is_ok());
    }

    #[test]
    fn parses_extended_standard_keys_and_legacy_aliases() {
        for spec in [
            "Win+Alt+F24",
            "Ctrl+Shift+Home",
            "Ctrl+Alt+Backquote",
            "Ctrl+Alt+Comma",
            "Ctrl+Alt+Numpad1",
        ] {
            assert!(parse_hotkey(spec).is_ok(), "{spec}");
        }
    }

    #[test]
    fn rejects_missing_or_unknown_key() {
        assert!(parse_hotkey("Ctrl+Shift").is_err());
        assert!(parse_hotkey("Ctrl+Mouse1").is_err());
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
    fn drains_every_queued_event_for_the_registered_hotkey() {
        let hotkey = parse_hotkey("Ctrl+Shift+Space").unwrap();
        let (sender, receiver) = unbounded();
        let service = HotkeyService {
            manager: None,
            hotkey: Some(hotkey),
            event_rx: receiver,
            last_error: None,
        };

        sender
            .send(GlobalHotKeyEvent {
                id: hotkey.id(),
                state: HotKeyState::Pressed,
            })
            .unwrap();
        sender
            .send(GlobalHotKeyEvent {
                id: hotkey.id(),
                state: HotKeyState::Released,
            })
            .unwrap();

        assert_eq!(
            service.poll_events(),
            vec![HotkeyEvent::Pressed, HotkeyEvent::Released]
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
    fn rejected_replacement_preserves_existing_hotkey() {
        let existing = parse_hotkey("Ctrl+Shift+Space").unwrap();
        let mut service = HotkeyService {
            manager: None,
            hotkey: Some(existing),
            event_rx: unbounded().1,
            last_error: None,
        };

        assert!(service.register("Ctrl+Mouse1").is_err());
        assert_eq!(service.hotkey, Some(existing));
        assert!(service.manager.is_none());
        assert!(service.last_error.is_some());
    }
}
