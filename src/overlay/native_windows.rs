#![allow(dead_code)]

mod accessibility;
mod raster;

use std::{
    cell::{Cell, RefCell},
    ffi::c_void,
    mem::{offset_of, size_of, zeroed},
    ptr::{null, null_mut},
    sync::{Arc, Mutex, OnceLock},
};

use crossbeam_channel::{Receiver, Sender, bounded};
use windows_sys::Win32::{
    Foundation::{GetLastError, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM},
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
            CREATESTRUCTW, CreateWindowExW, DefWindowProcW, DestroyWindow, GWL_EXSTYLE,
            GWLP_USERDATA, GetClientRect, GetWindowLongPtrW, HTCLIENT, HTTRANSPARENT, HWND_TOPMOST,
            IsWindowVisible, MA_NOACTIVATE, RegisterClassExW, SW_HIDE, SW_SHOWNOACTIVATE,
            SWP_HIDEWINDOW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE,
            SWP_NOZORDER, SWP_SHOWWINDOW, SendMessageW, SetWindowLongPtrW, SetWindowPos,
            ShowWindow, ULW_ALPHA, UpdateLayeredWindow, WM_CANCELMODE, WM_CAPTURECHANGED,
            WM_DESTROY, WM_GETOBJECT, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE, WM_NCCREATE,
            WM_NCDESTROY, WM_NCHITTEST, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
            WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
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

#[derive(Debug)]
struct ControlActionBridge {
    session_id: Mutex<Option<SessionId>>,
    tx: Sender<OverlayAction>,
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
            let _ = self.tx.try_send(OverlayAction::Abandon(session_id));
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
    ) -> bool {
        let updated = {
            let accessibility = self._procedure_state.accessibility.borrow();
            let Some(accessibility) = accessibility.as_ref() else {
                return false;
            };
            accessibility.update(state, visible, control_enabled)
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

    fn position_hidden(&self, bounds: OverlayWindowBounds) -> bool {
        self.hide();
        unsafe {
            SetWindowPos(
                self.hwnd,
                HWND_TOPMOST,
                bounds.x,
                bounds.y,
                bounds.width,
                bounds.height,
                SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            ) != 0
        }
    }

    fn submit_frame(&mut self, bounds: OverlayWindowBounds, frame: &LayeredFrame) -> bool {
        if frame.width != bounds.width || frame.height != bounds.height {
            return false;
        }
        let needs_surface = self
            .surface
            .as_ref()
            .is_none_or(|surface| surface.width != frame.width || surface.height != frame.height);
        if needs_surface {
            self.surface = LayeredSurface::new(frame.width, frame.height).ok();
        }
        self.surface
            .as_mut()
            .is_some_and(|surface| surface.submit(self.hwnd, bounds, frame))
    }

    fn show_no_activate(&mut self, bounds: OverlayWindowBounds) -> bool {
        if !self.is_hardened() {
            self.hide();
            return false;
        }
        if self.role == WindowRole::Control
            && !self
                .tooltip
                .as_mut()
                .is_some_and(|tooltip| tooltip.update_bounds(bounds.width, bounds.height))
        {
            self.hide();
            return false;
        }
        unsafe {
            ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
        }
        let placed = unsafe {
            SetWindowPos(
                self.hwnd,
                HWND_TOPMOST,
                bounds.x,
                bounds.y,
                bounds.width,
                bounds.height,
                SWP_NOACTIVATE | SWP_NOOWNERZORDER | SWP_SHOWWINDOW,
            ) != 0
        };
        let visible = placed && unsafe { IsWindowVisible(self.hwnd) != 0 };
        if !visible {
            self.hide();
        }
        visible
    }

    fn hide(&self) {
        self._procedure_state.cancel_press();
        if unsafe { GetCapture() } == self.hwnd {
            unsafe {
                ReleaseCapture();
            }
        }
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
            ShowWindow(self.hwnd, SW_HIDE);
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
            return Err(NativeTooltipError::InitializeCommonControls(unsafe {
                GetLastError()
            }));
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
            return Err(NativeTooltipError::CreateWindow(unsafe { GetLastError() }));
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
            return Err(NativeTooltipError::PositionWindow(unsafe {
                GetLastError()
            }));
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
    InitializeCommonControls(u32),
    ModuleHandle,
    CreateWindow(u32),
    RegisterTool,
    PositionWindow(u32),
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
    action_rx: Receiver<OverlayAction>,
}

impl NativeOverlayHost {
    fn new() -> Result<Self, NativeOverlayError> {
        let (action_tx, action_rx) = bounded(4);
        let action_bridge = Arc::new(ControlActionBridge {
            session_id: Mutex::new(None),
            tx: action_tx,
        });
        let display = NativeWindow::create(WindowRole::Display, None)?;
        let control = NativeWindow::create(WindowRole::Control, Some(Arc::clone(&action_bridge)))?;
        let rasterizer = NativeRasterizer::new()?;
        Ok(Self {
            display,
            control,
            rasterizer,
            action_bridge,
            action_rx,
        })
    }

    fn bind_cancellable_session(&self, session_id: Option<SessionId>) {
        self.action_bridge.bind(session_id);
    }

    fn next_action(&self) -> Option<OverlayAction> {
        let mut newest = None;
        while let Ok(action) = self.action_rx.try_recv() {
            newest = Some(action);
        }
        newest
    }

    #[allow(clippy::too_many_arguments)]
    fn present(
        &mut self,
        display_bounds: OverlayWindowBounds,
        display_frame: &LayeredFrame,
        control_requested: bool,
        control_bounds: OverlayWindowBounds,
        control_frame: &LayeredFrame,
        session_id: Option<SessionId>,
    ) -> NativePresentationResult {
        let display_presented = present_transaction(&mut WindowPresentation {
            window: &mut self.display,
            bounds: display_bounds,
            frame: display_frame,
        });
        if !display_presented {
            self.action_bridge.bind(None);
            self.control.hide();
            return NativePresentationResult::default();
        }

        let control_presented = if control_requested {
            let presented = present_transaction(&mut WindowPresentation {
                window: &mut self.control,
                bounds: control_bounds,
                frame: control_frame,
            });
            self.action_bridge
                .bind(presented.then_some(session_id).flatten());
            presented
        } else {
            self.action_bridge.bind(None);
            self.control.hide();
            false
        };
        combine_presentation_results(display_presented, control_requested, control_presented)
    }

    fn hide(&mut self) {
        self.action_bridge.bind(None);
        self.display.update_accessibility(None, false, false);
        self.control.update_accessibility(None, false, false);
        self.control.hide();
        self.display.hide();
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct NativePresentationResult {
    display_presented: bool,
    control_presented: bool,
}

trait PresentationTransaction {
    fn verify_hardening(&mut self) -> bool;
    fn submit_pixels(&mut self) -> bool;
    fn show_no_activate(&mut self) -> bool;
    fn is_visible(&mut self) -> bool;
    fn hide(&mut self);
}

struct WindowPresentation<'a> {
    window: &'a mut NativeWindow,
    bounds: OverlayWindowBounds,
    frame: &'a LayeredFrame,
}

impl PresentationTransaction for WindowPresentation<'_> {
    fn verify_hardening(&mut self) -> bool {
        self.window.is_hardened()
    }

    fn submit_pixels(&mut self) -> bool {
        self.window.submit_frame(self.bounds, self.frame)
    }

    fn show_no_activate(&mut self) -> bool {
        self.window.show_no_activate(self.bounds)
    }

    fn is_visible(&mut self) -> bool {
        unsafe { IsWindowVisible(self.window.hwnd) != 0 }
    }

    fn hide(&mut self) {
        self.window.hide();
    }
}

fn present_transaction(transaction: &mut impl PresentationTransaction) -> bool {
    let presented = transaction.verify_hardening()
        && transaction.submit_pixels()
        && transaction.show_no_activate()
        && transaction.is_visible();
    if !presented {
        transaction.hide();
    }
    presented
}

fn combine_presentation_results(
    display_presented: bool,
    control_requested: bool,
    control_presented: bool,
) -> NativePresentationResult {
    NativePresentationResult {
        display_presented,
        control_presented: display_presented && control_requested && control_presented,
    }
}

fn display_prerequisites_ready(rendered: bool, accessible: bool) -> bool {
    rendered && accessible
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

thread_local! {
    static NATIVE_OVERLAY_HOST: RefCell<Option<NativeOverlayHost>> = const { RefCell::new(None) };
}

pub(super) fn shutdown_overlay_viewport() {
    NATIVE_OVERLAY_HOST.with(|slot| {
        if let Some(mut host) = slot.borrow_mut().take() {
            host.hide();
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
    NATIVE_OVERLAY_HOST.with(|slot| {
        let mut slot = slot.borrow_mut();
        let requested_visible = state.is_visible() && presentation.permits_background_overlay();
        if !requested_visible {
            let action = slot.as_ref().and_then(NativeOverlayHost::next_action);
            if let Some(host) = slot.as_mut() {
                host.hide();
            }
            return OverlayViewportOutput {
                presented: false,
                action,
            };
        }

        let spec = window_spec(state.mode);
        let Some(display_bounds) = overlay_window_bounds(target, spec, position) else {
            let action = slot.as_ref().and_then(NativeOverlayHost::next_action);
            if let Some(host) = slot.as_mut() {
                host.hide();
            }
            return OverlayViewportOutput {
                presented: false,
                action,
            };
        };

        if slot.is_none() {
            let Ok(host) = NativeOverlayHost::new() else {
                return OverlayViewportOutput::default();
            };
            *slot = Some(host);
        }
        let host = slot.as_mut().expect("native overlay host initialized");
        let action = host.next_action();
        let dark_mode = context.style().visuals.dark_mode;
        let Ok(display_frame) = host.rasterizer.render_display(
            state,
            dark_mode,
            display_bounds.width,
            display_bounds.height,
        ) else {
            host.hide();
            return OverlayViewportOutput {
                presented: false,
                action,
            };
        };

        let control_bounds = control_window_bounds(display_bounds, spec);
        let control_requested = is_cancellable(state);
        let control_frame = control_requested
            .then(|| {
                host.rasterizer.render_control(
                    dark_mode,
                    control_bounds.width,
                    control_bounds.height,
                )
            })
            .transpose()
            .ok()
            .flatten();
        let control_rendered = control_frame.is_some();
        let display_accessible = host.display.update_accessibility(Some(state), true, false);
        if !display_prerequisites_ready(true, display_accessible) {
            host.hide();
            return OverlayViewportOutput {
                presented: false,
                action,
            };
        }
        let control_accessible = host.control.control_capabilities_ready()
            && host
                .control
                .update_accessibility(Some(state), control_requested, control_requested);
        let transparent_control = LayeredFrame {
            width: control_bounds.width,
            height: control_bounds.height,
            pixels: vec![
                0;
                frame_byte_len(control_bounds.width, control_bounds.height).unwrap_or(0)
            ],
        };
        let result = host.present(
            display_bounds,
            &display_frame,
            control_requested && control_rendered && control_accessible,
            control_bounds,
            control_frame.as_ref().unwrap_or(&transparent_control),
            state.session_id,
        );
        OverlayViewportOutput {
            presented: result.display_presented,
            action,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::super::platform::OverlayWindowSpec;
    use super::*;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum Step {
        Verify,
        Submit,
        Show,
        Visible,
        Hide,
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

        fn hide(&mut self) {
            self.steps.push(Step::Hide);
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
    fn presentation_requires_every_ordered_native_step() {
        let mut transaction = FakeTransaction::new(None);
        assert!(present_transaction(&mut transaction));
        assert_eq!(
            transaction.steps,
            vec![Step::Verify, Step::Submit, Step::Show, Step::Visible]
        );
    }

    #[test]
    fn every_required_presentation_failure_hides_fail_closed() {
        for failure in [Step::Verify, Step::Submit, Step::Show, Step::Visible] {
            let mut transaction = FakeTransaction::new(Some(failure));
            assert!(!present_transaction(&mut transaction));
            assert_eq!(transaction.steps.last(), Some(&Step::Hide));
            assert!(!transaction.steps.contains(&Step::Visible) || failure == Step::Visible);
        }
    }

    #[test]
    fn control_failure_does_not_revoke_a_presented_passive_display() {
        assert_eq!(
            combine_presentation_results(true, true, false),
            NativePresentationResult {
                display_presented: true,
                control_presented: false,
            }
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
            tx,
        };
        bridge.emit_abandon();
        assert!(rx.try_recv().is_err());

        bridge.bind(Some(SessionId(41)));
        bridge.emit_abandon();
        assert_eq!(rx.try_recv(), Ok(OverlayAction::Abandon(SessionId(41))));

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
