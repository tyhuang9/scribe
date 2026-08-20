//! Reviewed sherpa-onnx static archive admission.
//!
//! This module is compiled by the build script and by the explicit integration
//! harness. Keeping it outside `build.rs` makes archive verification runnable
//! without relying on Cargo's non-standard build-script test behavior.

use std::error::Error;
use std::fs;
use std::fs::File;
use std::io;
use std::path::{Component, Path};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use bzip2::read::BzDecoder;
use sha2::{Digest, Sha256};
use tar::Archive;

pub type DynError = Box<dyn Error>;

/// Immutable release metadata reviewed by Scribe. Do not accept an archive
/// merely because its name resembles a sherpa-onnx release asset.
#[derive(Clone, Copy, Debug)]
pub struct ArchiveIntegrity {
    pub name: &'static str,
    pub size: u64,
    pub sha256: &'static str,
}

pub const STATIC_ARCHIVES: &[ArchiveIntegrity] = &[
    ArchiveIntegrity {
        name: "sherpa-onnx-v1.13.5-win-x64-static-MT-Release-lib.tar.bz2",
        size: 120_217_991,
        sha256: "b7080b6f470bac96ef0afe56b25ae9b2f9f0ca82d10dad19bf3a2fc5ffd6cffc",
    },
    ArchiveIntegrity {
        name: "sherpa-onnx-v1.13.5-linux-x64-static-lib.tar.bz2",
        size: 22_394_054,
        sha256: "2ade8b7c62de66b9cf2e32bd7dbe077addaa4b18f422b49dc1bf3a1a0b1f762e",
    },
    ArchiveIntegrity {
        name: "sherpa-onnx-v1.13.5-linux-aarch64-static-lib.tar.bz2",
        size: 20_775_501,
        sha256: "f78af8260892f3060c8c0aba9ae93e4e4c1b16fe509238b88e3688889235e1b2",
    },
    ArchiveIntegrity {
        name: "sherpa-onnx-v1.13.5-osx-x64-static-lib.tar.bz2",
        size: 19_623_101,
        sha256: "689f8167a52dc4dbaf05369705e26c8f203c748a8c342750fdfdcd8ca6bb8699",
    },
    ArchiveIntegrity {
        name: "sherpa-onnx-v1.13.5-osx-arm64-static-lib.tar.bz2",
        size: 19_862_746,
        sha256: "339c8fc19bb4b26e118c80792bbc4546eb263040fac36ef0cc027ec29c756b44",
    },
];

pub fn debug_escape_allowed(
    cargo_profile: &str,
    debug_assertions: bool,
    explicitly_enabled: bool,
) -> bool {
    cargo_profile == "debug" && debug_assertions && explicitly_enabled
}

pub fn static_archive_name(
    version: &str,
    target_os: &str,
    target_arch: &str,
) -> Result<String, DynError> {
    let suffix = match (target_os, target_arch) {
        ("windows", "x86_64") => "win-x64-static-MT-Release-lib.tar.bz2",
        ("linux", "x86_64") => "linux-x64-static-lib.tar.bz2",
        ("linux", "aarch64") => "linux-aarch64-static-lib.tar.bz2",
        ("macos", "x86_64") => "osx-x64-static-lib.tar.bz2",
        ("macos", "aarch64") => "osx-arm64-static-lib.tar.bz2",
        _ => {
            return Err(format!(
                "Unsupported target for reviewed sherpa-onnx static libs: os={target_os}, arch={target_arch}"
            )
            .into());
        }
    };
    Ok(format!("sherpa-onnx-v{version}-{suffix}"))
}

pub fn reviewed_static_archive_integrity(
    archive_name: &str,
) -> Result<&'static ArchiveIntegrity, DynError> {
    STATIC_ARCHIVES
        .iter()
        .find(|candidate| candidate.name == archive_name)
        .ok_or_else(|| {
            format!("No reviewed SHA-256 metadata for sherpa-onnx archive {archive_name}").into()
        })
}

pub fn verify_archive(path: &Path, integrity: &ArchiveIntegrity) -> Result<(), DynError> {
    let metadata = fs::metadata(path)?;
    if metadata.len() != integrity.size {
        return Err(format!(
            "sherpa-onnx archive size mismatch for {}: expected {}, got {}",
            path.display(),
            integrity.size,
            metadata.len()
        )
        .into());
    }

    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = io::Read::read(&mut file, &mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let actual = format!("{:x}", hasher.finalize());
    if actual != integrity.sha256 {
        return Err(format!(
            "sherpa-onnx archive SHA-256 mismatch for {}: expected {}, got {}",
            path.display(),
            integrity.sha256,
            actual
        )
        .into());
    }
    Ok(())
}

pub fn unpack_archive_safely(
    archive_path: &Path,
    destination: &Path,
    expected_root: &str,
) -> Result<(), DynError> {
    let tar_file = File::open(archive_path)?;
    let decoder = BzDecoder::new(tar_file);
    let mut archive = Archive::new(decoder);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let entry_path = entry.path()?.into_owned();
        let mut components = entry_path.components();
        let Some(first) = components.next() else {
            return Err("sherpa-onnx archive contains an empty path".into());
        };
        if first.as_os_str() != expected_root {
            return Err(format!(
                "sherpa-onnx archive entry is outside expected root {expected_root}: {}",
                entry_path.display()
            )
            .into());
        }
        if components.clone().next().is_none() && !entry.header().entry_type().is_dir() {
            return Err(format!(
                "invalid sherpa-onnx archive root entry: {}",
                entry_path.display()
            )
            .into());
        }
        if entry_path.is_absolute()
            || entry_path
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
            || entry.header().entry_type().is_symlink()
            || entry.header().entry_type().is_hard_link()
        {
            return Err(
                format!("unsafe sherpa-onnx archive entry: {}", entry_path.display()).into(),
            );
        }
        if !entry.unpack_in(destination)? {
            return Err(format!(
                "sherpa-onnx archive entry escaped destination: {}",
                entry_path.display()
            )
            .into());
        }
    }
    Ok(())
}

static STAGING_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub fn validate_static_library_layout(
    lib_dir: &Path,
    target_os: &str,
    required_libraries: &[&str],
) -> Result<(), DynError> {
    if !lib_dir.is_dir() {
        return Err(format!(
            "missing sherpa-onnx static library directory: {}",
            lib_dir.display()
        )
        .into());
    }
    let extension = if target_os == "windows" { "lib" } else { "a" };
    for library in required_libraries {
        let filename = if target_os == "windows" {
            format!("{library}.{extension}")
        } else {
            format!("lib{library}.{extension}")
        };
        let path = lib_dir.join(filename);
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            format!(
                "missing required sherpa-onnx static library {}: {error}",
                path.display()
            )
        })?;
        if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
            return Err(format!("invalid sherpa-onnx static library: {}", path.display()).into());
        }
    }
    Ok(())
}

pub fn activate_verified_archive(
    archive_path: &Path,
    integrity: &ArchiveIntegrity,
    cache_root: &Path,
    archive_stem: &str,
    target_os: &str,
    required_libraries: &[&str],
) -> Result<std::path::PathBuf, DynError> {
    // The extracted tree is never an authority. Every reuse first proves the
    // immutable archive bytes that produced it.
    verify_archive(archive_path, integrity)?;
    fs::create_dir_all(cache_root)?;
    let activated = cache_root.join(archive_stem);
    let activated_lib = activated.join("lib");

    let staging_parent = unique_sibling(cache_root, archive_stem, "staging");
    fs::create_dir(&staging_parent)?;
    let staged_tree = staging_parent.join(archive_stem);
    let staged_lib = staged_tree.join("lib");
    let result = (|| {
        unpack_archive_safely(archive_path, &staging_parent, archive_stem)?;
        validate_static_library_layout(&staged_lib, target_os, required_libraries)?;

        let quarantine = unique_sibling(cache_root, archive_stem, "replaced");
        let had_previous = activated.exists();
        if had_previous {
            match fs::rename(&activated, &quarantine) {
                Ok(()) => {}
                Err(error) => {
                    return Err(format!(
                        "could not isolate partial sherpa-onnx cache {}: {error}",
                        activated.display()
                    )
                    .into());
                }
            }
        }

        if let Err(error) = fs::rename(&staged_tree, &activated) {
            if had_previous && !activated.exists() {
                let _ = fs::rename(&quarantine, &activated);
            }
            return Err(format!("could not activate verified sherpa-onnx cache: {error}").into());
        }
        let _ = fs::remove_dir_all(&quarantine);
        Ok(activated_lib.clone())
    })();
    let _ = fs::remove_dir_all(&staging_parent);
    result
}

fn unique_sibling(parent: &Path, stem: &str, label: &str) -> std::path::PathBuf {
    let sequence = STAGING_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    parent.join(format!(
        ".{stem}.{label}-{}-{nanos}-{sequence}",
        std::process::id()
    ))
}
