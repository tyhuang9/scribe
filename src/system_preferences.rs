//! Small, platform-backed UI preference queries shared by native and egui views.

/// Whether the operating system permits client-area animations.
#[cfg(target_os = "windows")]
pub(crate) fn client_area_animations_enabled() -> bool {
    use std::ffi::c_void;

    use windows_sys::Win32::UI::WindowsAndMessaging::SystemParametersInfoW;

    const SPI_GETCLIENTAREAANIMATION: u32 = 0x1042;
    let mut enabled = 0i32;
    unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            &mut enabled as *mut i32 as *mut c_void,
            0,
        ) != 0
            && enabled != 0
    }
}

/// Non-Windows targets have no equivalent setting in this application yet.
#[cfg(not(target_os = "windows"))]
pub(crate) fn client_area_animations_enabled() -> bool {
    true
}
