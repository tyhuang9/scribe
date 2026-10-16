//! Pure target selection and byte-policy helpers for the embedded GPU release authority.

pub(crate) const LEGACY_DEFAULT_AUTHORITY_PATH: &str =
    "runtime-manifests/gpu-pack-release-authority-macos-empty.json";
pub(crate) const LINUX_DEFAULT_AUTHORITY_PATH: &str =
    "runtime-manifests/gpu-pack-release-authority-linux-empty.json";
pub(crate) const MAX_AUTHORITY_BYTES: usize = 512 * 1024;

/// Source-reviewed Linux production default after the optional transport LF is removed.
///
/// This literal is deliberately independent of the default source file so changing that file
/// cannot silently authorize a non-empty production catalog.
pub(crate) const REVIEWED_LINUX_EMPTY_AUTHORITY: &[u8] = br#"{"schema_version":1,"target":"x86_64-unknown-linux-gnu","catalog_sha256":"c3f19154f1b2265dac92206eae3a35c130a078be46705e1be6032bc442c3b9dc","release_security_epoch":0,"entries":[]}"#;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ReleaseAuthoritySelection {
    pub(crate) default_path: &'static str,
    pub(crate) requires_reviewed_linux_empty: bool,
    pub(crate) linux_gpu_eligible: bool,
}

pub(crate) fn select_release_authority(
    target_os: &str,
    target_arch: &str,
    target_env: &str,
) -> ReleaseAuthoritySelection {
    if target_os == "linux" {
        return ReleaseAuthoritySelection {
            default_path: LINUX_DEFAULT_AUTHORITY_PATH,
            requires_reviewed_linux_empty: true,
            linux_gpu_eligible: target_arch == "x86_64" && target_env == "gnu",
        };
    }

    ReleaseAuthoritySelection {
        default_path: LEGACY_DEFAULT_AUTHORITY_PATH,
        requires_reviewed_linux_empty: false,
        linux_gpu_eligible: false,
    }
}

pub(crate) fn normalize_authority_transport(bytes: &[u8]) -> Result<Vec<u8>, &'static str> {
    if bytes.is_empty() || bytes.len() > MAX_AUTHORITY_BYTES {
        return Err("GPU pack release authority is empty or exceeds its build-time bound");
    }

    let mut normalized = bytes.to_vec();
    if normalized.last() == Some(&b'\n') {
        normalized.pop();
    }
    if normalized.is_empty()
        || normalized.last() == Some(&b'\r')
        || std::str::from_utf8(&normalized).is_err()
    {
        return Err(
            "GPU pack release authority must be canonical UTF-8 JSON with at most one trailing LF",
        );
    }
    Ok(normalized)
}

pub(crate) fn validate_selected_authority(
    selection: ReleaseAuthoritySelection,
    normalized: &[u8],
) -> Result<(), &'static str> {
    if selection.requires_reviewed_linux_empty && normalized != REVIEWED_LINUX_EMPTY_AUTHORITY {
        return Err(
            "Linux production builds require the source-reviewed empty GPU release authority",
        );
    }
    Ok(())
}

/// Runs the same normalization, target policy, and publication sequence as `build.rs`.
/// The publisher is not invoked unless every applicable validation has succeeded.
pub(crate) fn emit_selected_authority(
    selection: ReleaseAuthoritySelection,
    source_bytes: &[u8],
    publish: impl FnOnce(&[u8]),
) -> Result<(), &'static str> {
    let normalized = normalize_authority_transport(source_bytes)?;
    validate_selected_authority(selection, &normalized)?;
    publish(&normalized);
    Ok(())
}
