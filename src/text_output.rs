use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use arboard::Clipboard;
use enigo::{
    Direction::{Click, Press, Release},
    Enigo, Key, Keyboard, Settings,
};

use crate::config::AppConfig;

#[derive(Clone, Copy, Debug)]
pub struct TextOutputOptions {
    pub restore_clipboard_after_insert: bool,
    pub paste_delay_ms: u64,
}

impl TextOutputOptions {
    pub fn from_config(config: &AppConfig) -> Self {
        Self {
            restore_clipboard_after_insert: config.output.restore_clipboard_after_insert,
            paste_delay_ms: config.output.paste_delay_ms,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TextOutputResult {
    Inserted,
    CopiedOnly,
    Failed(String),
}

impl TextOutputResult {
    pub fn status_message(&self) -> String {
        match self {
            Self::Inserted => "Transcript inserted into the focused app".to_owned(),
            Self::CopiedOnly => {
                "Transcript copied to clipboard; paste automation was unavailable".to_owned()
            }
            Self::Failed(message) => format!("Transcript output failed: {message}"),
        }
    }
}

pub fn write_to_focused_app(text: &str, config: &AppConfig) -> TextOutputResult {
    let mut clipboard = match SystemClipboard::new() {
        Ok(clipboard) => clipboard,
        Err(err) => return TextOutputResult::Failed(err.to_string()),
    };
    let mut paste = EnigoPasteDriver;

    write_text_with_drivers(
        &mut clipboard,
        &mut paste,
        text,
        TextOutputOptions::from_config(config),
        paste_automation_available(),
    )
}

pub fn paste_automation_notice() -> Option<&'static str> {
    if paste_automation_available() {
        None
    } else {
        Some(
            "Wayland desktop detected: automatic paste may be blocked, so transcripts will fall back to the clipboard.",
        )
    }
}

pub trait ClipboardDriver {
    fn get_text(&mut self) -> Result<String>;
    fn set_text(&mut self, text: String) -> Result<()>;
}

pub trait PasteDriver {
    fn paste(&mut self) -> Result<()>;
}

pub fn write_text_with_drivers<C, P>(
    clipboard: &mut C,
    paste: &mut P,
    text: &str,
    options: TextOutputOptions,
    paste_available: bool,
) -> TextOutputResult
where
    C: ClipboardDriver,
    P: PasteDriver,
{
    if text.trim().is_empty() {
        return TextOutputResult::Failed("transcript is empty".to_owned());
    }

    let previous_clipboard = if options.restore_clipboard_after_insert {
        clipboard.get_text().ok()
    } else {
        None
    };

    if let Err(err) = clipboard.set_text(text.to_owned()) {
        return TextOutputResult::Failed(err.to_string());
    }

    if !paste_available {
        return TextOutputResult::CopiedOnly;
    }

    sleep_for_paste_delay(options.paste_delay_ms);
    if paste.paste().is_err() {
        return TextOutputResult::CopiedOnly;
    }

    sleep_for_paste_delay(options.paste_delay_ms);
    if let Some(previous_clipboard) = previous_clipboard {
        let _ = clipboard.set_text(previous_clipboard);
    }

    TextOutputResult::Inserted
}

fn sleep_for_paste_delay(delay_ms: u64) {
    if delay_ms > 0 {
        thread::sleep(Duration::from_millis(delay_ms));
    }
}

fn paste_automation_available() -> bool {
    #[cfg(target_os = "linux")]
    {
        std::env::var_os("WAYLAND_DISPLAY").is_none()
    }

    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

struct SystemClipboard {
    clipboard: Clipboard,
}

impl SystemClipboard {
    fn new() -> Result<Self> {
        Ok(Self {
            clipboard: Clipboard::new().context("failed to open system clipboard")?,
        })
    }
}

impl ClipboardDriver for SystemClipboard {
    fn get_text(&mut self) -> Result<String> {
        self.clipboard
            .get_text()
            .context("failed to read clipboard text")
    }

    fn set_text(&mut self, text: String) -> Result<()> {
        self.clipboard
            .set_text(text)
            .context("failed to set clipboard text")
    }
}

struct EnigoPasteDriver;

impl PasteDriver for EnigoPasteDriver {
    fn paste(&mut self) -> Result<()> {
        let mut enigo = Enigo::new(&Settings::default())
            .map_err(|err| anyhow!("failed to initialize keyboard automation: {err}"))?;
        let modifier = paste_modifier_key();

        enigo
            .key(modifier, Press)
            .map_err(|err| anyhow!("failed to press paste modifier: {err}"))?;
        let paste_result = enigo
            .key(Key::Unicode('v'), Click)
            .map_err(|err| anyhow!("failed to send paste key: {err}"));
        let release_result = enigo
            .key(modifier, Release)
            .map_err(|err| anyhow!("failed to release paste modifier: {err}"));

        paste_result?;
        release_result?;
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn paste_modifier_key() -> Key {
    Key::Meta
}

#[cfg(not(target_os = "macos"))]
fn paste_modifier_key() -> Key {
    Key::Control
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FakeClipboard {
        text: Option<String>,
        fail_get: bool,
        fail_set: bool,
        set_history: Vec<String>,
    }

    impl ClipboardDriver for FakeClipboard {
        fn get_text(&mut self) -> Result<String> {
            if self.fail_get {
                Err(anyhow!("get failed"))
            } else {
                self.text
                    .clone()
                    .ok_or_else(|| anyhow!("no clipboard text"))
            }
        }

        fn set_text(&mut self, text: String) -> Result<()> {
            if self.fail_set {
                Err(anyhow!("set failed"))
            } else {
                self.text = Some(text.clone());
                self.set_history.push(text);
                Ok(())
            }
        }
    }

    #[derive(Default)]
    struct FakePaste {
        calls: usize,
        fail: bool,
    }

    impl PasteDriver for FakePaste {
        fn paste(&mut self) -> Result<()> {
            self.calls += 1;
            if self.fail {
                Err(anyhow!("paste failed"))
            } else {
                Ok(())
            }
        }
    }

    fn fast_options() -> TextOutputOptions {
        TextOutputOptions {
            restore_clipboard_after_insert: true,
            paste_delay_ms: 0,
        }
    }

    #[test]
    fn successful_paste_restores_previous_clipboard() {
        let mut clipboard = FakeClipboard {
            text: Some("before".to_owned()),
            ..FakeClipboard::default()
        };
        let mut paste = FakePaste::default();

        let result =
            write_text_with_drivers(&mut clipboard, &mut paste, "hello", fast_options(), true);

        assert_eq!(result, TextOutputResult::Inserted);
        assert_eq!(paste.calls, 1);
        assert_eq!(clipboard.text.as_deref(), Some("before"));
        assert_eq!(clipboard.set_history, vec!["hello", "before"]);
    }

    #[test]
    fn restore_failure_does_not_fail_insert() {
        let mut clipboard = FakeClipboard {
            fail_get: true,
            ..FakeClipboard::default()
        };
        let mut paste = FakePaste::default();

        let result =
            write_text_with_drivers(&mut clipboard, &mut paste, "hello", fast_options(), true);

        assert_eq!(result, TextOutputResult::Inserted);
        assert_eq!(clipboard.text.as_deref(), Some("hello"));
        assert_eq!(paste.calls, 1);
    }

    #[test]
    fn paste_failure_leaves_transcript_on_clipboard() {
        let mut clipboard = FakeClipboard {
            text: Some("before".to_owned()),
            ..FakeClipboard::default()
        };
        let mut paste = FakePaste {
            fail: true,
            ..FakePaste::default()
        };

        let result =
            write_text_with_drivers(&mut clipboard, &mut paste, "hello", fast_options(), true);

        assert_eq!(result, TextOutputResult::CopiedOnly);
        assert_eq!(clipboard.text.as_deref(), Some("hello"));
        assert_eq!(paste.calls, 1);
    }

    #[test]
    fn unavailable_paste_leaves_transcript_on_clipboard() {
        let mut clipboard = FakeClipboard {
            text: Some("before".to_owned()),
            ..FakeClipboard::default()
        };
        let mut paste = FakePaste::default();

        let result =
            write_text_with_drivers(&mut clipboard, &mut paste, "hello", fast_options(), false);

        assert_eq!(result, TextOutputResult::CopiedOnly);
        assert_eq!(clipboard.text.as_deref(), Some("hello"));
        assert_eq!(paste.calls, 0);
    }
}
