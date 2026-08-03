use std::collections::HashMap;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value};

use super::schema::{
    AppConfig, CURRENT_SCHEMA_VERSION, DeveloperSettings, GeneralSettings, HistorySettings,
    OutputSettings, OverlaySettings, PerformanceSettings, RecordingSettings, StreamingSettings,
    UnknownFields,
};
use crate::transcription::AccelerationPreference;

impl<'de> Deserialize<'de> for AppConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(parse_settings_value(Value::deserialize(deserializer)?))
    }
}

pub(crate) fn parse_settings_value(value: Value) -> AppConfig {
    let Value::Object(root) = value else {
        return AppConfig::default();
    };

    if root.keys().any(|key| {
        matches!(
            key.as_str(),
            "general"
                | "recording"
                | "streaming"
                | "transcription"
                | "output"
                | "overlay"
                | "history"
                | "performance"
                | "developer"
        )
    }) {
        parse_sectioned(root)
    } else {
        migrate_legacy_flat(root)
    }
}

fn parse_sectioned(mut root: Map<String, Value>) -> AppConfig {
    let stored_schema_version = take(&mut root, "schema_version", &[], CURRENT_SCHEMA_VERSION);
    let schema_version = if stored_schema_version <= CURRENT_SCHEMA_VERSION {
        CURRENT_SCHEMA_VERSION
    } else {
        stored_schema_version
    };
    let general = parse_general(take_section(&mut root, "general", &[]));
    let recording = parse_recording(take_section(&mut root, "recording", &[]));
    let streaming = parse_streaming(take_section(&mut root, "streaming", &["transcription"]));
    let output = parse_output(take_section(&mut root, "output", &[]));
    let overlay = parse_overlay(take_section(&mut root, "overlay", &[]));
    let history = parse_history(take_section(&mut root, "history", &[]));
    let performance = parse_performance(take_section(&mut root, "performance", &[]));
    let developer = parse_developer(take_section(&mut root, "developer", &[]));

    AppConfig {
        schema_version,
        general,
        recording,
        streaming,
        output,
        overlay,
        history,
        performance,
        developer,
        unknown: into_unknown(root),
    }
}

fn migrate_legacy_flat(mut root: Map<String, Value>) -> AppConfig {
    let mut config = AppConfig::default();
    config.general.playground_model_order.clear();
    config.performance.acceleration_preference = AccelerationPreference::Cpu;
    config.output.auto_insert_transcript = true;

    config.general.selected_default_model = take(
        &mut root,
        "selected_default_model",
        &[],
        config.general.selected_default_model,
    );
    config.general.playground_selected_models = take(
        &mut root,
        "playground_selected_models",
        &["playground_enabled_models", "enabled_models"],
        config.general.playground_selected_models,
    );
    config.general.playground_model_order = take(
        &mut root,
        "playground_model_order",
        &[],
        config.general.playground_model_order,
    );
    config.general.managed_models = take_map(
        &mut root,
        "managed_models",
        &[],
        config.general.managed_models,
    );
    config.general.managed_runtimes = take_map(
        &mut root,
        "managed_runtimes",
        &[],
        config.general.managed_runtimes,
    );
    config.general.model_storage_dir = take(
        &mut root,
        "model_storage_dir",
        &[],
        config.general.model_storage_dir,
    );
    config.general.model_paths =
        take_map(&mut root, "model_paths", &[], config.general.model_paths);
    config.general.last_used_backend = take(
        &mut root,
        "last_used_backend",
        &[],
        config.general.last_used_backend,
    );
    config.general.theme_mode = take(&mut root, "theme_mode", &[], config.general.theme_mode);
    config.general.close_to_tray = take(
        &mut root,
        "close_to_tray",
        &[],
        config.general.close_to_tray,
    );
    config.recording.hotkey = take(&mut root, "hotkey", &[], config.recording.hotkey);
    config.recording.hotkey_mode =
        take(&mut root, "hotkey_mode", &[], config.recording.hotkey_mode);
    config.recording.audio_input_device_name = take(
        &mut root,
        "audio_input_device_name",
        &[],
        config.recording.audio_input_device_name,
    );
    config.recording.max_recording_seconds = take(
        &mut root,
        "max_recording_seconds",
        &[],
        config.recording.max_recording_seconds,
    );
    config.output.auto_insert_transcript = take(
        &mut root,
        "auto_insert_transcript",
        &[],
        config.output.auto_insert_transcript,
    );
    config.output.restore_clipboard_after_insert = take(
        &mut root,
        "restore_clipboard_after_insert",
        &[],
        config.output.restore_clipboard_after_insert,
    );
    config.output.paste_delay_ms = take(
        &mut root,
        "paste_delay_ms",
        &[],
        config.output.paste_delay_ms,
    );
    config.performance.acceleration_preference = take(
        &mut root,
        "acceleration_preference",
        &["whisper_compute_mode"],
        config.performance.acceleration_preference,
    );
    config.performance.whisper_gpu_device = take(
        &mut root,
        "whisper_gpu_device",
        &[],
        config.performance.whisper_gpu_device,
    );
    config.performance.whisper_cuda_backend_path = take(
        &mut root,
        "whisper_cuda_backend_path",
        &[],
        config.performance.whisper_cuda_backend_path,
    );
    config.performance.whisper_cuda_library_paths = take(
        &mut root,
        "whisper_cuda_library_paths",
        &[],
        config.performance.whisper_cuda_library_paths,
    );
    config.developer.whisper_executable_path = take(
        &mut root,
        "whisper_executable_path",
        &[],
        config.developer.whisper_executable_path,
    );
    config.developer.debug_mode = take(&mut root, "debug_mode", &[], config.developer.debug_mode);
    root.remove("schema_version");
    config.unknown = into_unknown(root);
    config
}

fn parse_general(mut section: Map<String, Value>) -> GeneralSettings {
    let defaults = GeneralSettings::default();
    GeneralSettings {
        selected_default_model: take(
            &mut section,
            "selected_default_model",
            &[],
            defaults.selected_default_model,
        ),
        playground_selected_models: take(
            &mut section,
            "playground_selected_models",
            &["playground_enabled_models", "enabled_models"],
            defaults.playground_selected_models,
        ),
        playground_model_order: take(
            &mut section,
            "playground_model_order",
            &[],
            defaults.playground_model_order,
        ),
        managed_models: take_map(&mut section, "managed_models", &[], defaults.managed_models),
        managed_runtimes: take_map(
            &mut section,
            "managed_runtimes",
            &[],
            defaults.managed_runtimes,
        ),
        model_storage_dir: take(
            &mut section,
            "model_storage_dir",
            &[],
            defaults.model_storage_dir,
        ),
        model_paths: take_map(&mut section, "model_paths", &[], defaults.model_paths),
        last_used_backend: take(
            &mut section,
            "last_used_backend",
            &[],
            defaults.last_used_backend,
        ),
        theme_mode: take(&mut section, "theme_mode", &[], defaults.theme_mode),
        close_to_tray: take(&mut section, "close_to_tray", &[], defaults.close_to_tray),
        unknown: into_unknown(section),
    }
}

fn parse_recording(mut section: Map<String, Value>) -> RecordingSettings {
    let defaults = RecordingSettings::default();
    RecordingSettings {
        hotkey: take(&mut section, "hotkey", &[], defaults.hotkey),
        hotkey_mode: take(&mut section, "hotkey_mode", &[], defaults.hotkey_mode),
        audio_input_device_name: take(
            &mut section,
            "audio_input_device_name",
            &[],
            defaults.audio_input_device_name,
        ),
        max_recording_seconds: take(
            &mut section,
            "max_recording_seconds",
            &[],
            defaults.max_recording_seconds,
        ),
        unknown: into_unknown(section),
    }
}

fn parse_streaming(section: Map<String, Value>) -> StreamingSettings {
    StreamingSettings {
        unknown: into_unknown(section),
    }
}

fn parse_output(mut section: Map<String, Value>) -> OutputSettings {
    let defaults = OutputSettings::default();
    OutputSettings {
        auto_insert_transcript: take(
            &mut section,
            "auto_insert_transcript",
            &[],
            defaults.auto_insert_transcript,
        ),
        restore_clipboard_after_insert: take(
            &mut section,
            "restore_clipboard_after_insert",
            &[],
            defaults.restore_clipboard_after_insert,
        ),
        paste_delay_ms: take(&mut section, "paste_delay_ms", &[], defaults.paste_delay_ms),
        unknown: into_unknown(section),
    }
}

fn parse_overlay(section: Map<String, Value>) -> OverlaySettings {
    OverlaySettings {
        unknown: into_unknown(section),
    }
}

fn parse_history(section: Map<String, Value>) -> HistorySettings {
    HistorySettings {
        unknown: into_unknown(section),
    }
}

fn parse_performance(mut section: Map<String, Value>) -> PerformanceSettings {
    let defaults = PerformanceSettings::default();
    PerformanceSettings {
        acceleration_preference: take(
            &mut section,
            "acceleration_preference",
            &["whisper_compute_mode"],
            defaults.acceleration_preference,
        ),
        whisper_gpu_device: take(
            &mut section,
            "whisper_gpu_device",
            &[],
            defaults.whisper_gpu_device,
        ),
        whisper_cuda_backend_path: take(
            &mut section,
            "whisper_cuda_backend_path",
            &[],
            defaults.whisper_cuda_backend_path,
        ),
        whisper_cuda_library_paths: take(
            &mut section,
            "whisper_cuda_library_paths",
            &[],
            defaults.whisper_cuda_library_paths,
        ),
        unknown: into_unknown(section),
    }
}

fn parse_developer(mut section: Map<String, Value>) -> DeveloperSettings {
    let defaults = DeveloperSettings::default();
    DeveloperSettings {
        whisper_executable_path: take(
            &mut section,
            "whisper_executable_path",
            &[],
            defaults.whisper_executable_path,
        ),
        debug_mode: take(&mut section, "debug_mode", &[], defaults.debug_mode),
        unknown: into_unknown(section),
    }
}

fn take<T>(root: &mut Map<String, Value>, key: &str, aliases: &[&str], default: T) -> T
where
    T: DeserializeOwned,
{
    let value = root
        .remove(key)
        .or_else(|| aliases.iter().find_map(|alias| root.remove(*alias)));
    for alias in aliases {
        root.remove(*alias);
    }
    value
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or(default)
}

fn take_map<T>(
    root: &mut Map<String, Value>,
    key: &str,
    aliases: &[&str],
    default: HashMap<String, T>,
) -> HashMap<String, T>
where
    T: DeserializeOwned,
{
    let value = root
        .remove(key)
        .or_else(|| aliases.iter().find_map(|alias| root.remove(*alias)));
    for alias in aliases {
        root.remove(*alias);
    }
    let Some(Value::Object(entries)) = value else {
        return default;
    };
    entries
        .into_iter()
        .filter_map(|(key, value)| serde_json::from_value(value).ok().map(|value| (key, value)))
        .collect()
}

fn take_section(root: &mut Map<String, Value>, key: &str, aliases: &[&str]) -> Map<String, Value> {
    let value = root
        .remove(key)
        .or_else(|| aliases.iter().find_map(|alias| root.remove(*alias)));
    for alias in aliases {
        root.remove(*alias);
    }
    match value {
        Some(Value::Object(section)) => section,
        _ => Map::new(),
    }
}

fn into_unknown(values: Map<String, Value>) -> UnknownFields {
    values.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::HotkeyMode;
    use serde_json::json;

    #[test]
    fn legacy_flat_aliases_and_missing_fields_migrate() {
        let config = parse_settings_value(json!({
            "selected_default_model": "whisper_cpp_base_en",
            "enabled_models": ["whisper_cpp_base_en"],
            "hotkey": "Alt+Space",
            "whisper_compute_mode": "prefer_gpu",
            "future_legacy_key": {"kept": true}
        }));

        assert_eq!(config.schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(config.general.selected_default_model, "whisper_cpp_base_en");
        assert_eq!(
            config.general.playground_selected_models,
            ["whisper_cpp_base_en"]
        );
        assert_eq!(config.recording.hotkey, "Alt+Space");
        assert_eq!(
            config.performance.acceleration_preference,
            AccelerationPreference::Gpu
        );
        assert!(config.general.playground_model_order.is_empty());
        assert!(config.output.auto_insert_transcript);
        assert_eq!(config.unknown["future_legacy_key"], json!({"kept": true}));
    }

    #[test]
    fn missing_new_sections_and_fields_use_current_defaults() {
        let config = parse_settings_value(json!({
            "schema_version": CURRENT_SCHEMA_VERSION,
            "general": {"selected_default_model": "whisper_cpp_base_en"}
        }));

        assert_eq!(config.general.selected_default_model, "whisper_cpp_base_en");
        assert_eq!(config.recording.max_recording_seconds, 30);
        assert_eq!(config.output.paste_delay_ms, 75);
        assert_eq!(
            config.performance.acceleration_preference,
            AccelerationPreference::Auto
        );
    }

    #[test]
    fn invalid_enum_scalar_and_map_entry_only_default_the_bad_values() {
        let config = parse_settings_value(json!({
            "schema_version": CURRENT_SCHEMA_VERSION,
            "general": {
                "selected_default_model": "whisper_cpp_base_en",
                "managed_models": {
                    "valid": {"path": "valid-model.bin"},
                    "invalid": {"path": 42}
                },
                "model_paths": {
                    "valid": "valid-model.bin",
                    "invalid": 42
                }
            },
            "recording": {
                "hotkey": "Ctrl+Alt+R",
                "hotkey_mode": "not-a-mode",
                "max_recording_seconds": "not-a-number"
            },
            "performance": {
                "acceleration_preference": "not-an-accelerator",
                "whisper_gpu_device": 4
            }
        }));

        assert_eq!(config.general.selected_default_model, "whisper_cpp_base_en");
        assert!(config.general.managed_models.contains_key("valid"));
        assert!(!config.general.managed_models.contains_key("invalid"));
        assert!(config.general.model_paths.contains_key("valid"));
        assert!(!config.general.model_paths.contains_key("invalid"));
        assert_eq!(config.recording.hotkey, "Ctrl+Alt+R");
        assert_eq!(config.recording.hotkey_mode, HotkeyMode::Toggle);
        assert_eq!(config.recording.max_recording_seconds, 30);
        assert_eq!(
            config.performance.acceleration_preference,
            AccelerationPreference::Auto
        );
        assert_eq!(config.performance.whisper_gpu_device, 4);
    }

    #[test]
    fn future_schema_and_unknown_root_and_section_fields_round_trip() {
        let value = json!({
            "schema_version": CURRENT_SCHEMA_VERSION + 7,
            "general": {
                "selected_default_model": "whisper_cpp_base_en",
                "future_general": [1, 2, 3]
            },
            "streaming": {"future_streaming": {"enabled": true}},
            "future_root": {"format": "new"}
        });

        let config = parse_settings_value(value);
        let serialized = serde_json::to_value(config).unwrap();

        assert_eq!(serialized["schema_version"], CURRENT_SCHEMA_VERSION + 7);
        assert_eq!(serialized["general"]["future_general"], json!([1, 2, 3]));
        assert_eq!(
            serialized["streaming"]["future_streaming"],
            json!({"enabled": true})
        );
        assert_eq!(serialized["future_root"], json!({"format": "new"}));
    }

    #[test]
    fn sectioned_version_zero_migrates_to_current_version() {
        let config = parse_settings_value(json!({
            "schema_version": 0,
            "general": {}
        }));

        assert_eq!(config.schema_version, CURRENT_SCHEMA_VERSION);
    }
}
