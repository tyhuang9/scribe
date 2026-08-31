mod migrations;
mod repository;
mod schema;

pub use repository::SettingsStore;
pub use schema::{
    AppConfig, CURRENT_SCHEMA_VERSION, DEFAULT_INPUT_THRESHOLD_DBFS, HistoryMode,
    MAX_INPUT_THRESHOLD_DBFS, MIN_INPUT_THRESHOLD_DBFS, OverlayMode, OverlayPosition,
    PendingOnnxRemoval, SpeechDetectionMode, StreamingMode,
};
#[cfg(test)]
pub use schema::{GeneralSettings, RecordingSettings};

pub(crate) use migrations::{discard_retired_config_values, parse_settings_value_with_diagnostics};
pub(crate) use repository::{
    SettingsTransaction, artifact_config_fingerprint, atomic_write_bytes, load_from_path,
    save_artifacts_to_path, save_to_path,
};
