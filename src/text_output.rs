use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
#[cfg(not(target_os = "windows"))]
use arboard::Error as ClipboardError;
use arboard::{Clipboard, ImageData};
use std::borrow::Cow;

use crate::config::AppConfig;
#[cfg(target_os = "windows")]
use crate::overlay::platform::reactivate_and_verify_captured_target;
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
    ClipboardSnapshotUnavailable,
    ClipboardSnapshotUnsupported,
    ClipboardSnapshotError,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NotInsertedReason {
    ClipboardChanged,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TextOutputResult {
    Inserted,
    InsertedClipboardRestoreFailed(String),
    CopiedOnly(CopyOnlyReason),
    NotInserted(NotInsertedReason),
    Failed(String),
}

impl TextOutputResult {
    pub fn did_insert(&self) -> bool {
        matches!(
            self,
            Self::Inserted | Self::InsertedClipboardRestoreFailed(_)
        )
    }

    pub fn status_message(&self) -> String {
        match self {
            Self::Inserted => "Transcript inserted into the focused app".to_owned(),
            Self::InsertedClipboardRestoreFailed(message) => format!(
                "Transcript inserted, but Scribe could not restore the previous clipboard: {message}"
            ),
            Self::CopiedOnly(CopyOnlyReason::TargetUnavailable) =>
                "Transcript copied; the original target is no longer active, so Scribe did not paste"
                    .to_owned(),
            Self::CopiedOnly(CopyOnlyReason::AutomationUnavailable) =>
                "Transcript copied; safe paste automation is unavailable on this desktop"
                    .to_owned(),
            Self::CopiedOnly(CopyOnlyReason::PasteFailed) =>
                "Transcript copied; the paste command failed without retrying".to_owned(),
            Self::CopiedOnly(CopyOnlyReason::ClipboardSnapshotUnavailable) =>
                "Transcript copied; Scribe could not safely preserve the existing clipboard, so it did not paste"
                    .to_owned(),
            Self::CopiedOnly(CopyOnlyReason::ClipboardSnapshotUnsupported) =>
                "Transcript copied; the existing clipboard format cannot be safely restored, so Scribe did not paste"
                    .to_owned(),
            Self::CopiedOnly(CopyOnlyReason::ClipboardSnapshotError) =>
                "Transcript copied; Scribe could not snapshot the existing clipboard, so it did not paste"
                    .to_owned(),
            Self::NotInserted(NotInsertedReason::ClipboardChanged) =>
                "Transcript was not pasted because another app changed the clipboard; the final text remains in Scribe"
                    .to_owned(),
            Self::Failed(message) => format!("Transcript output failed: {message}"),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct TextOutputTiming {
    pub target_activated_at: Option<Instant>,
    pub paste_completed_at: Option<Instant>,
}

#[derive(Clone, Debug)]
pub struct TextOutputOutcome {
    pub result: TextOutputResult,
    pub timing: TextOutputTiming,
}

impl From<TextOutputResult> for TextOutputOutcome {
    fn from(result: TextOutputResult) -> Self {
        Self {
            result,
            timing: TextOutputTiming::default(),
        }
    }
}

pub fn write_to_captured_target(
    text: &str,
    config: &AppConfig,
    target: Option<&CapturedTarget>,
) -> TextOutputOutcome {
    let mut clipboard = match SystemClipboard::new() {
        Ok(clipboard) => clipboard,
        Err(err) => return TextOutputResult::Failed(err.to_string()).into(),
    };
    let mut paste = SystemPasteDriver {
        target: target.copied(),
    };
    let mut verifier = SystemForegroundTargetVerifier;

    write_text_to_captured_target_with_drivers_outcome(
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
            "Verified paste automation is unavailable on this platform, so transcripts will remain on the clipboard.",
        )
    }
}

pub trait ClipboardDriver {
    fn snapshot(&mut self) -> Result<ClipboardSnapshot>;
    fn restore(&mut self, snapshot: ClipboardSnapshot) -> Result<()>;
    fn get_text(&mut self) -> Result<String>;
    fn set_text(&mut self, text: String) -> Result<()>;

    fn set_text_if_token(
        &mut self,
        expected_token: Option<u64>,
        text: String,
    ) -> Result<ConditionalClipboardWrite> {
        if expected_token.is_some() && self.change_token() != expected_token {
            return Ok(ConditionalClipboardWrite::Changed);
        }
        self.set_text(text)?;
        Ok(ConditionalClipboardWrite::Written(self.change_token()))
    }

    fn contents_match_token(&mut self, expected_token: u64, text: &str) -> bool {
        self.change_token() == Some(expected_token) && self.get_text().ok().as_deref() == Some(text)
    }

    fn restore_if_token(
        &mut self,
        expected_token: u64,
        expected_text: &str,
        snapshot: ClipboardSnapshot,
    ) -> Result<ConditionalClipboardRestore> {
        if !self.contents_match_token(expected_token, expected_text) {
            return Ok(ConditionalClipboardRestore::Changed);
        }
        self.restore(snapshot)?;
        Ok(ConditionalClipboardRestore::Restored)
    }

    /// Returns an OS clipboard generation when the platform exposes one.
    /// Content equality remains a secondary check, never the sole ownership
    /// signal on Windows where rich clipboard formats may differ.
    fn change_token(&mut self) -> Option<u64> {
        None
    }
}

const MAX_CLIPBOARD_IMAGE_BYTES: usize = 64 * 1024 * 1024;
const MAX_CLIPBOARD_TEXT_BYTES: usize = 16 * 1024 * 1024;
const DIBV5_HEADER_BYTES: usize = 124;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ConditionalClipboardWrite {
    Written(Option<u64>),
    Changed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConditionalClipboardRestore {
    Restored,
    Changed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WindowsClipboardFormat {
    format: u32,
    bytes: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClipboardImage {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

impl ClipboardImage {
    fn validate(&self) -> Result<()> {
        let byte_len = self
            .width
            .checked_mul(self.height)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| anyhow!("clipboard image dimensions overflow"))?;
        if self.width == 0 || self.height == 0 || byte_len != self.rgba.len() {
            return Err(anyhow!("clipboard image dimensions do not match RGBA data"));
        }
        if byte_len > MAX_CLIPBOARD_IMAGE_BYTES {
            return Err(anyhow!(
                "clipboard image exceeds the 64 MiB restoration limit"
            ));
        }
        Ok(())
    }
}

fn bounded_rgba_dimensions(width: u32, height: u32) -> bool {
    width != 0
        && height != 0
        && (width as usize)
            .checked_mul(height as usize)
            .and_then(|pixels| pixels.checked_mul(4))
            .is_some_and(|bytes| bytes <= MAX_CLIPBOARD_IMAGE_BYTES)
}

fn png_payload_is_bounded(payload_len: usize, header: &[u8]) -> bool {
    const PNG_SIGNATURE: [u8; 8] = [137, 80, 78, 71, 13, 10, 26, 10];

    (24..=MAX_CLIPBOARD_IMAGE_BYTES).contains(&payload_len)
        && header.len() >= 24
        && header[..8] == PNG_SIGNATURE
        && header[8..12] == [0, 0, 0, 13]
        && header[12..16] == *b"IHDR"
        && bounded_rgba_dimensions(
            u32::from_be_bytes(header[16..20].try_into().expect("PNG width is four bytes")),
            u32::from_be_bytes(header[20..24].try_into().expect("PNG height is four bytes")),
        )
}

fn dibv5_payload_is_bounded(payload_len: usize, header: &[u8]) -> bool {
    if !(DIBV5_HEADER_BYTES..=DIBV5_HEADER_BYTES + MAX_CLIPBOARD_IMAGE_BYTES).contains(&payload_len)
        || header.len() < DIBV5_HEADER_BYTES
        || u32::from_le_bytes(header[..4].try_into().expect("DIB size is four bytes"))
            != DIBV5_HEADER_BYTES as u32
    {
        return false;
    }

    let width = i32::from_le_bytes(header[4..8].try_into().expect("DIB width is four bytes"));
    let height = i32::from_le_bytes(header[8..12].try_into().expect("DIB height is four bytes"));
    width > 0 && bounded_rgba_dimensions(width as u32, height.unsigned_abs())
}

#[cfg_attr(all(target_os = "windows", not(test)), allow(dead_code))]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ClipboardSnapshot {
    Empty,
    Text(String),
    Image(ClipboardImage),
    WindowsFormats(Vec<WindowsClipboardFormat>),
    /// The platform exposed clipboard access but not a restorable format.
    Unsupported,
    /// The clipboard is temporarily unavailable (for example, owned by another process).
    Unavailable,
}

pub trait PasteDriver {
    fn paste(&mut self) -> Result<()>;
}

pub trait ForegroundTargetVerifier {
    fn reactivate_and_verify_captured_target(&mut self, target: &CapturedTarget) -> bool;
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
    let mut timing = TextOutputTiming::default();
    write_text_with_authorization(
        clipboard,
        paste,
        text,
        options,
        paste_available,
        || true,
        &mut timing,
    )
}

#[cfg_attr(not(test), allow(dead_code))]
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
    write_text_to_captured_target_with_drivers_outcome(
        clipboard,
        paste,
        verifier,
        text,
        options,
        paste_available,
        target,
    )
    .result
}

fn write_text_to_captured_target_with_drivers_outcome<C, P, V>(
    clipboard: &mut C,
    paste: &mut P,
    verifier: &mut V,
    text: &str,
    options: TextOutputOptions,
    paste_available: bool,
    target: Option<&CapturedTarget>,
) -> TextOutputOutcome
where
    C: ClipboardDriver,
    P: PasteDriver,
    V: ForegroundTargetVerifier,
{
    let mut timing = TextOutputTiming::default();
    let result = write_text_with_authorization(
        clipboard,
        paste,
        text,
        options,
        paste_available,
        || target.is_some_and(|target| verifier.reactivate_and_verify_captured_target(target)),
        &mut timing,
    );
    TextOutputOutcome { result, timing }
}

fn write_text_with_authorization<C, P, F>(
    clipboard: &mut C,
    paste: &mut P,
    text: &str,
    options: TextOutputOptions,
    paste_available: bool,
    mut paste_is_authorized: F,
    timing: &mut TextOutputTiming,
) -> TextOutputResult
where
    C: ClipboardDriver,
    P: PasteDriver,
    F: FnMut() -> bool,
{
    if text.trim().is_empty() {
        return TextOutputResult::Failed("transcript is empty".to_owned());
    }
    if !paste_available {
        return copy_without_paste(clipboard, text, CopyOnlyReason::AutomationUnavailable);
    }

    let mut original_clipboard_token = None;
    let previous_clipboard = if options.restore_clipboard_after_insert {
        let snapshot_token = clipboard.change_token();
        if clipboard_ownership_token_required() && snapshot_token.is_none() {
            return copy_without_paste(
                clipboard,
                text,
                CopyOnlyReason::ClipboardSnapshotUnavailable,
            );
        }
        original_clipboard_token = snapshot_token;
        match clipboard.snapshot() {
            Ok(
                snapshot @ (ClipboardSnapshot::Empty
                | ClipboardSnapshot::Text(_)
                | ClipboardSnapshot::Image(_)
                | ClipboardSnapshot::WindowsFormats(_)),
            ) => {
                if let ClipboardSnapshot::Image(image) = &snapshot
                    && image.validate().is_err()
                {
                    return copy_without_paste(
                        clipboard,
                        text,
                        CopyOnlyReason::ClipboardSnapshotError,
                    );
                }
                if snapshot_token.is_some() && clipboard.change_token() != snapshot_token {
                    return TextOutputResult::NotInserted(NotInsertedReason::ClipboardChanged);
                }
                Some(snapshot)
            }
            Ok(ClipboardSnapshot::Unavailable) => {
                return copy_without_paste(
                    clipboard,
                    text,
                    CopyOnlyReason::ClipboardSnapshotUnavailable,
                );
            }
            Ok(ClipboardSnapshot::Unsupported) => {
                return copy_without_paste(
                    clipboard,
                    text,
                    CopyOnlyReason::ClipboardSnapshotUnsupported,
                );
            }
            Err(_) => {
                return copy_without_paste(clipboard, text, CopyOnlyReason::ClipboardSnapshotError);
            }
        }
    } else {
        None
    };

    let owned_clipboard_token =
        match clipboard.set_text_if_token(original_clipboard_token, text.to_owned()) {
            Ok(ConditionalClipboardWrite::Written(token)) => token,
            Ok(ConditionalClipboardWrite::Changed) => {
                return TextOutputResult::NotInserted(NotInsertedReason::ClipboardChanged);
            }
            Err(err) => return TextOutputResult::Failed(err.to_string()),
        };
    let Some(owned_clipboard_token) = owned_clipboard_token else {
        return TextOutputResult::CopiedOnly(CopyOnlyReason::ClipboardSnapshotUnavailable);
    };

    sleep_for_paste_delay(options.paste_delay_ms);
    if !clipboard.contents_match_token(owned_clipboard_token, text) {
        return TextOutputResult::NotInserted(NotInsertedReason::ClipboardChanged);
    }
    // Keep activation and validation adjacent to synthetic input. Windows may
    // deny normal SetForegroundWindow activation; that becomes clipboard-only.
    if !paste_is_authorized() {
        return TextOutputResult::CopiedOnly(CopyOnlyReason::TargetUnavailable);
    }
    timing.target_activated_at = Some(Instant::now());
    // Foreground activation can synchronously wake clipboard managers. Verify
    // ownership again after activation and immediately before input dispatch.
    if !clipboard.contents_match_token(owned_clipboard_token, text) {
        return TextOutputResult::NotInserted(NotInsertedReason::ClipboardChanged);
    }
    if paste.paste().is_err() {
        return TextOutputResult::CopiedOnly(CopyOnlyReason::PasteFailed);
    }
    timing.paste_completed_at = Some(Instant::now());

    sleep_for_paste_delay(options.paste_delay_ms);
    if let Some(previous_clipboard) = previous_clipboard {
        match clipboard.restore_if_token(owned_clipboard_token, text, previous_clipboard) {
            Ok(ConditionalClipboardRestore::Restored | ConditionalClipboardRestore::Changed) => {}
            Err(err) => {
                return TextOutputResult::InsertedClipboardRestoreFailed(err.to_string());
            }
        }
    }

    TextOutputResult::Inserted
}

fn copy_without_paste<C>(clipboard: &mut C, text: &str, reason: CopyOnlyReason) -> TextOutputResult
where
    C: ClipboardDriver,
{
    match clipboard.set_text(text.to_owned()) {
        Ok(()) => TextOutputResult::CopiedOnly(reason),
        Err(err) => TextOutputResult::Failed(err.to_string()),
    }
}

fn sleep_for_paste_delay(delay_ms: u64) {
    if delay_ms > 0 {
        thread::sleep(Duration::from_millis(delay_ms));
    }
}

fn paste_automation_available() -> bool {
    #[cfg(target_os = "windows")]
    {
        true
    }

    #[cfg(not(target_os = "windows"))]
    {
        false
    }
}

fn clipboard_ownership_token_required() -> bool {
    cfg!(target_os = "windows")
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
    fn snapshot(&mut self) -> Result<ClipboardSnapshot> {
        #[cfg(target_os = "windows")]
        {
            snapshot_windows_clipboard()
        }

        #[cfg(not(target_os = "windows"))]
        match self.clipboard.get_text() {
            Ok(text) => Ok(ClipboardSnapshot::Text(text)),
            Err(ClipboardError::ContentNotAvailable) => match self.clipboard.get_image() {
                Ok(image) => {
                    let image = ClipboardImage {
                        width: image.width,
                        height: image.height,
                        rgba: image.bytes.into_owned(),
                    };
                    image.validate()?;
                    Ok(ClipboardSnapshot::Image(image))
                }
                Err(ClipboardError::ContentNotAvailable) => {
                    if system_clipboard_has_any_format() == Some(false) {
                        Ok(ClipboardSnapshot::Empty)
                    } else {
                        Ok(ClipboardSnapshot::Unsupported)
                    }
                }
                Err(ClipboardError::ClipboardNotSupported) => Ok(ClipboardSnapshot::Unsupported),
                Err(ClipboardError::ClipboardOccupied) => Ok(ClipboardSnapshot::Unavailable),
                Err(err) => Err(anyhow!(err).context("failed to read clipboard image")),
            },
            Err(ClipboardError::ClipboardNotSupported) => Ok(ClipboardSnapshot::Unsupported),
            Err(ClipboardError::ClipboardOccupied) => Ok(ClipboardSnapshot::Unavailable),
            Err(err) => Err(anyhow!(err).context("failed to read clipboard text")),
        }
    }

    fn restore(&mut self, snapshot: ClipboardSnapshot) -> Result<()> {
        match snapshot {
            ClipboardSnapshot::Empty => self.clipboard.clear().context("failed to clear clipboard"),
            ClipboardSnapshot::Text(text) => self.set_text(text),
            ClipboardSnapshot::Image(image) => {
                image.validate()?;
                self.clipboard
                    .set_image(ImageData {
                        width: image.width,
                        height: image.height,
                        bytes: Cow::Owned(image.rgba),
                    })
                    .context("failed to restore clipboard image")
            }
            ClipboardSnapshot::WindowsFormats(formats) => {
                #[cfg(target_os = "windows")]
                {
                    restore_windows_formats(formats)
                }
                #[cfg(not(target_os = "windows"))]
                {
                    let _ = formats;
                    Err(anyhow!("Windows clipboard formats are unavailable"))
                }
            }
            ClipboardSnapshot::Unsupported | ClipboardSnapshot::Unavailable => {
                Err(anyhow!("clipboard snapshot is not restorable"))
            }
        }
    }

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

    fn set_text_if_token(
        &mut self,
        expected_token: Option<u64>,
        text: String,
    ) -> Result<ConditionalClipboardWrite> {
        #[cfg(target_os = "windows")]
        {
            set_windows_text_if_token(expected_token, &text)
        }
        #[cfg(not(target_os = "windows"))]
        {
            if expected_token.is_some() && self.change_token() != expected_token {
                return Ok(ConditionalClipboardWrite::Changed);
            }
            self.set_text(text)?;
            Ok(ConditionalClipboardWrite::Written(self.change_token()))
        }
    }

    fn contents_match_token(&mut self, expected_token: u64, text: &str) -> bool {
        #[cfg(target_os = "windows")]
        {
            windows_text_matches_token(expected_token, text)
        }
        #[cfg(not(target_os = "windows"))]
        {
            self.change_token() == Some(expected_token)
                && self.get_text().ok().as_deref() == Some(text)
        }
    }

    fn restore_if_token(
        &mut self,
        expected_token: u64,
        expected_text: &str,
        snapshot: ClipboardSnapshot,
    ) -> Result<ConditionalClipboardRestore> {
        #[cfg(target_os = "windows")]
        {
            restore_windows_clipboard_if_token(expected_token, expected_text, snapshot)
        }
        #[cfg(not(target_os = "windows"))]
        {
            if !self.contents_match_token(expected_token, expected_text) {
                return Ok(ConditionalClipboardRestore::Changed);
            }
            self.restore(snapshot)?;
            Ok(ConditionalClipboardRestore::Restored)
        }
    }

    fn change_token(&mut self) -> Option<u64> {
        system_clipboard_change_token()
    }
}

#[cfg(target_os = "windows")]
fn system_clipboard_change_token() -> Option<u64> {
    use windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber;

    nonzero_clipboard_token(unsafe { GetClipboardSequenceNumber() })
}

#[cfg(target_os = "windows")]
const CF_TEXT: u32 = 1;
#[cfg(target_os = "windows")]
const CF_BITMAP: u32 = 2;
#[cfg(target_os = "windows")]
const CF_OEMTEXT: u32 = 7;
#[cfg(target_os = "windows")]
const CF_DIB: u32 = 8;
#[cfg(target_os = "windows")]
const CF_PALETTE: u32 = 9;
#[cfg(target_os = "windows")]
const CF_UNICODETEXT: u32 = 13;
#[cfg(target_os = "windows")]
const CF_LOCALE: u32 = 16;
#[cfg(target_os = "windows")]
const CF_DIBV5: u32 = 17;
#[cfg(target_os = "windows")]
const MAX_WINDOWS_SNAPSHOT_BYTES: usize = MAX_CLIPBOARD_IMAGE_BYTES + MAX_CLIPBOARD_TEXT_BYTES;

#[cfg(target_os = "windows")]
fn windows_png_format() -> Result<u32> {
    use windows_sys::Win32::System::DataExchange::RegisterClipboardFormatW;

    const PNG_FORMAT_NAME: [u16; 4] = [b'P' as u16, b'N' as u16, b'G' as u16, 0];
    let format = unsafe { RegisterClipboardFormatW(PNG_FORMAT_NAME.as_ptr()) };
    (format != 0)
        .then_some(format)
        .ok_or_else(|| anyhow!("failed to register the Windows PNG clipboard format"))
}

#[cfg(target_os = "windows")]
fn with_open_windows_clipboard<T>(operation: impl FnOnce() -> Result<T>) -> Result<T> {
    use windows_sys::Win32::System::DataExchange::{CloseClipboard, OpenClipboard};

    let owner = windows_clipboard_owner_window()
        .ok_or_else(|| anyhow!("failed to create the Windows clipboard owner window"))?;
    if unsafe { OpenClipboard(owner) } == 0 {
        return Err(anyhow!("Windows clipboard is unavailable"));
    }
    let result = operation();
    let closed = unsafe { CloseClipboard() } != 0;
    if !closed && result.is_ok() {
        return Err(anyhow!("failed to close the Windows clipboard"));
    }
    result
}

#[cfg(target_os = "windows")]
fn windows_clipboard_owner_window() -> Option<windows_sys::Win32::Foundation::HWND> {
    use std::cell::Cell;
    use windows_sys::Win32::UI::WindowsAndMessaging::{CreateWindowExW, HWND_MESSAGE, IsWindow};

    const STATIC_CLASS: [u16; 7] = [83, 84, 65, 84, 73, 67, 0];
    const OWNER_NAME: [u16; 24] = [
        83, 99, 114, 105, 98, 101, 32, 67, 108, 105, 112, 98, 111, 97, 114, 100, 32, 79, 119, 110,
        101, 114, 0, 0,
    ];

    thread_local! {
        static CLIPBOARD_OWNER: Cell<isize> = const { Cell::new(0) };
    }

    CLIPBOARD_OWNER.with(|slot| {
        let existing = slot.get() as windows_sys::Win32::Foundation::HWND;
        if !existing.is_null() && unsafe { IsWindow(existing) } != 0 {
            return Some(existing);
        }
        let window = unsafe {
            CreateWindowExW(
                0,
                STATIC_CLASS.as_ptr(),
                OWNER_NAME.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        if window.is_null() {
            None
        } else {
            slot.set(window as isize);
            Some(window)
        }
    })
}

#[cfg(target_os = "windows")]
fn snapshot_windows_clipboard() -> Result<ClipboardSnapshot> {
    use windows_sys::Win32::Foundation::{GetLastError, SetLastError};
    use windows_sys::Win32::System::DataExchange::EnumClipboardFormats;

    let png_format = windows_png_format()?;
    with_open_windows_clipboard(|| {
        let mut current = 0;
        let mut available_formats = Vec::new();
        loop {
            unsafe { SetLastError(0) };
            let next = unsafe { EnumClipboardFormats(current) };
            if next == 0 {
                if unsafe { GetLastError() } != 0 {
                    return Err(anyhow!("failed to enumerate Windows clipboard formats"));
                }
                break;
            }
            available_formats.push(next);
            current = next;
        }

        let bitmap_source = windows_bitmap_source(&available_formats);
        if bitmap_source.is_some() && bitmap_source != Some(CF_DIBV5) {
            return Ok(ClipboardSnapshot::Unsupported);
        }
        let text_source = available_formats
            .iter()
            .copied()
            .find(|format| matches!(*format, CF_TEXT | CF_OEMTEXT | CF_UNICODETEXT));
        let mut total_bytes = 0usize;
        let mut formats = Vec::new();
        for format in available_formats {
            if windows_format_is_synthesized_bitmap(format, bitmap_source) {
                continue;
            }
            if matches!(format, CF_TEXT | CF_OEMTEXT | CF_UNICODETEXT)
                && Some(format) != text_source
            {
                continue;
            }
            if !windows_format_is_restorable(format, png_format) {
                return Ok(ClipboardSnapshot::Unsupported);
            }
            let bytes = read_open_windows_format(format, png_format)?;
            total_bytes = total_bytes
                .checked_add(bytes.len())
                .filter(|total| *total <= MAX_WINDOWS_SNAPSHOT_BYTES)
                .ok_or_else(|| anyhow!("Windows clipboard snapshot exceeds the 80 MiB limit"))?;
            formats.push(WindowsClipboardFormat { format, bytes });
        }
        if formats.is_empty() {
            Ok(ClipboardSnapshot::Empty)
        } else {
            Ok(ClipboardSnapshot::WindowsFormats(formats))
        }
    })
}

#[cfg(target_os = "windows")]
fn windows_format_is_restorable(format: u32, png_format: u32) -> bool {
    matches!(
        format,
        CF_TEXT | CF_OEMTEXT | CF_UNICODETEXT | CF_LOCALE | CF_DIBV5
    ) || format == png_format
}

#[cfg(target_os = "windows")]
fn windows_bitmap_source(formats: &[u32]) -> Option<u32> {
    formats
        .iter()
        .copied()
        .find(|format| matches!(*format, CF_BITMAP | CF_DIB | CF_PALETTE | CF_DIBV5))
}

#[cfg(target_os = "windows")]
fn windows_format_is_synthesized_bitmap(format: u32, bitmap_source: Option<u32>) -> bool {
    bitmap_source == Some(CF_DIBV5) && matches!(format, CF_BITMAP | CF_DIB | CF_PALETTE)
}

#[cfg(target_os = "windows")]
fn read_open_windows_format(format: u32, png_format: u32) -> Result<Vec<u8>> {
    use windows_sys::Win32::System::DataExchange::GetClipboardData;
    use windows_sys::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};

    let handle = unsafe { GetClipboardData(format) };
    if handle.is_null() {
        return Err(anyhow!("Windows clipboard format {format} is unavailable"));
    }
    let payload_len = unsafe { GlobalSize(handle) };
    if payload_len == 0 || payload_len > MAX_WINDOWS_SNAPSHOT_BYTES {
        return Err(anyhow!(
            "Windows clipboard format {format} has an unsafe size"
        ));
    }
    let data = unsafe { GlobalLock(handle) };
    if data.is_null() {
        return Err(anyhow!("failed to lock Windows clipboard format {format}"));
    }
    let bytes = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), payload_len) }.to_vec();
    let _ = unsafe { GlobalUnlock(handle) };
    validate_windows_format(format, png_format, &bytes)?;
    Ok(bytes)
}

#[cfg(target_os = "windows")]
fn validate_windows_format(format: u32, png_format: u32, bytes: &[u8]) -> Result<()> {
    let valid = if format == png_format {
        png_payload_is_bounded(bytes.len(), bytes)
    } else if format == CF_DIBV5 {
        dibv5_payload_is_bounded(bytes.len(), bytes)
    } else if format == CF_UNICODETEXT {
        bytes.len() <= MAX_CLIPBOARD_TEXT_BYTES
            && bytes.len().is_multiple_of(2)
            && bytes.ends_with(&[0, 0])
    } else if matches!(format, CF_TEXT | CF_OEMTEXT) {
        bytes.len() <= MAX_CLIPBOARD_TEXT_BYTES && bytes.last() == Some(&0)
    } else {
        format == CF_LOCALE && bytes.len() == size_of::<u32>()
    };
    valid.then_some(()).ok_or_else(|| {
        anyhow!("Windows clipboard format {format} has an unsupported header or unsafe size")
    })
}

#[cfg(target_os = "windows")]
fn windows_clipboard_token_open() -> Option<u64> {
    use windows_sys::Win32::System::DataExchange::GetClipboardSequenceNumber;

    nonzero_clipboard_token(unsafe { GetClipboardSequenceNumber() })
}

#[cfg(target_os = "windows")]
fn nonzero_clipboard_token(token: u32) -> Option<u64> {
    (token != 0).then_some(u64::from(token))
}

#[cfg(target_os = "windows")]
fn windows_text_matches_token(expected_token: u64, expected_text: &str) -> bool {
    with_open_windows_clipboard(|| {
        Ok(windows_clipboard_token_open() == Some(expected_token)
            && read_open_windows_unicode_text().is_ok_and(|text| text == expected_text))
    })
    .unwrap_or(false)
}

#[cfg(target_os = "windows")]
fn read_open_windows_unicode_text() -> Result<String> {
    let bytes = read_open_windows_format(CF_UNICODETEXT, windows_png_format()?)?;
    let units = bytes
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .take_while(|unit| *unit != 0)
        .collect::<Vec<_>>();
    String::from_utf16(&units).context("Windows clipboard text is not valid UTF-16")
}

#[cfg(target_os = "windows")]
fn encode_windows_unicode_text(text: &str) -> Vec<u8> {
    text.encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect()
}

#[cfg(target_os = "windows")]
fn set_windows_text_if_token(
    expected_token: Option<u64>,
    text: &str,
) -> Result<ConditionalClipboardWrite> {
    let format = WindowsClipboardFormat {
        format: CF_UNICODETEXT,
        bytes: encode_windows_unicode_text(text),
    };
    with_open_windows_clipboard(|| {
        if expected_token.is_some() && windows_clipboard_token_open() != expected_token {
            return Ok(ConditionalClipboardWrite::Changed);
        }
        replace_open_windows_clipboard(&[format])?;
        Ok(ConditionalClipboardWrite::Written(
            windows_clipboard_token_open(),
        ))
    })
}

#[cfg(target_os = "windows")]
fn restore_windows_clipboard_if_token(
    expected_token: u64,
    expected_text: &str,
    snapshot: ClipboardSnapshot,
) -> Result<ConditionalClipboardRestore> {
    with_open_windows_clipboard(|| {
        if windows_clipboard_token_open() != Some(expected_token)
            || !read_open_windows_unicode_text().is_ok_and(|text| text == expected_text)
        {
            return Ok(ConditionalClipboardRestore::Changed);
        }
        match snapshot {
            ClipboardSnapshot::Empty => replace_open_windows_clipboard(&[])?,
            ClipboardSnapshot::WindowsFormats(formats) => replace_open_windows_clipboard(&formats)?,
            ClipboardSnapshot::Text(text) => {
                replace_open_windows_clipboard(&[WindowsClipboardFormat {
                    format: CF_UNICODETEXT,
                    bytes: encode_windows_unicode_text(&text),
                }])?
            }
            ClipboardSnapshot::Image(_)
            | ClipboardSnapshot::Unsupported
            | ClipboardSnapshot::Unavailable => {
                return Err(anyhow!(
                    "clipboard snapshot is not a native Windows transaction"
                ));
            }
        }
        Ok(ConditionalClipboardRestore::Restored)
    })
}

#[cfg(target_os = "windows")]
fn restore_windows_formats(formats: Vec<WindowsClipboardFormat>) -> Result<()> {
    with_open_windows_clipboard(|| replace_open_windows_clipboard(&formats))
}

#[cfg(target_os = "windows")]
fn replace_open_windows_clipboard(formats: &[WindowsClipboardFormat]) -> Result<()> {
    use windows_sys::Win32::Foundation::GlobalFree;
    use windows_sys::Win32::System::DataExchange::{EmptyClipboard, SetClipboardData};
    use windows_sys::Win32::System::Memory::{GHND, GlobalAlloc, GlobalLock, GlobalUnlock};

    let png_format = windows_png_format()?;
    let mut allocations = Vec::with_capacity(formats.len());
    for format in formats {
        validate_windows_format(format.format, png_format, &format.bytes)?;
        let handle = unsafe { GlobalAlloc(GHND, format.bytes.len()) };
        if handle.is_null() {
            for (_, allocation) in allocations.drain(..) {
                unsafe { GlobalFree(allocation) };
            }
            return Err(anyhow!("failed to allocate Windows clipboard memory"));
        }
        let destination = unsafe { GlobalLock(handle) };
        if destination.is_null() {
            unsafe { GlobalFree(handle) };
            for (_, allocation) in allocations.drain(..) {
                unsafe { GlobalFree(allocation) };
            }
            return Err(anyhow!("failed to lock Windows clipboard memory"));
        }
        unsafe {
            std::ptr::copy_nonoverlapping(
                format.bytes.as_ptr(),
                destination.cast::<u8>(),
                format.bytes.len(),
            );
            let _ = GlobalUnlock(handle);
        }
        allocations.push((format.format, handle));
    }

    if unsafe { EmptyClipboard() } == 0 {
        for (_, allocation) in allocations {
            unsafe { GlobalFree(allocation) };
        }
        return Err(anyhow!("failed to empty the Windows clipboard"));
    }
    for index in 0..allocations.len() {
        let (format, allocation) = allocations[index];
        if unsafe { SetClipboardData(format, allocation) }.is_null() {
            for (_, remaining) in allocations.drain(index..) {
                unsafe { GlobalFree(remaining) };
            }
            return Err(anyhow!(
                "failed to restore Windows clipboard format {format}"
            ));
        }
    }
    Ok(())
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
    fn reactivate_and_verify_captured_target(&mut self, target: &CapturedTarget) -> bool {
        #[cfg(target_os = "windows")]
        {
            reactivate_and_verify_captured_target(target)
        }
        #[cfg(not(target_os = "windows"))]
        {
            captured_target_is_foreground(target)
        }
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
            Err(anyhow!("verified paste automation is unavailable"))
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
        // release Control first, then V, using individual checked calls. A
        // failed key-up is retried once; the paste chord itself is never sent
        // again.
        let control_up = keyboard_input(VK_CONTROL, KEYEVENTF_KEYUP);
        let v_up = keyboard_input(VIRTUAL_KEY_V, KEYEVENTF_KEYUP);
        let release_key = |send: &mut dyn FnMut(&[INPUT]) -> u32, input: &INPUT| {
            send(std::slice::from_ref(input)) == 1 || send(std::slice::from_ref(input)) == 1
        };
        let control_released = release_key(&mut send, &control_up);
        let v_released = release_key(&mut send, &v_up);
        return Err(anyhow!(
            "Windows accepted {inserted} of {} paste input events; cleanup control_up={control_released}, v_up={v_released}",
            inputs.len(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FakeClipboard {
        text: Option<String>,
        snapshot: Option<ClipboardSnapshot>,
        token: Option<u64>,
        force_token_unavailable: bool,
        fail_get: bool,
        fail_set: bool,
        fail_restore: bool,
        set_history: Vec<String>,
    }

    impl ClipboardDriver for FakeClipboard {
        fn snapshot(&mut self) -> Result<ClipboardSnapshot> {
            self.snapshot
                .clone()
                .or_else(|| self.text.clone().map(ClipboardSnapshot::Text))
                .ok_or_else(|| anyhow!("snapshot failed"))
        }

        fn restore(&mut self, snapshot: ClipboardSnapshot) -> Result<()> {
            if self.fail_restore {
                return Err(anyhow!("restore failed"));
            }
            self.snapshot = Some(snapshot.clone());
            self.text = match snapshot {
                ClipboardSnapshot::Text(text) => Some(text),
                ClipboardSnapshot::Empty
                | ClipboardSnapshot::Image(_)
                | ClipboardSnapshot::WindowsFormats(_) => None,
                ClipboardSnapshot::Unsupported | ClipboardSnapshot::Unavailable => {
                    return Err(anyhow!("invalid restore snapshot"));
                }
            };
            Ok(())
        }

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

        fn change_token(&mut self) -> Option<u64> {
            (!self.force_token_unavailable).then_some(self.token.unwrap_or(1))
        }
    }

    #[derive(Default)]
    struct FakePaste {
        calls: usize,
        fail: bool,
    }

    struct FakeTargetVerifier {
        initially_foreground: bool,
        reactivation_succeeds: bool,
        calls: usize,
    }

    impl ForegroundTargetVerifier for FakeTargetVerifier {
        fn reactivate_and_verify_captured_target(&mut self, _target: &CapturedTarget) -> bool {
            self.calls += 1;
            self.reactivation_succeeds
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

    fn png_header(width: u32, height: u32) -> [u8; 24] {
        let mut header = [0_u8; 24];
        header[..8].copy_from_slice(&[137, 80, 78, 71, 13, 10, 26, 10]);
        header[8..12].copy_from_slice(&13_u32.to_be_bytes());
        header[12..16].copy_from_slice(b"IHDR");
        header[16..20].copy_from_slice(&width.to_be_bytes());
        header[20..24].copy_from_slice(&height.to_be_bytes());
        header
    }

    fn dibv5_header(width: i32, height: i32) -> [u8; DIBV5_HEADER_BYTES] {
        let mut header = [0_u8; DIBV5_HEADER_BYTES];
        header[..4].copy_from_slice(&(DIBV5_HEADER_BYTES as u32).to_le_bytes());
        header[4..8].copy_from_slice(&width.to_le_bytes());
        header[8..12].copy_from_slice(&height.to_le_bytes());
        header
    }

    #[test]
    fn native_image_payload_validation_accepts_bounded_png_and_dibv5_dimensions() {
        let png = png_header(4096, 4096);
        let dib = dibv5_header(4096, -4096);

        assert!(png_payload_is_bounded(1024, &png));
        assert!(dibv5_payload_is_bounded(DIBV5_HEADER_BYTES + 1024, &dib));
    }

    #[test]
    fn native_image_payload_validation_rejects_overflow_and_oversized_dimensions() {
        let extreme_png = png_header(u32::MAX, u32::MAX);
        let oversized_dib = dibv5_header(8193, 8192);

        assert!(!png_payload_is_bounded(1024, &extreme_png));
        assert!(!dibv5_payload_is_bounded(
            DIBV5_HEADER_BYTES + 1024,
            &oversized_dib
        ));
    }

    #[test]
    fn native_image_payload_validation_rejects_oversized_and_malformed_data() {
        let png = png_header(1, 1);
        let mut malformed_dib = dibv5_header(1, 1);
        malformed_dib[..4].copy_from_slice(&40_u32.to_le_bytes());

        assert!(!png_payload_is_bounded(MAX_CLIPBOARD_IMAGE_BYTES + 1, &png));
        assert!(!dibv5_payload_is_bounded(
            DIBV5_HEADER_BYTES + MAX_CLIPBOARD_IMAGE_BYTES + 1,
            &dibv5_header(1, 1)
        ));
        assert!(!dibv5_payload_is_bounded(
            DIBV5_HEADER_BYTES,
            &malformed_dib
        ));
        assert!(!png_payload_is_bounded(24, &[0; 24]));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn zero_windows_clipboard_sequence_is_unverifiable() {
        assert_eq!(nonzero_clipboard_token(0), None);
        assert_eq!(nonzero_clipboard_token(17), Some(17));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_format_allowlist_preserves_known_companions_and_rejects_rich_data() {
        let png_format = 0xC001;
        for format in [
            CF_TEXT,
            CF_OEMTEXT,
            CF_UNICODETEXT,
            CF_LOCALE,
            CF_DIBV5,
            png_format,
        ] {
            assert!(windows_format_is_restorable(format, png_format));
        }
        for format in [CF_BITMAP, CF_DIB, CF_PALETTE] {
            assert!(windows_format_is_synthesized_bitmap(format, Some(CF_DIBV5)));
            assert!(!windows_format_is_synthesized_bitmap(
                format,
                Some(CF_BITMAP)
            ));
        }
        assert_eq!(
            windows_bitmap_source(&[CF_DIBV5, CF_BITMAP, CF_DIB]),
            Some(CF_DIBV5)
        );
        assert_eq!(
            windows_bitmap_source(&[CF_BITMAP, CF_DIBV5]),
            Some(CF_BITMAP)
        );
        assert!(!windows_format_is_restorable(0xC002, png_format));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_text_payload_validation_is_bounded_and_terminated() {
        let png_format = 0xC001;
        assert!(validate_windows_format(CF_UNICODETEXT, png_format, &[65, 0, 0, 0]).is_ok());
        assert!(validate_windows_format(CF_UNICODETEXT, png_format, &[65, 0]).is_err());
        assert!(validate_windows_format(CF_LOCALE, png_format, &[0; 4]).is_ok());
        assert!(validate_windows_format(CF_LOCALE, png_format, &[0; 8]).is_err());
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
        assert_eq!(clipboard.set_history, vec!["hello"]);
    }

    #[test]
    fn successful_paste_restores_image_and_empty_snapshots() {
        for snapshot in [
            ClipboardSnapshot::Empty,
            ClipboardSnapshot::Image(ClipboardImage {
                width: 1,
                height: 1,
                rgba: vec![1, 2, 3, 4],
            }),
        ] {
            let mut clipboard = FakeClipboard {
                text: Some("before".to_owned()),
                snapshot: Some(snapshot.clone()),
                ..FakeClipboard::default()
            };
            let mut paste = FakePaste::default();

            let result =
                write_text_with_drivers(&mut clipboard, &mut paste, "hello", fast_options(), true);

            assert_eq!(result, TextOutputResult::Inserted);
            assert_eq!(clipboard.snapshot, Some(snapshot));
            assert_eq!(paste.calls, 1);
        }
    }

    #[test]
    fn unsafe_snapshot_copies_without_pasting() {
        for snapshot in [
            ClipboardSnapshot::Unavailable,
            ClipboardSnapshot::Unsupported,
        ] {
            let mut clipboard = FakeClipboard {
                text: Some("before".to_owned()),
                snapshot: Some(snapshot),
                ..FakeClipboard::default()
            };
            let mut paste = FakePaste::default();

            let result =
                write_text_with_drivers(&mut clipboard, &mut paste, "hello", fast_options(), true);

            assert!(matches!(result, TextOutputResult::CopiedOnly(_)));
            assert_eq!(clipboard.text.as_deref(), Some("hello"));
            assert_eq!(paste.calls, 0);
        }
    }

    #[test]
    fn unavailable_token_falls_back_to_copy_only_without_paste() {
        let mut clipboard = FakeClipboard {
            text: Some("before".to_owned()),
            force_token_unavailable: true,
            ..FakeClipboard::default()
        };
        let mut paste = FakePaste::default();

        let result =
            write_text_with_drivers(&mut clipboard, &mut paste, "hello", fast_options(), true);

        assert_eq!(
            result,
            TextOutputResult::CopiedOnly(CopyOnlyReason::ClipboardSnapshotUnavailable)
        );
        assert_eq!(paste.calls, 0);
    }

    #[test]
    fn clipboard_change_during_snapshot_is_not_overwritten() {
        struct SnapshotRaceClipboard {
            text: String,
            token: u64,
            set_calls: usize,
        }

        impl ClipboardDriver for SnapshotRaceClipboard {
            fn snapshot(&mut self) -> Result<ClipboardSnapshot> {
                let snapshot = ClipboardSnapshot::Text(self.text.clone());
                self.text = "external change".to_owned();
                self.token += 1;
                Ok(snapshot)
            }

            fn restore(&mut self, _snapshot: ClipboardSnapshot) -> Result<()> {
                panic!("snapshot race must never restore")
            }

            fn get_text(&mut self) -> Result<String> {
                Ok(self.text.clone())
            }

            fn set_text(&mut self, text: String) -> Result<()> {
                self.set_calls += 1;
                self.text = text;
                self.token += 1;
                Ok(())
            }

            fn change_token(&mut self) -> Option<u64> {
                Some(self.token)
            }
        }

        let mut clipboard = SnapshotRaceClipboard {
            text: "before".to_owned(),
            token: 10,
            set_calls: 0,
        };
        let mut paste = FakePaste::default();

        let result =
            write_text_with_drivers(&mut clipboard, &mut paste, "hello", fast_options(), true);

        assert_eq!(
            result,
            TextOutputResult::NotInserted(NotInsertedReason::ClipboardChanged)
        );
        assert_eq!(clipboard.text, "external change");
        assert_eq!(clipboard.set_calls, 0);
        assert_eq!(paste.calls, 0);
    }

    #[test]
    fn changed_token_with_same_text_prevents_paste() {
        struct ChangedTokenClipboard {
            text: String,
            token: u64,
            token_reads: usize,
        }

        impl ClipboardDriver for ChangedTokenClipboard {
            fn snapshot(&mut self) -> Result<ClipboardSnapshot> {
                Ok(ClipboardSnapshot::Text(self.text.clone()))
            }

            fn restore(&mut self, _snapshot: ClipboardSnapshot) -> Result<()> {
                Ok(())
            }

            fn get_text(&mut self) -> Result<String> {
                Ok(self.text.clone())
            }

            fn set_text(&mut self, text: String) -> Result<()> {
                self.text = text;
                self.token += 1;
                Ok(())
            }

            fn change_token(&mut self) -> Option<u64> {
                self.token_reads += 1;
                if self.token_reads == 2 {
                    self.token += 1;
                }
                Some(self.token)
            }
        }

        let mut clipboard = ChangedTokenClipboard {
            text: "before".to_owned(),
            token: 1,
            token_reads: 0,
        };
        let mut paste = FakePaste::default();

        let result =
            write_text_with_drivers(&mut clipboard, &mut paste, "hello", fast_options(), true);

        assert_eq!(
            result,
            TextOutputResult::NotInserted(NotInsertedReason::ClipboardChanged)
        );
        assert_eq!(paste.calls, 0);
    }

    #[test]
    fn restore_failure_is_reported_without_a_second_paste() {
        let mut clipboard = FakeClipboard {
            text: Some("before".to_owned()),
            fail_restore: true,
            ..FakeClipboard::default()
        };
        let mut paste = FakePaste::default();

        let result =
            write_text_with_drivers(&mut clipboard, &mut paste, "hello", fast_options(), true);

        assert!(matches!(
            result,
            TextOutputResult::InsertedClipboardRestoreFailed(_)
        ));
        assert_eq!(paste.calls, 1);
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
            TextOutputResult::CopiedOnly(CopyOnlyReason::ClipboardSnapshotError)
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
            initially_foreground: true,
            reactivation_succeeds: false,
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
    fn clipboard_change_during_target_activation_prevents_synthetic_input() {
        use std::cell::Cell;
        use std::rc::Rc;

        struct ActivationClipboard {
            text: String,
            changed: Rc<Cell<bool>>,
        }

        impl ClipboardDriver for ActivationClipboard {
            fn snapshot(&mut self) -> Result<ClipboardSnapshot> {
                Ok(ClipboardSnapshot::Text(self.text.clone()))
            }

            fn restore(&mut self, _snapshot: ClipboardSnapshot) -> Result<()> {
                Ok(())
            }

            fn get_text(&mut self) -> Result<String> {
                Ok(self.text.clone())
            }

            fn set_text(&mut self, text: String) -> Result<()> {
                self.text = text;
                Ok(())
            }

            fn change_token(&mut self) -> Option<u64> {
                Some(if self.changed.get() { 2 } else { 1 })
            }
        }

        struct ActivationMutatesClipboard(Rc<Cell<bool>>);

        impl ForegroundTargetVerifier for ActivationMutatesClipboard {
            fn reactivate_and_verify_captured_target(&mut self, _target: &CapturedTarget) -> bool {
                self.0.set(true);
                true
            }
        }

        let changed = Rc::new(Cell::new(false));
        let mut clipboard = ActivationClipboard {
            text: "before".to_owned(),
            changed: Rc::clone(&changed),
        };
        let mut verifier = ActivationMutatesClipboard(changed);
        let mut paste = FakePaste::default();
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
            TextOutputResult::NotInserted(NotInsertedReason::ClipboardChanged)
        );
        assert_eq!(paste.calls, 0);
    }

    #[test]
    fn missing_target_copies_without_pasting() {
        let mut clipboard = FakeClipboard {
            text: Some("before".to_owned()),
            ..FakeClipboard::default()
        };
        let mut paste = FakePaste::default();
        let mut verifier = FakeTargetVerifier {
            initially_foreground: true,
            reactivation_succeeds: true,
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
    fn reactivation_of_a_captured_target_pastes_exactly_once() {
        let mut clipboard = FakeClipboard {
            text: Some("before".to_owned()),
            ..FakeClipboard::default()
        };
        let mut paste = FakePaste::default();
        let mut verifier = FakeTargetVerifier {
            initially_foreground: false,
            reactivation_succeeds: true,
            calls: 0,
        };
        let target = CapturedTarget::for_test(11, 22);

        let outcome = write_text_to_captured_target_with_drivers_outcome(
            &mut clipboard,
            &mut paste,
            &mut verifier,
            "hello",
            fast_options(),
            true,
            Some(&target),
        );

        assert_eq!(outcome.result, TextOutputResult::Inserted);
        assert!(outcome.timing.target_activated_at.is_some());
        assert!(outcome.timing.paste_completed_at.is_some());
        assert!(
            outcome.timing.target_activated_at.unwrap()
                <= outcome.timing.paste_completed_at.unwrap()
        );
        assert!(!verifier.initially_foreground);
        assert_eq!(verifier.calls, 1);
        assert_eq!(paste.calls, 1);
        assert_eq!(clipboard.text.as_deref(), Some("before"));
    }

    #[test]
    fn independent_clipboard_change_is_not_overwritten_after_paste() {
        struct ClipboardChangedByPaste {
            text: String,
            reads: usize,
            token: u64,
        }

        impl ClipboardDriver for ClipboardChangedByPaste {
            fn snapshot(&mut self) -> Result<ClipboardSnapshot> {
                Ok(ClipboardSnapshot::Text(self.text.clone()))
            }

            fn restore(&mut self, snapshot: ClipboardSnapshot) -> Result<()> {
                self.text = match snapshot {
                    ClipboardSnapshot::Text(text) => text,
                    _ => return Err(anyhow!("unexpected snapshot")),
                };
                Ok(())
            }

            fn get_text(&mut self) -> Result<String> {
                self.reads += 1;
                if self.reads == 3 {
                    self.text = "user change".to_owned();
                    self.token += 1;
                }
                Ok(self.text.clone())
            }

            fn set_text(&mut self, text: String) -> Result<()> {
                self.text = text;
                Ok(())
            }

            fn change_token(&mut self) -> Option<u64> {
                Some(self.token)
            }
        }

        let mut clipboard = ClipboardChangedByPaste {
            text: "before".to_owned(),
            reads: 0,
            token: 1,
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
            fn snapshot(&mut self) -> Result<ClipboardSnapshot> {
                Ok(ClipboardSnapshot::Text(self.text.clone()))
            }

            fn restore(&mut self, snapshot: ClipboardSnapshot) -> Result<()> {
                self.text = match snapshot {
                    ClipboardSnapshot::Text(text) => text,
                    _ => return Err(anyhow!("unexpected snapshot")),
                };
                Ok(())
            }

            fn get_text(&mut self) -> Result<String> {
                self.reads += 1;
                if self.reads == 1 {
                    self.text = "user change".to_owned();
                }
                Ok(self.text.clone())
            }

            fn set_text(&mut self, text: String) -> Result<()> {
                self.text = text;
                Ok(())
            }

            fn change_token(&mut self) -> Option<u64> {
                Some(1)
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
            fn snapshot(&mut self) -> Result<ClipboardSnapshot> {
                Ok(ClipboardSnapshot::Text(self.text.clone()))
            }

            fn restore(&mut self, snapshot: ClipboardSnapshot) -> Result<()> {
                self.text = match snapshot {
                    ClipboardSnapshot::Text(text) => text,
                    _ => return Err(anyhow!("unexpected snapshot")),
                };
                Ok(())
            }

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
        assert_eq!(clipboard.text, "before");
        assert_eq!(paste.calls, 0);
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn every_partial_windows_input_batch_releases_control_then_v_individually() {
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
            assert_eq!(batches.len(), 3);
            assert_eq!(batches[1], vec![(VK_CONTROL, KEYEVENTF_KEYUP)]);
            assert_eq!(batches[2], vec![(0x56, KEYEVENTF_KEYUP)]);
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn failed_windows_key_release_cleanup_retries_only_key_up_events() {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{KEYEVENTF_KEYUP, VK_CONTROL};

        let mut batches = Vec::new();
        let result = send_windows_paste_chord_with(|inputs| {
            batches.push(
                inputs
                    .iter()
                    .map(|input| unsafe { (input.Anonymous.ki.wVk, input.Anonymous.ki.dwFlags) })
                    .collect::<Vec<_>>(),
            );
            if batches.len() == 1 { 1 } else { 0 }
        });

        assert!(result.is_err());
        assert_eq!(batches.len(), 5);
        assert_eq!(batches[1], vec![(VK_CONTROL, KEYEVENTF_KEYUP)]);
        assert_eq!(batches[2], vec![(VK_CONTROL, KEYEVENTF_KEYUP)]);
        assert_eq!(batches[3], vec![(0x56, KEYEVENTF_KEYUP)]);
        assert_eq!(batches[4], vec![(0x56, KEYEVENTF_KEYUP)]);
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
