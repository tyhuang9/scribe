//! Private, exact ONNX model-bundle catalog and installation receipts.
//!
//! This module is deliberately below `TranscriptionService`. The embedded
//! manifest is the only authority allowed to initiate a remote installation;
//! installed receipts remain self-contained so retired bundles can still be
//! verified and opened without catalog or network access.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::installations::InstallError;
use crate::onnx_worker::{OnnxFileRole, OnnxModelFamily, OnnxModelSpec};

const CATALOG_BYTES: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/resources/onnx-model-bundles-v1.json"
));
const CATALOG_SCHEMA_VERSION: u16 = 1;
const RECEIPT_SCHEMA_VERSION: u16 = 1;
const RECEIPT_FILE_NAME: &str = "install-receipt.json";
const NOTICE_FILE_NAME: &str = "NOTICE.txt";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct BundleCatalog {
    schema_version: u16,
    runtime: RuntimeEvidence,
    bundles: Vec<OnnxBundleManifest>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct RuntimeEvidence {
    name: String,
    version: String,
    source_revision: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BundleAvailability {
    Available,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum CompatibilityEvidence {
    Experimental,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BundleFileRole {
    Model,
    Encoder,
    Decoder,
    Joiner,
    Tokens,
    Preprocessor,
    UncachedDecoder,
    CachedDecoder,
    MergedDecoder,
    License,
}

impl BundleFileRole {
    fn runtime_role(self) -> Option<OnnxFileRole> {
        Some(match self {
            Self::Model => OnnxFileRole::Model,
            Self::Encoder => OnnxFileRole::Encoder,
            Self::Decoder => OnnxFileRole::Decoder,
            Self::Joiner => OnnxFileRole::Joiner,
            Self::Tokens => OnnxFileRole::Tokens,
            Self::Preprocessor => OnnxFileRole::Preprocessor,
            Self::UncachedDecoder => OnnxFileRole::UncachedDecoder,
            Self::CachedDecoder => OnnxFileRole::CachedDecoder,
            Self::MergedDecoder => OnnxFileRole::MergedDecoder,
            Self::License => return None,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct BundleFileManifest {
    pub(crate) role: BundleFileRole,
    pub(crate) path: PathBuf,
    pub(crate) size_bytes: u64,
    pub(crate) sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct CapabilityEvidence {
    decode_mode: String,
    languages: Vec<String>,
    native_streaming: bool,
    notes: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct LicenseEvidence {
    spdx: String,
    copyright: String,
    source_repository: String,
    source_revision: Option<String>,
    notice: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct OnnxBundleManifest {
    pub(crate) id: String,
    pub(crate) availability: BundleAvailability,
    compatibility: CompatibilityEvidence,
    pub(crate) repository: String,
    pub(crate) revision: String,
    pub(crate) family: OnnxModelFamily,
    pub(crate) num_threads: u16,
    #[serde(default)]
    unavailable_reason: Option<String>,
    capability: CapabilityEvidence,
    license: LicenseEvidence,
    pub(crate) files: Vec<BundleFileManifest>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum ReceiptState {
    Verified,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct OnnxBundleReceipt {
    schema_version: u16,
    manifest_schema_version: u16,
    manifest_sha256: String,
    model_id: String,
    runtime: RuntimeEvidence,
    repository: String,
    revision: String,
    family: OnnxModelFamily,
    num_threads: u16,
    files: Vec<BundleFileManifest>,
    capability: CapabilityEvidence,
    license: LicenseEvidence,
    verified_at_unix_seconds: u64,
    state: ReceiptState,
}

fn failed(message: impl Into<String>) -> InstallError {
    InstallError::Failed(message.into())
}

fn parse_catalog(bytes: &[u8]) -> Result<BundleCatalog, InstallError> {
    let catalog: BundleCatalog = serde_json::from_slice(bytes)
        .map_err(|error| failed(format!("invalid embedded ONNX bundle catalog: {error}")))?;
    validate_catalog(&catalog)?;
    Ok(catalog)
}

fn catalog() -> &'static BundleCatalog {
    static CATALOG: OnceLock<BundleCatalog> = OnceLock::new();
    CATALOG.get_or_init(|| {
        parse_catalog(CATALOG_BYTES).expect("embedded ONNX bundle catalog must be valid")
    })
}

pub(crate) fn bundle_manifest(model_id: &str) -> Option<&'static OnnxBundleManifest> {
    catalog()
        .bundles
        .iter()
        .find(|bundle| bundle.id == model_id)
}

pub(crate) fn available_bundle_manifests() -> impl Iterator<Item = &'static OnnxBundleManifest> {
    catalog()
        .bundles
        .iter()
        .filter(|bundle| bundle.availability == BundleAvailability::Available)
}

fn validate_catalog(catalog: &BundleCatalog) -> Result<(), InstallError> {
    if catalog.schema_version != CATALOG_SCHEMA_VERSION {
        return Err(failed(format!(
            "unsupported ONNX bundle catalog schema {}",
            catalog.schema_version
        )));
    }
    validate_runtime_evidence(&catalog.runtime)?;
    let mut ids = HashSet::new();
    for bundle in &catalog.bundles {
        if !ids.insert(bundle.id.as_str()) {
            return Err(failed(format!(
                "ONNX bundle catalog repeats model id {}",
                bundle.id
            )));
        }
        validate_bundle_manifest(bundle)?;
    }
    Ok(())
}

fn validate_runtime_evidence(runtime: &RuntimeEvidence) -> Result<(), InstallError> {
    if runtime.name != "sherpa-onnx" || runtime.version != "1.13.5" {
        return Err(failed(
            "ONNX bundle runtime evidence is not sherpa-onnx 1.13.5",
        ));
    }
    validate_revision(&runtime.source_revision, "runtime source revision")
}

fn validate_bundle_manifest(bundle: &OnnxBundleManifest) -> Result<(), InstallError> {
    validate_stable_id(&bundle.id)?;
    validate_repository(&bundle.repository)?;
    validate_revision(&bundle.revision, "bundle revision")?;
    if bundle.num_threads == 0 || bundle.num_threads > 256 {
        return Err(failed(format!(
            "ONNX bundle {} has an invalid fixed thread count",
            bundle.id
        )));
    }
    if bundle.capability.languages.is_empty()
        || bundle
            .capability
            .languages
            .iter()
            .any(|language| language != "en")
        || bundle.capability.notes.trim().is_empty()
    {
        return Err(failed(format!(
            "ONNX bundle {} makes an unsupported language/capability claim",
            bundle.id
        )));
    }
    if bundle.license.spdx.trim().is_empty()
        || bundle.license.copyright.trim().is_empty()
        || bundle.license.notice.trim().is_empty()
    {
        return Err(failed(format!(
            "ONNX bundle {} has incomplete license evidence",
            bundle.id
        )));
    }
    validate_repository(&bundle.license.source_repository)?;
    if let Some(revision) = &bundle.license.source_revision {
        validate_revision(revision, "license source revision")?;
    }
    match bundle.availability {
        BundleAvailability::Available => {
            if bundle.files.is_empty() || bundle.unavailable_reason.is_some() {
                return Err(failed(format!(
                    "available ONNX bundle {} has no exact files or has an unavailable reason",
                    bundle.id
                )));
            }
        }
        BundleAvailability::Unavailable => {
            if !bundle.files.is_empty()
                || bundle
                    .unavailable_reason
                    .as_deref()
                    .is_none_or(|reason| reason.trim().is_empty())
            {
                return Err(failed(format!(
                    "unavailable ONNX bundle {} must have a reason and no files",
                    bundle.id
                )));
            }
            return Ok(());
        }
    }

    let mut paths = HashSet::new();
    let mut roles = BTreeSet::new();
    for file in &bundle.files {
        validate_relative_path(&file.path)?;
        if file.path == Path::new(RECEIPT_FILE_NAME) || file.path == Path::new(NOTICE_FILE_NAME) {
            return Err(failed(format!(
                "ONNX bundle {} reserves file path {}",
                bundle.id,
                file.path.display()
            )));
        }
        if file.size_bytes == 0 {
            return Err(failed(format!(
                "ONNX bundle {} file {} has zero size",
                bundle.id,
                file.path.display()
            )));
        }
        validate_sha256(&file.sha256)?;
        let normalized = file.path.to_string_lossy().to_ascii_lowercase();
        if !paths.insert(normalized) {
            return Err(failed(format!(
                "ONNX bundle {} repeats a case-insensitive path",
                bundle.id
            )));
        }
        if !roles.insert(file.role) {
            return Err(failed(format!(
                "ONNX bundle {} repeats file role {:?}",
                bundle.id, file.role
            )));
        }
    }
    validate_family_layout(bundle)
}

fn validate_family_layout(bundle: &OnnxBundleManifest) -> Result<(), InstallError> {
    let roles = bundle
        .files
        .iter()
        .filter_map(|file| file.role.runtime_role())
        .collect::<BTreeSet<_>>();
    let expected = match bundle.family {
        OnnxModelFamily::Moonshine => [
            OnnxFileRole::Encoder,
            OnnxFileRole::MergedDecoder,
            OnnxFileRole::Tokens,
        ]
        .into_iter()
        .collect(),
        OnnxModelFamily::NemoCtc => [OnnxFileRole::Model, OnnxFileRole::Tokens]
            .into_iter()
            .collect(),
        OnnxModelFamily::Canary => [
            OnnxFileRole::Encoder,
            OnnxFileRole::Decoder,
            OnnxFileRole::Tokens,
        ]
        .into_iter()
        .collect(),
        OnnxModelFamily::OfflineTransducer | OnnxModelFamily::OnlineTransducer => [
            OnnxFileRole::Encoder,
            OnnxFileRole::Decoder,
            OnnxFileRole::Joiner,
            OnnxFileRole::Tokens,
        ]
        .into_iter()
        .collect(),
    };
    if roles != expected {
        return Err(failed(format!(
            "ONNX bundle {} has the wrong typed role layout for {:?}",
            bundle.id, bundle.family
        )));
    }
    let expected_streaming = bundle.family == OnnxModelFamily::OnlineTransducer;
    if bundle.capability.native_streaming != expected_streaming {
        return Err(failed(format!(
            "ONNX bundle {} has inconsistent streaming evidence",
            bundle.id
        )));
    }
    Ok(())
}

fn validate_stable_id(value: &str) -> Result<(), InstallError> {
    let valid = !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !value.starts_with('-')
        && !value.ends_with('-');
    if valid {
        Ok(())
    } else {
        Err(failed("ONNX bundle id is not a safe stable identifier"))
    }
}

fn validate_repository(value: &str) -> Result<(), InstallError> {
    let mut parts = value.split('/');
    let valid_part = |part: &str| {
        !part.is_empty()
            && part.len() <= 96
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
            && part != "."
            && part != ".."
    };
    if parts.next().is_some_and(valid_part)
        && parts.next().is_some_and(valid_part)
        && parts.next().is_none()
    {
        Ok(())
    } else {
        Err(failed(format!(
            "unsafe Hugging Face repository identifier {value:?}"
        )))
    }
}

fn validate_revision(value: &str, label: &str) -> Result<(), InstallError> {
    if value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(failed(format!(
            "{label} must be a full lowercase Git commit"
        )))
    }
}

fn validate_sha256(value: &str) -> Result<(), InstallError> {
    if value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(failed(
            "ONNX bundle SHA-256 must be 64 lowercase hex characters",
        ))
    }
}

fn validate_relative_path(path: &Path) -> Result<(), InstallError> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        || path.to_string_lossy().contains('\0')
        || path.to_str().is_none()
    {
        return Err(failed(format!(
            "unsafe ONNX bundle relative path {}",
            path.display()
        )));
    }
    Ok(())
}

fn embedded_manifest_sha256() -> String {
    format!("{:x}", Sha256::digest(CATALOG_BYTES))
}

fn spec_from_parts(
    model_id: &str,
    root: PathBuf,
    family: OnnxModelFamily,
    num_threads: u16,
    files: &[BundleFileManifest],
) -> Result<OnnxModelSpec, InstallError> {
    let mut runtime_files = BTreeMap::new();
    for file in files {
        validate_relative_path(&file.path)?;
        if let Some(role) = file.role.runtime_role()
            && runtime_files.insert(role, file.path.clone()).is_some()
        {
            return Err(failed(format!(
                "ONNX receipt repeats runtime role {:?}",
                file.role
            )));
        }
    }
    Ok(OnnxModelSpec {
        id: model_id.to_owned(),
        root,
        family,
        files: runtime_files,
        num_threads,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_catalog_has_exact_first_wave_evidence() {
        let catalog = parse_catalog(CATALOG_BYTES).unwrap();
        assert_eq!(catalog.bundles.len(), 4);
        assert_eq!(available_bundle_manifests().count(), 3);
        let moonshine = bundle_manifest("moonshine-tiny-en-int8-onnx").unwrap();
        assert_eq!(
            moonshine.revision,
            "d1e6c30921780b8508d04b492dfb3ce8a51605d4"
        );
        assert_eq!(moonshine.files.len(), 4);
        let parakeet = bundle_manifest("parakeet-tdt-ctc-110m-en-int8-onnx").unwrap();
        assert_eq!(parakeet.availability, BundleAvailability::Unavailable);
        assert!(parakeet.files.is_empty());
        let canary = bundle_manifest("canary-180m-flash-int8-onnx").unwrap();
        assert_eq!(canary.capability.languages, ["en"]);
        assert!(!canary.capability.native_streaming);
        let zipformer = bundle_manifest("zipformer-streaming-en-20m-int8-onnx").unwrap();
        assert!(zipformer.capability.native_streaming);
        assert_eq!(
            catalog.runtime.source_revision,
            "3dc7c569f31ca2cd4a20ed6f7db780327e6714c5"
        );
    }

    #[test]
    fn catalog_rejects_unsafe_and_ambiguous_authority() {
        let mut catalog = parse_catalog(CATALOG_BYTES).unwrap();
        catalog.bundles[0].repository = "owner/repo/extra".to_owned();
        assert!(validate_catalog(&catalog).is_err());
        let mut catalog = parse_catalog(CATALOG_BYTES).unwrap();
        catalog.bundles[0].revision = "main".to_owned();
        assert!(validate_catalog(&catalog).is_err());
        let mut catalog = parse_catalog(CATALOG_BYTES).unwrap();
        catalog.bundles[0].files[0].path = PathBuf::from("../escape.onnx");
        assert!(validate_catalog(&catalog).is_err());
        let mut catalog = parse_catalog(CATALOG_BYTES).unwrap();
        catalog.bundles[0].files[1].path = catalog.bundles[0].files[0].path.clone();
        assert!(validate_catalog(&catalog).is_err());
        let mut catalog = parse_catalog(CATALOG_BYTES).unwrap();
        catalog.bundles[0].files[0].sha256 = "A".repeat(64);
        assert!(validate_catalog(&catalog).is_err());
        let mut catalog = parse_catalog(CATALOG_BYTES).unwrap();
        catalog.bundles[0].files[0].size_bytes = 0;
        assert!(validate_catalog(&catalog).is_err());
    }

    #[test]
    fn license_role_is_never_exposed_to_the_runtime_spec() {
        let bundle = bundle_manifest("moonshine-tiny-en-int8-onnx").unwrap();
        let spec = spec_from_parts(
            &bundle.id,
            PathBuf::from("bundle"),
            bundle.family,
            bundle.num_threads,
            &bundle.files,
        )
        .unwrap();
        assert_eq!(spec.files.len(), 3);
        assert!(!spec.files.values().any(|path| path == Path::new("LICENSE")));
    }

    #[test]
    fn manifest_digest_is_stable_and_lowercase_sha256() {
        let digest = embedded_manifest_sha256();
        validate_sha256(&digest).unwrap();
    }
}
