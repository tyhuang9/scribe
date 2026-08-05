mod migrations;
mod repository;
mod schema;

pub use repository::SettingsStore;
pub use schema::{
    AppConfig, CURRENT_SCHEMA_VERSION, DeveloperSettings, GeneralSettings, HistorySettings,
    OutputSettings, OverlayMode, OverlayPosition, OverlaySettings, PerformanceSettings,
    RecordingSettings, StreamingSettings,
};

pub(crate) use migrations::parse_settings_value_with_diagnostics;
pub(crate) use repository::{load_from_path, save_to_path};
