use std::time::Instant;

use anyhow::{Result, anyhow};
use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HotkeyEvent {
    Pressed,
    Released,
}

#[derive(Clone, Copy, Debug)]
pub struct ObservedHotkeyEvent {
    pub event: HotkeyEvent,
    pub observed_at: Instant,
}

pub struct HotkeyService {
    manager: Option<GlobalHotKeyManager>,
    hotkey: Option<HotKey>,
    pub last_error: Option<String>,
}

impl HotkeyService {
    pub fn new(spec: &str) -> Self {
        let mut service = Self {
            manager: None,
            hotkey: None,
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
        self.hotkey = None;
        self.manager = None;
        global_hotkey_startup_allowed()?;

        let manager = GlobalHotKeyManager::new()?;
        let hotkey = parse_hotkey(spec)?;
        manager.register(hotkey)?;

        self.manager = Some(manager);
        self.hotkey = Some(hotkey);
        self.last_error = None;
        Ok(())
    }

    pub fn poll_events(&self) -> Vec<ObservedHotkeyEvent> {
        let registered_id = self.hotkey.map(|hotkey| hotkey.id());
        let mut events = Vec::new();
        while let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
            if let Some(event) = event_from_global(event, registered_id) {
                events.push(ObservedHotkeyEvent {
                    event,
                    observed_at: Instant::now(),
                });
            }
        }
        events
    }
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
}
