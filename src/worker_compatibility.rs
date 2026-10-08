//! Compile-time-only compatibility authority for immutable Windows inference workers.
//!
//! The current desktop build is the implicit consumer of this policy. Entries
//! identify an exact older worker origin and immutable executable (and, for a
//! GPU worker, its already signed pack identity). Runtime input can never add
//! an entry or construct an approval.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

pub(crate) const FROZEN_WORKER_COMPATIBILITY_VERSION: u8 = 1;
const MAX_POLICY_BYTES: usize = 256 * 1024;
const MAX_ENTRIES: usize = 64;
const COMPILED_WINDOWS_X64_POLICY: &[u8] =
    include_bytes!("../runtime-manifests/frozen-worker-compatibility-windows-x64.json");

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FrozenWorkerCompatibilityContext {
    pub(crate) version: u8,
    pub(crate) origin_app_build: String,
}

impl FrozenWorkerCompatibilityContext {
    pub(crate) fn validate_shape(&self) -> bool {
        self.version == FROZEN_WORKER_COMPATIBILITY_VERSION
            && is_valid_build_identity(&self.origin_app_build)
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FrozenGpuBackend {
    Cuda,
    Vulkan,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum FrozenWorkerApprovalKind {
    Cpu,
    Gpu {
        backend: FrozenGpuBackend,
        provider: String,
        pack_id: String,
        pack_version: String,
        pack_digest: String,
        security_epoch: u64,
    },
}

/// Opaque proof that the compile-time policy approved one exact foreign worker.
/// Its fields remain private so launch code cannot fabricate compatibility from
/// caller-supplied Hello or Ready metadata.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FrozenWorkerApproval {
    origin_app_build: String,
    worker_build: String,
    worker_sha256: String,
    protocol_version: u8,
    runtime_abi_version: u16,
    kind: FrozenWorkerApprovalKind,
}

impl FrozenWorkerApproval {
    pub(crate) fn worker_build(&self) -> &str {
        &self.worker_build
    }

    pub(crate) fn worker_sha256(&self) -> &str {
        &self.worker_sha256
    }

    pub(crate) fn protocol_version(&self) -> u8 {
        self.protocol_version
    }

    pub(crate) fn runtime_abi_version(&self) -> u16 {
        self.runtime_abi_version
    }

    pub(crate) fn context(&self) -> FrozenWorkerCompatibilityContext {
        FrozenWorkerCompatibilityContext {
            version: FROZEN_WORKER_COMPATIBILITY_VERSION,
            origin_app_build: self.origin_app_build.clone(),
        }
    }

    pub(crate) fn is_cpu(&self) -> bool {
        matches!(self.kind, FrozenWorkerApprovalKind::Cpu)
    }

    pub(crate) fn matches_gpu(&self, candidate: &FrozenGpuWorkerCandidate<'_>) -> bool {
        self.origin_app_build == candidate.origin_app_build
            && self.worker_build == candidate.worker_build
            && self.worker_sha256 == candidate.worker_sha256
            && self.protocol_version == candidate.protocol_version
            && self.runtime_abi_version == candidate.runtime_abi_version
            && matches!(
                &self.kind,
                FrozenWorkerApprovalKind::Gpu {
                    backend,
                    provider,
                    pack_id,
                    pack_version,
                    pack_digest,
                    security_epoch,
                } if *backend == candidate.backend
                    && provider == candidate.provider
                    && pack_id == candidate.pack_id
                    && pack_version == candidate.pack_version
                    && pack_digest == candidate.pack_digest
                    && *security_epoch == candidate.security_epoch
            )
    }
}

pub(crate) struct FrozenCpuWorkerCandidate<'a> {
    pub(crate) worker_sha256: &'a str,
    pub(crate) protocol_version: u8,
    pub(crate) runtime_abi_version: u16,
}

pub(crate) struct FrozenGpuWorkerCandidate<'a> {
    pub(crate) origin_app_build: &'a str,
    pub(crate) worker_build: &'a str,
    pub(crate) worker_sha256: &'a str,
    pub(crate) protocol_version: u8,
    pub(crate) runtime_abi_version: u16,
    pub(crate) backend: FrozenGpuBackend,
    pub(crate) provider: &'a str,
    pub(crate) pack_id: &'a str,
    pub(crate) pack_version: &'a str,
    pub(crate) pack_digest: &'a str,
    pub(crate) security_epoch: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FrozenWorkerPolicyDocument {
    schema_version: u8,
    target_os: String,
    target_arch: String,
    entries: Vec<FrozenWorkerPolicyEntry>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum FrozenWorkerPolicyEntry {
    Cpu {
        origin_app_build: String,
        worker_build: String,
        worker_sha256: String,
        protocol_version: u8,
        runtime_abi_version: u16,
    },
    Gpu {
        origin_app_build: String,
        worker_build: String,
        worker_sha256: String,
        protocol_version: u8,
        runtime_abi_version: u16,
        backend: FrozenGpuBackend,
        provider: String,
        pack_id: String,
        pack_version: String,
        pack_digest: String,
        security_epoch: u64,
    },
}

#[derive(Clone, Debug)]
struct FrozenWorkerPolicy {
    entries: Vec<FrozenWorkerPolicyEntry>,
}

impl FrozenWorkerPolicy {
    fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.is_empty() || bytes.len() > MAX_POLICY_BYTES {
            return None;
        }
        let document = serde_json::from_slice::<FrozenWorkerPolicyDocument>(bytes).ok()?;
        let canonical = serde_json::to_vec(&document).ok()?;
        let canonical_with_newline = canonical
            .iter()
            .copied()
            .chain(std::iter::once(b'\n'))
            .collect::<Vec<_>>();
        let canonical_with_crlf = canonical
            .iter()
            .copied()
            .chain([b'\r', b'\n'])
            .collect::<Vec<_>>();
        if bytes != canonical.as_slice()
            && bytes != canonical_with_newline.as_slice()
            && bytes != canonical_with_crlf.as_slice()
        {
            return None;
        }
        if document.schema_version != FROZEN_WORKER_COMPATIBILITY_VERSION
            || document.target_os != "windows"
            || document.target_arch != "x86_64"
            || document.entries.len() > MAX_ENTRIES
        {
            return None;
        }
        let mut unique = BTreeSet::new();
        for entry in &document.entries {
            let key = match entry {
                FrozenWorkerPolicyEntry::Cpu {
                    origin_app_build,
                    worker_build,
                    worker_sha256,
                    protocol_version,
                    runtime_abi_version,
                } => {
                    if !is_valid_build_identity(origin_app_build)
                        || !is_valid_build_identity(worker_build)
                        || !is_canonical_sha256(worker_sha256)
                        || *protocol_version == 0
                        || *runtime_abi_version == 0
                    {
                        return None;
                    }
                    format!("cpu:{worker_sha256}")
                }
                FrozenWorkerPolicyEntry::Gpu {
                    origin_app_build,
                    worker_build,
                    worker_sha256,
                    protocol_version,
                    runtime_abi_version,
                    backend,
                    provider,
                    pack_id,
                    pack_version,
                    pack_digest,
                    security_epoch,
                } => {
                    if !is_valid_build_identity(origin_app_build)
                        || !is_valid_build_identity(worker_build)
                        || !is_canonical_sha256(worker_sha256)
                        || !is_identifier(provider)
                        || !is_store_component(pack_id)
                        || !is_store_component(pack_version)
                        || !is_canonical_sha256(pack_digest)
                        || *protocol_version == 0
                        || *runtime_abi_version == 0
                        || *security_epoch == 0
                    {
                        return None;
                    }
                    format!(
                        "gpu:{backend:?}:{provider}:{pack_id}:{pack_version}:{pack_digest}:{security_epoch}:{worker_sha256}:{origin_app_build}:{worker_build}"
                    )
                }
            };
            if !unique.insert(key) {
                return None;
            }
        }
        Some(Self {
            entries: document.entries,
        })
    }

    fn approve_cpu(
        &self,
        candidate: &FrozenCpuWorkerCandidate<'_>,
    ) -> Option<FrozenWorkerApproval> {
        self.entries.iter().find_map(|entry| {
            let FrozenWorkerPolicyEntry::Cpu {
                origin_app_build,
                worker_build,
                worker_sha256,
                protocol_version,
                runtime_abi_version,
            } = entry
            else {
                return None;
            };
            (worker_sha256 == candidate.worker_sha256
                && *protocol_version == candidate.protocol_version
                && *runtime_abi_version == candidate.runtime_abi_version)
                .then(|| FrozenWorkerApproval {
                    origin_app_build: origin_app_build.clone(),
                    worker_build: worker_build.clone(),
                    worker_sha256: worker_sha256.clone(),
                    protocol_version: *protocol_version,
                    runtime_abi_version: *runtime_abi_version,
                    kind: FrozenWorkerApprovalKind::Cpu,
                })
        })
    }

    fn approve_gpu(
        &self,
        candidate: &FrozenGpuWorkerCandidate<'_>,
    ) -> Option<FrozenWorkerApproval> {
        self.entries.iter().find_map(|entry| {
            let FrozenWorkerPolicyEntry::Gpu {
                origin_app_build,
                worker_build,
                worker_sha256,
                protocol_version,
                runtime_abi_version,
                backend,
                provider,
                pack_id,
                pack_version,
                pack_digest,
                security_epoch,
            } = entry
            else {
                return None;
            };
            (origin_app_build == candidate.origin_app_build
                && worker_build == candidate.worker_build
                && worker_sha256 == candidate.worker_sha256
                && *protocol_version == candidate.protocol_version
                && *runtime_abi_version == candidate.runtime_abi_version
                && *backend == candidate.backend
                && provider == candidate.provider
                && pack_id == candidate.pack_id
                && pack_version == candidate.pack_version
                && pack_digest == candidate.pack_digest
                && *security_epoch == candidate.security_epoch)
                .then(|| FrozenWorkerApproval {
                    origin_app_build: origin_app_build.clone(),
                    worker_build: worker_build.clone(),
                    worker_sha256: worker_sha256.clone(),
                    protocol_version: *protocol_version,
                    runtime_abi_version: *runtime_abi_version,
                    kind: FrozenWorkerApprovalKind::Gpu {
                        backend: *backend,
                        provider: provider.clone(),
                        pack_id: pack_id.clone(),
                        pack_version: pack_version.clone(),
                        pack_digest: pack_digest.clone(),
                        security_epoch: *security_epoch,
                    },
                })
        })
    }
}

fn compiled_policy() -> Option<FrozenWorkerPolicy> {
    FrozenWorkerPolicy::parse(COMPILED_WINDOWS_X64_POLICY)
}

#[cfg(test)]
thread_local! {
    static TEST_POLICY: std::cell::RefCell<Option<FrozenWorkerPolicy>> = const {
        std::cell::RefCell::new(None)
    };
}

#[cfg(test)]
fn test_policy_approval<T>(approve: impl FnOnce(&FrozenWorkerPolicy) -> Option<T>) -> Option<T> {
    TEST_POLICY.with(|policy| policy.borrow().as_ref().and_then(approve))
}

pub(crate) fn approve_compiled_cpu_worker(
    candidate: &FrozenCpuWorkerCandidate<'_>,
) -> Option<FrozenWorkerApproval> {
    #[cfg(test)]
    if let Some(approval) = test_policy_approval(|policy| policy.approve_cpu(candidate)) {
        return Some(approval);
    }
    if !cfg!(all(windows, target_arch = "x86_64")) {
        return None;
    }
    compiled_policy()?.approve_cpu(candidate)
}

pub(crate) fn approve_compiled_gpu_worker(
    candidate: &FrozenGpuWorkerCandidate<'_>,
) -> Option<FrozenWorkerApproval> {
    #[cfg(test)]
    if let Some(approval) = test_policy_approval(|policy| policy.approve_gpu(candidate)) {
        return Some(approval);
    }
    if !cfg!(all(windows, target_arch = "x86_64")) {
        return None;
    }
    compiled_policy()?.approve_gpu(candidate)
}

pub(crate) fn is_valid_build_identity(value: &str) -> bool {
    value.len() >= 12
        && value.len() <= 192
        && value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
}

fn is_canonical_sha256(value: &str) -> bool {
    value.len() == 64
        && value == value.to_ascii_lowercase()
        && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn is_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
}

fn is_store_component(value: &str) -> bool {
    let bytes = value.as_bytes();
    !value.is_empty()
        && value.len() <= 96
        && value != "."
        && value != ".."
        && bytes
            .first()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && bytes
            .last()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && bytes.iter().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(*byte, b'.' | b'_' | b'-')
        })
}

#[cfg(test)]
pub(crate) fn approve_test_cpu_worker(
    policy: &[u8],
    candidate: &FrozenCpuWorkerCandidate<'_>,
) -> Option<FrozenWorkerApproval> {
    FrozenWorkerPolicy::parse(policy)?.approve_cpu(candidate)
}

#[cfg(test)]
pub(crate) fn approve_test_gpu_worker(
    policy: &[u8],
    candidate: &FrozenGpuWorkerCandidate<'_>,
) -> Option<FrozenWorkerApproval> {
    FrozenWorkerPolicy::parse(policy)?.approve_gpu(candidate)
}

#[cfg(test)]
pub(crate) fn with_test_frozen_worker_policy<T>(policy: &[u8], run: impl FnOnce() -> T) -> T {
    let parsed =
        FrozenWorkerPolicy::parse(policy).expect("test compatibility policy must be valid");
    TEST_POLICY.with(|slot| {
        assert!(
            slot.borrow().is_none(),
            "test compatibility policy is already set"
        );
        *slot.borrow_mut() = Some(parsed);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(run));
        *slot.borrow_mut() = None;
        match result {
            Ok(value) => value,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cpu_policy() -> Vec<u8> {
        serde_json::to_vec(&FrozenWorkerPolicyDocument {
            schema_version: 1,
            target_os: "windows".to_owned(),
            target_arch: "x86_64".to_owned(),
            entries: vec![FrozenWorkerPolicyEntry::Cpu {
                origin_app_build: "local-transcriber@0.1.0#origin".to_owned(),
                worker_build: "scribe-inference-worker@0.1.0#origin".to_owned(),
                worker_sha256: "11".repeat(32),
                protocol_version: 5,
                runtime_abi_version: 1,
            }],
        })
        .unwrap()
    }

    fn gpu_policy(backend: FrozenGpuBackend) -> Vec<u8> {
        serde_json::to_vec(&FrozenWorkerPolicyDocument {
            schema_version: 1,
            target_os: "windows".to_owned(),
            target_arch: "x86_64".to_owned(),
            entries: vec![FrozenWorkerPolicyEntry::Gpu {
                origin_app_build: "local-transcriber@0.1.0#origin".to_owned(),
                worker_build: "scribe-inference-worker@0.1.0#origin".to_owned(),
                worker_sha256: "22".repeat(32),
                protocol_version: 5,
                runtime_abi_version: 1,
                backend,
                provider: match backend {
                    FrozenGpuBackend::Cuda => "transcribe-cpp-ggml-cuda",
                    FrozenGpuBackend::Vulkan => "transcribe-cpp-ggml-vulkan",
                }
                .to_owned(),
                pack_id: "scribe-gpu-worker".to_owned(),
                pack_version: "1.0.0".to_owned(),
                pack_digest: "33".repeat(32),
                security_epoch: 7,
            }],
        })
        .unwrap()
    }

    fn gpu_candidate<'a>(
        backend: FrozenGpuBackend,
        provider: &'a str,
        origin_app_build: &'a str,
        worker_build: &'a str,
        worker_sha256: &'a str,
        pack_digest: &'a str,
    ) -> FrozenGpuWorkerCandidate<'a> {
        FrozenGpuWorkerCandidate {
            origin_app_build,
            worker_build,
            worker_sha256,
            protocol_version: 5,
            runtime_abi_version: 1,
            backend,
            provider,
            pack_id: "scribe-gpu-worker",
            pack_version: "1.0.0",
            pack_digest,
            security_epoch: 7,
        }
    }

    #[test]
    fn frozen_worker_policy_rejects_empty_malformed_oversized_and_wrong_platform() {
        assert!(FrozenWorkerPolicy::parse(b"").is_none());
        assert!(FrozenWorkerPolicy::parse(b"{}").is_none());
        assert!(FrozenWorkerPolicy::parse(&vec![b' '; MAX_POLICY_BYTES + 1]).is_none());
        let mut document: FrozenWorkerPolicyDocument =
            serde_json::from_slice(&cpu_policy()).unwrap();
        document.target_arch = "aarch64".to_owned();
        assert!(FrozenWorkerPolicy::parse(&serde_json::to_vec(&document).unwrap()).is_none());
    }

    #[test]
    fn frozen_worker_policy_rejects_duplicate_and_excess_entries() {
        let mut document: FrozenWorkerPolicyDocument =
            serde_json::from_slice(&cpu_policy()).unwrap();
        document.entries.push(document.entries[0].clone());
        assert!(FrozenWorkerPolicy::parse(&serde_json::to_vec(&document).unwrap()).is_none());
        document.entries = vec![document.entries[0].clone(); MAX_ENTRIES + 1];
        assert!(FrozenWorkerPolicy::parse(&serde_json::to_vec(&document).unwrap()).is_none());
    }

    #[test]
    fn frozen_worker_policy_approves_only_exact_cpu_identity() {
        let hash = "11".repeat(32);
        let candidate = FrozenCpuWorkerCandidate {
            worker_sha256: &hash,
            protocol_version: 5,
            runtime_abi_version: 1,
        };
        let approval = approve_test_cpu_worker(&cpu_policy(), &candidate).unwrap();
        assert!(approval.is_cpu());
        assert_eq!(approval.worker_sha256(), hash);
        let wrong_hash = "22".repeat(32);
        assert!(
            approve_test_cpu_worker(
                &cpu_policy(),
                &FrozenCpuWorkerCandidate {
                    worker_sha256: &wrong_hash,
                    ..candidate
                }
            )
            .is_none()
        );
    }

    #[test]
    fn compiled_empty_policy_denies_foreign_workers() {
        let hash = "11".repeat(32);
        assert!(compiled_policy().is_some());
        assert!(
            compiled_policy()
                .unwrap()
                .approve_cpu(&FrozenCpuWorkerCandidate {
                    worker_sha256: &hash,
                    protocol_version: 5,
                    runtime_abi_version: 1,
                })
                .is_none()
        );
    }

    #[test]
    fn frozen_worker_policy_approves_exact_cuda_and_vulkan_identities() {
        let worker_hash = "22".repeat(32);
        let pack_digest = "33".repeat(32);
        for (backend, provider) in [
            (FrozenGpuBackend::Cuda, "transcribe-cpp-ggml-cuda"),
            (FrozenGpuBackend::Vulkan, "transcribe-cpp-ggml-vulkan"),
        ] {
            let candidate = gpu_candidate(
                backend,
                provider,
                "local-transcriber@0.1.0#origin",
                "scribe-inference-worker@0.1.0#origin",
                &worker_hash,
                &pack_digest,
            );
            let approval = approve_test_gpu_worker(&gpu_policy(backend), &candidate).unwrap();
            assert!(approval.matches_gpu(&candidate));
            assert!(!approval.is_cpu());
        }
    }

    #[test]
    fn frozen_worker_policy_rejects_every_gpu_identity_substitution() {
        let worker_hash = "22".repeat(32);
        let wrong_worker_hash = "44".repeat(32);
        let pack_digest = "33".repeat(32);
        let wrong_pack_digest = "55".repeat(32);
        let policy = gpu_policy(FrozenGpuBackend::Cuda);
        let candidates = [
            gpu_candidate(
                FrozenGpuBackend::Cuda,
                "transcribe-cpp-ggml-cuda",
                "local-transcriber@0.1.0#wrong",
                "scribe-inference-worker@0.1.0#origin",
                &worker_hash,
                &pack_digest,
            ),
            gpu_candidate(
                FrozenGpuBackend::Cuda,
                "transcribe-cpp-ggml-cuda",
                "local-transcriber@0.1.0#origin",
                "scribe-inference-worker@0.1.0#wrong",
                &worker_hash,
                &pack_digest,
            ),
            gpu_candidate(
                FrozenGpuBackend::Cuda,
                "transcribe-cpp-ggml-cuda",
                "local-transcriber@0.1.0#origin",
                "scribe-inference-worker@0.1.0#origin",
                &wrong_worker_hash,
                &pack_digest,
            ),
            gpu_candidate(
                FrozenGpuBackend::Cuda,
                "wrong-provider",
                "local-transcriber@0.1.0#origin",
                "scribe-inference-worker@0.1.0#origin",
                &worker_hash,
                &pack_digest,
            ),
            gpu_candidate(
                FrozenGpuBackend::Cuda,
                "transcribe-cpp-ggml-cuda",
                "local-transcriber@0.1.0#origin",
                "scribe-inference-worker@0.1.0#origin",
                &worker_hash,
                &wrong_pack_digest,
            ),
        ];
        for candidate in candidates {
            assert!(approve_test_gpu_worker(&policy, &candidate).is_none());
        }
        let mut wrong_abi = gpu_candidate(
            FrozenGpuBackend::Cuda,
            "transcribe-cpp-ggml-cuda",
            "local-transcriber@0.1.0#origin",
            "scribe-inference-worker@0.1.0#origin",
            &worker_hash,
            &pack_digest,
        );
        wrong_abi.runtime_abi_version = 2;
        assert!(approve_test_gpu_worker(&policy, &wrong_abi).is_none());
        wrong_abi.runtime_abi_version = 1;
        wrong_abi.protocol_version = 4;
        assert!(approve_test_gpu_worker(&policy, &wrong_abi).is_none());
        wrong_abi.protocol_version = 5;
        wrong_abi.backend = FrozenGpuBackend::Vulkan;
        assert!(approve_test_gpu_worker(&policy, &wrong_abi).is_none());
        wrong_abi.backend = FrozenGpuBackend::Cuda;
        wrong_abi.pack_id = "wrong-pack";
        assert!(approve_test_gpu_worker(&policy, &wrong_abi).is_none());
        wrong_abi.pack_id = "scribe-gpu-worker";
        wrong_abi.pack_version = "2.0.0";
        assert!(approve_test_gpu_worker(&policy, &wrong_abi).is_none());
        wrong_abi.pack_version = "1.0.0";
        wrong_abi.security_epoch = 8;
        assert!(approve_test_gpu_worker(&policy, &wrong_abi).is_none());
    }
}
