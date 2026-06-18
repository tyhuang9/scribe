use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::models::{SttModelInfo, default_model_catalog};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppConfig {
    pub selected_default_model: String,
    pub enabled_models: Vec<String>,
    #[serde(default)]
    pub playground_model_order: Vec<String>,
    pub hotkey: String,
    pub whisper_executable_path: Option<PathBuf>,
    pub model_paths: HashMap<String, PathBuf>,
    pub last_used_backend: String,
    pub debug_mode: bool,
    pub max_recording_seconds: u32,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            selected_default_model: "whisper_cpp_tiny_en".to_owned(),
            enabled_models: vec!["whisper_cpp_tiny_en".to_owned()],
            playground_model_order: default_playground_model_order(),
            hotkey: "Ctrl+Shift+Space".to_owned(),
            whisper_executable_path: None,
            model_paths: default_model_paths(),
            last_used_backend: "whisper.cpp".to_owned(),
            debug_mode: false,
            max_recording_seconds: 30,
        }
    }
}

pub fn project_dirs() -> Result<ProjectDirs> {
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
        let config = AppConfig::default();
        save_config(&config)?;
        return Ok((config, path));
    }

    let content = fs::read_to_string(&path)
        .with_context(|| format!("failed to read config {}", path.display()))?;
    let mut config: AppConfig = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse config {}", path.display()))?;
    normalize_config(&mut config);
    Ok((config, path))
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
            model.enabled = config.enabled_models.iter().any(|id| id == &model.id);
            model.local_path = config.model_paths.get(&model.id).cloned();
            model.download_status = match &model.local_path {
                Some(path) if path.exists() => "Configured".to_owned(),
                Some(_) => "Missing file".to_owned(),
                None => "Not configured".to_owned(),
            };
            model
        })
        .collect()
}

pub fn configured_models_for_playground(config: &AppConfig) -> Vec<SttModelInfo> {
    let mut models = configured_models(config);
    let order = &config.playground_model_order;

    models.sort_by_key(|model| {
        let enabled_group = if model.enabled { 0 } else { 1 };
        let order_index = order
            .iter()
            .position(|id| id == &model.id)
            .unwrap_or(usize::MAX);
        (enabled_group, order_index)
    });

    models
}

pub fn selected_model(config: &AppConfig) -> Option<SttModelInfo> {
    configured_models(config)
        .into_iter()
        .find(|model| model.id == config.selected_default_model)
}

pub fn enabled_models(config: &AppConfig) -> Vec<SttModelInfo> {
    configured_models(config)
        .into_iter()
        .filter(|model| model.enabled)
        .collect()
}

pub fn normalize_config(config: &mut AppConfig) {
    let catalog = default_model_catalog();
    let catalog_ids = catalog
        .iter()
        .map(|model| model.id.clone())
        .collect::<Vec<_>>();

    migrate_legacy_model_ids(config);
    apply_default_model_paths(config);

    if !catalog
        .iter()
        .any(|model| model.id == config.selected_default_model)
    {
        config.selected_default_model = "whisper_cpp_tiny_en".to_owned();
    }

    config
        .enabled_models
        .retain(|id| catalog_ids.iter().any(|catalog_id| catalog_id == id));
    dedup_preserving_order(&mut config.enabled_models);

    if config.enabled_models.is_empty() {
        config
            .enabled_models
            .push(config.selected_default_model.clone());
    }

    normalize_playground_order(config, &catalog_ids);

    if config.max_recording_seconds == 0 {
        config.max_recording_seconds = 30;
    }
}

fn default_playground_model_order() -> Vec<String> {
    default_model_catalog()
        .into_iter()
        .map(|model| model.id)
        .collect()
}

fn default_model_paths() -> HashMap<String, PathBuf> {
    [
        (
            "whisper_cpp_tiny_en",
            "/home/tyhuang/Projects/whisper.cpp/models/ggml-tiny.en.bin",
        ),
        (
            "whisper_cpp_base_en",
            "/home/tyhuang/Projects/whisper.cpp/models/ggml-base.en.bin",
        ),
        (
            "whisper_cpp_small_en",
            "/home/tyhuang/Projects/whisper.cpp/models/ggml-small.en.bin",
        ),
        (
            "whisper_cpp_medium_en",
            "/home/tyhuang/Projects/whisper.cpp/models/ggml-medium.en.bin",
        ),
        (
            "faster_whisper_small_en_gpu",
            "/home/tyhuang/Projects/stt-models/faster-whisper-small.en",
        ),
        (
            "faster_whisper_medium_en_gpu",
            "/home/tyhuang/Projects/stt-models/faster-whisper-medium.en",
        ),
        (
            "sherpa_onnx_zipformer_small",
            "/home/tyhuang/Projects/stt-models/sherpa-onnx-zipformer-small-en",
        ),
        (
            "moonshine",
            "/home/tyhuang/Projects/stt-models/moonshine/examples/ios/Transcriber/models/small-streaming-en",
        ),
        (
            "parakeet_0_6b",
            "/home/tyhuang/Projects/stt-models/parakeet-tdt-0.6b-v3/parakeet-tdt-0.6b-v3.nemo",
        ),
    ]
    .into_iter()
    .filter_map(|(id, path)| {
        let path = PathBuf::from(path);
        path.exists().then(|| (id.to_owned(), path))
    })
    .collect()
}

fn apply_default_model_paths(config: &mut AppConfig) {
    for (id, path) in default_model_paths() {
        config.model_paths.entry(id).or_insert(path);
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
        for id in &mut config.enabled_models {
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
    }

    config
        .model_paths
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
    }

    #[test]
    fn playground_models_sort_enabled_first_then_manual_order() {
        let mut config = AppConfig {
            enabled_models: vec![
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

        assert_eq!(ids[0], "faster_whisper_medium_en_gpu");
        assert_eq!(ids[1], "whisper_cpp_tiny_en");
        assert!(
            ids.iter()
                .position(|id| id == "vosk_small_en")
                .expect("vosk model should be present")
                > 1
        );
    }
}
