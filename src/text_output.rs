use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use arboard::Clipboard;
#[cfg(not(target_os = "windows"))]
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CopyOnlyReason {
    TargetUnavailable,
    AutomationUnavailable,
    PasteFailed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotInsertedReason {
    ClipboardChanged,
    ClipboardUnverifiable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TextOutputResult {
    Inserted,
    CopiedOnly(CopyOnlyReason),
    NotInserted(NotInsertedReason),
    Failed(String),
}

impl TextOutputResult {
    pub fn status_message(&self) -> String {
        match self {
            Self::Inserted => "Transcript inserted into the focused app".to_owned(),
            Self::CopiedOnly(CopyOnlyReason::TargetUnavailable) =>
                "Transcript copied; the original target is no longer active, so Scribe did not paste"
                    .to_owned(),
            Self::CopiedOnly(CopyOnlyReason::AutomationUnavailable) =>
                "Transcript copied; safe paste automation is unavailable on this desktop"
                    .to_owned(),
            Self::CopiedOnly(CopyOnlyReason::PasteFailed) =>
                "Transcript copied; the paste command failed without retrying".to_owned(),
            Self::NotInserted(NotInsertedReason::ClipboardChanged) =>
                "Transcript was not pasted because another app changed the clipboard; the final text remains in Scribe"
                    .to_owned(),
            Self::NotInserted(NotInsertedReason::ClipboardUnverifiable) =>
                "Transcript was not pasted because Scribe could not verify clipboard ownership; the final text remains in Scribe"
                    .to_owned(),
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
    let mut paste = SystemPasteDriver {
        target: target.copied(),
    };
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

    /// Returns an OS clipboard generation when the platform exposes one.
    /// Content equality remains a secondary check, never the sole ownership
    /// signal on Windows where rich clipboard formats may differ.
    fn change_token(&mut self) -> Option<u64> {
        None
    }
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
    let owned_clipboard_token = clipboard.change_token();

    if !paste_available {
        return TextOutputResult::CopiedOnly(CopyOnlyReason::AutomationUnavailable);
    }

    sleep_for_paste_delay(options.paste_delay_ms);
    match clipboard.get_text() {
        Ok(current) if current == text => {}
        Ok(_) => return TextOutputResult::NotInserted(NotInsertedReason::ClipboardChanged),
        Err(_) => {
            return TextOutputResult::NotInserted(NotInsertedReason::ClipboardUnverifiable);
        }
    }
    if owned_clipboard_token.is_some() && clipboard.change_token() != owned_clipboard_token {
        return TextOutputResult::NotInserted(NotInsertedReason::ClipboardChanged);
    }
    // Keep this check adjacent to the synthetic input. We intentionally do not
    // reactivate or guess a target: a focus change becomes clipboard-only.
    if !paste_is_authorized() {
        return TextOutputResult::CopiedOnly(CopyOnlyReason::TargetUnavailable);
    }
    if paste.paste().is_err() {
        return TextOutputResult::CopiedOnly(CopyOnlyReason::PasteFailed);
    }

    sleep_for_paste_delay(options.paste_delay_ms);
    if let Some(previous_clipboard) = previous_clipboard {
        // Do not overwrite a clipboard value changed independently while the
        // paste was in flight.
        let generation_is_owned = owned_clipboard_token
            .map(|token| clipboard.change_token() == Some(token))
            .unwrap_or(true);
        if generation_is_owned && clipboard.get_text().ok().as_deref() == Some(text) {
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

    fn change_token(&mut self) -> Option<u64> {
        system_clipboard_change_token()
    }
}

#[cfg(target_os = "windows")]
fn system_clipboard_change_token() -> Option<u64> {
    use windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber;

    Some(unsafe { GetClipboardSequenceNumber() } as u64)
}

#[cfg(not(target_os = "windows"))]
fn system_clipboard_change_token() -> Option<u64> {
    None
}

struct SystemPasteDriver {
    target: Option<CapturedTarget>,
}

struct SystemForegroundTargetVerifier;

impl ForegroundTargetVerifier for SystemForegroundTargetVerifier {
    fn captured_target_is_foreground(&mut self, target: &CapturedTarget) -> bool {
        captured_target_is_foreground(target)
    }
}

impl PasteDriver for SystemPasteDriver {
    fn paste(&mut self) -> Result<()> {
        #[cfg(target_os = "windows")]
        {
            let target = self
                .target
                .as_ref()
                .ok_or_else(|| anyhow!("captured target is unavailable"))?;
            if !captured_target_is_foreground(target) {
                return Err(anyhow!("captured target changed before input injection"));
            }
            send_windows_paste_chord()
        }

        #[cfg(not(target_os = "windows"))]
        {
            let _ = self.target;
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
}

#[cfg(target_os = "windows")]
fn send_windows_paste_chord() -> Result<()> {
    use std::mem::size_of;

    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{INPUT, SendInput};
    send_windows_paste_chord_with(|inputs| unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            size_of::<INPUT>() as i32,
        )
    })
}

#[cfg(target_os = "windows")]
fn send_windows_paste_chord_with(
    mut send: impl FnMut(&[windows_sys::Win32::UI::Input::KeyboardAndMouse::INPUT]) -> u32,
) -> Result<()> {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VK_CONTROL,
    };

    const VIRTUAL_KEY_V: u16 = 0x56;
    let keyboard_input = |virtual_key, flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: virtual_key,
                wScan: 0,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let inputs = [
        keyboard_input(VK_CONTROL, 0),
        keyboard_input(VIRTUAL_KEY_V, 0),
        keyboard_input(VIRTUAL_KEY_V, KEYEVENTF_KEYUP),
        keyboard_input(VK_CONTROL, KEYEVENTF_KEYUP),
    ];
    let inserted = send(&inputs);
    if inserted != inputs.len() as u32 {
        // Windows may accept only a prefix of the batch. Always attempt to
        // release both keys before reporting the failure so a partial
        // Control-down cannot leave the user's keyboard state latched.
        let releases = [
            keyboard_input(VIRTUAL_KEY_V, KEYEVENTF_KEYUP),
            keyboard_input(VK_CONTROL, KEYEVENTF_KEYUP),
        ];
        let _ = send(&releases);
        return Err(anyhow!(
            "Windows accepted {inserted} of {} paste input events",
            inputs.len()
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn paste_modifier_key() -> Key {
    Key::Meta
}

#[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
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
    fn unverifiable_clipboard_ownership_prevents_paste() {
        let mut clipboard = FakeClipboard {
            fail_get: true,
            ..FakeClipboard::default()
        };
        let mut paste = FakePaste::default();

        let result =
            write_text_with_drivers(&mut clipboard, &mut paste, "hello", fast_options(), true);

        assert_eq!(
            result,
            TextOutputResult::NotInserted(NotInsertedReason::ClipboardUnverifiable)
        );
        assert_eq!(clipboard.text.as_deref(), Some("hello"));
        assert_eq!(paste.calls, 0);
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

        assert_eq!(
            result,
            TextOutputResult::CopiedOnly(CopyOnlyReason::PasteFailed)
        );
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

        assert_eq!(
            result,
            TextOutputResult::CopiedOnly(CopyOnlyReason::AutomationUnavailable)
        );
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

        assert_eq!(
            result,
            TextOutputResult::CopiedOnly(CopyOnlyReason::TargetUnavailable)
        );
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

        assert_eq!(
            result,
            TextOutputResult::CopiedOnly(CopyOnlyReason::TargetUnavailable)
        );
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
                if self.reads == 3 {
                    self.text = "user change".to_owned();
                }
                Ok(self.text.clone())
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

    #[test]
    fn clipboard_change_before_paste_prevents_synthetic_input() {
        struct ClipboardChangedDuringDelay {
            text: String,
            reads: usize,
        }

        impl ClipboardDriver for ClipboardChangedDuringDelay {
            fn get_text(&mut self) -> Result<String> {
                self.reads += 1;
                if self.reads == 2 {
                    self.text = "user change".to_owned();
                }
                Ok(self.text.clone())
            }

            fn set_text(&mut self, text: String) -> Result<()> {
                self.text = text;
                Ok(())
            }
        }

        let mut clipboard = ClipboardChangedDuringDelay {
            text: "before".to_owned(),
            reads: 0,
        };
        let mut paste = FakePaste::default();

        let result =
            write_text_with_drivers(&mut clipboard, &mut paste, "hello", fast_options(), true);

        assert_eq!(
            result,
            TextOutputResult::NotInserted(NotInsertedReason::ClipboardChanged)
        );
        assert_eq!(clipboard.text, "user change");
        assert_eq!(paste.calls, 0);
    }

    #[test]
    fn same_text_with_a_new_clipboard_generation_prevents_paste() {
        struct SameTextNewGeneration {
            text: String,
            generation: u64,
            token_reads: usize,
        }

        impl ClipboardDriver for SameTextNewGeneration {
            fn get_text(&mut self) -> Result<String> {
                Ok(self.text.clone())
            }

            fn set_text(&mut self, text: String) -> Result<()> {
                self.text = text;
                self.generation += 1;
                Ok(())
            }

            fn change_token(&mut self) -> Option<u64> {
                self.token_reads += 1;
                if self.token_reads == 2 {
                    self.generation += 1;
                }
                Some(self.generation)
            }
        }

        let mut clipboard = SameTextNewGeneration {
            text: "before".to_owned(),
            generation: 4,
            token_reads: 0,
        };
        let mut paste = FakePaste::default();

        let result =
            write_text_with_drivers(&mut clipboard, &mut paste, "hello", fast_options(), true);

        assert_eq!(
            result,
            TextOutputResult::NotInserted(NotInsertedReason::ClipboardChanged)
        );
        assert_eq!(clipboard.text, "hello");
        assert_eq!(paste.calls, 0);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn every_partial_windows_input_batch_releases_v_then_control() {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{KEYEVENTF_KEYUP, VK_CONTROL};

        for partial in 0..4 {
            let mut batches = Vec::new();
            let result = send_windows_paste_chord_with(|inputs| {
                let keys = inputs
                    .iter()
                    .map(|input| unsafe { (input.Anonymous.ki.wVk, input.Anonymous.ki.dwFlags) })
                    .collect::<Vec<_>>();
                batches.push(keys);
                if batches.len() == 1 {
                    partial
                } else {
                    inputs.len() as u32
                }
            });

            assert!(result.is_err());
            assert_eq!(batches.len(), 2);
            assert_eq!(
                batches[1],
                vec![(0x56, KEYEVENTF_KEYUP), (VK_CONTROL, KEYEVENTF_KEYUP)]
            );
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn failed_windows_key_release_cleanup_is_not_retried() {
        let mut batch_lengths = Vec::new();
        let result = send_windows_paste_chord_with(|inputs| {
            batch_lengths.push(inputs.len());
            0
        });

        assert!(result.is_err());
        assert_eq!(batch_lengths, vec![4, 2]);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn complete_windows_input_batch_needs_no_cleanup() {
        let mut batch_lengths = Vec::new();
        let result = send_windows_paste_chord_with(|inputs| {
            batch_lengths.push(inputs.len());
            inputs.len() as u32
        });

        assert!(result.is_ok());
        assert_eq!(batch_lengths, vec![4]);
    }
}
