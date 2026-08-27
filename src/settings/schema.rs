use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::super::{
    HotkeyMode, ImportedGgufModelInstall, ManagedModelInstall, ManagedRemoteModelInstall,
    ManagedRuntimeInstall, ThemeMode, default_model_storage_dir, default_paste_delay_ms,
    default_playground_model_order, default_whisper_cuda_backend_path,
    default_whisper_cuda_library_paths,
};
use crate::model_catalog::BUNDLED_BASE_MODEL_ID;
use crate::transcription::AccelerationPreference;

pub const CURRENT_SCHEMA_VERSION: u32 = 3;
pub const DEFAULT_SPEECH_CONFIRMATION_MS: u32 = 150;
pub const DEFAULT_INTERNAL_PAUSE_MS: u32 = 450;
pub const DEFAULT_ENDPOINT_SILENCE_MS: u32 = 900;
pub const DEFAULT_PRE_ROLL_MS: u32 = 250;
pub const DEFAULT_POST_ROLL_MS: u32 = 200;
pub const DEFAULT_INPUT_THRESHOLD_DBFS: f32 = -42.0;
pub const MIN_INPUT_THRESHOLD_DBFS: f32 = -72.0;
pub const MAX_INPUT_THRESHOLD_DBFS: f32 = 0.0;
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
    /// Bundled artifacts the user explicitly removed. The opt-out survives
    /// application updates until the user chooses Install for that model.
    pub excluded_bundled_model_ids: Vec<String>,
    pub playground_selected_models: Vec<String>,
    pub playground_model_order: Vec<String>,
    pub managed_models: HashMap<String, ManagedModelInstall>,
    pub managed_remote_models: HashMap<String, ManagedRemoteModelInstall>,
    pub imported_gguf_models: HashMap<String, ImportedGgufModelInstall>,
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
    pub vad_enabled: bool,
    pub speech_detection_mode: SpeechDetectionMode,
    pub input_threshold_dbfs: f32,
    pub speech_confirmation_ms: u32,
    pub internal_pause_ms: u32,
    pub endpoint_silence_ms: u32,
    pub pre_roll_ms: u32,
    pub post_roll_ms: u32,
    #[serde(flatten)]
    pub unknown: UnknownFields,
}

#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeechDetectionMode {
    #[default]
    Ai,
    ManualThreshold,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamingMode {
    #[default]
    Auto,
    Rolling,
    FinalOnly,
}

impl StreamingMode {
    #[allow(dead_code)]
    pub const ALL: [Self; 3] = [Self::Auto, Self::Rolling, Self::FinalOnly];

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Rolling => "Rolling preview",
            Self::FinalOnly => "Final text only",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct StreamingSettings {
    pub mode: StreamingMode,
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
    Live,
    #[default]
    Minimal,
    Off,
}

impl OverlayMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Live => "Live preview",
            Self::Minimal => "Compact status",
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
    #[allow(dead_code)]
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryMode {
    Off,
    #[default]
    TranscriptOnly,
    TranscriptAndAudio,
}

impl HistoryMode {
    #[allow(dead_code)]
    pub const ALL: [Self; 3] = [Self::Off, Self::TranscriptOnly, Self::TranscriptAndAudio];

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::TranscriptOnly => "Transcript only",
            Self::TranscriptAndAudio => "Transcript and audio",
        }
    }

    pub fn stores_transcripts(self) -> bool {
        !matches!(self, Self::Off)
    }

    pub fn stores_audio(self) -> bool {
        matches!(self, Self::TranscriptAndAudio)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct HistorySettings {
    pub mode: HistoryMode,
    pub max_unpinned_entries: u32,
    pub transcript_retention_days: Option<u32>,
    pub audio_retention_days: Option<u32>,
    pub store_application_identity: bool,
    #[serde(flatten)]
    pub unknown: UnknownFields,
}

impl Default for HistorySettings {
    fn default() -> Self {
        Self {
            mode: HistoryMode::TranscriptOnly,
            max_unpinned_entries: 20,
            transcript_retention_days: None,
            audio_retention_days: None,
            store_application_identity: false,
            unknown: UnknownFields::new(),
        }
    }
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
            selected_default_model: BUNDLED_BASE_MODEL_ID.to_owned(),
            excluded_bundled_model_ids: Vec::new(),
            playground_selected_models: vec![BUNDLED_BASE_MODEL_ID.to_owned()],
            playground_model_order: default_playground_model_order(),
            managed_models: HashMap::new(),
            managed_remote_models: HashMap::new(),
            imported_gguf_models: HashMap::new(),
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
            hotkey: "Ctrl+Space".to_owned(),
            hotkey_mode: HotkeyMode::HoldToTalk,
            audio_input_device_name: None,
            max_recording_seconds: 30,
            vad_enabled: true,
            speech_detection_mode: SpeechDetectionMode::Ai,
            input_threshold_dbfs: DEFAULT_INPUT_THRESHOLD_DBFS,
            speech_confirmation_ms: DEFAULT_SPEECH_CONFIRMATION_MS,
            internal_pause_ms: DEFAULT_INTERNAL_PAUSE_MS,
            endpoint_silence_ms: DEFAULT_ENDPOINT_SILENCE_MS,
            pre_roll_ms: DEFAULT_PRE_ROLL_MS,
            post_roll_ms: DEFAULT_POST_ROLL_MS,
            unknown: UnknownFields::new(),
        }
    }
}

impl Default for StreamingSettings {
    fn default() -> Self {
        Self {
            mode: StreamingMode::Auto,
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
            mode: OverlayMode::Minimal,
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
