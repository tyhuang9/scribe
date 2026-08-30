mod accessibility;
mod layout;
mod raster;
mod transition;

use std::{
    cell::{Cell, RefCell},
    ffi::c_void,
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
    view::{
        OverlayDiagnostic, OverlayViewportOutput, control_window_bounds, is_cancellable,
        window_spec,
    },
};
use crate::transcription::SessionId;

use self::{
    accessibility::{CANCEL_RECORDING_LABEL, NativeAccessibility},
    raster::{LayeredFrame, NativeRasterizer},
    transition::{FRAME_INTERVAL, OverlayTransitionEngine, RenderPlan, TransitionStep},
};

const DISPLAY_CLASS_NAME: &str = "Scribe.NativeOverlay.Display";
const CONTROL_CLASS_NAME: &str = "Scribe.NativeOverlay.Control";
const DISPLAY_WINDOW_TITLE: &str = "Scribe Dictation Overlay";
const CONTROL_WINDOW_TITLE: &str = "Scribe Dictation Overlay Cancel";

const REQUIRED_BASE_EX_STYLE: u32 =
    WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST;
const DISPLAY_EX_STYLE: u32 = REQUIRED_BASE_EX_STYLE | WS_EX_TRANSPARENT;
const CONTROL_EX_STYLE: u32 = REQUIRED_BASE_EX_STYLE;
const OVERLAY_THREAD_IDLE_INTERVAL: Duration = Duration::from_millis(500);
const OVERLAY_HEALTH_INTERVAL: Duration = Duration::from_millis(500);
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
    WindowProcedure,
    WorkerPanicked,
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

    const fn diagnostic(self) -> OverlayDiagnostic {
        match self.stage {
            NativeOverlayFailureStage::HostCreation => OverlayDiagnostic::Host,
            NativeOverlayFailureStage::Rasterization => OverlayDiagnostic::Rasterization,
            NativeOverlayFailureStage::Accessibility => OverlayDiagnostic::Accessibility,
            NativeOverlayFailureStage::LayeredPresentation => OverlayDiagnostic::Presentation,
            NativeOverlayFailureStage::Positioning => OverlayDiagnostic::Positioning,
            NativeOverlayFailureStage::Visibility => OverlayDiagnostic::Visibility,
            NativeOverlayFailureStage::WindowProcedure => OverlayDiagnostic::WindowProcedure,
            NativeOverlayFailureStage::WorkerPanicked => OverlayDiagnostic::Worker,
        }
    }
}

#[derive(Clone, Debug)]
enum NativeOverlayEvent {
    Presented {
        visible: bool,
        session_id: Option<SessionId>,
    },
    Failure(NativeOverlayFailure),
}

#[derive(Clone, Debug)]
struct NativeEventSink {
    tx: Sender<NativeOverlayEvent>,
    repaint_context: eframe::egui::Context,
    presentation: Arc<Mutex<NativePresentationObservation>>,
    retained_action: Arc<Mutex<Option<OverlayAction>>>,
}

impl NativeEventSink {
    fn emit(&self, event: NativeOverlayEvent) {
        if let NativeOverlayEvent::Presented {
            visible,
            session_id,
        } = &event
        {
            let mut presentation = self
                .presentation
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            presentation.visible = *visible;
            presentation.session_id = visible.then_some(*session_id).flatten();
        }
        let _ = self.tx.try_send(event);
        self.repaint_context.request_repaint();
    }

    fn emit_action(&self, action: OverlayAction) {
        *self
            .retained_action
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(action);
        self.repaint_context.request_repaint();
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct NativePresentationObservation {
    visible: bool,
    session_id: Option<SessionId>,
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

    fn display_render_key(&self, animation_frame: u8) -> DisplayRenderKey {
        DisplayRenderKey {
            mode: self.state.mode,
            phase: self.state.phase,
            transcript_revision: self.state.transcript.revision,
            content: OverlayRenderContent::from(&self.state),
            rms_bucket: quantized_level(self.state.audio_level.rms),
            peak_bucket: quantized_level(self.state.audio_level.peak),
            elapsed_second: self.state.elapsed.map_or(0, |elapsed| elapsed.as_secs()),
            dark_mode: self.dark_mode,
            dpi: self.dpi,
            display_bounds: self.display_bounds,
            animation_frame,
        }
    }

    fn control_render_key(&self) -> ControlRenderKey {
        ControlRenderKey {
            visible: self.control_requested,
            enabled: self.control_requested,
            dark_mode: self.dark_mode,
            dpi: self.dpi,
            bounds: self.control_bounds,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct DisplayRenderKey {
    mode: super::controller::OverlayMode,
    phase: super::controller::OverlayPhase,
    transcript_revision: u64,
    content: OverlayRenderContent,
    rms_bucket: u8,
    peak_bucket: u8,
    elapsed_second: u64,
    dark_mode: bool,
    dpi: u32,
    display_bounds: Option<OverlayWindowBounds>,
    // The rasterizer resolves the shared time-based progress glyph while it
    // paints. This epoch is the cache invalidator for that indirect animation
    // input; reduced motion holds it at zero.
    animation_frame: u8,
}

/// The cancel control has an independent layered surface. It is deliberately
/// insulated from display content, meter, and spinner invalidation because its
/// painted X is static; a control visibility, theme, DPI, or bounds change is
/// the only reason to redraw it.
#[derive(Clone, Debug, Eq, PartialEq)]
struct ControlRenderKey {
    visible: bool,
    enabled: bool,
    dark_mode: bool,
    dpi: u32,
    bounds: Option<OverlayWindowBounds>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct OverlayRenderContent {
    committed: String,
    tentative: String,
    phase_announcement: Option<String>,
    transcript_announcement: Option<String>,
    notice: Option<String>,
    error: Option<(String, super::controller::OverlayRecovery)>,
    live_preview_available: bool,
}

/// Accessibility reflects semantic state, never intermediate paint frames.
/// Keeping this key independent from meter/progress invalidation prevents a
/// live-region announcement from being re-emitted during an animation.
#[derive(Clone, Debug, Eq, PartialEq)]
struct DisplayAccessibilityKey {
    mode: super::controller::OverlayMode,
    phase: super::controller::OverlayPhase,
    content: OverlayRenderContent,
    bounds: OverlayWindowBounds,
}

impl From<&OverlayViewState> for OverlayRenderContent {
    fn from(state: &OverlayViewState) -> Self {
        Self {
            committed: state.transcript.committed.clone(),
            tentative: state.transcript.tentative.clone(),
            phase_announcement: state.phase_announcement.clone(),
            transcript_announcement: state.transcript_announcement.clone(),
            notice: state.notice.clone(),
            error: state
                .error
                .as_ref()
                .map(|error| (error.message.clone(), error.recovery)),
            live_preview_available: state.live_preview_available,
        }
    }
}

fn quantized_level(level: f32) -> u8 {
    (level.clamp(0.0, 1.0) * 40.0).round() as u8
}

#[derive(Debug)]
struct ControlActionBridge {
    session_id: Mutex<Option<SessionId>>,
    event_sink: NativeEventSink,
}

#[derive(Clone, Copy, Debug, Default)]
struct OverlayWindowPair {
    display: isize,
    control: isize,
}

struct PairFailureBridge {
    windows: Mutex<OverlayWindowPair>,
    recovery_requested: AtomicBool,
    action_bridge: Arc<ControlActionBridge>,
    event_sink: NativeEventSink,
}

impl PairFailureBridge {
    fn bind(&self, role: WindowRole, hwnd: HWND) {
        let mut windows = self
            .windows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match role {
            WindowRole::Display => windows.display = hwnd as isize,
            WindowRole::Control => windows.control = hwnd as isize,
        }
    }

    fn unbind(&self, role: WindowRole, hwnd: HWND) {
        let mut windows = self
            .windows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let slot = match role {
            WindowRole::Display => &mut windows.display,
            WindowRole::Control => &mut windows.control,
        };
        if *slot == hwnd as isize {
            *slot = 0;
        }
    }

    fn fail_closed(&self, role: WindowRole) {
        self.action_bridge.bind(None);
        self.recovery_requested.store(true, Ordering::Release);
        let windows = *self
            .windows
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let captured = unsafe { GetCapture() } as isize;
        if captured != 0 && (captured == windows.display || captured == windows.control) {
            unsafe {
                ReleaseCapture();
            }
        }
        for handle in [windows.control, windows.display] {
            if handle == 0 {
                continue;
            }
            unsafe {
                SetWindowPos(
                    handle as HWND,
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
                ShowWindow(handle as HWND, SW_HIDE);
            }
        }
        self.event_sink
            .emit(NativeOverlayEvent::Failure(NativeOverlayFailure::new(
                NativeOverlayFailureStage::WindowProcedure,
                Some(role),
            )));
        self.event_sink.emit(NativeOverlayEvent::Presented {
            visible: false,
            session_id: None,
        });
    }

    fn take_recovery_request(&self) -> bool {
        self.recovery_requested.swap(false, Ordering::AcqRel)
    }
}

impl ControlActionBridge {
    fn bind(&self, session_id: Option<SessionId>) {
        if let Ok(mut current) = self.session_id.lock() {
            *current = session_id;
        }
    }

    fn is_bound(&self) -> bool {
        self.session_id
            .lock()
            .is_ok_and(|session_id| session_id.is_some())
    }

    fn emit_abandon(&self) {
        let session_id = self.session_id.lock().ok().and_then(|current| *current);
        if let Some(session_id) = session_id {
            self.event_sink
                .emit_action(OverlayAction::Abandon(session_id));
        }
    }
}

struct WindowProcedureState {
    role: WindowRole,
    action_bridge: Option<Arc<ControlActionBridge>>,
    pair_failure: Arc<PairFailureBridge>,
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

    fn control_enabled(&self) -> bool {
        self.action_bridge
            .as_ref()
            .is_some_and(|bridge| bridge.is_bound())
    }
}

struct NativeWindow {
    hwnd: HWND,
    role: WindowRole,
    surface: Option<LayeredSurface>,
    tooltip: Option<NativeTooltip>,
    pair_failure: Arc<PairFailureBridge>,
    _procedure_state: Box<WindowProcedureState>,
}

impl NativeWindow {
    fn create(
        role: WindowRole,
        action_bridge: Option<Arc<ControlActionBridge>>,
        pair_failure: Arc<PairFailureBridge>,
    ) -> Result<Self, NativeOverlayError> {
        register_window_classes()?;
        let class_name = wide_null(role.class_name());
        let title = wide_null(role.title());
        let accessibility_bridge = action_bridge.clone();
        let mut procedure_state = Box::new(WindowProcedureState {
            role,
            action_bridge,
            pair_failure: Arc::clone(&pair_failure),
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
            pair_failure,
            _procedure_state: procedure_state,
        };
        window.pair_failure.bind(role, hwnd);
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
        self.pair_failure.unbind(self.role, self.hwnd);
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
    pair_failure: Arc<PairFailureBridge>,
    last_display_render_key: Option<DisplayRenderKey>,
    last_display_accessibility_key: Option<DisplayAccessibilityKey>,
    last_control_render_key: Option<ControlRenderKey>,
}

impl NativeOverlayHost {
    fn new(event_sink: NativeEventSink) -> Result<Self, NativeOverlayError> {
        let action_bridge = Arc::new(ControlActionBridge {
            session_id: Mutex::new(None),
            event_sink,
        });
        let pair_failure = Arc::new(PairFailureBridge {
            windows: Mutex::new(OverlayWindowPair::default()),
            recovery_requested: AtomicBool::new(false),
            action_bridge: Arc::clone(&action_bridge),
            event_sink: action_bridge.event_sink.clone(),
        });
        let display = NativeWindow::create(WindowRole::Display, None, Arc::clone(&pair_failure))?;
        let control = NativeWindow::create(
            WindowRole::Control,
            Some(Arc::clone(&action_bridge)),
            Arc::clone(&pair_failure),
        )?;
        let rasterizer = NativeRasterizer::new()?;
        Ok(Self {
            display,
            control,
            rasterizer,
            action_bridge,
            pair_failure,
            last_display_render_key: None,
            last_display_accessibility_key: None,
            last_control_render_key: None,
        })
    }

    fn apply_snapshot(
        &mut self,
        snapshot: &OverlaySnapshot,
        animation_frame: u8,
        plan: Option<&RenderPlan>,
        control_enabled: bool,
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
        let display_key = snapshot.display_render_key(animation_frame);
        let mut control_key = snapshot.control_render_key();
        control_key.enabled = control_enabled;
        let bounds_changed = self
            .last_display_render_key
            .as_ref()
            .is_some_and(|previous| {
                previous.display_bounds != display_key.display_bounds
                    || previous.dpi != display_key.dpi
            })
            || self
                .last_control_render_key
                .as_ref()
                .is_some_and(|previous| {
                    previous.bounds != control_key.bounds || previous.dpi != control_key.dpi
                });
        let invalidation = pixel_invalidation(
            self.last_display_render_key.as_ref(),
            self.last_control_render_key.as_ref(),
            &display_key,
            &control_key,
        );
        if invalidation.display_pixels {
            self.render_and_submit_display(snapshot, display_bounds, plan)?;
            self.last_display_render_key = Some(display_key);
        }
        if invalidation.control_changed {
            if invalidation.control_pixels {
                self.render_and_submit_control(snapshot, control_bounds, control_enabled)?;
            } else {
                self.control.update_accessibility(None, false, false, None);
            }
            self.last_control_render_key = Some(control_key);
        }
        if control_enabled {
            self.action_bridge.bind(snapshot.state.session_id);
        } else {
            self.action_bridge.bind(None);
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

    fn render_and_submit_display(
        &mut self,
        snapshot: &OverlaySnapshot,
        display_bounds: OverlayWindowBounds,
        plan: Option<&RenderPlan>,
    ) -> Result<(), NativeOverlayFailure> {
        let mut display_frame = self
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
        if let Some(plan) = plan {
            let previous_frame = if let Some(previous) = &plan.previous
                && previous.state.mode == snapshot.state.mode
            {
                self.rasterizer
                    .render_display(
                        &previous.state,
                        previous.dark_mode,
                        display_bounds.width,
                        display_bounds.height,
                    )
                    .map_err(|_| {
                        NativeOverlayFailure::new(
                            NativeOverlayFailureStage::Rasterization,
                            Some(WindowRole::Display),
                        )
                    })?
            } else {
                LayeredFrame::transparent(display_bounds.width, display_bounds.height).map_err(
                    |_| {
                        NativeOverlayFailure::new(
                            NativeOverlayFailureStage::Rasterization,
                            Some(WindowRole::Display),
                        )
                    },
                )?
            };
            display_frame = LayeredFrame::crossfade(
                &previous_frame,
                &display_frame,
                plan.previous_opacity,
                plan.target_opacity,
            )
            .map_err(|_| {
                NativeOverlayFailure::new(
                    NativeOverlayFailureStage::Rasterization,
                    Some(WindowRole::Display),
                )
            })?;
        }
        let accessibility_key = DisplayAccessibilityKey {
            mode: snapshot.state.mode,
            phase: snapshot.state.phase,
            content: OverlayRenderContent::from(&snapshot.state),
            bounds: display_bounds,
        };
        if self.last_display_accessibility_key.as_ref() != Some(&accessibility_key) {
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
            self.last_display_accessibility_key = Some(accessibility_key);
        }
        self.display.submit_frame(display_bounds, &display_frame)?;
        Ok(())
    }

    fn render_and_submit_control(
        &mut self,
        snapshot: &OverlaySnapshot,
        control_bounds: OverlayWindowBounds,
        control_enabled: bool,
    ) -> Result<(), NativeOverlayFailure> {
        if !self.control.control_capabilities_ready()
            || !self.control.update_accessibility(
                Some(&snapshot.state),
                control_enabled,
                true,
                Some(control_bounds),
            )
        {
            return Err(NativeOverlayFailure::new(
                NativeOverlayFailureStage::Accessibility,
                Some(WindowRole::Control),
            ));
        }
        let control_frame = if control_enabled {
            self.rasterizer
                .render_control(
                    snapshot.dark_mode,
                    control_bounds.width,
                    control_bounds.height,
                )
                .map_err(|_| {
                    NativeOverlayFailure::new(
                        NativeOverlayFailureStage::Rasterization,
                        Some(WindowRole::Control),
                    )
                })?
        } else {
            LayeredFrame::transparent(control_bounds.width, control_bounds.height).map_err(
                |_| {
                    NativeOverlayFailure::new(
                        NativeOverlayFailureStage::Rasterization,
                        Some(WindowRole::Control),
                    )
                },
            )?
        };
        self.control.submit_frame(control_bounds, &control_frame)?;
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
        self.last_display_render_key = None;
        self.last_display_accessibility_key = None;
        self.last_control_render_key = None;
    }

    fn take_pair_recovery_request(&self) -> bool {
        self.pair_failure.take_recovery_request()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PixelInvalidation {
    display_pixels: bool,
    control_changed: bool,
    control_pixels: bool,
}

fn pixel_invalidation(
    previous_display: Option<&DisplayRenderKey>,
    previous_control: Option<&ControlRenderKey>,
    next_display: &DisplayRenderKey,
    next_control: &ControlRenderKey,
) -> PixelInvalidation {
    let control_changed = needs_pixel_submission(previous_control, next_control);
    PixelInvalidation {
        display_pixels: needs_pixel_submission(previous_display, next_display),
        control_changed,
        control_pixels: control_changed && next_control.visible,
    }
}

fn needs_pixel_submission<T: PartialEq>(previous: Option<&T>, next: &T) -> bool {
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
        let state = unsafe { &*state };
        fail_closed_window_state(state);
        state.pair_failure.fail_closed(state.role);
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
        WM_NCHITTEST => control_hit_test(state.role, state.control_enabled()),
        WM_LBUTTONDOWN if state.role == WindowRole::Control && state.control_enabled() => {
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

fn control_hit_test(role: WindowRole, control_enabled: bool) -> LRESULT {
    match role {
        WindowRole::Display => HTTRANSPARENT as LRESULT,
        WindowRole::Control if control_enabled => HTCLIENT as LRESULT,
        WindowRole::Control => HTTRANSPARENT as LRESULT,
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
    presentation: Arc<Mutex<NativePresentationObservation>>,
    retained_action: Arc<Mutex<Option<OverlayAction>>>,
    pending_action: Option<OverlayAction>,
    pending_diagnostic: Option<OverlayDiagnostic>,
    last_reported_diagnostic: Option<OverlayDiagnostic>,
}

impl NativeOverlayService {
    fn new(context: &eframe::egui::Context) -> Result<Self, NativeOverlayFailure> {
        let mailbox = Arc::new(SnapshotMailbox {
            latest: Mutex::new(None),
            shutdown: AtomicBool::new(false),
        });
        let (event_tx, events) = bounded(OVERLAY_EVENT_CAPACITY);
        let presentation = Arc::new(Mutex::new(NativePresentationObservation::default()));
        let retained_action = Arc::new(Mutex::new(None));
        let event_sink = NativeEventSink {
            tx: event_tx,
            repaint_context: context.clone(),
            presentation: Arc::clone(&presentation),
            retained_action: Arc::clone(&retained_action),
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
            presentation,
            retained_action,
            pending_action: None,
            pending_diagnostic: None,
            last_reported_diagnostic: None,
        })
    }

    fn submit(&self, snapshot: OverlaySnapshot) {
        *self
            .mailbox
            .latest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(snapshot);
        if let Some(worker) = &self.worker {
            worker.thread().unpark();
        }
    }

    fn poll_events(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                NativeOverlayEvent::Presented {
                    visible,
                    session_id: _,
                } => {
                    if visible {
                        self.last_reported_diagnostic = None;
                    }
                }
                NativeOverlayEvent::Failure(failure) => {
                    let diagnostic = failure.diagnostic();
                    if self.last_reported_diagnostic != Some(diagnostic) {
                        self.pending_diagnostic = Some(diagnostic);
                        self.last_reported_diagnostic = Some(diagnostic);
                    }
                }
            }
        }
        if let Some(action) = self
            .retained_action
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            self.pending_action = Some(action);
        }
    }

    fn output_for(&mut self, snapshot: &OverlaySnapshot) -> OverlayViewportOutput {
        self.poll_events();
        let presentation = *self
            .presentation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let presented = snapshot.requested_visible
            && presentation.visible
            && (snapshot.state.session_id.is_none()
                || presentation.session_id == snapshot.state.session_id);
        OverlayViewportOutput {
            presented,
            action: self.pending_action.take(),
            diagnostic: self.pending_diagnostic.take(),
        }
    }

    fn shutdown(&mut self) {
        self.mailbox.shutdown.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            let _ = worker.join();
        }
        *self
            .presentation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            NativePresentationObservation::default();
    }
}

impl Drop for NativeOverlayService {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run_native_overlay_thread(mailbox: Arc<SnapshotMailbox>, event_sink: NativeEventSink) {
    // Layered topmost HWNDs are a desktop-compositor integration. They cover
    // ordinary and DWM-composited borderless windows; exclusive fullscreen,
    // Independent Flip, graphics injection, and anti-cheat interaction are
    // intentionally outside this service's contract.
    // Build the hidden HWND pair and GDI+ rasterizer when the overlay service
    // starts. This has no capture dependency, so it removes first-recording
    // setup work without opening or querying microphone resources.
    let mut last_failure = None;
    let mut host = match NativeOverlayHost::new(event_sink.clone()) {
        Ok(host) => Some(host),
        Err(_) => {
            emit_failure_once(
                &event_sink,
                NativeOverlayFailure::new(NativeOverlayFailureStage::HostCreation, None),
                &mut last_failure,
            );
            None
        }
    };
    let mut current_snapshot: Option<OverlaySnapshot> = None;
    let mut last_presented: Option<(bool, Option<SessionId>)> = None;
    let mut last_health_check = Instant::now();
    let mut animations_enabled = crate::system_preferences::client_area_animations_enabled();
    let mut transitions = OverlayTransitionEngine::default();

    while !mailbox.shutdown.load(Ordering::Acquire) {
        pump_overlay_messages();
        if let Some(host) = host.as_mut()
            && host.take_pair_recovery_request()
        {
            host.hide();
            emit_presented_if_changed(&event_sink, false, None, &mut last_presented);
        }
        let next_snapshot = mailbox
            .latest
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(snapshot) = next_snapshot {
            let step = transitions.advance(snapshot.clone(), Instant::now(), !animations_enabled);
            let hidden = matches!(step, TransitionStep::Hidden);
            current_snapshot = Some(snapshot);
            process_transition_step(
                &mut host,
                current_snapshot.as_ref().expect("snapshot stored"),
                &event_sink,
                &mut last_presented,
                &mut last_failure,
                animations_enabled,
                step,
            );
            if hidden {
                current_snapshot = None;
            }
        }

        let now = Instant::now();
        if let Some(snapshot) = current_snapshot.as_ref() {
            let step = transitions.tick(now, !animations_enabled);
            let hidden = matches!(step, TransitionStep::Hidden);
            let progress_active = animations_enabled
                && snapshot.requested_visible
                && matches!(
                    snapshot.state.phase,
                    super::controller::OverlayPhase::Listening
                        | super::controller::OverlayPhase::Preparing
                        | super::controller::OverlayPhase::Finalizing
                        | super::controller::OverlayPhase::Processing
                        | super::controller::OverlayPhase::Pasting
                );
            if matches!(step, TransitionStep::Idle) && progress_active {
                process_snapshot(
                    &mut host,
                    snapshot,
                    &event_sink,
                    &mut last_presented,
                    &mut last_failure,
                    animations_enabled,
                    None,
                );
            } else if !matches!(step, TransitionStep::Idle) {
                process_transition_step(
                    &mut host,
                    snapshot,
                    &event_sink,
                    &mut last_presented,
                    &mut last_failure,
                    animations_enabled,
                    step,
                );
            }
            if hidden {
                current_snapshot = None;
            }
        }

        if now.duration_since(last_health_check) >= OVERLAY_HEALTH_INTERVAL {
            last_health_check = now;
            animations_enabled = crate::system_preferences::client_area_animations_enabled();
            if let (Some(host), Some(snapshot)) = (host.as_mut(), current_snapshot.as_ref())
                && snapshot.requested_visible
                && let Err(failure) = host.health_check(snapshot)
            {
                host.hide();
                emit_failure_once(&event_sink, failure, &mut last_failure);
                emit_presented_if_changed(&event_sink, false, None, &mut last_presented);
            }
        }
        let wait = current_snapshot
            .as_ref()
            .map_or(OVERLAY_THREAD_IDLE_INTERVAL, |snapshot| {
                transitions.next_wait(
                    now,
                    animations_enabled
                        && snapshot.requested_visible
                        && snapshot.state.phase.is_progressing(),
                )
            });
        thread::park_timeout(wait);
    }

    if let Some(host) = host.as_mut() {
        host.hide();
    }
    pump_overlay_messages();
    emit_presented_if_changed(&event_sink, false, None, &mut last_presented);
}

fn process_transition_step(
    host: &mut Option<NativeOverlayHost>,
    snapshot: &OverlaySnapshot,
    event_sink: &NativeEventSink,
    last_presented: &mut Option<(bool, Option<SessionId>)>,
    last_failure: &mut Option<NativeOverlayFailure>,
    animations_enabled: bool,
    step: TransitionStep,
) {
    match step {
        TransitionStep::Render(plan) => process_snapshot(
            host,
            snapshot,
            event_sink,
            last_presented,
            last_failure,
            animations_enabled,
            Some(plan),
        ),
        TransitionStep::Hidden => {
            if let Some(host) = host.as_mut() {
                host.hide();
            }
            emit_presented_if_changed(event_sink, false, None, last_presented);
        }
        TransitionStep::Idle => {}
    }
}

fn process_snapshot(
    host: &mut Option<NativeOverlayHost>,
    snapshot: &OverlaySnapshot,
    event_sink: &NativeEventSink,
    last_presented: &mut Option<(bool, Option<SessionId>)>,
    last_failure: &mut Option<NativeOverlayFailure>,
    animations_enabled: bool,
    plan: Option<RenderPlan>,
) {
    let mut render_snapshot = plan
        .as_ref()
        .map(|plan| plan.target.clone())
        .unwrap_or_else(|| snapshot.clone());
    let control_enabled = render_snapshot.control_requested;
    if plan.as_ref().is_some_and(|plan| plan.reserve_cancel_region) {
        // Keep the independent 44px surface in place until the content
        // crossfade finishes, but revoke both pointer and UIA actions now.
        render_snapshot.control_requested = true;
    }
    if !render_snapshot.requested_visible || plan.as_ref().is_some_and(|plan| !plan.visible) {
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
    let animation_frame = overlay_animation_frame(
        &render_snapshot,
        animations_enabled,
        plan.as_ref().is_some_and(|plan| plan.animated),
    );
    match host
        .as_mut()
        .expect("native overlay host initialized")
        .apply_snapshot(
            &render_snapshot,
            animation_frame,
            plan.as_ref(),
            control_enabled,
        ) {
        Ok(visible) => {
            *last_failure = None;
            emit_presented_if_changed(
                event_sink,
                visible,
                render_snapshot.state.session_id,
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

fn overlay_animation_frame(
    snapshot: &OverlaySnapshot,
    animations_enabled: bool,
    transition_active: bool,
) -> u8 {
    if !animations_enabled || (!snapshot.state.phase.is_progressing() && !transition_active) {
        return 0;
    }
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    ((elapsed / FRAME_INTERVAL.as_millis()) % u128::from(u8::MAX)) as u8
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
            let service = match NativeOverlayService::new(context) {
                Ok(service) => service,
                Err(failure) => {
                    return OverlayViewportOutput {
                        diagnostic: Some(failure.diagnostic()),
                        ..OverlayViewportOutput::default()
                    };
                }
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
    fn unchanged_display_render_keys_do_not_request_another_pixel_submission() {
        let snapshot = snapshot_for_test();
        let first = snapshot.display_render_key(0);
        let same = snapshot.display_render_key(0);
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
    fn display_and_control_render_keys_cover_their_respective_inputs() {
        let snapshot = snapshot_for_test();
        let display_baseline = snapshot.display_render_key(2);
        let control_baseline = snapshot.control_render_key();

        let mut same_buckets = snapshot.clone();
        same_buckets.state.audio_level.rms = 0.001;
        same_buckets.state.audio_level.peak = 0.001;
        assert_eq!(
            display_baseline.rms_bucket,
            same_buckets.display_render_key(2).rms_bucket
        );

        let mut content_changed = snapshot.clone();
        content_changed.state.transcript.committed = "different".to_owned();
        assert_ne!(display_baseline, content_changed.display_render_key(2));
        assert_eq!(control_baseline, content_changed.control_render_key());

        let mut level_changed = snapshot.clone();
        level_changed.state.audio_level.rms = 1.0;
        assert_ne!(display_baseline, level_changed.display_render_key(2));
        assert_eq!(control_baseline, level_changed.control_render_key());

        let mut theme_changed = snapshot.clone();
        theme_changed.dark_mode = false;
        assert_ne!(display_baseline, theme_changed.display_render_key(2));
        assert_ne!(control_baseline, theme_changed.control_render_key());

        let mut dpi_changed = snapshot.clone();
        dpi_changed.dpi = 144;
        assert_ne!(display_baseline, dpi_changed.display_render_key(2));
        assert_ne!(control_baseline, dpi_changed.control_render_key());

        let mut display_bounds_changed = snapshot.clone();
        display_bounds_changed.display_bounds.as_mut().unwrap().x += 1;
        assert_ne!(
            display_baseline,
            display_bounds_changed.display_render_key(2)
        );
        assert_eq!(
            control_baseline,
            display_bounds_changed.control_render_key()
        );

        let mut control_bounds_changed = snapshot.clone();
        control_bounds_changed.control_bounds.as_mut().unwrap().x += 1;
        assert_eq!(
            display_baseline,
            control_bounds_changed.display_render_key(2)
        );
        assert_ne!(
            control_baseline,
            control_bounds_changed.control_render_key()
        );
        assert_ne!(display_baseline, snapshot.display_render_key(3));
        assert_eq!(control_baseline, snapshot.control_render_key());
    }

    #[test]
    fn meter_or_transcript_changes_do_not_resubmit_control_pixels() {
        let snapshot = snapshot_for_test();
        let display_baseline = snapshot.display_render_key(0);
        let control_baseline = snapshot.control_render_key();

        let mut meter_changed = snapshot.clone();
        meter_changed.state.audio_level.rms = 0.8;
        meter_changed.state.audio_level.peak = 0.9;
        let meter_invalidation = pixel_invalidation(
            Some(&display_baseline),
            Some(&control_baseline),
            &meter_changed.display_render_key(0),
            &meter_changed.control_render_key(),
        );
        assert!(meter_invalidation.display_pixels);
        assert!(!meter_invalidation.control_changed);
        assert!(!meter_invalidation.control_pixels);

        let mut transcript_changed = snapshot;
        transcript_changed.state.transcript.committed = "new words".to_owned();
        transcript_changed.state.transcript.revision += 1;
        let transcript_invalidation = pixel_invalidation(
            Some(&display_baseline),
            Some(&control_baseline),
            &transcript_changed.display_render_key(0),
            &transcript_changed.control_render_key(),
        );
        assert!(transcript_invalidation.display_pixels);
        assert!(!transcript_invalidation.control_changed);
        assert!(!transcript_invalidation.control_pixels);
    }

    #[test]
    fn animation_only_updates_do_not_change_accessibility_semantics() {
        let snapshot = snapshot_for_test();
        let key = |snapshot: &OverlaySnapshot| DisplayAccessibilityKey {
            mode: snapshot.state.mode,
            phase: snapshot.state.phase,
            content: OverlayRenderContent::from(&snapshot.state),
            bounds: snapshot.display_bounds.unwrap(),
        };
        let baseline = key(&snapshot);

        let mut meter_frame = snapshot.clone();
        meter_frame.state.audio_level.rms = 0.95;
        meter_frame.state.audio_level.peak = 0.99;
        assert_eq!(baseline, key(&meter_frame));

        let mut semantic_frame = snapshot;
        semantic_frame.state.phase = super::super::controller::OverlayPhase::Finalizing;
        assert_ne!(baseline, key(&semantic_frame));
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

        let app_diagnostic = failure.diagnostic();
        assert_eq!(app_diagnostic.code(), "native-overlay-raster");
        assert!(!app_diagnostic.status_message().contains("HWND"));
        assert!(!app_diagnostic.settings_diagnostic().contains("HWND"));
        assert!(!app_diagnostic.settings_diagnostic().contains("transcript"));
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
        let presentation = Arc::new(Mutex::new(NativePresentationObservation::default()));
        let retained_action = Arc::new(Mutex::new(None));
        let event_sink = NativeEventSink {
            tx,
            repaint_context: eframe::egui::Context::default(),
            presentation,
            retained_action,
        };
        let bridge = Arc::new(ControlActionBridge {
            session_id: Mutex::new(Some(SessionId(91))),
            event_sink: event_sink.clone(),
        });
        let pair_failure = Arc::new(PairFailureBridge {
            windows: Mutex::new(OverlayWindowPair::default()),
            recovery_requested: AtomicBool::new(false),
            action_bridge: Arc::clone(&bridge),
            event_sink,
        });
        let state = WindowProcedureState {
            role: WindowRole::Control,
            action_bridge: Some(Arc::clone(&bridge)),
            pair_failure,
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
    fn action_bridge_binds_the_session_at_event_time() {
        let (tx, rx) = bounded(4);
        let retained_action = Arc::new(Mutex::new(None));
        let bridge = ControlActionBridge {
            session_id: Mutex::new(None),
            event_sink: NativeEventSink {
                tx,
                repaint_context: eframe::egui::Context::default(),
                presentation: Arc::new(Mutex::new(NativePresentationObservation::default())),
                retained_action: Arc::clone(&retained_action),
            },
        };
        bridge.emit_abandon();
        assert!(rx.try_recv().is_err());

        bridge.bind(Some(SessionId(41)));
        bridge.emit_abandon();
        assert_eq!(
            retained_action.lock().unwrap().take(),
            Some(OverlayAction::Abandon(SessionId(41)))
        );
        assert!(rx.try_recv().is_err());

        bridge.bind(None);
        bridge.emit_abandon();
        assert!(retained_action.lock().unwrap().is_none());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn disabled_cancel_surface_is_click_through_while_its_bounds_are_reserved() {
        assert_eq!(
            control_hit_test(WindowRole::Control, false),
            HTTRANSPARENT as LRESULT
        );
        assert_eq!(
            control_hit_test(WindowRole::Control, true),
            HTCLIENT as LRESULT
        );
        assert_eq!(
            control_hit_test(WindowRole::Display, false),
            HTTRANSPARENT as LRESULT
        );
    }

    #[test]
    fn saturated_event_channel_retains_the_latest_cancel_action() {
        let (tx, rx) = bounded(1);
        let retained_action = Arc::new(Mutex::new(None));
        let event_sink = NativeEventSink {
            tx,
            repaint_context: eframe::egui::Context::default(),
            presentation: Arc::new(Mutex::new(NativePresentationObservation::default())),
            retained_action: Arc::clone(&retained_action),
        };
        event_sink.emit(NativeOverlayEvent::Failure(NativeOverlayFailure::new(
            NativeOverlayFailureStage::Visibility,
            None,
        )));
        event_sink.emit_action(OverlayAction::Abandon(SessionId(52)));

        assert!(matches!(rx.try_recv(), Ok(NativeOverlayEvent::Failure(_))));
        assert_eq!(
            retained_action.lock().unwrap().take(),
            Some(OverlayAction::Abandon(SessionId(52)))
        );
    }

    #[test]
    fn action_slot_delivers_each_cancel_only_once() {
        let (tx, events) = bounded(1);
        let presentation = Arc::new(Mutex::new(NativePresentationObservation::default()));
        let retained_action = Arc::new(Mutex::new(None));
        let event_sink = NativeEventSink {
            tx,
            repaint_context: eframe::egui::Context::default(),
            presentation: Arc::clone(&presentation),
            retained_action: Arc::clone(&retained_action),
        };
        let mut service = NativeOverlayService {
            mailbox: Arc::new(SnapshotMailbox {
                latest: Mutex::new(None),
                shutdown: AtomicBool::new(false),
            }),
            events,
            worker: None,
            presentation,
            retained_action,
            pending_action: None,
            pending_diagnostic: None,
            last_reported_diagnostic: None,
        };

        event_sink.emit_action(OverlayAction::Abandon(SessionId(63)));
        service.poll_events();
        assert_eq!(
            service.pending_action.take(),
            Some(OverlayAction::Abandon(SessionId(63)))
        );

        service.poll_events();
        assert_eq!(service.pending_action.take(), None);
    }

    #[test]
    fn service_propagates_each_failure_diagnostic_once_until_a_presentation_recovers() {
        let (tx, events) = bounded(8);
        let presentation = Arc::new(Mutex::new(NativePresentationObservation::default()));
        let retained_action = Arc::new(Mutex::new(None));
        let event_sink = NativeEventSink {
            tx,
            repaint_context: eframe::egui::Context::default(),
            presentation: Arc::clone(&presentation),
            retained_action: Arc::clone(&retained_action),
        };
        let mut service = NativeOverlayService {
            mailbox: Arc::new(SnapshotMailbox {
                latest: Mutex::new(None),
                shutdown: AtomicBool::new(false),
            }),
            events,
            worker: None,
            presentation,
            retained_action,
            pending_action: None,
            pending_diagnostic: None,
            last_reported_diagnostic: None,
        };
        let snapshot = snapshot_for_test();

        event_sink.emit(NativeOverlayEvent::Failure(NativeOverlayFailure::new(
            NativeOverlayFailureStage::Rasterization,
            Some(WindowRole::Display),
        )));
        let first = service.output_for(&snapshot);
        assert_eq!(first.diagnostic, Some(OverlayDiagnostic::Rasterization));

        event_sink.emit(NativeOverlayEvent::Failure(NativeOverlayFailure::new(
            NativeOverlayFailureStage::Rasterization,
            Some(WindowRole::Control),
        )));
        assert_eq!(service.output_for(&snapshot).diagnostic, None);

        event_sink.emit(NativeOverlayEvent::Presented {
            visible: true,
            session_id: snapshot.state.session_id,
        });
        assert_eq!(service.output_for(&snapshot).diagnostic, None);

        event_sink.emit(NativeOverlayEvent::Failure(NativeOverlayFailure::new(
            NativeOverlayFailureStage::Rasterization,
            Some(WindowRole::Display),
        )));
        assert_eq!(
            service.output_for(&snapshot).diagnostic,
            Some(OverlayDiagnostic::Rasterization)
        );
    }

    #[test]
    fn harness_documentation_describes_the_native_service_thread_contract() {
        let harness = include_str!("../../docs/UI_HARNESS.md");
        assert!(harness.contains("dedicated `scribe-native-overlay` service thread"));
        assert!(!harness.contains("layered windows on the UI thread"));
    }

    #[test]
    fn pair_failure_bridge_revokes_actions_and_marks_the_pair_hidden() {
        let (tx, _rx) = bounded(4);
        let presentation = Arc::new(Mutex::new(NativePresentationObservation {
            visible: true,
            session_id: Some(SessionId(81)),
        }));
        let event_sink = NativeEventSink {
            tx,
            repaint_context: eframe::egui::Context::default(),
            presentation: Arc::clone(&presentation),
            retained_action: Arc::new(Mutex::new(None)),
        };
        let action_bridge = Arc::new(ControlActionBridge {
            session_id: Mutex::new(Some(SessionId(81))),
            event_sink: event_sink.clone(),
        });
        let pair = PairFailureBridge {
            windows: Mutex::new(OverlayWindowPair::default()),
            recovery_requested: AtomicBool::new(false),
            action_bridge: Arc::clone(&action_bridge),
            event_sink,
        };

        pair.fail_closed(WindowRole::Display);
        action_bridge.emit_abandon();

        assert_eq!(
            *presentation.lock().unwrap(),
            NativePresentationObservation::default()
        );
        assert!(pair.take_recovery_request());
        assert!(action_bridge.session_id.lock().unwrap().is_none());
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
