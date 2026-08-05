mod migrations;
mod repository;
mod schema;

pub use repository::SettingsStore;
pub use schema::{
    AppConfig, CURRENT_SCHEMA_VERSION, DEFAULT_MANUAL_ACTIVATION_RMS, DeveloperSettings,
    GeneralSettings, HistoryMode, HistorySettings, MAX_MANUAL_ACTIVATION_RMS,
    MIN_MANUAL_ACTIVATION_RMS, OutputSettings, OverlayMode, OverlayPosition, OverlaySettings,
    PerformanceSettings, RecordingSettings, StreamingMode, StreamingSettings,
};

pub(crate) use migrations::parse_settings_value_with_diagnostics;
pub(crate) use repository::{
    artifact_config_fingerprint, atomic_write_bytes, load_from_path, save_to_path,
};
