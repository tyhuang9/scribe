//! Conservative local free-space preflight for managed artifact downloads.
//!
//! The result is advisory with respect to concurrent writers: callers must
//! still handle an `ENOSPC` error during the write. It is deliberately
//! fail-closed before starting a managed download so Scribe can explain a
//! known shortage without disturbing an installed artifact or its runtime.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

/// Space retained after all newly required artifact bytes have been reserved.
pub(crate) const SAFETY_HEADROOM_BYTES: u64 = 1024 * 1024 * 1024;

#[cfg(all(windows, test))]
const WINDOWS_DRIVE_FIXED: u32 = 3;
#[cfg(windows)]
const WINDOWS_DRIVE_REMOTE: u32 = 4;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DiskSpacePreflight {
    pub(crate) volume: String,
    pub(crate) available_bytes: u64,
    pub(crate) additional_bytes: u64,
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
    reservation_identity: PhysicalVolumeIdentity,
}

/// Stable reservation identity for the physical filesystem backing a target.
/// This is deliberately separate from the user-facing mount/volume label,
/// which can have multiple aliases for the same capacity pool.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct PhysicalVolumeIdentity(String);

impl PhysicalVolumeIdentity {
    pub(crate) fn key_material(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

#[cfg(windows)]
type CanonicalTargetIdentityValue = Vec<u16>;

#[cfg(not(windows))]
type CanonicalTargetIdentityValue = PathBuf;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct CanonicalTargetIdentity(CanonicalTargetIdentityValue);

impl std::fmt::Display for CanonicalTargetIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        #[cfg(windows)]
        return formatter.write_str(&String::from_utf16_lossy(&self.0));

        #[cfg(not(windows))]
        self.0.display().fmt(formatter)
    }
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

/// Checks whether the volume containing `destination` can accommodate the
/// additional artifact bytes and a fixed safety reserve. The destination need
/// not exist; no directories or files are created by this operation.
pub(crate) fn preflight_download_destination(
    destination: &Path,
    additional_bytes: u64,
) -> Result<DiskSpacePreflight, String> {
    preflight_with(&SystemSpaceProbe, destination, additional_bytes)
}

pub(crate) fn physical_volume_identity(
    destination: &Path,
) -> Result<PhysicalVolumeIdentity, String> {
    Ok(availability_with(&SystemSpaceProbe, destination)?.reservation_identity)
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
        additional_bytes,
        required_bytes,
    })
}

/// Resolves aliases through the nearest existing ancestor without creating
/// the destination. This lets the install coordinator reject two logical jobs
/// that would mutate the same path through symlink or junction aliases.
pub(crate) fn canonical_target_identity(
    destination: &Path,
) -> Result<CanonicalTargetIdentity, String> {
    let mut candidate = destination.to_path_buf();
    let mut suffix = Vec::<OsString>::new();
    loop {
        match fs::symlink_metadata(&candidate) {
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let name = candidate.file_name().ok_or_else(|| {
                    format!(
                        "could not resolve a canonical target identity for {}",
                        destination.display()
                    )
                })?;
                suffix.push(name.to_os_string());
                if !candidate.pop() {
                    return Err(format!(
                        "could not find an existing ancestor for {}",
                        destination.display()
                    ));
                }
            }
            Err(error) => {
                return Err(format!(
                    "could not inspect target ancestor {}: {error}",
                    candidate.display()
                ));
            }
        }
    }
    let mut canonical = fs::canonicalize(&candidate).map_err(|error| {
        format!(
            "could not canonicalize target ancestor {}: {error}",
            candidate.display()
        )
    })?;
    for component in suffix.into_iter().rev() {
        canonical.push(component);
    }
    #[cfg(windows)]
    {
        Ok(CanonicalTargetIdentity(normalize_windows_identity(
            &canonical,
        )))
    }
    #[cfg(not(windows))]
    {
        Ok(CanonicalTargetIdentity(canonical))
    }
}

#[cfg(windows)]
fn normalize_windows_identity(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;

    path.as_os_str()
        .encode_wide()
        .map(|unit| {
            if unit == u16::from(b'\\') {
                u16::from(b'/')
            } else if (u16::from(b'A')..=u16::from(b'Z')).contains(&unit) {
                unit + u16::from(b'a' - b'A')
            } else {
                unit
            }
        })
        .collect()
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
    use windows_sys::Win32::Storage::FileSystem::{
        GetDiskFreeSpaceExW, GetDriveTypeW, GetVolumeNameForVolumeMountPointW, GetVolumePathNameW,
    };

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
    let drive_type = unsafe { GetDriveTypeW(volume_path.as_ptr()) };
    let mut volume_name = vec![0_u16; 261];
    let named = unsafe {
        GetVolumeNameForVolumeMountPointW(
            volume_path.as_ptr(),
            volume_name.as_mut_ptr(),
            volume_name.len() as u32,
        )
    };
    let reservation_identity = if named != 0 {
        let name_nul = volume_name
            .iter()
            .position(|character| *character == 0)
            .unwrap_or(volume_name.len());
        let name = String::from_utf16(&volume_name[..name_nul])
            .map_err(|_| "Windows volume GUID was not valid UTF-16".to_owned())?;
        windows_physical_volume_identity(drive_type, Some(&name))?
    } else {
        windows_physical_volume_identity(drive_type, None).map_err(|message| {
            format!(
                "{message} for {}: {}",
                existing_directory.display(),
                std::io::Error::last_os_error()
            )
        })?
    };
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
        reservation_identity,
    })
}

#[cfg(windows)]
fn windows_physical_volume_identity(
    drive_type: u32,
    volume_guid: Option<&str>,
) -> Result<PhysicalVolumeIdentity, String> {
    if let Some(volume_guid) = volume_guid.filter(|value| !value.is_empty()) {
        return Ok(PhysicalVolumeIdentity(format!(
            "windows-volume-guid:{}",
            volume_guid.to_ascii_lowercase()
        )));
    }
    if drive_type == WINDOWS_DRIVE_REMOTE {
        // Some network redirectors expose no volume GUID. All such paths use
        // one user-scoped bucket: this can over-coordinate unrelated shares,
        // but never under-reserves a share reachable through multiple aliases.
        return Ok(PhysicalVolumeIdentity("windows-network-global".to_owned()));
    }
    Err("could not obtain a stable GUID for local Windows volume".to_owned())
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
        reservation_identity: PhysicalVolumeIdentity(format!("unix-device:{}", metadata.st_dev)),
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
                reservation_identity: PhysicalVolumeIdentity("test-device".to_owned()),
            }),
        };
        let preflight = preflight_with(&probe, &existing_destination(), 100).unwrap();

        assert_eq!(preflight.volume, "test-volume");
        assert_eq!(preflight.additional_bytes, 100);
        assert_eq!(preflight.required_bytes, SAFETY_HEADROOM_BYTES + 100);
        assert!(preflight.has_sufficient_space());
    }

    #[test]
    fn preflight_reports_an_insufficient_volume_without_rounding_down() {
        let probe = FakeProbe {
            result: Ok(DiskSpaceAvailability {
                volume: "test-volume".to_owned(),
                available_bytes: SAFETY_HEADROOM_BYTES + 99,
                reservation_identity: PhysicalVolumeIdentity("test-device".to_owned()),
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

    #[test]
    fn canonical_target_identity_collapses_existing_directory_aliases() {
        let root = std::env::temp_dir().join(format!(
            "scribe-canonical-target-test-{}",
            std::process::id()
        ));
        let real = root.join("real");
        let alias = root.join("alias");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&real).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        #[cfg(target_os = "windows")]
        if std::os::windows::fs::symlink_dir(&real, &alias).is_err() {
            let _ = fs::remove_dir_all(&root);
            return;
        }

        assert_eq!(
            canonical_target_identity(&real.join("model.gguf")).unwrap(),
            canonical_target_identity(&alias.join("model.gguf")).unwrap()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn physical_volume_identity_is_shared_by_distinct_targets_and_path_aliases() {
        let root = std::env::temp_dir().join(format!(
            "scribe-physical-volume-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let canonical_root = fs::canonicalize(&root).unwrap();

        let first = physical_volume_identity(&root.join("first").join("model.bin")).unwrap();
        let second = physical_volume_identity(
            &canonical_root
                .join("alias")
                .join("..")
                .join("second")
                .join("model.bin"),
        )
        .unwrap();

        assert_eq!(first, second);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_target_identity_normalization_is_lossless_and_alias_aware() {
        use std::os::windows::ffi::OsStringExt;

        assert_eq!(
            normalize_windows_identity(Path::new(r"C:\Models\MODEL.GGUF")),
            normalize_windows_identity(Path::new("c:/models/model.gguf"))
        );

        let first = PathBuf::from(OsString::from_wide(&[b'C' as u16, b':' as u16, 0xd800]));
        let second = PathBuf::from(OsString::from_wide(&[b'C' as u16, b':' as u16, 0xd801]));
        assert_ne!(
            normalize_windows_identity(&first),
            normalize_windows_identity(&second)
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_physical_identity_prefers_guid_and_fails_safe_for_networks() {
        assert_eq!(
            windows_physical_volume_identity(
                WINDOWS_DRIVE_FIXED,
                Some(r"\\?\Volume{AABBCCDD-0000-1111-2222-333344445555}\")
            )
            .unwrap(),
            windows_physical_volume_identity(
                WINDOWS_DRIVE_REMOTE,
                Some(r"\\?\volume{aabbccdd-0000-1111-2222-333344445555}\")
            )
            .unwrap()
        );
        assert_eq!(
            windows_physical_volume_identity(WINDOWS_DRIVE_REMOTE, None).unwrap(),
            PhysicalVolumeIdentity("windows-network-global".to_owned())
        );
        assert!(windows_physical_volume_identity(WINDOWS_DRIVE_FIXED, None).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn canonical_target_identity_preserves_backslashes_in_unix_names() {
        let root = std::env::temp_dir().join(format!(
            "scribe-canonical-backslash-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        let separator_path = root.join("models").join("model.gguf");
        let backslash_path = root.join("models\\model.gguf");
        assert_ne!(
            canonical_target_identity(&separator_path).unwrap(),
            canonical_target_identity(&backslash_path).unwrap()
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn canonical_target_identity_preserves_distinct_non_utf8_names() {
        use std::os::unix::ffi::OsStringExt;

        let root = std::env::temp_dir().join(format!(
            "scribe-canonical-non-utf8-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        let first = root.join(OsString::from_vec(vec![b'm', 0x80]));
        let second = root.join(OsString::from_vec(vec![b'm', 0x81]));
        assert_ne!(
            canonical_target_identity(&first).unwrap(),
            canonical_target_identity(&second).unwrap()
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn canonical_target_identity_rejects_a_dangling_symlink_ancestor() {
        let root = std::env::temp_dir().join(format!(
            "scribe-canonical-dangling-link-test-{}",
            std::process::id()
        ));
        let dangling = root.join("dangling");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        std::os::unix::fs::symlink(root.join("missing"), &dangling).unwrap();

        let error = canonical_target_identity(&dangling.join("model.gguf")).unwrap_err();
        assert!(error.contains("could not canonicalize target ancestor"));

        fs::remove_dir_all(root).unwrap();
    }
}
