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
