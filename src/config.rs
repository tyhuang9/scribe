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
            hotkey: "Ctrl+Shift+Space".to_owned(),
            whisper_executable_path: None,
            model_paths: HashMap::new(),
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
    if !catalog
        .iter()
        .any(|model| model.id == config.selected_default_model)
    {
        config.selected_default_model = "whisper_cpp_tiny_en".to_owned();
    }

    config
        .enabled_models
        .retain(|id| catalog.iter().any(|model| &model.id == id));

    if config.enabled_models.is_empty() {
        config
            .enabled_models
            .push(config.selected_default_model.clone());
    }

    if config.max_recording_seconds == 0 {
        config.max_recording_seconds = 30;
    }
}
