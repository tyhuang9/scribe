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
            restore_clipboard_after_insert: config.restore_clipboard_after_insert,
            paste_delay_ms: config.paste_delay_ms,
        }
    }
}

/// The destination captured before a delayed semantic edit begins.
///
/// Platforms without a stable, OS-backed focused-window identity deliberately
/// fall back to clipboard-only output for delayed edits.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CapturedOutputTarget {
    #[cfg(target_os = "windows")]
    Windows(WindowsOutputTarget),
    #[cfg_attr(target_os = "windows", allow(dead_code))]
    CopyOnly,
}

#[cfg(target_os = "windows")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WindowsOutputTarget {
    hwnd: usize,
    interaction: WindowsInteractionIdentity,
    process_id: u32,
    process_creation_time: u64,
}

#[cfg(target_os = "windows")]
#[derive(Clone, Debug, Eq, PartialEq)]
struct WindowsInteractionIdentity {
    focused_hwnd: usize,
    caret_hwnd: usize,
    caret_rect: [i32; 4],
    window_title: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TextOutputResult {
    Inserted,
    CopiedOnly,
    TargetChanged,
    Failed(String),
}

impl TextOutputResult {
    pub fn status_message(&self) -> String {
        match self {
            Self::Inserted => "Transcript inserted into the focused app".to_owned(),
            Self::CopiedOnly => {
                "Transcript copied to clipboard; paste automation was unavailable".to_owned()
            }
            Self::TargetChanged => {
                "The focused app changed; Scribe did not paste the edited transcript".to_owned()
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

/// Captures a stable destination identity before a delayed semantic operation.
///
/// Failure is intentionally fail-closed on Windows: callers should retain the
/// edited text in Scribe and offer an explicit Copy action instead of guessing
/// where to paste it.
pub fn capture_output_target() -> Result<CapturedOutputTarget, String> {
    #[cfg(target_os = "windows")]
    {
        windows_target::capture().map(CapturedOutputTarget::Windows)
    }

    #[cfg(not(target_os = "windows"))]
    {
        Ok(CapturedOutputTarget::CopyOnly)
    }
}

/// Writes a delayed result only when the previously captured destination is
/// still foreground and still belongs to the same process instance.
pub fn write_to_captured_target(
    text: &str,
    config: &AppConfig,
    target: &CapturedOutputTarget,
) -> TextOutputResult {
    let mut clipboard = match SystemClipboard::new() {
        Ok(clipboard) => clipboard,
        Err(err) => return TextOutputResult::Failed(err.to_string()),
    };
    let mut paste = EnigoPasteDriver;
    let mut guard = SystemTargetGuard { target };

    write_text_with_target_guard(
        &mut clipboard,
        &mut paste,
        &mut guard,
        text,
        TextOutputOptions::from_config(config),
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

trait TargetGuard {
    fn matches_captured_target(&mut self) -> bool;
    fn paste_allowed(&self) -> bool;
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

fn write_text_with_target_guard<C, P, G>(
    clipboard: &mut C,
    paste: &mut P,
    guard: &mut G,
    text: &str,
    options: TextOutputOptions,
) -> TextOutputResult
where
    C: ClipboardDriver,
    P: PasteDriver,
    G: TargetGuard,
{
    if text.trim().is_empty() {
        return TextOutputResult::Failed("transcript is empty".to_owned());
    }

    // This check intentionally precedes even reading or writing the clipboard.
    if !guard.matches_captured_target() {
        return TextOutputResult::TargetChanged;
    }

    // A delayed target mismatch is an aborted output operation, so retain the
    // prior value independently of the normal post-insert restore preference.
    let previous_clipboard = clipboard.get_text().ok();
    // Clipboard reads may block behind another process. Revalidate again at
    // the actual mutation boundary so a focus change during that read cannot
    // redirect a delayed result.
    if !guard.matches_captured_target() {
        return TextOutputResult::TargetChanged;
    }
    if let Err(err) = clipboard.set_text(text.to_owned()) {
        return TextOutputResult::Failed(err.to_string());
    }

    if !guard.paste_allowed() {
        return TextOutputResult::CopiedOnly;
    }

    sleep_for_paste_delay(options.paste_delay_ms);
    if !guard.matches_captured_target() {
        restore_clipboard_if_unchanged(clipboard, text, previous_clipboard);
        return TextOutputResult::TargetChanged;
    }

    if paste.paste().is_err() {
        return TextOutputResult::CopiedOnly;
    }

    sleep_for_paste_delay(options.paste_delay_ms);
    if options.restore_clipboard_after_insert {
        restore_clipboard_if_unchanged(clipboard, text, previous_clipboard);
    }

    TextOutputResult::Inserted
}

fn restore_clipboard_if_unchanged<C: ClipboardDriver>(
    clipboard: &mut C,
    scribe_text: &str,
    previous_clipboard: Option<String>,
) {
    let Some(previous_clipboard) = previous_clipboard else {
        return;
    };
    if clipboard
        .get_text()
        .is_ok_and(|current| current == scribe_text)
    {
        let _ = clipboard.set_text(previous_clipboard);
    }
}

struct SystemTargetGuard<'a> {
    target: &'a CapturedOutputTarget,
}

impl TargetGuard for SystemTargetGuard<'_> {
    fn matches_captured_target(&mut self) -> bool {
        match self.target {
            #[cfg(target_os = "windows")]
            CapturedOutputTarget::Windows(target) => windows_target::matches(target),
            CapturedOutputTarget::CopyOnly => true,
        }
    }

    fn paste_allowed(&self) -> bool {
        match self.target {
            #[cfg(target_os = "windows")]
            CapturedOutputTarget::Windows(_) => paste_automation_available(),
            CapturedOutputTarget::CopyOnly => false,
        }
    }
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

#[cfg(target_os = "windows")]
mod windows_target {
    use std::mem::{size_of, zeroed};
    use std::ptr::null_mut;

    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HWND};
    use windows_sys::Win32::Security::{
        GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcessId, GetProcessTimes, OpenProcess, OpenProcessToken,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GUITHREADINFO, GetForegroundWindow, GetGUIThreadInfo, GetWindowTextLengthW, GetWindowTextW,
        GetWindowThreadProcessId, IsWindow,
    };

    use super::{WindowsInteractionIdentity, WindowsOutputTarget};

    pub(super) fn capture() -> Result<WindowsOutputTarget, String> {
        // SAFETY: Every Win32 handle is checked before use. The process handle
        // is closed on all paths, and all output pointers reference live local
        // variables for the duration of each call.
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.is_null() || IsWindow(hwnd) == 0 {
                return Err("no verifiable foreground window is available".to_owned());
            }

            let mut process_id = 0_u32;
            let thread_id = GetWindowThreadProcessId(hwnd, &mut process_id);
            if thread_id == 0 || process_id == 0 {
                return Err("the foreground window owner could not be verified".to_owned());
            }
            if process_id == GetCurrentProcessId() {
                return Err("Scribe cannot be its own delayed paste target".to_owned());
            }

            let process_creation_time = process_creation_time(process_id)?;
            let target = WindowsOutputTarget {
                hwnd: hwnd as usize,
                interaction: interaction_identity(hwnd, thread_id)?,
                process_id,
                process_creation_time,
            };
            if !matches(&target) {
                return Err("the foreground window changed while it was captured".to_owned());
            }
            Ok(target)
        }
    }

    pub(super) fn matches(target: &WindowsOutputTarget) -> bool {
        // SAFETY: The stored integer is used only as an opaque HWND and is
        // validated with IsWindow before any identity comparison.
        unsafe {
            let hwnd = target.hwnd as HWND;
            if hwnd.is_null() || IsWindow(hwnd) == 0 || GetForegroundWindow() != hwnd {
                return false;
            }
            let mut process_id = 0_u32;
            let thread_id = GetWindowThreadProcessId(hwnd, &mut process_id);
            if thread_id == 0 || process_id != target.process_id {
                return false;
            }
            let Ok(interaction) = interaction_identity(hwnd, thread_id) else {
                return false;
            };
            process_creation_time(process_id)
                .is_ok_and(|created| created == target.process_creation_time)
                && interaction_matches(&target.interaction, &interaction)
        }
    }

    unsafe fn interaction_identity(
        hwnd: HWND,
        thread_id: u32,
    ) -> Result<WindowsInteractionIdentity, String> {
        // SAFETY: User32 initializes the caller-owned structure after cbSize is
        // set. Returned HWND values remain opaque and are validated before use.
        let mut info: GUITHREADINFO = unsafe { zeroed() };
        info.cbSize = size_of::<GUITHREADINFO>() as u32;
        if unsafe { GetGUIThreadInfo(thread_id, &mut info) } == 0
            || info.hwndFocus.is_null()
            || unsafe { IsWindow(info.hwndFocus) } == 0
        {
            return Err("the focused control could not be verified".to_owned());
        }
        Ok(WindowsInteractionIdentity {
            focused_hwnd: info.hwndFocus as usize,
            caret_hwnd: info.hwndCaret as usize,
            caret_rect: [
                info.rcCaret.left,
                info.rcCaret.top,
                info.rcCaret.right,
                info.rcCaret.bottom,
            ],
            window_title: unsafe { window_title(hwnd) }?,
        })
    }

    fn interaction_matches(
        captured: &WindowsInteractionIdentity,
        current: &WindowsInteractionIdentity,
    ) -> bool {
        captured == current
    }

    unsafe fn window_title(hwnd: HWND) -> Result<String, String> {
        // SAFETY: The UTF-16 buffer is sized for the reported title length and
        // passed only for the duration of GetWindowTextW.
        let length = unsafe { GetWindowTextLengthW(hwnd) };
        if length <= 0 {
            return Ok(String::new());
        }
        let capacity = usize::try_from(length)
            .unwrap_or_default()
            .min(1_024)
            .saturating_add(1);
        let mut buffer = vec![0_u16; capacity];
        let copied = unsafe { GetWindowTextW(hwnd, buffer.as_mut_ptr(), capacity as i32) };
        if copied <= 0 {
            return Err("the target window title could not be read".to_owned());
        }
        Ok(String::from_utf16_lossy(&buffer[..copied as usize]))
    }

    unsafe fn process_creation_time(process_id: u32) -> Result<u64, String> {
        // SAFETY: The caller passes a PID obtained from User32. OpenProcess
        // returns an owned handle which this function always closes.
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id) };
        if process.is_null() {
            return Err("the target process is inaccessible".to_owned());
        }
        let mut created: FILETIME = unsafe { zeroed() };
        let mut exited: FILETIME = unsafe { zeroed() };
        let mut kernel: FILETIME = unsafe { zeroed() };
        let mut user: FILETIME = unsafe { zeroed() };
        let times_succeeded =
            unsafe { GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) }
                != 0;
        let mut token = null_mut();
        let token_opened = unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } != 0;
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut returned = 0_u32;
        let elevation_read = token_opened
            && unsafe {
                GetTokenInformation(
                    token,
                    TokenElevation,
                    (&raw mut elevation).cast(),
                    size_of::<TOKEN_ELEVATION>() as u32,
                    &mut returned,
                )
            } != 0;
        if token_opened {
            unsafe {
                CloseHandle(token);
            }
        }
        unsafe {
            CloseHandle(process);
        }
        if !times_succeeded || !elevation_read {
            return Err("the target process identity could not be read".to_owned());
        }
        if elevation.TokenIsElevated != 0 {
            return Err("elevated applications are not eligible for delayed paste".to_owned());
        }
        Ok(((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn identity() -> WindowsInteractionIdentity {
            WindowsInteractionIdentity {
                focused_hwnd: 20,
                caret_hwnd: 30,
                caret_rect: [4, 8, 5, 24],
                window_title: "Document A - Editor".to_owned(),
            }
        }

        #[test]
        fn same_top_level_window_with_changed_child_is_rejected() {
            let captured = identity();
            let mut current = captured.clone();
            current.focused_hwnd = 21;

            assert!(!interaction_matches(&captured, &current));
        }

        #[test]
        fn same_control_with_changed_document_or_caret_is_rejected() {
            let captured = identity();
            let mut changed_document = captured.clone();
            changed_document.window_title = "Document B - Editor".to_owned();
            let mut changed_caret = captured.clone();
            changed_caret.caret_rect[0] += 1;

            assert!(!interaction_matches(&captured, &changed_document));
            assert!(!interaction_matches(&captured, &changed_caret));
            assert!(interaction_matches(&captured, &captured));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;

    #[derive(Default)]
    struct FakeClipboard {
        text: Option<String>,
        fail_get: bool,
        fail_set: bool,
        scripted_gets: VecDeque<Option<String>>,
        set_history: Vec<String>,
    }

    impl ClipboardDriver for FakeClipboard {
        fn get_text(&mut self) -> Result<String> {
            if self.fail_get {
                Err(anyhow!("get failed"))
            } else if let Some(scripted) = self.scripted_gets.pop_front() {
                scripted.ok_or_else(|| anyhow!("scripted get failed"))
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

    struct FakeTargetGuard {
        checks: VecDeque<bool>,
        paste_allowed: bool,
    }

    impl FakeTargetGuard {
        fn new(checks: impl IntoIterator<Item = bool>, paste_allowed: bool) -> Self {
            Self {
                checks: checks.into_iter().collect(),
                paste_allowed,
            }
        }
    }

    impl TargetGuard for FakeTargetGuard {
        fn matches_captured_target(&mut self) -> bool {
            self.checks.pop_front().unwrap_or(false)
        }

        fn paste_allowed(&self) -> bool {
            self.paste_allowed
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

    #[test]
    fn delayed_output_target_change_before_clipboard_does_not_mutate_output() {
        let mut clipboard = FakeClipboard {
            text: Some("before".to_owned()),
            ..FakeClipboard::default()
        };
        let mut paste = FakePaste::default();
        let mut guard = FakeTargetGuard::new([false], true);

        let result = write_text_with_target_guard(
            &mut clipboard,
            &mut paste,
            &mut guard,
            "hello",
            fast_options(),
        );

        assert_eq!(result, TextOutputResult::TargetChanged);
        assert_eq!(clipboard.text.as_deref(), Some("before"));
        assert!(clipboard.set_history.is_empty());
        assert_eq!(paste.calls, 0);
    }

    #[test]
    fn delayed_output_rechecks_focus_after_read_and_before_clipboard_write() {
        let mut clipboard = FakeClipboard {
            text: Some("before".to_owned()),
            ..FakeClipboard::default()
        };
        let mut paste = FakePaste::default();
        let mut guard = FakeTargetGuard::new([true, false], true);

        let result = write_text_with_target_guard(
            &mut clipboard,
            &mut paste,
            &mut guard,
            "hello",
            fast_options(),
        );

        assert_eq!(result, TextOutputResult::TargetChanged);
        assert_eq!(clipboard.text.as_deref(), Some("before"));
        assert!(clipboard.set_history.is_empty());
        assert_eq!(paste.calls, 0);
    }

    #[test]
    fn delayed_output_target_change_before_paste_restores_untouched_clipboard() {
        let mut clipboard = FakeClipboard {
            text: Some("before".to_owned()),
            ..FakeClipboard::default()
        };
        let mut paste = FakePaste::default();
        let mut guard = FakeTargetGuard::new([true, true, false], true);

        let result = write_text_with_target_guard(
            &mut clipboard,
            &mut paste,
            &mut guard,
            "hello",
            fast_options(),
        );

        assert_eq!(result, TextOutputResult::TargetChanged);
        assert_eq!(clipboard.text.as_deref(), Some("before"));
        assert_eq!(clipboard.set_history, vec!["hello", "before"]);
        assert_eq!(paste.calls, 0);
    }

    #[test]
    fn delayed_output_never_clobbers_a_concurrent_clipboard_change() {
        let mut clipboard = FakeClipboard {
            text: Some("before".to_owned()),
            scripted_gets: [Some("before".to_owned()), Some("external".to_owned())]
                .into_iter()
                .collect(),
            ..FakeClipboard::default()
        };
        let mut paste = FakePaste::default();
        let mut guard = FakeTargetGuard::new([true, true, false], true);

        let result = write_text_with_target_guard(
            &mut clipboard,
            &mut paste,
            &mut guard,
            "hello",
            fast_options(),
        );

        assert_eq!(result, TextOutputResult::TargetChanged);
        assert_eq!(clipboard.set_history, vec!["hello"]);
        assert_eq!(paste.calls, 0);
    }

    #[test]
    fn unsupported_delayed_target_is_copy_only() {
        let mut clipboard = FakeClipboard {
            text: Some("before".to_owned()),
            ..FakeClipboard::default()
        };
        let mut paste = FakePaste::default();
        let mut guard = FakeTargetGuard::new([true, true], false);

        let result = write_text_with_target_guard(
            &mut clipboard,
            &mut paste,
            &mut guard,
            "hello",
            fast_options(),
        );

        assert_eq!(result, TextOutputResult::CopiedOnly);
        assert_eq!(clipboard.text.as_deref(), Some("hello"));
        assert_eq!(paste.calls, 0);
    }

    #[test]
    fn guarded_success_restores_only_when_scribe_still_owns_clipboard() {
        let mut clipboard = FakeClipboard {
            text: Some("before".to_owned()),
            scripted_gets: [Some("before".to_owned()), Some("external".to_owned())]
                .into_iter()
                .collect(),
            ..FakeClipboard::default()
        };
        let mut paste = FakePaste::default();
        let mut guard = FakeTargetGuard::new([true, true, true], true);

        let result = write_text_with_target_guard(
            &mut clipboard,
            &mut paste,
            &mut guard,
            "hello",
            fast_options(),
        );

        assert_eq!(result, TextOutputResult::Inserted);
        assert_eq!(clipboard.set_history, vec!["hello"]);
        assert_eq!(paste.calls, 1);
    }
}
