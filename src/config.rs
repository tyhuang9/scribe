use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::models::{ModelInstallStatus, SttModelInfo, default_model_catalog};
use crate::runtime_catalog;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppConfig {
    pub selected_default_model: String,
    #[serde(default, alias = "enabled_models")]
    pub playground_enabled_models: Vec<String>,
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
    pub max_recording_seconds: u32,
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
    pub installed_at_unix_seconds: Option<u64>,
}

impl ManagedModelInstall {
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

impl HotkeyMode {
    pub const ALL: [HotkeyMode; 2] = [HotkeyMode::Toggle, HotkeyMode::HoldToTalk];

    pub fn label(self) -> &'static str {
        match self {
            Self::Toggle => "Toggle record",
            Self::HoldToTalk => "Hold to talk",
        }
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            selected_default_model: "whisper_cpp_tiny_en".to_owned(),
            playground_enabled_models: vec!["whisper_cpp_tiny_en".to_owned()],
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
            max_recording_seconds: 30,
            close_to_tray: true,
            auto_insert_transcript: true,
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

fn read_config_file(path: &PathBuf) -> Result<AppConfig> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read config {}", path.display()))?;
    serde_json::from_str(&content)
        .with_context(|| format!("failed to parse config {}", path.display()))
}

pub fn save_config(config: &AppConfig) -> Result<()> {
    let path = config_file_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create config directory {}", parent.display()))?;
    }

    let content = serde_json::to_string_pretty(config)?;
    fs::write(&path, content).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

pub fn configured_models(config: &AppConfig) -> Vec<SttModelInfo> {
    default_model_catalog()
        .into_iter()
        .map(|mut model| {
            model.enabled = config
                .playground_enabled_models
                .iter()
                .any(|id| id == &model.id);
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

pub fn configured_models_for_playground(config: &AppConfig) -> Vec<SttModelInfo> {
    let mut models = configured_models(config);
    let order = &config.playground_model_order;

    models.sort_by_key(|model| {
        let active_group = if model.id == config.selected_default_model {
            0
        } else {
            1
        };
        let enabled_group = if model.enabled { 0 } else { 1 };
        let order_index = order
            .iter()
            .position(|id| id == &model.id)
            .unwrap_or(usize::MAX);
        (active_group, enabled_group, order_index)
    });

    models
}

pub fn selected_model(config: &AppConfig) -> Option<SttModelInfo> {
    configured_models(config)
        .into_iter()
        .find(|model| model.id == config.selected_default_model)
}

pub fn playground_enabled_models(config: &AppConfig) -> Vec<SttModelInfo> {
    configured_models(config)
        .into_iter()
        .filter(|model| model.enabled)
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
        .playground_enabled_models
        .retain(|id| catalog_ids.iter().any(|catalog_id| catalog_id == id));
    dedup_preserving_order(&mut config.playground_enabled_models);

    normalize_playground_order(config, &catalog_ids);

    if config.max_recording_seconds == 0 {
        config.max_recording_seconds = 30;
    }
    if config.paste_delay_ms == 0 {
        config.paste_delay_ms = default_paste_delay_ms();
    }
}

fn default_true() -> bool {
    true
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
        for id in &mut config.playground_enabled_models {
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
            config.playground_enabled_models,
            vec!["whisper_cpp_tiny_en".to_owned()]
        );
        assert_eq!(config.whisper_gpu_device, 0);
        assert!(config.whisper_cuda_library_paths.len() <= 3);
        assert!(config.audio_input_device_name.is_none());
        assert!(!config.model_storage_dir.as_os_str().is_empty());
        assert!(config.model_storage_dir.ends_with("models"));
    }

    #[test]
    fn new_default_config_uses_auto_performance() {
        let config = AppConfig::default();

        assert_eq!(config.whisper_compute_mode, WhisperComputeMode::Auto);
        assert_eq!(config.whisper_gpu_device, 0);
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
    fn empty_enabled_models_remain_empty_after_normalize() {
        let mut config = AppConfig {
            playground_enabled_models: Vec::new(),
            ..AppConfig::default()
        };

        normalize_config(&mut config);

        assert!(config.playground_enabled_models.is_empty());
        assert_eq!(config.selected_default_model, "whisper_cpp_tiny_en");
    }

    #[test]
    fn playground_models_pin_active_model_then_sort_enabled_by_manual_order() {
        let mut config = AppConfig {
            selected_default_model: "whisper_cpp_tiny_en".to_owned(),
            playground_enabled_models: vec![
                "faster_whisper_medium_en_gpu".to_owned(),
                "whisper_cpp_tiny_en".to_owned(),
            ],
            playground_model_order: vec![
                "faster_whisper_medium_en_gpu".to_owned(),
                "vosk_small_en".to_owned(),
                "whisper_cpp_tiny_en".to_owned(),
            ],
            ..AppConfig::default()
        };

        normalize_config(&mut config);
        let ids = configured_models_for_playground(&config)
            .into_iter()
            .map(|model| model.id)
            .collect::<Vec<_>>();

        assert_eq!(ids[0], "whisper_cpp_tiny_en");
        assert_eq!(ids[1], "faster_whisper_medium_en_gpu");
        assert!(
            ids.iter()
                .position(|id| id == "vosk_small_en")
                .expect("vosk model should be present")
                > 1
        );
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
