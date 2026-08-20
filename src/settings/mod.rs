mod migrations;
mod repository;
mod schema;

pub use repository::SettingsStore;
pub use schema::{
    AppConfig, CURRENT_SCHEMA_VERSION, DEFAULT_SPEECH_PROBABILITY_THRESHOLD, DeveloperSettings,
    GeneralSettings, HistoryMode, HistorySettings, MAX_SPEECH_PROBABILITY_THRESHOLD,
    MIN_SPEECH_PROBABILITY_THRESHOLD, OutputSettings, OverlayMode, OverlayPosition,
    OverlaySettings, PerformanceSettings, RecordingSettings, StreamingMode, StreamingSettings,
};

pub(crate) use migrations::parse_settings_value_with_diagnostics;
pub(crate) use repository::{
    artifact_config_fingerprint, atomic_write_bytes, load_from_path, save_to_path,
};
