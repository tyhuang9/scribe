#![cfg_attr(not(test), allow(dead_code))]

use std::collections::HashSet;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::transcription::ModelId;

const WHISPER_CPP_REVISION: &str = "5359861c739e955e79d9a303bcbc70fb988958b1";
const HANDY_COMPUTER_TINY_EN_REVISION: &str = "becb8bcb804405dc97b380a523d9975888820986";
const COMPATIBILITY_EVIDENCE_DOCUMENT: &str = "docs/SCRIBE_REVAMP.md";

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum RuntimeRequirement {
    PrimaryNative,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ModelArchitecture {
    EncoderDecoder,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ModelFormat {
    Ggml,
    Gguf,
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
    description: &'static str,
    storage_guidance: &'static str,
    expected_ram: &'static str,
    speed_guidance: &'static str,
    accuracy_guidance: &'static str,
    recommended: bool,
    runtime: RuntimeRequirement,
    architecture: ModelArchitecture,
    format: ModelFormat,
    minimum_runtime_version: RuntimeVersion,
    artifact: ArtifactManifest,
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
    Supported {
        evidence: EvidenceLink,
    },
    Experimental {
        evidence: EvidenceLink,
        reason: &'static str,
    },
    Incompatible {
        evidence: EvidenceLink,
        reason: &'static str,
    },
}

impl CompatibilityStatus {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Supported { .. } => "Supported",
            Self::Experimental { .. } => "Experimental",
            Self::Incompatible { .. } => "Incompatible",
        }
    }
}

/// Curated user-facing roles. A role is valid only for a Supported model.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ModelRole {
    FastEnglish,
    BalancedMultilingual,
    HighAccuracy,
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
    whisper_manifest(
        "whisper_cpp_base_en",
        "English Base",
        ModelGuidance {
            description: "Local English model with a balanced speed and quality profile.",
            storage: "~150 MB",
            expected_ram: "1 GB",
            speed: "Fast",
            accuracy: "Good",
        },
        "ggml-base.en.bin",
        147_964_211,
        "a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002",
        true,
        PHASE_TWO_BASE_SMOKE,
    ),
    whisper_manifest(
        "whisper_cpp_small_en",
        "English Small",
        ModelGuidance {
            description: "More accurate local English model for longer dictation and clean audio.",
            storage: "~470 MB",
            expected_ram: "2 GB",
            speed: "Medium",
            accuracy: "Better",
        },
        "ggml-small.en.bin",
        487_614_201,
        "c6138d6d58ecc8322097e0f987c32f1be8bb0a18532a3f88f734d1bbf9c41e5d",
        false,
        PHASE_ZERO_SMOKE,
    ),
    whisper_manifest(
        "whisper_cpp_medium_en",
        "English Medium",
        ModelGuidance {
            description: "Higher-accuracy local English model for machines with more memory.",
            storage: "~1.5 GB",
            expected_ram: "5 GB",
            speed: "Slower",
            accuracy: "High",
        },
        "ggml-medium.en.bin",
        1_533_774_781,
        "cc37e93478338ec7700281a7ac30a10128929eb8f427dda2e865faa8f6da4356",
        false,
        PHASE_ZERO_SMOKE,
    ),
];

const fn handy_computer_tiny_en_manifest() -> ModelManifest {
    ModelManifest {
        id: "whisper_cpp_tiny_en",
        display_name: "English Tiny",
        description: "Small English GGUF model for low-resource local dictation.",
        storage_guidance: "~42 MB",
        expected_ram: "1 GB",
        speed_guidance: "Fastest",
        accuracy_guidance: "Basic",
        runtime: RuntimeRequirement::PrimaryNative,
        architecture: ModelArchitecture::EncoderDecoder,
        format: ModelFormat::Gguf,
        minimum_runtime_version: TRANSCRIBE_CPP_VERSION,
        artifact: ArtifactManifest {
            repository: "handy-computer/whisper-tiny.en-gguf",
            revision: HANDY_COMPUTER_TINY_EN_REVISION,
            filename: "whisper-tiny.en-Q4_K_M.gguf",
            size_bytes: 43_545_248,
            sha256: "3bfa6200aa12a21409445401f7871b5c733546dc45a29eb4871fcb3c7954e08b",
        },
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

#[allow(clippy::too_many_arguments)] // Manifest fields stay explicit at the single catalog definition site.
const fn whisper_manifest(
    id: &'static str,
    display_name: &'static str,
    guidance: ModelGuidance,
    filename: &'static str,
    size_bytes: u64,
    sha256: &'static str,
    recommended: bool,
    evidence: CompatibilityEvidence,
) -> ModelManifest {
    ModelManifest {
        id,
        display_name,
        description: guidance.description,
        storage_guidance: guidance.storage,
        expected_ram: guidance.expected_ram,
        speed_guidance: guidance.speed,
        accuracy_guidance: guidance.accuracy,
        recommended,
        runtime: RuntimeRequirement::PrimaryNative,
        architecture: ModelArchitecture::EncoderDecoder,
        format: ModelFormat::Ggml,
        minimum_runtime_version: TRANSCRIBE_CPP_VERSION,
        artifact: ArtifactManifest {
            repository: "ggerganov/whisper.cpp",
            revision: WHISPER_CPP_REVISION,
            filename,
            size_bytes,
            sha256,
        },
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

/// Models available in Scribe's normal user-facing flow. GGML artifacts are
/// deliberately excluded because they require the retained compatibility
/// native package; their descriptors remain available to migrate existing
/// configurations without advertising a second installation architecture.
pub fn normal_model_descriptors() -> Vec<ModelDescriptor> {
    assert_catalog_valid();
    MODELS
        .iter()
        .filter(|manifest| manifest.format == ModelFormat::Gguf)
        .map(ModelManifest::descriptor)
        .collect()
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
        .map(|manifest| RuntimeModelManifest {
            id: manifest.id,
            runtime: manifest.runtime,
            minimum_runtime_version: manifest.minimum_runtime_version,
            artifact_repository: manifest.artifact.repository,
            artifact_revision: manifest.artifact.revision,
            artifact_filename: manifest.artifact.filename,
            artifact_size_bytes: manifest.artifact.size_bytes,
            artifact_storage_estimate: manifest.storage_guidance,
            artifact_sha256: manifest.artifact.sha256,
        })
}

/// Keeps the file-format-to-runtime routing decision inside catalog
/// validation, rather than leaking it into application or installer code.
pub(crate) fn model_uses_embedded_runtime(id: &ModelId) -> bool {
    assert_catalog_valid();
    MODELS
        .iter()
        .find(|manifest| manifest.id == id.as_str())
        .is_some_and(|manifest| manifest.format == ModelFormat::Gguf)
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
            manifest.artifact.repository == repository
                && manifest.artifact.revision == revision
                && manifest.artifact.filename == filename
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

fn assert_catalog_valid() {
    validate_catalog().expect("normalized model catalog must satisfy evidence and integrity rules");
}

impl ModelManifest {
    fn descriptor(&self) -> ModelDescriptor {
        ModelDescriptor {
            id: ModelId::new(self.id),
            display_name: self.display_name,
            description: self.description,
            expected_ram: self.expected_ram,
            speed_guidance: self.speed_guidance,
            accuracy_guidance: self.accuracy_guidance,
            recommended: self.recommended,
            artifact_size_bytes: self.artifact.size_bytes,
            languages: self.languages.to_vec(),
            capabilities: self.capabilities,
            roles: self.roles.to_vec(),
            compatibility: self.compatibility,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EvidenceGateDecision {
    Go,
    NoGo,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EvidenceGateCriterion {
    pub name: &'static str,
    pub requirement: &'static str,
    pub met: bool,
    pub finding: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StreamingCandidateGate {
    pub runtime_version: &'static str,
    pub runtime_commit: &'static str,
    pub model_id: &'static str,
    pub criteria: &'static [EvidenceGateCriterion],
}

impl StreamingCandidateGate {
    pub fn decision(self) -> EvidenceGateDecision {
        if self.criteria.iter().all(|criterion| criterion.met) {
            EvidenceGateDecision::Go
        } else {
            EvidenceGateDecision::NoGo
        }
    }
}

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
        if manifest.minimum_runtime_version
            == (RuntimeVersion {
                major: 0,
                minor: 0,
                patch: 0,
            })
        {
            return Err(format!("{} has no minimum runtime version", manifest.id));
        }
        validate_artifact(manifest.artifact)?;
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
        let (status_evidence, reason) = match manifest.compatibility {
            CompatibilityStatus::Supported { evidence } => (evidence, None),
            CompatibilityStatus::Experimental { evidence, reason }
            | CompatibilityStatus::Incompatible { evidence, reason } => (evidence, Some(reason)),
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
        if matches!(
            manifest.compatibility,
            CompatibilityStatus::Supported { .. }
        ) {
            if !manifest.evidence.complete() {
                return Err(format!(
                    "{} cannot be Supported without complete evidence and a receipt",
                    manifest.id
                ));
            }
            validate_compatibility_receipt(manifest)?;
        }
        if !manifest.roles.is_empty()
            && !matches!(
                manifest.compatibility,
                CompatibilityStatus::Supported { .. }
            )
        {
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
    if document.schema_version != 1
        || document.model_id != manifest.id
        || document.evidence_id != manifest.evidence.id
        || document.runtime_version != runtime_version
        || !document
            .model_artifact_sha256
            .eq_ignore_ascii_case(manifest.artifact.sha256)
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

    #[test]
    fn production_catalog_is_valid_and_has_unique_ids() {
        assert_eq!(validate_catalog(), Ok(()));
        assert_eq!(model_descriptors().len(), 4);
        assert_eq!(normal_model_descriptors().len(), 1);
        assert_eq!(
            normal_model_descriptors()
                .into_iter()
                .map(|descriptor| descriptor.id)
                .collect::<Vec<_>>(),
            vec![ModelId::new("whisper_cpp_tiny_en")]
        );
    }

    #[test]
    fn remote_artifact_must_match_every_pinned_source_fact_to_use_local_installation() {
        let artifact = MODELS[0].artifact;
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
        manifest.artifact.filename = "../model.bin";
        manifest.artifact.sha256 = "not-a-sha";

        assert!(
            validate_manifests(&[manifest])
                .unwrap_err()
                .contains("malformed artifact")
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
        manifest.artifact.sha256 =
            "921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f";
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

        assert_eq!(renamed.runtime, RuntimeRequirement::PrimaryNative);
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
    fn runtime_manifest_exposes_exact_pins_without_family_metadata() {
        let manifest = runtime_model_manifest(&ModelId::new("whisper_cpp_base_en")).unwrap();

        assert_eq!(manifest.id, "whisper_cpp_base_en");
        assert_eq!(manifest.runtime, RuntimeRequirement::PrimaryNative);
        assert_eq!(manifest.minimum_runtime_version, TRANSCRIBE_CPP_VERSION);
        assert_eq!(manifest.artifact_repository, "ggerganov/whisper.cpp");
        assert_eq!(manifest.artifact_revision, WHISPER_CPP_REVISION);
        assert_eq!(manifest.artifact_filename, "ggml-base.en.bin");
        assert_eq!(manifest.artifact_size_bytes, 147_964_211);
        assert_eq!(
            manifest.artifact_sha256,
            "a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002"
        );
    }

    #[test]
    fn primary_download_url_is_derived_from_the_authoritative_manifest() {
        assert_eq!(
            runtime_model_download_url(&ModelId::new("whisper_cpp_base_en")).as_deref(),
            Some(
                "https://huggingface.co/ggerganov/whisper.cpp/resolve/5359861c739e955e79d9a303bcbc70fb988958b1/ggml-base.en.bin"
            )
        );
    }

    #[test]
    fn catalog_contains_exactly_one_runtime_handler_candidate() {
        let runtimes: HashSet<_> = MODELS.iter().map(|model| model.runtime).collect();

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
