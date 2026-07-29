use std::collections::HashMap;
use std::fs;
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::durable_fs;
use crate::models::{ModelInstallStatus, SttModelInfo, default_model_catalog};
use crate::runtime_catalog;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppConfig {
    pub selected_default_model: String,
    #[serde(default, alias = "playground_enabled_models", alias = "enabled_models")]
    pub playground_selected_models: Vec<String>,
    #[serde(default)]
    pub playground_model_order: Vec<String>,
    #[serde(default)]
    pub managed_models: HashMap<String, ManagedModelInstall>,
    #[serde(default)]
    pub managed_runtimes: HashMap<String, ManagedRuntimeInstall>,
    pub hotkey: String,
    #[serde(default)]
    pub hotkey_mode: HotkeyMode,
    pub whisper_executable_path: Option<PathBuf>,
    #[serde(default = "default_legacy_whisper_compute_mode")]
    pub whisper_compute_mode: WhisperComputeMode,
    #[serde(default)]
    pub whisper_gpu_device: u32,
    #[serde(default = "default_whisper_cuda_backend_path")]
    pub whisper_cuda_backend_path: Option<PathBuf>,
    #[serde(default = "default_whisper_cuda_library_paths")]
    pub whisper_cuda_library_paths: Vec<PathBuf>,
    #[serde(default = "default_model_storage_dir")]
    pub model_storage_dir: PathBuf,
    #[serde(default)]
    pub theme_mode: ThemeMode,
    #[serde(default)]
    pub audio_input_device_name: Option<String>,
    pub model_paths: HashMap<String, PathBuf>,
    pub last_used_backend: String,
    pub debug_mode: bool,
    #[serde(default = "default_max_recording_seconds")]
    pub max_recording_seconds: u32,
    #[serde(default, alias = "live_whisper_preview")]
    pub live_transcription_enabled: bool,
    #[serde(default)]
    pub voice_editing_enabled: bool,
    #[serde(default)]
    pub voice_editing_model_tier: VoiceEditingModelTier,
    #[serde(default = "default_true")]
    pub close_to_tray: bool,
    #[serde(default = "default_true")]
    pub auto_insert_transcript: bool,
    #[serde(default = "default_true")]
    pub restore_clipboard_after_insert: bool,
    #[serde(default = "default_paste_delay_ms")]
    pub paste_delay_ms: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
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
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ManagedRuntimeInstall {
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
    pub device: Option<String>,
    #[serde(default)]
    pub installed_at_unix_seconds: Option<u64>,
}

impl ManagedModelInstall {
    #[cfg(test)]
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            ..Self::default()
        }
    }

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

impl ManagedRuntimeInstall {
    #[cfg(test)]
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            ..Self::default()
        }
    }

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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WhisperComputeMode {
    #[default]
    Auto,
    #[serde(alias = "cuda")]
    PreferGpu,
    Cpu,
}

impl WhisperComputeMode {
    pub const ALL: [WhisperComputeMode; 3] = [
        WhisperComputeMode::Auto,
        WhisperComputeMode::PreferGpu,
        WhisperComputeMode::Cpu,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::PreferGpu => "Prefer GPU",
            Self::Cpu => "CPU only",
        }
    }
}

impl ThemeMode {
    pub const ALL: [ThemeMode; 3] = [ThemeMode::Light, ThemeMode::Dark, ThemeMode::System];

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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoiceEditingModelTier {
    Compact,
    #[default]
    Balanced,
}

impl VoiceEditingModelTier {
    pub const ALL: [Self; 2] = [Self::Compact, Self::Balanced];

    pub fn label(self) -> &'static str {
        match self {
            Self::Compact => "Compact",
            Self::Balanced => "Balanced",
        }
    }

    pub fn model_id(self) -> &'static str {
        match self {
            Self::Compact => "qwen3_0_6b_q8_0",
            Self::Balanced => "qwen3_1_7b_q8_0",
        }
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            selected_default_model: "whisper_cpp_tiny_en".to_owned(),
            playground_selected_models: vec!["whisper_cpp_tiny_en".to_owned()],
            playground_model_order: default_playground_model_order(),
            managed_models: HashMap::new(),
            managed_runtimes: HashMap::new(),
            hotkey: "Ctrl+Shift+Space".to_owned(),
            hotkey_mode: HotkeyMode::Toggle,
            whisper_executable_path: None,
            whisper_compute_mode: WhisperComputeMode::Auto,
            whisper_gpu_device: 0,
            whisper_cuda_backend_path: default_whisper_cuda_backend_path(),
            whisper_cuda_library_paths: default_whisper_cuda_library_paths(),
            model_storage_dir: default_model_storage_dir(),
            theme_mode: ThemeMode::Light,
            audio_input_device_name: None,
            model_paths: HashMap::new(),
            last_used_backend: "whisper.cpp".to_owned(),
            debug_mode: false,
            max_recording_seconds: default_max_recording_seconds(),
            live_transcription_enabled: false,
            voice_editing_enabled: false,
            voice_editing_model_tier: VoiceEditingModelTier::Balanced,
            close_to_tray: true,
            auto_insert_transcript: false,
            restore_clipboard_after_insert: true,
            paste_delay_ms: default_paste_delay_ms(),
        }
    }
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
    let lock = lock_config_path(&path, Duration::from_secs(10))?;
    if let Some(mut config) = recover_config_file(&path)? {
        normalize_config(&mut config);
        return Ok((config, path));
    }
    if !path.exists() {
        if let Ok(legacy_path) = legacy_config_file_path()
            && legacy_path.exists()
        {
            let mut config = read_config_file(&legacy_path)?;
            normalize_config(&mut config);
            ensure_config_save_durable(save_config_file_locked(&path, &config, &lock)?)?;
            return Ok((config, path));
        }

        let config = AppConfig::default();
        ensure_config_save_durable(save_config_file_locked(&path, &config, &lock)?)?;
        return Ok((config, path));
    }

    read_config_file(&path).map(|config| (config, path))
}

fn legacy_config_file_path() -> Result<PathBuf> {
    Ok(legacy_project_dirs()?.config_dir().join("config.json"))
}

fn read_config_file(path: &PathBuf) -> Result<AppConfig> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read config {}", path.display()))?;
    serde_json::from_str(&content)
        .with_context(|| format!("failed to parse config {}", path.display()))
}

pub(crate) struct ConfigSaveOutcome {
    pub(crate) config: AppConfig,
    pub(crate) durability_warning: Option<String>,
}

pub(crate) fn save_config_merging_managed_runtimes(
    config: &AppConfig,
) -> Result<ConfigSaveOutcome> {
    save_config_with_runtime_update(config, None)
}

pub(crate) fn save_config_with_runtime_update(
    config: &AppConfig,
    runtime_update: Option<(&str, Option<ManagedRuntimeInstall>)>,
) -> Result<ConfigSaveOutcome> {
    let path = config_file_path()?;
    save_config_with_runtime_update_at(&path, config, runtime_update)
}

fn save_config_with_runtime_update_at(
    path: &Path,
    config: &AppConfig,
    runtime_update: Option<(&str, Option<ManagedRuntimeInstall>)>,
) -> Result<ConfigSaveOutcome> {
    let lock = lock_config_path(path, Duration::from_secs(10))?;
    let persisted = recover_config_file(path)?.unwrap_or_default();
    let mut merged = config.clone();
    merged.managed_runtimes = persisted.managed_runtimes;
    if let Some((runtime_id, install)) = runtime_update {
        match install {
            Some(install) => {
                merged
                    .managed_runtimes
                    .insert(runtime_id.to_owned(), install);
            }
            None => {
                merged.managed_runtimes.remove(runtime_id);
            }
        }
    }
    normalize_config(&mut merged);
    let durability_warning = save_config_file_locked(path, &merged, &lock)?;
    Ok(ConfigSaveOutcome {
        config: merged,
        durability_warning,
    })
}

fn ensure_config_save_durable(warning: Option<String>) -> Result<()> {
    match warning {
        Some(warning) => Err(anyhow!(
            "configuration was published but its durability could not be confirmed: {warning}"
        )),
        None => Ok(()),
    }
}

#[derive(Debug)]
struct ConfigFileLock {
    _file: File,
}

fn lock_config_path(path: &Path, timeout: Duration) -> Result<ConfigFileLock> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("config path {} has no parent", path.display()))?;
    durable_fs::create_dir_all(parent)
        .with_context(|| format!("failed to create config directory {}", parent.display()))?;
    let lock_path = config_sibling_path(path, "lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("failed to open config lock {}", lock_path.display()))?;
    let deadline = Instant::now() + timeout;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(ConfigFileLock { _file: file }),
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(25));
            }
            Err(TryLockError::WouldBlock) => {
                return Err(anyhow!(
                    "another Scribe process is saving the configuration"
                ));
            }
            Err(TryLockError::Error(err)) => {
                return Err(anyhow!("failed to lock {}: {err}", lock_path.display()));
            }
        }
    }
}

fn recover_config_file(path: &Path) -> Result<Option<AppConfig>> {
    let backup = config_sibling_path(path, "backup");
    let mut temporary_paths = config_temporary_paths(path)?;
    temporary_paths.sort_by_key(|candidate| {
        (
            candidate
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.rsplit('-').next())
                .and_then(|value| value.parse::<u128>().ok())
                .unwrap_or_default(),
            candidate.clone(),
        )
    });
    let current = read_valid_config(path);
    if let Some(config) = current {
        durable_fs::remove(&backup)
            .with_context(|| format!("failed to durably remove {}", backup.display()))?;
        for temporary in temporary_paths {
            durable_fs::remove(&temporary)
                .with_context(|| format!("failed to durably remove {}", temporary.display()))?;
        }
        return Ok(Some(config));
    }

    if let Some(config) = read_valid_config(&backup) {
        if path.exists() {
            preserve_invalid_config(path)?;
        }
        durable_fs::rename(&backup, path, false).with_context(|| {
            format!(
                "failed to durably restore config {} from {}",
                path.display(),
                backup.display()
            )
        })?;
        for temporary in temporary_paths {
            durable_fs::remove(&temporary)
                .with_context(|| format!("failed to durably remove {}", temporary.display()))?;
        }
        return Ok(Some(config));
    }

    let valid_temporary = temporary_paths.iter().rev().find_map(|candidate| {
        read_valid_config(candidate).map(|config| (candidate.clone(), config))
    });
    if let Some((temporary, config)) = valid_temporary {
        if path.exists() {
            preserve_invalid_config(path)?;
        }
        durable_fs::rename(&temporary, path, false).with_context(|| {
            format!(
                "failed to durably recover config {} from {}",
                path.display(),
                temporary.display()
            )
        })?;
        for candidate in temporary_paths {
            if candidate != temporary {
                durable_fs::remove(&candidate)
                    .with_context(|| format!("failed to durably remove {}", candidate.display()))?;
            }
        }
        return Ok(Some(config));
    }

    if path.exists() {
        read_config_file(&path.to_path_buf()).map(Some)
    } else {
        Ok(None)
    }
}

fn save_config_file_locked(
    path: &Path,
    config: &AppConfig,
    lock: &ConfigFileLock,
) -> Result<Option<String>> {
    save_config_file_locked_with(
        path,
        config,
        lock,
        durable_fs::rename_with_outcome,
        durable_fs::remove,
    )
}

fn save_config_file_locked_with(
    path: &Path,
    config: &AppConfig,
    _lock: &ConfigFileLock,
    mut rename: impl FnMut(&Path, &Path, bool) -> io::Result<Option<io::Error>>,
    mut remove: impl FnMut(&Path) -> io::Result<()>,
) -> Result<Option<String>> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("config path {} has no parent", path.display()))?;
    durable_fs::create_dir_all(parent)
        .with_context(|| format!("failed to create config directory {}", parent.display()))?;
    let content = serde_json::to_vec_pretty(config)?;
    let temporary = unique_config_temporary_path(path);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .with_context(|| format!("failed to create {}", temporary.display()))?;
    file.write_all(&content)
        .with_context(|| format!("failed to write {}", temporary.display()))?;
    file.write_all(b"\n")
        .with_context(|| format!("failed to finish {}", temporary.display()))?;
    file.flush()
        .with_context(|| format!("failed to flush {}", temporary.display()))?;
    file.sync_all()
        .with_context(|| format!("failed to sync {}", temporary.display()))?;
    drop(file);

    let backup = config_sibling_path(path, "backup");
    remove(&backup).with_context(|| {
        format!(
            "failed to durably remove stale config backup {}",
            backup.display()
        )
    })?;
    let had_previous = path.exists();
    if had_previous {
        let backup_warning = rename(path, &backup, false).with_context(|| {
            format!(
                "failed to preserve config {} as {}",
                path.display(),
                backup.display()
            )
        })?;
        if let Some(backup_warning) = backup_warning {
            let restore_warning = rename(&backup, path, false).with_context(|| {
                format!(
                    "failed to restore config {} after its backup durability barrier failed: {backup_warning}",
                    path.display()
                )
            })?;
            if let Some(restore_warning) = restore_warning {
                return Err(anyhow!(
                    "config backup durability failed ({backup_warning}) and restoring the old config was not durably confirmed ({restore_warning})"
                ));
            }
            return Err(anyhow!(
                "config backup durability failed; the old config was restored: {backup_warning}"
            ));
        }
    }

    let publish = match rename(&temporary, path, false) {
        Ok(outcome) => outcome,
        Err(err) => {
            if had_previous {
                let restore_warning = rename(&backup, path, false).with_context(|| {
                    format!(
                        "failed to restore config {} after publication failed: {err}",
                        path.display()
                    )
                })?;
                if let Some(restore_warning) = restore_warning {
                    return Err(anyhow!(
                        "config publication failed ({err}) and restoring the old config was not durably confirmed ({restore_warning})"
                    ));
                }
            }
            return Err(err)
                .with_context(|| format!("failed to publish config {}", path.display()));
        }
    };
    if let Some(warning) = publish {
        return Ok(Some(format!(
            "config {} was published, but syncing its containing directory failed: {warning}",
            path.display()
        )));
    }

    if let Err(error) = remove(&backup) {
        return Ok(Some(format!(
            "config {} is durable, but its old backup could not be durably removed: {error}",
            path.display()
        )));
    }
    Ok(None)
}

fn read_valid_config(path: &Path) -> Option<AppConfig> {
    fs::read_to_string(path)
        .ok()
        .and_then(|content| serde_json::from_str(&content).ok())
}

fn config_sibling_path(path: &Path, suffix: &str) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config.json");
    path.with_file_name(format!(".{name}.{suffix}"))
}

fn unique_config_temporary_path(path: &Path) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    config_sibling_path(path, &format!("tmp-{}-{nonce:020}", std::process::id()))
}

fn preserve_invalid_config(path: &Path) -> Result<()> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let corrupt = config_sibling_path(path, &format!("corrupt-{}-{nonce:020}", std::process::id()));
    durable_fs::rename(path, &corrupt, false).with_context(|| {
        format!(
            "failed to durably preserve invalid config {} as {}",
            path.display(),
            corrupt.display()
        )
    })
}

fn config_temporary_paths(path: &Path) -> Result<Vec<PathBuf>> {
    let Some(parent) = path.parent() else {
        return Ok(Vec::new());
    };
    if !parent.exists() {
        return Ok(Vec::new());
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config.json");
    let prefix = format!(".{name}.tmp-");
    Ok(fs::read_dir(parent)
        .with_context(|| format!("failed to inspect config directory {}", parent.display()))?
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|candidate| {
            candidate
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(&prefix))
        })
        .collect())
}

pub fn configured_models(config: &AppConfig) -> Vec<SttModelInfo> {
    default_model_catalog()
        .into_iter()
        .map(|mut model| {
            let configured_path = config.model_paths.get(&model.id).cloned();
            let managed_path = managed_model_path(config, &model);
            let downloaded_path = downloaded_model_path(config, &model);
            let explicit_path =
                first_non_empty_path([managed_path.clone(), configured_path.clone()]);
            let mut candidate_paths = [downloaded_path, managed_path, configured_path]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
            dedup_paths_preserving_order(&mut candidate_paths);

            let installed_path = first_valid_model_path(&model, candidate_paths.iter().cloned());
            let existing_invalid_path = first_existing_path(candidate_paths.iter().cloned());
            model.local_path = installed_path
                .clone()
                .or(existing_invalid_path)
                .or(explicit_path);
            model.install_status = if installed_path.is_some() {
                ModelInstallStatus::Installed
            } else if model.local_path.is_some() {
                ModelInstallStatus::Missing
            } else {
                ModelInstallStatus::NotInstalled
            };
            model
        })
        .collect()
}

pub fn selected_model(config: &AppConfig) -> Option<SttModelInfo> {
    configured_models(config)
        .into_iter()
        .find(|model| model.id == config.selected_default_model)
}

pub fn playground_selected_installed_models(config: &AppConfig) -> Vec<SttModelInfo> {
    let mut models = configured_models(config)
        .into_iter()
        .filter(|model| model.install_status.is_runnable())
        .map(|model| (model.id.clone(), model))
        .collect::<HashMap<_, _>>();

    config
        .playground_model_order
        .iter()
        .filter(|id| {
            config
                .playground_selected_models
                .iter()
                .any(|selected| selected == *id)
        })
        .filter_map(|id| models.remove(id))
        .collect()
}

pub fn model_storage_dir(config: &AppConfig) -> PathBuf {
    if config.model_storage_dir.as_os_str().is_empty() {
        default_model_storage_dir()
    } else {
        config.model_storage_dir.clone()
    }
}

pub fn runtime_storage_dir() -> PathBuf {
    scribe_project_dirs()
        .map(|dirs| dirs.data_dir().join("runtimes"))
        .unwrap_or_else(|_| PathBuf::from("runtimes"))
}

pub fn managed_model_path(config: &AppConfig, model: &SttModelInfo) -> Option<PathBuf> {
    config
        .managed_models
        .get(&model.id)
        .map(|install| install.path.clone())
        .filter(|path| !path.as_os_str().is_empty())
}

pub fn is_valid_model_install_path(model: &SttModelInfo, path: &Path) -> bool {
    match model.backend.as_str() {
        "whisper.cpp" => path.is_file(),
        "faster-whisper" => is_faster_whisper_model_dir(path),
        "Vosk" => is_vosk_model_dir(path),
        "sherpa-onnx" => is_sherpa_onnx_model_dir(path),
        "Moonshine" => is_moonshine_model_dir(path),
        "Parakeet" => is_parakeet_model_dir(path),
        _ => path.exists(),
    }
}

pub fn is_faster_whisper_model_dir(path: &Path) -> bool {
    path.is_dir() && path.join("model.bin").is_file() && path.join("config.json").is_file()
}

pub fn is_vosk_model_dir(path: &Path) -> bool {
    let graph = path.join("graph");
    let has_graph = graph.join("HCLG.fst").is_file()
        || (graph.join("HCLr.fst").is_file() && graph.join("Gr.fst").is_file());
    path.is_dir()
        && path.join("am").join("final.mdl").is_file()
        && path.join("conf").join("model.conf").is_file()
        && has_graph
}

pub fn is_sherpa_onnx_model_dir(path: &Path) -> bool {
    path.is_dir()
        && path.join("tokens.txt").is_file()
        && first_matching_file(
            path,
            &[
                "encoder-epoch-99-avg-1.int8.onnx",
                "encoder-epoch-99-avg-1.onnx",
                "encoder*.onnx",
            ],
        )
        .is_some()
        && first_matching_file(
            path,
            &[
                "decoder-epoch-99-avg-1.onnx",
                "decoder-epoch-99-avg-1.int8.onnx",
                "decoder*.onnx",
            ],
        )
        .is_some()
        && first_matching_file(
            path,
            &[
                "joiner-epoch-99-avg-1.int8.onnx",
                "joiner-epoch-99-avg-1.onnx",
                "joiner*.onnx",
            ],
        )
        .is_some()
}

pub fn is_moonshine_model_dir(path: &Path) -> bool {
    path.is_dir()
        && path.join("tokens.txt").is_file()
        && path.join("encoder_model.ort").is_file()
        && path.join("decoder_model_merged.ort").is_file()
}

pub fn is_parakeet_model_dir(path: &Path) -> bool {
    path.is_dir()
        && path.join("tokens.txt").is_file()
        && path.join("encoder.int8.onnx").is_file()
        && path.join("decoder.int8.onnx").is_file()
        && path.join("joiner.int8.onnx").is_file()
}

fn first_matching_file(root: &Path, patterns: &[&str]) -> Option<PathBuf> {
    for pattern in patterns {
        let literal = root.join(pattern);
        if literal.is_file() {
            return Some(literal);
        }
        if !pattern.contains('*') {
            continue;
        }
        let Some((prefix, suffix)) = pattern.split_once('*') else {
            continue;
        };
        let Ok(entries) = fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if path.is_file() && file_name.starts_with(prefix) && file_name.ends_with(suffix) {
                return Some(path);
            }
        }
    }
    None
}

pub fn downloaded_model_path(config: &AppConfig, model: &SttModelInfo) -> Option<PathBuf> {
    model
        .download_model
        .as_ref()
        .map(|download_model| match model.backend.as_str() {
            "whisper.cpp" => model_storage_dir(config)
                .join("whisper.cpp")
                .join(format!("ggml-{download_model}.bin")),
            "faster-whisper" => model_storage_dir(config)
                .join("faster-whisper")
                .join(&model.id),
            _ => model_storage_dir(config)
                .join(runtime_id_for_backend(&model.backend))
                .join(&model.id),
        })
}

pub fn managed_runtime_path(config: &AppConfig, backend: &str) -> Option<PathBuf> {
    config
        .managed_runtimes
        .get(&runtime_id_for_backend(backend))
        .map(|install| install.path.clone())
        .filter(|path| !path.as_os_str().is_empty())
}

pub fn runtime_id_for_backend(backend: &str) -> String {
    runtime_catalog::runtime_id_for_backend(backend)
}

pub fn normalize_config(config: &mut AppConfig) {
    let catalog = default_model_catalog();
    let catalog_ids = catalog
        .iter()
        .map(|model| model.id.clone())
        .collect::<Vec<_>>();

    migrate_legacy_model_ids(config);
    if config.model_storage_dir.as_os_str().is_empty() {
        config.model_storage_dir = default_model_storage_dir();
    }
    apply_managed_model_metadata(config);
    if let Some(device_name) = &config.audio_input_device_name
        && device_name.trim().is_empty()
    {
        config.audio_input_device_name = None;
    }
    if config.whisper_gpu_device > 16 {
        config.whisper_gpu_device = 0;
    }
    config
        .whisper_cuda_library_paths
        .retain(|path| !path.as_os_str().is_empty());
    dedup_paths_preserving_order(&mut config.whisper_cuda_library_paths);

    if !config.selected_default_model.is_empty()
        && !catalog
            .iter()
            .any(|model| model.id == config.selected_default_model)
    {
        config.selected_default_model = "whisper_cpp_tiny_en".to_owned();
    }

    config
        .playground_selected_models
        .retain(|id| catalog_ids.iter().any(|catalog_id| catalog_id == id));
    dedup_preserving_order(&mut config.playground_selected_models);

    normalize_playground_order(config, &catalog_ids);

    config.max_recording_seconds = normalize_recording_duration(config.max_recording_seconds);
    if config.paste_delay_ms == 0 {
        config.paste_delay_ms = default_paste_delay_ms();
    }
}

fn default_true() -> bool {
    true
}

pub const MAX_RECORDING_SECONDS: u32 = 120 * 60;

pub fn normalize_recording_duration(seconds: u32) -> u32 {
    if seconds == 0 {
        default_max_recording_seconds()
    } else {
        seconds.min(MAX_RECORDING_SECONDS)
    }
}

fn default_max_recording_seconds() -> u32 {
    10 * 60
}

fn default_legacy_whisper_compute_mode() -> WhisperComputeMode {
    WhisperComputeMode::Cpu
}

fn default_whisper_cuda_backend_path() -> Option<PathBuf> {
    [
        "/usr/local/lib/ollama/cuda_v13/libggml-cuda.so",
        "/usr/local/lib/ollama/cuda_v12/libggml-cuda.so",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|path| path.exists())
}

fn default_whisper_cuda_library_paths() -> Vec<PathBuf> {
    [
        "/usr/local/lib/ollama",
        "/usr/local/lib/ollama/cuda_v13",
        "/usr/local/lib/ollama/cuda_v12",
    ]
    .into_iter()
    .map(PathBuf::from)
    .filter(|path| path.exists())
    .collect()
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
    let storage_dir = model_storage_dir(config);
    config.managed_models.retain(|_, install| {
        !install.path.as_os_str().is_empty() && install.path.starts_with(&storage_dir)
    });

    for (id, path) in &config.model_paths {
        if path.exists() && path.starts_with(&storage_dir) {
            config.managed_models.entry(id.clone()).or_insert_with(|| {
                ManagedModelInstall::app_managed(path.clone(), "legacy-model-path")
            });
        }
    }

    for install in config.managed_models.values_mut() {
        if install.path.as_os_str().is_empty() {
            install.path = PathBuf::new();
        }
    }
}

fn migrate_legacy_model_ids(config: &mut AppConfig) {
    let legacy_ids = [
        "faster_whisper",
        "sherpa_onnx_streaming",
        "faster_whisper_small_en",
        "faster_whisper_medium_en",
    ];
    let migrations = [
        ("faster_whisper", "faster_whisper_small_en_gpu"),
        ("sherpa_onnx_streaming", "sherpa_onnx_zipformer_small"),
        ("faster_whisper_small_en", "faster_whisper_small_en_gpu"),
        ("faster_whisper_medium_en", "faster_whisper_medium_en_gpu"),
    ];

    for (old_id, new_id) in migrations {
        if config.selected_default_model == old_id {
            config.selected_default_model = new_id.to_owned();
        }
        for id in &mut config.playground_selected_models {
            if id == old_id {
                *id = new_id.to_owned();
            }
        }
        for id in &mut config.playground_model_order {
            if id == old_id {
                *id = new_id.to_owned();
            }
        }
        if let Some(path) = config.model_paths.remove(old_id) {
            config.model_paths.entry(new_id.to_owned()).or_insert(path);
        }
        if let Some(install) = config.managed_models.remove(old_id) {
            config
                .managed_models
                .entry(new_id.to_owned())
                .or_insert(install);
        }
    }

    config
        .model_paths
        .retain(|id, _| !legacy_ids.iter().any(|legacy_id| legacy_id == &id.as_str()));
    config
        .managed_models
        .retain(|id, _| !legacy_ids.iter().any(|legacy_id| legacy_id == &id.as_str()));
}

fn normalize_playground_order(config: &mut AppConfig, catalog_ids: &[String]) {
    config
        .playground_model_order
        .retain(|id| catalog_ids.iter().any(|catalog_id| catalog_id == id));
    dedup_preserving_order(&mut config.playground_model_order);

    for id in catalog_ids {
        if !config
            .playground_model_order
            .iter()
            .any(|existing| existing == id)
        {
            config.playground_model_order.push(id.clone());
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

    fn config_test_path(name: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!(
                "scribe-config-integrity-{name}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ))
            .join("config.json")
    }

    fn write_config_candidate(path: &Path, config: &AppConfig) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, serde_json::to_vec_pretty(config).unwrap()).unwrap();
    }

    #[test]
    fn atomic_config_recovery_handles_each_publication_phase() {
        for phase in [
            "before_backup",
            "after_backup",
            "after_publish",
            "truncated_temp",
            "invalid_current",
        ] {
            let path = config_test_path(phase);
            let root = path.parent().unwrap();
            let backup = config_sibling_path(&path, "backup");
            let temporary = unique_config_temporary_path(&path);
            let old = AppConfig {
                hotkey: "Ctrl+Alt+1".to_owned(),
                ..AppConfig::default()
            };
            let new = AppConfig {
                hotkey: "Ctrl+Alt+2".to_owned(),
                ..AppConfig::default()
            };
            match phase {
                "before_backup" => {
                    write_config_candidate(&path, &old);
                    write_config_candidate(&temporary, &new);
                }
                "after_backup" => {
                    write_config_candidate(&backup, &old);
                    write_config_candidate(&temporary, &new);
                }
                "after_publish" => {
                    write_config_candidate(&backup, &old);
                    write_config_candidate(&path, &new);
                }
                "truncated_temp" => {
                    write_config_candidate(&backup, &old);
                    fs::write(&temporary, b"{").unwrap();
                }
                "invalid_current" => {
                    fs::create_dir_all(root).unwrap();
                    fs::write(&path, b"{").unwrap();
                    write_config_candidate(&backup, &old);
                    write_config_candidate(&temporary, &new);
                }
                _ => unreachable!(),
            }

            let recovered = recover_config_file(&path).unwrap().unwrap();

            let expected = if phase == "after_publish" { &new } else { &old };
            assert_eq!(recovered.hotkey, expected.hotkey);
            assert_eq!(read_config_file(&path).unwrap().hotkey, expected.hotkey);
            assert!(!backup.exists());
            assert!(config_temporary_paths(&path).unwrap().is_empty());
            assert_eq!(
                fs::read_dir(root)
                    .unwrap()
                    .filter_map(std::result::Result::ok)
                    .filter(|entry| entry.file_name().to_string_lossy().contains(".corrupt-"))
                    .count(),
                usize::from(phase == "invalid_current")
            );
            let _ = fs::remove_dir_all(root);
        }
    }

    #[test]
    fn config_publish_sync_failure_reports_committed_and_preserves_backup() {
        let path = config_test_path("committed-warning");
        let root = path.parent().unwrap();
        let backup = config_sibling_path(&path, "backup");
        let old = AppConfig {
            hotkey: "old".to_owned(),
            ..AppConfig::default()
        };
        let new = AppConfig {
            hotkey: "new".to_owned(),
            ..AppConfig::default()
        };
        write_config_candidate(&path, &old);
        let lock = lock_config_path(&path, Duration::from_secs(1)).unwrap();
        let mut rename_count = 0;

        let warning = save_config_file_locked_with(
            &path,
            &new,
            &lock,
            |source, destination, _| {
                rename_count += 1;
                fs::rename(source, destination)?;
                Ok((rename_count == 2).then(|| io::Error::other("injected directory sync")))
            },
            |candidate| {
                if candidate.exists() {
                    fs::remove_file(candidate)?;
                }
                Ok(())
            },
        )
        .unwrap()
        .unwrap();

        assert!(warning.contains("published"));
        assert_eq!(read_config_file(&path).unwrap().hotkey, "new");
        assert_eq!(read_config_file(&backup).unwrap().hotkey, "old");
        drop(lock);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn config_precommit_sync_failure_durably_restores_old_config() {
        let path = config_test_path("precommit-restore");
        let root = path.parent().unwrap();
        let old = AppConfig {
            hotkey: "old".to_owned(),
            ..AppConfig::default()
        };
        let new = AppConfig {
            hotkey: "new".to_owned(),
            ..AppConfig::default()
        };
        write_config_candidate(&path, &old);
        let lock = lock_config_path(&path, Duration::from_secs(1)).unwrap();
        let mut rename_count = 0;

        let error = save_config_file_locked_with(
            &path,
            &new,
            &lock,
            |source, destination, _| {
                rename_count += 1;
                fs::rename(source, destination)?;
                Ok((rename_count == 1).then(|| io::Error::other("injected directory sync")))
            },
            |candidate| {
                if candidate.exists() {
                    fs::remove_file(candidate)?;
                }
                Ok(())
            },
        )
        .unwrap_err();

        assert!(error.to_string().contains("old config was restored"));
        assert_eq!(read_config_file(&path).unwrap().hotkey, "old");
        assert!(!config_sibling_path(&path, "backup").exists());
        drop(lock);
        assert_eq!(recover_config_file(&path).unwrap().unwrap().hotkey, "old");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn merged_config_save_preserves_other_process_runtime_records() {
        let path = config_test_path("merge-runtimes");
        let root = path.parent().unwrap();
        let mut first = AppConfig::default();
        first.managed_runtimes.insert(
            "vosk".to_owned(),
            ManagedRuntimeInstall::new(PathBuf::from("vosk/bin/runtime")),
        );
        let lock = lock_config_path(&path, Duration::from_secs(1)).unwrap();
        assert!(
            save_config_file_locked(&path, &first, &lock)
                .unwrap()
                .is_none()
        );
        drop(lock);

        let stale_second = AppConfig::default();
        let sherpa = ManagedRuntimeInstall::new(PathBuf::from("sherpa/bin/runtime"));
        let merged = save_config_with_runtime_update_at(
            &path,
            &stale_second,
            Some(("sherpa_onnx", Some(sherpa.clone()))),
        )
        .unwrap();

        assert!(merged.config.managed_runtimes.contains_key("vosk"));
        assert_eq!(
            merged.config.managed_runtimes.get("sherpa_onnx"),
            Some(&sherpa)
        );
        let persisted = read_config_file(&path).unwrap();
        assert_eq!(persisted.managed_runtimes, merged.config.managed_runtimes);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn recovery_chooses_newest_valid_temp_across_process_ids() {
        let path = config_test_path("temp-order");
        let root = path.parent().unwrap();
        let older = config_sibling_path(&path, "tmp-99999-00000000000000000001");
        let newer = config_sibling_path(&path, "tmp-1-00000000000000000002");
        let old = AppConfig {
            hotkey: "older".to_owned(),
            ..AppConfig::default()
        };
        let new = AppConfig {
            hotkey: "newer".to_owned(),
            ..AppConfig::default()
        };
        write_config_candidate(&older, &old);
        write_config_candidate(&newer, &new);

        let recovered = recover_config_file(&path).unwrap().unwrap();

        assert_eq!(recovered.hotkey, "newer");
        assert!(config_temporary_paths(&path).unwrap().is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn config_lock_is_exclusive_and_released() {
        let path = config_test_path("lock");
        let root = path.parent().unwrap();
        let first = lock_config_path(&path, Duration::from_millis(10)).unwrap();
        let error = lock_config_path(&path, Duration::from_millis(10)).unwrap_err();
        assert!(error.to_string().contains("another Scribe process"));
        drop(first);
        lock_config_path(&path, Duration::from_millis(10)).unwrap();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn old_config_without_playground_order_normalizes() {
        let old_config = r#"{
            "selected_default_model": "whisper_cpp_tiny_en",
            "enabled_models": ["whisper_cpp_tiny_en"],
            "hotkey": "Ctrl+Shift+Space",
            "whisper_executable_path": null,
            "model_paths": {},
            "last_used_backend": "whisper.cpp",
            "debug_mode": false,
            "max_recording_seconds": 30
        }"#;

        let mut config: AppConfig = serde_json::from_str(old_config).unwrap();
        assert!(config.playground_model_order.is_empty());

        normalize_config(&mut config);

        assert!(config.playground_model_order.len() >= default_model_catalog().len());
        assert!(
            config
                .playground_model_order
                .iter()
                .any(|id| id == "faster_whisper_turbo")
        );
        assert!(config.close_to_tray);
        assert!(config.auto_insert_transcript);
        assert!(config.restore_clipboard_after_insert);
        assert_eq!(config.hotkey_mode, HotkeyMode::Toggle);
        assert_eq!(config.paste_delay_ms, 75);
        assert_eq!(config.theme_mode, ThemeMode::Light);
        assert_eq!(config.whisper_compute_mode, WhisperComputeMode::Cpu);
        assert_eq!(
            config.playground_selected_models,
            vec!["whisper_cpp_tiny_en".to_owned()]
        );
        assert_eq!(config.whisper_gpu_device, 0);
        assert!(config.whisper_cuda_library_paths.len() <= 3);
        assert!(config.audio_input_device_name.is_none());
        assert!(!config.model_storage_dir.as_os_str().is_empty());
        assert!(config.model_storage_dir.ends_with("models"));
        assert_eq!(config.max_recording_seconds, 30);
        assert!(!config.live_transcription_enabled);
        assert!(!config.voice_editing_enabled);
        assert_eq!(
            config.voice_editing_model_tier,
            VoiceEditingModelTier::Balanced
        );
    }

    #[test]
    fn new_default_config_uses_auto_performance() {
        let config = AppConfig::default();

        assert_eq!(config.whisper_compute_mode, WhisperComputeMode::Auto);
        assert_eq!(config.whisper_gpu_device, 0);
        assert!(!config.auto_insert_transcript);
        assert!(!config.live_transcription_enabled);
        assert!(!config.voice_editing_enabled);
        assert_eq!(
            config.voice_editing_model_tier,
            VoiceEditingModelTier::Balanced
        );
        assert_eq!(config.max_recording_seconds, 600);
    }

    #[test]
    fn recording_duration_defaults_and_normalizes_legacy_values() {
        let mut serialized = serde_json::to_value(AppConfig::default()).unwrap();
        serialized
            .as_object_mut()
            .unwrap()
            .remove("max_recording_seconds");
        let missing: AppConfig = serde_json::from_value(serialized).unwrap();
        assert_eq!(missing.max_recording_seconds, 600);

        assert_eq!(normalize_recording_duration(0), 600);
        assert_eq!(normalize_recording_duration(1), 1);
        assert_eq!(normalize_recording_duration(30), 30);
        assert_eq!(normalize_recording_duration(600), 600);
        assert_eq!(normalize_recording_duration(7_200), 7_200);
        assert_eq!(
            normalize_recording_duration(u32::MAX),
            MAX_RECORDING_SECONDS
        );
    }

    #[test]
    fn live_transcription_is_opt_in_and_migrates_the_legacy_key() {
        let mut config = AppConfig::default();
        assert!(!config.live_transcription_enabled);
        config.live_transcription_enabled = true;

        let serialized = serde_json::to_value(&config).unwrap();
        assert_eq!(serialized["live_transcription_enabled"], true);
        assert!(serialized.get("live_whisper_preview").is_none());
        let restored: AppConfig = serde_json::from_value(serialized).unwrap();
        assert!(restored.live_transcription_enabled);

        let mut legacy = serde_json::to_value(AppConfig::default()).unwrap();
        let legacy = legacy.as_object_mut().unwrap();
        legacy.remove("live_transcription_enabled");
        legacy.insert("live_whisper_preview".to_owned(), true.into());
        let legacy = serde_json::Value::Object(legacy.clone());
        let migrated: AppConfig = serde_json::from_value(legacy).unwrap();
        assert!(migrated.live_transcription_enabled);
        let migrated = serde_json::to_value(migrated).unwrap();
        assert_eq!(migrated["live_transcription_enabled"], true);
        assert!(migrated.get("live_whisper_preview").is_none());
    }

    #[test]
    fn voice_editing_is_opt_in_and_model_tiers_have_stable_names() {
        let mut legacy = serde_json::to_value(AppConfig::default()).unwrap();
        let legacy = legacy.as_object_mut().unwrap();
        legacy.remove("voice_editing_enabled");
        legacy.remove("voice_editing_model_tier");
        let restored: AppConfig =
            serde_json::from_value(serde_json::Value::Object(legacy.clone())).unwrap();
        assert!(!restored.voice_editing_enabled);
        assert_eq!(
            restored.voice_editing_model_tier,
            VoiceEditingModelTier::Balanced
        );

        let compact = AppConfig {
            voice_editing_enabled: true,
            voice_editing_model_tier: VoiceEditingModelTier::Compact,
            ..Default::default()
        };
        let serialized = serde_json::to_value(&compact).unwrap();
        assert_eq!(serialized["voice_editing_enabled"], true);
        assert_eq!(serialized["voice_editing_model_tier"], "compact");
        let restored: AppConfig = serde_json::from_value(serialized).unwrap();
        assert_eq!(
            restored.voice_editing_model_tier,
            VoiceEditingModelTier::Compact
        );
        assert_eq!(VoiceEditingModelTier::ALL.len(), 2);
        assert_eq!(VoiceEditingModelTier::Compact.label(), "Compact");
        assert_eq!(
            VoiceEditingModelTier::Balanced.model_id(),
            "qwen3_1_7b_q8_0"
        );
    }

    #[test]
    fn old_cuda_value_deserializes_to_prefer_gpu() {
        let mode: WhisperComputeMode = serde_json::from_str(r#""cuda""#).unwrap();

        assert_eq!(mode, WhisperComputeMode::PreferGpu);
    }

    #[test]
    fn hotkey_mode_uses_stable_snake_case_names() {
        let config = AppConfig {
            hotkey_mode: HotkeyMode::HoldToTalk,
            ..Default::default()
        };

        let serialized = serde_json::to_string(&config).unwrap();
        assert!(serialized.contains(r#""hotkey_mode":"hold_to_talk""#));

        let parsed: AppConfig = serde_json::from_str(&serialized).unwrap();
        assert_eq!(parsed.hotkey_mode, HotkeyMode::HoldToTalk);
    }

    #[test]
    fn invalid_gpu_device_normalizes_to_default() {
        let mut config = AppConfig {
            whisper_gpu_device: 99,
            ..AppConfig::default()
        };

        normalize_config(&mut config);

        assert_eq!(config.whisper_gpu_device, 0);
    }

    #[test]
    fn duplicate_cuda_library_paths_normalize_to_unique_paths() {
        let mut config = AppConfig {
            whisper_cuda_library_paths: vec![
                PathBuf::from("/tmp/cuda"),
                PathBuf::from("/tmp/cuda"),
                PathBuf::new(),
            ],
            ..AppConfig::default()
        };

        normalize_config(&mut config);

        assert_eq!(
            config.whisper_cuda_library_paths,
            vec![PathBuf::from("/tmp/cuda")]
        );
    }

    #[test]
    fn empty_playground_selection_remains_empty_after_normalize() {
        let mut config = AppConfig {
            playground_selected_models: Vec::new(),
            ..AppConfig::default()
        };

        normalize_config(&mut config);

        assert!(config.playground_selected_models.is_empty());
        assert_eq!(config.selected_default_model, "whisper_cpp_tiny_en");
    }

    #[test]
    fn playground_selection_normalizes_invalid_and_duplicate_ids() {
        let mut config = AppConfig {
            playground_selected_models: vec![
                "faster_whisper_medium_en_gpu".to_owned(),
                "invalid".to_owned(),
                "faster_whisper_medium_en_gpu".to_owned(),
            ],
            ..AppConfig::default()
        };

        normalize_config(&mut config);
        assert_eq!(
            config.playground_selected_models,
            ["faster_whisper_medium_en_gpu"]
        );
    }

    #[test]
    fn legacy_playground_selection_keys_deserialize_and_new_key_serializes() {
        for key in ["playground_enabled_models", "enabled_models"] {
            let mut value = serde_json::to_value(AppConfig::default()).unwrap();
            value
                .as_object_mut()
                .unwrap()
                .remove("playground_selected_models");
            value
                .as_object_mut()
                .unwrap()
                .insert(key.to_owned(), serde_json::json!(["whisper_cpp_base_en"]));
            let config: AppConfig = serde_json::from_value(value).unwrap();
            assert_eq!(config.playground_selected_models, ["whisper_cpp_base_en"]);
        }

        let serialized = serde_json::to_string(&AppConfig::default()).unwrap();
        assert!(serialized.contains("playground_selected_models"));
        assert!(!serialized.contains("playground_enabled_models"));
    }

    #[test]
    fn selected_installed_playground_models_follow_persisted_drag_order() {
        let root = std::env::temp_dir().join(format!(
            "scribe-playground-selection-{}",
            std::process::id()
        ));
        let model_dir = root.join("whisper.cpp");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&model_dir).unwrap();
        fs::write(model_dir.join("ggml-tiny.en.bin"), b"tiny").unwrap();
        fs::write(model_dir.join("ggml-base.en.bin"), b"base").unwrap();
        fs::write(model_dir.join("ggml-small.en.bin"), b"small").unwrap();

        let mut config = AppConfig {
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
    fn downloaded_model_path_resolves_inside_storage_dir() {
        let config = AppConfig {
            model_storage_dir: PathBuf::from("/tmp/scribe-models"),
            ..AppConfig::default()
        };
        let model = default_model_catalog()
            .into_iter()
            .find(|model| model.id == "whisper_cpp_base_en")
            .unwrap();

        assert_eq!(
            downloaded_model_path(&config, &model).unwrap(),
            PathBuf::from("/tmp/scribe-models/whisper.cpp/ggml-base.en.bin")
        );

        let faster_model = default_model_catalog()
            .into_iter()
            .find(|model| model.id == "faster_whisper_tiny_en")
            .unwrap();

        assert_eq!(
            downloaded_model_path(&config, &faster_model).unwrap(),
            PathBuf::from("/tmp/scribe-models/faster-whisper/faster_whisper_tiny_en")
        );
    }

    #[test]
    fn vosk_directory_accepts_official_small_model_layout() {
        let root = std::env::temp_dir().join(format!("scribe-vosk-model-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("am")).unwrap();
        fs::create_dir_all(root.join("conf")).unwrap();
        fs::create_dir_all(root.join("graph")).unwrap();
        fs::write(root.join("am").join("final.mdl"), b"model").unwrap();
        fs::write(root.join("conf").join("model.conf"), b"conf").unwrap();

        assert!(!is_vosk_model_dir(&root));

        fs::write(root.join("graph").join("HCLr.fst"), b"hclr").unwrap();
        assert!(!is_vosk_model_dir(&root));

        fs::write(root.join("graph").join("Gr.fst"), b"gr").unwrap();

        assert!(is_vosk_model_dir(&root));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn sherpa_family_directories_require_backend_specific_model_files() {
        let root = std::env::temp_dir().join(format!("scribe-sherpa-model-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let sherpa = root.join("sherpa");
        let moonshine = root.join("moonshine");
        let parakeet = root.join("parakeet");
        fs::create_dir_all(&sherpa).unwrap();
        fs::create_dir_all(&moonshine).unwrap();
        fs::create_dir_all(&parakeet).unwrap();

        fs::write(sherpa.join("tokens.txt"), b"tokens").unwrap();
        fs::write(sherpa.join("encoder-epoch-99-avg-1.int8.onnx"), b"encoder").unwrap();
        fs::write(sherpa.join("decoder-epoch-99-avg-1.onnx"), b"decoder").unwrap();
        assert!(!is_sherpa_onnx_model_dir(&sherpa));
        fs::write(sherpa.join("joiner-epoch-99-avg-1.int8.onnx"), b"joiner").unwrap();
        assert!(is_sherpa_onnx_model_dir(&sherpa));

        fs::write(moonshine.join("tokens.txt"), b"tokens").unwrap();
        fs::write(moonshine.join("encoder_model.ort"), b"encoder").unwrap();
        assert!(!is_moonshine_model_dir(&moonshine));
        fs::write(moonshine.join("decoder_model_merged.ort"), b"decoder").unwrap();
        assert!(is_moonshine_model_dir(&moonshine));

        fs::write(parakeet.join("tokens.txt"), b"tokens").unwrap();
        fs::write(parakeet.join("encoder.int8.onnx"), b"encoder").unwrap();
        fs::write(parakeet.join("decoder.int8.onnx"), b"decoder").unwrap();
        assert!(!is_parakeet_model_dir(&parakeet));
        fs::write(parakeet.join("joiner.int8.onnx"), b"joiner").unwrap();
        assert!(is_parakeet_model_dir(&parakeet));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn downloaded_model_file_wins_over_stale_configured_path() {
        let temp_dir =
            std::env::temp_dir().join(format!("scribe-config-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp_dir);
        let model_dir = temp_dir.join("whisper.cpp");
        fs::create_dir_all(&model_dir).unwrap();
        let downloaded_path = model_dir.join("ggml-base.en.bin");
        fs::write(&downloaded_path, b"model").unwrap();

        let mut model_paths = HashMap::new();
        model_paths.insert(
            "whisper_cpp_base_en".to_owned(),
            temp_dir.join("missing-model.bin"),
        );
        let config = AppConfig {
            model_storage_dir: temp_dir.clone(),
            model_paths,
            ..AppConfig::default()
        };

        let model = configured_models(&config)
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
        let model_path = app_storage.join("whisper.cpp").join("ggml-base.en.bin");
        fs::create_dir_all(model_path.parent().unwrap()).unwrap();
        fs::write(&model_path, b"model").unwrap();

        let mut model_paths = HashMap::new();
        model_paths.insert("whisper_cpp_base_en".to_owned(), model_path.clone());
        let mut config = AppConfig {
            model_storage_dir: app_storage,
            model_paths,
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
    fn legacy_managed_install_records_deserialize_with_empty_metadata() {
        let model: ManagedModelInstall =
            serde_json::from_str(r#"{"path":"/tmp/scribe/model.bin"}"#).unwrap();
        let runtime: ManagedRuntimeInstall =
            serde_json::from_str(r#"{"path":"/tmp/scribe/runtime/bin/runner"}"#).unwrap();

        assert_eq!(model.path, PathBuf::from("/tmp/scribe/model.bin"));
        assert!(model.source.is_none());
        assert!(model.version.is_none());
        assert!(model.sha256.is_none());
        assert!(model.platform.is_none());
        assert!(model.installed_at_unix_seconds.is_none());

        assert_eq!(
            runtime.path,
            PathBuf::from("/tmp/scribe/runtime/bin/runner")
        );
        assert!(runtime.source.is_none());
        assert!(runtime.version.is_none());
        assert!(runtime.sha256.is_none());
        assert!(runtime.platform.is_none());
        assert!(runtime.installed_at_unix_seconds.is_none());
    }

    #[test]
    fn app_managed_install_records_include_source_and_platform() {
        let model =
            ManagedModelInstall::app_managed(PathBuf::from("/tmp/scribe/model.bin"), "download");
        let runtime = ManagedRuntimeInstall::app_managed(
            PathBuf::from("/tmp/scribe/runtime/bin/runner"),
            "packaged-runtime",
        );

        assert_eq!(model.source.as_deref(), Some("download"));
        assert_eq!(runtime.source.as_deref(), Some("packaged-runtime"));
        assert_eq!(
            model.platform.as_deref(),
            Some(current_platform_key().as_str())
        );
        assert_eq!(
            runtime.platform.as_deref(),
            Some(current_platform_key().as_str())
        );
        assert!(model.installed_at_unix_seconds.is_some());
        assert!(runtime.installed_at_unix_seconds.is_some());
    }

    #[test]
    fn external_configured_model_path_stays_readable_but_not_managed() {
        let temp_dir = std::env::temp_dir().join(format!(
            "scribe-external-config-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&temp_dir);
        let app_storage = temp_dir.join("app-models");
        let external_path = temp_dir.join("external").join("ggml-base.en.bin");
        fs::create_dir_all(external_path.parent().unwrap()).unwrap();
        fs::write(&external_path, b"model").unwrap();

        let mut model_paths = HashMap::new();
        model_paths.insert("whisper_cpp_base_en".to_owned(), external_path.clone());
        let mut config = AppConfig {
            model_storage_dir: app_storage,
            model_paths,
            ..AppConfig::default()
        };

        normalize_config(&mut config);
        let model = configured_models(&config)
            .into_iter()
            .find(|model| model.id == "whisper_cpp_base_en")
            .unwrap();

        assert_eq!(managed_model_path(&config, &model), None);
        assert_eq!(model.local_path.as_deref(), Some(external_path.as_path()));
        assert_eq!(model.install_status, ModelInstallStatus::Installed);

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn faster_whisper_directory_requires_ctranslate2_payload() {
        let temp_dir =
            std::env::temp_dir().join(format!("scribe-fw-config-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp_dir);
        let model_dir = temp_dir
            .join("faster-whisper")
            .join("faster_whisper_small_en_gpu");
        fs::create_dir_all(&model_dir).unwrap();
        fs::write(model_dir.join("config.json"), b"{}").unwrap();

        let config = AppConfig {
            model_storage_dir: temp_dir.clone(),
            ..AppConfig::default()
        };
        let model = configured_models(&config)
            .into_iter()
            .find(|model| model.id == "faster_whisper_small_en_gpu")
            .unwrap();

        assert_eq!(model.local_path.as_deref(), Some(model_dir.as_path()));
        assert_eq!(model.install_status, ModelInstallStatus::Missing);

        fs::write(model_dir.join("model.bin"), b"model").unwrap();
        let model = configured_models(&config)
            .into_iter()
            .find(|model| model.id == "faster_whisper_small_en_gpu")
            .unwrap();

        assert_eq!(model.local_path.as_deref(), Some(model_dir.as_path()));
        assert_eq!(model.install_status, ModelInstallStatus::Installed);

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn runtime_ids_are_stable_slugs() {
        assert_eq!(runtime_id_for_backend("whisper.cpp"), "whisper_cpp");
        assert_eq!(runtime_id_for_backend("sherpa-onnx"), "sherpa_onnx");
    }
}
