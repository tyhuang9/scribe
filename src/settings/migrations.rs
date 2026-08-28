use std::collections::HashMap;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value};

use super::schema::{
    AppConfig, CURRENT_SCHEMA_VERSION, DeveloperSettings, GeneralSettings, HistorySettings,
    OutputSettings, OverlaySettings, PerformanceSettings, RecordingSettings, SpeechDetectionMode,
    StreamingSettings, UnknownFields,
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
    parse_settings_value_with_diagnostics(value).0
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ParseDiagnostics {
    pub invalid_values_salvaged: bool,
}

pub(crate) fn parse_settings_value_with_diagnostics(value: Value) -> (AppConfig, ParseDiagnostics) {
    let mut diagnostics = ParseDiagnostics::default();
    let Value::Object(root) = value else {
        diagnostics.invalid_values_salvaged = true;
        return (AppConfig::default(), diagnostics);
    };

    let config = if root.keys().any(|key| {
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
        parse_sectioned(root, &mut diagnostics)
    } else {
        migrate_legacy_flat(root, &mut diagnostics)
    };
    (config, diagnostics)
}

fn parse_sectioned(mut root: Map<String, Value>, diagnostics: &mut ParseDiagnostics) -> AppConfig {
    let stored_schema_version = take(
        &mut root,
        "schema_version",
        &[],
        CURRENT_SCHEMA_VERSION,
        diagnostics,
    );
    let schema_version = if stored_schema_version <= CURRENT_SCHEMA_VERSION {
        CURRENT_SCHEMA_VERSION
    } else {
        stored_schema_version
    };
    let general = parse_general(
        take_section(&mut root, "general", &[], diagnostics),
        diagnostics,
    );
    let recording = parse_recording(
        take_section(&mut root, "recording", &[], diagnostics),
        stored_schema_version,
        diagnostics,
    );
    let streaming = parse_streaming(
        take_section(&mut root, "streaming", &["transcription"], diagnostics),
        diagnostics,
    );
    let output = parse_output(
        take_section(&mut root, "output", &[], diagnostics),
        diagnostics,
    );
    let overlay = parse_overlay(
        take_section(&mut root, "overlay", &[], diagnostics),
        diagnostics,
    );
    let history = parse_history(
        take_section(&mut root, "history", &[], diagnostics),
        diagnostics,
    );
    let performance = parse_performance(
        take_section(&mut root, "performance", &[], diagnostics),
        diagnostics,
    );
    let developer = parse_developer(
        take_section(&mut root, "developer", &[], diagnostics),
        diagnostics,
    );

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

fn migrate_legacy_flat(
    mut root: Map<String, Value>,
    diagnostics: &mut ParseDiagnostics,
) -> AppConfig {
    let mut config = AppConfig::default();
    config.general.playground_model_order.clear();
    config.performance.acceleration_preference = AccelerationPreference::Cpu;
    config.output.auto_insert_transcript = true;

    config.general.selected_default_model = take(
        &mut root,
        "selected_default_model",
        &[],
        config.general.selected_default_model,
        diagnostics,
    );
    config.general.excluded_bundled_model_ids = take(
        &mut root,
        "excluded_bundled_model_ids",
        &[],
        config.general.excluded_bundled_model_ids,
        diagnostics,
    );
    config.general.playground_selected_models = take(
        &mut root,
        "playground_selected_models",
        &["playground_enabled_models", "enabled_models"],
        config.general.playground_selected_models,
        diagnostics,
    );
    config.general.playground_model_order = take(
        &mut root,
        "playground_model_order",
        &[],
        config.general.playground_model_order,
        diagnostics,
    );
    config.general.managed_models = take_map(
        &mut root,
        "managed_models",
        &[],
        config.general.managed_models,
        diagnostics,
        Some(managed_install_value_is_valid),
    );
    config.general.managed_remote_models = take_map(
        &mut root,
        "managed_remote_models",
        &[],
        config.general.managed_remote_models,
        diagnostics,
        None,
    );
    config.general.imported_gguf_models = take_map(
        &mut root,
        "imported_gguf_models",
        &[],
        config.general.imported_gguf_models,
        diagnostics,
        None,
    );
    config.general.model_storage_dir = take(
        &mut root,
        "model_storage_dir",
        &[],
        config.general.model_storage_dir,
        diagnostics,
    );
    config.general.model_paths = take_map(
        &mut root,
        "model_paths",
        &[],
        config.general.model_paths,
        diagnostics,
        None,
    );
    config.general.theme_mode = take(
        &mut root,
        "theme_mode",
        &[],
        config.general.theme_mode,
        diagnostics,
    );
    config.general.close_to_tray = take(
        &mut root,
        "close_to_tray",
        &[],
        config.general.close_to_tray,
        diagnostics,
    );
    config.recording.hotkey = take(
        &mut root,
        "hotkey",
        &[],
        config.recording.hotkey,
        diagnostics,
    );
    config.recording.hotkey_mode = take(
        &mut root,
        "hotkey_mode",
        &[],
        config.recording.hotkey_mode,
        diagnostics,
    );
    config.recording.audio_input_device_name = take(
        &mut root,
        "audio_input_device_name",
        &[],
        config.recording.audio_input_device_name,
        diagnostics,
    );
    config.recording.max_recording_seconds = take(
        &mut root,
        "max_recording_seconds",
        &[],
        config.recording.max_recording_seconds,
        diagnostics,
    );
    config.recording.vad_enabled = take(
        &mut root,
        "vad_enabled",
        &[],
        config.recording.vad_enabled,
        diagnostics,
    );
    config.recording.speech_detection_mode = take(
        &mut root,
        "speech_detection_mode",
        &[],
        SpeechDetectionMode::Ai,
        diagnostics,
    );
    config.recording.input_threshold_dbfs = take_input_threshold_dbfs(
        &mut root,
        config.recording.input_threshold_dbfs,
        true,
        true,
        diagnostics,
    );
    config.recording.speech_confirmation_ms = take(
        &mut root,
        "speech_confirmation_ms",
        &[],
        config.recording.speech_confirmation_ms,
        diagnostics,
    );
    config.recording.internal_pause_ms = take(
        &mut root,
        "internal_pause_ms",
        &[],
        config.recording.internal_pause_ms,
        diagnostics,
    );
    config.recording.endpoint_silence_ms = take(
        &mut root,
        "endpoint_silence_ms",
        &[],
        config.recording.endpoint_silence_ms,
        diagnostics,
    );
    config.recording.pre_roll_ms = take(
        &mut root,
        "pre_roll_ms",
        &[],
        config.recording.pre_roll_ms,
        diagnostics,
    );
    config.recording.post_roll_ms = take(
        &mut root,
        "post_roll_ms",
        &[],
        config.recording.post_roll_ms,
        diagnostics,
    );
    config.output.auto_insert_transcript = take(
        &mut root,
        "auto_insert_transcript",
        &[],
        config.output.auto_insert_transcript,
        diagnostics,
    );
    config.output.restore_clipboard_after_insert = take(
        &mut root,
        "restore_clipboard_after_insert",
        &[],
        config.output.restore_clipboard_after_insert,
        diagnostics,
    );
    config.output.paste_delay_ms = take(
        &mut root,
        "paste_delay_ms",
        &[],
        config.output.paste_delay_ms,
        diagnostics,
    );
    config.performance.acceleration_preference = take(
        &mut root,
        "acceleration_preference",
        &["whisper_compute_mode"],
        config.performance.acceleration_preference,
        diagnostics,
    );
    config.developer.debug_mode = take(
        &mut root,
        "debug_mode",
        &[],
        config.developer.debug_mode,
        diagnostics,
    );
    move_unknown_fields(
        &mut root,
        &mut config.general.unknown,
        &["managed_runtimes", "last_used_backend"],
    );
    move_unknown_fields(
        &mut root,
        &mut config.performance.unknown,
        &[
            "whisper_gpu_device",
            "whisper_cuda_backend_path",
            "whisper_cuda_library_paths",
        ],
    );
    move_unknown_fields(
        &mut root,
        &mut config.developer.unknown,
        &["whisper_executable_path"],
    );
    root.remove("schema_version");
    config.unknown = into_unknown(root);
    config
}

fn parse_general(
    mut section: Map<String, Value>,
    diagnostics: &mut ParseDiagnostics,
) -> GeneralSettings {
    let defaults = GeneralSettings::default();
    GeneralSettings {
        selected_default_model: take(
            &mut section,
            "selected_default_model",
            &[],
            defaults.selected_default_model,
            diagnostics,
        ),
        excluded_bundled_model_ids: take(
            &mut section,
            "excluded_bundled_model_ids",
            &[],
            defaults.excluded_bundled_model_ids,
            diagnostics,
        ),
        playground_selected_models: take(
            &mut section,
            "playground_selected_models",
            &["playground_enabled_models", "enabled_models"],
            defaults.playground_selected_models,
            diagnostics,
        ),
        playground_model_order: take(
            &mut section,
            "playground_model_order",
            &[],
            defaults.playground_model_order,
            diagnostics,
        ),
        managed_models: take_map(
            &mut section,
            "managed_models",
            &[],
            defaults.managed_models,
            diagnostics,
            Some(managed_install_value_is_valid),
        ),
        managed_remote_models: take_map(
            &mut section,
            "managed_remote_models",
            &[],
            defaults.managed_remote_models,
            diagnostics,
            None,
        ),
        imported_gguf_models: take_map(
            &mut section,
            "imported_gguf_models",
            &[],
            defaults.imported_gguf_models,
            diagnostics,
            None,
        ),
        model_storage_dir: take(
            &mut section,
            "model_storage_dir",
            &[],
            defaults.model_storage_dir,
            diagnostics,
        ),
        model_paths: take_map(
            &mut section,
            "model_paths",
            &[],
            defaults.model_paths,
            diagnostics,
            None,
        ),
        theme_mode: take(
            &mut section,
            "theme_mode",
            &[],
            defaults.theme_mode,
            diagnostics,
        ),
        close_to_tray: take(
            &mut section,
            "close_to_tray",
            &[],
            defaults.close_to_tray,
            diagnostics,
        ),
        unknown: into_unknown(section),
    }
}

fn take_input_threshold_dbfs(
    section: &mut Map<String, Value>,
    default: f32,
    discard_legacy_fields: bool,
    convert_legacy_rms: bool,
    diagnostics: &mut ParseDiagnostics,
) -> f32 {
    if discard_legacy_fields && !convert_legacy_rms {
        // Version 2 predates the literal threshold and selected mode. Do not
        // reinterpret any stray values as a user choice during this upgrade.
        section.remove("input_threshold_dbfs");
        section.remove("speech_probability_threshold");
        section.remove("manual_activation_rms");
        return default;
    }
    let has_current_value = section.contains_key("input_threshold_dbfs");
    let threshold = take(section, "input_threshold_dbfs", &[], default, diagnostics);
    if !discard_legacy_fields {
        return threshold;
    }
    // The old probability setting no longer controls a capture mode. Remove it
    // rather than serializing a dormant, misleading option back to disk.
    section.remove("speech_probability_threshold");
    if has_current_value || !convert_legacy_rms {
        section.remove("manual_activation_rms");
        threshold
    } else {
        match section.remove("manual_activation_rms") {
            Some(Value::Number(value)) => value
                .as_f64()
                .filter(|rms| rms.is_finite() && *rms > 0.0)
                .map(|rms| (20.0 * rms.log10()) as f32)
                .unwrap_or(default),
            Some(_) => {
                diagnostics.invalid_values_salvaged = true;
                default
            }
            None => default,
        }
    }
}

fn parse_recording(
    mut section: Map<String, Value>,
    stored_schema_version: u32,
    diagnostics: &mut ParseDiagnostics,
) -> RecordingSettings {
    let defaults = RecordingSettings::default();
    RecordingSettings {
        hotkey: take(&mut section, "hotkey", &[], defaults.hotkey, diagnostics),
        hotkey_mode: take(
            &mut section,
            "hotkey_mode",
            &[],
            defaults.hotkey_mode,
            diagnostics,
        ),
        audio_input_device_name: take(
            &mut section,
            "audio_input_device_name",
            &[],
            defaults.audio_input_device_name,
            diagnostics,
        ),
        max_recording_seconds: take(
            &mut section,
            "max_recording_seconds",
            &[],
            defaults.max_recording_seconds,
            diagnostics,
        ),
        vad_enabled: take(
            &mut section,
            "vad_enabled",
            &[],
            defaults.vad_enabled,
            diagnostics,
        ),
        speech_detection_mode: if stored_schema_version == 2 {
            section.remove("speech_detection_mode");
            SpeechDetectionMode::Ai
        } else {
            take(
                &mut section,
                "speech_detection_mode",
                &[],
                SpeechDetectionMode::Ai,
                diagnostics,
            )
        },
        input_threshold_dbfs: take_input_threshold_dbfs(
            &mut section,
            defaults.input_threshold_dbfs,
            stored_schema_version < CURRENT_SCHEMA_VERSION,
            stored_schema_version < 2,
            diagnostics,
        ),
        speech_confirmation_ms: take(
            &mut section,
            "speech_confirmation_ms",
            &[],
            defaults.speech_confirmation_ms,
            diagnostics,
        ),
        internal_pause_ms: take(
            &mut section,
            "internal_pause_ms",
            &[],
            defaults.internal_pause_ms,
            diagnostics,
        ),
        endpoint_silence_ms: take(
            &mut section,
            "endpoint_silence_ms",
            &[],
            defaults.endpoint_silence_ms,
            diagnostics,
        ),
        pre_roll_ms: take(
            &mut section,
            "pre_roll_ms",
            &[],
            defaults.pre_roll_ms,
            diagnostics,
        ),
        post_roll_ms: take(
            &mut section,
            "post_roll_ms",
            &[],
            defaults.post_roll_ms,
            diagnostics,
        ),
        unknown: into_unknown(section),
    }
}

fn parse_streaming(
    mut section: Map<String, Value>,
    diagnostics: &mut ParseDiagnostics,
) -> StreamingSettings {
    let defaults = StreamingSettings::default();
    StreamingSettings {
        mode: take(&mut section, "mode", &[], defaults.mode, diagnostics),
        unknown: into_unknown(section),
    }
}

fn parse_output(
    mut section: Map<String, Value>,
    diagnostics: &mut ParseDiagnostics,
) -> OutputSettings {
    let defaults = OutputSettings::default();
    OutputSettings {
        auto_insert_transcript: take(
            &mut section,
            "auto_insert_transcript",
            &[],
            defaults.auto_insert_transcript,
            diagnostics,
        ),
        restore_clipboard_after_insert: take(
            &mut section,
            "restore_clipboard_after_insert",
            &[],
            defaults.restore_clipboard_after_insert,
            diagnostics,
        ),
        paste_delay_ms: take(
            &mut section,
            "paste_delay_ms",
            &[],
            defaults.paste_delay_ms,
            diagnostics,
        ),
        unknown: into_unknown(section),
    }
}

fn parse_overlay(
    mut section: Map<String, Value>,
    diagnostics: &mut ParseDiagnostics,
) -> OverlaySettings {
    let defaults = OverlaySettings::default();
    OverlaySettings {
        mode: take(&mut section, "mode", &[], defaults.mode, diagnostics),
        position: take(
            &mut section,
            "position",
            &[],
            defaults.position,
            diagnostics,
        ),
        unknown: into_unknown(section),
    }
}

fn parse_history(
    mut section: Map<String, Value>,
    diagnostics: &mut ParseDiagnostics,
) -> HistorySettings {
    let defaults = HistorySettings::default();
    HistorySettings {
        mode: take(&mut section, "mode", &[], defaults.mode, diagnostics),
        max_unpinned_entries: take(
            &mut section,
            "max_unpinned_entries",
            &[],
            defaults.max_unpinned_entries,
            diagnostics,
        ),
        transcript_retention_days: take(
            &mut section,
            "transcript_retention_days",
            &[],
            defaults.transcript_retention_days,
            diagnostics,
        ),
        audio_retention_days: take(
            &mut section,
            "audio_retention_days",
            &[],
            defaults.audio_retention_days,
            diagnostics,
        ),
        store_application_identity: take(
            &mut section,
            "store_application_identity",
            &[],
            defaults.store_application_identity,
            diagnostics,
        ),
        unknown: into_unknown(section),
    }
}

fn parse_performance(
    mut section: Map<String, Value>,
    diagnostics: &mut ParseDiagnostics,
) -> PerformanceSettings {
    let defaults = PerformanceSettings::default();
    PerformanceSettings {
        acceleration_preference: take(
            &mut section,
            "acceleration_preference",
            &["whisper_compute_mode"],
            defaults.acceleration_preference,
            diagnostics,
        ),
        unknown: into_unknown(section),
    }
}

fn parse_developer(
    mut section: Map<String, Value>,
    diagnostics: &mut ParseDiagnostics,
) -> DeveloperSettings {
    let defaults = DeveloperSettings::default();
    DeveloperSettings {
        debug_mode: take(
            &mut section,
            "debug_mode",
            &[],
            defaults.debug_mode,
            diagnostics,
        ),
        unknown: into_unknown(section),
    }
}

fn move_unknown_fields(root: &mut Map<String, Value>, unknown: &mut UnknownFields, keys: &[&str]) {
    for key in keys {
        if let Some(value) = root.remove(*key) {
            unknown.insert((*key).to_owned(), value);
        }
    }
}

fn take<T>(
    root: &mut Map<String, Value>,
    key: &str,
    aliases: &[&str],
    default: T,
    diagnostics: &mut ParseDiagnostics,
) -> T
where
    T: DeserializeOwned,
{
    let value = root
        .remove(key)
        .or_else(|| aliases.iter().find_map(|alias| root.remove(*alias)));
    for alias in aliases {
        root.remove(*alias);
    }
    match value {
        Some(value) => serde_json::from_value(value).unwrap_or_else(|_| {
            diagnostics.invalid_values_salvaged = true;
            default
        }),
        None => default,
    }
}

fn take_map<T>(
    root: &mut Map<String, Value>,
    key: &str,
    aliases: &[&str],
    default: HashMap<String, T>,
    diagnostics: &mut ParseDiagnostics,
    validate: Option<fn(&Value) -> bool>,
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
    let entries = match value {
        Some(Value::Object(entries)) => entries,
        Some(_) => {
            diagnostics.invalid_values_salvaged = true;
            return default;
        }
        None => return default,
    };
    entries
        .into_iter()
        .filter_map(|(key, value)| {
            if validate.is_some_and(|validate| !validate(&value)) {
                diagnostics.invalid_values_salvaged = true;
            }
            match serde_json::from_value(value) {
                Ok(value) => Some((key, value)),
                Err(_) => {
                    diagnostics.invalid_values_salvaged = true;
                    None
                }
            }
        })
        .collect()
}

fn managed_install_value_is_valid(value: &Value) -> bool {
    let Value::Object(fields) = value else {
        return false;
    };
    if !fields.get("path").is_some_and(Value::is_string) {
        return false;
    }
    ["source", "version", "sha256", "platform"]
        .into_iter()
        .all(|field| {
            fields
                .get(field)
                .is_none_or(|value| value.is_null() || value.is_string())
        })
        && fields
            .get("installed_at_unix_seconds")
            .is_none_or(|value| value.is_null() || value.is_u64())
}

fn take_section(
    root: &mut Map<String, Value>,
    key: &str,
    aliases: &[&str],
    diagnostics: &mut ParseDiagnostics,
) -> Map<String, Value> {
    let value = root
        .remove(key)
        .or_else(|| aliases.iter().find_map(|alias| root.remove(*alias)));
    for alias in aliases {
        root.remove(*alias);
    }
    match value {
        Some(Value::Object(section)) => section,
        Some(_) => {
            diagnostics.invalid_values_salvaged = true;
            Map::new()
        }
        None => Map::new(),
    }
}

fn into_unknown(values: Map<String, Value>) -> UnknownFields {
    values.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{HistoryMode, HotkeyMode, OverlayMode, OverlayPosition, StreamingMode};
    use serde_json::json;

    #[test]
    fn legacy_flat_aliases_and_missing_fields_migrate() {
        let config = parse_settings_value(json!({
            "selected_default_model": "whisper_cpp_base_en",
            "enabled_models": ["whisper_cpp_base_en"],
            "hotkey": "Alt+Space",
            "vad_enabled": false,
            "manual_activation_rms": 0.031,
            "speech_confirmation_ms": 180,
            "internal_pause_ms": 520,
            "endpoint_silence_ms": 980,
            "pre_roll_ms": 280,
            "post_roll_ms": 220,
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
        assert!(!config.recording.vad_enabled);
        assert_eq!(
            config.recording.speech_detection_mode,
            SpeechDetectionMode::Ai
        );
        assert!((config.recording.input_threshold_dbfs - 20.0 * 0.031_f32.log10()).abs() < 1e-5);
        assert_eq!(config.recording.speech_confirmation_ms, 180);
        assert_eq!(config.recording.internal_pause_ms, 520);
        assert_eq!(config.recording.endpoint_silence_ms, 980);
        assert_eq!(config.recording.pre_roll_ms, 280);
        assert_eq!(config.recording.post_roll_ms, 220);
        assert_eq!(
            config.performance.acceleration_preference,
            AccelerationPreference::Gpu
        );
        assert!(config.general.playground_model_order.is_empty());
        assert!(config.output.auto_insert_transcript);
        assert_eq!(config.unknown["future_legacy_key"], json!({"kept": true}));
    }

    #[test]
    fn version_three_retired_runtime_fields_become_exact_inert_unknown_values() {
        let retired_runtimes = json!({
            "faster_whisper": {
                "path": "legacy/runtime.py",
                "source": "copied-profile",
                "opaque": [1, {"sentinel": true}]
            }
        });
        let cuda_backend = json!("legacy/cuda/whisper.dll");
        let cuda_libraries = json!(["legacy/cuda", {"future": "value"}]);
        let executable = json!({"path": "legacy/whisper-cli", "opaque": 7});
        let config = parse_settings_value(json!({
            "schema_version": 3,
            "general": {
                "managed_runtimes": retired_runtimes.clone(),
                "last_used_backend": {"id": "faster_whisper", "opaque": true}
            },
            "performance": {
                "whisper_gpu_device": {"ordinal": 4},
                "whisper_cuda_backend_path": cuda_backend.clone(),
                "whisper_cuda_library_paths": cuda_libraries.clone()
            },
            "developer": {
                "whisper_executable_path": executable.clone()
            }
        }));

        assert_eq!(config.schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(config.general.unknown["managed_runtimes"], retired_runtimes);
        assert_eq!(
            config.general.unknown["last_used_backend"],
            json!({"id": "faster_whisper", "opaque": true})
        );
        assert_eq!(
            config.performance.unknown["whisper_gpu_device"],
            json!({"ordinal": 4})
        );
        assert_eq!(
            config.performance.unknown["whisper_cuda_backend_path"],
            cuda_backend
        );
        assert_eq!(
            config.performance.unknown["whisper_cuda_library_paths"],
            cuda_libraries
        );
        assert_eq!(
            config.developer.unknown["whisper_executable_path"],
            executable
        );

        let serialized = serde_json::to_value(config).unwrap();
        assert_eq!(serialized["schema_version"], json!(4));
        assert_eq!(serialized["general"]["managed_runtimes"], retired_runtimes);
        assert_eq!(
            serialized["performance"]["whisper_cuda_library_paths"],
            cuda_libraries
        );
        assert_eq!(
            serialized["developer"]["whisper_executable_path"],
            executable
        );
    }

    #[test]
    fn missing_new_sections_and_fields_use_current_defaults() {
        let config = parse_settings_value(json!({
            "schema_version": CURRENT_SCHEMA_VERSION,
            "general": {"selected_default_model": "whisper_cpp_base_en"}
        }));

        assert_eq!(config.general.selected_default_model, "whisper_cpp_base_en");
        assert!(config.general.excluded_bundled_model_ids.is_empty());
        assert_eq!(config.recording.hotkey, "Ctrl+Space");
        assert_eq!(config.recording.hotkey_mode, HotkeyMode::HoldToTalk);
        assert_eq!(config.recording.max_recording_seconds, 30);
        assert!(config.recording.vad_enabled);
        assert_eq!(config.recording.speech_confirmation_ms, 150);
        assert_eq!(config.recording.internal_pause_ms, 450);
        assert_eq!(config.recording.endpoint_silence_ms, 900);
        assert_eq!(config.recording.pre_roll_ms, 250);
        assert_eq!(config.recording.post_roll_ms, 200);
        assert_eq!(
            config.recording.speech_detection_mode,
            SpeechDetectionMode::Ai
        );
        assert_eq!(
            config.recording.input_threshold_dbfs,
            RecordingSettings::default().input_threshold_dbfs
        );
        assert_eq!(config.output.paste_delay_ms, 75);
        assert_eq!(config.overlay.mode, OverlayMode::Minimal);
        assert_eq!(config.overlay.position, OverlayPosition::Bottom);
        assert_eq!(config.history.mode, HistoryMode::TranscriptOnly);
        assert_eq!(config.history.max_unpinned_entries, 20);
        assert_eq!(config.history.transcript_retention_days, None);
        assert_eq!(config.history.audio_retention_days, None);
        assert!(!config.history.store_application_identity);
        assert_eq!(
            config.performance.acceleration_preference,
            AccelerationPreference::Auto
        );
    }

    #[test]
    fn bundled_model_exclusions_round_trip_in_the_current_schema() {
        let config = parse_settings_value(json!({
            "schema_version": 2,
            "general": {
                "selected_default_model": "",
                "excluded_bundled_model_ids": ["whisper_cpp_base_en"]
            }
        }));

        assert_eq!(config.schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(
            config.general.excluded_bundled_model_ids,
            ["whisper_cpp_base_en"]
        );
        let serialized = serde_json::to_value(config).unwrap();
        assert_eq!(
            serialized["general"]["excluded_bundled_model_ids"],
            json!(["whisper_cpp_base_en"])
        );
    }

    #[test]
    fn invalid_legacy_rms_defaults_to_the_safe_manual_threshold() {
        let (config, diagnostics) = parse_settings_value_with_diagnostics(json!({
            "schema_version": 1,
            "recording": {
                "sensitivity_mode": "not-a-mode",
                "manual_activation_rms": "not-a-number",
                "future_microphone_option": {"kept": true}
            }
        }));

        assert_eq!(
            config.recording.speech_detection_mode,
            SpeechDetectionMode::Ai
        );
        assert_eq!(config.recording.input_threshold_dbfs, -42.0);
        assert_eq!(
            config.recording.unknown["future_microphone_option"],
            json!({"kept": true})
        );
        assert_eq!(
            config.recording.unknown["sensitivity_mode"],
            json!("not-a-mode")
        );
        assert!(diagnostics.invalid_values_salvaged);
        assert_eq!(
            serde_json::to_value(config).unwrap()["recording"]["future_microphone_option"],
            json!({"kept": true})
        );
    }

    #[test]
    fn explicit_hotkey_and_mode_are_preserved_without_a_schema_migration() {
        let config = parse_settings_value(json!({
            "schema_version": 2,
            "recording": {
                "hotkey": "Alt+Shift+R",
                "hotkey_mode": "toggle"
            }
        }));

        assert_eq!(config.schema_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(config.recording.hotkey, "Alt+Shift+R");
        assert_eq!(config.recording.hotkey_mode, HotkeyMode::Toggle);
    }

    #[test]
    fn detection_mode_and_manual_dbfs_threshold_round_trip() {
        let config = parse_settings_value(json!({
            "schema_version": CURRENT_SCHEMA_VERSION,
            "recording": {
                "speech_detection_mode": "manual_threshold",
                "input_threshold_dbfs": -37.0,
                "future_recording_option": {"preserved": true}
            }
        }));

        assert_eq!(
            config.recording.speech_detection_mode,
            SpeechDetectionMode::ManualThreshold
        );
        assert_eq!(config.recording.input_threshold_dbfs, -37.0);
        let serialized = serde_json::to_value(config).unwrap();
        assert_eq!(
            serialized["recording"]["speech_detection_mode"],
            json!("manual_threshold")
        );
        assert_eq!(
            serialized["recording"]["input_threshold_dbfs"],
            json!(-37.0)
        );
        assert_eq!(
            serialized["recording"]["future_recording_option"],
            json!({"preserved": true})
        );
    }

    #[test]
    fn version_two_probability_settings_migrate_to_ai_and_do_not_round_trip() {
        let config = parse_settings_value(json!({
            "schema_version": 2,
            "recording": {
                "speech_probability_threshold": 0.35,
                "manual_activation_rms": 0.99
            }
        }));

        assert_eq!(
            config.recording.speech_detection_mode,
            SpeechDetectionMode::Ai
        );
        assert_eq!(
            config.recording.input_threshold_dbfs,
            RecordingSettings::default().input_threshold_dbfs
        );
        let serialized = serde_json::to_value(config).unwrap();
        assert_eq!(
            serialized["recording"]["speech_detection_mode"],
            json!("ai")
        );
        assert!(
            serialized["recording"]
                .get("speech_probability_threshold")
                .is_none()
        );
        assert!(
            serialized["recording"]
                .get("manual_activation_rms")
                .is_none()
        );
    }

    #[test]
    fn version_one_manual_rms_converts_to_the_dormant_manual_threshold() {
        let config = parse_settings_value(json!({
            "schema_version": 1,
            "recording": {"manual_activation_rms": 0.1}
        }));

        assert_eq!(
            config.recording.speech_detection_mode,
            SpeechDetectionMode::Ai
        );
        assert!((config.recording.input_threshold_dbfs + 20.0).abs() < 1e-5);
        let serialized = serde_json::to_value(config).unwrap();
        assert!(
            serialized["recording"]
                .get("manual_activation_rms")
                .is_none()
        );
    }

    #[test]
    fn history_settings_salvage_invalid_fields_and_preserve_future_values() {
        let (config, diagnostics) = parse_settings_value_with_diagnostics(json!({
            "schema_version": CURRENT_SCHEMA_VERSION,
            "history": {
                "mode": "not-a-mode",
                "max_unpinned_entries": "many",
                "transcript_retention_days": 30,
                "audio_retention_days": null,
                "store_application_identity": true,
                "future_history": {"encryption": "kept"}
            }
        }));

        assert_eq!(config.history.mode, HistoryMode::TranscriptOnly);
        assert_eq!(config.history.max_unpinned_entries, 20);
        assert_eq!(config.history.transcript_retention_days, Some(30));
        assert_eq!(config.history.audio_retention_days, None);
        assert!(config.history.store_application_identity);
        assert_eq!(
            config.history.unknown["future_history"],
            json!({"encryption": "kept"})
        );
        assert!(diagnostics.invalid_values_salvaged);

        let serialized = serde_json::to_value(config).unwrap();
        assert_eq!(
            serialized["history"]["future_history"],
            json!({"encryption": "kept"})
        );
    }

    #[test]
    fn every_history_mode_round_trips() {
        for (stored, expected) in [
            ("off", HistoryMode::Off),
            ("transcript_only", HistoryMode::TranscriptOnly),
            ("transcript_and_audio", HistoryMode::TranscriptAndAudio),
        ] {
            let (config, diagnostics) = parse_settings_value_with_diagnostics(json!({
                "schema_version": CURRENT_SCHEMA_VERSION,
                "history": {"mode": stored}
            }));
            assert_eq!(config.history.mode, expected);
            assert!(!diagnostics.invalid_values_salvaged);
            assert_eq!(
                serde_json::to_value(config).unwrap()["history"]["mode"],
                stored
            );
        }
    }

    #[test]
    fn overlay_settings_salvage_invalid_fields_and_preserve_future_values() {
        let (config, diagnostics) = parse_settings_value_with_diagnostics(json!({
            "schema_version": CURRENT_SCHEMA_VERSION,
            "overlay": {
                "mode": "not-a-mode",
                "position": "top",
                "future_overlay": {"opacity": 0.75}
            }
        }));

        assert_eq!(config.overlay.mode, OverlayMode::Minimal);
        assert_eq!(config.overlay.position, OverlayPosition::Top);
        assert_eq!(
            config.overlay.unknown["future_overlay"],
            json!({"opacity": 0.75})
        );
        assert!(diagnostics.invalid_values_salvaged);

        let serialized = serde_json::to_value(config).unwrap();
        assert_eq!(
            serialized["overlay"]["future_overlay"],
            json!({"opacity": 0.75})
        );
    }

    #[test]
    fn overlay_modes_and_positions_round_trip() {
        for (mode, position) in [
            (OverlayMode::Live, OverlayPosition::Top),
            (OverlayMode::Minimal, OverlayPosition::Bottom),
            (OverlayMode::Off, OverlayPosition::Bottom),
        ] {
            let config = parse_settings_value(json!({
                "schema_version": CURRENT_SCHEMA_VERSION,
                "overlay": {
                    "mode": mode,
                    "position": position
                }
            }));
            assert_eq!(config.overlay.mode, mode);
            assert_eq!(config.overlay.position, position);
        }
    }

    #[test]
    fn invalid_enum_scalar_and_map_entry_only_default_the_bad_values() {
        let (config, diagnostics) = parse_settings_value_with_diagnostics(json!({
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
                "max_recording_seconds": "not-a-number",
                "vad_enabled": "not-a-boolean",
                "speech_confirmation_ms": "not-a-number",
                "internal_pause_ms": 600,
                "endpoint_silence_ms": 1200,
                "pre_roll_ms": 300,
                "post_roll_ms": 250,
                "future_vad": {"strategy": "kept"}
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
        assert_eq!(config.recording.hotkey_mode, HotkeyMode::HoldToTalk);
        assert_eq!(config.recording.max_recording_seconds, 30);
        assert!(config.recording.vad_enabled);
        assert_eq!(config.recording.speech_confirmation_ms, 150);
        assert_eq!(config.recording.internal_pause_ms, 600);
        assert_eq!(config.recording.endpoint_silence_ms, 1200);
        assert_eq!(config.recording.pre_roll_ms, 300);
        assert_eq!(config.recording.post_roll_ms, 250);
        assert_eq!(
            config.recording.unknown["future_vad"],
            json!({"strategy": "kept"})
        );
        assert_eq!(
            config.performance.acceleration_preference,
            AccelerationPreference::Auto
        );
        assert_eq!(config.performance.unknown["whisper_gpu_device"], json!(4));
        assert!(diagnostics.invalid_values_salvaged);
    }

    #[test]
    fn vad_fields_and_unknown_recording_values_round_trip() {
        let config = parse_settings_value(json!({
            "schema_version": CURRENT_SCHEMA_VERSION,
            "recording": {
                "vad_enabled": false,
                "sensitivity_mode": "manual",
                "speech_detection_mode": "manual_threshold",
                "input_threshold_dbfs": -37.0,
                "speech_confirmation_ms": 200,
                "internal_pause_ms": 500,
                "endpoint_silence_ms": 1000,
                "pre_roll_ms": 275,
                "post_roll_ms": 225,
                "future_endpointing": {"mode": "adaptive"}
            }
        }));

        assert!(!config.recording.vad_enabled);
        assert_eq!(
            config.recording.speech_detection_mode,
            SpeechDetectionMode::ManualThreshold
        );
        assert_eq!(config.recording.input_threshold_dbfs, -37.0);
        assert_eq!(config.recording.speech_confirmation_ms, 200);
        assert_eq!(config.recording.internal_pause_ms, 500);
        assert_eq!(config.recording.endpoint_silence_ms, 1000);
        assert_eq!(config.recording.pre_roll_ms, 275);
        assert_eq!(config.recording.post_roll_ms, 225);
        let serialized = serde_json::to_value(config).unwrap();
        assert_eq!(
            serialized["recording"]["future_endpointing"],
            json!({"mode": "adaptive"})
        );
        assert_eq!(serialized["recording"]["sensitivity_mode"], json!("manual"));
        assert_eq!(
            serialized["recording"]["speech_detection_mode"],
            json!("manual_threshold")
        );
        assert_eq!(
            serialized["recording"]["input_threshold_dbfs"],
            json!(-37.0)
        );
    }

    #[test]
    fn streaming_mode_and_unknown_values_round_trip() {
        let (config, diagnostics) = parse_settings_value_with_diagnostics(json!({
            "schema_version": CURRENT_SCHEMA_VERSION,
            "streaming": {
                "mode": "rolling",
                "future_alignment": {"timestamps": true}
            }
        }));

        assert_eq!(config.streaming.mode, StreamingMode::Rolling);
        assert!(!diagnostics.invalid_values_salvaged);
        let serialized = serde_json::to_value(config).unwrap();
        assert_eq!(
            serialized["streaming"]["future_alignment"],
            json!({"timestamps": true})
        );
    }

    #[test]
    fn every_streaming_mode_round_trips_and_invalid_mode_is_salvaged() {
        for (stored, expected) in [
            ("auto", StreamingMode::Auto),
            ("rolling", StreamingMode::Rolling),
            ("final_only", StreamingMode::FinalOnly),
        ] {
            let (config, diagnostics) = parse_settings_value_with_diagnostics(json!({
                "schema_version": CURRENT_SCHEMA_VERSION,
                "streaming": {"mode": stored}
            }));
            assert_eq!(config.streaming.mode, expected);
            assert!(!diagnostics.invalid_values_salvaged);
            assert_eq!(
                serde_json::to_value(config).unwrap()["streaming"]["mode"],
                stored
            );
        }

        let (config, diagnostics) = parse_settings_value_with_diagnostics(json!({
            "schema_version": CURRENT_SCHEMA_VERSION,
            "streaming": {
                "mode": "native_streaming_claim",
                "future_alignment": {"kept": true}
            }
        }));
        assert_eq!(config.streaming.mode, StreamingMode::Auto);
        assert!(diagnostics.invalid_values_salvaged);
        assert_eq!(
            config.streaming.unknown["future_alignment"],
            json!({"kept": true})
        );
    }

    #[test]
    fn managed_install_salvages_bad_optional_metadata_and_preserves_future_fields() {
        let (config, diagnostics) = parse_settings_value_with_diagnostics(json!({
            "schema_version": CURRENT_SCHEMA_VERSION,
            "general": {
                "managed_models": {
                    "fixture": {
                        "path": "fixture.bin",
                        "source": "test",
                        "installed_at_unix_seconds": "invalid",
                        "future_receipt": {"signature": "kept"}
                    }
                }
            }
        }));

        let install = &config.general.managed_models["fixture"];
        assert_eq!(install.path, std::path::PathBuf::from("fixture.bin"));
        assert_eq!(install.source.as_deref(), Some("test"));
        assert_eq!(install.installed_at_unix_seconds, None);
        assert_eq!(
            install.unknown["future_receipt"],
            json!({"signature": "kept"})
        );
        assert!(diagnostics.invalid_values_salvaged);
        let serialized = serde_json::to_value(config).unwrap();
        assert_eq!(
            serialized["general"]["managed_models"]["fixture"]["future_receipt"],
            json!({"signature": "kept"})
        );
    }

    #[test]
    fn future_schema_and_unknown_root_and_section_fields_round_trip() {
        let value = json!({
            "schema_version": CURRENT_SCHEMA_VERSION + 7,
            "general": {
                "selected_default_model": "whisper_cpp_base_en",
                "future_general": [1, 2, 3]
            },
            "recording": {
                "manual_activation_rms": 0.99,
                "speech_probability_threshold": 0.35,
                "future_recording": {"kept": true}
            },
            "streaming": {"future_streaming": {"enabled": true}},
            "future_root": {"format": "new"}
        });

        let config = parse_settings_value(value);
        let serialized = serde_json::to_value(config).unwrap();

        assert_eq!(serialized["schema_version"], CURRENT_SCHEMA_VERSION + 7);
        assert_eq!(serialized["general"]["future_general"], json!([1, 2, 3]));
        assert_eq!(
            serialized["recording"]["manual_activation_rms"],
            json!(0.99)
        );
        assert_eq!(
            serialized["recording"]["speech_probability_threshold"],
            json!(0.35)
        );
        assert_eq!(
            serialized["recording"]["future_recording"],
            json!({"kept": true})
        );
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
