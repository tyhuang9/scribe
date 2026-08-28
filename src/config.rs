use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Result, anyhow};
use directories::ProjectDirs;
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::model_catalog::BUNDLED_BASE_MODEL_ID;
use crate::model_catalog::runtime_model_manifest;
use crate::models::{ModelArtifactOrigin, ModelInstallStatus, SttModelInfo, default_model_catalog};
#[cfg(test)]
use crate::transcription::AccelerationPreference;
use crate::transcription::ModelId;

#[path = "settings/mod.rs"]
pub mod settings;

pub use settings::{
    AppConfig, CURRENT_SCHEMA_VERSION, GeneralSettings, HistoryMode, OverlayMode, OverlayPosition,
    RecordingSettings, SettingsStore, SpeechDetectionMode, StreamingMode,
};

pub const MAX_RECORDING_SECONDS: u32 = 600;
pub(crate) const RECORDING_CAPTURE_SAFETY_ALLOWANCE_SECONDS: u32 = 2;
pub const MAX_HISTORY_ENTRIES: u32 = 1_000;
pub const MAX_HISTORY_RETENTION_DAYS: u32 = 3_650;

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct ManagedModelInstall {
    pub path: PathBuf,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub installed_at_unix_seconds: Option<u64>,
    #[serde(flatten)]
    pub unknown: BTreeMap<String, Value>,
}

/// A Scribe-managed GGUF selected from the trusted Hugging Face catalog.
///
/// This is intentionally separate from `ManagedModelInstall`: that map is a
/// compatibility record for the static catalog and normalizes unknown IDs
/// away. Remote artifacts retain the exact immutable Hub source that was
/// verified before activation, so they can be loaded without treating a
/// user-supplied path or URL as a model definition.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ManagedRemoteModelInstall {
    pub repository: String,
    pub revision: String,
    pub filename: String,
    pub expected_size_bytes: u64,
    pub expected_sha256: String,
    pub path: PathBuf,
    pub display_name: String,
    pub description: String,
    pub languages: Vec<String>,
    pub recommended: bool,
    pub installed_at_unix_seconds: Option<u64>,
}

/// A user-selected local GGUF that Scribe has hashed and smoke-validated in
/// place. It deliberately has no Hub source, is never treated as app-owned,
/// and must therefore never be removed from disk by Scribe.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ImportedGgufModelInstall {
    pub path: PathBuf,
    pub size_bytes: u64,
    pub sha256: String,
    pub display_name: String,
    pub imported_at_unix_seconds: Option<u64>,
}

impl ImportedGgufModelInstall {
    pub fn validated(path: PathBuf, size_bytes: u64, sha256: String, display_name: String) -> Self {
        Self {
            path,
            size_bytes,
            sha256: sha256.to_ascii_lowercase(),
            display_name,
            imported_at_unix_seconds: current_unix_seconds(),
        }
    }
}

impl ManagedRemoteModelInstall {
    pub fn trusted(
        artifact: RemoteGgufArtifact,
        path: PathBuf,
        display_name: String,
        description: String,
        languages: Vec<String>,
        recommended: bool,
    ) -> Self {
        Self {
            repository: artifact.repository,
            revision: artifact.revision,
            filename: artifact.filename,
            expected_size_bytes: artifact.expected_size_bytes,
            expected_sha256: artifact.expected_sha256.to_ascii_lowercase(),
            path,
            display_name,
            description,
            languages,
            recommended,
            installed_at_unix_seconds: current_unix_seconds(),
        }
    }
}

#[derive(Default)]
struct ManagedInstallFields {
    path: PathBuf,
    source: Option<String>,
    version: Option<String>,
    sha256: Option<String>,
    platform: Option<String>,
    installed_at_unix_seconds: Option<u64>,
    unknown: BTreeMap<String, Value>,
}

fn deserialize_managed_install<'de, D>(deserializer: D) -> Result<ManagedInstallFields, D::Error>
where
    D: Deserializer<'de>,
{
    let mut values = BTreeMap::<String, Value>::deserialize(deserializer)?;
    let path = values
        .remove("path")
        .ok_or_else(|| D::Error::missing_field("path"))
        .and_then(|value| serde_json::from_value(value).map_err(D::Error::custom))?;
    let source = values
        .remove("source")
        .and_then(|value| serde_json::from_value(value).ok());
    let version = values
        .remove("version")
        .and_then(|value| serde_json::from_value(value).ok());
    let sha256 = values
        .remove("sha256")
        .and_then(|value| serde_json::from_value(value).ok());
    let platform = values
        .remove("platform")
        .and_then(|value| serde_json::from_value(value).ok());
    let installed_at_unix_seconds = values
        .remove("installed_at_unix_seconds")
        .and_then(|value| serde_json::from_value(value).ok());
    Ok(ManagedInstallFields {
        path,
        source,
        version,
        sha256,
        platform,
        installed_at_unix_seconds,
        unknown: values,
    })
}

impl<'de> Deserialize<'de> for ManagedModelInstall {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let fields = deserialize_managed_install(deserializer)?;
        Ok(Self {
            path: fields.path,
            source: fields.source,
            version: fields.version,
            sha256: fields.sha256,
            platform: fields.platform,
            installed_at_unix_seconds: fields.installed_at_unix_seconds,
            unknown: fields.unknown,
        })
    }
}

impl ManagedModelInstall {
    pub fn app_managed(path: PathBuf, source: &str) -> Self {
        Self {
            path,
            source: Some(source.to_owned()),
            platform: Some(current_platform_key()),
            installed_at_unix_seconds: current_unix_seconds(),
            ..Self::default()
        }
    }
}

pub fn current_platform_key() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

fn current_unix_seconds() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum ThemeMode {
    #[default]
    Light,
    Dark,
    System,
}

impl ThemeMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Light => "Light",
            Self::Dark => "Dark",
            Self::System => "System",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HotkeyMode {
    #[default]
    Toggle,
    HoldToTalk,
}

pub fn project_dirs() -> Result<ProjectDirs> {
    scribe_project_dirs()
}

fn scribe_project_dirs() -> Result<ProjectDirs> {
    ProjectDirs::from("com", "Scribe", "Scribe")
        .ok_or_else(|| anyhow!("could not resolve a platform config directory"))
}

fn legacy_project_dirs() -> Result<ProjectDirs> {
    ProjectDirs::from("com", "Local Transcriber", "Local Transcriber")
        .ok_or_else(|| anyhow!("could not resolve a platform config directory"))
}

pub fn config_file_path() -> Result<PathBuf> {
    Ok(project_dirs()?.config_dir().join("config.json"))
}

pub fn cache_dir() -> Result<PathBuf> {
    Ok(project_dirs()?.cache_dir().to_path_buf())
}

pub fn load_config() -> Result<(AppConfig, PathBuf)> {
    let path = config_file_path()?;
    if !path.exists() {
        if let Ok(legacy_path) = legacy_config_file_path()
            && legacy_path.exists()
        {
            let mut config = read_config_file(&legacy_path)?;
            normalize_config(&mut config);
            save_config(&config)?;
            return Ok((config, path));
        }

        let config = AppConfig::default();
        save_config(&config)?;
        return Ok((config, path));
    }

    let mut config = read_config_file(&path)?;
    normalize_config(&mut config);
    Ok((config, path))
}

fn legacy_config_file_path() -> Result<PathBuf> {
    Ok(legacy_project_dirs()?.config_dir().join("config.json"))
}

fn read_config_file(path: &Path) -> Result<AppConfig> {
    settings::load_from_path(path)
}

pub fn save_config(config: &AppConfig) -> Result<()> {
    let path = config_file_path()?;
    settings::save_to_path(&path, config)
}

pub fn configured_models(config: &AppConfig) -> Vec<SttModelInfo> {
    configured_models_with_bundled_path(config, bundled_model_path())
}

pub(crate) fn onnx_bundle_storage_dir(config: &AppConfig) -> PathBuf {
    model_storage_dir(config).join("onnx-bundles")
}

pub(crate) fn installed_onnx_bundle_root(
    config: &AppConfig,
    model_id: &ModelId,
) -> Option<PathBuf> {
    crate::model_catalog::normalized_receipt_backed_bundle_id(model_id)?;
    let root = onnx_bundle_target_root(config, model_id)?;
    // This is routing metadata only. Receipt/tree integrity is established by
    // `TranscriptionService::onnx_artifact_from_receipt` immediately before
    // native dispatch; inventory construction must not become an execution
    // authorization cache or perform a second full tree verification.
    (root.is_dir() && root.join("install-receipt.json").is_file()).then_some(root)
}

pub(crate) fn onnx_bundle_target_root(config: &AppConfig, model_id: &ModelId) -> Option<PathBuf> {
    let crate::model_catalog::NormalizedInstallArtifact::ReceiptBackedBundle { bundle_id, .. } =
        crate::model_catalog::normalized_install_artifact(model_id)?
    else {
        return None;
    };
    (bundle_id == model_id.as_str()).then(|| onnx_bundle_storage_dir(config).join(bundle_id))
}

fn configured_models_with_bundled_path(
    config: &AppConfig,
    bundled_base_path: Option<PathBuf>,
) -> Vec<SttModelInfo> {
    let mut models = default_model_catalog()
        .into_iter()
        .map(|mut model| {
            let model_id = ModelId::new(&model.id);
            if matches!(
                crate::model_catalog::normalized_install_artifact(&model_id),
                Some(crate::model_catalog::NormalizedInstallArtifact::ReceiptBackedBundle { .. })
            ) {
                let root = crate::onnx_model_bundles::bundle_target_root(
                    &onnx_bundle_storage_dir(config),
                    &model.id,
                )
                .expect("normalized ONNX model id is a stable bundle id");
                let installed = installed_onnx_bundle_root(config, &model_id).is_some();
                model.local_path = root.exists().then_some(root);
                model.artifact_origin = ModelArtifactOrigin::Managed;
                model.install_status = if installed {
                    ModelInstallStatus::Installed
                } else if model.local_path.is_some() {
                    ModelInstallStatus::Missing
                } else {
                    ModelInstallStatus::NotInstalled
                };
                return model;
            }
            let bundled_path = (model.id == BUNDLED_BASE_MODEL_ID
                && !config
                    .general
                    .excluded_bundled_model_ids
                    .iter()
                    .any(|id| id == &model.id))
            .then(|| bundled_base_path.clone())
            .flatten();
            let configured_path = config
                .general
                .model_paths
                .get(&model.id)
                .filter(|path| current_pinned_gguf_path(&model_id, path))
                .cloned();
            let managed_path = managed_model_path(config, &model)
                .filter(|path| current_pinned_gguf_path(&model_id, path));
            let downloaded_path = downloaded_model_path(config, &model);
            let explicit_path =
                first_non_empty_path([managed_path.clone(), configured_path.clone()]);
            let mut candidate_paths = [
                managed_path.clone(),
                configured_path.clone(),
                downloaded_path.clone(),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
            if let Some(path) = bundled_path.as_ref() {
                let verified_managed_primary = config
                    .general
                    .managed_models
                    .get(&model.id)
                    .is_some_and(|install| {
                        managed_path.as_ref() == Some(&install.path)
                            && install.source.as_deref() == Some("verified-manifest-download")
                            && runtime_model_manifest(&ModelId::new(&model.id)).is_some_and(
                                |manifest| {
                                    install.sha256.as_deref().is_some_and(|sha256| {
                                        sha256.eq_ignore_ascii_case(manifest.artifact_sha256)
                                    })
                                },
                            )
                    });
                if !verified_managed_primary {
                    candidate_paths.insert(0, path.clone());
                }
            }
            dedup_paths_preserving_order(&mut candidate_paths);

            let installed_path = first_valid_model_path(&model, candidate_paths.iter().cloned());
            let existing_invalid_path = first_existing_path(candidate_paths.iter().cloned());
            model.local_path = installed_path
                .clone()
                .or(existing_invalid_path)
                .or(explicit_path)
                .or_else(|| bundled_path.clone());
            model.artifact_origin = match model.local_path.as_ref() {
                Some(path) if bundled_path.as_ref() == Some(path) => ModelArtifactOrigin::Bundled,
                Some(path)
                    if managed_path.as_ref() == Some(path)
                        || downloaded_path.as_ref() == Some(path) =>
                {
                    ModelArtifactOrigin::Managed
                }
                Some(_) => ModelArtifactOrigin::External,
                None => ModelArtifactOrigin::Catalog,
            };
            model.install_status = if installed_path.is_some() {
                ModelInstallStatus::Installed
            } else if model.local_path.is_some() {
                ModelInstallStatus::Missing
            } else {
                ModelInstallStatus::NotInstalled
            };
            model
        })
        .collect::<Vec<_>>();
    let mut managed_remote_models = config
        .general
        .managed_remote_models
        .iter()
        .filter(|(id, install)| valid_managed_remote_model(config, id, install))
        .map(|(id, install)| remote_model_info(id, install))
        .collect::<Vec<_>>();
    managed_remote_models.sort_by(|left, right| left.id.cmp(&right.id));
    models.extend(managed_remote_models);

    let mut imported_gguf_models = config
        .general
        .imported_gguf_models
        .iter()
        .filter(|(id, install)| valid_imported_gguf_model(id, install))
        .map(|(id, install)| imported_gguf_model_info(id, install))
        .collect::<Vec<_>>();
    imported_gguf_models.sort_by(|left, right| left.id.cmp(&right.id));
    models.extend(imported_gguf_models);
    models
}

fn current_pinned_gguf_path(model_id: &ModelId, path: &Path) -> bool {
    runtime_model_manifest(model_id).is_some_and(|manifest| {
        path.file_name().and_then(|name| name.to_str()) == Some(manifest.artifact_filename)
            && manifest.artifact_filename.ends_with(".gguf")
    })
}

/// Stable opaque IDs ensure a catalog display name or filename cannot be used
/// to overwrite a different remote artifact. The full immutable source is
/// still persisted and revalidated before every use.
pub fn managed_remote_model_id(repository: &str, revision: &str, filename: &str) -> Option<String> {
    if !valid_remote_artifact_metadata(
        repository,
        revision,
        filename,
        1,
        "0000000000000000000000000000000000000000000000000000000000000000",
    ) {
        return None;
    }
    let digest = format!(
        "{:x}",
        Sha256::digest(format!("{repository}\n{revision}\n{filename}").as_bytes())
    );
    Some(format!("hf-{}", &digest[..24]))
}

/// Content-addressed IDs prevent a path or display-name change from replacing
/// a different imported artifact. The local file remains external to Scribe's
/// managed storage and is reverified by the embedded runtime before use.
pub fn imported_gguf_model_id(sha256: &str) -> Option<String> {
    let sha256 = sha256.trim();
    (sha256.len() == 64 && sha256.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| format!("local-{}", sha256.to_ascii_lowercase()))
}

/// The exact source and integrity facts used by the common runtime. This is
/// not a download request and never carries a remote URL.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RemoteGgufArtifact {
    pub(crate) repository: String,
    pub(crate) revision: String,
    pub(crate) filename: String,
    pub(crate) expected_size_bytes: u64,
    pub(crate) expected_sha256: String,
}

/// The locally observed facts for an imported GGUF. These are not a trusted
/// remote checksum claim; they are rechecked as the runtime's expected bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ImportedGgufArtifact {
    pub(crate) expected_size_bytes: u64,
    pub(crate) expected_sha256: String,
}

pub(crate) fn remote_gguf_artifact(
    config: &AppConfig,
    model_id: &str,
) -> Option<RemoteGgufArtifact> {
    let install = config.general.managed_remote_models.get(model_id)?;
    valid_managed_remote_model(config, model_id, install).then(|| RemoteGgufArtifact {
        repository: install.repository.clone(),
        revision: install.revision.clone(),
        filename: install.filename.clone(),
        expected_size_bytes: install.expected_size_bytes,
        expected_sha256: install.expected_sha256.clone(),
    })
}

pub(crate) fn managed_remote_model_path(config: &AppConfig, model_id: &str) -> Option<PathBuf> {
    let install = config.general.managed_remote_models.get(model_id)?;
    valid_managed_remote_model(config, model_id, install).then(|| install.path.clone())
}

pub(crate) fn imported_gguf_artifact(
    config: &AppConfig,
    model_id: &str,
) -> Option<ImportedGgufArtifact> {
    let install = config.general.imported_gguf_models.get(model_id)?;
    valid_imported_gguf_model(model_id, install).then(|| ImportedGgufArtifact {
        expected_size_bytes: install.size_bytes,
        expected_sha256: install.sha256.clone(),
    })
}

fn remote_model_info(id: &str, install: &ManagedRemoteModelInstall) -> SttModelInfo {
    let file_matches = install.path.metadata().ok().is_some_and(|metadata| {
        metadata.is_file() && metadata.len() == install.expected_size_bytes
    });
    SttModelInfo {
        id: id.to_owned(),
        name: install.display_name.clone(),
        backend: "transcribe-cpp".to_owned(),
        description: install.description.clone(),
        expected_ram: "Not measured".to_owned(),
        accuracy_tier: "Runtime-validated".to_owned(),
        speed_tier: "Not measured".to_owned(),
        local_path: Some(install.path.clone()),
        artifact_origin: ModelArtifactOrigin::Managed,
        install_status: if file_matches {
            ModelInstallStatus::Installed
        } else {
            ModelInstallStatus::Missing
        },
        download_model: None,
    }
}

fn imported_gguf_model_info(id: &str, install: &ImportedGgufModelInstall) -> SttModelInfo {
    SttModelInfo {
        id: id.to_owned(),
        name: install.display_name.clone(),
        backend: "transcribe-cpp".to_owned(),
        description: "Local GGUF imported after Scribe hash and runtime validation.".to_owned(),
        expected_ram: "Not measured".to_owned(),
        accuracy_tier: "Runtime-validated local import".to_owned(),
        speed_tier: "Not measured".to_owned(),
        local_path: Some(install.path.clone()),
        artifact_origin: ModelArtifactOrigin::Imported,
        install_status: ModelInstallStatus::Installed,
        download_model: None,
    }
}

pub fn playground_selected_installed_models(config: &AppConfig) -> Vec<SttModelInfo> {
    let mut models = configured_models(config)
        .into_iter()
        .filter(|model| model.install_status.is_runnable())
        .map(|model| (model.id.clone(), model))
        .collect::<HashMap<_, _>>();

    config
        .general
        .playground_model_order
        .iter()
        .filter(|id| {
            config
                .general
                .playground_selected_models
                .iter()
                .any(|selected| selected == *id)
        })
        .filter_map(|id| models.remove(id))
        .collect()
}

pub fn model_storage_dir(config: &AppConfig) -> PathBuf {
    if config.general.model_storage_dir.as_os_str().is_empty() {
        default_model_storage_dir()
    } else {
        config.general.model_storage_dir.clone()
    }
}

/// Resolves the release-bundled base model directly beside the executable.
/// The path is never persisted or copied into writable application storage.
pub(crate) fn bundled_model_path() -> Option<PathBuf> {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        std::env::current_exe()
            .ok()
            .and_then(|executable| bundled_model_path_for_executable(&executable))
    }
    #[cfg(not(all(target_os = "windows", target_arch = "x86_64")))]
    {
        None
    }
}

fn bundled_model_path_for_executable(executable: &Path) -> Option<PathBuf> {
    let manifest = runtime_model_manifest(&ModelId::new(BUNDLED_BASE_MODEL_ID))?;
    executable
        .parent()
        .map(|directory| directory.join(manifest.artifact_filename))
}

pub fn artifact_state_storage_dir() -> PathBuf {
    scribe_project_dirs()
        .map(|dirs| dirs.data_dir().join("artifact-state"))
        .unwrap_or_else(|_| PathBuf::from("artifact-state"))
}

pub fn history_storage_dir() -> Result<PathBuf> {
    Ok(scribe_project_dirs()?.data_dir().join("history"))
}

pub fn managed_model_path(config: &AppConfig, model: &SttModelInfo) -> Option<PathBuf> {
    config
        .general
        .managed_models
        .get(&model.id)
        .map(|install| install.path.clone())
        .filter(|path| !path.as_os_str().is_empty())
}

pub fn is_valid_model_install_path(model: &SttModelInfo, path: &Path) -> bool {
    match model.backend.as_str() {
        "whisper.cpp" | "transcribe-cpp" => path.is_file(),
        // Receipt-backed ONNX bundles are validated before reaching this
        // compatibility helper; unsupported legacy providers stay inert.
        _ => false,
    }
}

pub fn downloaded_model_path(config: &AppConfig, model: &SttModelInfo) -> Option<PathBuf> {
    if matches!(
        crate::model_catalog::normalized_install_artifact(&ModelId::new(&model.id)),
        Some(crate::model_catalog::NormalizedInstallArtifact::ReceiptBackedBundle { .. })
    ) {
        return crate::onnx_model_bundles::bundle_target_root(
            &onnx_bundle_storage_dir(config),
            &model.id,
        )
        .ok();
    }
    let download_model = model.download_model.as_ref()?;
    match model.backend.as_str() {
        "whisper.cpp" if download_model.ends_with(".gguf") => {
            Some(model_storage_dir(config).join("gguf").join(download_model))
        }
        _ => None,
    }
}

pub fn normalize_config(config: &mut AppConfig) {
    config
        .general
        .excluded_bundled_model_ids
        .retain(|id| id == BUNDLED_BASE_MODEL_ID);
    dedup_preserving_order(&mut config.general.excluded_bundled_model_ids);
    if config.general.model_storage_dir.as_os_str().is_empty() {
        config.general.model_storage_dir = default_model_storage_dir();
    }
    apply_managed_model_metadata(config);
    normalize_managed_remote_models(config);
    normalize_imported_gguf_models(config);
    let catalog = configured_models(config);
    let catalog_ids = catalog
        .iter()
        .map(|model| model.id.clone())
        .collect::<Vec<_>>();
    if let Some(device_name) = &config.recording.audio_input_device_name
        && device_name.trim().is_empty()
    {
        config.recording.audio_input_device_name = None;
    }
    let bundled_base_is_excluded = config
        .general
        .excluded_bundled_model_ids
        .iter()
        .any(|id| id == BUNDLED_BASE_MODEL_ID);
    let selected_is_current = catalog_ids
        .iter()
        .any(|id| id == &config.general.selected_default_model)
        && !(bundled_base_is_excluded
            && config.general.selected_default_model == BUNDLED_BASE_MODEL_ID);
    if !config.general.selected_default_model.is_empty() && !selected_is_current {
        config.general.selected_default_model = if bundled_base_is_excluded {
            String::new()
        } else {
            BUNDLED_BASE_MODEL_ID.to_owned()
        };
    }

    config.general.playground_selected_models.retain(|id| {
        catalog_ids.iter().any(|catalog_id| catalog_id == id)
            && !(bundled_base_is_excluded && id == BUNDLED_BASE_MODEL_ID)
    });
    dedup_preserving_order(&mut config.general.playground_selected_models);

    normalize_playground_order(config, &catalog_ids);

    if config.recording.max_recording_seconds == 0 {
        config.recording.max_recording_seconds = 30;
    }
    config.recording.max_recording_seconds = config
        .recording
        .max_recording_seconds
        .min(MAX_RECORDING_SECONDS);
    config.recording.speech_confirmation_ms =
        config.recording.speech_confirmation_ms.clamp(50, 1_000);
    if !config.recording.input_threshold_dbfs.is_finite() {
        config.recording.input_threshold_dbfs = settings::DEFAULT_INPUT_THRESHOLD_DBFS;
    }
    config.recording.input_threshold_dbfs = config.recording.input_threshold_dbfs.clamp(
        settings::MIN_INPUT_THRESHOLD_DBFS,
        settings::MAX_INPUT_THRESHOLD_DBFS,
    );
    config.recording.internal_pause_ms = config
        .recording
        .internal_pause_ms
        .clamp(config.recording.speech_confirmation_ms.max(100), 3_000);
    config.recording.endpoint_silence_ms = config
        .recording
        .endpoint_silence_ms
        .clamp(config.recording.internal_pause_ms.max(300), 5_000);
    config.recording.pre_roll_ms = config.recording.pre_roll_ms.min(2_000);
    config.recording.post_roll_ms = config.recording.post_roll_ms.min(2_000);
    config.history.max_unpinned_entries = config
        .history
        .max_unpinned_entries
        .clamp(1, MAX_HISTORY_ENTRIES);
    config.history.transcript_retention_days = config
        .history
        .transcript_retention_days
        .filter(|days| *days > 0)
        .map(|days| days.min(MAX_HISTORY_RETENTION_DAYS));
    config.history.audio_retention_days = config
        .history
        .audio_retention_days
        .filter(|days| *days > 0)
        .map(|days| days.min(MAX_HISTORY_RETENTION_DAYS));
    if config.output.paste_delay_ms == 0 {
        config.output.paste_delay_ms = default_paste_delay_ms();
    }
}

fn default_paste_delay_ms() -> u64 {
    75
}

fn default_model_storage_dir() -> PathBuf {
    scribe_project_dirs()
        .map(|dirs| dirs.data_dir().join("models"))
        .unwrap_or_else(|_| PathBuf::from("models"))
}

fn default_playground_model_order() -> Vec<String> {
    default_model_catalog()
        .into_iter()
        .map(|model| model.id)
        .collect()
}

fn apply_managed_model_metadata(config: &mut AppConfig) {
    apply_managed_model_metadata_with_path_probe(config, Path::exists);
}

fn apply_managed_model_metadata_with_path_probe(
    config: &mut AppConfig,
    mut path_exists: impl FnMut(&Path) -> bool,
) {
    let expected_paths = default_model_catalog()
        .into_iter()
        .filter_map(|model| {
            downloaded_model_path(config, &model).map(|path| (model.id.clone(), vec![path]))
        })
        .collect::<HashMap<_, _>>();
    let storage_dir = model_storage_dir(config);
    config.general.managed_models.retain(|id, install| {
        let Some(paths) = expected_paths.get(id) else {
            // Unsupported and retired records are intentionally inert. Keep
            // them byte-for-byte at the settings layer so users can downgrade
            // or inspect an old profile without Scribe rewriting their legacy
            // installation metadata or touching the referenced artifacts.
            return true;
        };
        paths.contains(&install.path) && safe_managed_model_path(&storage_dir, &install.path)
    });

    for (id, path) in &config.general.model_paths {
        let Some(paths) = expected_paths.get(id) else {
            continue;
        };
        if !paths.contains(path) {
            continue;
        }
        if !path_exists(path) || !safe_managed_model_path(&storage_dir, path) {
            continue;
        }
        config
            .general
            .managed_models
            .entry(id.clone())
            .or_insert_with(|| ManagedModelInstall::app_managed(path.clone(), "legacy-model-path"));
    }

    for install in config.general.managed_models.values_mut() {
        if install.path.as_os_str().is_empty() {
            install.path = PathBuf::new();
        }
    }
}

fn normalize_managed_remote_models(config: &mut AppConfig) {
    let storage_dir = model_storage_dir(config);
    config
        .general
        .managed_remote_models
        .retain(|id, install| valid_managed_remote_model_in_storage(&storage_dir, id, install));
}

fn normalize_imported_gguf_models(config: &mut AppConfig) {
    config
        .general
        .imported_gguf_models
        .retain(|id, install| valid_imported_gguf_model(id, install));
}

fn valid_imported_gguf_model(id: &str, install: &ImportedGgufModelInstall) -> bool {
    imported_gguf_model_id(&install.sha256).as_deref() == Some(id)
        && install.size_bytes > 0
        && !install.display_name.trim().is_empty()
        && install
            .path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("gguf"))
        && regular_imported_gguf_file(&install.path, install.size_bytes)
}

fn regular_imported_gguf_file(path: &Path, expected_size_bytes: u64) -> bool {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return false;
    };
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return false;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;

        if metadata.file_attributes() & 0x400 != 0 {
            return false;
        }
    }
    metadata.len() == expected_size_bytes
}

fn valid_managed_remote_model(
    config: &AppConfig,
    id: &str,
    install: &ManagedRemoteModelInstall,
) -> bool {
    valid_managed_remote_model_in_storage(&model_storage_dir(config), id, install)
}

fn valid_managed_remote_model_in_storage(
    storage_dir: &Path,
    id: &str,
    install: &ManagedRemoteModelInstall,
) -> bool {
    managed_remote_model_id(&install.repository, &install.revision, &install.filename).as_deref()
        == Some(id)
        && valid_remote_artifact_metadata(
            &install.repository,
            &install.revision,
            &install.filename,
            install.expected_size_bytes,
            &install.expected_sha256,
        )
        && remote_managed_model_path_in_storage(storage_dir, install) == Some(install.path.clone())
        && safe_managed_model_path(storage_dir, &install.path)
}

fn valid_remote_artifact_metadata(
    repository: &str,
    revision: &str,
    filename: &str,
    expected_size_bytes: u64,
    expected_sha256: &str,
) -> bool {
    let Some((organization, repository_name)) = repository.split_once('/') else {
        return false;
    };
    organization == "handy-computer"
        && safe_hub_identifier(repository_name)
        && revision.len() == 40
        && revision.bytes().all(|byte| byte.is_ascii_hexdigit())
        && expected_size_bytes > 0
        && expected_sha256.len() == 64
        && expected_sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        && safe_hub_gguf_filename(filename)
}

fn remote_managed_model_path_in_storage(
    storage_dir: &Path,
    install: &ManagedRemoteModelInstall,
) -> Option<PathBuf> {
    let (organization, repository) = install.repository.split_once('/')?;
    Some(
        storage_dir
            .join("huggingface")
            .join(organization)
            .join(repository)
            .join(&install.revision)
            .join(managed_remote_model_id(
                &install.repository,
                &install.revision,
                &install.filename,
            )?)
            .join(&install.filename),
    )
}

fn safe_hub_identifier(value: &str) -> bool {
    value
        .bytes()
        .next()
        .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn safe_hub_gguf_filename(value: &str) -> bool {
    let path = Path::new(value);
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("gguf"))
        && !path.is_absolute()
        && path.components().all(|component| {
            matches!(component, std::path::Component::Normal(_))
                && component
                    .as_os_str()
                    .to_str()
                    .is_some_and(safe_hub_identifier)
        })
}

fn safe_managed_model_path(storage_dir: &Path, path: &Path) -> bool {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        || !path.starts_with(storage_dir)
    {
        return false;
    }
    if !path.exists() {
        return true;
    }
    path.canonicalize()
        .ok()
        .zip(storage_dir.canonicalize().ok())
        .is_some_and(|(path, storage)| path.starts_with(storage))
}

fn normalize_playground_order(config: &mut AppConfig, catalog_ids: &[String]) {
    config
        .general
        .playground_model_order
        .retain(|id| catalog_ids.iter().any(|catalog_id| catalog_id == id));
    dedup_preserving_order(&mut config.general.playground_model_order);

    for id in catalog_ids {
        if !config
            .general
            .playground_model_order
            .iter()
            .any(|existing| existing == id)
        {
            config.general.playground_model_order.push(id.clone());
        }
    }
}

fn dedup_preserving_order(ids: &mut Vec<String>) {
    let mut seen = Vec::new();
    ids.retain(|id| {
        if seen.iter().any(|seen_id| seen_id == id) {
            false
        } else {
            seen.push(id.clone());
            true
        }
    });
}

fn dedup_paths_preserving_order(paths: &mut Vec<PathBuf>) {
    let mut seen = Vec::new();
    paths.retain(|path| {
        if seen.iter().any(|seen_path| seen_path == path) {
            false
        } else {
            seen.push(path.clone());
            true
        }
    });
}

fn first_existing_path(paths: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    paths.into_iter().find(|path| path.exists())
}

fn first_valid_model_path(
    model: &SttModelInfo,
    paths: impl IntoIterator<Item = PathBuf>,
) -> Option<PathBuf> {
    paths
        .into_iter()
        .find(|path| is_valid_model_install_path(model, path))
}

fn first_non_empty_path(paths: impl IntoIterator<Item = Option<PathBuf>>) -> Option<PathBuf> {
    paths
        .into_iter()
        .flatten()
        .find(|path| !path.as_os_str().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_bundled_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "scribe-bundled-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn bundled_base_model_resolves_as_an_immutable_executable_sibling() {
        let executable = Path::new("release").join("local-transcriber.exe");

        assert_eq!(
            bundled_model_path_for_executable(&executable).as_deref(),
            Some(
                Path::new("release")
                    .join("whisper-base.en-Q8_0.gguf")
                    .as_path()
            )
        );
    }

    #[test]
    fn fresh_profile_projects_present_or_missing_bundled_base_without_persisting_it() {
        let root = unique_bundled_root("projection");
        fs::create_dir_all(&root).unwrap();
        let bundle = root.join("whisper-base.en-Q8_0.gguf");
        let mut config = AppConfig::default();
        config.general.model_storage_dir = root.join("storage");
        assert!(
            !config
                .general
                .model_paths
                .contains_key(BUNDLED_BASE_MODEL_ID)
        );

        let missing = configured_models_with_bundled_path(&config, Some(bundle.clone()))
            .into_iter()
            .find(|model| model.id == BUNDLED_BASE_MODEL_ID)
            .unwrap();
        assert_eq!(missing.local_path.as_deref(), Some(bundle.as_path()));
        assert_eq!(missing.artifact_origin, ModelArtifactOrigin::Bundled);
        assert_eq!(missing.install_status, ModelInstallStatus::Missing);

        fs::write(&bundle, b"packaged model fixture").unwrap();
        let included = configured_models_with_bundled_path(&config, Some(bundle.clone()))
            .into_iter()
            .find(|model| model.id == BUNDLED_BASE_MODEL_ID)
            .unwrap();
        assert_eq!(included.local_path.as_deref(), Some(bundle.as_path()));
        assert_eq!(included.artifact_origin, ModelArtifactOrigin::Bundled);
        assert_eq!(included.install_status, ModelInstallStatus::Installed);
        assert!(
            !config
                .general
                .model_paths
                .contains_key(BUNDLED_BASE_MODEL_ID)
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn bundled_projection_does_not_mutate_existing_managed_base_configuration() {
        let root = unique_bundled_root("managed-preservation");
        fs::create_dir_all(&root).unwrap();
        let bundle = root.join("whisper-base.en-Q8_0.gguf");
        let managed = root.join("managed").join("whisper-base.en-Q8_0.gguf");
        fs::create_dir_all(managed.parent().unwrap()).unwrap();
        fs::write(&bundle, b"packaged fixture").unwrap();
        fs::write(&managed, b"managed fixture").unwrap();
        let mut config = AppConfig::default();
        config.general.model_storage_dir = root.join("storage");
        config
            .general
            .model_paths
            .insert(BUNDLED_BASE_MODEL_ID.to_owned(), managed.clone());
        config.general.managed_models.insert(
            BUNDLED_BASE_MODEL_ID.to_owned(),
            ManagedModelInstall::app_managed(managed.clone(), "legacy-model-path"),
        );
        let before = serde_json::to_value(&config).unwrap();

        let projected = configured_models_with_bundled_path(&config, Some(bundle.clone()))
            .into_iter()
            .find(|model| model.id == BUNDLED_BASE_MODEL_ID)
            .unwrap();

        assert_eq!(projected.local_path.as_deref(), Some(bundle.as_path()));
        assert_eq!(projected.artifact_origin, ModelArtifactOrigin::Bundled);
        assert_eq!(serde_json::to_value(&config).unwrap(), before);
        assert!(managed.is_file());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn verified_user_triggered_repair_supersedes_a_corrupt_bundled_artifact() {
        let root = unique_bundled_root("verified-repair");
        fs::create_dir_all(&root).unwrap();
        let bundle = root.join("whisper-base.en-Q8_0.gguf");
        let managed = root.join("managed").join("whisper-base.en-Q8_0.gguf");
        fs::create_dir_all(managed.parent().unwrap()).unwrap();
        fs::write(&bundle, b"corrupt packaged fixture").unwrap();
        fs::write(&managed, b"verified managed fixture").unwrap();
        let manifest = runtime_model_manifest(&ModelId::new(BUNDLED_BASE_MODEL_ID)).unwrap();
        let mut config = AppConfig::default();
        config.general.model_storage_dir = root.join("storage");
        let mut receipt =
            ManagedModelInstall::app_managed(managed.clone(), "verified-manifest-download");
        receipt.sha256 = Some(manifest.artifact_sha256.to_owned());
        config
            .general
            .managed_models
            .insert(BUNDLED_BASE_MODEL_ID.to_owned(), receipt);

        let projected = configured_models_with_bundled_path(&config, Some(bundle))
            .into_iter()
            .find(|model| model.id == BUNDLED_BASE_MODEL_ID)
            .unwrap();

        assert_eq!(projected.local_path.as_deref(), Some(managed.as_path()));
        assert_eq!(projected.artifact_origin, ModelArtifactOrigin::Managed);
        assert!(managed.is_file());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn excluded_bundled_base_is_available_even_when_an_update_restores_its_file() {
        let root = unique_bundled_root("excluded-restored");
        fs::create_dir_all(&root).unwrap();
        let bundle = root.join("whisper-base.en-Q8_0.gguf");
        fs::write(&bundle, b"restored packaged fixture").unwrap();
        let mut config = AppConfig::default();
        config.general.model_storage_dir = root.join("storage");
        config
            .general
            .excluded_bundled_model_ids
            .push(BUNDLED_BASE_MODEL_ID.to_owned());

        let projected = configured_models_with_bundled_path(&config, Some(bundle.clone()))
            .into_iter()
            .find(|model| model.id == BUNDLED_BASE_MODEL_ID)
            .unwrap();

        assert_ne!(projected.local_path.as_deref(), Some(bundle.as_path()));
        assert_ne!(projected.artifact_origin, ModelArtifactOrigin::Bundled);
        assert_eq!(projected.install_status, ModelInstallStatus::NotInstalled);
        assert!(
            bundle.is_file(),
            "projection must not mutate restored bytes"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn excluded_bundled_ids_normalize_to_the_known_unique_set() {
        let mut config = AppConfig::default();
        config.general.excluded_bundled_model_ids = vec![
            "unknown-bundle".to_owned(),
            BUNDLED_BASE_MODEL_ID.to_owned(),
            BUNDLED_BASE_MODEL_ID.to_owned(),
        ];

        normalize_config(&mut config);

        assert_eq!(
            config.general.excluded_bundled_model_ids,
            [BUNDLED_BASE_MODEL_ID]
        );
    }

    #[test]
    fn fresh_default_is_base_and_normalization_preserves_an_explicit_existing_selection() {
        let root = unique_bundled_root("identity-normalization");
        let mut fresh = AppConfig::default();
        fresh.general.model_storage_dir = root.clone();
        normalize_config(&mut fresh);
        assert_eq!(fresh.general.selected_default_model, BUNDLED_BASE_MODEL_ID);

        let mut existing = AppConfig::default();
        existing.general.model_storage_dir = root;
        existing.general.selected_default_model = "whisper_cpp_small_en".to_owned();
        normalize_config(&mut existing);
        assert_eq!(
            existing.general.selected_default_model,
            "whisper_cpp_small_en"
        );
    }

    #[test]
    fn endpointing_values_normalize_to_safe_ordered_ranges() {
        let mut config = AppConfig::default();
        config.recording.speech_confirmation_ms = 0;
        config.recording.internal_pause_ms = 10;
        config.recording.endpoint_silence_ms = 20;
        config.recording.pre_roll_ms = 9_000;
        config.recording.post_roll_ms = 9_000;

        normalize_config(&mut config);

        assert_eq!(config.recording.speech_confirmation_ms, 50);
        assert_eq!(config.recording.internal_pause_ms, 100);
        assert_eq!(config.recording.endpoint_silence_ms, 300);
        assert_eq!(config.recording.pre_roll_ms, 2_000);
        assert_eq!(config.recording.post_roll_ms, 2_000);
    }

    #[test]
    fn input_threshold_dbfs_normalizes_to_the_supported_range() {
        let mut config = AppConfig::default();
        config.recording.input_threshold_dbfs = f32::INFINITY;
        normalize_config(&mut config);
        assert_eq!(
            config.recording.input_threshold_dbfs,
            settings::DEFAULT_INPUT_THRESHOLD_DBFS
        );

        config.recording.input_threshold_dbfs = 10.0;
        normalize_config(&mut config);
        assert_eq!(
            config.recording.input_threshold_dbfs,
            settings::MAX_INPUT_THRESHOLD_DBFS
        );

        config.recording.input_threshold_dbfs = -100.0;
        normalize_config(&mut config);
        assert_eq!(
            config.recording.input_threshold_dbfs,
            settings::MIN_INPUT_THRESHOLD_DBFS
        );
    }

    #[test]
    fn recording_duration_is_bounded_for_hand_edited_settings() {
        let mut config = AppConfig::default();
        config.recording.max_recording_seconds = u32::MAX;

        normalize_config(&mut config);

        assert_eq!(
            config.recording.max_recording_seconds,
            MAX_RECORDING_SECONDS
        );
    }

    #[test]
    fn history_retention_values_are_bounded_for_hand_edited_settings() {
        let mut config = AppConfig::default();
        config.history.max_unpinned_entries = 0;
        config.history.transcript_retention_days = Some(0);
        config.history.audio_retention_days = Some(u32::MAX);

        normalize_config(&mut config);

        assert_eq!(config.history.max_unpinned_entries, 1);
        assert_eq!(config.history.transcript_retention_days, None);
        assert_eq!(
            config.history.audio_retention_days,
            Some(MAX_HISTORY_RETENTION_DAYS)
        );

        config.history.max_unpinned_entries = u32::MAX;
        normalize_config(&mut config);
        assert_eq!(config.history.max_unpinned_entries, MAX_HISTORY_ENTRIES);
    }

    #[test]
    fn old_cuda_value_deserializes_to_prefer_gpu() {
        let mode: AccelerationPreference = serde_json::from_str(r#""cuda""#).unwrap();

        assert_eq!(mode, AccelerationPreference::Gpu);
    }

    #[test]
    fn hotkey_mode_uses_stable_snake_case_names() {
        let config = AppConfig {
            recording: RecordingSettings {
                hotkey_mode: HotkeyMode::HoldToTalk,
                ..Default::default()
            },
            ..Default::default()
        };

        let serialized = serde_json::to_string(&config).unwrap();
        assert!(serialized.contains(r#""hotkey_mode":"hold_to_talk""#));

        let parsed: AppConfig = serde_json::from_str(&serialized).unwrap();
        assert_eq!(parsed.recording.hotkey_mode, HotkeyMode::HoldToTalk);
    }

    #[test]
    fn explicit_empty_playground_selection_is_preserved() {
        let mut config = AppConfig {
            general: GeneralSettings {
                playground_selected_models: Vec::new(),
                ..Default::default()
            },
            ..AppConfig::default()
        };

        normalize_config(&mut config);

        assert_eq!(config.general.selected_default_model, BUNDLED_BASE_MODEL_ID);
        assert!(config.general.playground_selected_models.is_empty());
    }

    #[test]
    fn playground_selection_filters_retired_invalid_and_duplicate_ids() {
        let mut config = AppConfig {
            general: GeneralSettings {
                playground_selected_models: vec![
                    "faster_whisper_medium_en_gpu".to_owned(),
                    "invalid".to_owned(),
                    "faster_whisper_medium_en_gpu".to_owned(),
                    "whisper_cpp_small_en".to_owned(),
                    "whisper_cpp_small_en".to_owned(),
                ],
                ..Default::default()
            },
            ..AppConfig::default()
        };

        normalize_config(&mut config);
        assert_eq!(
            config.general.playground_selected_models,
            ["whisper_cpp_small_en"]
        );
    }

    #[test]
    fn retired_and_unauthorized_model_paths_are_not_probed() {
        let mut config = AppConfig::default();
        config.general.model_storage_dir = PathBuf::from("test-model-storage");
        let catalog = default_model_catalog();
        let authorized_model = catalog
            .iter()
            .find(|model| model.id == "whisper_cpp_tiny_en")
            .unwrap();
        let authorized_path = downloaded_model_path(&config, authorized_model).unwrap();
        let unauthorized_path = PathBuf::from("copied-profile/unmanaged-supported-model.gguf");
        let retired_path = PathBuf::from("copied-profile/retired-provider-model.bin");
        config
            .general
            .model_paths
            .insert(authorized_model.id.clone(), authorized_path.clone());
        config
            .general
            .model_paths
            .insert("whisper_cpp_small_en".to_owned(), unauthorized_path.clone());
        config
            .general
            .model_paths
            .insert("faster_whisper".to_owned(), retired_path.clone());

        let mut probed = Vec::new();
        apply_managed_model_metadata_with_path_probe(&mut config, |path| {
            probed.push(path.to_path_buf());
            false
        });

        assert_eq!(probed, [authorized_path]);
        assert_eq!(
            config.general.model_paths.get("faster_whisper"),
            Some(&retired_path)
        );
        assert_eq!(
            config.general.model_paths.get("whisper_cpp_small_en"),
            Some(&unauthorized_path)
        );
        assert!(config.general.managed_models.is_empty());
    }

    #[test]
    fn selected_installed_playground_models_follow_persisted_drag_order() {
        let root = std::env::temp_dir().join(format!(
            "scribe-playground-selection-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let model_dir = root.join("gguf");
        fs::create_dir_all(&model_dir).unwrap();
        let tiny_path = root.join("gguf").join("whisper-tiny.en-Q4_K_M.gguf");
        fs::write(tiny_path, b"tiny").unwrap();
        fs::write(model_dir.join("whisper-base.en-Q8_0.gguf"), b"base").unwrap();
        fs::write(model_dir.join("whisper-small.en-Q8_0.gguf"), b"small").unwrap();

        let mut config = AppConfig {
            general: GeneralSettings {
                model_storage_dir: root.clone(),
                playground_selected_models: vec![
                    "whisper_cpp_tiny_en".to_owned(),
                    "whisper_cpp_base_en".to_owned(),
                    "whisper_cpp_medium_en".to_owned(),
                ],
                playground_model_order: vec![
                    "whisper_cpp_small_en".to_owned(),
                    "whisper_cpp_medium_en".to_owned(),
                    "whisper_cpp_base_en".to_owned(),
                    "whisper_cpp_tiny_en".to_owned(),
                ],
                ..Default::default()
            },
            ..AppConfig::default()
        };
        normalize_config(&mut config);

        let ids = playground_selected_installed_models(&config)
            .into_iter()
            .map(|model| model.id)
            .collect::<Vec<_>>();
        assert_eq!(ids, ["whisper_cpp_base_en", "whisper_cpp_tiny_en"]);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn receipt_backed_onnx_projection_is_canonical_and_failure_closed() {
        let root = std::env::temp_dir().join(format!(
            "scribe-onnx-config-projection-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let config = AppConfig {
            general: GeneralSettings {
                model_storage_dir: root.clone(),
                ..Default::default()
            },
            ..AppConfig::default()
        };
        let before = serde_json::to_vec(&config).unwrap();
        let model_id = ModelId::new("moonshine-tiny-en-int8-onnx");
        let expected = root
            .join("onnx-bundles")
            .join("moonshine-tiny-en-int8-onnx");
        let catalog_model = default_model_catalog()
            .into_iter()
            .find(|model| model.id == model_id.as_str())
            .unwrap();
        assert_eq!(
            downloaded_model_path(&config, &catalog_model),
            Some(expected.clone())
        );
        assert!(installed_onnx_bundle_root(&config, &model_id).is_none());
        assert_eq!(
            onnx_bundle_target_root(&config, &model_id),
            Some(expected.clone())
        );
        assert_eq!(
            serde_json::to_vec(&config).unwrap(),
            before,
            "read-only discovery must not mutate settings"
        );

        fs::create_dir_all(&expected).unwrap();
        fs::write(expected.join("tokens.txt"), b"plausible but unreceipted").unwrap();
        fs::write(expected.join("encoder.int8.onnx"), b"tampered").unwrap();
        fs::write(expected.join("decoder.int8.onnx"), b"tampered").unwrap();
        fs::write(expected.join("joiner.int8.onnx"), b"tampered").unwrap();
        let projected = configured_models(&config)
            .into_iter()
            .find(|model| model.id == model_id.as_str())
            .unwrap();
        assert_eq!(projected.local_path.as_deref(), Some(expected.as_path()));
        assert_eq!(projected.install_status, ModelInstallStatus::Missing);
        assert_eq!(
            onnx_bundle_target_root(&config, &model_id),
            Some(expected.clone()),
            "cleanup target remains deterministic when execution receipt is invalid"
        );
        assert_eq!(
            serde_json::to_vec(&config).unwrap(),
            before,
            "failed receipt discovery must not mutate settings"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn wrong_onnx_bundle_id_has_no_canonical_install_root() {
        let config = AppConfig::default();
        assert!(installed_onnx_bundle_root(&config, &ModelId::new("moonshine-wrong-id")).is_none());
    }

    #[test]
    fn downloaded_model_file_wins_over_stale_configured_path() {
        let temp_dir =
            std::env::temp_dir().join(format!("scribe-config-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp_dir);
        let model_dir = temp_dir.join("gguf");
        fs::create_dir_all(&model_dir).unwrap();
        let downloaded_path = model_dir.join("whisper-base.en-Q8_0.gguf");
        fs::write(&downloaded_path, b"model").unwrap();

        let mut model_paths = HashMap::new();
        model_paths.insert(
            "whisper_cpp_base_en".to_owned(),
            temp_dir.join("missing").join("whisper-base.en-Q8_0.gguf"),
        );
        let config = AppConfig {
            general: GeneralSettings {
                model_storage_dir: temp_dir.clone(),
                model_paths,
                ..Default::default()
            },
            ..AppConfig::default()
        };

        let model = configured_models_with_bundled_path(&config, None)
            .into_iter()
            .find(|model| model.id == "whisper_cpp_base_en")
            .unwrap();

        assert_eq!(model.local_path.as_deref(), Some(downloaded_path.as_path()));
        assert_eq!(model.install_status, ModelInstallStatus::Installed);

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn existing_configured_model_path_populates_managed_metadata() {
        let temp_dir =
            std::env::temp_dir().join(format!("scribe-managed-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp_dir);
        let app_storage = temp_dir.join("app-models");
        fs::create_dir_all(&app_storage).unwrap();
        let model_path = app_storage.join("gguf").join("whisper-base.en-Q8_0.gguf");
        fs::create_dir_all(model_path.parent().unwrap()).unwrap();
        fs::write(&model_path, b"model").unwrap();

        let mut model_paths = HashMap::new();
        model_paths.insert("whisper_cpp_base_en".to_owned(), model_path.clone());
        let mut config = AppConfig {
            general: GeneralSettings {
                model_storage_dir: app_storage,
                model_paths,
                ..Default::default()
            },
            ..AppConfig::default()
        };

        normalize_config(&mut config);

        assert_eq!(
            managed_model_path(
                &config,
                &configured_models(&config)
                    .into_iter()
                    .find(|model| model.id == "whisper_cpp_base_en")
                    .unwrap()
            )
            .as_deref(),
            Some(model_path.as_path())
        );

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn trusted_remote_gguf_survives_normalization_only_at_its_pinned_path() {
        let root =
            std::env::temp_dir().join(format!("scribe-remote-model-config-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let repository = "handy-computer/example-asr-gguf";
        let revision = "0123456789abcdef0123456789abcdef01234567";
        let filename = "example-Q4_K_M.gguf";
        let id = managed_remote_model_id(repository, revision, filename).unwrap();
        let path = root
            .join("huggingface")
            .join("handy-computer")
            .join("example-asr-gguf")
            .join(revision)
            .join(&id)
            .join(filename);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"fixture").unwrap();

        let mut config = AppConfig::default();
        config.general.model_storage_dir = root.clone();
        config.general.selected_default_model = id.clone();
        config.general.managed_remote_models.insert(
            id.clone(),
            ManagedRemoteModelInstall::trusted(
                RemoteGgufArtifact {
                    repository: repository.to_owned(),
                    revision: revision.to_owned(),
                    filename: filename.to_owned(),
                    expected_size_bytes: 7,
                    expected_sha256: "a".repeat(64),
                },
                path.clone(),
                "Example ASR".to_owned(),
                "Trusted test model".to_owned(),
                vec!["en".to_owned()],
                false,
            ),
        );

        normalize_config(&mut config);

        let model = configured_models(&config)
            .into_iter()
            .find(|model| model.id == id)
            .unwrap();
        assert_eq!(model.local_path.as_deref(), Some(path.as_path()));
        assert_eq!(model.install_status, ModelInstallStatus::Installed);
        assert!(remote_gguf_artifact(&config, &model.id).is_some());
        assert_eq!(config.general.selected_default_model, model.id);

        config
            .general
            .managed_remote_models
            .get_mut(&model.id)
            .unwrap()
            .path = root.join("outside.gguf");
        normalize_config(&mut config);
        assert!(config.general.managed_remote_models.is_empty());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn managed_remote_model_id_rejects_dot_path_repository_components() {
        let revision = "0123456789abcdef0123456789abcdef01234567";
        let filename = "example-Q4_K_M.gguf";

        for repository in ["handy-computer/.", "handy-computer/.."] {
            assert!(managed_remote_model_id(repository, revision, filename).is_none());
        }
    }

    #[test]
    fn imported_gguf_stays_external_and_is_never_classified_as_remote_or_managed() {
        let root = std::env::temp_dir().join(format!(
            "scribe-imported-gguf-config-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let storage = root.join("scribe-models");
        let external = root.join("external").join("imported.gguf");
        fs::create_dir_all(external.parent().unwrap()).unwrap();
        fs::write(&external, b"imported fixture").unwrap();
        let sha256 = format!("{:x}", Sha256::digest(b"imported fixture"));
        let id = imported_gguf_model_id(&sha256).unwrap();
        let canonical = fs::canonicalize(&external).unwrap();
        let mut config = AppConfig::default();
        config.general.model_storage_dir = storage;
        config.general.imported_gguf_models.insert(
            id.clone(),
            ImportedGgufModelInstall::validated(
                canonical.clone(),
                b"imported fixture".len() as u64,
                sha256,
                "Imported fixture".to_owned(),
            ),
        );

        normalize_config(&mut config);

        let model = configured_models(&config)
            .into_iter()
            .find(|model| model.id == id)
            .expect("imported model remains configured");
        assert_eq!(model.local_path.as_deref(), Some(canonical.as_path()));
        assert_eq!(model.install_status, ModelInstallStatus::Installed);
        assert!(imported_gguf_artifact(&config, &model.id).is_some());
        assert!(remote_gguf_artifact(&config, &model.id).is_none());
        assert!(!config.general.managed_models.contains_key(&model.id));
        assert!(!config.general.managed_remote_models.contains_key(&model.id));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn configured_models_sorts_managed_and_imported_gguf_models_by_id() {
        let root = std::env::temp_dir().join(format!(
            "scribe-configured-model-order-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let revision = "0123456789abcdef0123456789abcdef01234567";
        let mut config = AppConfig::default();
        config.general.model_storage_dir = root.join("models");

        let mut managed_ids = Vec::new();
        for (repository, filename) in [
            ("handy-computer/zeta", "zeta.gguf"),
            ("handy-computer/alpha", "alpha.gguf"),
        ] {
            let id = managed_remote_model_id(repository, revision, filename).unwrap();
            let path = config
                .general
                .model_storage_dir
                .join("huggingface")
                .join(repository)
                .join(revision)
                .join(&id)
                .join(filename);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, b"fixture").unwrap();
            config.general.managed_remote_models.insert(
                id.clone(),
                ManagedRemoteModelInstall::trusted(
                    RemoteGgufArtifact {
                        repository: repository.to_owned(),
                        revision: revision.to_owned(),
                        filename: filename.to_owned(),
                        expected_size_bytes: 7,
                        expected_sha256: "a".repeat(64),
                    },
                    path,
                    id.clone(),
                    "Trusted test model".to_owned(),
                    vec!["en".to_owned()],
                    false,
                ),
            );
            managed_ids.push(id);
        }

        let mut imported_ids = Vec::new();
        for (filename, bytes) in [
            ("zeta.gguf", b"zeta".as_slice()),
            ("alpha.gguf", b"alpha".as_slice()),
        ] {
            let path = root.join("external").join(filename);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, bytes).unwrap();
            let sha256 = format!("{:x}", Sha256::digest(bytes));
            let id = imported_gguf_model_id(&sha256).unwrap();
            config.general.imported_gguf_models.insert(
                id.clone(),
                ImportedGgufModelInstall::validated(
                    fs::canonicalize(path).unwrap(),
                    bytes.len() as u64,
                    sha256,
                    id.clone(),
                ),
            );
            imported_ids.push(id);
        }

        managed_ids.sort();
        imported_ids.sort();
        let models = configured_models(&config);
        let actual_managed_ids = models
            .iter()
            .filter(|model| managed_ids.contains(&model.id))
            .map(|model| model.id.clone())
            .collect::<Vec<_>>();
        let actual_imported_ids = models
            .iter()
            .filter(|model| imported_ids.contains(&model.id))
            .map(|model| model.id.clone())
            .collect::<Vec<_>>();
        assert_eq!(actual_managed_ids, managed_ids);
        assert_eq!(actual_imported_ids, imported_ids);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn external_configured_model_path_stays_readable_but_not_managed() {
        let temp_dir = std::env::temp_dir().join(format!(
            "scribe-external-config-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&temp_dir);
        let app_storage = temp_dir.join("app-models");
        let external_path = temp_dir.join("external").join("whisper-base.en-Q8_0.gguf");
        fs::create_dir_all(external_path.parent().unwrap()).unwrap();
        fs::write(&external_path, b"model").unwrap();

        let mut model_paths = HashMap::new();
        model_paths.insert("whisper_cpp_base_en".to_owned(), external_path.clone());
        let mut config = AppConfig {
            general: GeneralSettings {
                model_storage_dir: app_storage,
                model_paths,
                ..Default::default()
            },
            ..AppConfig::default()
        };

        normalize_config(&mut config);
        let model = configured_models_with_bundled_path(&config, None)
            .into_iter()
            .find(|model| model.id == "whisper_cpp_base_en")
            .unwrap();

        assert_eq!(managed_model_path(&config, &model), None);
        assert_eq!(model.local_path.as_deref(), Some(external_path.as_path()));
        assert_eq!(model.install_status, ModelInstallStatus::Installed);

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn managed_metadata_rejects_parent_directory_escape() {
        let root = std::env::temp_dir().join(format!(
            "scribe-managed-parent-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let storage = root.join("models");
        fs::create_dir_all(&storage).unwrap();
        let escaped = storage.join("..").join("external.bin");
        fs::write(root.join("external.bin"), b"external").unwrap();
        let mut config = AppConfig::default();
        config.general.model_storage_dir = storage;
        config.general.managed_models.insert(
            "whisper_cpp_base_en".to_owned(),
            ManagedModelInstall::app_managed(escaped.clone(), "tampered"),
        );
        config
            .general
            .model_paths
            .insert("whisper_cpp_base_en".to_owned(), escaped);

        normalize_config(&mut config);

        assert!(
            !config
                .general
                .managed_models
                .contains_key("whisper_cpp_base_en")
        );
        assert_eq!(fs::read(root.join("external.bin")).unwrap(), b"external");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn managed_metadata_rejects_symlink_escape_from_catalog_path() {
        let root = std::env::temp_dir().join(format!(
            "scribe-managed-symlink-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let storage = root.join("models");
        let external = root.join("external.bin");
        fs::create_dir_all(&storage).unwrap();
        fs::write(&external, b"external").unwrap();
        let mut config = AppConfig::default();
        config.general.model_storage_dir = storage;
        let model = default_model_catalog()
            .into_iter()
            .find(|model| model.id == "whisper_cpp_base_en")
            .unwrap();
        let expected = downloaded_model_path(&config, &model).unwrap();
        fs::create_dir_all(expected.parent().unwrap()).unwrap();
        #[cfg(unix)]
        let linked = std::os::unix::fs::symlink(&external, &expected).is_ok();
        #[cfg(windows)]
        let linked = std::os::windows::fs::symlink_file(&external, &expected).is_ok();
        if !linked {
            fs::remove_dir_all(root).unwrap();
            return;
        }
        config.general.managed_models.insert(
            model.id.clone(),
            ManagedModelInstall::app_managed(expected, "tampered"),
        );

        normalize_config(&mut config);

        assert!(!config.general.managed_models.contains_key(&model.id));
        assert_eq!(fs::read(external).unwrap(), b"external");
        fs::remove_dir_all(root).unwrap();
    }
}
