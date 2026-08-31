//! Signed-release evidence gate for automatic GPU selection.
//!
//! This module deliberately does not benchmark or probe hardware.  Release
//! engineering records immutable qualification evidence in the embedded
//! manifest, while the client only compares that evidence binding with the
//! verified pack and the worker's fresh capability facts.  An absent, invalid,
//! or non-matching entry is a denial; explicit GPU selection is intentionally
//! outside this policy.

use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::backend_policy::{
    BackendKind, BackendPackIdentity, BackendTarget, DeviceClass, GpuVendor, ProviderIdentity,
};

pub(crate) const AUTO_QUALIFICATION_POLICY_VERSION: u16 = 3;
const QUALIFICATION_SCHEMA_VERSION: u16 = 2;
const WINDOWS_X64_OS: &str = "windows";
const WINDOWS_X64_ARCH: &str = "x86_64";
const LINUX_X64_OS: &str = "linux";
const LINUX_X64_ARCH: &str = "x86_64";
const EMBEDDED_WINDOWS_X64_MANIFEST: &str =
    include_str!("../runtime-manifests/gpu-auto-qualification-windows-x64.json");
const EMBEDDED_LINUX_X64_MANIFEST: &str =
    include_str!("../runtime-manifests/gpu-auto-qualification-linux-x86_64.json");
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const EMBEDDED_CURRENT_MANIFEST: &str =
    include_str!("../runtime-manifests/gpu-auto-qualification-macos-aarch64.json");
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
const EMBEDDED_CURRENT_MANIFEST: &str =
    include_str!("../runtime-manifests/gpu-auto-qualification-macos-x86_64.json");
#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
const EMBEDDED_CURRENT_MANIFEST: &str = EMBEDDED_WINDOWS_X64_MANIFEST;
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const EMBEDDED_CURRENT_MANIFEST: &str = EMBEDDED_LINUX_X64_MANIFEST;

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub(crate) enum AutoQualificationError {
    #[error("GPU Auto qualification manifest is not canonical")]
    NonCanonical,
    #[error("GPU Auto qualification manifest has an unsupported schema")]
    UnsupportedSchema,
    #[error("GPU Auto qualification manifest must use default_deny mode")]
    UnsafeMode,
    #[error("GPU Auto qualification manifest targets an unsupported platform")]
    UnsupportedPlatform,
    #[error("GPU Auto qualification manifest contains an invalid entry: {0}")]
    InvalidEntry(&'static str),
    #[error("GPU Auto qualification manifest entries are not strictly canonical and sorted")]
    NonCanonicalEntries,
    #[error("GPU Auto qualification manifest could not be parsed: {0}")]
    Parse(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum QualificationDenial {
    ManifestNotForCurrentPlatform,
    NoMatchingPackEvidence,
    NoMatchingTargetEvidence,
    MissingTargetPackIdentity,
    MissingDriverIdentity,
    InvalidAvailableMemory,
    InsufficientMemory,
    InsufficientAvailableMemory,
}

impl QualificationDenial {
    pub(crate) fn diagnostic(self) -> &'static str {
        match self {
            Self::ManifestNotForCurrentPlatform => {
                "Auto GPU qualification is unavailable on this platform"
            }
            Self::NoMatchingPackEvidence => {
                "verified GPU pack has no matching Auto qualification evidence"
            }
            Self::NoMatchingTargetEvidence => {
                "GPU device facts do not match Auto qualification evidence"
            }
            Self::MissingTargetPackIdentity => {
                "GPU worker did not provide a complete verified pack identity"
            }
            Self::MissingDriverIdentity => {
                "GPU worker did not provide a driver identity required by Auto qualification"
            }
            Self::InvalidAvailableMemory => {
                "GPU worker reported available memory greater than total memory"
            }
            Self::InsufficientMemory => "GPU total memory is below the qualified Auto minimum",
            Self::InsufficientAvailableMemory => {
                "GPU available memory is unknown or below the qualified Auto minimum"
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum QualificationDecision {
    Approved { evidence_id: String },
    Denied(QualificationDenial),
}

impl QualificationDecision {
    pub(crate) fn is_approved(&self) -> bool {
        matches!(self, Self::Approved { .. })
    }

    pub(crate) fn diagnostic(&self) -> String {
        match self {
            Self::Approved { evidence_id } => {
                format!("Auto GPU qualification matched release evidence {evidence_id}")
            }
            Self::Denied(reason) => reason.diagnostic().to_owned(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct QualificationDocument {
    schema_version: u16,
    mode: QualificationMode,
    target_os: String,
    target_arch: String,
    entries: Vec<QualificationEntry>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum QualificationMode {
    DefaultDeny,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct QualificationEntry {
    pack: PackBinding,
    model_digest: String,
    backend: BackendKind,
    provider_id: String,
    vendor: GpuVendor,
    device_class: DeviceClass,
    minimum_total_memory_bytes: u64,
    minimum_available_memory_bytes: u64,
    driver: DriverConstraint,
    evidence: QualificationEvidence,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackBinding {
    pack_id: String,
    pack_version: String,
    pack_digest: String,
    security_epoch: u64,
    runtime_abi: u16,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum DriverConstraint {
    /// Stage 5 intentionally supports only an exact bounded driver identity.
    /// Ranges and prefixes would need a separately reviewed normalization
    /// contract before they could safely widen an Auto-qualified lane.
    Exact { value: String },
}

impl DriverConstraint {
    fn matches(&self, observed: &str) -> bool {
        match self {
            Self::Exact { value } => observed == value,
        }
    }

    fn value(&self) -> &str {
        match self {
            Self::Exact { value } => value,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct QualificationEvidence {
    id: String,
    cold_runs: u16,
    warm_runs: u16,
    gpu_p95_ms: u64,
    cpu_p95_ms: u64,
    correctness_verified: bool,
    reliability_verified: bool,
    cold_evidence_sha256: String,
    warm_evidence_sha256: String,
    transcript_parity_evidence_sha256: String,
}

/// A validated, immutable document.  It is intentionally private so callers
/// cannot construct a permissive policy from arbitrary runtime configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AutoQualificationPolicy {
    document: QualificationDocument,
}

impl AutoQualificationPolicy {
    pub(crate) fn embedded_current_platform() -> Result<&'static Self, String> {
        #[cfg(any(
            all(target_os = "windows", target_arch = "x86_64"),
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "macos", target_arch = "x86_64")
        ))]
        {
            static POLICY: OnceLock<Result<AutoQualificationPolicy, AutoQualificationError>> =
                OnceLock::new();
            POLICY
                .get_or_init(|| Self::from_canonical_json(EMBEDDED_CURRENT_MANIFEST))
                .as_ref()
                .map_err(ToString::to_string)
        }
        #[cfg(not(any(
            all(target_os = "windows", target_arch = "x86_64"),
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "macos", target_arch = "x86_64")
        )))]
        Err(AutoQualificationError::UnsupportedPlatform.to_string())
    }

    pub(crate) fn embedded_windows_x64() -> Result<&'static Self, String> {
        static POLICY: OnceLock<Result<AutoQualificationPolicy, AutoQualificationError>> =
            OnceLock::new();
        POLICY
            .get_or_init(|| Self::from_canonical_json(EMBEDDED_WINDOWS_X64_MANIFEST))
            .as_ref()
            .map_err(ToString::to_string)
    }

    #[cfg(test)]
    fn embedded_linux_x64() -> Result<&'static Self, String> {
        static POLICY: OnceLock<Result<AutoQualificationPolicy, AutoQualificationError>> =
            OnceLock::new();
        POLICY
            .get_or_init(|| Self::from_canonical_json(EMBEDDED_LINUX_X64_MANIFEST))
            .as_ref()
            .map_err(ToString::to_string)
    }

    pub(crate) fn policy_version(&self) -> u16 {
        AUTO_QUALIFICATION_POLICY_VERSION
    }

    pub(crate) fn manifest_digest(&self) -> String {
        let canonical = serde_json::to_vec(&self.document)
            .expect("validated qualification document must serialize canonically");
        format!("{:x}", Sha256::digest(canonical))
    }

    pub(crate) fn qualify_pack(
        &self,
        backend: BackendKind,
        provider_id: &ProviderIdentity,
        pack: &BackendPackIdentity,
        model_digest: &str,
    ) -> QualificationDecision {
        self.qualify_pack_on_platform(
            std::env::consts::OS,
            std::env::consts::ARCH,
            backend,
            provider_id,
            pack,
            model_digest,
        )
    }

    fn qualify_pack_on_platform(
        &self,
        operating_system: &str,
        architecture: &str,
        backend: BackendKind,
        provider_id: &ProviderIdentity,
        pack: &BackendPackIdentity,
        model_digest: &str,
    ) -> QualificationDecision {
        if operating_system != self.document.target_os || architecture != self.document.target_arch
        {
            return QualificationDecision::Denied(
                QualificationDenial::ManifestNotForCurrentPlatform,
            );
        }
        let Some(_entry) = self
            .document
            .entries
            .iter()
            .find(|entry| entry_matches_pack(entry, backend, provider_id, pack, model_digest))
        else {
            return QualificationDecision::Denied(QualificationDenial::NoMatchingPackEvidence);
        };
        QualificationDecision::Approved {
            evidence_id: _entry.evidence.id.clone(),
        }
    }

    pub(crate) fn qualify_target(
        &self,
        target: &BackendTarget,
        model_digest: &str,
    ) -> QualificationDecision {
        self.qualify_target_on_platform(
            std::env::consts::OS,
            std::env::consts::ARCH,
            target,
            model_digest,
        )
    }

    fn qualify_target_on_platform(
        &self,
        operating_system: &str,
        architecture: &str,
        target: &BackendTarget,
        model_digest: &str,
    ) -> QualificationDecision {
        if operating_system != self.document.target_os || architecture != self.document.target_arch
        {
            return QualificationDecision::Denied(
                QualificationDenial::ManifestNotForCurrentPlatform,
            );
        }
        let Some(pack) = target.pack.as_ref() else {
            return QualificationDecision::Denied(QualificationDenial::MissingTargetPackIdentity);
        };
        let pack_matches = self
            .document
            .entries
            .iter()
            .filter(|entry| {
                entry_matches_pack(
                    entry,
                    target.backend,
                    &target.provider_id,
                    pack,
                    model_digest,
                )
            })
            .collect::<Vec<_>>();
        if pack_matches.is_empty() {
            return QualificationDecision::Denied(QualificationDenial::NoMatchingPackEvidence);
        }
        let Some(driver) = target.driver_version.as_deref() else {
            return QualificationDecision::Denied(QualificationDenial::MissingDriverIdentity);
        };
        if target.memory_available_bytes > target.memory_total_bytes {
            return QualificationDecision::Denied(QualificationDenial::InvalidAvailableMemory);
        }
        if pack_matches
            .iter()
            .all(|entry| target.memory_total_bytes < entry.minimum_total_memory_bytes)
        {
            return QualificationDecision::Denied(QualificationDenial::InsufficientMemory);
        }
        if pack_matches
            .iter()
            .all(|entry| target.memory_available_bytes < entry.minimum_available_memory_bytes)
        {
            return QualificationDecision::Denied(QualificationDenial::InsufficientAvailableMemory);
        }
        let Some(entry) = pack_matches.into_iter().find(|entry| {
            entry.vendor == target.vendor
                && entry.device_class == target.device_class
                && target.memory_total_bytes >= entry.minimum_total_memory_bytes
                && target.memory_available_bytes >= entry.minimum_available_memory_bytes
                && entry.driver.matches(driver)
        }) else {
            return QualificationDecision::Denied(QualificationDenial::NoMatchingTargetEvidence);
        };
        QualificationDecision::Approved {
            evidence_id: entry.evidence.id.clone(),
        }
    }

    fn from_canonical_json(input: &str) -> Result<Self, AutoQualificationError> {
        // Repository text files end in one LF. Keep the serialized document
        // itself compact and canonical while accepting that single transport
        // terminator; whitespace, CRLF, and additional lines remain rejected.
        let document_input = input.strip_suffix('\n').unwrap_or(input);
        let document: QualificationDocument = serde_json::from_str(document_input)
            .map_err(|error| AutoQualificationError::Parse(error.to_string()))?;
        let canonical = serde_json::to_string(&document)
            .map_err(|error| AutoQualificationError::Parse(error.to_string()))?;
        if canonical != document_input {
            return Err(AutoQualificationError::NonCanonical);
        }
        if document.schema_version != QUALIFICATION_SCHEMA_VERSION {
            return Err(AutoQualificationError::UnsupportedSchema);
        }
        if document.mode != QualificationMode::DefaultDeny {
            return Err(AutoQualificationError::UnsafeMode);
        }
        if !supported_platform(&document.target_os, &document.target_arch) {
            return Err(AutoQualificationError::UnsupportedPlatform);
        }
        let mut prior = None;
        for entry in &document.entries {
            validate_entry(entry, &document.target_os)?;
            let key = serde_json::to_string(entry)
                .map_err(|error| AutoQualificationError::Parse(error.to_string()))?;
            if prior
                .as_ref()
                .is_some_and(|previous: &String| previous >= &key)
            {
                return Err(AutoQualificationError::NonCanonicalEntries);
            }
            prior = Some(key);
        }
        Ok(Self { document })
    }

    #[cfg(test)]
    pub(crate) fn from_fixture_json(input: &str) -> Result<Self, AutoQualificationError> {
        Self::from_canonical_json(input)
    }
}

fn entry_matches_pack(
    entry: &QualificationEntry,
    backend: BackendKind,
    provider_id: &ProviderIdentity,
    pack: &BackendPackIdentity,
    model_digest: &str,
) -> bool {
    entry.backend == backend
        && entry.provider_id == provider_id.as_str()
        && entry.pack.pack_id == pack.pack_id
        && entry.pack.pack_version == pack.pack_version
        && entry.pack.pack_digest == pack.pack_digest
        && entry.pack.security_epoch == pack.security_epoch
        && entry.pack.runtime_abi == pack.runtime_abi
        && entry.model_digest == model_digest
}

fn validate_entry(
    entry: &QualificationEntry,
    target_os: &str,
) -> Result<(), AutoQualificationError> {
    if !is_store_component(&entry.pack.pack_id) || !is_store_component(&entry.pack.pack_version) {
        return Err(AutoQualificationError::InvalidEntry("pack identity"));
    }
    if !is_sha256(&entry.pack.pack_digest) || !is_sha256(&entry.model_digest) {
        return Err(AutoQualificationError::InvalidEntry("digest"));
    }
    if entry.pack.security_epoch == 0 || entry.pack.runtime_abi == 0 {
        return Err(AutoQualificationError::InvalidEntry(
            "pack security epoch or runtime ABI",
        ));
    }
    if !platform_backend_binding_is_valid(entry, target_os)
        || !matches!(
            entry.device_class,
            DeviceClass::DiscreteGpu | DeviceClass::IntegratedGpu | DeviceClass::UnifiedGpu
        )
        || !is_identifier(&entry.provider_id, 128)
        || entry.minimum_total_memory_bytes == 0
        || entry.minimum_available_memory_bytes == 0
        || entry.minimum_available_memory_bytes > entry.minimum_total_memory_bytes
        || !is_driver_value(entry.driver.value())
    {
        return Err(AutoQualificationError::InvalidEntry("target binding"));
    }
    let evidence = &entry.evidence;
    if !is_identifier(&evidence.id, 160)
        || evidence.cold_runs < 5
        || evidence.warm_runs < 20
        || evidence.gpu_p95_ms == 0
        || evidence.cpu_p95_ms == 0
        || !evidence.correctness_verified
        || !evidence.reliability_verified
        || !is_sha256(&evidence.cold_evidence_sha256)
        || !is_sha256(&evidence.warm_evidence_sha256)
        || !is_sha256(&evidence.transcript_parity_evidence_sha256)
    {
        return Err(AutoQualificationError::InvalidEntry("release evidence"));
    }
    if u128::from(evidence.gpu_p95_ms) * 100 > u128::from(evidence.cpu_p95_ms) * 110 {
        return Err(AutoQualificationError::InvalidEntry(
            "p95 performance threshold",
        ));
    }
    Ok(())
}

fn supported_platform(target_os: &str, target_arch: &str) -> bool {
    matches!(
        (target_os, target_arch),
        ("windows", "x86_64") | ("linux", "x86_64") | ("macos", "aarch64") | ("macos", "x86_64")
    )
}

fn platform_backend_binding_is_valid(entry: &QualificationEntry, target_os: &str) -> bool {
    match (target_os, entry.backend) {
        ("windows" | "linux", BackendKind::Cuda) => {
            entry.provider_id == "transcribe-cpp-ggml-cuda" && entry.vendor == GpuVendor::Nvidia
        }
        ("windows" | "linux", BackendKind::Vulkan) => {
            entry.provider_id == "transcribe-cpp-ggml-vulkan"
                && matches!(
                    entry.vendor,
                    GpuVendor::Nvidia | GpuVendor::Amd | GpuVendor::Intel
                )
        }
        ("macos", BackendKind::Metal) => {
            entry.provider_id == "transcribe-cpp-ggml-metal"
                && matches!(
                    entry.vendor,
                    GpuVendor::Apple | GpuVendor::Amd | GpuVendor::Intel
                )
        }
        _ => false,
    }
}

fn is_store_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && value
            .bytes()
            .last()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
}

fn is_identifier(value: &str, max_len: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_len
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'.' | b'_' | b'-' | b':')
        })
}

fn is_driver_value(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| (0x20..=0x7e).contains(&byte) && byte != b'\\')
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE_DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const MODEL_DIGEST: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const EVIDENCE_DIGEST: &str =
        "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

    fn fixture_entry() -> QualificationEntry {
        QualificationEntry {
            pack: PackBinding {
                pack_id: "scribe-cuda-windows-x64".to_owned(),
                pack_version: "1.0.0".to_owned(),
                pack_digest: FIXTURE_DIGEST.to_owned(),
                security_epoch: 7,
                runtime_abi: 3,
            },
            model_digest: MODEL_DIGEST.to_owned(),
            backend: BackendKind::Cuda,
            provider_id: "transcribe-cpp-ggml-cuda".to_owned(),
            vendor: GpuVendor::Nvidia,
            device_class: DeviceClass::DiscreteGpu,
            minimum_total_memory_bytes: 8 * 1024 * 1024 * 1024,
            minimum_available_memory_bytes: 4 * 1024 * 1024 * 1024,
            driver: DriverConstraint::Exact {
                value: "windows-display:32.0.16.1088".to_owned(),
            },
            evidence: QualificationEvidence {
                id: "windows-nvidia-cuda-fixture-v1".to_owned(),
                cold_runs: 5,
                warm_runs: 20,
                gpu_p95_ms: 110,
                cpu_p95_ms: 100,
                correctness_verified: true,
                reliability_verified: true,
                cold_evidence_sha256: EVIDENCE_DIGEST.to_owned(),
                warm_evidence_sha256: EVIDENCE_DIGEST.to_owned(),
                transcript_parity_evidence_sha256: EVIDENCE_DIGEST.to_owned(),
            },
        }
    }

    fn fixture_document() -> QualificationDocument {
        QualificationDocument {
            schema_version: QUALIFICATION_SCHEMA_VERSION,
            mode: QualificationMode::DefaultDeny,
            target_os: WINDOWS_X64_OS.to_owned(),
            target_arch: WINDOWS_X64_ARCH.to_owned(),
            entries: vec![fixture_entry()],
        }
    }

    fn fixture_policy() -> AutoQualificationPolicy {
        let document = fixture_document();
        AutoQualificationPolicy::from_fixture_json(&serde_json::to_string(&document).unwrap())
            .unwrap()
    }

    fn canonical_fixture_json() -> String {
        serde_json::to_string(&fixture_document()).unwrap()
    }

    fn fixture_target() -> BackendTarget {
        BackendTarget {
            backend: BackendKind::Cuda,
            provider_id: ProviderIdentity::new("transcribe-cpp-ggml-cuda"),
            driver_version: Some("windows-display:32.0.16.1088".to_owned()),
            device_id: crate::backend_policy::DeviceIdentity::new("native:pci:0000:01:00.0"),
            display_name: "Fixture RTX".to_owned(),
            vendor: GpuVendor::Nvidia,
            device_class: DeviceClass::DiscreteGpu,
            memory_total_bytes: 8 * 1024 * 1024 * 1024,
            memory_available_bytes: 6 * 1024 * 1024 * 1024,
            pack: Some(BackendPackIdentity {
                pack_id: "scribe-cuda-windows-x64".to_owned(),
                pack_version: "1.0.0".to_owned(),
                pack_digest: FIXTURE_DIGEST.to_owned(),
                security_epoch: 7,
                runtime_abi: 3,
            }),
            process_index: Some(0),
        }
    }

    #[test]
    fn embedded_manifest_is_strict_default_deny_with_no_production_entries() {
        let policy = AutoQualificationPolicy::embedded_windows_x64().unwrap();
        assert_eq!(policy.policy_version(), AUTO_QUALIFICATION_POLICY_VERSION);
        assert!(policy.document.entries.is_empty());
        let target = fixture_target();
        assert_eq!(
            policy.qualify_target_on_platform(
                WINDOWS_X64_OS,
                WINDOWS_X64_ARCH,
                &target,
                MODEL_DIGEST
            ),
            QualificationDecision::Denied(QualificationDenial::NoMatchingPackEvidence)
        );
    }

    #[test]
    fn embedded_linux_manifest_is_strict_default_deny_with_no_production_entries() {
        let policy = AutoQualificationPolicy::embedded_linux_x64().unwrap();
        assert_eq!(policy.document.target_os, LINUX_X64_OS);
        assert_eq!(policy.document.target_arch, LINUX_X64_ARCH);
        assert!(policy.document.entries.is_empty());
    }

    #[test]
    fn qualified_fixture_requires_every_pack_model_and_target_fact() {
        let policy = fixture_policy();
        // Unit fixtures intentionally exercise the Windows manifest on every
        // host. Production calls additionally compare the current OS/arch.
        let mut target = fixture_target();
        let entry = policy.document.entries.first().unwrap();
        assert!(entry_matches_pack(
            entry,
            target.backend,
            &target.provider_id,
            target.pack.as_ref().unwrap(),
            MODEL_DIGEST
        ));
        assert!(matches!(
            policy.qualify_target_on_platform(
                WINDOWS_X64_OS,
                WINDOWS_X64_ARCH,
                &target,
                MODEL_DIGEST
            ),
            QualificationDecision::Approved { .. }
        ));

        target.pack.as_mut().unwrap().pack_digest = "d".repeat(64);
        assert_eq!(
            policy.qualify_target_on_platform(
                WINDOWS_X64_OS,
                WINDOWS_X64_ARCH,
                &target,
                MODEL_DIGEST
            ),
            QualificationDecision::Denied(QualificationDenial::NoMatchingPackEvidence)
        );
        target = fixture_target();
        assert_eq!(
            policy.qualify_target_on_platform(
                WINDOWS_X64_OS,
                WINDOWS_X64_ARCH,
                &target,
                &"d".repeat(64)
            ),
            QualificationDecision::Denied(QualificationDenial::NoMatchingPackEvidence)
        );
        target.driver_version = Some("windows-display:32.0.16.9999".to_owned());
        assert_eq!(
            policy.qualify_target_on_platform(
                WINDOWS_X64_OS,
                WINDOWS_X64_ARCH,
                &target,
                MODEL_DIGEST
            ),
            QualificationDecision::Denied(QualificationDenial::NoMatchingTargetEvidence)
        );
        target = fixture_target();
        target.vendor = GpuVendor::Amd;
        assert_eq!(
            policy.qualify_target_on_platform(
                WINDOWS_X64_OS,
                WINDOWS_X64_ARCH,
                &target,
                MODEL_DIGEST
            ),
            QualificationDecision::Denied(QualificationDenial::NoMatchingTargetEvidence)
        );
        target = fixture_target();
        target.memory_total_bytes -= 1;
        assert_eq!(
            policy.qualify_target_on_platform(
                WINDOWS_X64_OS,
                WINDOWS_X64_ARCH,
                &target,
                MODEL_DIGEST
            ),
            QualificationDecision::Denied(QualificationDenial::InsufficientMemory)
        );
    }

    #[test]
    fn pack_gate_binds_platform_backend_provider_and_complete_pack_identity() {
        let policy = fixture_policy();
        let target = fixture_target();
        let pack = target.pack.as_ref().unwrap();
        let provider = &target.provider_id;

        assert!(matches!(
            policy.qualify_pack_on_platform(
                WINDOWS_X64_OS,
                WINDOWS_X64_ARCH,
                BackendKind::Cuda,
                provider,
                pack,
                MODEL_DIGEST,
            ),
            QualificationDecision::Approved { .. }
        ));
        assert_eq!(
            policy.qualify_pack_on_platform(
                "linux",
                WINDOWS_X64_ARCH,
                BackendKind::Cuda,
                provider,
                pack,
                MODEL_DIGEST,
            ),
            QualificationDecision::Denied(QualificationDenial::ManifestNotForCurrentPlatform)
        );

        let mut mismatched_pack = pack.clone();
        for mutation in [
            |value: &mut BackendPackIdentity| value.pack_id = "different-pack".to_owned(),
            |value: &mut BackendPackIdentity| value.pack_version = "2.0.0".to_owned(),
            |value: &mut BackendPackIdentity| value.pack_digest = "d".repeat(64),
            |value: &mut BackendPackIdentity| value.security_epoch += 1,
            |value: &mut BackendPackIdentity| value.runtime_abi += 1,
        ] {
            mismatched_pack.clone_from(pack);
            mutation(&mut mismatched_pack);
            assert_eq!(
                policy.qualify_pack_on_platform(
                    WINDOWS_X64_OS,
                    WINDOWS_X64_ARCH,
                    BackendKind::Cuda,
                    provider,
                    &mismatched_pack,
                    MODEL_DIGEST,
                ),
                QualificationDecision::Denied(QualificationDenial::NoMatchingPackEvidence)
            );
        }
        assert_eq!(
            policy.qualify_pack_on_platform(
                WINDOWS_X64_OS,
                WINDOWS_X64_ARCH,
                BackendKind::Vulkan,
                provider,
                pack,
                MODEL_DIGEST,
            ),
            QualificationDecision::Denied(QualificationDenial::NoMatchingPackEvidence)
        );
        assert_eq!(
            policy.qualify_pack_on_platform(
                WINDOWS_X64_OS,
                WINDOWS_X64_ARCH,
                BackendKind::Cuda,
                &ProviderIdentity::new("transcribe-cpp-ggml-vulkan"),
                pack,
                MODEL_DIGEST,
            ),
            QualificationDecision::Denied(QualificationDenial::NoMatchingPackEvidence)
        );
    }

    #[test]
    fn target_gate_requires_pack_driver_vendor_class_and_live_total_and_available_memory() {
        let policy = fixture_policy();
        let approve = |target: &BackendTarget| {
            policy.qualify_target_on_platform(
                WINDOWS_X64_OS,
                WINDOWS_X64_ARCH,
                target,
                MODEL_DIGEST,
            )
        };

        let mut target = fixture_target();
        target.memory_available_bytes = 0;
        assert_eq!(
            approve(&target),
            QualificationDecision::Denied(QualificationDenial::InsufficientAvailableMemory)
        );

        target = fixture_target();
        target.memory_available_bytes = 4 * 1024 * 1024 * 1024 - 1;
        assert_eq!(
            approve(&target),
            QualificationDecision::Denied(QualificationDenial::InsufficientAvailableMemory)
        );

        target = fixture_target();
        target.memory_available_bytes = target.memory_total_bytes + 1;
        assert_eq!(
            approve(&target),
            QualificationDecision::Denied(QualificationDenial::InvalidAvailableMemory)
        );

        target.pack = None;
        assert_eq!(
            approve(&target),
            QualificationDecision::Denied(QualificationDenial::MissingTargetPackIdentity)
        );
        target = fixture_target();
        target.driver_version = None;
        assert_eq!(
            approve(&target),
            QualificationDecision::Denied(QualificationDenial::MissingDriverIdentity)
        );
        target = fixture_target();
        target.provider_id = ProviderIdentity::new("transcribe-cpp-ggml-vulkan");
        assert_eq!(
            approve(&target),
            QualificationDecision::Denied(QualificationDenial::NoMatchingPackEvidence)
        );
        target = fixture_target();
        target.backend = BackendKind::Vulkan;
        assert_eq!(
            approve(&target),
            QualificationDecision::Denied(QualificationDenial::NoMatchingPackEvidence)
        );
        target = fixture_target();
        target.device_class = DeviceClass::IntegratedGpu;
        assert_eq!(
            approve(&target),
            QualificationDecision::Denied(QualificationDenial::NoMatchingTargetEvidence)
        );
    }

    #[test]
    fn canonical_manifest_rejects_whitespace_duplicate_or_unsorted_entries() {
        let canonical = canonical_fixture_json();
        assert!(AutoQualificationPolicy::from_fixture_json(&(canonical.clone() + "\n")).is_ok());
        for noncanonical in [
            format!(" {canonical}"),
            format!("{canonical} "),
            format!("{canonical}\r\n"),
            format!("{canonical}\n\n"),
            serde_json::to_string_pretty(&fixture_document()).unwrap(),
        ] {
            assert_eq!(
                AutoQualificationPolicy::from_fixture_json(&noncanonical),
                Err(AutoQualificationError::NonCanonical)
            );
        }

        let mut duplicate = fixture_document();
        duplicate.entries.push(duplicate.entries[0].clone());
        assert_eq!(
            AutoQualificationPolicy::from_fixture_json(&serde_json::to_string(&duplicate).unwrap()),
            Err(AutoQualificationError::NonCanonicalEntries)
        );

        let mut unsorted = fixture_document();
        let mut first = unsorted.entries[0].clone();
        first.pack.pack_id = "z-pack".to_owned();
        let mut second = unsorted.entries[0].clone();
        second.pack.pack_id = "a-pack".to_owned();
        unsorted.entries = vec![first, second];
        assert_eq!(
            AutoQualificationPolicy::from_fixture_json(&serde_json::to_string(&unsorted).unwrap()),
            Err(AutoQualificationError::NonCanonicalEntries)
        );
    }

    #[test]
    fn strict_schema_rejects_unknown_fields_wrong_types_and_invalid_digests() {
        let mut value = serde_json::to_value(fixture_document()).unwrap();
        value["unexpected"] = serde_json::json!(true);
        assert!(matches!(
            AutoQualificationPolicy::from_fixture_json(&serde_json::to_string(&value).unwrap()),
            Err(AutoQualificationError::Parse(_))
        ));

        for path in ["pack", "driver", "evidence"] {
            let mut value = serde_json::to_value(fixture_document()).unwrap();
            value["entries"][0][path]["unexpected"] = serde_json::json!(true);
            assert!(matches!(
                AutoQualificationPolicy::from_fixture_json(&serde_json::to_string(&value).unwrap()),
                Err(AutoQualificationError::Parse(_))
            ));
        }

        let mut missing_available_memory_floor = serde_json::to_value(fixture_document()).unwrap();
        missing_available_memory_floor["entries"][0]
            .as_object_mut()
            .unwrap()
            .remove("minimum_available_memory_bytes");
        assert!(matches!(
            AutoQualificationPolicy::from_fixture_json(
                &serde_json::to_string(&missing_available_memory_floor).unwrap()
            ),
            Err(AutoQualificationError::Parse(_))
        ));

        for (field, invalid) in [
            ("cold_runs", serde_json::json!("5")),
            ("warm_runs", serde_json::json!(20.0)),
            ("gpu_p95_ms", serde_json::json!(-1)),
            ("correctness_verified", serde_json::json!("false")),
            ("reliability_verified", serde_json::json!(1)),
        ] {
            let mut value = serde_json::to_value(fixture_document()).unwrap();
            value["entries"][0]["evidence"][field] = invalid;
            assert!(matches!(
                AutoQualificationPolicy::from_fixture_json(&serde_json::to_string(&value).unwrap()),
                Err(AutoQualificationError::Parse(_))
            ));
        }

        for field in [
            "pack_digest",
            "model_digest",
            "cold_evidence_sha256",
            "warm_evidence_sha256",
            "transcript_parity_evidence_sha256",
        ] {
            let mut document = fixture_document();
            match field {
                "pack_digest" => document.entries[0].pack.pack_digest = "A".repeat(64),
                "model_digest" => document.entries[0].model_digest = "b".repeat(63),
                "cold_evidence_sha256" => {
                    document.entries[0].evidence.cold_evidence_sha256 = "g".repeat(64)
                }
                "warm_evidence_sha256" => document.entries[0].evidence.warm_evidence_sha256.clear(),
                "transcript_parity_evidence_sha256" => {
                    document.entries[0]
                        .evidence
                        .transcript_parity_evidence_sha256 = "0".repeat(65)
                }
                _ => unreachable!(),
            }
            assert!(matches!(
                AutoQualificationPolicy::from_fixture_json(
                    &serde_json::to_string(&document).unwrap()
                ),
                Err(AutoQualificationError::InvalidEntry(_))
            ));
        }
    }

    #[test]
    fn release_evidence_enforces_every_threshold_and_boolean() {
        for mutation in [
            |evidence: &mut QualificationEvidence| evidence.cold_runs = 4,
            |evidence: &mut QualificationEvidence| evidence.warm_runs = 19,
            |evidence: &mut QualificationEvidence| evidence.gpu_p95_ms = 0,
            |evidence: &mut QualificationEvidence| evidence.cpu_p95_ms = 0,
            |evidence: &mut QualificationEvidence| evidence.gpu_p95_ms = 111,
            |evidence: &mut QualificationEvidence| evidence.correctness_verified = false,
            |evidence: &mut QualificationEvidence| evidence.reliability_verified = false,
        ] {
            let mut document = fixture_document();
            mutation(&mut document.entries[0].evidence);
            assert!(matches!(
                AutoQualificationPolicy::from_fixture_json(
                    &serde_json::to_string(&document).unwrap()
                ),
                Err(AutoQualificationError::InvalidEntry(_))
            ));
        }
    }

    #[test]
    fn windows_entries_reject_backend_provider_and_vendor_mismatches() {
        for mutation in [
            |entry: &mut QualificationEntry| {
                entry.provider_id = "transcribe-cpp-ggml-vulkan".to_owned()
            },
            |entry: &mut QualificationEntry| entry.vendor = GpuVendor::Amd,
            |entry: &mut QualificationEntry| entry.backend = BackendKind::Metal,
        ] {
            let mut document = fixture_document();
            mutation(&mut document.entries[0]);
            assert_eq!(
                AutoQualificationPolicy::from_fixture_json(
                    &serde_json::to_string(&document).unwrap()
                ),
                Err(AutoQualificationError::InvalidEntry("target binding"))
            );
        }
    }

    #[test]
    fn entries_require_a_positive_available_memory_floor_not_above_total_memory() {
        let mutations: [fn(&mut QualificationEntry); 2] = [
            |entry: &mut QualificationEntry| entry.minimum_available_memory_bytes = 0,
            |entry: &mut QualificationEntry| {
                entry.minimum_available_memory_bytes = entry.minimum_total_memory_bytes + 1
            },
        ];
        for mutation in mutations {
            let mut document = fixture_document();
            mutation(&mut document.entries[0]);
            assert_eq!(
                AutoQualificationPolicy::from_fixture_json(
                    &serde_json::to_string(&document).unwrap()
                ),
                Err(AutoQualificationError::InvalidEntry("target binding"))
            );
        }
    }

    #[test]
    fn linux_entries_accept_only_reviewed_cuda_and_vulkan_bindings() {
        for (backend, provider, vendor) in [
            (
                BackendKind::Cuda,
                "transcribe-cpp-ggml-cuda",
                GpuVendor::Nvidia,
            ),
            (
                BackendKind::Vulkan,
                "transcribe-cpp-ggml-vulkan",
                GpuVendor::Nvidia,
            ),
            (
                BackendKind::Vulkan,
                "transcribe-cpp-ggml-vulkan",
                GpuVendor::Amd,
            ),
            (
                BackendKind::Vulkan,
                "transcribe-cpp-ggml-vulkan",
                GpuVendor::Intel,
            ),
        ] {
            let mut document = fixture_document();
            document.target_os = LINUX_X64_OS.to_owned();
            document.target_arch = LINUX_X64_ARCH.to_owned();
            document.entries[0].backend = backend;
            document.entries[0].provider_id = provider.to_owned();
            document.entries[0].vendor = vendor;
            assert!(
                AutoQualificationPolicy::from_fixture_json(
                    &serde_json::to_string(&document).unwrap()
                )
                .is_ok()
            );
        }

        for (backend, provider, vendor) in [
            (
                BackendKind::Cuda,
                "transcribe-cpp-ggml-cuda",
                GpuVendor::Amd,
            ),
            (
                BackendKind::Vulkan,
                "transcribe-cpp-ggml-cuda",
                GpuVendor::Nvidia,
            ),
            (
                BackendKind::Metal,
                "transcribe-cpp-ggml-metal",
                GpuVendor::Nvidia,
            ),
        ] {
            let mut document = fixture_document();
            document.target_os = LINUX_X64_OS.to_owned();
            document.target_arch = LINUX_X64_ARCH.to_owned();
            document.entries[0].backend = backend;
            document.entries[0].provider_id = provider.to_owned();
            document.entries[0].vendor = vendor;
            assert_eq!(
                AutoQualificationPolicy::from_fixture_json(
                    &serde_json::to_string(&document).unwrap()
                ),
                Err(AutoQualificationError::InvalidEntry("target binding"))
            );
        }
    }

    #[test]
    fn macos_entries_accept_only_metal_with_supported_hardware_vendors() {
        for (vendor, class) in [
            (GpuVendor::Apple, DeviceClass::UnifiedGpu),
            (GpuVendor::Intel, DeviceClass::IntegratedGpu),
            (GpuVendor::Amd, DeviceClass::DiscreteGpu),
        ] {
            let mut document = fixture_document();
            document.target_os = "macos".to_owned();
            document.target_arch = "aarch64".to_owned();
            let entry = &mut document.entries[0];
            entry.pack.pack_id = "scribe-metal-macos-aarch64".to_owned();
            entry.backend = BackendKind::Metal;
            entry.provider_id = "transcribe-cpp-ggml-metal".to_owned();
            entry.vendor = vendor;
            entry.device_class = class;
            entry.driver = DriverConstraint::Exact {
                value: "macos-build:23f79".to_owned(),
            };
            assert!(
                AutoQualificationPolicy::from_fixture_json(
                    &serde_json::to_string(&document).unwrap()
                )
                .is_ok()
            );
        }

        let mut invalid = fixture_document();
        invalid.target_os = "macos".to_owned();
        invalid.target_arch = "x86_64".to_owned();
        invalid.entries[0].backend = BackendKind::Vulkan;
        assert_eq!(
            AutoQualificationPolicy::from_fixture_json(&serde_json::to_string(&invalid).unwrap()),
            Err(AutoQualificationError::InvalidEntry("target binding"))
        );
    }

    #[test]
    fn current_platform_manifest_is_canonical_and_default_deny() {
        #[cfg(any(
            all(target_os = "windows", target_arch = "x86_64"),
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "macos", target_arch = "x86_64")
        ))]
        {
            let policy = AutoQualificationPolicy::embedded_current_platform().unwrap();
            assert_eq!(policy.document.target_os, std::env::consts::OS);
            assert_eq!(policy.document.target_arch, std::env::consts::ARCH);
            assert!(policy.document.entries.is_empty());
        }
    }

    #[test]
    fn malformed_or_unqualified_evidence_is_rejected_before_runtime() {
        let mut document = fixture_document();
        document.entries[0].evidence.warm_runs = 19;
        let input = serde_json::to_string(&document).unwrap();
        assert!(matches!(
            AutoQualificationPolicy::from_fixture_json(&input),
            Err(AutoQualificationError::InvalidEntry("release evidence"))
        ));
        document.entries[0].evidence.warm_runs = 20;
        document.entries[0].evidence.gpu_p95_ms = 111;
        let input = serde_json::to_string(&document).unwrap();
        assert!(matches!(
            AutoQualificationPolicy::from_fixture_json(&input),
            Err(AutoQualificationError::InvalidEntry(
                "p95 performance threshold"
            ))
        ));
        assert!(matches!(
            AutoQualificationPolicy::from_fixture_json("{\"schema_version\":1}"),
            Err(AutoQualificationError::Parse(_))
        ));
        let mut old_schema = fixture_document();
        old_schema.schema_version = QUALIFICATION_SCHEMA_VERSION - 1;
        assert_eq!(
            AutoQualificationPolicy::from_fixture_json(
                &serde_json::to_string(&old_schema).unwrap()
            ),
            Err(AutoQualificationError::UnsupportedSchema),
            "pre-available-memory qualification entries must fail closed"
        );
        for (backend, vendor, device_class) in [
            (
                BackendKind::Cpu,
                GpuVendor::Nvidia,
                DeviceClass::DiscreteGpu,
            ),
            (
                BackendKind::Cuda,
                GpuVendor::Unknown,
                DeviceClass::DiscreteGpu,
            ),
            (BackendKind::Cuda, GpuVendor::Nvidia, DeviceClass::Cpu),
        ] {
            let mut invalid_target = fixture_document();
            invalid_target.entries[0].backend = backend;
            invalid_target.entries[0].vendor = vendor;
            invalid_target.entries[0].device_class = device_class;
            let input = serde_json::to_string(&invalid_target).unwrap();
            assert!(matches!(
                AutoQualificationPolicy::from_fixture_json(&input),
                Err(AutoQualificationError::InvalidEntry("target binding"))
            ));
        }
        assert!(matches!(
            AutoQualificationPolicy::from_fixture_json(
                "{\"schema_version\":2,\"mode\":\"default_deny\",\"target_os\":\"windows\",\"target_arch\":\"x86_64\",\"entries\":[],\"unexpected\":true}"
            ),
            Err(AutoQualificationError::Parse(_))
        ));
    }
}
