use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::super::{
    HotkeyMode, ManagedModelInstall, ManagedRuntimeInstall, ThemeMode, default_model_storage_dir,
    default_paste_delay_ms, default_playground_model_order, default_whisper_cuda_backend_path,
    default_whisper_cuda_library_paths,
};
use crate::transcription::AccelerationPreference;

pub const CURRENT_SCHEMA_VERSION: u32 = 1;
pub type UnknownFields = BTreeMap<String, Value>;

#[derive(Clone, Debug, Serialize)]
pub struct AppConfig {
    pub schema_version: u32,
    pub general: GeneralSettings,
    pub recording: RecordingSettings,
    pub streaming: StreamingSettings,
    pub output: OutputSettings,
    pub overlay: OverlaySettings,
    pub history: HistorySettings,
    pub performance: PerformanceSettings,
    pub developer: DeveloperSettings,
    #[serde(flatten)]
    pub unknown: UnknownFields,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralSettings {
    pub selected_default_model: String,
    pub playground_selected_models: Vec<String>,
    pub playground_model_order: Vec<String>,
    pub managed_models: HashMap<String, ManagedModelInstall>,
    pub managed_runtimes: HashMap<String, ManagedRuntimeInstall>,
    pub model_storage_dir: PathBuf,
    pub model_paths: HashMap<String, PathBuf>,
    pub last_used_backend: String,
    pub theme_mode: ThemeMode,
    pub close_to_tray: bool,
    #[serde(flatten)]
    pub unknown: UnknownFields,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct RecordingSettings {
    pub hotkey: String,
    pub hotkey_mode: HotkeyMode,
    pub audio_input_device_name: Option<String>,
    pub max_recording_seconds: u32,
    #[serde(flatten)]
    pub unknown: UnknownFields,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct StreamingSettings {
    #[serde(flatten)]
    pub unknown: UnknownFields,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct OutputSettings {
    pub auto_insert_transcript: bool,
    pub restore_clipboard_after_insert: bool,
    pub paste_delay_ms: u64,
    #[serde(flatten)]
    pub unknown: UnknownFields,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlayMode {
    #[default]
    Live,
    Minimal,
    Off,
}

impl OverlayMode {
    pub const ALL: [Self; 3] = [Self::Live, Self::Minimal, Self::Off];

    pub fn label(self) -> &'static str {
        match self {
            Self::Live => "Live",
            Self::Minimal => "Minimal",
            Self::Off => "Off",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlayPosition {
    Top,
    #[default]
    Bottom,
}

impl OverlayPosition {
    pub const ALL: [Self; 2] = [Self::Top, Self::Bottom];

    pub fn label(self) -> &'static str {
        match self {
            Self::Top => "Top",
            Self::Bottom => "Bottom",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct OverlaySettings {
    pub mode: OverlayMode,
    pub position: OverlayPosition,
    #[serde(flatten)]
    pub unknown: UnknownFields,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct HistorySettings {
    #[serde(flatten)]
    pub unknown: UnknownFields,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct PerformanceSettings {
    pub acceleration_preference: AccelerationPreference,
    pub whisper_gpu_device: u32,
    pub whisper_cuda_backend_path: Option<PathBuf>,
    pub whisper_cuda_library_paths: Vec<PathBuf>,
    #[serde(flatten)]
    pub unknown: UnknownFields,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct DeveloperSettings {
    pub whisper_executable_path: Option<PathBuf>,
    pub debug_mode: bool,
    #[serde(flatten)]
    pub unknown: UnknownFields,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            general: GeneralSettings::default(),
            recording: RecordingSettings::default(),
            streaming: StreamingSettings::default(),
            output: OutputSettings::default(),
            overlay: OverlaySettings::default(),
            history: HistorySettings::default(),
            performance: PerformanceSettings::default(),
            developer: DeveloperSettings::default(),
            unknown: UnknownFields::new(),
        }
    }
}

impl Default for GeneralSettings {
    fn default() -> Self {
        Self {
            selected_default_model: "whisper_cpp_tiny_en".to_owned(),
            playground_selected_models: vec!["whisper_cpp_tiny_en".to_owned()],
            playground_model_order: default_playground_model_order(),
            managed_models: HashMap::new(),
            managed_runtimes: HashMap::new(),
            model_storage_dir: default_model_storage_dir(),
            model_paths: HashMap::new(),
            last_used_backend: "whisper.cpp".to_owned(),
            theme_mode: ThemeMode::Light,
            close_to_tray: true,
            unknown: UnknownFields::new(),
        }
    }
}

impl Default for RecordingSettings {
    fn default() -> Self {
        Self {
            hotkey: "Ctrl+Shift+Space".to_owned(),
            hotkey_mode: HotkeyMode::Toggle,
            audio_input_device_name: None,
            max_recording_seconds: 30,
            unknown: UnknownFields::new(),
        }
    }
}

impl Default for OutputSettings {
    fn default() -> Self {
        Self {
            auto_insert_transcript: false,
            restore_clipboard_after_insert: true,
            paste_delay_ms: default_paste_delay_ms(),
            unknown: UnknownFields::new(),
        }
    }
}

impl Default for OverlaySettings {
    fn default() -> Self {
        Self {
            mode: OverlayMode::Live,
            position: OverlayPosition::Bottom,
            unknown: UnknownFields::new(),
        }
    }
}

impl Default for PerformanceSettings {
    fn default() -> Self {
        Self {
            acceleration_preference: AccelerationPreference::Auto,
            whisper_gpu_device: 0,
            whisper_cuda_backend_path: default_whisper_cuda_backend_path(),
            whisper_cuda_library_paths: default_whisper_cuda_library_paths(),
            unknown: UnknownFields::new(),
        }
    }
}
