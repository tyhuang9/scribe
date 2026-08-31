//! Runtime-neutral compute-backend selection policy.
//!
//! The policy operates only on an injected [`BackendSnapshot`]. Native runtime
//! discovery stays in the adapter that owns that runtime, which keeps policy
//! tests deterministic and lets future worker packs provide the same facts
//! without exposing provider-specific handles above the worker boundary.
//!
//! Total memory is a stable target fact. Available memory is volatile and is
//! deliberately excluded from target identity and warm-state fingerprints;
//! the verified Auto qualification policy consumes it only as fresh,
//! evidence-bound admission input.

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::transcription::AccelerationPreference;

/// Compute implementation selected for a model load.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BackendKind {
    Cpu,
    Cuda,
    Vulkan,
    Metal,
}

impl BackendKind {
    pub(crate) fn is_gpu(self) -> bool {
        !matches!(self, Self::Cpu)
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::Cuda => "CUDA",
            Self::Vulkan => "Vulkan",
            Self::Metal => "Metal",
        }
    }
}

/// Hardware vendor used by the fixed backend priority policy.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GpuVendor {
    Nvidia,
    Amd,
    Intel,
    Apple,
    Other,
    Unknown,
}

/// Backend-reported memory and power class.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DeviceClass {
    Cpu,
    Accelerator,
    DiscreteGpu,
    IntegratedGpu,
    UnifiedGpu,
    Unknown,
}

impl DeviceClass {
    fn is_gpu(self) -> bool {
        matches!(
            self,
            Self::DiscreteGpu | Self::IntegratedGpu | Self::UnifiedGpu | Self::Unknown
        )
    }

    pub(crate) fn is_battery_eligible_gpu(self) -> bool {
        matches!(self, Self::IntegratedGpu | Self::UnifiedGpu)
    }
}

/// Power source observed immediately before selecting a backend.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PowerSource {
    Ac,
    Battery,
    Unknown,
}

impl PowerSource {
    pub(crate) fn current() -> Self {
        current_power_source()
    }
}

/// Operating-system family used by the fixed backend compatibility table.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) enum OperatingSystem {
    Windows,
    MacOs,
    Linux,
    Other,
}

impl OperatingSystem {
    pub(crate) const fn current() -> Self {
        if cfg!(target_os = "windows") {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else {
            Self::Other
        }
    }
}

/// Stable device identity used across enumeration-order and driver changes.
///
/// A provider should use its OS/native stable identifier when one is
/// available. If it has none, it may supply a deterministic provider/name
/// fingerprint. The registry index is deliberately not part of this value.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct DeviceIdentity(String);

impl DeviceIdentity {
    pub(crate) fn new(stable_id: impl Into<String>) -> Self {
        let stable_id = stable_id.into();
        let trimmed = stable_id.trim();
        debug_assert!(!trimmed.is_empty(), "device identity must not be empty");
        Self(if trimmed.is_empty() {
            "unknown-device".to_owned()
        } else {
            trimmed.to_ascii_lowercase()
        })
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    fn canonical_key(&self) -> String {
        self.0.clone()
    }

    pub(crate) fn is_derived(&self) -> bool {
        self.0.starts_with("derived:")
    }
}

/// Versioned provider identity used by qualification and warm invalidation.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct ProviderIdentity(String);

impl ProviderIdentity {
    pub(crate) fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        let trimmed = value.trim();
        debug_assert!(!trimmed.is_empty(), "provider identity must not be empty");
        Self(if trimmed.is_empty() {
            "unknown-provider".to_owned()
        } else {
            trimmed.to_ascii_lowercase()
        })
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

/// Immutable worker-pack identity attached to GPU targets. Executable paths
/// are deliberately excluded; only a resolver-owned verified lease authorizes
/// launch.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BackendPackIdentity {
    pub(crate) pack_id: String,
    pub(crate) pack_version: String,
    pub(crate) pack_digest: String,
    pub(crate) security_epoch: u64,
    pub(crate) runtime_abi: u16,
}

/// One backend/device pair that can be selected in the current process.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct BackendTarget {
    pub(crate) backend: BackendKind,
    pub(crate) provider_id: ProviderIdentity,
    /// Provider/driver build fact when the discovery API exposes it. The
    /// pinned transcribe-cpp API does not currently report a driver version.
    pub(crate) driver_version: Option<String>,
    pub(crate) device_id: DeviceIdentity,
    pub(crate) display_name: String,
    pub(crate) vendor: GpuVendor,
    pub(crate) device_class: DeviceClass,
    pub(crate) memory_total_bytes: u64,
    /// Live resource fact. Signed Auto qualification may admit it against an
    /// evidence-bound threshold, but it never enters a stable identity or
    /// warm-state fingerprint.
    pub(crate) memory_available_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) pack: Option<BackendPackIdentity>,
    /// Provider registry index for this process only. It is rediscovered from
    /// `device_id` for every fresh snapshot and must never become durable.
    #[serde(skip)]
    pub(crate) process_index: Option<usize>,
}

impl BackendTarget {
    pub(crate) fn cpu() -> Self {
        Self {
            backend: BackendKind::Cpu,
            provider_id: ProviderIdentity::new("transcribe-cpp:cpu"),
            driver_version: None,
            device_id: DeviceIdentity::new("cpu:system"),
            display_name: "CPU".to_owned(),
            vendor: GpuVendor::Unknown,
            device_class: DeviceClass::Cpu,
            memory_total_bytes: 0,
            memory_available_bytes: 0,
            pack: None,
            process_index: None,
        }
    }

    fn is_structurally_valid(&self) -> bool {
        if self.backend.is_gpu() {
            self.device_class.is_gpu()
        } else {
            matches!(
                self.device_class,
                DeviceClass::Cpu | DeviceClass::Accelerator
            )
        }
    }

    fn dedup_key(&self) -> (BackendKind, ProviderIdentity, String) {
        (
            self.backend,
            self.provider_id.clone(),
            self.device_id.canonical_key(),
        )
    }
}

/// Current usability of one discovered backend target.
#[allow(
    dead_code,
    reason = "provider and health integrations construct every state in later stacked stages"
)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum CandidateAvailability {
    Available,
    Unaddressable,
    Incompatible,
    Unhealthy,
    Quarantined,
}

impl CandidateAvailability {
    fn rank(self) -> u8 {
        match self {
            Self::Available => 0,
            Self::Unaddressable => 1,
            Self::Quarantined => 2,
            Self::Unhealthy => 3,
            Self::Incompatible => 4,
        }
    }
}

/// Provider-supplied selection input for one target.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BackendCandidate {
    pub(crate) target: BackendTarget,
    pub(crate) availability: CandidateAvailability,
}

impl BackendCandidate {
    pub(crate) fn available(target: BackendTarget) -> Self {
        Self {
            target,
            availability: CandidateAvailability::Available,
        }
    }
}

/// Narrow projection from private health state into runtime-neutral selection.
/// Implementations must not expose persisted health details or provider paths.
pub(crate) trait CandidateQuarantineProjection {
    fn is_quarantined(&self, target: &BackendTarget) -> bool;
}

pub(crate) fn apply_quarantine_projection(
    candidates: &mut [BackendCandidate],
    projection: &dyn CandidateQuarantineProjection,
) {
    for candidate in candidates {
        if candidate.availability == CandidateAvailability::Available
            && candidate.target.backend.is_gpu()
            && projection.is_quarantined(&candidate.target)
        {
            candidate.availability = CandidateAvailability::Quarantined;
        }
    }
}

/// Auto qualification is deliberately an explicit, versioned allowlist.
/// Stage 1 ships no entries: discovered GPUs remain available to explicit GPU
/// mode while Auto stays on the guaranteed CPU path.
pub(crate) const BACKEND_QUALIFICATION_POLICY_VERSION: u16 = 1;

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct BackendQualification {
    pub(crate) operating_system: OperatingSystem,
    pub(crate) backend: BackendKind,
    pub(crate) provider_id: ProviderIdentity,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BackendQualificationPolicy {
    pub(crate) version: u16,
    qualified: Vec<BackendQualification>,
}

impl BackendQualificationPolicy {
    pub(crate) fn stage_one_default_deny() -> Self {
        Self {
            version: BACKEND_QUALIFICATION_POLICY_VERSION,
            qualified: Vec::new(),
        }
    }

    fn qualifies(&self, operating_system: OperatingSystem, target: &BackendTarget) -> bool {
        self.qualified
            .binary_search(&BackendQualification {
                operating_system,
                backend: target.backend,
                provider_id: target.provider_id.clone(),
            })
            .is_ok()
    }

    #[cfg(test)]
    pub(crate) fn qualify_all_for_testing(
        operating_system: OperatingSystem,
        candidates: &[BackendCandidate],
    ) -> Self {
        let mut qualified = candidates
            .iter()
            .filter(|candidate| candidate.target.backend.is_gpu())
            .map(|candidate| BackendQualification {
                operating_system,
                backend: candidate.target.backend,
                provider_id: candidate.target.provider_id.clone(),
            })
            .collect::<Vec<_>>();
        qualified.sort();
        qualified.dedup();
        Self {
            version: BACKEND_QUALIFICATION_POLICY_VERSION,
            qualified,
        }
    }
}

/// Injectable view of the machine at selection time.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BackendSnapshot {
    pub(crate) operating_system: OperatingSystem,
    pub(crate) power_source: PowerSource,
    pub(crate) candidates: Vec<BackendCandidate>,
    pub(crate) qualification_policy: BackendQualificationPolicy,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BackendEnvironmentFingerprint {
    operating_system: OperatingSystem,
    power_source: PowerSource,
    qualification_policy_version: u16,
    candidates: Vec<BackendCandidateFingerprint>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct BackendCandidateFingerprint {
    backend: BackendKind,
    provider_id: ProviderIdentity,
    driver_version: Option<String>,
    device_id: DeviceIdentity,
    display_name: String,
    vendor: GpuVendor,
    device_class: DeviceClass,
    availability: CandidateAvailability,
    auto_qualified: bool,
    memory_total_bytes: u64,
    pack: Option<BackendPackIdentity>,
}

impl BackendSnapshot {
    /// Facts which make an already-loaded model safe to reuse. Volatile free
    /// memory is excluded because model-aware memory preflight is deferred.
    pub(crate) fn environment_fingerprint(&self) -> BackendEnvironmentFingerprint {
        let mut candidates = self
            .candidates
            .iter()
            .map(|candidate| BackendCandidateFingerprint {
                backend: candidate.target.backend,
                provider_id: candidate.target.provider_id.clone(),
                driver_version: candidate.target.driver_version.clone(),
                device_id: candidate.target.device_id.clone(),
                display_name: candidate.target.display_name.trim().to_ascii_lowercase(),
                vendor: candidate.target.vendor,
                device_class: candidate.target.device_class,
                availability: candidate.availability,
                auto_qualified: self
                    .qualification_policy
                    .qualifies(self.operating_system, &candidate.target),
                memory_total_bytes: candidate.target.memory_total_bytes,
                pack: candidate.target.pack.clone(),
            })
            .collect::<Vec<_>>();
        candidates.sort();
        Self::fingerprint(
            self.operating_system,
            self.power_source,
            self.qualification_policy.version,
            candidates,
        )
    }

    fn fingerprint(
        operating_system: OperatingSystem,
        power_source: PowerSource,
        qualification_policy_version: u16,
        candidates: Vec<BackendCandidateFingerprint>,
    ) -> BackendEnvironmentFingerprint {
        BackendEnvironmentFingerprint {
            operating_system,
            power_source,
            qualification_policy_version,
            candidates,
        }
    }
}

/// Why the selected target won.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BackendSelectionReason {
    RequestedCpu,
    RequestedGpu,
    AutoPriority,
    AutoCpuFallback,
}

/// Power constraint applied while resolving the request.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PowerPolicyDecision {
    NotApplied,
    Unrestricted,
    BatteryEfficientGpuOnly,
    UnknownConservativeGpuOnly,
}

/// Typed reason an otherwise discovered target was not eligible.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BackendSkipReason {
    PlatformUnsupported,
    StructurallyInvalid,
    Unaddressable,
    Incompatible,
    Unhealthy,
    Quarantined,
    BatteryPolicy,
    UnknownPowerSource,
    NotAutoQualified,
    FallbackBound,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct SkippedBackend {
    pub(crate) target: BackendTarget,
    pub(crate) reason: BackendSkipReason,
}

/// Stable failure category recorded after a future bounded fallback.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BackendFailureCategory {
    BackendUnavailable,
    InitializationFailed,
    OutOfMemory,
    DeviceLost,
    WorkerFailed,
}

/// One failed target from a bounded fallback chain.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct BackendFallback {
    pub(crate) target: BackendTarget,
    pub(crate) category: BackendFailureCategory,
}

/// Deterministic backend resolution for one model load.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct BackendSelection {
    pub(crate) requested: AccelerationPreference,
    pub(crate) target: BackendTarget,
    pub(crate) reason: BackendSelectionReason,
    pub(crate) power_source: PowerSource,
    pub(crate) power_policy: PowerPolicyDecision,
    pub(crate) qualification_policy_version: u16,
    /// Remaining eligible targets in deterministic retry order. Worker
    /// supervision consumes this bounded list without silently widening
    /// strict-GPU semantics.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) fallback_targets: Vec<BackendTarget>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) fallback_history: Vec<BackendFallback>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) skipped_targets: Vec<SkippedBackend>,
}

impl BackendSelection {
    pub(crate) fn auto_cpu_diagnostic(&self) -> Option<String> {
        if self.requested != AccelerationPreference::Auto
            || self.reason != BackendSelectionReason::AutoCpuFallback
        {
            return None;
        }
        let skipped = self
            .skipped_targets
            .iter()
            .filter(|skipped| skipped.target.backend.is_gpu())
            .map(|skipped| skipped.reason)
            .collect::<Vec<_>>();
        if skipped.contains(&BackendSkipReason::BatteryPolicy) {
            Some(
                "Auto selected CPU because discrete or unclassified GPUs are disabled on battery."
                    .to_owned(),
            )
        } else if skipped.contains(&BackendSkipReason::UnknownPowerSource) {
            Some(
                "Auto selected CPU because the power source could not be verified, so discrete or unclassified GPUs are disabled."
                    .to_owned(),
            )
        } else if skipped.contains(&BackendSkipReason::NotAutoQualified) {
            Some(
                "Auto selected CPU because available GPU backends are not qualified for automatic use."
                    .to_owned(),
            )
        } else if skipped.contains(&BackendSkipReason::Unaddressable) {
            Some(
                "Auto selected CPU because the available GPU could not be addressed stably."
                    .to_owned(),
            )
        } else if skipped.iter().any(|reason| {
            matches!(
                reason,
                BackendSkipReason::Incompatible
                    | BackendSkipReason::Unhealthy
                    | BackendSkipReason::Quarantined
            )
        }) {
            Some(
                "Auto selected CPU because compatible GPU backends are currently unavailable."
                    .to_owned(),
            )
        } else {
            Some("No compatible GPU was available; Auto selected CPU.".to_owned())
        }
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub(crate) enum BackendSelectionError {
    #[error("no compatible CPU backend is available")]
    CpuNotFound,
    #[error("no compatible GPU backend is available for the current system")]
    NoGpuTarget,
    #[error("no compatible backend is available for Auto; the required CPU fallback is missing")]
    AutoMissingCpuFallback,
}

/// Resolves a preference without probing hardware or mutating runtime state.
pub(crate) fn select_backend(
    requested: AccelerationPreference,
    snapshot: &BackendSnapshot,
) -> Result<BackendSelection, BackendSelectionError> {
    let normalized = normalize_candidates(&snapshot.candidates);
    let power_policy = match requested {
        AccelerationPreference::Auto if snapshot.power_source == PowerSource::Battery => {
            PowerPolicyDecision::BatteryEfficientGpuOnly
        }
        AccelerationPreference::Auto if snapshot.power_source == PowerSource::Unknown => {
            PowerPolicyDecision::UnknownConservativeGpuOnly
        }
        AccelerationPreference::Auto => PowerPolicyDecision::Unrestricted,
        AccelerationPreference::Cpu | AccelerationPreference::Gpu => {
            PowerPolicyDecision::NotApplied
        }
    };

    let mut eligible = Vec::new();
    let mut skipped_targets = Vec::new();
    for candidate in normalized {
        if !candidate_matches_preference(&candidate, requested) {
            continue;
        }
        if let Some(reason) = candidate_skip_reason(&candidate, requested, snapshot, power_policy) {
            skipped_targets.push(SkippedBackend {
                target: candidate.target,
                reason,
            });
        } else {
            eligible.push(candidate.target);
        }
    }
    eligible.sort_by(|left, right| target_order(left, right, requested, snapshot.operating_system));
    skipped_targets.sort_by(|left, right| {
        target_order(
            &left.target,
            &right.target,
            requested,
            snapshot.operating_system,
        )
    });

    if requested == AccelerationPreference::Auto
        && !eligible
            .iter()
            .any(|target| target.backend == BackendKind::Cpu)
    {
        return Err(BackendSelectionError::AutoMissingCpuFallback);
    }

    let Some(target) = eligible.first().cloned() else {
        return Err(match requested {
            AccelerationPreference::Cpu => BackendSelectionError::CpuNotFound,
            AccelerationPreference::Gpu => BackendSelectionError::NoGpuTarget,
            AccelerationPreference::Auto => BackendSelectionError::AutoMissingCpuFallback,
        });
    };
    let reason = match requested {
        AccelerationPreference::Cpu => BackendSelectionReason::RequestedCpu,
        AccelerationPreference::Gpu => BackendSelectionReason::RequestedGpu,
        AccelerationPreference::Auto if target.backend == BackendKind::Cpu => {
            BackendSelectionReason::AutoCpuFallback
        }
        AccelerationPreference::Auto => BackendSelectionReason::AutoPriority,
    };

    Ok(BackendSelection {
        requested,
        target,
        reason,
        power_source: snapshot.power_source,
        power_policy,
        qualification_policy_version: snapshot.qualification_policy.version,
        fallback_targets: eligible.into_iter().skip(1).collect(),
        fallback_history: Vec::new(),
        skipped_targets,
    })
}

fn normalize_candidates(candidates: &[BackendCandidate]) -> Vec<BackendCandidate> {
    let mut normalized = candidates.to_vec();
    normalized.sort_by(|left, right| {
        left.target
            .dedup_key()
            .cmp(&right.target.dedup_key())
            .then_with(|| left.availability.rank().cmp(&right.availability.rank()))
            .then_with(|| {
                left.target
                    .process_index
                    .unwrap_or(usize::MAX)
                    .cmp(&right.target.process_index.unwrap_or(usize::MAX))
            })
            .then_with(|| {
                right
                    .target
                    .memory_available_bytes
                    .cmp(&left.target.memory_available_bytes)
            })
    });
    normalized.dedup_by(|right, left| right.target.dedup_key() == left.target.dedup_key());
    normalized
}

fn candidate_matches_preference(
    candidate: &BackendCandidate,
    requested: AccelerationPreference,
) -> bool {
    match requested {
        AccelerationPreference::Cpu => candidate.target.backend == BackendKind::Cpu,
        AccelerationPreference::Gpu => candidate.target.backend.is_gpu(),
        AccelerationPreference::Auto => true,
    }
}

fn candidate_skip_reason(
    candidate: &BackendCandidate,
    requested: AccelerationPreference,
    snapshot: &BackendSnapshot,
    power_policy: PowerPolicyDecision,
) -> Option<BackendSkipReason> {
    if !candidate.target.is_structurally_valid() {
        return Some(BackendSkipReason::StructurallyInvalid);
    }
    if !platform_supports(&candidate.target, snapshot.operating_system) {
        return Some(BackendSkipReason::PlatformUnsupported);
    }
    let availability = match candidate.availability {
        CandidateAvailability::Available => None,
        CandidateAvailability::Unaddressable => Some(BackendSkipReason::Unaddressable),
        CandidateAvailability::Incompatible => Some(BackendSkipReason::Incompatible),
        CandidateAvailability::Unhealthy => Some(BackendSkipReason::Unhealthy),
        CandidateAvailability::Quarantined => Some(BackendSkipReason::Quarantined),
    };
    if availability.is_some() {
        return availability;
    }
    if requested == AccelerationPreference::Auto && candidate.target.backend.is_gpu() {
        if !candidate.target.device_class.is_battery_eligible_gpu()
            && matches!(
                power_policy,
                PowerPolicyDecision::BatteryEfficientGpuOnly
                    | PowerPolicyDecision::UnknownConservativeGpuOnly
            )
        {
            return Some(if snapshot.power_source == PowerSource::Battery {
                BackendSkipReason::BatteryPolicy
            } else {
                BackendSkipReason::UnknownPowerSource
            });
        }
        if !snapshot
            .qualification_policy
            .qualifies(snapshot.operating_system, &candidate.target)
        {
            return Some(BackendSkipReason::NotAutoQualified);
        }
    }
    None
}

fn platform_supports(target: &BackendTarget, operating_system: OperatingSystem) -> bool {
    match operating_system {
        OperatingSystem::Windows | OperatingSystem::Linux => match target.backend {
            BackendKind::Cpu | BackendKind::Vulkan => true,
            BackendKind::Cuda => target.vendor == GpuVendor::Nvidia,
            BackendKind::Metal => false,
        },
        OperatingSystem::MacOs => match target.backend {
            BackendKind::Cpu => true,
            BackendKind::Metal => matches!(
                target.vendor,
                GpuVendor::Apple | GpuVendor::Amd | GpuVendor::Intel
            ),
            BackendKind::Cuda | BackendKind::Vulkan => false,
        },
        OperatingSystem::Other => target.backend == BackendKind::Cpu,
    }
}

fn target_order(
    left: &BackendTarget,
    right: &BackendTarget,
    requested: AccelerationPreference,
    operating_system: OperatingSystem,
) -> Ordering {
    backend_rank(left.backend, requested, operating_system)
        .cmp(&backend_rank(right.backend, requested, operating_system))
        .then_with(|| {
            device_class_rank(left.device_class).cmp(&device_class_rank(right.device_class))
        })
        .then_with(|| vendor_rank(left.vendor).cmp(&vendor_rank(right.vendor)))
        .then_with(|| {
            left.device_id
                .canonical_key()
                .cmp(&right.device_id.canonical_key())
        })
        .then_with(|| {
            left.process_index
                .unwrap_or(usize::MAX)
                .cmp(&right.process_index.unwrap_or(usize::MAX))
        })
}

fn backend_rank(
    backend: BackendKind,
    requested: AccelerationPreference,
    operating_system: OperatingSystem,
) -> u8 {
    if requested == AccelerationPreference::Cpu {
        return u8::from(backend != BackendKind::Cpu);
    }
    match operating_system {
        OperatingSystem::Windows | OperatingSystem::Linux => match backend {
            BackendKind::Cuda => 0,
            BackendKind::Vulkan => 1,
            BackendKind::Metal => 2,
            BackendKind::Cpu => 3,
        },
        OperatingSystem::MacOs => match backend {
            BackendKind::Metal => 0,
            BackendKind::Cuda => 1,
            BackendKind::Vulkan => 2,
            BackendKind::Cpu => 3,
        },
        OperatingSystem::Other => match backend {
            BackendKind::Cpu => 0,
            BackendKind::Cuda | BackendKind::Vulkan | BackendKind::Metal => 1,
        },
    }
}

fn device_class_rank(class: DeviceClass) -> u8 {
    match class {
        DeviceClass::DiscreteGpu => 0,
        DeviceClass::UnifiedGpu => 1,
        DeviceClass::IntegratedGpu => 2,
        DeviceClass::Unknown => 3,
        DeviceClass::Accelerator => 4,
        DeviceClass::Cpu => 5,
    }
}

fn vendor_rank(vendor: GpuVendor) -> u8 {
    match vendor {
        GpuVendor::Nvidia => 0,
        GpuVendor::Amd => 1,
        GpuVendor::Intel => 2,
        GpuVendor::Apple => 3,
        GpuVendor::Other => 4,
        GpuVendor::Unknown => 5,
    }
}

#[cfg(windows)]
fn current_power_source() -> PowerSource {
    use std::mem::MaybeUninit;
    use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};

    let mut status = MaybeUninit::<SYSTEM_POWER_STATUS>::uninit();
    // SAFETY: `status` is valid writable storage for the API's output. A
    // nonzero return guarantees the structure was initialized, and failure is
    // handled before `assume_init`.
    if unsafe { GetSystemPowerStatus(status.as_mut_ptr()) } == 0 {
        return PowerSource::Unknown;
    }
    // SAFETY: the successful API call above initialized the output structure.
    let status = unsafe { status.assume_init() };
    match status.ACLineStatus {
        0 => PowerSource::Battery,
        1 => PowerSource::Ac,
        _ => PowerSource::Unknown,
    }
}

#[cfg(target_os = "macos")]
fn current_power_source() -> PowerSource {
    crate::macos_power::power_source()
}

#[cfg(target_os = "linux")]
fn current_power_source() -> PowerSource {
    crate::linux_power::power_source()
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn current_power_source() -> PowerSource {
    PowerSource::Unknown
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcription::{ComputeDevice, ResolvedAcceleration};

    fn target(
        backend: BackendKind,
        vendor: GpuVendor,
        class: DeviceClass,
        id: &str,
        index: usize,
    ) -> BackendTarget {
        BackendTarget {
            backend,
            provider_id: ProviderIdentity::new(format!("test:{}", backend.label())),
            driver_version: Some("test-driver-1".to_owned()),
            device_id: DeviceIdentity::new(id),
            display_name: id.to_owned(),
            vendor,
            device_class: class,
            memory_total_bytes: 8 * 1024 * 1024 * 1024,
            memory_available_bytes: 6 * 1024 * 1024 * 1024,
            pack: None,
            process_index: Some(index),
        }
    }

    fn candidate(target: BackendTarget) -> BackendCandidate {
        BackendCandidate::available(target)
    }

    fn cpu() -> BackendCandidate {
        candidate(BackendTarget::cpu())
    }

    fn snapshot(
        operating_system: OperatingSystem,
        power_source: PowerSource,
        candidates: Vec<BackendCandidate>,
    ) -> BackendSnapshot {
        let qualification_policy =
            BackendQualificationPolicy::qualify_all_for_testing(operating_system, &candidates);
        BackendSnapshot {
            operating_system,
            power_source,
            candidates,
            qualification_policy,
        }
    }

    fn default_deny_snapshot(
        operating_system: OperatingSystem,
        power_source: PowerSource,
        candidates: Vec<BackendCandidate>,
    ) -> BackendSnapshot {
        BackendSnapshot {
            operating_system,
            power_source,
            candidates,
            qualification_policy: BackendQualificationPolicy::stage_one_default_deny(),
        }
    }

    #[test]
    fn fixed_platform_vendor_priority_matrix_is_deterministic() {
        let cases = [
            (
                OperatingSystem::Windows,
                GpuVendor::Nvidia,
                vec![BackendKind::Vulkan, BackendKind::Cuda],
                BackendKind::Cuda,
            ),
            (
                OperatingSystem::Windows,
                GpuVendor::Amd,
                vec![BackendKind::Cuda, BackendKind::Vulkan],
                BackendKind::Vulkan,
            ),
            (
                OperatingSystem::Windows,
                GpuVendor::Intel,
                vec![BackendKind::Vulkan],
                BackendKind::Vulkan,
            ),
            (
                OperatingSystem::MacOs,
                GpuVendor::Apple,
                vec![BackendKind::Vulkan, BackendKind::Metal],
                BackendKind::Metal,
            ),
            (
                OperatingSystem::Linux,
                GpuVendor::Nvidia,
                vec![BackendKind::Vulkan, BackendKind::Cuda],
                BackendKind::Cuda,
            ),
            (
                OperatingSystem::Linux,
                GpuVendor::Amd,
                vec![BackendKind::Vulkan],
                BackendKind::Vulkan,
            ),
            (
                OperatingSystem::Linux,
                GpuVendor::Intel,
                vec![BackendKind::Vulkan],
                BackendKind::Vulkan,
            ),
        ];

        for (os, vendor, backends, expected) in cases {
            let gpu_class = if vendor == GpuVendor::Apple {
                DeviceClass::UnifiedGpu
            } else {
                DeviceClass::DiscreteGpu
            };
            let mut candidates = backends
                .into_iter()
                .enumerate()
                .map(|(index, backend)| {
                    candidate(target(backend, vendor, gpu_class, backend.label(), index))
                })
                .collect::<Vec<_>>();
            candidates.push(cpu());
            let selected = select_backend(
                AccelerationPreference::Auto,
                &snapshot(os, PowerSource::Ac, candidates),
            )
            .unwrap();

            assert_eq!(selected.target.backend, expected, "{os:?} {vendor:?}");
            assert_eq!(selected.reason, BackendSelectionReason::AutoPriority);
        }
    }

    #[test]
    fn stage_one_default_deny_keeps_all_production_auto_paths_on_cpu() {
        for (os, backend, vendor, class) in [
            (
                OperatingSystem::Windows,
                BackendKind::Cuda,
                GpuVendor::Nvidia,
                DeviceClass::DiscreteGpu,
            ),
            (
                OperatingSystem::MacOs,
                BackendKind::Metal,
                GpuVendor::Apple,
                DeviceClass::UnifiedGpu,
            ),
            (
                OperatingSystem::Linux,
                BackendKind::Vulkan,
                GpuVendor::Amd,
                DeviceClass::DiscreteGpu,
            ),
        ] {
            let candidates = vec![
                candidate(target(backend, vendor, class, "production-gpu", 1)),
                cpu(),
            ];
            let production = default_deny_snapshot(os, PowerSource::Ac, candidates.clone());

            let automatic = select_backend(AccelerationPreference::Auto, &production).unwrap();
            assert_eq!(automatic.target.backend, BackendKind::Cpu, "{os:?}");
            assert_eq!(
                automatic.auto_cpu_diagnostic().as_deref(),
                Some(
                    "Auto selected CPU because available GPU backends are not qualified for automatic use."
                )
            );
            assert_eq!(
                automatic.skipped_targets[0].reason,
                BackendSkipReason::NotAutoQualified
            );

            let explicit = select_backend(AccelerationPreference::Gpu, &production).unwrap();
            assert_eq!(explicit.target.backend, backend, "{os:?}");
        }
    }

    #[test]
    fn auto_fallback_order_is_cuda_then_vulkan_then_cpu() {
        let selected = select_backend(
            AccelerationPreference::Auto,
            &snapshot(
                OperatingSystem::Windows,
                PowerSource::Ac,
                vec![
                    cpu(),
                    candidate(target(
                        BackendKind::Vulkan,
                        GpuVendor::Nvidia,
                        DeviceClass::DiscreteGpu,
                        "gpu-vulkan",
                        2,
                    )),
                    candidate(target(
                        BackendKind::Cuda,
                        GpuVendor::Nvidia,
                        DeviceClass::DiscreteGpu,
                        "gpu-cuda",
                        1,
                    )),
                ],
            ),
        )
        .unwrap();

        let complete_order = std::iter::once(&selected.target)
            .chain(&selected.fallback_targets)
            .map(|target| target.backend)
            .collect::<Vec<_>>();
        assert_eq!(
            complete_order,
            [BackendKind::Cuda, BackendKind::Vulkan, BackendKind::Cpu]
        );
    }

    #[test]
    fn auto_exposes_the_complete_stable_multi_device_chain() {
        let selected = select_backend(
            AccelerationPreference::Auto,
            &snapshot(
                OperatingSystem::Linux,
                PowerSource::Ac,
                vec![
                    candidate(target(
                        BackendKind::Vulkan,
                        GpuVendor::Nvidia,
                        DeviceClass::DiscreteGpu,
                        "vulkan-b",
                        8,
                    )),
                    candidate(target(
                        BackendKind::Cuda,
                        GpuVendor::Nvidia,
                        DeviceClass::DiscreteGpu,
                        "cuda-b",
                        3,
                    )),
                    cpu(),
                    candidate(target(
                        BackendKind::Vulkan,
                        GpuVendor::Nvidia,
                        DeviceClass::DiscreteGpu,
                        "vulkan-a",
                        7,
                    )),
                    candidate(target(
                        BackendKind::Cuda,
                        GpuVendor::Nvidia,
                        DeviceClass::DiscreteGpu,
                        "cuda-a",
                        2,
                    )),
                ],
            ),
        )
        .unwrap();

        assert_eq!(selected.requested, AccelerationPreference::Auto);
        assert_eq!(selected.reason, BackendSelectionReason::AutoPriority);
        let complete_order = std::iter::once(&selected.target)
            .chain(&selected.fallback_targets)
            .map(|target| (target.backend, target.device_id.as_str()))
            .collect::<Vec<_>>();
        assert_eq!(
            complete_order,
            [
                (BackendKind::Cuda, "cuda-a"),
                (BackendKind::Cuda, "cuda-b"),
                (BackendKind::Vulkan, "vulkan-a"),
                (BackendKind::Vulkan, "vulkan-b"),
                (BackendKind::Cpu, "cpu:system"),
            ]
        );
    }

    #[test]
    fn auto_on_battery_excludes_discrete_and_unknown_gpus() {
        let candidates = vec![
            candidate(target(
                BackendKind::Cuda,
                GpuVendor::Nvidia,
                DeviceClass::DiscreteGpu,
                "nvidia-discrete",
                1,
            )),
            candidate(target(
                BackendKind::Vulkan,
                GpuVendor::Other,
                DeviceClass::Unknown,
                "unknown-gpu",
                2,
            )),
            cpu(),
        ];
        let selected = select_backend(
            AccelerationPreference::Auto,
            &snapshot(OperatingSystem::Windows, PowerSource::Battery, candidates),
        )
        .unwrap();

        assert_eq!(selected.target.backend, BackendKind::Cpu);
        assert_eq!(selected.reason, BackendSelectionReason::AutoCpuFallback);
        assert_eq!(
            selected.auto_cpu_diagnostic().as_deref(),
            Some(
                "Auto selected CPU because discrete or unclassified GPUs are disabled on battery."
            )
        );
        assert_eq!(
            selected.power_policy,
            PowerPolicyDecision::BatteryEfficientGpuOnly
        );
    }

    #[test]
    fn auto_on_battery_keeps_integrated_and_unified_gpus_eligible() {
        for class in [DeviceClass::IntegratedGpu, DeviceClass::UnifiedGpu] {
            let vendor = if class == DeviceClass::UnifiedGpu {
                GpuVendor::Apple
            } else {
                GpuVendor::Intel
            };
            let (os, backend) = if vendor == GpuVendor::Apple {
                (OperatingSystem::MacOs, BackendKind::Metal)
            } else {
                (OperatingSystem::Windows, BackendKind::Vulkan)
            };
            let selected = select_backend(
                AccelerationPreference::Auto,
                &snapshot(
                    os,
                    PowerSource::Battery,
                    vec![
                        candidate(target(backend, vendor, class, "efficient-gpu", 1)),
                        cpu(),
                    ],
                ),
            )
            .unwrap();

            assert_eq!(selected.target.backend, backend);
        }
    }

    #[test]
    fn macos_metal_accepts_apple_intel_and_amd_but_excludes_discrete_on_battery() {
        for (vendor, class, power, expected) in [
            (
                GpuVendor::Apple,
                DeviceClass::UnifiedGpu,
                PowerSource::Battery,
                BackendKind::Metal,
            ),
            (
                GpuVendor::Intel,
                DeviceClass::IntegratedGpu,
                PowerSource::Battery,
                BackendKind::Metal,
            ),
            (
                GpuVendor::Amd,
                DeviceClass::DiscreteGpu,
                PowerSource::Battery,
                BackendKind::Cpu,
            ),
            (
                GpuVendor::Amd,
                DeviceClass::DiscreteGpu,
                PowerSource::Ac,
                BackendKind::Metal,
            ),
        ] {
            let selected = select_backend(
                AccelerationPreference::Auto,
                &snapshot(
                    OperatingSystem::MacOs,
                    power,
                    vec![
                        candidate(target(BackendKind::Metal, vendor, class, "metal", 1)),
                        cpu(),
                    ],
                ),
            )
            .unwrap();
            assert_eq!(selected.target.backend, expected, "{vendor:?} {power:?}");
        }
    }

    #[test]
    fn explicit_gpu_ignores_power_and_auto_qualification_but_never_uses_cpu() {
        let opt_in_only = candidate(target(
            BackendKind::Vulkan,
            GpuVendor::Amd,
            DeviceClass::DiscreteGpu,
            "amd-gpu",
            1,
        ));
        let selected = select_backend(
            AccelerationPreference::Gpu,
            &default_deny_snapshot(
                OperatingSystem::Windows,
                PowerSource::Battery,
                vec![cpu(), opt_in_only],
            ),
        )
        .unwrap();

        assert_eq!(selected.target.backend, BackendKind::Vulkan);
        assert_eq!(selected.reason, BackendSelectionReason::RequestedGpu);
        assert_eq!(selected.power_policy, PowerPolicyDecision::NotApplied);

        let error = select_backend(
            AccelerationPreference::Gpu,
            &snapshot(OperatingSystem::Windows, PowerSource::Ac, vec![cpu()]),
        )
        .unwrap_err();
        assert_eq!(error, BackendSelectionError::NoGpuTarget);
    }

    #[test]
    fn auto_unknown_power_denies_discrete_gpu_but_keeps_integrated_and_unified_gpu_eligible() {
        for operating_system in [
            OperatingSystem::Windows,
            OperatingSystem::MacOs,
            OperatingSystem::Linux,
        ] {
            let backend = if operating_system == OperatingSystem::MacOs {
                BackendKind::Metal
            } else {
                BackendKind::Vulkan
            };
            let vendor = if operating_system == OperatingSystem::MacOs {
                GpuVendor::Apple
            } else {
                GpuVendor::Intel
            };
            let selected = select_backend(
                AccelerationPreference::Auto,
                &snapshot(
                    operating_system,
                    PowerSource::Unknown,
                    vec![
                        cpu(),
                        candidate(target(
                            backend,
                            vendor,
                            DeviceClass::DiscreteGpu,
                            "discrete",
                            0,
                        )),
                        candidate(target(
                            backend,
                            vendor,
                            DeviceClass::IntegratedGpu,
                            "integrated",
                            1,
                        )),
                        candidate(target(
                            backend,
                            vendor,
                            DeviceClass::UnifiedGpu,
                            "unified",
                            2,
                        )),
                    ],
                ),
            )
            .unwrap();

            assert_ne!(selected.target.device_class, DeviceClass::DiscreteGpu);
            assert_eq!(
                selected.power_policy,
                PowerPolicyDecision::UnknownConservativeGpuOnly
            );
            assert!(selected.skipped_targets.iter().any(|skipped| {
                skipped.target.device_class == DeviceClass::DiscreteGpu
                    && skipped.reason == BackendSkipReason::UnknownPowerSource
            }));
        }
    }

    #[test]
    fn auto_unknown_power_uses_cpu_fallback_when_only_discrete_gpu_is_available() {
        let selected = select_backend(
            AccelerationPreference::Auto,
            &snapshot(
                OperatingSystem::Linux,
                PowerSource::Unknown,
                vec![
                    cpu(),
                    candidate(target(
                        BackendKind::Vulkan,
                        GpuVendor::Amd,
                        DeviceClass::DiscreteGpu,
                        "discrete",
                        0,
                    )),
                ],
            ),
        )
        .unwrap();

        assert_eq!(selected.target.backend, BackendKind::Cpu);
        assert_eq!(selected.reason, BackendSelectionReason::AutoCpuFallback);
        assert_eq!(
            selected.auto_cpu_diagnostic().as_deref(),
            Some(
                "Auto selected CPU because the power source could not be verified, so discrete or unclassified GPUs are disabled."
            )
        );
    }

    #[test]
    fn explicit_gpu_allows_unknown_available_memory_without_cpu_fallback() {
        let mut gpu = target(
            BackendKind::Vulkan,
            GpuVendor::Amd,
            DeviceClass::DiscreteGpu,
            "amd-gpu",
            1,
        );
        gpu.memory_available_bytes = 0;
        let selected = select_backend(
            AccelerationPreference::Gpu,
            &default_deny_snapshot(
                OperatingSystem::Linux,
                PowerSource::Unknown,
                vec![cpu(), candidate(gpu)],
            ),
        )
        .unwrap();

        assert!(selected.target.backend.is_gpu());
        assert!(selected.fallback_targets.is_empty());
    }

    #[test]
    fn explicit_cpu_never_selects_a_gpu() {
        let selected = select_backend(
            AccelerationPreference::Cpu,
            &snapshot(
                OperatingSystem::Windows,
                PowerSource::Ac,
                vec![
                    candidate(target(
                        BackendKind::Cuda,
                        GpuVendor::Nvidia,
                        DeviceClass::DiscreteGpu,
                        "gpu",
                        1,
                    )),
                    cpu(),
                ],
            ),
        )
        .unwrap();

        assert_eq!(selected.target.backend, BackendKind::Cpu);
        assert_eq!(selected.reason, BackendSelectionReason::RequestedCpu);
        assert!(selected.fallback_targets.is_empty());
    }

    #[test]
    fn auto_requires_its_guaranteed_cpu_fallback() {
        let only_gpu = candidate(target(
            BackendKind::Vulkan,
            GpuVendor::Amd,
            DeviceClass::DiscreteGpu,
            "gpu",
            1,
        ));

        let error = select_backend(
            AccelerationPreference::Auto,
            &snapshot(OperatingSystem::Windows, PowerSource::Ac, vec![only_gpu]),
        )
        .unwrap_err();

        assert_eq!(error, BackendSelectionError::AutoMissingCpuFallback);
    }

    #[test]
    fn auto_uses_cpu_when_gpu_is_unhealthy_quarantined_or_incompatible() {
        for availability in [
            CandidateAvailability::Unhealthy,
            CandidateAvailability::Quarantined,
            CandidateAvailability::Incompatible,
        ] {
            let mut gpu = candidate(target(
                BackendKind::Vulkan,
                GpuVendor::Amd,
                DeviceClass::DiscreteGpu,
                "gpu",
                1,
            ));
            gpu.availability = availability;
            let selected = select_backend(
                AccelerationPreference::Auto,
                &snapshot(OperatingSystem::Windows, PowerSource::Ac, vec![gpu, cpu()]),
            )
            .unwrap();

            assert_eq!(selected.target.backend, BackendKind::Cpu);
            assert_eq!(selected.reason, BackendSelectionReason::AutoCpuFallback);
        }
    }

    #[test]
    fn ordering_and_dedup_do_not_depend_on_provider_enumeration_order() {
        let duplicate_unhealthy = BackendCandidate {
            target: target(
                BackendKind::Vulkan,
                GpuVendor::Nvidia,
                DeviceClass::DiscreteGpu,
                "GPU-B",
                7,
            ),
            availability: CandidateAvailability::Unhealthy,
        };
        let first = vec![
            candidate(target(
                BackendKind::Vulkan,
                GpuVendor::Nvidia,
                DeviceClass::DiscreteGpu,
                "gpu-b",
                4,
            )),
            candidate(target(
                BackendKind::Vulkan,
                GpuVendor::Nvidia,
                DeviceClass::DiscreteGpu,
                "gpu-a",
                9,
            )),
            duplicate_unhealthy.clone(),
            cpu(),
        ];
        let mut reversed = first.clone();
        reversed.reverse();

        let first_selection = select_backend(
            AccelerationPreference::Auto,
            &snapshot(OperatingSystem::Windows, PowerSource::Ac, first),
        )
        .unwrap();
        let reversed_selection = select_backend(
            AccelerationPreference::Auto,
            &snapshot(OperatingSystem::Windows, PowerSource::Ac, reversed),
        )
        .unwrap();

        assert_eq!(first_selection.target.device_id.as_str(), "gpu-a");
        assert_eq!(
            first_selection.target.device_id,
            reversed_selection.target.device_id
        );
        assert_eq!(first_selection.fallback_targets.len(), 2);
        assert_eq!(
            first_selection.fallback_targets[0].device_id.as_str(),
            "gpu-b"
        );
        assert_eq!(
            first_selection.fallback_targets[1].backend,
            BackendKind::Cpu
        );
    }

    #[test]
    fn process_index_is_never_serialized_or_restored() {
        let target = target(
            BackendKind::Cuda,
            GpuVendor::Nvidia,
            DeviceClass::DiscreteGpu,
            "pci:01:00.0",
            17,
        );

        let value = serde_json::to_value(&target).unwrap();
        assert!(value.get("process_index").is_none());
        let restored: BackendTarget = serde_json::from_value(value).unwrap();
        assert_eq!(restored.device_id, target.device_id);
        assert_eq!(restored.process_index, None);
    }

    #[test]
    fn legacy_resolved_acceleration_json_remains_compatible() {
        let legacy = r#"{
            "requested":"auto",
            "resolved":{"Gpu":{"name":"Test GPU"}},
            "diagnostic":null
        }"#;

        let resolved: ResolvedAcceleration = serde_json::from_str(legacy).unwrap();
        assert_eq!(
            resolved.resolved,
            ComputeDevice::Gpu {
                name: "Test GPU".to_owned()
            }
        );
        assert_eq!(resolved.selection, None);

        let cpu = ResolvedAcceleration {
            requested: AccelerationPreference::Cpu,
            resolved: ComputeDevice::Cpu,
            diagnostic: None,
            selection: None,
        };
        let serialized = serde_json::to_value(cpu).unwrap();
        assert!(serialized.get("selection").is_none());
    }

    #[test]
    fn unknown_operating_system_conservatively_uses_cpu() {
        let selected = select_backend(
            AccelerationPreference::Auto,
            &snapshot(
                OperatingSystem::Other,
                PowerSource::Unknown,
                vec![
                    candidate(target(
                        BackendKind::Vulkan,
                        GpuVendor::Other,
                        DeviceClass::IntegratedGpu,
                        "gpu",
                        1,
                    )),
                    cpu(),
                ],
            ),
        )
        .unwrap();

        assert_eq!(selected.target.backend, BackendKind::Cpu);
    }
}
