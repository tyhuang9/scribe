use std::fmt;

#[derive(Clone, Copy, Eq, PartialEq)]
struct TargetIdentity {
    native_handle: isize,
    process_id: u32,
}

/// Opaque identity for the external window that had focus when dictation
/// started. Callers can retain and compare it, but cannot retarget it.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct CapturedTarget {
    identity: TargetIdentity,
}

impl fmt::Debug for CapturedTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CapturedTarget")
            .finish_non_exhaustive()
    }
}

impl CapturedTarget {
    #[cfg(test)]
    pub(crate) fn for_test(native_handle: isize, process_id: u32) -> Self {
        Self {
            identity: TargetIdentity {
                native_handle,
                process_id,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum OverlayPosition {
    TopCenter,
    #[default]
    BottomCenter,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OverlayWindowSpec {
    pub width_points: f32,
    pub height_points: f32,
    pub margin_points: f32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PhysicalWorkArea {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OverlayWindowBounds {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

pub fn calculate_window_bounds(
    work_area: PhysicalWorkArea,
    dpi: u32,
    spec: OverlayWindowSpec,
    position: OverlayPosition,
) -> OverlayWindowBounds {
    let effective_dpi = if dpi == 0 { 96 } else { dpi };
    let scale = effective_dpi as f32 / 96.0;
    let width = points_to_pixels(spec.width_points, scale);
    let height = points_to_pixels(spec.height_points, scale);
    let margin = points_to_pixels(spec.margin_points, scale);
    let work_width = (work_area.right - work_area.left).max(0);
    let work_height = (work_area.bottom - work_area.top).max(0);
    let available_vertical_space = (work_height - height).max(0);
    let margin = margin.min(available_vertical_space);

    let x = work_area.left + (work_width - width).max(0) / 2;
    let y = match position {
        OverlayPosition::TopCenter => work_area.top + margin,
        OverlayPosition::BottomCenter => work_area.bottom - height - margin,
    };

    OverlayWindowBounds {
        x,
        y: y.max(work_area.top),
        width,
        height,
    }
}

fn points_to_pixels(points: f32, scale: f32) -> i32 {
    if !points.is_finite() || points <= 0.0 {
        1
    } else {
        (points * scale).round().max(1.0) as i32
    }
}

pub fn capture_foreground_target() -> Option<CapturedTarget> {
    imp::capture_foreground_target()
}

pub fn captured_target_is_foreground(target: &CapturedTarget) -> bool {
    imp::captured_target_is_foreground(target)
}

pub fn overlay_window_bounds(
    target: Option<&CapturedTarget>,
    spec: OverlayWindowSpec,
    position: OverlayPosition,
) -> Option<OverlayWindowBounds> {
    imp::overlay_window_bounds(target, spec, position)
}

pub fn harden_overlay_window(
    exact_title: &str,
    target: Option<&CapturedTarget>,
    spec: OverlayWindowSpec,
    position: OverlayPosition,
    visible: bool,
) -> bool {
    imp::harden_overlay_window(exact_title, target, spec, position, visible)
}

pub fn reduced_motion_preferred() -> bool {
    imp::reduced_motion_preferred()
}

/// Whether this platform adapter can enforce non-activation and reject Scribe
/// windows as output targets. Callers should force the effective mode to Off
/// when this returns false.
pub fn overlay_focus_safety_available() -> bool {
    imp::overlay_focus_safety_available()
}

#[cfg(target_os = "windows")]
mod imp {
    use std::ffi::c_void;
    use std::mem::size_of;
    use std::process;

    use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM, POINT, RECT};
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint, MonitorFromWindow,
    };
    use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GWL_EXSTYLE, GetCursorPos, GetForegroundWindow, GetWindowLongPtrW,
        GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, HWND_TOPMOST,
        SPI_GETCLIENTAREAANIMATION, SWP_FRAMECHANGED, SWP_HIDEWINDOW, SWP_NOACTIVATE,
        SWP_SHOWWINDOW, SetWindowLongPtrW, SetWindowPos, SystemParametersInfoW, WS_EX_NOACTIVATE,
        WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
    };

    use super::{
        CapturedTarget, OverlayPosition, OverlayWindowBounds, OverlayWindowSpec, PhysicalWorkArea,
        TargetIdentity, calculate_window_bounds,
    };

    trait ForegroundWindowProbe {
        fn foreground_identity(&mut self) -> Option<(isize, u32)>;
    }

    struct SystemForegroundWindowProbe;

    impl ForegroundWindowProbe for SystemForegroundWindowProbe {
        fn foreground_identity(&mut self) -> Option<(isize, u32)> {
            let window = unsafe { GetForegroundWindow() };
            if window.is_null() {
                return None;
            }
            window_process_id(window).map(|process_id| (window as isize, process_id))
        }
    }

    pub(super) fn capture_foreground_target() -> Option<CapturedTarget> {
        capture_external_target_with(&mut SystemForegroundWindowProbe, process::id())
    }

    fn capture_external_target_with<P>(
        probe: &mut P,
        current_process_id: u32,
    ) -> Option<CapturedTarget>
    where
        P: ForegroundWindowProbe,
    {
        let (native_handle, process_id) = probe.foreground_identity()?;
        if native_handle == 0 || process_id == 0 || process_id == current_process_id {
            return None;
        }
        Some(CapturedTarget {
            identity: TargetIdentity {
                native_handle,
                process_id,
            },
        })
    }

    pub(super) fn captured_target_is_foreground(target: &CapturedTarget) -> bool {
        capture_foreground_target().is_some_and(|current| current == *target)
    }

    pub(super) fn overlay_window_bounds(
        target: Option<&CapturedTarget>,
        spec: OverlayWindowSpec,
        position: OverlayPosition,
    ) -> Option<OverlayWindowBounds> {
        let (monitor, dpi) = monitor_and_dpi(target)?;
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            rcMonitor: RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            rcWork: RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            dwFlags: 0,
        };
        if unsafe { GetMonitorInfoW(monitor, &mut info) } == 0 {
            return None;
        }

        Some(calculate_window_bounds(
            PhysicalWorkArea {
                left: info.rcWork.left,
                top: info.rcWork.top,
                right: info.rcWork.right,
                bottom: info.rcWork.bottom,
            },
            dpi,
            spec,
            position,
        ))
    }

    pub(super) fn harden_overlay_window(
        exact_title: &str,
        target: Option<&CapturedTarget>,
        spec: OverlayWindowSpec,
        position: OverlayPosition,
        visible: bool,
    ) -> bool {
        let Some(window) = find_current_process_window_by_exact_title(exact_title) else {
            return false;
        };

        let current_style = unsafe { GetWindowLongPtrW(window, GWL_EXSTYLE) };
        let hardened_style = current_style
            | WS_EX_NOACTIVATE as isize
            | WS_EX_TOOLWINDOW as isize
            | WS_EX_TRANSPARENT as isize;
        if hardened_style != current_style {
            unsafe {
                SetWindowLongPtrW(window, GWL_EXSTYLE, hardened_style);
            }
        }
        let applied_style = unsafe { GetWindowLongPtrW(window, GWL_EXSTYLE) };
        if applied_style & hardened_style != hardened_style {
            return false;
        }

        let Some(bounds) = overlay_window_bounds(target, spec, position) else {
            return false;
        };
        let visibility_flag = if visible {
            SWP_SHOWWINDOW
        } else {
            SWP_HIDEWINDOW
        };
        unsafe {
            SetWindowPos(
                window,
                HWND_TOPMOST,
                bounds.x,
                bounds.y,
                bounds.width,
                bounds.height,
                SWP_NOACTIVATE | SWP_FRAMECHANGED | visibility_flag,
            ) != 0
        }
    }

    pub(super) fn reduced_motion_preferred() -> bool {
        let mut animations_enabled: BOOL = 1;
        let succeeded = unsafe {
            SystemParametersInfoW(
                SPI_GETCLIENTAREAANIMATION,
                0,
                (&mut animations_enabled as *mut BOOL).cast::<c_void>(),
                0,
            )
        };
        succeeded != 0 && animations_enabled == 0
    }

    pub(super) fn overlay_focus_safety_available() -> bool {
        true
    }

    fn monitor_and_dpi(target: Option<&CapturedTarget>) -> Option<(*mut c_void, u32)> {
        if let Some(target) = target {
            let window = target.identity.native_handle as HWND;
            if window_process_id(window) == Some(target.identity.process_id) {
                let monitor = unsafe { MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST) };
                if !monitor.is_null() {
                    return Some((monitor, effective_monitor_dpi(monitor)));
                }
            }
        }

        let mut cursor = POINT { x: 0, y: 0 };
        if unsafe { GetCursorPos(&mut cursor) } == 0 {
            return None;
        }
        let monitor = unsafe { MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST) };
        if monitor.is_null() {
            return None;
        }
        Some((monitor, effective_monitor_dpi(monitor)))
    }

    fn effective_monitor_dpi(monitor: *mut c_void) -> u32 {
        let mut dpi_x = 0;
        let mut dpi_y = 0;
        let result =
            unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) };
        if result == 0 && dpi_x > 0 { dpi_x } else { 96 }
    }

    fn window_process_id(window: HWND) -> Option<u32> {
        if window.is_null() {
            return None;
        }
        let mut process_id = 0;
        unsafe {
            GetWindowThreadProcessId(window, &mut process_id);
        }
        (process_id != 0).then_some(process_id)
    }

    struct WindowSearch {
        process_id: u32,
        exact_title: Vec<u16>,
        found: HWND,
    }

    fn find_current_process_window_by_exact_title(exact_title: &str) -> Option<HWND> {
        let mut search = WindowSearch {
            process_id: process::id(),
            exact_title: exact_title.encode_utf16().collect(),
            found: std::ptr::null_mut(),
        };
        unsafe {
            EnumWindows(
                Some(search_window_callback),
                (&mut search as *mut WindowSearch) as LPARAM,
            );
        }
        (!search.found.is_null()).then_some(search.found)
    }

    unsafe extern "system" fn search_window_callback(window: HWND, parameter: LPARAM) -> BOOL {
        let search = unsafe { &mut *(parameter as *mut WindowSearch) };
        if window_process_id(window) != Some(search.process_id) {
            return 1;
        }

        let title_length = unsafe { GetWindowTextLengthW(window) };
        if title_length < 0 || title_length as usize != search.exact_title.len() {
            return 1;
        }
        let mut title = vec![0_u16; title_length as usize + 1];
        let copied = unsafe { GetWindowTextW(window, title.as_mut_ptr(), title.len() as i32) };
        if copied >= 0 && &title[..copied as usize] == search.exact_title.as_slice() {
            search.found = window;
            return 0;
        }
        1
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        struct FakeProbe(Option<(isize, u32)>);

        impl ForegroundWindowProbe for FakeProbe {
            fn foreground_identity(&mut self) -> Option<(isize, u32)> {
                self.0
            }
        }

        #[test]
        fn capture_rejects_every_window_owned_by_current_process() {
            let mut probe = FakeProbe(Some((44, 9001)));

            assert_eq!(capture_external_target_with(&mut probe, 9001), None);
        }

        #[test]
        fn capture_accepts_an_external_window_identity() {
            let mut probe = FakeProbe(Some((44, 9002)));

            let target = capture_external_target_with(&mut probe, 9001).unwrap();

            assert_eq!(target.identity.native_handle, 44);
            assert_eq!(target.identity.process_id, 9002);
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod imp {
    use super::{CapturedTarget, OverlayPosition, OverlayWindowBounds, OverlayWindowSpec};

    pub(super) fn capture_foreground_target() -> Option<CapturedTarget> {
        None
    }

    pub(super) fn captured_target_is_foreground(_target: &CapturedTarget) -> bool {
        false
    }

    pub(super) fn overlay_window_bounds(
        _target: Option<&CapturedTarget>,
        _spec: OverlayWindowSpec,
        _position: OverlayPosition,
    ) -> Option<OverlayWindowBounds> {
        None
    }

    pub(super) fn harden_overlay_window(
        _exact_title: &str,
        _target: Option<&CapturedTarget>,
        _spec: OverlayWindowSpec,
        _position: OverlayPosition,
        _visible: bool,
    ) -> bool {
        false
    }

    pub(super) fn reduced_motion_preferred() -> bool {
        false
    }

    pub(super) fn overlay_focus_safety_available() -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_top_center_with_negative_monitor_coordinates() {
        let bounds = calculate_window_bounds(
            PhysicalWorkArea {
                left: -1920,
                top: -80,
                right: 0,
                bottom: 1000,
            },
            96,
            OverlayWindowSpec {
                width_points: 440.0,
                height_points: 140.0,
                margin_points: 24.0,
            },
            OverlayPosition::TopCenter,
        );

        assert_eq!(
            bounds,
            OverlayWindowBounds {
                x: -1180,
                y: -56,
                width: 440,
                height: 140
            }
        );
    }

    #[test]
    fn positions_bottom_center_using_target_monitor_dpi() {
        let bounds = calculate_window_bounds(
            PhysicalWorkArea {
                left: 1920,
                top: 0,
                right: 4480,
                bottom: 1400,
            },
            144,
            OverlayWindowSpec {
                width_points: 276.0,
                height_points: 52.0,
                margin_points: 24.0,
            },
            OverlayPosition::BottomCenter,
        );

        assert_eq!(
            bounds,
            OverlayWindowBounds {
                x: 2993,
                y: 1286,
                width: 414,
                height: 78
            }
        );
    }

    #[test]
    fn oversized_overlay_stays_anchored_inside_work_area_origin() {
        let bounds = calculate_window_bounds(
            PhysicalWorkArea {
                left: 10,
                top: 20,
                right: 110,
                bottom: 70,
            },
            96,
            OverlayWindowSpec {
                width_points: 200.0,
                height_points: 100.0,
                margin_points: 24.0,
            },
            OverlayPosition::BottomCenter,
        );

        assert_eq!(bounds.x, 10);
        assert_eq!(bounds.y, 20);
    }
}
