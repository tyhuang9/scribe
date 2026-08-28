use std::{collections::HashSet, path::Path, sync::OnceLock};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::transcription::ModelId;

const HANDY_COMPUTER_TINY_EN_REVISION: &str = "becb8bcb804405dc97b380a523d9975888820986";
const COMPATIBILITY_EVIDENCE_DOCUMENT: &str = "docs/SCRIBE_REVAMP.md";
pub(crate) const BUNDLED_BASE_MODEL_ID: &str = "whisper_cpp_base_en";

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum RuntimeRequirement {
    PrimaryNative,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ModelArchitecture {
    EncoderDecoder,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct RuntimeVersion {
    pub(crate) major: u16,
    pub(crate) minor: u16,
    pub(crate) patch: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ArtifactManifest {
    repository: &'static str,
    revision: &'static str,
    filename: &'static str,
    size_bytes: u64,
    sha256: &'static str,
}

/// The trusted installation shape behind a normalized catalog entry.
///
/// GGUF models retain their individual pinned artifact manifests. ONNX models
/// are installed only through their verified multi-file receipt bundle; they
/// must never be represented as a made-up single-file artifact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ModelArtifactBinding {
    SingleGguf(ArtifactManifest),
    ReceiptBackedBundle {
        bundle_id: &'static str,
        aggregate_size_bytes: u64,
    },
}

impl ModelArtifactBinding {
    const fn aggregate_size_bytes(self) -> u64 {
        match self {
            Self::SingleGguf(artifact) => artifact.size_bytes,
            Self::ReceiptBackedBundle {
                aggregate_size_bytes,
                ..
            } => aggregate_size_bytes,
        }
    }

    const fn single_gguf(self) -> Option<ArtifactManifest> {
        match self {
            Self::SingleGguf(artifact) => Some(artifact),
            Self::ReceiptBackedBundle { .. } => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CompatibilityEvidence {
    id: &'static str,
    source: &'static str,
    load: bool,
    known_fixture: bool,
    cancellation: bool,
    unload_reload: bool,
    acceleration: bool,
    platform: bool,
    receipt: Option<CompatibilityReceipt>,
}

impl CompatibilityEvidence {
    const fn complete(self) -> bool {
        self.load
            && self.known_fixture
            && self.cancellation
            && self.unload_reload
            && self.acceleration
            && self.platform
            && self.receipt.is_some()
    }

    const fn link(self) -> EvidenceLink {
        EvidenceLink {
            id: self.id,
            source: self.source,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CompatibilityReceipt {
    json: &'static str,
    sha256: &'static str,
    runtime_package_manifest: &'static [u8],
    fixture_corpus_manifest: &'static [u8],
    results_manifest: &'static [u8],
}

#[derive(Debug, Deserialize)]
struct CompatibilityReceiptDocument {
    schema_version: u16,
    model_id: String,
    evidence_id: String,
    runtime_version: String,
    model_artifact_sha256: String,
    runtime_package_sha256: String,
    fixture_corpus_sha256: String,
    results_sha256: String,
    platform: String,
    load: bool,
    known_fixture: bool,
    cancellation: bool,
    unload_reload: bool,
    acceleration: bool,
    platform_tests: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ModelManifest {
    id: &'static str,
    display_name: &'static str,
    variant_label: &'static str,
    description: &'static str,
    storage_guidance: &'static str,
    expected_ram: &'static str,
    speed_guidance: &'static str,
    accuracy_guidance: &'static str,
    recommended: bool,
    runtime: Option<RuntimeRequirement>,
    architecture: ModelArchitecture,
    minimum_runtime_version: RuntimeVersion,
    artifact: ModelArtifactBinding,
    languages: &'static [&'static str],
    capabilities: ModelCapabilities,
    roles: &'static [ModelRole],
    compatibility: CompatibilityStatus,
    evidence: CompatibilityEvidence,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ModelGuidance {
    description: &'static str,
    storage: &'static str,
    expected_ram: &'static str,
    speed: &'static str,
    accuracy: &'static str,
}

/// A link to the local evidence used to assign a compatibility status.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EvidenceLink {
    pub id: &'static str,
    pub source: &'static str,
}

/// Runtime-neutral compatibility exposed to the service and UI.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompatibilityStatus {
    #[cfg(test)]
    Supported { evidence: EvidenceLink },
    Experimental {
        evidence: EvidenceLink,
        reason: &'static str,
    },
    #[cfg(test)]
    Incompatible {
        evidence: EvidenceLink,
        reason: &'static str,
    },
}

impl CompatibilityStatus {
    pub const fn label(self) -> &'static str {
        match self {
            #[cfg(test)]
            Self::Supported { .. } => "Supported",
            Self::Experimental { .. } => "Experimental",
            #[cfg(test)]
            Self::Incompatible { .. } => "Incompatible",
        }
    }
}

/// Curated user-facing roles. A role is valid only for a Supported model.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ModelRole {
    #[cfg(test)]
    FastEnglish,
    #[cfg(test)]
    BalancedMultilingual,
    #[cfg(test)]
    HighAccuracy,
    #[cfg(test)]
    LowMemory,
}

/// Capabilities that do not disclose a model family or concrete runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModelCapabilities {
    pub batch_transcription: bool,
    pub native_streaming: bool,
    pub cancellation: bool,
    pub timestamps: bool,
    pub translation: bool,
    pub language_detection: bool,
    pub confidence_scores: bool,
    pub custom_vocabulary: bool,
    pub cpu: bool,
    pub gpu: bool,
}

/// The runtime-neutral model information available above TranscriptionService.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelDescriptor {
    pub id: ModelId,
    pub display_name: &'static str,
    pub variant_label: &'static str,
    pub description: &'static str,
    pub expected_ram: &'static str,
    pub speed_guidance: &'static str,
    pub accuracy_guidance: &'static str,
    /// Curated recommendation from the normalized catalog, independent of installation or selection.
    pub recommended: bool,
    pub artifact_size_bytes: u64,
    pub languages: Vec<&'static str>,
    pub capabilities: ModelCapabilities,
    pub roles: Vec<ModelRole>,
    pub compatibility: CompatibilityStatus,
}

/// Exact runtime-facing model requirements. Keep this below TranscriptionService.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeModelManifest {
    pub(crate) id: &'static str,
    pub(crate) runtime: RuntimeRequirement,
    pub(crate) minimum_runtime_version: RuntimeVersion,
    pub(crate) artifact_repository: &'static str,
    pub(crate) artifact_revision: &'static str,
    pub(crate) artifact_filename: &'static str,
    pub(crate) artifact_size_bytes: u64,
    pub(crate) artifact_storage_estimate: &'static str,
    pub(crate) artifact_sha256: &'static str,
}

/// Trusted runtime artifact format, assigned by catalog/provenance resolution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ArtifactFormat {
    Gguf,
}

/// Immutable integrity facts for an artifact the runtime may resolve.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeArtifactManifest {
    pub(crate) repository: &'static str,
    pub(crate) revision: &'static str,
    pub(crate) filename: &'static str,
    pub(crate) size_bytes: u64,
    pub(crate) sha256: &'static str,
    pub(crate) format: ArtifactFormat,
}

/// Safe normalized installation binding for callers that need to select an
/// installer without assuming every model is a single GGUF download.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NormalizedInstallArtifact {
    SingleGguf(RuntimeArtifactManifest),
    ReceiptBackedBundle {
        bundle_id: &'static str,
        aggregate_size_bytes: u64,
    },
}

const TRANSCRIBE_CPP_VERSION: RuntimeVersion = RuntimeVersion {
    major: 1,
    minor: 9,
    patch: 1,
};

const BATCH_ENGLISH_CAPABILITIES: ModelCapabilities = ModelCapabilities {
    batch_transcription: true,
    native_streaming: false,
    cancellation: true,
    timestamps: true,
    translation: false,
    language_detection: false,
    confidence_scores: false,
    custom_vocabulary: false,
    cpu: true,
    gpu: false,
};

const MOONSHINE_TINY_CAPABILITIES: ModelCapabilities = ModelCapabilities {
    timestamps: false,
    ..BATCH_ENGLISH_CAPABILITIES
};

const MOONSHINE_BASE_CAPABILITIES: ModelCapabilities = ModelCapabilities {
    cancellation: false,
    timestamps: false,
    ..BATCH_ENGLISH_CAPABILITIES
};

const PARAKEET_TDT_V2_CAPABILITIES: ModelCapabilities = ModelCapabilities {
    cancellation: false,
    timestamps: false,
    cancellation: false,
    ..BATCH_ENGLISH_CAPABILITIES
};

const NO_ROLES: &[ModelRole] = &[];

const PHASE_ZERO_SMOKE: CompatibilityEvidence = CompatibilityEvidence {
    id: "phase-0-whisper-jfk-process-smoke",
    source: COMPATIBILITY_EVIDENCE_DOCUMENT,
    load: true,
    known_fixture: true,
    cancellation: false,
    unload_reload: false,
    acceleration: false,
    platform: false,
    receipt: None,
};

const MOONSHINE_TINY_ONNX_EXPERIMENTAL: CompatibilityEvidence = CompatibilityEvidence {
    id: "moonshine-tiny-en-int8-onnx-catalog-evidence",
    source: "resources/onnx-model-bundles-v1.json",
    load: false,
    known_fixture: false,
    cancellation: false,
    unload_reload: false,
    acceleration: false,
    platform: false,
    receipt: None,
};

const MOONSHINE_BASE_ONNX_EXPERIMENTAL: CompatibilityEvidence = CompatibilityEvidence {
    id: "moonshine-base-en-int8-onnx-windows-sherpa-1.13.5-fixture-gate",
    source: "docs/MANUAL_TEST_MATRIX.md",
    load: true,
    known_fixture: true,
    cancellation: false,
    unload_reload: true,
    acceleration: false,
    platform: true,
    receipt: None,
};

const PARAKEET_TDT_V2_ONNX_EXPERIMENTAL: CompatibilityEvidence = CompatibilityEvidence {
    id: "parakeet-tdt-06b-v2-en-int8-onnx-windows-sherpa-1.13.5-gate",
    source: "docs/MANUAL_TEST_MATRIX.md",
    load: true,
    known_fixture: true,
    cancellation: false,
    unload_reload: true,
    acceleration: false,
    platform: true,
    receipt: None,
};

const PHASE_TWO_BASE_SMOKE: CompatibilityEvidence = CompatibilityEvidence {
    id: "phase-2-native-base-en-jfk-smoke",
    source: COMPATIBILITY_EVIDENCE_DOCUMENT,
    load: true,
    known_fixture: true,
    cancellation: true,
    unload_reload: true,
    acceleration: true,
    platform: false,
    receipt: None,
};

const MODELS: &[ModelManifest] = &[
    handy_computer_tiny_en_manifest(),
    moonshine_tiny_en_int8_onnx_manifest(),
    moonshine_base_en_int8_onnx_manifest(),
    parakeet_tdt_06b_v2_en_int8_onnx_manifest(),
    whisper_manifest(
        BUNDLED_BASE_MODEL_ID,
        "Whisper Base — English",
        "base.en",
        ModelGuidance {
            description: "Local English model with a balanced speed and quality profile.",
            storage: "~81 MB",
            expected_ram: "1 GB",
            speed: "Fast",
            accuracy: "Good",
        },
        ArtifactManifest {
            repository: "handy-computer/whisper-base.en-gguf",
            revision: "cf0804db15fb341d00c9274b90da9cbb4fe2e5c6",
            filename: "whisper-base.en-Q8_0.gguf",
            size_bytes: 84_886_208,
            sha256: "3b46ca40bccbf7609c68d88a36d96077a04ca7c87f2060ede06f129fac3e7652",
        },
        true,
        PHASE_TWO_BASE_SMOKE,
    ),
    whisper_manifest(
        "whisper_cpp_small_en",
        "Whisper Small — English",
        "small.en",
        ModelGuidance {
            description: "More accurate local English model for longer dictation and clean audio.",
            storage: "~257 MB",
            expected_ram: "2 GB",
            speed: "Medium",
            accuracy: "Better",
        },
        ArtifactManifest {
            repository: "handy-computer/whisper-small.en-gguf",
            revision: "41b0f75fd44415ba127a5356c5ba9ed450c1debd",
            filename: "whisper-small.en-Q8_0.gguf",
            size_bytes: 269_674_144,
            sha256: "9614e6b7fda2d26018e4f268aece8ca25a83296ea0b534169a585b740bfd71ef",
        },
        false,
        PHASE_ZERO_SMOKE,
    ),
    whisper_manifest(
        "whisper_cpp_medium_en",
        "Whisper Medium — English",
        "medium.en",
        ModelGuidance {
            description: "Higher-accuracy local English model for machines with more memory.",
            storage: "~793 MB",
            expected_ram: "5 GB",
            speed: "Slower",
            accuracy: "High",
        },
        ArtifactManifest {
            repository: "handy-computer/whisper-medium.en-gguf",
            revision: "f25c70d9095dcfdad187ebb3b113d157b414aee8",
            filename: "whisper-medium.en-Q8_0.gguf",
            size_bytes: 831_460_928,
            sha256: "03d7257fef498750ce272631bc6a34de322fc2b438aab5c268ff49dfd1b64c49",
        },
        false,
        PHASE_ZERO_SMOKE,
    ),
];

const fn handy_computer_tiny_en_manifest() -> ModelManifest {
    ModelManifest {
        id: "whisper_cpp_tiny_en",
        display_name: "Whisper Tiny — English",
        variant_label: "tiny.en",
        description: "Small English GGUF model for low-resource local dictation.",
        storage_guidance: "~42 MB",
        expected_ram: "1 GB",
        speed_guidance: "Fastest",
        accuracy_guidance: "Basic",
        runtime: Some(RuntimeRequirement::PrimaryNative),
        architecture: ModelArchitecture::EncoderDecoder,
        minimum_runtime_version: TRANSCRIBE_CPP_VERSION,
        artifact: ModelArtifactBinding::SingleGguf(ArtifactManifest {
            repository: "handy-computer/whisper-tiny.en-gguf",
            revision: HANDY_COMPUTER_TINY_EN_REVISION,
            filename: "whisper-tiny.en-Q4_K_M.gguf",
            size_bytes: 43_545_248,
            sha256: "3bfa6200aa12a21409445401f7871b5c733546dc45a29eb4871fcb3c7954e08b",
        }),
        languages: &["en"],
        capabilities: BATCH_ENGLISH_CAPABILITIES,
        recommended: false,
        roles: NO_ROLES,
        compatibility: CompatibilityStatus::Experimental {
            evidence: PHASE_ZERO_SMOKE.link(),
            reason: "The complete compatibility suite has not passed.",
        },
        evidence: PHASE_ZERO_SMOKE,
    }
}

const fn moonshine_tiny_en_int8_onnx_manifest() -> ModelManifest {
    ModelManifest {
        id: "moonshine-tiny-en-int8-onnx",
        display_name: "Moonshine Tiny — English",
        variant_label: "Tiny",
        description: "Compact local English model for fast dictation.",
        storage_guidance: "~42 MB",
        expected_ram: "1 GB",
        speed_guidance: "Fast",
        accuracy_guidance: "Good",
        recommended: false,
        runtime: None,
        architecture: ModelArchitecture::EncoderDecoder,
        minimum_runtime_version: TRANSCRIBE_CPP_VERSION,
        artifact: ModelArtifactBinding::ReceiptBackedBundle {
            bundle_id: "moonshine-tiny-en-int8-onnx",
            aggregate_size_bytes: 44_256_550,
        },
        languages: &["en"],
        capabilities: MOONSHINE_TINY_CAPABILITIES,
        roles: NO_ROLES,
        compatibility: CompatibilityStatus::Experimental {
            evidence: MOONSHINE_TINY_ONNX_EXPERIMENTAL.link(),
            reason: "The complete compatibility suite has not passed.",
        },
        evidence: MOONSHINE_TINY_ONNX_EXPERIMENTAL,
    }
}

const fn moonshine_base_en_int8_onnx_manifest() -> ModelManifest {
    ModelManifest {
        id: "moonshine-base-en-int8-onnx",
        display_name: "Moonshine Base — English",
        variant_label: "Base INT8",
        description: "Converted five-file Moonshine Base INT8 English model; source and converter revisions are unrecorded.",
        storage_guidance: "~274 MiB",
        expected_ram: "Not yet measured",
        speed_guidance: "Not yet measured",
        accuracy_guidance: "Fixture verified only",
        recommended: false,
        runtime: None,
        architecture: ModelArchitecture::EncoderDecoder,
        minimum_runtime_version: TRANSCRIBE_CPP_VERSION,
        artifact: ModelArtifactBinding::ReceiptBackedBundle {
            bundle_id: "moonshine-base-en-int8-onnx",
            aggregate_size_bytes: 286_930_831,
        },
        languages: &["en"],
        capabilities: MOONSHINE_BASE_CAPABILITIES,
        roles: NO_ROLES,
        compatibility: CompatibilityStatus::Experimental {
            evidence: MOONSHINE_BASE_ONNX_EXPERIMENTAL.link(),
            reason: "Cancellation, restart recovery, latency, resource use, accelerators, and non-Windows support remain unverified.",
        },
        evidence: MOONSHINE_BASE_ONNX_EXPERIMENTAL,
    }
}

const fn parakeet_tdt_06b_v2_en_int8_onnx_manifest() -> ModelManifest {
    ModelManifest {
        id: "parakeet-tdt-06b-v2-en-int8-onnx",
        display_name: "Parakeet TDT 0.6B v2 — English",
        variant_label: "int8",
        description: "Experimental local English final-text model. Fixture verified only.",
        storage_guidance: "~631 MiB",
        expected_ram: "Not measured",
        speed_guidance: "Not measured",
        accuracy_guidance: "Not measured",
        recommended: false,
        runtime: None,
        architecture: ModelArchitecture::EncoderDecoder,
        minimum_runtime_version: TRANSCRIBE_CPP_VERSION,
        artifact: ModelArtifactBinding::ReceiptBackedBundle {
            bundle_id: "parakeet-tdt-06b-v2-en-int8-onnx",
            aggregate_size_bytes: 661_190_513,
        },
        languages: &["en"],
        capabilities: PARAKEET_TDT_V2_CAPABILITIES,
        roles: NO_ROLES,
        compatibility: CompatibilityStatus::Experimental {
            evidence: PARAKEET_TDT_V2_ONNX_EXPERIMENTAL.link(),
            reason: "Only the Windows Sherpa ONNX load, fixture, and unload/reload gate has passed; cancellation, restart recovery, latency, resource use, accelerators, and non-Windows support remain unverified.",
        },
        evidence: PARAKEET_TDT_V2_ONNX_EXPERIMENTAL,
    }
}

#[allow(clippy::too_many_arguments)] // Manifest fields stay explicit at the single catalog definition site.
const fn whisper_manifest(
    id: &'static str,
    display_name: &'static str,
    variant_label: &'static str,
    guidance: ModelGuidance,
    artifact: ArtifactManifest,
    recommended: bool,
    evidence: CompatibilityEvidence,
) -> ModelManifest {
    ModelManifest {
        id,
        display_name,
        variant_label,
        description: guidance.description,
        storage_guidance: guidance.storage,
        expected_ram: guidance.expected_ram,
        speed_guidance: guidance.speed,
        accuracy_guidance: guidance.accuracy,
        recommended,
        runtime: Some(RuntimeRequirement::PrimaryNative),
        architecture: ModelArchitecture::EncoderDecoder,
        minimum_runtime_version: TRANSCRIBE_CPP_VERSION,
        artifact: ModelArtifactBinding::SingleGguf(artifact),
        languages: &["en"],
        capabilities: BATCH_ENGLISH_CAPABILITIES,
        roles: NO_ROLES,
        compatibility: CompatibilityStatus::Experimental {
            evidence: evidence.link(),
            reason: "The complete compatibility suite has not passed.",
        },
        evidence,
    }
}

pub fn model_descriptors() -> Vec<ModelDescriptor> {
    assert_catalog_valid();
    MODELS.iter().map(ModelManifest::descriptor).collect()
}

/// Models available in Scribe's normal user-facing flow. Legacy GGML artifacts
/// are retained only to resolve existing installations, never as new installs.
pub fn normal_model_descriptors() -> Vec<ModelDescriptor> {
    assert_catalog_valid();
    MODELS.iter().map(ModelManifest::descriptor).collect()
}

pub fn model_descriptor(id: &ModelId) -> Option<ModelDescriptor> {
    assert_catalog_valid();
    MODELS
        .iter()
        .find(|manifest| manifest.id == id.as_str())
        .map(ModelManifest::descriptor)
}

pub(crate) fn runtime_model_manifest(id: &ModelId) -> Option<RuntimeModelManifest> {
    assert_catalog_valid();
    MODELS
        .iter()
        .find(|manifest| manifest.id == id.as_str())
        .and_then(|manifest| {
            let runtime = manifest.runtime?;
            manifest
                .artifact
                .single_gguf()
                .map(|artifact| RuntimeModelManifest {
                    id: manifest.id,
                    runtime,
                    minimum_runtime_version: manifest.minimum_runtime_version,
                    artifact_repository: artifact.repository,
                    artifact_revision: artifact.revision,
                    artifact_filename: artifact.filename,
                    artifact_size_bytes: artifact.size_bytes,
                    artifact_storage_estimate: manifest.storage_guidance,
                    artifact_sha256: artifact.sha256,
                })
        })
}

pub(crate) fn normalized_install_artifact(id: &ModelId) -> Option<NormalizedInstallArtifact> {
    assert_catalog_valid();
    MODELS
        .iter()
        .find(|manifest| manifest.id == id.as_str())
        .map(|manifest| match manifest.artifact {
            ModelArtifactBinding::SingleGguf(artifact) => NormalizedInstallArtifact::SingleGguf(
                runtime_artifact(artifact, ArtifactFormat::Gguf),
            ),
            ModelArtifactBinding::ReceiptBackedBundle {
                bundle_id,
                aggregate_size_bytes,
            } => NormalizedInstallArtifact::ReceiptBackedBundle {
                bundle_id,
                aggregate_size_bytes,
            },
        })
}

/// Returns a catalog-authorized receipt-backed bundle ID only when the
/// descriptor's typed receipt binding is self-consistent.
pub(crate) fn normalized_receipt_backed_bundle_id(id: &ModelId) -> Option<&'static str> {
    receipt_backed_bundle_id_for_artifact(id.as_str(), normalized_install_artifact(id)?)
}

fn receipt_backed_bundle_id_for_artifact(
    model_id: &str,
    artifact: NormalizedInstallArtifact,
) -> Option<&'static str> {
    match artifact {
        NormalizedInstallArtifact::ReceiptBackedBundle { bundle_id, .. }
            if bundle_id == model_id =>
        {
            Some(bundle_id)
        }
        _ => None,
    }
}

pub(crate) fn runtime_artifact_manifest_for_path(
    id: &ModelId,
    path: &Path,
) -> Option<RuntimeArtifactManifest> {
    let manifest = runtime_model_manifest(id)?;
    let filename = path.file_name()?.to_str()?;
    if filename == manifest.artifact_filename {
        return Some(RuntimeArtifactManifest {
            repository: manifest.artifact_repository,
            revision: manifest.artifact_revision,
            filename: manifest.artifact_filename,
            size_bytes: manifest.artifact_size_bytes,
            sha256: manifest.artifact_sha256,
            format: ArtifactFormat::Gguf,
        });
    }
    None
}

const fn runtime_artifact(
    artifact: ArtifactManifest,
    format: ArtifactFormat,
) -> RuntimeArtifactManifest {
    RuntimeArtifactManifest {
        repository: artifact.repository,
        revision: artifact.revision,
        filename: artifact.filename,
        size_bytes: artifact.size_bytes,
        sha256: artifact.sha256,
        format,
    }
}

/// Only single-file GGUF entries are loaded by the embedded runtime.
pub(crate) fn model_uses_embedded_runtime(id: &ModelId) -> bool {
    assert_catalog_valid();
    MODELS
        .iter()
        .any(|manifest| manifest.id == id.as_str() && manifest.artifact.single_gguf().is_some())
}

/// Resolves a remote artifact to an existing normalized catalog entry only
/// when every trust-relevant source fact is identical. Callers must not infer
/// that another variant from the same repository is managed by Scribe.
pub(crate) fn normalized_model_id_for_pinned_artifact(
    repository: &str,
    revision: &str,
    filename: &str,
) -> Option<ModelId> {
    assert_catalog_valid();
    MODELS
        .iter()
        .find(|manifest| {
            manifest.artifact.single_gguf().is_some_and(|artifact| {
                artifact.repository == repository
                    && artifact.revision == revision
                    && artifact.filename == filename
            })
        })
        .map(|manifest| ModelId::new(manifest.id))
}

pub(crate) fn runtime_model_download_url(id: &ModelId) -> Option<String> {
    let manifest = runtime_model_manifest(id)?;
    Some(format!(
        "https://huggingface.co/{}/resolve/{}/{}",
        manifest.artifact_repository, manifest.artifact_revision, manifest.artifact_filename
    ))
}

pub(crate) fn validate_catalog() -> Result<(), String> {
    validate_manifests(MODELS)
}

fn cached_validation(
    cache: &OnceLock<Result<(), String>>,
    validate: impl FnOnce() -> Result<(), String>,
) -> &Result<(), String> {
    cache.get_or_init(validate)
}

fn assert_catalog_valid() {
    static VALIDATION: OnceLock<Result<(), String>> = OnceLock::new();
    cached_validation(&VALIDATION, validate_catalog)
        .as_ref()
        .expect("normalized model catalog must satisfy evidence and integrity rules");
}

impl ModelManifest {
    fn descriptor(&self) -> ModelDescriptor {
        ModelDescriptor {
            id: ModelId::new(self.id),
            display_name: self.display_name,
            variant_label: self.variant_label,
            description: self.description,
            expected_ram: self.expected_ram,
            speed_guidance: self.speed_guidance,
            accuracy_guidance: self.accuracy_guidance,
            recommended: self.recommended,
            artifact_size_bytes: self.artifact.aggregate_size_bytes(),
            languages: self.languages.to_vec(),
            capabilities: self.capabilities,
            roles: self.roles.to_vec(),
            compatibility: self.compatibility,
        }
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EvidenceGateDecision {
    Go,
    NoGo,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EvidenceGateCriterion {
    pub name: &'static str,
    pub requirement: &'static str,
    pub met: bool,
    pub finding: &'static str,
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StreamingCandidateGate {
    pub runtime_version: &'static str,
    pub runtime_commit: &'static str,
    pub model_id: &'static str,
    pub criteria: &'static [EvidenceGateCriterion],
}

#[cfg(test)]
impl StreamingCandidateGate {
    pub fn decision(self) -> EvidenceGateDecision {
        if self.criteria.iter().all(|criterion| criterion.met) {
            EvidenceGateDecision::Go
        } else {
            EvidenceGateDecision::NoGo
        }
    }
}

#[cfg(test)]
const ZIPFORMER_CRITERIA: &[EvidenceGateCriterion] = &[
    EvidenceGateCriterion {
        name: "pinned-package-and-model",
        requirement: "Exact native package and model artifact revisions, sizes, and SHA-256 hashes",
        met: false,
        finding: "The source commit and model name are known, but no exact Windows native package or model artifact has been pinned and verified.",
    },
    EvidenceGateCriterion {
        name: "primary-comparator",
        requirement: "Primary rolling-preview p95 measured on the same machine and fixture corpus",
        met: false,
        finding: "The primary rolling-preview implementation is scheduled for Phase 7, so the required comparator does not exist.",
    },
    EvidenceGateCriterion {
        name: "warm-first-partial",
        requirement: "p95 warm first partial at or below 800 ms and at least 30% below the primary comparator",
        met: false,
        finding: "No native candidate first-partial measurement is available and the comparative threshold cannot be evaluated.",
    },
    EvidenceGateCriterion {
        name: "real-time-factor",
        requirement: "Real-time factor below 1 on the shared fixture corpus",
        met: false,
        finding: "No candidate RTF measurement exists for the pinned native package and model artifact.",
    },
    EvidenceGateCriterion {
        name: "cancellation",
        requirement: "Cancellation acknowledgement within 250 ms",
        met: false,
        finding: "No native candidate cancellation measurement exists.",
    },
    EvidenceGateCriterion {
        name: "wer",
        requirement: "No more than 3 percentage points absolute WER regression on the shared fixture corpus",
        met: false,
        finding: "A shared transcribed fixture corpus and candidate WER result are not available.",
    },
    EvidenceGateCriterion {
        name: "common-contract",
        requirement: "Complete common-contract and unload/reload tests",
        met: false,
        finding: "The candidate has not been implemented behind SpeechEngine, so contract and lifecycle evidence is absent.",
    },
    EvidenceGateCriterion {
        name: "recovery-and-resources",
        requirement: "Crash-recovery and memory tests",
        met: false,
        finding: "No candidate crash-recovery or memory evidence exists.",
    },
    EvidenceGateCriterion {
        name: "windows-platform",
        requirement: "Complete Windows platform measurements using the native C API",
        met: false,
        finding: "The exact native package has not been installed or exercised on Windows.",
    },
];

#[cfg(test)]
pub const ZIPFORMER_STREAMING_GATE: StreamingCandidateGate = StreamingCandidateGate {
    runtime_version: "1.13.4",
    runtime_commit: "142807252687d81b40d6315f23470a1512a00de3",
    model_id: "sherpa-onnx-streaming-zipformer-en-2023-06-26",
    criteria: ZIPFORMER_CRITERIA,
};

fn validate_manifests(manifests: &[ModelManifest]) -> Result<(), String> {
    let mut ids = HashSet::new();
    for manifest in manifests {
        if manifest.id.is_empty() || !ids.insert(manifest.id) {
            return Err(format!("duplicate or empty model id: {}", manifest.id));
        }
        if manifest.display_name.is_empty()
            || manifest.description.is_empty()
            || manifest.storage_guidance.is_empty()
            || manifest.expected_ram.is_empty()
            || manifest.speed_guidance.is_empty()
            || manifest.accuracy_guidance.is_empty()
        {
            return Err(format!("{} has incomplete user guidance", manifest.id));
        }
        if manifest.runtime.is_some()
            && manifest.minimum_runtime_version
                == (RuntimeVersion {
                    major: 0,
                    minor: 0,
                    patch: 0,
                })
        {
            return Err(format!("{} has no minimum runtime version", manifest.id));
        }
        match manifest.artifact {
            ModelArtifactBinding::SingleGguf(artifact) => validate_artifact(artifact)?,
            ModelArtifactBinding::ReceiptBackedBundle {
                bundle_id,
                aggregate_size_bytes,
            } => {
                if manifest.id != bundle_id {
                    return Err(format!(
                        "{} must use its own receipt-backed bundle id",
                        manifest.id
                    ));
                }
                validate_receipt_backed_bundle(bundle_id, aggregate_size_bytes)?;
            }
        }
        if manifest.languages.is_empty()
            || manifest
                .languages
                .iter()
                .any(|language| language.is_empty())
        {
            return Err(format!("{} has invalid languages", manifest.id));
        }
        if manifest.evidence.id.is_empty() || manifest.evidence.source.is_empty() {
            return Err(format!(
                "{} has unlinked compatibility evidence",
                manifest.id
            ));
        }
        if manifest.variant_label.is_empty() {
            return Err(format!("{} has empty variant label", manifest.id));
        }
        let (status_evidence, reason) = match manifest.compatibility {
            CompatibilityStatus::Experimental { evidence, reason } => (evidence, Some(reason)),
            #[cfg(test)]
            CompatibilityStatus::Supported { evidence } => (evidence, None),
            #[cfg(test)]
            CompatibilityStatus::Incompatible { evidence, reason } => (evidence, Some(reason)),
        };
        if status_evidence != manifest.evidence.link() {
            return Err(format!(
                "{} compatibility status cites different evidence",
                manifest.id
            ));
        }
        if reason.is_some_and(str::is_empty) {
            return Err(format!(
                "{} compatibility status has no explanatory reason",
                manifest.id
            ));
        }
        let supported = match manifest.compatibility {
            CompatibilityStatus::Experimental { .. } => false,
            #[cfg(test)]
            CompatibilityStatus::Supported { .. } => true,
            #[cfg(test)]
            CompatibilityStatus::Incompatible { .. } => false,
        };
        if supported {
            if !manifest.evidence.complete() {
                return Err(format!(
                    "{} cannot be Supported without complete evidence and a receipt",
                    manifest.id
                ));
            }
            validate_compatibility_receipt(manifest)?;
        }
        if !manifest.roles.is_empty() && !supported {
            return Err(format!(
                "{} cannot receive a curated role before Supported status",
                manifest.id
            ));
        }
        let mut roles = HashSet::new();
        if manifest.roles.iter().any(|role| !roles.insert(*role)) {
            return Err(format!("{} has duplicate roles", manifest.id));
        }
    }
    Ok(())
}

fn validate_compatibility_receipt(manifest: &ModelManifest) -> Result<(), String> {
    let receipt = manifest
        .evidence
        .receipt
        .ok_or_else(|| format!("{} has no compatibility receipt", manifest.id))?;
    if receipt.sha256.len() != 64 || !receipt.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("{} has a malformed receipt hash", manifest.id));
    }
    let actual_hash = format!("{:x}", Sha256::digest(receipt.json.as_bytes()));
    if !actual_hash.eq_ignore_ascii_case(receipt.sha256) {
        return Err(format!(
            "{} compatibility receipt hash mismatch",
            manifest.id
        ));
    }
    let document: CompatibilityReceiptDocument =
        serde_json::from_str(receipt.json).map_err(|error| {
            format!(
                "{} has an invalid compatibility receipt: {error}",
                manifest.id
            )
        })?;
    let runtime_version = format!(
        "{}.{}.{}",
        manifest.minimum_runtime_version.major,
        manifest.minimum_runtime_version.minor,
        manifest.minimum_runtime_version.patch
    );
    let valid_hash =
        |value: &str| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit());
    let embedded_hash = |contents: &[u8]| format!("{:x}", Sha256::digest(contents));
    let artifact = manifest.artifact.single_gguf().ok_or_else(|| {
        format!(
            "{} cannot use a single-artifact compatibility receipt for a receipt-backed bundle",
            manifest.id
        )
    })?;
    if document.schema_version != 1
        || document.model_id != manifest.id
        || document.evidence_id != manifest.evidence.id
        || document.runtime_version != runtime_version
        || !document
            .model_artifact_sha256
            .eq_ignore_ascii_case(artifact.sha256)
        || !valid_hash(&document.runtime_package_sha256)
        || !embedded_hash(receipt.runtime_package_manifest)
            .eq_ignore_ascii_case(&document.runtime_package_sha256)
        || !valid_hash(&document.fixture_corpus_sha256)
        || !embedded_hash(receipt.fixture_corpus_manifest)
            .eq_ignore_ascii_case(&document.fixture_corpus_sha256)
        || !valid_hash(&document.results_sha256)
        || !embedded_hash(receipt.results_manifest).eq_ignore_ascii_case(&document.results_sha256)
        || document.platform != "windows-x86_64"
        || !document.load
        || !document.known_fixture
        || !document.cancellation
        || !document.unload_reload
        || !document.acceleration
        || !document.platform_tests
    {
        return Err(format!(
            "{} compatibility receipt does not match the manifest or complete gate",
            manifest.id
        ));
    }
    Ok(())
}

fn validate_receipt_backed_bundle(
    bundle_id: &str,
    aggregate_size_bytes: u64,
) -> Result<(), String> {
    let stable_id = !bundle_id.is_empty()
        && bundle_id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        });
    if !stable_id {
        return Err("receipt-backed bundle id is invalid".to_owned());
    }
    if aggregate_size_bytes == 0 {
        return Err("receipt-backed bundle aggregate size is invalid".to_owned());
    }
    let pinned_aggregate_size_bytes =
        crate::receipt_bundle_catalog::available_bundle_aggregate_size_bytes(bundle_id)
            .ok_or_else(|| {
                format!("receipt-backed bundle {bundle_id} is not an available pinned bundle")
            })?;
    if pinned_aggregate_size_bytes != aggregate_size_bytes {
        return Err(format!(
            "receipt-backed bundle {bundle_id} aggregate size does not match its pinned files"
        ));
    }
    Ok(())
}

fn validate_artifact(artifact: ArtifactManifest) -> Result<(), String> {
    let safe_filename = !artifact.filename.is_empty()
        && !artifact.filename.contains('/')
        && !artifact.filename.contains('\\')
        && artifact.filename != "."
        && artifact.filename != "..";
    if artifact.repository.is_empty()
        || artifact.revision.len() != 40
        || !artifact
            .revision
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || !safe_filename
        || artifact.size_bytes == 0
        || artifact.sha256.len() != 64
        || !artifact.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(format!("malformed artifact for {}", artifact.filename));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn immutable_catalog_validation_is_cached_once_per_once_lock() {
        let cache = OnceLock::new();
        let calls = Cell::new(0);

        let first = cached_validation(&cache, || {
            calls.set(calls.get() + 1);
            Ok(())
        });
        let second = cached_validation(&cache, || {
            calls.set(calls.get() + 1);
            Err("second validation must not run".to_owned())
        });

        assert!(first.is_ok());
        assert!(second.is_ok());
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn production_catalog_is_valid_and_has_unique_ids() {
        assert_eq!(validate_catalog(), Ok(()));
        assert_eq!(model_descriptors().len(), 7);
        assert_eq!(normal_model_descriptors().len(), 7);
        assert_eq!(
            model_descriptors()
                .into_iter()
                .map(|descriptor| (
                    descriptor.id,
                    descriptor.display_name,
                    descriptor.variant_label
                ))
                .collect::<Vec<_>>(),
            vec![
                (
                    ModelId::new("whisper_cpp_tiny_en"),
                    "Whisper Tiny — English",
                    "tiny.en",
                ),
                (
                    ModelId::new("moonshine-tiny-en-int8-onnx"),
                    "Moonshine Tiny — English",
                    "Tiny",
                ),
                (
                    ModelId::new("moonshine-base-en-int8-onnx"),
                    "Moonshine Base — English",
                    "Base INT8",
                ),
                (
                    ModelId::new("parakeet-tdt-06b-v2-en-int8-onnx"),
                    "Parakeet TDT 0.6B v2 — English",
                    "int8",
                ),
                (
                    ModelId::new("whisper_cpp_base_en"),
                    "Whisper Base — English",
                    "base.en",
                ),
                (
                    ModelId::new("whisper_cpp_small_en"),
                    "Whisper Small — English",
                    "small.en",
                ),
                (
                    ModelId::new("whisper_cpp_medium_en"),
                    "Whisper Medium — English",
                    "medium.en",
                ),
            ]
        );
        assert_eq!(
            normal_model_descriptors()
                .into_iter()
                .map(|descriptor| descriptor.id)
                .collect::<Vec<_>>(),
            vec![
                ModelId::new("whisper_cpp_tiny_en"),
                ModelId::new("moonshine-tiny-en-int8-onnx"),
                ModelId::new("moonshine-base-en-int8-onnx"),
                ModelId::new("parakeet-tdt-06b-v2-en-int8-onnx"),
                ModelId::new("whisper_cpp_base_en"),
                ModelId::new("whisper_cpp_small_en"),
                ModelId::new("whisper_cpp_medium_en"),
            ]
        );
    }

    #[test]
    fn remote_artifact_must_match_every_pinned_source_fact_to_use_local_installation() {
        let artifact = MODELS[0].artifact.single_gguf().unwrap();
        assert_eq!(
            normalized_model_id_for_pinned_artifact(
                artifact.repository,
                artifact.revision,
                artifact.filename,
            ),
            Some(ModelId::new("whisper_cpp_tiny_en"))
        );
        assert!(
            normalized_model_id_for_pinned_artifact(
                artifact.repository,
                artifact.revision,
                "another-quantization.gguf",
            )
            .is_none()
        );
    }

    #[test]
    fn descriptor_exposes_the_catalog_recommendation() {
        let base = model_descriptor(&ModelId::new("whisper_cpp_base_en")).unwrap();
        let tiny = model_descriptor(&ModelId::new("whisper_cpp_tiny_en")).unwrap();

        assert!(base.recommended);
        assert!(!tiny.recommended);
    }

    #[test]
    fn duplicate_ids_are_rejected() {
        assert!(
            validate_manifests(&[MODELS[0], MODELS[0]])
                .unwrap_err()
                .contains("duplicate")
        );
    }

    #[test]
    fn malformed_artifacts_are_rejected() {
        let mut manifest = MODELS[0];
        let mut artifact = manifest.artifact.single_gguf().unwrap();
        artifact.filename = "../model.bin";
        artifact.sha256 = "not-a-sha";
        manifest.artifact = ModelArtifactBinding::SingleGguf(artifact);

        assert!(
            validate_manifests(&[manifest])
                .unwrap_err()
                .contains("malformed artifact")
        );
    }

    #[test]
    fn empty_variant_labels_are_rejected() {
        let mut manifest = MODELS[0];
        manifest.variant_label = "";

        assert!(
            validate_manifests(&[manifest])
                .unwrap_err()
                .contains("empty variant label")
        );
    }

    #[test]
    fn incomplete_evidence_cannot_promote_a_model() {
        let mut manifest = MODELS[1];
        manifest.compatibility = CompatibilityStatus::Supported {
            evidence: manifest.evidence.link(),
        };

        assert!(
            validate_manifests(&[manifest])
                .unwrap_err()
                .contains("cannot be Supported")
        );
    }

    #[test]
    fn complete_booleans_cannot_promote_without_a_hashed_receipt() {
        let mut manifest = MODELS[0];
        manifest.evidence = CompatibilityEvidence {
            load: true,
            known_fixture: true,
            cancellation: true,
            unload_reload: true,
            acceleration: true,
            platform: true,
            receipt: None,
            ..manifest.evidence
        };
        manifest.compatibility = CompatibilityStatus::Supported {
            evidence: manifest.evidence.link(),
        };

        assert!(
            validate_manifests(&[manifest])
                .unwrap_err()
                .contains("receipt")
        );
    }

    #[test]
    fn supported_status_requires_a_matching_machine_readable_receipt() {
        const RECEIPT: &str = r#"{"schema_version":1,"model_id":"whisper_cpp_tiny_en","evidence_id":"phase-0-whisper-jfk-process-smoke","runtime_version":"1.9.1","model_artifact_sha256":"921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f","runtime_package_sha256":"6510693d373c9ed4adbb708015135ea9ec885c8e4312d543ca7d1c2f3dbbd7dc","fixture_corpus_sha256":"9799f3da4289f4db1586d89570026fc0d2ba4f5cec8c64daaebedf8e0643cccf","results_sha256":"a341e990e02ed8589238eb1e8c152a855ec2fbbcd2519d069f5378c098bb28fa","platform":"windows-x86_64","load":true,"known_fixture":true,"cancellation":true,"unload_reload":true,"acceleration":true,"platform_tests":true}"#;
        let mut manifest = MODELS[0];
        let mut artifact = manifest.artifact.single_gguf().unwrap();
        artifact.sha256 = "921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f";
        manifest.artifact = ModelArtifactBinding::SingleGguf(artifact);
        manifest.evidence = CompatibilityEvidence {
            load: true,
            known_fixture: true,
            cancellation: true,
            unload_reload: true,
            acceleration: true,
            platform: true,
            receipt: Some(CompatibilityReceipt {
                json: RECEIPT,
                sha256: "602aa9895b6b3eea75c93eebddfc828c34a8a19c105c5e47034aa9c06556b39f",
                runtime_package_manifest: br#"{"package":"fixture"}"#,
                fixture_corpus_manifest: br#"{"corpus":"fixture"}"#,
                results_manifest: br#"{"results":"pass"}"#,
            }),
            ..manifest.evidence
        };
        manifest.compatibility = CompatibilityStatus::Supported {
            evidence: manifest.evidence.link(),
        };

        assert_eq!(validate_manifests(&[manifest]), Ok(()));

        manifest.evidence.receipt = Some(CompatibilityReceipt {
            json: RECEIPT,
            sha256: "602aa9895b6b3eea75c93eebddfc828c34a8a19c105c5e47034aa9c06556b39f",
            runtime_package_manifest: br#"{"package":"fixture"}"#,
            fixture_corpus_manifest: br#"{"corpus":"fixture"}"#,
            results_manifest: b"tampered",
        });
        assert!(
            validate_manifests(&[manifest])
                .unwrap_err()
                .contains("does not match")
        );

        manifest.evidence.receipt = Some(CompatibilityReceipt {
            json: RECEIPT,
            sha256: "702aa9895b6b3eea75c93eebddfc828c34a8a19c105c5e47034aa9c06556b39f",
            runtime_package_manifest: br#"{"package":"fixture"}"#,
            fixture_corpus_manifest: br#"{"corpus":"fixture"}"#,
            results_manifest: br#"{"results":"pass"}"#,
        });
        assert!(
            validate_manifests(&[manifest])
                .unwrap_err()
                .contains("hash mismatch")
        );
    }

    #[test]
    fn compatibility_status_must_cite_its_manifest_evidence() {
        let mut manifest = MODELS[0];
        manifest.compatibility = CompatibilityStatus::Experimental {
            evidence: PHASE_TWO_BASE_SMOKE.link(),
            reason: "still incomplete",
        };

        assert!(
            validate_manifests(&[manifest])
                .unwrap_err()
                .contains("cites different evidence")
        );
    }

    #[test]
    fn tentative_status_requires_an_explanatory_reason() {
        let mut manifest = MODELS[0];
        manifest.compatibility = CompatibilityStatus::Experimental {
            evidence: manifest.evidence.link(),
            reason: "",
        };

        assert!(
            validate_manifests(&[manifest])
                .unwrap_err()
                .contains("no explanatory reason")
        );
    }

    #[test]
    fn curated_roles_require_supported_status() {
        let mut manifest = MODELS[0];
        manifest.roles = &[ModelRole::FastEnglish];

        assert!(
            validate_manifests(&[manifest])
                .unwrap_err()
                .contains("before Supported status")
        );
        assert!(MODELS.iter().all(|model| model.roles.is_empty()));
    }

    #[test]
    fn routing_uses_manifest_data_not_model_id_prefixes() {
        let mut renamed = MODELS[0];
        renamed.id = "name-with-no-runtime-prefix";

        assert_eq!(renamed.runtime, Some(RuntimeRequirement::PrimaryNative));
        assert_eq!(
            runtime_model_manifest(&ModelId::new("whisper_cpp_base_en"))
                .map(|manifest| manifest.runtime),
            Some(RuntimeRequirement::PrimaryNative),
        );
        assert_eq!(
            runtime_model_manifest(&ModelId::new("transcribe_cpp_unknown")),
            None
        );
    }

    #[test]
    fn q8_gguf_download_urls_are_derived_from_the_authoritative_manifests() {
        assert_eq!(
            runtime_model_download_url(&ModelId::new("whisper_cpp_base_en")).as_deref(),
            Some(
                "https://huggingface.co/handy-computer/whisper-base.en-gguf/resolve/cf0804db15fb341d00c9274b90da9cbb4fe2e5c6/whisper-base.en-Q8_0.gguf"
            ),
        );
        assert_eq!(
            runtime_model_download_url(&ModelId::new("whisper_cpp_small_en")).as_deref(),
            Some(
                "https://huggingface.co/handy-computer/whisper-small.en-gguf/resolve/41b0f75fd44415ba127a5356c5ba9ed450c1debd/whisper-small.en-Q8_0.gguf"
            ),
        );
        assert_eq!(
            runtime_model_download_url(&ModelId::new("whisper_cpp_medium_en")).as_deref(),
            Some(
                "https://huggingface.co/handy-computer/whisper-medium.en-gguf/resolve/f25c70d9095dcfdad187ebb3b113d157b414aee8/whisper-medium.en-Q8_0.gguf"
            ),
        );
    }

    #[test]
    fn receipt_backed_onnx_descriptors_are_exactly_normalized_and_experimental() {
        let receipt_backed = [
            ("moonshine-tiny-en-int8-onnx", 44_256_550),
            ("moonshine-base-en-int8-onnx", 286_930_831),
            ("parakeet-tdt-06b-v2-en-int8-onnx", 661_190_513),
        ];

        for (id, aggregate_size_bytes) in receipt_backed {
            let descriptor = model_descriptor(&ModelId::new(id)).unwrap();
            assert_eq!(descriptor.artifact_size_bytes, aggregate_size_bytes);
            assert_eq!(descriptor.languages, vec!["en"]);
            assert!(descriptor.capabilities.batch_transcription);
            assert!(!descriptor.capabilities.native_streaming);
            assert!(!descriptor.capabilities.timestamps);
            assert_eq!(
                descriptor.capabilities.cancellation,
                id != "moonshine-base-en-int8-onnx" && id != "parakeet-tdt-06b-v2-en-int8-onnx"
            );
            assert!(!descriptor.capabilities.translation);
            assert!(!descriptor.capabilities.language_detection);
            assert!(descriptor.capabilities.cpu);
            assert!(!descriptor.capabilities.gpu);
            assert!(!descriptor.recommended);
            assert!(descriptor.roles.is_empty());
            assert!(matches!(
                descriptor.compatibility,
                CompatibilityStatus::Experimental { .. }
            ));
            assert_eq!(
                normalized_install_artifact(&descriptor.id),
                Some(NormalizedInstallArtifact::ReceiptBackedBundle {
                    bundle_id: id,
                    aggregate_size_bytes,
                })
            );
            assert_eq!(
                normalized_receipt_backed_bundle_id(&descriptor.id),
                Some(id)
            );
            assert_eq!(runtime_model_manifest(&descriptor.id), None);
            assert!(!model_uses_embedded_runtime(&descriptor.id));
            assert_eq!(runtime_model_download_url(&descriptor.id), None);
            assert_eq!(
                crate::receipt_bundle_catalog::available_bundle_aggregate_size_bytes(id),
                Some(aggregate_size_bytes)
            );
        }

        let base = model_descriptor(&ModelId::new("moonshine-base-en-int8-onnx")).unwrap();
        assert!(!base.capabilities.cancellation);
        assert_eq!(base.expected_ram, "Not yet measured");
        assert_eq!(base.speed_guidance, "Not yet measured");
        assert_eq!(base.accuracy_guidance, "Fixture verified only");
        let base_manifest = MODELS
            .iter()
            .find(|manifest| manifest.id == "moonshine-base-en-int8-onnx")
            .unwrap();
        assert_eq!(base_manifest.evidence, MOONSHINE_BASE_ONNX_EXPERIMENTAL);
        assert!(base_manifest.evidence.load);
        assert!(base_manifest.evidence.known_fixture);
        assert!(!base_manifest.evidence.cancellation);
        assert!(base_manifest.evidence.unload_reload);
        assert!(!base_manifest.evidence.acceleration);
        assert!(base_manifest.evidence.platform);
        assert_eq!(base_manifest.evidence.receipt, None);
        assert!(matches!(
            base.compatibility,
            CompatibilityStatus::Experimental { evidence, reason }
                if evidence == MOONSHINE_BASE_ONNX_EXPERIMENTAL.link()
                    && reason.contains("Cancellation")
        ));
        assert_eq!(
            MODELS
                .iter()
                .filter(|model| {
                    matches!(
                        model.artifact,
                        ModelArtifactBinding::ReceiptBackedBundle { .. }
                    )
                })
                .count(),
            3
        );
    }

    #[test]
    fn moonshine_base_descriptor_matches_the_available_bundle_aggregate() {
        assert_eq!(
            crate::receipt_bundle_catalog::available_bundle_aggregate_size_bytes(
                "moonshine-base-en-int8-onnx"
            ),
            Some(286_930_831)
        );
        assert_eq!(
            validate_receipt_backed_bundle("moonshine-base-en-int8-onnx", 286_930_831),
            Ok(())
        );
        assert_eq!(
            normalized_install_artifact(&ModelId::new("moonshine-base-en-int8-onnx")),
            Some(NormalizedInstallArtifact::ReceiptBackedBundle {
                bundle_id: "moonshine-base-en-int8-onnx",
                aggregate_size_bytes: 286_930_831,
            })
        );
    }

    #[test]
    fn parakeet_tdt_v2_evidence_records_only_the_observed_windows_gate() {
        let manifest = MODELS
            .iter()
            .find(|manifest| manifest.id == "parakeet-tdt-06b-v2-en-int8-onnx")
            .unwrap();

        assert_eq!(
            manifest.evidence.id,
            "parakeet-tdt-06b-v2-en-int8-onnx-windows-sherpa-1.13.5-gate"
        );
        assert_eq!(manifest.evidence.source, "docs/MANUAL_TEST_MATRIX.md");
        assert!(manifest.evidence.load);
        assert!(manifest.evidence.known_fixture);
        assert!(!manifest.evidence.cancellation);
        assert!(manifest.evidence.unload_reload);
        assert!(!manifest.evidence.acceleration);
        assert!(manifest.evidence.platform);
        assert_eq!(manifest.evidence.receipt, None);
    }

    #[test]
    fn receipt_backed_bundle_authorization_comes_from_catalog_metadata() {
        assert_eq!(
            receipt_backed_bundle_id_for_artifact(
                "synthetic-receipt-backed-bundle",
                NormalizedInstallArtifact::ReceiptBackedBundle {
                    bundle_id: "synthetic-receipt-backed-bundle",
                    aggregate_size_bytes: 1,
                },
            ),
            Some("synthetic-receipt-backed-bundle")
        );
        assert_eq!(
            receipt_backed_bundle_id_for_artifact(
                "synthetic-receipt-backed-bundle",
                NormalizedInstallArtifact::ReceiptBackedBundle {
                    bundle_id: "different-bundle",
                    aggregate_size_bytes: 1,
                },
            ),
            None
        );
        assert_eq!(
            receipt_backed_bundle_id_for_artifact(
                "synthetic-receipt-backed-bundle",
                normalized_install_artifact(&ModelId::new("whisper_cpp_tiny_en")).unwrap(),
            ),
            None
        );
        assert_eq!(
            normalized_receipt_backed_bundle_id(&ModelId::new("unknown-receipt-backed-bundle")),
            None
        );
    }

    #[test]
    fn receipt_backed_bundles_must_match_available_pinned_bundle_metadata() {
        assert_eq!(
            validate_receipt_backed_bundle("moonshine-tiny-en-int8-onnx", 44_256_550),
            Ok(())
        );
        assert_eq!(
            validate_receipt_backed_bundle("parakeet-tdt-06b-v2-en-int8-onnx", 661_190_513),
            Ok(())
        );
        assert!(
            validate_receipt_backed_bundle("unknown-receipt-backed-bundle", 1)
                .unwrap_err()
                .contains("not an available pinned bundle")
        );
        assert!(
            validate_receipt_backed_bundle("parakeet-tdt-ctc-110m-en-int8-onnx", 1)
                .unwrap_err()
                .contains("not an available pinned bundle")
        );
        assert!(
            validate_receipt_backed_bundle("moonshine-tiny-en-int8-onnx", 44_256_549)
                .unwrap_err()
                .contains("does not match its pinned files")
        );
    }

    #[test]
    fn catalog_contains_exactly_one_runtime_handler_candidate() {
        let runtimes: HashSet<_> = MODELS.iter().filter_map(|model| model.runtime).collect();

        assert_eq!(runtimes, HashSet::from([RuntimeRequirement::PrimaryNative]));
    }

    #[test]
    fn all_whisper_models_remain_experimental_without_roles() {
        for descriptor in model_descriptors() {
            assert!(matches!(
                descriptor.compatibility,
                CompatibilityStatus::Experimental { .. }
            ));
            assert!(descriptor.roles.is_empty());
        }
        assert!(model_descriptor(&ModelId::new("whisper_cpp_base_en")).is_some());
    }

    #[test]
    fn neutral_status_and_role_vocabulary_is_complete() {
        let evidence = PHASE_ZERO_SMOKE.link();
        assert_eq!(
            CompatibilityStatus::Incompatible {
                evidence,
                reason: "test",
            }
            .label(),
            "Incompatible"
        );
        assert_eq!(
            CompatibilityStatus::Supported { evidence }.label(),
            "Supported"
        );
        assert_eq!(
            [
                ModelRole::FastEnglish,
                ModelRole::BalancedMultilingual,
                ModelRole::HighAccuracy,
                ModelRole::LowMemory,
            ]
            .len(),
            4
        );
    }

    #[test]
    fn zipformer_gate_fails_closed_without_required_evidence() {
        assert_eq!(
            ZIPFORMER_STREAMING_GATE.decision(),
            EvidenceGateDecision::NoGo
        );
        assert!(
            ZIPFORMER_STREAMING_GATE
                .criteria
                .iter()
                .all(|item| !item.met)
        );
        for required in [
            "pinned-package-and-model",
            "primary-comparator",
            "warm-first-partial",
            "real-time-factor",
            "cancellation",
            "wer",
            "common-contract",
            "recovery-and-resources",
            "windows-platform",
        ] {
            assert!(
                ZIPFORMER_STREAMING_GATE
                    .criteria
                    .iter()
                    .any(|criterion| criterion.name == required),
                "missing {required} evidence criterion"
            );
        }
    }
}
