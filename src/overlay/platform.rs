use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TargetIdentity {
    native_handle: isize,
    thread_id: u32,
    process_id: u32,
    process_creation_time: u64,
}

/// Opaque identity for the external window that had focus when dictation
/// started. Callers can retain and compare it, but cannot retarget it.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct CapturedTarget {
    identity: TargetIdentity,
    window_property_token: usize,
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
                thread_id: 1,
                process_id,
                process_creation_time: 1,
            },
            window_property_token: 1,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OverlayHardeningProfile {
    PassThroughDisplay,
    NonActivatingControl,
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn hardened_overlay_ex_style(
    current: isize,
    no_activate: isize,
    tool_window: isize,
    transparent: isize,
    profile: OverlayHardeningProfile,
) -> isize {
    let base = current | no_activate | tool_window;
    match profile {
        OverlayHardeningProfile::PassThroughDisplay => base | transparent,
        OverlayHardeningProfile::NonActivatingControl => base & !transparent,
    }
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

/// Returns only the executable filename for a still-valid captured target.
/// Window titles and document paths are deliberately excluded.
pub fn captured_target_application_identity(target: &CapturedTarget) -> Option<String> {
    imp::captured_target_application_identity(target)
}

/// Requests foreground activation for the exact captured target, then verifies
/// it immediately. Windows may deny activation; that denial is not bypassed.
pub fn reactivate_and_verify_captured_target(target: &CapturedTarget) -> bool {
    imp::reactivate_and_verify_captured_target(target)
}

/// Retires Scribe's generation marker without disturbing a newer capture of
/// the same HWND. Window destruction also removes the scalar property.
pub fn release_captured_target(target: &CapturedTarget) {
    imp::release_captured_target(target);
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
    harden_overlay_window_with_profile(
        exact_title,
        target,
        spec,
        position,
        visible,
        OverlayHardeningProfile::PassThroughDisplay,
    )
}

pub fn harden_overlay_window_with_profile(
    exact_title: &str,
    target: Option<&CapturedTarget>,
    spec: OverlayWindowSpec,
    position: OverlayPosition,
    visible: bool,
    profile: OverlayHardeningProfile,
) -> bool {
    imp::harden_overlay_window(exact_title, target, spec, position, visible, profile)
}

pub fn harden_overlay_window_at(
    exact_title: &str,
    bounds: OverlayWindowBounds,
    visible: bool,
    profile: OverlayHardeningProfile,
) -> bool {
    imp::harden_overlay_window_at(exact_title, bounds, visible, profile)
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
    use std::mem::{size_of, zeroed};
    use std::process;
    use std::sync::{
        OnceLock,
        atomic::{AtomicUsize, Ordering},
    };

    use windows_sys::Win32::Foundation::{
        BOOL, CloseHandle, FILETIME, HWND, LPARAM, POINT, RECT, STILL_ACTIVE,
    };
    use windows_sys::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint, MonitorFromWindow,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    };
    use windows_sys::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GWL_EXSTYLE, GetCursorPos, GetForegroundWindow, GetPropW, GetWindowLongPtrW,
        GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, HWND_TOPMOST, IsWindow,
        SPI_GETCLIENTAREAANIMATION, SWP_FRAMECHANGED, SWP_HIDEWINDOW, SWP_NOACTIVATE,
        SWP_SHOWWINDOW, SetForegroundWindow, SetPropW, SetWindowLongPtrW, SetWindowPos,
        SystemParametersInfoW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
    };

    use super::{
        CapturedTarget, OverlayHardeningProfile, OverlayPosition, OverlayWindowBounds,
        OverlayWindowSpec, PhysicalWorkArea, TargetIdentity, calculate_window_bounds,
        hardened_overlay_ex_style,
    };

    trait CapturedTargetProbe {
        fn foreground_identity(&mut self) -> Option<TargetIdentity>;
        fn install_window_property(&mut self, identity: TargetIdentity) -> Option<usize>;
        fn window_property_matches(&mut self, token: usize, identity: TargetIdentity) -> bool;
        fn release_window_property(&mut self, token: usize, identity: TargetIdentity);
        fn identity_for_window(&mut self, native_handle: isize) -> Option<TargetIdentity>;
        fn set_foreground_window(&mut self, native_handle: isize) -> bool;
    }

    struct SystemCapturedTargetProbe;

    impl CapturedTargetProbe for SystemCapturedTargetProbe {
        fn foreground_identity(&mut self) -> Option<TargetIdentity> {
            let window = unsafe { GetForegroundWindow() };
            window_identity(window)
        }

        fn identity_for_window(&mut self, native_handle: isize) -> Option<TargetIdentity> {
            window_identity(native_handle as HWND)
        }

        fn install_window_property(&mut self, identity: TargetIdentity) -> Option<usize> {
            install_window_property(identity)
        }

        fn window_property_matches(&mut self, token: usize, identity: TargetIdentity) -> bool {
            window_property_matches(token, identity)
        }

        fn release_window_property(&mut self, token: usize, identity: TargetIdentity) {
            remove_window_property_if_matches(token, identity);
        }

        fn set_foreground_window(&mut self, native_handle: isize) -> bool {
            unsafe { SetForegroundWindow(native_handle as HWND) != 0 }
        }
    }

    pub(super) fn capture_foreground_target() -> Option<CapturedTarget> {
        capture_external_target_with(&mut SystemCapturedTargetProbe, process::id())
    }

    fn capture_external_target_with<P>(
        probe: &mut P,
        current_process_id: u32,
    ) -> Option<CapturedTarget>
    where
        P: CapturedTargetProbe,
    {
        let identity = probe.foreground_identity()?;
        if !is_external_identity(identity, current_process_id) {
            return None;
        }
        let window_property_token = probe.install_window_property(identity)?;
        let target = CapturedTarget {
            identity,
            window_property_token,
        };
        if probe.foreground_identity() == Some(identity)
            && probe.window_property_matches(window_property_token, identity)
        {
            Some(target)
        } else {
            probe.release_window_property(window_property_token, identity);
            None
        }
    }

    pub(super) fn captured_target_is_foreground(target: &CapturedTarget) -> bool {
        captured_target_is_foreground_with(&mut SystemCapturedTargetProbe, process::id(), target)
    }

    pub(super) fn captured_target_application_identity(target: &CapturedTarget) -> Option<String> {
        if !captured_target_is_valid(target) {
            return None;
        }
        let process = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                target.identity.process_id,
            )
        };
        if process.is_null() {
            return None;
        }

        let result = (|| {
            let mut path = vec![0_u16; 32_768];
            let mut length = u32::try_from(path.len()).ok()?;
            if unsafe { QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut length) }
                == 0
            {
                return None;
            }
            path.truncate(length as usize);
            let filename = std::path::Path::new(&String::from_utf16_lossy(&path))
                .file_name()?
                .to_string_lossy()
                .trim()
                .to_owned();
            (!filename.is_empty()).then_some(filename)
        })();
        unsafe {
            CloseHandle(process);
        }
        result
    }

    pub(super) fn captured_target_is_valid(target: &CapturedTarget) -> bool {
        captured_target_is_valid_with(&mut SystemCapturedTargetProbe, process::id(), target)
    }

    pub(super) fn reactivate_and_verify_captured_target(target: &CapturedTarget) -> bool {
        reactivate_and_verify_captured_target_with(
            &mut SystemCapturedTargetProbe,
            process::id(),
            target,
        )
    }

    pub(super) fn release_captured_target(target: &CapturedTarget) {
        remove_window_property_if_matches(target.window_property_token, target.identity);
    }

    fn captured_target_is_valid_with<P>(
        probe: &mut P,
        current_process_id: u32,
        target: &CapturedTarget,
    ) -> bool
    where
        P: CapturedTargetProbe,
    {
        is_external_identity(target.identity, current_process_id)
            && probe.identity_for_window(target.identity.native_handle) == Some(target.identity)
            && probe.window_property_matches(target.window_property_token, target.identity)
    }

    fn captured_target_is_foreground_with<P>(
        probe: &mut P,
        current_process_id: u32,
        target: &CapturedTarget,
    ) -> bool
    where
        P: CapturedTargetProbe,
    {
        probe.foreground_identity() == Some(target.identity)
            && captured_target_is_valid_with(probe, current_process_id, target)
    }

    fn reactivate_and_verify_captured_target_with<P>(
        probe: &mut P,
        current_process_id: u32,
        target: &CapturedTarget,
    ) -> bool
    where
        P: CapturedTargetProbe,
    {
        captured_target_is_valid_with(probe, current_process_id, target)
            && probe.set_foreground_window(target.identity.native_handle)
            && captured_target_is_foreground_with(probe, current_process_id, target)
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
        profile: OverlayHardeningProfile,
    ) -> bool {
        let Some(bounds) = overlay_window_bounds(target, spec, position) else {
            return false;
        };
        harden_overlay_window_at(exact_title, bounds, visible, profile)
    }

    pub(super) fn harden_overlay_window_at(
        exact_title: &str,
        bounds: OverlayWindowBounds,
        visible: bool,
        profile: OverlayHardeningProfile,
    ) -> bool {
        let Some(window) = find_current_process_window_by_exact_title(exact_title) else {
            return false;
        };

        let current_style = unsafe { GetWindowLongPtrW(window, GWL_EXSTYLE) };
        let hardened_style = hardened_overlay_ex_style(
            current_style,
            WS_EX_NOACTIVATE as isize,
            WS_EX_TOOLWINDOW as isize,
            WS_EX_TRANSPARENT as isize,
            profile,
        );
        if hardened_style != current_style {
            unsafe {
                SetWindowLongPtrW(window, GWL_EXSTYLE, hardened_style);
            }
        }
        let applied_style = unsafe { GetWindowLongPtrW(window, GWL_EXSTYLE) };
        let profile_applied = match profile {
            OverlayHardeningProfile::PassThroughDisplay => {
                applied_style & WS_EX_TRANSPARENT as isize != 0
            }
            OverlayHardeningProfile::NonActivatingControl => {
                applied_style & WS_EX_TRANSPARENT as isize == 0
            }
        };
        if applied_style & (WS_EX_NOACTIVATE as isize | WS_EX_TOOLWINDOW as isize)
            != (WS_EX_NOACTIVATE as isize | WS_EX_TOOLWINDOW as isize)
            || !profile_applied
        {
            return false;
        }

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
            if !captured_target_is_valid(target) {
                return None;
            }
            let window = target.identity.native_handle as HWND;
            let monitor = unsafe { MonitorFromWindow(window, MONITOR_DEFAULTTONEAREST) };
            if !monitor.is_null() {
                return Some((monitor, effective_monitor_dpi(monitor)));
            }
            return None;
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

    static TARGET_WITNESS_PROPERTY: OnceLock<Vec<u16>> = OnceLock::new();
    static NEXT_TARGET_WITNESS: AtomicUsize = AtomicUsize::new(1);

    fn target_witness_property_name() -> &'static [u16] {
        TARGET_WITNESS_PROPERTY.get_or_init(|| {
            let creation_time = process_creation_time(process::id()).unwrap_or(0);
            format!("Scribe.TargetWitness.{}.{}", process::id(), creation_time)
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect()
        })
    }

    fn install_window_property(identity: TargetIdentity) -> Option<usize> {
        let token = NEXT_TARGET_WITNESS
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1).filter(|next| *next != 0)
            })
            .ok()?;
        let window = identity.native_handle as HWND;
        let value = token as *mut c_void;
        if unsafe { SetPropW(window, target_witness_property_name().as_ptr(), value) } == 0 {
            return None;
        }
        window_property_matches(token, identity).then_some(token)
    }

    fn window_property_matches(token: usize, identity: TargetIdentity) -> bool {
        token != 0
            && unsafe {
                GetPropW(
                    identity.native_handle as HWND,
                    target_witness_property_name().as_ptr(),
                ) == token as *mut c_void
            }
    }

    fn remove_window_property_if_matches(token: usize, identity: TargetIdentity) {
        use windows_sys::Win32::UI::WindowsAndMessaging::RemovePropW;

        if window_property_matches(token, identity) {
            let _ = unsafe {
                RemovePropW(
                    identity.native_handle as HWND,
                    target_witness_property_name().as_ptr(),
                )
            };
        }
    }

    fn is_external_identity(identity: TargetIdentity, current_process_id: u32) -> bool {
        identity.native_handle != 0
            && identity.thread_id != 0
            && identity.process_id != 0
            && identity.process_creation_time != 0
            && identity.process_id != current_process_id
    }

    fn window_identity(window: HWND) -> Option<TargetIdentity> {
        if window.is_null() || unsafe { IsWindow(window) } == 0 {
            return None;
        }
        let (thread_id, process_id) = window_thread_process_id(window)?;
        let process_creation_time = process_creation_time(process_id)?;
        Some(TargetIdentity {
            native_handle: window as isize,
            thread_id,
            process_id,
            process_creation_time,
        })
    }

    fn window_thread_process_id(window: HWND) -> Option<(u32, u32)> {
        if window.is_null() {
            return None;
        }
        let mut process_id = 0;
        let thread_id = unsafe { GetWindowThreadProcessId(window, &mut process_id) };
        (thread_id != 0 && process_id != 0).then_some((thread_id, process_id))
    }

    fn process_creation_time(process_id: u32) -> Option<u64> {
        if process_id == 0 {
            return None;
        }
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id) };
        if process.is_null() {
            return None;
        }

        let result = (|| {
            let mut exit_code = 0;
            if unsafe { GetExitCodeProcess(process, &mut exit_code) } == 0
                || exit_code != STILL_ACTIVE as u32
            {
                return None;
            }

            let mut creation_time: FILETIME = unsafe { zeroed() };
            let mut exit_time: FILETIME = unsafe { zeroed() };
            let mut kernel_time: FILETIME = unsafe { zeroed() };
            let mut user_time: FILETIME = unsafe { zeroed() };
            if unsafe {
                GetProcessTimes(
                    process,
                    &mut creation_time,
                    &mut exit_time,
                    &mut kernel_time,
                    &mut user_time,
                )
            } == 0
            {
                return None;
            }

            let fingerprint = u64::from(creation_time.dwLowDateTime)
                | (u64::from(creation_time.dwHighDateTime) << 32);
            (fingerprint != 0).then_some(fingerprint)
        })();
        unsafe {
            CloseHandle(process);
        }
        result
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
        if window_thread_process_id(window).map(|(_, process_id)| process_id)
            != Some(search.process_id)
        {
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
        use std::collections::VecDeque;

        struct FakeProbe {
            foreground: VecDeque<Option<TargetIdentity>>,
            windows: VecDeque<Option<TargetIdentity>>,
            property_token: Option<usize>,
            property_matches: bool,
            released_properties: Vec<(usize, TargetIdentity)>,
            activation_succeeds: bool,
            activations: Vec<isize>,
        }

        impl Default for FakeProbe {
            fn default() -> Self {
                Self {
                    foreground: VecDeque::new(),
                    windows: VecDeque::new(),
                    property_token: Some(1),
                    property_matches: true,
                    released_properties: Vec::new(),
                    activation_succeeds: false,
                    activations: Vec::new(),
                }
            }
        }

        impl CapturedTargetProbe for FakeProbe {
            fn foreground_identity(&mut self) -> Option<TargetIdentity> {
                self.foreground.pop_front().flatten()
            }

            fn install_window_property(&mut self, _identity: TargetIdentity) -> Option<usize> {
                self.property_token
            }

            fn window_property_matches(&mut self, token: usize, _identity: TargetIdentity) -> bool {
                self.property_matches && Some(token) == self.property_token
            }

            fn release_window_property(&mut self, token: usize, identity: TargetIdentity) {
                self.released_properties.push((token, identity));
            }

            fn identity_for_window(&mut self, _native_handle: isize) -> Option<TargetIdentity> {
                self.windows.pop_front().flatten()
            }

            fn set_foreground_window(&mut self, native_handle: isize) -> bool {
                self.activations.push(native_handle);
                self.activation_succeeds
            }
        }

        fn identity(
            native_handle: isize,
            thread_id: u32,
            process_id: u32,
            process_creation_time: u64,
        ) -> TargetIdentity {
            TargetIdentity {
                native_handle,
                thread_id,
                process_id,
                process_creation_time,
            }
        }

        fn target(identity: TargetIdentity) -> CapturedTarget {
            CapturedTarget {
                identity,
                window_property_token: 1,
            }
        }

        #[test]
        fn capture_rejects_window_property_installation_failure() {
            let target_identity = identity(44, 7, 9002, 11);
            let mut probe = FakeProbe {
                foreground: [Some(target_identity)].into(),
                property_token: None,
                ..Default::default()
            };

            assert_eq!(capture_external_target_with(&mut probe, 9001), None);
            assert!(probe.released_properties.is_empty());
        }

        #[test]
        fn capture_rejects_property_loss_during_revalidation() {
            let target_identity = identity(44, 7, 9002, 11);
            let mut probe = FakeProbe {
                foreground: [Some(target_identity), Some(target_identity)].into(),
                property_matches: false,
                ..Default::default()
            };

            assert_eq!(capture_external_target_with(&mut probe, 9001), None);
            assert_eq!(probe.released_properties, [(1, target_identity)]);
        }

        #[test]
        fn capture_rejects_scribe_window() {
            let mut probe = FakeProbe {
                foreground: [Some(identity(44, 7, 9001, 11))].into(),
                ..Default::default()
            };

            assert_eq!(capture_external_target_with(&mut probe, 9001), None);
        }

        #[test]
        fn capture_rejects_inaccessible_or_dead_foreground_process() {
            let mut probe = FakeProbe {
                foreground: [None].into(),
                ..Default::default()
            };

            assert_eq!(capture_external_target_with(&mut probe, 9001), None);
        }

        #[test]
        fn capture_rejects_foreground_changes_during_capture() {
            let original_identity = identity(44, 7, 9002, 11);
            let mut probe = FakeProbe {
                foreground: [Some(original_identity), Some(identity(45, 8, 9003, 12))].into(),
                ..Default::default()
            };

            assert_eq!(capture_external_target_with(&mut probe, 9001), None);
            assert_eq!(probe.released_properties, [(1, original_identity)]);
        }

        #[test]
        fn capture_accepts_a_stable_external_window_identity() {
            let target_identity = identity(44, 7, 9002, 11);
            let mut probe = FakeProbe {
                foreground: [Some(target_identity), Some(target_identity)].into(),
                ..Default::default()
            };

            let target = capture_external_target_with(&mut probe, 9001).unwrap();

            assert_eq!(target.identity.native_handle, 44);
            assert_eq!(target.identity.thread_id, 7);
            assert_eq!(target.identity.process_id, 9002);
            assert_eq!(target.identity.process_creation_time, 11);
        }

        #[test]
        fn validation_rejects_inaccessible_or_dead_process() {
            let target = target(identity(44, 7, 9002, 11));
            let mut probe = FakeProbe {
                windows: [None].into(),
                ..Default::default()
            };

            assert!(!captured_target_is_valid_with(&mut probe, 9001, &target));
        }

        #[test]
        fn validation_rejects_scribe_target() {
            let target = target(identity(44, 7, 9001, 11));
            let mut probe = FakeProbe::default();

            assert!(!captured_target_is_valid_with(&mut probe, 9001, &target));
        }

        #[test]
        fn validation_rejects_hwnd_pid_and_fingerprint_reuse() {
            let target = target(identity(44, 7, 9002, 11));
            let mut probe = FakeProbe {
                windows: [Some(identity(44, 7, 9002, 12))].into(),
                ..Default::default()
            };

            assert!(!captured_target_is_valid_with(&mut probe, 9001, &target));
        }

        #[test]
        fn validation_rejects_destroyed_target_even_after_exact_hwnd_reuse() {
            let target_identity = identity(44, 7, 9002, 11);
            let target = target(target_identity);
            let mut probe = FakeProbe {
                windows: [Some(target_identity)].into(),
                property_matches: false,
                ..Default::default()
            };

            assert!(!captured_target_is_valid_with(&mut probe, 9001, &target));
        }

        #[test]
        fn validation_accepts_the_exact_live_target() {
            let target_identity = identity(44, 7, 9002, 11);
            let target = target(target_identity);
            let mut probe = FakeProbe {
                windows: [Some(target_identity)].into(),
                ..Default::default()
            };

            assert!(captured_target_is_valid_with(&mut probe, 9001, &target));
        }

        #[test]
        fn reactivation_rejects_windows_activation_denial() {
            let target_identity = identity(44, 7, 9002, 11);
            let target = target(target_identity);
            let mut probe = FakeProbe {
                windows: [Some(target_identity)].into(),
                activation_succeeds: false,
                ..Default::default()
            };

            assert!(!reactivate_and_verify_captured_target_with(
                &mut probe, 9001, &target
            ));
            assert_eq!(probe.activations, [44]);
        }

        #[test]
        fn reactivation_rejects_focus_changes_after_activation() {
            let target_identity = identity(44, 7, 9002, 11);
            let target = target(target_identity);
            let mut probe = FakeProbe {
                windows: [Some(target_identity), Some(target_identity)].into(),
                foreground: [Some(identity(45, 8, 9003, 12))].into(),
                activation_succeeds: true,
                ..Default::default()
            };

            assert!(!reactivate_and_verify_captured_target_with(
                &mut probe, 9001, &target
            ));
        }

        #[test]
        fn reactivation_succeeds_only_when_the_exact_target_is_foreground() {
            let target_identity = identity(44, 7, 9002, 11);
            let target = target(target_identity);
            let mut probe = FakeProbe {
                windows: [Some(target_identity), Some(target_identity)].into(),
                foreground: [Some(target_identity)].into(),
                activation_succeeds: true,
                ..Default::default()
            };

            assert!(reactivate_and_verify_captured_target_with(
                &mut probe, 9001, &target
            ));
            assert_eq!(probe.activations, [44]);
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod imp {
    use super::{
        CapturedTarget, OverlayHardeningProfile, OverlayPosition, OverlayWindowBounds,
        OverlayWindowSpec,
    };

    pub(super) fn capture_foreground_target() -> Option<CapturedTarget> {
        None
    }

    pub(super) fn captured_target_is_foreground(_target: &CapturedTarget) -> bool {
        false
    }

    pub(super) fn captured_target_application_identity(_target: &CapturedTarget) -> Option<String> {
        None
    }

    pub(super) fn captured_target_is_valid(_target: &CapturedTarget) -> bool {
        false
    }

    pub(super) fn reactivate_and_verify_captured_target(_target: &CapturedTarget) -> bool {
        false
    }

    pub(super) fn release_captured_target(_target: &CapturedTarget) {}

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
        _profile: OverlayHardeningProfile,
    ) -> bool {
        false
    }

    pub(super) fn harden_overlay_window_at(
        _exact_title: &str,
        _bounds: OverlayWindowBounds,
        _visible: bool,
        _profile: OverlayHardeningProfile,
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
    fn control_hardening_clears_existing_transparency() {
        let no_activate = 0b001;
        let tool_window = 0b010;
        let transparent = 0b100;
        assert_eq!(
            hardened_overlay_ex_style(
                transparent,
                no_activate,
                tool_window,
                transparent,
                OverlayHardeningProfile::NonActivatingControl,
            ),
            no_activate | tool_window,
        );
        assert_eq!(
            hardened_overlay_ex_style(
                0,
                no_activate,
                tool_window,
                transparent,
                OverlayHardeningProfile::PassThroughDisplay,
            ),
            no_activate | tool_window | transparent,
        );
    }

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
