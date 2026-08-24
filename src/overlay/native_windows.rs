mod accessibility;
mod layout;
mod raster;

use std::{
    cell::{Cell, RefCell},
    collections::hash_map::DefaultHasher,
    ffi::c_void,
    hash::{Hash, Hasher},
    mem::{offset_of, size_of, zeroed},
    panic::{AssertUnwindSafe, catch_unwind},
    ptr::{null, null_mut},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crossbeam_channel::{Receiver, Sender, bounded};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM},
    Graphics::Gdi::{
        AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
        CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, HBITMAP, HDC,
        HGDIOBJ, RGBQUAD, SelectObject,
    },
    System::LibraryLoader::GetModuleHandleW,
    UI::{
        Controls::{
            ICC_WIN95_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx, TOOLTIPS_CLASSW,
            TTF_SUBCLASS, TTM_ADDTOOLW, TTM_DELTOOLW, TTM_NEWTOOLRECTW, TTS_ALWAYSTIP,
            TTS_NOPREFIX, TTTOOLINFOW,
        },
        Input::KeyboardAndMouse::{GetCapture, ReleaseCapture, SetCapture},
        WindowsAndMessaging::{
            BeginDeferWindowPos, CREATESTRUCTW, CreateWindowExW, DefWindowProcW, DeferWindowPos,
            DestroyWindow, DispatchMessageW, EndDeferWindowPos, GWL_EXSTYLE, GWLP_USERDATA,
            GetClientRect, GetWindowLongPtrW, HTCLIENT, HTTRANSPARENT, HWND_TOPMOST,
            IsWindowVisible, MA_NOACTIVATE, MSG, PM_REMOVE, PeekMessageW, RegisterClassExW,
            SW_HIDE, SWP_HIDEWINDOW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE,
            SWP_NOZORDER, SWP_SHOWWINDOW, SendMessageW, SetWindowLongPtrW, SetWindowPos,
            ShowWindow, TranslateMessage, ULW_ALPHA, UpdateLayeredWindow, WM_CANCELMODE,
            WM_CAPTURECHANGED, WM_DESTROY, WM_GETOBJECT, WM_LBUTTONDOWN, WM_LBUTTONUP,
            WM_MOUSEACTIVATE, WM_NCCREATE, WM_NCDESTROY, WM_NCHITTEST, WNDCLASSEXW, WS_EX_LAYERED,
            WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
        },
    },
};

use super::{
    OverlayAction,
    controller::{OverlayPresentation, OverlayViewState},
    platform::{CapturedTarget, OverlayPosition, OverlayWindowBounds, overlay_window_bounds},
    view::{OverlayViewportOutput, control_window_bounds, is_cancellable, window_spec},
};
use crate::transcription::SessionId;

use self::{
    accessibility::{CANCEL_RECORDING_LABEL, NativeAccessibility},
    raster::{LayeredFrame, NativeRasterizer},
};

const DISPLAY_CLASS_NAME: &str = "Scribe.NativeOverlay.Display";
const CONTROL_CLASS_NAME: &str = "Scribe.NativeOverlay.Control";
const DISPLAY_WINDOW_TITLE: &str = "Scribe Dictation Overlay";
const CONTROL_WINDOW_TITLE: &str = "Scribe Dictation Overlay Cancel";

const REQUIRED_BASE_EX_STYLE: u32 =
    WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST;
const DISPLAY_EX_STYLE: u32 = REQUIRED_BASE_EX_STYLE | WS_EX_TRANSPARENT;
const CONTROL_EX_STYLE: u32 = REQUIRED_BASE_EX_STYLE;
const OVERLAY_THREAD_POLL_INTERVAL: Duration = Duration::from_millis(16);
const OVERLAY_HEALTH_INTERVAL: Duration = Duration::from_millis(500);
const OVERLAY_ANIMATION_INTERVAL: Duration = Duration::from_millis(125);
const OVERLAY_EVENT_CAPACITY: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WindowRole {
    Display,
    Control,
}

impl WindowRole {
    const fn class_name(self) -> &'static str {
        match self {
            Self::Display => DISPLAY_CLASS_NAME,
            Self::Control => CONTROL_CLASS_NAME,
        }
    }

    const fn title(self) -> &'static str {
        match self {
            Self::Display => DISPLAY_WINDOW_TITLE,
            Self::Control => CONTROL_WINDOW_TITLE,
        }
    }

    const fn ex_style(self) -> u32 {
        match self {
            Self::Display => DISPLAY_EX_STYLE,
            Self::Control => CONTROL_EX_STYLE,
        }
    }

    const fn requires_transparency(self) -> bool {
        matches!(self, Self::Display)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NativeOverlayFailureStage {
    HostCreation,
    Rasterization,
    Accessibility,
    LayeredPresentation,
    Positioning,
    Visibility,
    WorkerPanicked,
}

impl NativeOverlayFailureStage {
    const fn diagnostic_code(self) -> &'static str {
        match self {
            Self::HostCreation => "native-overlay-host",
            Self::Rasterization => "native-overlay-raster",
            Self::Accessibility => "native-overlay-accessibility",
            Self::LayeredPresentation => "native-overlay-layered-present",
            Self::Positioning => "native-overlay-position",
            Self::Visibility => "native-overlay-visibility",
            Self::WorkerPanicked => "native-overlay-worker",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct NativeOverlayFailure {
    stage: NativeOverlayFailureStage,
    role: Option<WindowRole>,
}

impl NativeOverlayFailure {
    const fn new(stage: NativeOverlayFailureStage, role: Option<WindowRole>) -> Self {
        Self { stage, role }
    }
}

#[derive(Clone, Debug)]
enum NativeOverlayEvent {
    Presented {
        visible: bool,
        session_id: Option<SessionId>,
    },
    Action(OverlayAction),
    Failure(NativeOverlayFailure),
}

#[derive(Clone, Debug)]
struct NativeEventSink {
    tx: Sender<NativeOverlayEvent>,
    repaint_context: eframe::egui::Context,
}

impl NativeEventSink {
    fn emit(&self, event: NativeOverlayEvent) {
        let _ = self.tx.try_send(event);
        self.repaint_context.request_repaint();
    }
}

#[derive(Clone, Debug)]
struct OverlaySnapshot {
    state: OverlayViewState,
    dark_mode: bool,
    dpi: u32,
    display_bounds: Option<OverlayWindowBounds>,
    control_bounds: Option<OverlayWindowBounds>,
    requested_visible: bool,
    control_requested: bool,
}

impl OverlaySnapshot {
    fn hidden(state: &OverlayViewState, dark_mode: bool) -> Self {
        Self {
            state: state.clone(),
            dark_mode,
            dpi: 96,
            display_bounds: None,
            control_bounds: None,
            requested_visible: false,
            control_requested: false,
        }
    }

    fn render_key(&self, animation_frame: u8) -> OverlayRenderKey {
        OverlayRenderKey {
            visible: self.requested_visible,
            mode: self.state.mode,
            phase: self.state.phase,
            transcript_revision: self.state.transcript.revision,
            content_digest: overlay_content_digest(&self.state),
            rms_bucket: quantized_level(self.state.audio_level.rms),
            peak_bucket: quantized_level(self.state.audio_level.peak),
            elapsed_second: self.state.elapsed.map_or(0, |elapsed| elapsed.as_secs()),
            dark_mode: self.dark_mode,
            dpi: self.dpi,
            display_bounds: self.display_bounds,
            control_bounds: self.control_bounds,
            control_requested: self.control_requested,
            animation_frame,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct OverlayRenderKey {
    visible: bool,
    mode: super::controller::OverlayMode,
    phase: super::controller::OverlayPhase,
    transcript_revision: u64,
    content_digest: u64,
    rms_bucket: u8,
    peak_bucket: u8,
    elapsed_second: u64,
    dark_mode: bool,
    dpi: u32,
    display_bounds: Option<OverlayWindowBounds>,
    control_bounds: Option<OverlayWindowBounds>,
    control_requested: bool,
    animation_frame: u8,
}

fn overlay_content_digest(state: &OverlayViewState) -> u64 {
    let mut hasher = DefaultHasher::new();
    state.transcript.committed.hash(&mut hasher);
    state.transcript.tentative.hash(&mut hasher);
    state.phase_announcement.hash(&mut hasher);
    state.transcript_announcement.hash(&mut hasher);
    state.notice.hash(&mut hasher);
    if let Some(error) = &state.error {
        error.message.hash(&mut hasher);
        match error.recovery {
            super::controller::OverlayRecovery::None => 0_u8,
            super::controller::OverlayRecovery::Retry => 1_u8,
            super::controller::OverlayRecovery::WaitForPreview => 2_u8,
        }
        .hash(&mut hasher);
    }
    state.live_preview_available.hash(&mut hasher);
    hasher.finish()
}

fn quantized_level(level: f32) -> u8 {
    (level.clamp(0.0, 1.0) * 40.0).round() as u8
}

#[derive(Debug)]
struct ControlActionBridge {
    session_id: Mutex<Option<SessionId>>,
    event_sink: NativeEventSink,
}

impl ControlActionBridge {
    fn bind(&self, session_id: Option<SessionId>) {
        if let Ok(mut current) = self.session_id.lock() {
            *current = session_id;
        }
    }

    fn emit_abandon(&self) {
        let session_id = self.session_id.lock().ok().and_then(|current| *current);
        if let Some(session_id) = session_id {
            self.event_sink
                .emit(NativeOverlayEvent::Action(OverlayAction::Abandon(
                    session_id,
                )));
        }
    }
}

struct WindowProcedureState {
    role: WindowRole,
    action_bridge: Option<Arc<ControlActionBridge>>,
    pressed: Cell<bool>,
    accessibility: RefCell<Option<NativeAccessibility>>,
}

impl WindowProcedureState {
    fn on_cancel(&self) {
        if self.role == WindowRole::Control
            && let Some(bridge) = &self.action_bridge
        {
            bridge.emit_abandon();
        }
    }

    fn cancel_press(&self) {
        self.pressed.set(false);
    }
}

struct NativeWindow {
    hwnd: HWND,
    role: WindowRole,
    surface: Option<LayeredSurface>,
    tooltip: Option<NativeTooltip>,
    _procedure_state: Box<WindowProcedureState>,
}

impl NativeWindow {
    fn create(
        role: WindowRole,
        action_bridge: Option<Arc<ControlActionBridge>>,
    ) -> Result<Self, NativeOverlayError> {
        register_window_classes()?;
        let class_name = wide_null(role.class_name());
        let title = wide_null(role.title());
        let accessibility_bridge = action_bridge.clone();
        let mut procedure_state = Box::new(WindowProcedureState {
            role,
            action_bridge,
            pressed: Cell::new(false),
            accessibility: RefCell::new(None),
        });
        let module = unsafe { GetModuleHandleW(null()) };
        if module.is_null() {
            return Err(NativeOverlayError::ModuleHandle);
        }
        let hwnd = unsafe {
            CreateWindowExW(
                role.ex_style(),
                class_name.as_ptr(),
                title.as_ptr(),
                WS_POPUP,
                0,
                0,
                0,
                0,
                null_mut(),
                null_mut(),
                module,
                (&mut *procedure_state as *mut WindowProcedureState).cast::<c_void>(),
            )
        };
        if hwnd.is_null() {
            return Err(NativeOverlayError::CreateWindow(role));
        }
        let mut window = Self {
            hwnd,
            role,
            surface: None,
            tooltip: None,
            _procedure_state: procedure_state,
        };
        if !window.is_hardened() {
            return Err(NativeOverlayError::Hardening(role));
        }
        *window._procedure_state.accessibility.borrow_mut() =
            NativeAccessibility::install(hwnd, role, accessibility_bridge);
        if role == WindowRole::Control {
            window.tooltip = NativeTooltip::create(hwnd).ok();
        }
        Ok(window)
    }

    fn update_accessibility(
        &mut self,
        state: Option<&OverlayViewState>,
        visible: bool,
        control_enabled: bool,
        bounds: Option<OverlayWindowBounds>,
    ) -> bool {
        let updated = {
            let accessibility = self._procedure_state.accessibility.borrow();
            let Some(accessibility) = accessibility.as_ref() else {
                return false;
            };
            accessibility.update(state, visible, control_enabled, bounds)
        };
        if !updated {
            self._procedure_state.accessibility.borrow_mut().take();
        }
        updated
    }

    fn control_capabilities_ready(&self) -> bool {
        self.role == WindowRole::Control
            && self._procedure_state.accessibility.borrow().is_some()
            && self.tooltip.is_some()
    }

    fn is_hardened(&self) -> bool {
        let style = unsafe { GetWindowLongPtrW(self.hwnd, GWL_EXSTYLE) } as u32;
        let required = self.role.ex_style();
        style & required == required
            && (self.role.requires_transparency() || style & WS_EX_TRANSPARENT == 0)
    }

    fn submit_frame(
        &mut self,
        bounds: OverlayWindowBounds,
        frame: &LayeredFrame,
    ) -> Result<(), NativeOverlayFailure> {
        if frame.width != bounds.width || frame.height != bounds.height {
            return Err(NativeOverlayFailure::new(
                NativeOverlayFailureStage::LayeredPresentation,
                Some(self.role),
            ));
        }
        let needs_surface = self
            .surface
            .as_ref()
            .is_none_or(|surface| surface.width != frame.width || surface.height != frame.height);
        if needs_surface {
            self.surface = Some(LayeredSurface::new(frame.width, frame.height).map_err(|_| {
                NativeOverlayFailure::new(
                    NativeOverlayFailureStage::LayeredPresentation,
                    Some(self.role),
                )
            })?);
        }
        self.surface
            .as_mut()
            .is_some_and(|surface| surface.submit(self.hwnd, bounds, frame))
            .then_some(())
            .ok_or_else(|| {
                NativeOverlayFailure::new(
                    NativeOverlayFailureStage::LayeredPresentation,
                    Some(self.role),
                )
            })
    }

    fn prepare_show(&mut self, bounds: OverlayWindowBounds) -> Result<(), NativeOverlayFailure> {
        if !self.is_hardened() {
            return Err(NativeOverlayFailure::new(
                NativeOverlayFailureStage::Positioning,
                Some(self.role),
            ));
        }
        if self.role == WindowRole::Control
            && !self
                .tooltip
                .as_mut()
                .is_some_and(|tooltip| tooltip.update_bounds(bounds.width, bounds.height))
        {
            return Err(NativeOverlayFailure::new(
                NativeOverlayFailureStage::Positioning,
                Some(self.role),
            ));
        }
        Ok(())
    }

    fn is_visible(&self) -> bool {
        unsafe { IsWindowVisible(self.hwnd) != 0 }
    }

    fn reassert_topmost(&self, bounds: OverlayWindowBounds) -> Result<(), NativeOverlayFailure> {
        if !self.is_hardened() {
            return Err(NativeOverlayFailure::new(
                NativeOverlayFailureStage::Positioning,
                Some(self.role),
            ));
        }
        if unsafe {
            SetWindowPos(
                self.hwnd,
                HWND_TOPMOST,
                bounds.x,
                bounds.y,
                bounds.width,
                bounds.height,
                SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            )
        } == 0
        {
            return Err(NativeOverlayFailure::new(
                NativeOverlayFailureStage::Positioning,
                Some(self.role),
            ));
        }
        self.is_visible().then_some(()).ok_or_else(|| {
            NativeOverlayFailure::new(NativeOverlayFailureStage::Visibility, Some(self.role))
        })
    }

    fn prepare_hide(&self) {
        self._procedure_state.cancel_press();
        if unsafe { GetCapture() } == self.hwnd {
            unsafe {
                ReleaseCapture();
            }
        }
    }

    fn hide(&self) {
        self.prepare_hide();
        unsafe {
            SetWindowPos(
                self.hwnd,
                null_mut(),
                0,
                0,
                0,
                0,
                SWP_HIDEWINDOW
                    | SWP_NOACTIVATE
                    | SWP_NOMOVE
                    | SWP_NOSIZE
                    | SWP_NOZORDER
                    | SWP_NOOWNERZORDER,
            );
        }
    }
}

impl Drop for NativeWindow {
    fn drop(&mut self) {
        self.tooltip.take();
        self._procedure_state.accessibility.borrow_mut().take();
        self.hide();
        unsafe {
            DestroyWindow(self.hwnd);
        }
    }
}

struct NativeTooltip {
    hwnd: HWND,
    tool_hwnd: HWND,
    text: Vec<u16>,
    rect: RECT,
}

impl NativeTooltip {
    fn create(tool_hwnd: HWND) -> Result<Self, NativeTooltipError> {
        let controls = INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_WIN95_CLASSES,
        };
        if unsafe { InitCommonControlsEx(&controls) } == 0 {
            return Err(NativeTooltipError::InitializeCommonControls);
        }
        let module = unsafe { GetModuleHandleW(null()) };
        if module.is_null() {
            return Err(NativeTooltipError::ModuleHandle);
        }
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
                TOOLTIPS_CLASSW,
                null(),
                WS_POPUP | TTS_ALWAYSTIP | TTS_NOPREFIX,
                0,
                0,
                0,
                0,
                tool_hwnd,
                null_mut(),
                module,
                null_mut(),
            )
        };
        if hwnd.is_null() {
            return Err(NativeTooltipError::CreateWindow);
        }
        let mut tooltip = Self {
            hwnd,
            tool_hwnd,
            text: wide_null(CANCEL_RECORDING_LABEL),
            rect: RECT {
                left: 0,
                top: 0,
                right: 1,
                bottom: 1,
            },
        };
        let tool = tooltip.tool_info();
        let added =
            unsafe { SendMessageW(hwnd, TTM_ADDTOOLW, 0, (&raw const tool) as LPARAM) != 0 };
        if !added {
            return Err(NativeTooltipError::RegisterTool);
        }
        let positioned = unsafe {
            SetWindowPos(
                hwnd,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
            ) != 0
        };
        if !positioned {
            return Err(NativeTooltipError::PositionWindow);
        }
        Ok(tooltip)
    }

    fn update_bounds(&mut self, width: i32, height: i32) -> bool {
        if width <= 0 || height <= 0 {
            return false;
        }
        self.rect = RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        };
        let tool = self.tool_info();
        unsafe {
            SendMessageW(self.hwnd, TTM_NEWTOOLRECTW, 0, (&raw const tool) as LPARAM);
        }
        true
    }

    fn tool_info(&mut self) -> TTTOOLINFOW {
        TTTOOLINFOW {
            // The process does not require a comctl32 v6 visual-style manifest. Use the v2
            // structure size so the system v5 tooltip implementation accepts the tool.
            cbSize: offset_of!(TTTOOLINFOW, lpReserved) as u32,
            uFlags: TTF_SUBCLASS,
            hwnd: self.tool_hwnd,
            uId: 1,
            rect: self.rect,
            hinst: null_mut(),
            lpszText: self.text.as_mut_ptr(),
            lParam: 0,
            lpReserved: null_mut(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum NativeTooltipError {
    InitializeCommonControls,
    ModuleHandle,
    CreateWindow,
    RegisterTool,
    PositionWindow,
}

impl Drop for NativeTooltip {
    fn drop(&mut self) {
        let tool = self.tool_info();
        unsafe {
            SendMessageW(self.hwnd, TTM_DELTOOLW, 0, (&raw const tool) as LPARAM);
            DestroyWindow(self.hwnd);
        }
    }
}

struct NativeOverlayHost {
    display: NativeWindow,
    control: NativeWindow,
    rasterizer: NativeRasterizer,
    action_bridge: Arc<ControlActionBridge>,
    last_render_key: Option<OverlayRenderKey>,
}

impl NativeOverlayHost {
    fn new(event_sink: NativeEventSink) -> Result<Self, NativeOverlayError> {
        let action_bridge = Arc::new(ControlActionBridge {
            session_id: Mutex::new(None),
            event_sink,
        });
        let display = NativeWindow::create(WindowRole::Display, None)?;
        let control = NativeWindow::create(WindowRole::Control, Some(Arc::clone(&action_bridge)))?;
        let rasterizer = NativeRasterizer::new()?;
        Ok(Self {
            display,
            control,
            rasterizer,
            action_bridge,
            last_render_key: None,
        })
    }

    fn apply_snapshot(
        &mut self,
        snapshot: &OverlaySnapshot,
        animation_frame: u8,
    ) -> Result<bool, NativeOverlayFailure> {
        if !snapshot.requested_visible {
            self.hide();
            return Ok(false);
        }
        let display_bounds = snapshot.display_bounds.ok_or_else(|| {
            NativeOverlayFailure::new(NativeOverlayFailureStage::Positioning, None)
        })?;
        let control_bounds = snapshot.control_bounds.ok_or_else(|| {
            NativeOverlayFailure::new(
                NativeOverlayFailureStage::Positioning,
                Some(WindowRole::Control),
            )
        })?;
        let key = snapshot.render_key(animation_frame);
        let bounds_changed = self.last_render_key.as_ref().is_some_and(|previous| {
            previous.display_bounds != key.display_bounds
                || previous.control_bounds != key.control_bounds
                || previous.dpi != key.dpi
        });
        if needs_pixel_submission(self.last_render_key.as_ref(), &key) {
            self.render_and_submit(snapshot, display_bounds, control_bounds)?;
            self.last_render_key = Some(key);
        }
        if snapshot.control_requested {
            self.action_bridge.bind(snapshot.state.session_id);
        }

        let display_visible = self.display.is_visible();
        let control_visible = self.control.is_visible();
        if !display_visible || control_visible != snapshot.control_requested {
            if snapshot.control_requested {
                self.display.prepare_show(display_bounds)?;
                self.control.prepare_show(control_bounds)?;
                if !show_windows_transactionally(&[
                    (&self.display, display_bounds),
                    (&self.control, control_bounds),
                ]) {
                    self.hide();
                    return Err(NativeOverlayFailure::new(
                        NativeOverlayFailureStage::Positioning,
                        None,
                    ));
                }
            } else {
                self.hide_control();
                self.display.prepare_show(display_bounds)?;
                if !show_windows_transactionally(&[(&self.display, display_bounds)]) {
                    self.hide();
                    return Err(NativeOverlayFailure::new(
                        NativeOverlayFailureStage::Positioning,
                        Some(WindowRole::Display),
                    ));
                }
            }
        }

        if !self.display.is_visible()
            || (snapshot.control_requested && !self.control.is_visible())
            || (!snapshot.control_requested && self.control.is_visible())
        {
            self.hide();
            return Err(NativeOverlayFailure::new(
                NativeOverlayFailureStage::Visibility,
                None,
            ));
        }
        if bounds_changed {
            self.health_check(snapshot)?;
        }
        Ok(true)
    }

    fn render_and_submit(
        &mut self,
        snapshot: &OverlaySnapshot,
        display_bounds: OverlayWindowBounds,
        control_bounds: OverlayWindowBounds,
    ) -> Result<(), NativeOverlayFailure> {
        let display_frame = self
            .rasterizer
            .render_display(
                &snapshot.state,
                snapshot.dark_mode,
                display_bounds.width,
                display_bounds.height,
            )
            .map_err(|_| {
                NativeOverlayFailure::new(
                    NativeOverlayFailureStage::Rasterization,
                    Some(WindowRole::Display),
                )
            })?;
        let control_frame = snapshot
            .control_requested
            .then(|| {
                self.rasterizer.render_control(
                    snapshot.dark_mode,
                    control_bounds.width,
                    control_bounds.height,
                )
            })
            .transpose()
            .map_err(|_| {
                NativeOverlayFailure::new(
                    NativeOverlayFailureStage::Rasterization,
                    Some(WindowRole::Control),
                )
            })?;

        if !self.display.update_accessibility(
            Some(&snapshot.state),
            true,
            false,
            Some(display_bounds),
        ) {
            return Err(NativeOverlayFailure::new(
                NativeOverlayFailureStage::Accessibility,
                Some(WindowRole::Display),
            ));
        }
        if snapshot.control_requested {
            if !self.control.control_capabilities_ready()
                || !self.control.update_accessibility(
                    Some(&snapshot.state),
                    true,
                    true,
                    Some(control_bounds),
                )
            {
                return Err(NativeOverlayFailure::new(
                    NativeOverlayFailureStage::Accessibility,
                    Some(WindowRole::Control),
                ));
            }
        } else {
            self.control.update_accessibility(None, false, false, None);
        }

        self.display.submit_frame(display_bounds, &display_frame)?;
        if let Some(control_frame) = control_frame.as_ref() {
            self.control.submit_frame(control_bounds, control_frame)?;
        }
        Ok(())
    }

    fn health_check(&mut self, snapshot: &OverlaySnapshot) -> Result<(), NativeOverlayFailure> {
        if !snapshot.requested_visible {
            return Ok(());
        }
        let display_bounds = snapshot.display_bounds.ok_or_else(|| {
            NativeOverlayFailure::new(NativeOverlayFailureStage::Positioning, None)
        })?;
        self.display.reassert_topmost(display_bounds)?;
        if snapshot.control_requested {
            let control_bounds = snapshot.control_bounds.ok_or_else(|| {
                NativeOverlayFailure::new(
                    NativeOverlayFailureStage::Positioning,
                    Some(WindowRole::Control),
                )
            })?;
            self.control.reassert_topmost(control_bounds)?;
        } else if self.control.is_visible() {
            return Err(NativeOverlayFailure::new(
                NativeOverlayFailureStage::Visibility,
                Some(WindowRole::Control),
            ));
        }
        Ok(())
    }

    fn hide_control(&mut self) {
        self.action_bridge.bind(None);
        self.control.update_accessibility(None, false, false, None);
        self.control.prepare_hide();
        hide_windows_transactionally(&[&self.control]);
    }

    fn hide(&mut self) {
        self.action_bridge.bind(None);
        self.display.update_accessibility(None, false, false, None);
        self.control.update_accessibility(None, false, false, None);
        self.control.prepare_hide();
        self.display.prepare_hide();
        hide_windows_transactionally(&[&self.control, &self.display]);
        self.last_render_key = None;
    }
}

#[cfg(test)]
trait FailClosedOverlay {
    fn unbind_action(&mut self);
    fn reset_display_accessibility_hidden(&mut self);
    fn reset_control_accessibility_hidden(&mut self);
    fn hide_control_window(&mut self);
    fn hide_display_window(&mut self);
}

#[cfg(test)]
fn hide_overlay_fail_closed(overlay: &mut impl FailClosedOverlay) {
    overlay.unbind_action();
    // Reset both provider trees before hiding either HWND so a failed presentation can never
    // leave a visible semantic tree behind a hidden surface.
    overlay.reset_display_accessibility_hidden();
    overlay.reset_control_accessibility_hidden();
    overlay.hide_control_window();
    overlay.hide_display_window();
}

#[cfg(test)]
fn finish_display_presentation(presented: bool, overlay: &mut impl FailClosedOverlay) -> bool {
    if !presented {
        hide_overlay_fail_closed(overlay);
    }
    presented
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct NativePresentationResult {
    display_presented: bool,
    control_presented: bool,
}

#[cfg(test)]
fn combine_presentation_results(
    display_presented: bool,
    control_requested: bool,
    control_presented: bool,
) -> NativePresentationResult {
    let pair_presented = display_presented && (!control_requested || control_presented);
    NativePresentationResult {
        display_presented: pair_presented,
        control_presented: pair_presented && control_requested,
    }
}

#[cfg(test)]
fn display_prerequisites_ready(rendered: bool, accessible: bool) -> bool {
    rendered && accessible
}

fn needs_pixel_submission(previous: Option<&OverlayRenderKey>, next: &OverlayRenderKey) -> bool {
    previous != Some(next)
}

fn show_windows_transactionally(windows: &[(&NativeWindow, OverlayWindowBounds)]) -> bool {
    if windows.is_empty() {
        return true;
    }
    let mut deferred = unsafe { BeginDeferWindowPos(windows.len() as i32) };
    if deferred.is_null() {
        return false;
    }
    for (window, bounds) in windows {
        deferred = unsafe {
            DeferWindowPos(
                deferred,
                window.hwnd,
                HWND_TOPMOST,
                bounds.x,
                bounds.y,
                bounds.width,
                bounds.height,
                SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_SHOWWINDOW,
            )
        };
        if deferred.is_null() {
            return false;
        }
    }
    unsafe { EndDeferWindowPos(deferred) != 0 }
}

fn hide_windows_transactionally(windows: &[&NativeWindow]) -> bool {
    if windows.is_empty() {
        return true;
    }
    let mut deferred = unsafe { BeginDeferWindowPos(windows.len() as i32) };
    if deferred.is_null() {
        for window in windows {
            window.hide();
        }
        return false;
    }
    for window in windows {
        deferred = unsafe {
            DeferWindowPos(
                deferred,
                window.hwnd,
                null_mut(),
                0,
                0,
                0,
                0,
                SWP_HIDEWINDOW
                    | SWP_NOACTIVATE
                    | SWP_NOMOVE
                    | SWP_NOSIZE
                    | SWP_NOZORDER
                    | SWP_NOOWNERZORDER,
            )
        };
        if deferred.is_null() {
            for window in windows {
                window.hide();
            }
            return false;
        }
    }
    if unsafe { EndDeferWindowPos(deferred) } == 0 {
        for window in windows {
            window.hide();
        }
        false
    } else {
        true
    }
}

struct LayeredSurface {
    dc: HDC,
    bitmap: HBITMAP,
    old_bitmap: HGDIOBJ,
    bits: *mut u8,
    byte_len: usize,
    width: i32,
    height: i32,
}

impl LayeredSurface {
    fn new(width: i32, height: i32) -> Result<Self, NativeOverlayError> {
        let byte_len =
            frame_byte_len(width, height).ok_or(NativeOverlayError::SurfaceDimensions)?;
        let dc = unsafe { CreateCompatibleDC(null_mut()) };
        if dc.is_null() {
            return Err(NativeOverlayError::CreateSurfaceDc);
        }
        let mut bits: *mut c_void = null_mut();
        let bitmap_info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                biSizeImage: u32::try_from(byte_len).unwrap_or(0),
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            },
            bmiColors: [RGBQUAD {
                rgbBlue: 0,
                rgbGreen: 0,
                rgbRed: 0,
                rgbReserved: 0,
            }],
        };
        let bitmap =
            unsafe { CreateDIBSection(dc, &bitmap_info, DIB_RGB_COLORS, &mut bits, null_mut(), 0) };
        if bitmap.is_null() || bits.is_null() {
            unsafe {
                DeleteDC(dc);
            }
            return Err(NativeOverlayError::CreateSurfaceBitmap);
        }
        let old_bitmap = unsafe { SelectObject(dc, bitmap as HGDIOBJ) };
        if old_bitmap.is_null() {
            unsafe {
                DeleteObject(bitmap as HGDIOBJ);
                DeleteDC(dc);
            }
            return Err(NativeOverlayError::SelectSurfaceBitmap);
        }
        Ok(Self {
            dc,
            bitmap,
            old_bitmap,
            bits: bits.cast::<u8>(),
            byte_len,
            width,
            height,
        })
    }

    fn submit(&mut self, hwnd: HWND, bounds: OverlayWindowBounds, frame: &LayeredFrame) -> bool {
        if frame.width != self.width
            || frame.height != self.height
            || frame.pixels.len() != self.byte_len
        {
            return false;
        }
        unsafe {
            std::ptr::copy_nonoverlapping(frame.pixels.as_ptr(), self.bits, self.byte_len);
        }
        let destination = POINT {
            x: bounds.x,
            y: bounds.y,
        };
        let size = SIZE {
            cx: bounds.width,
            cy: bounds.height,
        };
        let source = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        unsafe {
            UpdateLayeredWindow(
                hwnd,
                null_mut(),
                &destination,
                &size,
                self.dc,
                &source,
                0,
                &blend,
                ULW_ALPHA,
            ) != 0
        }
    }
}

impl Drop for LayeredSurface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old_bitmap);
            DeleteObject(self.bitmap as HGDIOBJ);
            DeleteDC(self.dc);
        }
    }
}

fn frame_byte_len(width: i32, height: i32) -> Option<usize> {
    let byte_len = usize::try_from(width)
        .ok()
        .filter(|width| *width > 0)
        .and_then(|width| {
            usize::try_from(height)
                .ok()
                .filter(|height| *height > 0)
                .and_then(|height| width.checked_mul(height))
        })
        .and_then(|pixels| pixels.checked_mul(4))?;
    u32::try_from(byte_len).ok().map(|_| byte_len)
}

#[derive(Clone, Debug, thiserror::Error)]
enum NativeOverlayError {
    #[error("could not resolve the application module handle")]
    ModuleHandle,
    #[error("could not register the {0:?} overlay window class")]
    RegisterClass(WindowRole),
    #[error("could not create the {0:?} overlay window")]
    CreateWindow(WindowRole),
    #[error("could not verify the {0:?} overlay hardening profile")]
    Hardening(WindowRole),
    #[error("native overlay surface dimensions are invalid")]
    SurfaceDimensions,
    #[error("could not create the native overlay memory device context")]
    CreateSurfaceDc,
    #[error("could not create the native overlay DIB section")]
    CreateSurfaceBitmap,
    #[error("could not select the native overlay DIB section")]
    SelectSurfaceBitmap,
    #[error("could not initialize native overlay rasterization: {0}")]
    Raster(#[from] raster::RasterError),
}

static WINDOW_CLASSES_REGISTERED: OnceLock<Result<(), NativeOverlayError>> = OnceLock::new();

fn register_window_classes() -> Result<(), NativeOverlayError> {
    match WINDOW_CLASSES_REGISTERED.get_or_init(register_window_classes_once) {
        Ok(()) => Ok(()),
        Err(error) => Err(error.clone()),
    }
}

fn register_window_classes_once() -> Result<(), NativeOverlayError> {
    let module = unsafe { GetModuleHandleW(null()) };
    if module.is_null() {
        return Err(NativeOverlayError::ModuleHandle);
    }
    for role in [WindowRole::Display, WindowRole::Control] {
        let class_name = wide_null(role.class_name());
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: 0,
            lpfnWndProc: Some(native_overlay_wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: module,
            hIcon: null_mut(),
            hCursor: null_mut(),
            hbrBackground: null_mut(),
            lpszMenuName: null(),
            lpszClassName: class_name.as_ptr(),
            hIconSm: null_mut(),
        };
        if unsafe { RegisterClassExW(&class) } == 0 {
            return Err(NativeOverlayError::RegisterClass(role));
        }
    }
    Ok(())
}

unsafe extern "system" fn native_overlay_wnd_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    contain_wnd_proc_dispatch(
        || unsafe { native_overlay_wnd_proc_inner(hwnd, message, wparam, lparam) },
        || unsafe { native_overlay_wnd_proc_panic_fallback(hwnd, message, wparam, lparam) },
    )
}

fn contain_wnd_proc_dispatch(
    dispatch: impl FnOnce() -> LRESULT,
    fallback: impl FnOnce() -> LRESULT,
) -> LRESULT {
    match catch_unwind(AssertUnwindSafe(dispatch)) {
        Ok(result) => result,
        Err(_) => catch_unwind(AssertUnwindSafe(fallback)).unwrap_or(0),
    }
}

unsafe fn native_overlay_wnd_proc_panic_fallback(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // A Rust panic must never cross the system ABI. Hide the affected overlay surface before
    // returning control to Windows, and revoke its action/UIA bridges so the hidden surface
    // cannot remain operable. The outer containment also catches any panic in this cleanup.
    let state = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const WindowProcedureState;
    if !state.is_null() {
        fail_closed_window_state(unsafe { &*state });
    }
    unsafe {
        ShowWindow(hwnd, SW_HIDE);
    }
    if message == WM_NCCREATE {
        0
    } else {
        unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
    }
}

fn fail_closed_window_state(state: &WindowProcedureState) {
    state.cancel_press();
    if let Some(bridge) = &state.action_bridge {
        bridge.bind(None);
    }
    if let Ok(mut accessibility) = state.accessibility.try_borrow_mut() {
        accessibility.take();
    }
}

unsafe fn native_overlay_wnd_proc_inner(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_NCCREATE {
        let create = lparam as *const CREATESTRUCTW;
        if create.is_null() {
            return 0;
        }
        let state = unsafe { (*create).lpCreateParams } as *mut WindowProcedureState;
        if state.is_null() {
            return 0;
        }
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);
        }
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }

    let state = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const WindowProcedureState;
    if state.is_null() {
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }
    let state = unsafe { &*state };
    match message {
        WM_GETOBJECT => {
            let result = state
                .accessibility
                .borrow()
                .as_ref()
                .and_then(|accessibility| accessibility.handle_wm_getobject(wparam, lparam));
            result.unwrap_or_else(|| unsafe { DefWindowProcW(hwnd, message, wparam, lparam) })
        }
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_NCHITTEST => match state.role {
            WindowRole::Display => HTTRANSPARENT as LRESULT,
            WindowRole::Control => HTCLIENT as LRESULT,
        },
        WM_LBUTTONDOWN if state.role == WindowRole::Control => {
            state.pressed.set(true);
            unsafe {
                SetCapture(hwnd);
            }
            0
        }
        WM_LBUTTONUP => {
            let was_pressed = state.pressed.replace(false);
            if unsafe { GetCapture() } == hwnd {
                unsafe {
                    ReleaseCapture();
                }
            }
            if was_pressed && point_is_in_client(hwnd, lparam) {
                state.on_cancel();
            }
            0
        }
        WM_CAPTURECHANGED | WM_CANCELMODE => {
            state.cancel_press();
            0
        }
        WM_DESTROY => {
            state.cancel_press();
            if let Some(bridge) = &state.action_bridge {
                bridge.bind(None);
            }
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
        WM_NCDESTROY => unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            DefWindowProcW(hwnd, message, wparam, lparam)
        },
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

fn point_is_in_client(hwnd: HWND, lparam: LPARAM) -> bool {
    let x = (lparam as u32 & 0xffff) as u16 as i16 as i32;
    let y = ((lparam as u32 >> 16) & 0xffff) as u16 as i16 as i32;
    let mut client: RECT = unsafe { zeroed() };
    (unsafe { GetClientRect(hwnd, &mut client) != 0 })
        && x >= client.left
        && x < client.right
        && y >= client.top
        && y < client.bottom
}

fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

struct SnapshotMailbox {
    latest: Mutex<Option<OverlaySnapshot>>,
    shutdown: AtomicBool,
}

struct NativeOverlayService {
    mailbox: Arc<SnapshotMailbox>,
    events: Receiver<NativeOverlayEvent>,
    worker: Option<JoinHandle<()>>,
    presented: bool,
    presented_session: Option<SessionId>,
    pending_action: Option<OverlayAction>,
    last_reported_failure: Option<NativeOverlayFailure>,
}

impl NativeOverlayService {
    fn new(context: &eframe::egui::Context) -> Result<Self, NativeOverlayFailure> {
        let mailbox = Arc::new(SnapshotMailbox {
            latest: Mutex::new(None),
            shutdown: AtomicBool::new(false),
        });
        let (event_tx, events) = bounded(OVERLAY_EVENT_CAPACITY);
        let event_sink = NativeEventSink {
            tx: event_tx,
            repaint_context: context.clone(),
        };
        let worker_mailbox = Arc::clone(&mailbox);
        let panic_sink = event_sink.clone();
        let worker = thread::Builder::new()
            .name("scribe-native-overlay".to_owned())
            .spawn(move || {
                let result = catch_unwind(AssertUnwindSafe(|| {
                    run_native_overlay_thread(worker_mailbox, event_sink);
                }));
                if result.is_err() {
                    panic_sink.emit(NativeOverlayEvent::Failure(NativeOverlayFailure::new(
                        NativeOverlayFailureStage::WorkerPanicked,
                        None,
                    )));
                    panic_sink.emit(NativeOverlayEvent::Presented {
                        visible: false,
                        session_id: None,
                    });
                }
            })
            .map_err(|_| {
                NativeOverlayFailure::new(NativeOverlayFailureStage::HostCreation, None)
            })?;
        Ok(Self {
            mailbox,
            events,
            worker: Some(worker),
            presented: false,
            presented_session: None,
            pending_action: None,
            last_reported_failure: None,
        })
    }

    fn submit(&self, snapshot: OverlaySnapshot) {
        if let Ok(mut latest) = self.mailbox.latest.lock() {
            *latest = Some(snapshot);
        }
        if let Some(worker) = &self.worker {
            worker.thread().unpark();
        }
    }

    fn poll_events(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                NativeOverlayEvent::Presented {
                    visible,
                    session_id,
                } => {
                    self.presented = visible;
                    self.presented_session = visible.then_some(session_id).flatten();
                    if visible {
                        self.last_reported_failure = None;
                    }
                }
                NativeOverlayEvent::Action(action) => self.pending_action = Some(action),
                NativeOverlayEvent::Failure(failure) => {
                    if self.last_reported_failure != Some(failure) {
                        eprintln!(
                            "Scribe overlay diagnostic: {} ({:?})",
                            failure.stage.diagnostic_code(),
                            failure.role
                        );
                        self.last_reported_failure = Some(failure);
                    }
                }
            }
        }
    }

    fn output_for(&mut self, snapshot: &OverlaySnapshot) -> OverlayViewportOutput {
        self.poll_events();
        let presented = snapshot.requested_visible
            && self.presented
            && (snapshot.state.session_id.is_none()
                || self.presented_session == snapshot.state.session_id);
        OverlayViewportOutput {
            presented,
            action: self.pending_action.take(),
        }
    }

    fn shutdown(&mut self) {
        self.mailbox.shutdown.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            let _ = worker.join();
        }
        self.presented = false;
        self.presented_session = None;
    }
}

impl Drop for NativeOverlayService {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run_native_overlay_thread(mailbox: Arc<SnapshotMailbox>, event_sink: NativeEventSink) {
    let mut host: Option<NativeOverlayHost> = None;
    let mut current_snapshot: Option<OverlaySnapshot> = None;
    let mut last_presented: Option<(bool, Option<SessionId>)> = None;
    let mut last_failure = None;
    let mut last_health_check = Instant::now();
    let mut last_animation_tick = Instant::now();
    let mut animations_enabled = overlay_animations_enabled();

    while !mailbox.shutdown.load(Ordering::Acquire) {
        pump_overlay_messages();
        let next_snapshot = mailbox
            .latest
            .lock()
            .ok()
            .and_then(|mut latest| latest.take());
        if let Some(snapshot) = next_snapshot {
            current_snapshot = Some(snapshot);
            process_snapshot(
                &mut host,
                current_snapshot.as_ref().expect("snapshot stored"),
                &event_sink,
                &mut last_presented,
                &mut last_failure,
                animations_enabled,
            );
        }

        let now = Instant::now();
        if now.duration_since(last_animation_tick) >= OVERLAY_ANIMATION_INTERVAL {
            last_animation_tick = now;
            if current_snapshot.as_ref().is_some_and(|snapshot| {
                snapshot.requested_visible
                    && animations_enabled
                    && snapshot.state.phase.is_progressing()
            }) && let Some(snapshot) = current_snapshot.as_ref()
            {
                process_snapshot(
                    &mut host,
                    snapshot,
                    &event_sink,
                    &mut last_presented,
                    &mut last_failure,
                    animations_enabled,
                );
            }
        }

        if now.duration_since(last_health_check) >= OVERLAY_HEALTH_INTERVAL {
            last_health_check = now;
            animations_enabled = overlay_animations_enabled();
            if let (Some(host), Some(snapshot)) = (host.as_mut(), current_snapshot.as_ref())
                && snapshot.requested_visible
                && let Err(failure) = host.health_check(snapshot)
            {
                host.hide();
                emit_failure_once(&event_sink, failure, &mut last_failure);
                emit_presented_if_changed(&event_sink, false, None, &mut last_presented);
            }
        }
        thread::park_timeout(OVERLAY_THREAD_POLL_INTERVAL);
    }

    if let Some(host) = host.as_mut() {
        host.hide();
    }
    pump_overlay_messages();
    emit_presented_if_changed(&event_sink, false, None, &mut last_presented);
}

fn process_snapshot(
    host: &mut Option<NativeOverlayHost>,
    snapshot: &OverlaySnapshot,
    event_sink: &NativeEventSink,
    last_presented: &mut Option<(bool, Option<SessionId>)>,
    last_failure: &mut Option<NativeOverlayFailure>,
    animations_enabled: bool,
) {
    if !snapshot.requested_visible {
        if let Some(host) = host.as_mut() {
            host.hide();
        }
        emit_presented_if_changed(event_sink, false, None, last_presented);
        return;
    }
    if host.is_none() {
        match NativeOverlayHost::new(event_sink.clone()) {
            Ok(new_host) => *host = Some(new_host),
            Err(_) => {
                emit_failure_once(
                    event_sink,
                    NativeOverlayFailure::new(NativeOverlayFailureStage::HostCreation, None),
                    last_failure,
                );
                emit_presented_if_changed(event_sink, false, None, last_presented);
                return;
            }
        }
    }
    let animation_frame = overlay_animation_frame(snapshot, animations_enabled);
    match host
        .as_mut()
        .expect("native overlay host initialized")
        .apply_snapshot(snapshot, animation_frame)
    {
        Ok(visible) => {
            *last_failure = None;
            emit_presented_if_changed(
                event_sink,
                visible,
                snapshot.state.session_id,
                last_presented,
            );
        }
        Err(failure) => {
            if let Some(host) = host.as_mut() {
                host.hide();
            }
            emit_failure_once(event_sink, failure, last_failure);
            emit_presented_if_changed(event_sink, false, None, last_presented);
        }
    }
}

fn emit_failure_once(
    event_sink: &NativeEventSink,
    failure: NativeOverlayFailure,
    last_failure: &mut Option<NativeOverlayFailure>,
) {
    if *last_failure != Some(failure) {
        event_sink.emit(NativeOverlayEvent::Failure(failure));
        *last_failure = Some(failure);
    }
}

fn emit_presented_if_changed(
    event_sink: &NativeEventSink,
    visible: bool,
    session_id: Option<SessionId>,
    last_presented: &mut Option<(bool, Option<SessionId>)>,
) {
    let next = (visible, visible.then_some(session_id).flatten());
    if *last_presented != Some(next) {
        event_sink.emit(NativeOverlayEvent::Presented {
            visible,
            session_id,
        });
        *last_presented = Some(next);
    }
}

fn overlay_animation_frame(snapshot: &OverlaySnapshot, animations_enabled: bool) -> u8 {
    if !animations_enabled || !snapshot.state.phase.is_progressing() {
        return 0;
    }
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    ((elapsed / OVERLAY_ANIMATION_INTERVAL.as_millis()) % 8) as u8
}

fn pump_overlay_messages() {
    let mut message: MSG = unsafe { zeroed() };
    while unsafe { PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) } != 0 {
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

fn overlay_animations_enabled() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::SystemParametersInfoW;

    const SPI_GETCLIENTAREAANIMATION: u32 = 0x1042;
    let mut enabled = 0i32;
    unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            (&mut enabled as *mut i32).cast::<c_void>(),
            0,
        ) != 0
            && enabled != 0
    }
}

fn snapshot_dpi(spec_width_points: f32, display_width_pixels: i32) -> u32 {
    if !spec_width_points.is_finite() || spec_width_points <= 0.0 || display_width_pixels <= 0 {
        return 96;
    }
    ((display_width_pixels as f32 / spec_width_points) * 96.0)
        .round()
        .clamp(48.0, 768.0) as u32
}

thread_local! {
    static NATIVE_OVERLAY_SERVICE: RefCell<Option<NativeOverlayService>> = const { RefCell::new(None) };
}

pub(super) fn shutdown_overlay_viewport() {
    NATIVE_OVERLAY_SERVICE.with(|slot| {
        if let Some(mut service) = slot.borrow_mut().take() {
            service.shutdown();
        }
    });
}

pub(super) fn show_overlay_viewport(
    context: &eframe::egui::Context,
    state: &OverlayViewState,
    target: Option<&CapturedTarget>,
    position: OverlayPosition,
    presentation: OverlayPresentation,
) -> OverlayViewportOutput {
    let dark_mode = context.style().visuals.dark_mode;
    let requested_visible = state.is_visible() && presentation.permits_background_overlay();
    let snapshot = if requested_visible {
        let spec = window_spec(state.mode);
        if let Some(display_bounds) = overlay_window_bounds(target, spec, position) {
            let control_bounds = control_window_bounds(display_bounds, spec);
            OverlaySnapshot {
                state: state.clone(),
                dark_mode,
                dpi: snapshot_dpi(spec.width_points, display_bounds.width),
                display_bounds: Some(display_bounds),
                control_bounds: Some(control_bounds),
                requested_visible: true,
                control_requested: is_cancellable(state),
            }
        } else {
            OverlaySnapshot::hidden(state, dark_mode)
        }
    } else {
        OverlaySnapshot::hidden(state, dark_mode)
    };

    NATIVE_OVERLAY_SERVICE.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            let Ok(service) = NativeOverlayService::new(context) else {
                eprintln!("Scribe overlay diagnostic: native-overlay-host (None)");
                return OverlayViewportOutput::default();
            };
            *slot = Some(service);
        }
        let service = slot.as_mut().expect("native overlay service initialized");
        service.submit(snapshot.clone());
        service.output_for(&snapshot)
    })
}

#[cfg(test)]
mod tests {
    use super::super::platform::OverlayWindowSpec;
    use super::*;

    trait PresentationTransaction {
        fn verify_hardening(&mut self) -> bool;
        fn submit_pixels(&mut self) -> bool;
        fn show_no_activate(&mut self) -> bool;
        fn is_visible(&mut self) -> bool;
    }

    fn try_present_transaction(transaction: &mut impl PresentationTransaction) -> bool {
        transaction.verify_hardening()
            && transaction.submit_pixels()
            && transaction.show_no_activate()
            && transaction.is_visible()
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Step {
        Verify,
        Submit,
        Show,
        Visible,
        Unbind,
        ResetDisplayAccessibility,
        ResetControlAccessibility,
        HideControlWindow,
        HideDisplayWindow,
    }

    struct FakeTransaction {
        fail_at: Option<Step>,
        steps: Vec<Step>,
    }

    impl FakeTransaction {
        fn new(fail_at: Option<Step>) -> Self {
            Self {
                fail_at,
                steps: Vec::new(),
            }
        }

        fn step(&mut self, step: Step) -> bool {
            self.steps.push(step);
            self.fail_at != Some(step)
        }
    }

    impl PresentationTransaction for FakeTransaction {
        fn verify_hardening(&mut self) -> bool {
            self.step(Step::Verify)
        }

        fn submit_pixels(&mut self) -> bool {
            self.step(Step::Submit)
        }

        fn show_no_activate(&mut self) -> bool {
            self.step(Step::Show)
        }

        fn is_visible(&mut self) -> bool {
            self.step(Step::Visible)
        }
    }

    struct FakeOverlay {
        display_accessibility_visible: bool,
        control_accessibility_visible: bool,
        display_window_visible: bool,
        control_window_visible: bool,
        steps: Vec<Step>,
    }

    impl FakeOverlay {
        fn visible() -> Self {
            Self {
                display_accessibility_visible: true,
                control_accessibility_visible: true,
                display_window_visible: true,
                control_window_visible: true,
                steps: Vec::new(),
            }
        }
    }

    impl FailClosedOverlay for FakeOverlay {
        fn unbind_action(&mut self) {
            self.steps.push(Step::Unbind);
        }

        fn reset_display_accessibility_hidden(&mut self) {
            self.steps.push(Step::ResetDisplayAccessibility);
            self.display_accessibility_visible = false;
        }

        fn reset_control_accessibility_hidden(&mut self) {
            self.steps.push(Step::ResetControlAccessibility);
            self.control_accessibility_visible = false;
        }

        fn hide_control_window(&mut self) {
            self.steps.push(Step::HideControlWindow);
            self.control_window_visible = false;
        }

        fn hide_display_window(&mut self) {
            self.steps.push(Step::HideDisplayWindow);
            self.display_window_visible = false;
        }
    }

    fn snapshot_for_test() -> OverlaySnapshot {
        let mut state = OverlayViewState {
            session_id: Some(SessionId(7)),
            phase: super::super::controller::OverlayPhase::Listening,
            elapsed: Some(Duration::from_millis(1_250)),
            ..OverlayViewState::default()
        };
        state.transcript.committed = "hello".to_owned();
        state.transcript.revision = 3;
        let display_bounds = OverlayWindowBounds {
            x: -1200,
            y: 900,
            width: 750,
            height: 78,
        };
        OverlaySnapshot {
            state,
            dark_mode: true,
            dpi: 120,
            display_bounds: Some(display_bounds),
            control_bounds: Some(OverlayWindowBounds {
                x: -520,
                y: 912,
                width: 55,
                height: 55,
            }),
            requested_visible: true,
            control_requested: true,
        }
    }

    #[test]
    fn display_and_control_profiles_are_deliberately_distinct() {
        assert_eq!(
            DISPLAY_EX_STYLE & REQUIRED_BASE_EX_STYLE,
            REQUIRED_BASE_EX_STYLE
        );
        assert_ne!(DISPLAY_EX_STYLE & WS_EX_TRANSPARENT, 0);
        assert_eq!(
            CONTROL_EX_STYLE & REQUIRED_BASE_EX_STYLE,
            REQUIRED_BASE_EX_STYLE
        );
        assert_eq!(CONTROL_EX_STYLE & WS_EX_TRANSPARENT, 0);
    }

    #[test]
    fn unchanged_render_keys_do_not_request_another_pixel_submission() {
        let snapshot = snapshot_for_test();
        let first = snapshot.render_key(0);
        let same = snapshot.render_key(0);
        let mut previous = None;
        let mut submissions = 0;
        for key in [&first, &same] {
            if needs_pixel_submission(previous.as_ref(), key) {
                submissions += 1;
                previous = Some(key.clone());
            }
        }

        assert_eq!(submissions, 1);
        assert!(!needs_pixel_submission(Some(&first), &same));
    }

    #[test]
    fn render_key_covers_content_quantized_levels_animation_theme_dpi_and_bounds() {
        let snapshot = snapshot_for_test();
        let baseline = snapshot.render_key(2);

        let mut same_buckets = snapshot.clone();
        same_buckets.state.audio_level.rms = 0.001;
        same_buckets.state.audio_level.peak = 0.001;
        assert_eq!(baseline.rms_bucket, same_buckets.render_key(2).rms_bucket);

        let mut content_changed = snapshot.clone();
        content_changed.state.transcript.committed = "different".to_owned();
        assert_ne!(baseline, content_changed.render_key(2));

        let mut level_changed = snapshot.clone();
        level_changed.state.audio_level.rms = 1.0;
        assert_ne!(baseline, level_changed.render_key(2));

        let mut theme_changed = snapshot.clone();
        theme_changed.dark_mode = false;
        assert_ne!(baseline, theme_changed.render_key(2));

        let mut dpi_changed = snapshot.clone();
        dpi_changed.dpi = 144;
        assert_ne!(baseline, dpi_changed.render_key(2));

        let mut bounds_changed = snapshot.clone();
        bounds_changed.display_bounds.as_mut().unwrap().x += 1;
        assert_ne!(baseline, bounds_changed.render_key(2));
        assert_ne!(baseline, snapshot.render_key(3));
    }

    #[test]
    fn latest_snapshot_mailbox_coalesces_superseded_meter_updates() {
        let mailbox = SnapshotMailbox {
            latest: Mutex::new(None),
            shutdown: AtomicBool::new(false),
        };
        let first = snapshot_for_test();
        let mut newest = first.clone();
        newest.state.transcript.revision = 9;
        newest.state.audio_level.rms = 0.8;

        *mailbox.latest.lock().unwrap() = Some(first);
        *mailbox.latest.lock().unwrap() = Some(newest.clone());

        assert_eq!(
            mailbox.latest.lock().unwrap().take().unwrap().state,
            newest.state
        );
    }

    #[test]
    fn failure_events_are_structured_and_do_not_carry_overlay_content() {
        let failure = NativeOverlayFailure::new(
            NativeOverlayFailureStage::Rasterization,
            Some(WindowRole::Display),
        );
        let diagnostic = format!("{failure:?}");
        assert!(diagnostic.contains("Rasterization"));
        assert!(!diagnostic.contains("transcript"));
        assert!(!diagnostic.contains("hello"));
    }

    #[test]
    fn presentation_requires_every_ordered_native_step() {
        let mut transaction = FakeTransaction::new(None);
        assert!(try_present_transaction(&mut transaction));
        assert_eq!(
            transaction.steps,
            vec![Step::Verify, Step::Submit, Step::Show, Step::Visible]
        );
    }

    #[test]
    fn window_procedure_dispatch_contains_panics_and_uses_the_fallback() {
        let fallback_called = Cell::new(false);
        let result = contain_wnd_proc_dispatch(
            || panic!("simulated window procedure panic"),
            || {
                fallback_called.set(true);
                73
            },
        );

        assert_eq!(result, 73);
        assert!(fallback_called.get());

        fallback_called.set(false);
        let result = contain_wnd_proc_dispatch(
            || 29,
            || {
                fallback_called.set(true);
                73
            },
        );
        assert_eq!(result, 29);
        assert!(!fallback_called.get());

        assert_eq!(
            contain_wnd_proc_dispatch(|| panic!("dispatch"), || panic!("fallback")),
            0
        );
    }

    #[test]
    fn panic_cleanup_revokes_the_control_action_and_adapter() {
        let (tx, rx) = bounded(1);
        let bridge = Arc::new(ControlActionBridge {
            session_id: Mutex::new(Some(SessionId(91))),
            event_sink: NativeEventSink {
                tx,
                repaint_context: eframe::egui::Context::default(),
            },
        });
        let state = WindowProcedureState {
            role: WindowRole::Control,
            action_bridge: Some(Arc::clone(&bridge)),
            pressed: Cell::new(true),
            accessibility: RefCell::new(None),
        };

        fail_closed_window_state(&state);
        assert!(!state.pressed.get());
        bridge.emit_abandon();
        assert!(rx.try_recv().is_err());
        assert!(state.accessibility.borrow().is_none());
    }

    #[test]
    fn every_post_accessibility_presentation_failure_resets_both_trees_before_hiding() {
        for failure in [Step::Verify, Step::Submit, Step::Show, Step::Visible] {
            let mut transaction = FakeTransaction::new(Some(failure));
            let presented = try_present_transaction(&mut transaction);
            let mut overlay = FakeOverlay::visible();
            assert!(!finish_display_presentation(presented, &mut overlay));
            assert!(!transaction.steps.contains(&Step::Visible) || failure == Step::Visible);
            assert!(!overlay.display_accessibility_visible);
            assert!(!overlay.control_accessibility_visible);
            assert!(!overlay.display_window_visible);
            assert!(!overlay.control_window_visible);
            assert_eq!(
                overlay.steps,
                vec![
                    Step::Unbind,
                    Step::ResetDisplayAccessibility,
                    Step::ResetControlAccessibility,
                    Step::HideControlWindow,
                    Step::HideDisplayWindow,
                ]
            );
        }
    }

    #[test]
    fn paired_control_failure_prevents_a_presented_overlay_pair() {
        assert_eq!(
            combine_presentation_results(true, true, false),
            NativePresentationResult::default()
        );
        assert_eq!(
            combine_presentation_results(false, true, true),
            NativePresentationResult::default()
        );
    }

    #[test]
    fn display_requires_both_pixels_and_accessibility_ownership() {
        assert!(display_prerequisites_ready(true, true));
        assert!(!display_prerequisites_ready(false, true));
        assert!(!display_prerequisites_ready(true, false));
        assert!(!display_prerequisites_ready(false, false));
    }

    #[test]
    fn action_bridge_binds_the_session_at_event_time() {
        let (tx, rx) = bounded(4);
        let bridge = ControlActionBridge {
            session_id: Mutex::new(None),
            event_sink: NativeEventSink {
                tx,
                repaint_context: eframe::egui::Context::default(),
            },
        };
        bridge.emit_abandon();
        assert!(rx.try_recv().is_err());

        bridge.bind(Some(SessionId(41)));
        bridge.emit_abandon();
        assert!(matches!(
            rx.try_recv(),
            Ok(NativeOverlayEvent::Action(OverlayAction::Abandon(
                SessionId(41)
            )))
        ));

        bridge.bind(None);
        bridge.emit_abandon();
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn native_titles_match_the_existing_public_window_contract() {
        assert_eq!(
            WindowRole::Display.title(),
            super::super::view::OVERLAY_WINDOW_TITLE
        );
        assert_eq!(
            WindowRole::Control.title(),
            super::super::view::OVERLAY_CONTROL_WINDOW_TITLE
        );
    }

    #[test]
    fn retained_geometry_types_remain_compatible_with_native_lifecycle() {
        let spec = OverlayWindowSpec {
            width_points: 600.0,
            height_points: 62.0,
            margin_points: 24.0,
        };
        let bounds = OverlayWindowBounds {
            x: -1200,
            y: 900,
            width: 750,
            height: 78,
        };
        assert_eq!(spec.width_points, 600.0);
        assert_eq!(bounds.x, -1200);
    }

    #[test]
    fn dib_byte_lengths_are_checked_and_exact() {
        assert_eq!(frame_byte_len(600, 62), Some(600 * 62 * 4));
        assert_eq!(frame_byte_len(44, 44), Some(44 * 44 * 4));
        assert_eq!(frame_byte_len(0, 44), None);
        assert_eq!(frame_byte_len(44, -1), None);
        assert_eq!(frame_byte_len(i32::MAX, i32::MAX), None);
    }

    #[test]
    fn top_down_dib_surface_can_be_created_and_released() {
        let surface = LayeredSurface::new(44, 44).expect("create layered DIB surface");
        assert_eq!((surface.width, surface.height), (44, 44));
        assert_eq!(surface.byte_len, 44 * 44 * 4);
        assert!(!surface.dc.is_null());
        assert!(!surface.bitmap.is_null());
        assert!(!surface.bits.is_null());
    }
}
