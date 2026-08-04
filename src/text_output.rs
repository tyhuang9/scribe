use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use arboard::Clipboard;
use enigo::{
    Direction::{Click, Press, Release},
    Enigo, Key, Keyboard, Settings,
};

use crate::config::AppConfig;
use crate::overlay::platform::{CapturedTarget, captured_target_is_foreground};

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
                "Transcript copied to clipboard; the target or paste automation was unavailable"
                    .to_owned()
            }
            Self::Failed(message) => format!("Transcript output failed: {message}"),
        }
    }
}

#[allow(dead_code)] // Compatibility API; target-aware application code uses write_to_captured_target.
pub fn write_to_focused_app(text: &str, config: &AppConfig) -> TextOutputResult {
    // Compatibility entry point: without a target captured when dictation
    // started, copying is safe but synthesizing a paste is not.
    write_to_captured_target(text, config, None)
}

pub fn write_to_captured_target(
    text: &str,
    config: &AppConfig,
    target: Option<&CapturedTarget>,
) -> TextOutputResult {
    let mut clipboard = match SystemClipboard::new() {
        Ok(clipboard) => clipboard,
        Err(err) => return TextOutputResult::Failed(err.to_string()),
    };
    let mut paste = EnigoPasteDriver;
    let mut verifier = SystemForegroundTargetVerifier;

    write_text_to_captured_target_with_drivers(
        &mut clipboard,
        &mut paste,
        &mut verifier,
        text,
        TextOutputOptions::from_config(config),
        paste_automation_available(),
        target,
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

pub trait ForegroundTargetVerifier {
    fn captured_target_is_foreground(&mut self, target: &CapturedTarget) -> bool;
}

#[cfg_attr(not(test), allow(dead_code))] // Retained for the existing injected-driver test API.
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
    write_text_with_authorization(clipboard, paste, text, options, paste_available, || true)
}

pub fn write_text_to_captured_target_with_drivers<C, P, V>(
    clipboard: &mut C,
    paste: &mut P,
    verifier: &mut V,
    text: &str,
    options: TextOutputOptions,
    paste_available: bool,
    target: Option<&CapturedTarget>,
) -> TextOutputResult
where
    C: ClipboardDriver,
    P: PasteDriver,
    V: ForegroundTargetVerifier,
{
    write_text_with_authorization(clipboard, paste, text, options, paste_available, || {
        target.is_some_and(|target| verifier.captured_target_is_foreground(target))
    })
}

fn write_text_with_authorization<C, P, F>(
    clipboard: &mut C,
    paste: &mut P,
    text: &str,
    options: TextOutputOptions,
    paste_available: bool,
    mut paste_is_authorized: F,
) -> TextOutputResult
where
    C: ClipboardDriver,
    P: PasteDriver,
    F: FnMut() -> bool,
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
    // Keep this check adjacent to the synthetic input. We intentionally do not
    // reactivate or guess a target: a focus change becomes clipboard-only.
    if !paste_is_authorized() {
        return TextOutputResult::CopiedOnly;
    }
    if paste.paste().is_err() {
        return TextOutputResult::CopiedOnly;
    }

    sleep_for_paste_delay(options.paste_delay_ms);
    if let Some(previous_clipboard) = previous_clipboard {
        // Do not overwrite a clipboard value changed independently while the
        // paste was in flight.
        if clipboard.get_text().ok().as_deref() == Some(text) {
            let _ = clipboard.set_text(previous_clipboard);
        }
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

struct SystemForegroundTargetVerifier;

impl ForegroundTargetVerifier for SystemForegroundTargetVerifier {
    fn captured_target_is_foreground(&mut self, target: &CapturedTarget) -> bool {
        captured_target_is_foreground(target)
    }
}

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

    struct FakeTargetVerifier {
        is_foreground: bool,
        calls: usize,
    }

    impl ForegroundTargetVerifier for FakeTargetVerifier {
        fn captured_target_is_foreground(&mut self, _target: &CapturedTarget) -> bool {
            self.calls += 1;
            self.is_foreground
        }
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

    #[test]
    fn changed_target_copies_without_pasting() {
        let mut clipboard = FakeClipboard {
            text: Some("before".to_owned()),
            ..FakeClipboard::default()
        };
        let mut paste = FakePaste::default();
        let mut verifier = FakeTargetVerifier {
            is_foreground: false,
            calls: 0,
        };
        let target = CapturedTarget::for_test(11, 22);

        let result = write_text_to_captured_target_with_drivers(
            &mut clipboard,
            &mut paste,
            &mut verifier,
            "hello",
            fast_options(),
            true,
            Some(&target),
        );

        assert_eq!(result, TextOutputResult::CopiedOnly);
        assert_eq!(clipboard.text.as_deref(), Some("hello"));
        assert_eq!(verifier.calls, 1);
        assert_eq!(paste.calls, 0);
    }

    #[test]
    fn missing_target_copies_without_pasting() {
        let mut clipboard = FakeClipboard::default();
        let mut paste = FakePaste::default();
        let mut verifier = FakeTargetVerifier {
            is_foreground: true,
            calls: 0,
        };

        let result = write_text_to_captured_target_with_drivers(
            &mut clipboard,
            &mut paste,
            &mut verifier,
            "hello",
            fast_options(),
            true,
            None,
        );

        assert_eq!(result, TextOutputResult::CopiedOnly);
        assert_eq!(clipboard.text.as_deref(), Some("hello"));
        assert_eq!(verifier.calls, 0);
        assert_eq!(paste.calls, 0);
    }

    #[test]
    fn unchanged_captured_target_pastes_exactly_once() {
        let mut clipboard = FakeClipboard {
            text: Some("before".to_owned()),
            ..FakeClipboard::default()
        };
        let mut paste = FakePaste::default();
        let mut verifier = FakeTargetVerifier {
            is_foreground: true,
            calls: 0,
        };
        let target = CapturedTarget::for_test(11, 22);

        let result = write_text_to_captured_target_with_drivers(
            &mut clipboard,
            &mut paste,
            &mut verifier,
            "hello",
            fast_options(),
            true,
            Some(&target),
        );

        assert_eq!(result, TextOutputResult::Inserted);
        assert_eq!(verifier.calls, 1);
        assert_eq!(paste.calls, 1);
        assert_eq!(clipboard.text.as_deref(), Some("before"));
    }

    #[test]
    fn independent_clipboard_change_is_not_overwritten_after_paste() {
        struct ClipboardChangedByPaste {
            text: String,
            reads: usize,
        }

        impl ClipboardDriver for ClipboardChangedByPaste {
            fn get_text(&mut self) -> Result<String> {
                self.reads += 1;
                if self.reads == 1 {
                    Ok(self.text.clone())
                } else {
                    self.text = "user change".to_owned();
                    Ok(self.text.clone())
                }
            }

            fn set_text(&mut self, text: String) -> Result<()> {
                self.text = text;
                Ok(())
            }
        }

        let mut clipboard = ClipboardChangedByPaste {
            text: "before".to_owned(),
            reads: 0,
        };
        let mut paste = FakePaste::default();

        let result =
            write_text_with_drivers(&mut clipboard, &mut paste, "hello", fast_options(), true);

        assert_eq!(result, TextOutputResult::Inserted);
        assert_eq!(clipboard.text, "user change");
    }
}
