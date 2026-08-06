//! Conservative local free-space preflight for managed artifact downloads.
//!
//! The result is advisory with respect to concurrent writers: callers must
//! still handle an `ENOSPC` error during the write. It is deliberately
//! fail-closed before starting a managed download so Scribe can explain a
//! known shortage without disturbing an installed artifact or its runtime.

use std::fs;
use std::path::{Path, PathBuf};

/// Space retained after all newly required artifact bytes have been reserved.
pub(crate) const SAFETY_HEADROOM_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DiskSpacePreflight {
    pub(crate) volume: String,
    pub(crate) available_bytes: u64,
    pub(crate) required_bytes: u64,
}

impl DiskSpacePreflight {
    pub(crate) fn has_sufficient_space(&self) -> bool {
        self.available_bytes >= self.required_bytes
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DiskSpaceAvailability {
    pub(crate) volume: String,
    pub(crate) available_bytes: u64,
}

trait SpaceProbe {
    fn probe(&self, existing_directory: &Path) -> Result<DiskSpaceAvailability, String>;
}

struct SystemSpaceProbe;

impl SpaceProbe for SystemSpaceProbe {
    fn probe(&self, existing_directory: &Path) -> Result<DiskSpaceAvailability, String> {
        probe_system_volume(existing_directory)
    }
}

/// Captures one free-space observation for the volume containing
/// `destination`. Catalog callers may reuse this advisory snapshot across a
/// bounded projection; the install backend must still preflight the exact
/// target and remaining bytes immediately before writing.
pub(crate) fn available_space_for_destination(
    destination: &Path,
) -> Result<DiskSpaceAvailability, String> {
    availability_with(&SystemSpaceProbe, destination)
}

/// Checks whether the volume containing `destination` can accommodate the
/// additional artifact bytes and a fixed safety reserve. The destination need
/// not exist; no directories or files are created by this operation.
pub(crate) fn preflight_download_destination(
    destination: &Path,
    additional_bytes: u64,
) -> Result<DiskSpacePreflight, String> {
    preflight_with(&SystemSpaceProbe, destination, additional_bytes)
}

fn preflight_with(
    probe: &dyn SpaceProbe,
    destination: &Path,
    additional_bytes: u64,
) -> Result<DiskSpacePreflight, String> {
    let required_bytes = additional_bytes
        .checked_add(SAFETY_HEADROOM_BYTES)
        .ok_or_else(|| "download-space requirement overflowed".to_owned())?;
    let volume = availability_with(probe, destination)?;
    Ok(DiskSpacePreflight {
        volume: volume.volume,
        available_bytes: volume.available_bytes,
        required_bytes,
    })
}

fn availability_with(
    probe: &dyn SpaceProbe,
    destination: &Path,
) -> Result<DiskSpaceAvailability, String> {
    let existing_directory = nearest_existing_directory(destination)?;
    probe.probe(&existing_directory)
}

fn nearest_existing_directory(destination: &Path) -> Result<PathBuf, String> {
    let mut candidate = destination
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", destination.display()))?
        .to_path_buf();
    loop {
        match fs::metadata(&candidate) {
            Ok(metadata) if metadata.is_dir() => {
                return fs::canonicalize(&candidate).map_err(|error| {
                    format!(
                        "could not canonicalize existing storage directory {}: {error}",
                        candidate.display()
                    )
                });
            }
            Ok(_) => {
                return Err(format!(
                    "storage ancestor {} is not a directory",
                    candidate.display()
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "could not inspect storage ancestor {}: {error}",
                    candidate.display()
                ));
            }
        }
        if !candidate.pop() {
            return Err(format!(
                "could not find an existing storage ancestor for {}",
                destination.display()
            ));
        }
    }
}

#[cfg(target_os = "windows")]
fn probe_system_volume(existing_directory: &Path) -> Result<DiskSpaceAvailability, String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{GetDiskFreeSpaceExW, GetVolumePathNameW};

    let directory = existing_directory
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let mut volume_path = vec![0_u16; 261];
    let resolved = unsafe {
        GetVolumePathNameW(
            directory.as_ptr(),
            volume_path.as_mut_ptr(),
            volume_path.len() as u32,
        )
    };
    if resolved == 0 {
        return Err(format!(
            "could not determine the Windows volume for {}",
            existing_directory.display()
        ));
    }
    let nul = volume_path
        .iter()
        .position(|character| *character == 0)
        .unwrap_or(volume_path.len());
    let volume = String::from_utf16(&volume_path[..nul])
        .map_err(|_| "Windows volume path was not valid UTF-16".to_owned())?;
    let mut available_bytes = 0_u64;
    let queried = unsafe {
        GetDiskFreeSpaceExW(
            volume_path.as_ptr(),
            &mut available_bytes,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if queried == 0 {
        return Err(format!(
            "could not read available space on Windows volume {volume}"
        ));
    }
    Ok(DiskSpaceAvailability {
        volume,
        available_bytes,
    })
}

#[cfg(unix)]
fn probe_system_volume(existing_directory: &Path) -> Result<DiskSpaceAvailability, String> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let path = CString::new(existing_directory.as_os_str().as_bytes())
        .map_err(|_| "storage path contains an interior NUL byte".to_owned())?;
    let mut metadata = std::mem::MaybeUninit::<libc::stat>::zeroed();
    if unsafe { libc::stat(path.as_ptr(), metadata.as_mut_ptr()) } != 0 {
        return Err(format!(
            "could not identify the filesystem for {}",
            existing_directory.display()
        ));
    }
    let metadata = unsafe { metadata.assume_init() };
    let mut filesystem = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
    if unsafe { libc::statvfs(path.as_ptr(), filesystem.as_mut_ptr()) } != 0 {
        return Err(format!(
            "could not read available space for {}",
            existing_directory.display()
        ));
    }
    let filesystem = unsafe { filesystem.assume_init() };
    let available_bytes = u64::try_from(filesystem.f_bavail)
        .ok()
        .zip(u64::try_from(filesystem.f_frsize).ok())
        .and_then(|(blocks, block_size)| blocks.checked_mul(block_size))
        .ok_or_else(|| "available Unix filesystem space overflowed".to_owned())?;
    Ok(DiskSpaceAvailability {
        volume: format!("device:{}", metadata.st_dev),
        available_bytes,
    })
}

#[cfg(not(any(target_os = "windows", unix)))]
fn probe_system_volume(_existing_directory: &Path) -> Result<DiskSpaceAvailability, String> {
    Err("free-space preflight is unavailable on this operating system".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeProbe {
        result: Result<DiskSpaceAvailability, String>,
    }

    impl SpaceProbe for FakeProbe {
        fn probe(&self, _existing_directory: &Path) -> Result<DiskSpaceAvailability, String> {
            self.result.clone()
        }
    }

    fn existing_destination() -> PathBuf {
        std::env::temp_dir()
            .join("scribe-disk-space-test")
            .join("model.gguf")
    }

    #[test]
    fn preflight_reserves_artifact_bytes_and_safety_headroom() {
        let probe = FakeProbe {
            result: Ok(DiskSpaceAvailability {
                volume: "test-volume".to_owned(),
                available_bytes: SAFETY_HEADROOM_BYTES + 100,
            }),
        };
        let preflight = preflight_with(&probe, &existing_destination(), 100).unwrap();

        assert_eq!(preflight.volume, "test-volume");
        assert_eq!(preflight.required_bytes, SAFETY_HEADROOM_BYTES + 100);
        assert!(preflight.has_sufficient_space());
    }

    #[test]
    fn preflight_reports_an_insufficient_volume_without_rounding_down() {
        let probe = FakeProbe {
            result: Ok(DiskSpaceAvailability {
                volume: "test-volume".to_owned(),
                available_bytes: SAFETY_HEADROOM_BYTES + 99,
            }),
        };
        let preflight = preflight_with(&probe, &existing_destination(), 100).unwrap();

        assert!(!preflight.has_sufficient_space());
    }

    #[test]
    fn preflight_rejects_requirement_overflow_and_probe_failures() {
        let probe = FakeProbe {
            result: Err("probe failed".to_owned()),
        };

        assert!(preflight_with(&probe, &existing_destination(), u64::MAX).is_err());
        assert!(
            preflight_with(&probe, &existing_destination(), 1)
                .unwrap_err()
                .contains("probe failed")
        );
    }
}
