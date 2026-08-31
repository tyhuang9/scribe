//! macOS power and OS-build witnesses with no Metal runtime dependency.

use crate::backend_policy::PowerSource;
use anyhow::{Result, bail};

pub(crate) fn power_source() -> PowerSource {
    platform::power_source()
}

/// Darwin's immutable OS build number is the Metal driver/runtime witness.
/// This parent-safe query does not enumerate or load `MTLDevice`.
pub(crate) fn os_build_witness() -> Result<String> {
    platform::os_build_witness()
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::{CString, c_char, c_int, c_void};

    use anyhow::{Context, anyhow};

    use super::*;

    unsafe extern "C" {
        fn scribe_macos_power_source() -> i32;
        fn sysctlbyname(
            name: *const c_char,
            oldp: *mut c_void,
            oldlenp: *mut usize,
            newp: *mut c_void,
            newlen: usize,
        ) -> c_int;
    }

    pub(super) fn power_source() -> PowerSource {
        // SAFETY: the IOKit-only shim has no pointer arguments or mutable globals.
        match unsafe { scribe_macos_power_source() } {
            1 => PowerSource::Ac,
            2 => PowerSource::Battery,
            _ => PowerSource::Unknown,
        }
    }

    pub(super) fn os_build_witness() -> Result<String> {
        let name = CString::new("kern.osversion").expect("static sysctl name has no NUL");
        let mut length = 0_usize;
        // SAFETY: this is the documented size query for sysctlbyname.
        if unsafe {
            sysctlbyname(
                name.as_ptr(),
                std::ptr::null_mut(),
                &mut length,
                std::ptr::null_mut(),
                0,
            )
        } != 0
            || !(2..=128).contains(&length)
        {
            return Err(std::io::Error::last_os_error())
                .map_err(anyhow::Error::from)
                .context("could not size the macOS build witness");
        }
        let mut bytes = vec![0_u8; length];
        // SAFETY: bytes owns the writable buffer and length names its capacity.
        if unsafe {
            sysctlbyname(
                name.as_ptr(),
                bytes.as_mut_ptr().cast(),
                &mut length,
                std::ptr::null_mut(),
                0,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error())
                .map_err(anyhow::Error::from)
                .context("could not read the macOS build witness");
        }
        bytes.truncate(length);
        if bytes.last() == Some(&0) {
            bytes.pop();
        }
        let build = std::str::from_utf8(&bytes)
            .map_err(|_| anyhow!("macOS build witness is not UTF-8"))?
            .trim();
        if build.is_empty()
            || build.len() > 96
            || !build
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        {
            bail!("macOS build witness is not canonical");
        }
        Ok(format!("macos-build:{}", build.to_ascii_lowercase()))
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use super::*;

    pub(super) fn power_source() -> PowerSource {
        PowerSource::Unknown
    }

    pub(super) fn os_build_witness() -> Result<String> {
        bail!("macOS build witness requires macOS")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_macos_power_probe_fails_safe() {
        #[cfg(not(target_os = "macos"))]
        {
            assert_eq!(power_source(), PowerSource::Unknown);
            assert!(os_build_witness().is_err());
        }
    }
}
