use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::super::normalize_config;
use super::{AppConfig, parse_settings_value_with_diagnostics};

pub struct SettingsStore {
    path: PathBuf,
    debounce: Duration,
    pending: Option<(Instant, AppConfig)>,
}

impl SettingsStore {
    pub fn new(path: PathBuf, debounce: Duration) -> Self {
        Self {
            path,
            debounce,
            pending: None,
        }
    }

    pub fn schedule(&mut self, config: &AppConfig) {
        self.pending = Some((Instant::now() + self.debounce, config.clone()));
    }

    pub fn flush_if_due(&mut self) -> Result<bool> {
        if self
            .pending
            .as_ref()
            .is_some_and(|(deadline, _)| Instant::now() >= *deadline)
        {
            self.flush()
        } else {
            Ok(false)
        }
    }

    pub fn flush(&mut self) -> Result<bool> {
        let Some((_, config)) = self.pending.as_ref() else {
            return Ok(false);
        };
        save_to_path(&self.path, config)?;
        self.pending = None;
        Ok(true)
    }

    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Discards a scheduled snapshot after another transactional path has
    /// persisted the current configuration successfully.
    pub fn mark_current_persisted(&mut self) {
        self.pending = None;
    }
}

pub(crate) fn load_from_path(path: &Path) -> Result<AppConfig> {
    if !path.exists() {
        let config = AppConfig::default();
        save_to_path(path, &config)?;
        return Ok(config);
    }

    let bytes =
        fs::read(path).with_context(|| format!("failed to read config {}", path.display()))?;
    let value: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => {
            backup_corrupt(path)?;
            let config = AppConfig::default();
            save_to_path(path, &config)?;
            return Ok(config);
        }
    };

    let original = value.clone();
    let (mut config, diagnostics) = parse_settings_value_with_diagnostics(value);
    normalize_config(&mut config);
    let rewritten = serde_json::to_value(&config)? != original;
    if rewritten {
        if diagnostics.invalid_values_salvaged {
            backup_corrupt(path)?;
        } else {
            backup_before_migration(path)?;
        }
        save_to_path(path, &config)?;
    }
    Ok(config)
}

pub(crate) fn save_to_path(path: &Path, config: &AppConfig) -> Result<()> {
    let content = serialized_config_bytes(config)?;
    atomic_write_bytes(path, &content)
}

pub(crate) fn artifact_config_fingerprint(config: &AppConfig) -> Result<String> {
    let normalized = normalized_config(config);
    let mut witness = serde_json::json!({
        "managed_models": normalized.general.managed_models,
        "managed_remote_models": normalized.general.managed_remote_models,
        "imported_gguf_models": normalized.general.imported_gguf_models,
        "managed_runtimes": normalized.general.managed_runtimes,
        "model_paths": normalized.general.model_paths,
    });
    // Preserve fingerprints produced by schema-v2 builds when there is no
    // exclusion. Once an included artifact is deleted, the non-empty opt-out
    // becomes the durable witness distinguishing rollback from commit.
    if !normalized.general.excluded_bundled_model_ids.is_empty() {
        witness["excluded_bundled_model_ids"] =
            serde_json::to_value(normalized.general.excluded_bundled_model_ids)?;
    }
    let canonical = canonical_json(witness);
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&canonical)?)
    ))
}

fn serialized_config_bytes(config: &AppConfig) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec_pretty(&normalized_config(config))?)
}

fn normalized_config(config: &AppConfig) -> AppConfig {
    let mut normalized = config.clone();
    normalize_config(&mut normalized);
    if normalized.schema_version <= super::super::CURRENT_SCHEMA_VERSION {
        normalized.schema_version = super::super::CURRENT_SCHEMA_VERSION;
    }
    normalized
}

fn canonical_json(value: Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.into_iter().map(canonical_json).collect()),
        Value::Object(values) => {
            let mut entries = values.into_iter().collect::<Vec<_>>();
            entries.sort_by(|(left, _), (right, _)| left.cmp(right));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key, canonical_json(value)))
                    .collect(),
            )
        }
        scalar => scalar,
    }
}

fn backup_corrupt(path: &Path) -> Result<PathBuf> {
    backup_original(path, "corrupt")
}

fn backup_before_migration(path: &Path) -> Result<PathBuf> {
    backup_original(path, "pre-v1-migration")
}

fn backup_original(path: &Path, reason: &str) -> Result<PathBuf> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("config path has no parent: {}", path.display()))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config");
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let backup = parent.join(format!("{file_name}.{reason}-{stamp}.bak"));
    fs::copy(path, &backup).with_context(|| {
        format!(
            "failed to back up corrupt config {} to {}",
            path.display(),
            backup.display()
        )
    })?;
    secure_file_permissions(&backup)?;
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(&backup)?
        .sync_all()?;
    Ok(backup)
}

pub(crate) fn atomic_write_bytes(path: &Path, content: &[u8]) -> Result<()> {
    atomic_write_with_replace(path, content, replace_file)
}

fn atomic_write_with_replace<F>(path: &Path, content: &[u8], replace: F) -> Result<()>
where
    F: FnOnce(&Path, &Path) -> io::Result<()>,
{
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("config path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create config directory {}", parent.display()))?;
    secure_directory_permissions(parent)?;
    let (temp_path, mut temp) = create_temp_file(path)?;
    let result = (|| -> Result<()> {
        temp.write_all(content)?;
        temp.flush()?;
        temp.sync_all()?;
        drop(temp);
        replace(&temp_path, path)?;
        sync_parent(parent)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result.with_context(|| format!("failed to atomically write {}", path.display()))
}

fn create_temp_file(path: &Path) -> Result<(PathBuf, File)> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("config path has no parent: {}", path.display()))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("config");
    for attempt in 0..100_u32 {
        let temp_path = parent.join(format!(
            ".{file_name}.{}.{}.tmp",
            std::process::id(),
            attempt
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&temp_path) {
            Ok(file) => return Ok((temp_path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Err(anyhow!("could not create a unique config temporary file"))
}

#[cfg(unix)]
fn secure_directory_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("failed to secure settings directory {}", path.display()))
}

#[cfg(not(unix))]
fn secure_directory_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn secure_file_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to secure settings file {}", path.display()))
}

#[cfg(not(unix))]
fn secure_file_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };

    let source = source
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    for attempt in 0..100 {
        let succeeded = unsafe {
            MoveFileExW(
                source.as_ptr(),
                destination.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if succeeded != 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if !matches!(error.raw_os_error(), Some(5 | 32)) || attempt == 99 {
            return Err(error);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    unreachable!()
}

#[cfg(unix)]
fn sync_parent(parent: &Path) -> io::Result<()> {
    File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent(_: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::thread;

    use super::*;

    static TEST_ID: AtomicU64 = AtomicU64::new(0);

    fn test_dir(name: &str) -> PathBuf {
        let id = TEST_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "scribe-settings-{name}-{}-{id}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn truncated_json_is_backed_up_before_defaults_are_regenerated() {
        let dir = test_dir("truncated");
        let path = dir.join("config.json");
        let corrupt = br#"{"general":{"selected_default_model":"whisper_cpp_base_en""#;
        fs::write(&path, corrupt).unwrap();

        let config = load_from_path(&path).unwrap();

        assert_eq!(config.schema_version, super::super::CURRENT_SCHEMA_VERSION);
        let backups = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .filter(|entry| {
                entry
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.starts_with("config.json.corrupt-") && name.ends_with(".bak")
                    })
            })
            .collect::<Vec<_>>();
        assert_eq!(backups.len(), 1);
        assert_eq!(fs::read(&backups[0]).unwrap(), corrupt);
        let regenerated: AppConfig = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            regenerated.schema_version,
            super::super::CURRENT_SCHEMA_VERSION
        );

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn invalid_known_field_is_backed_up_before_field_level_salvage() {
        let dir = test_dir("invalid-field");
        let path = dir.join("config.json");
        let original = br#"{
            "schema_version": 1,
            "general": {"selected_default_model": "whisper_cpp_base_en"},
            "recording": {"hotkey": "Ctrl+Alt+R", "max_recording_seconds": "invalid"}
        }"#;
        fs::write(&path, original).unwrap();

        let config = load_from_path(&path).unwrap();

        assert_eq!(config.general.selected_default_model, "whisper_cpp_base_en");
        assert_eq!(config.recording.hotkey, "Ctrl+Alt+R");
        assert_eq!(config.recording.max_recording_seconds, 30);
        let backup = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .find(|entry| {
                entry
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.starts_with("config.json.corrupt-") && name.ends_with(".bak")
                    })
            })
            .expect("invalid source document backup");
        assert_eq!(fs::read(backup).unwrap(), original);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn valid_legacy_config_is_backed_up_before_sectioned_migration() {
        let dir = test_dir("legacy-backup");
        let path = dir.join("config.json");
        let original = br#"{"hotkey":"Alt+Space","selected_default_model":"whisper_cpp_base_en"}"#;
        fs::write(&path, original).unwrap();

        let config = load_from_path(&path).unwrap();

        assert_eq!(config.recording.hotkey, "Alt+Space");
        let backup = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .find(|entry| {
                entry
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.starts_with("config.json.pre-v1-migration-") && name.ends_with(".bak")
                    })
            })
            .expect("pre-migration backup");
        assert_eq!(fs::read(backup).unwrap(), original);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn copied_profile_preserves_retired_settings_unknown_fields_and_artifact_bytes() {
        let dir = test_dir("retired-provider-profile");
        let path = dir.join("config.json");
        let artifact = dir.join("copied-profile").join("legacy-model.bin");
        let runtime = dir.join("copied-profile").join("legacy-runner.py");
        let sentinel = b"legacy-provider-artifact-sentinel\0\xff";
        fs::create_dir_all(artifact.parent().unwrap()).unwrap();
        fs::write(&artifact, sentinel).unwrap();
        fs::write(&runtime, b"legacy runner sentinel").unwrap();
        let original = serde_json::json!({
            "schema_version": super::super::CURRENT_SCHEMA_VERSION,
            "general": {
                "selected_default_model": "faster_whisper",
                "playground_selected_models": [
                    "faster_whisper_tiny_en",
                    "whisper_cpp_small_en"
                ],
                "playground_model_order": [
                    "faster_whisper_tiny_en",
                    "whisper_cpp_small_en"
                ],
                "model_paths": {
                    "faster_whisper": artifact
                },
                "managed_models": {
                    "faster_whisper_tiny_en": {
                        "path": artifact,
                        "source": "copied-profile",
                        "future_receipt": {"preserved": true}
                    }
                },
                "managed_runtimes": {
                    "faster_whisper": {
                        "path": runtime,
                        "source": "copied-profile",
                        "future_runtime_receipt": {"preserved": true}
                    }
                },
                "future_general": {"preserved": true}
            },
            "future_root": {"preserved": true}
        });
        fs::write(&path, serde_json::to_vec_pretty(&original).unwrap()).unwrap();

        let config = load_from_path(&path).unwrap();

        assert_eq!(
            config.general.selected_default_model,
            crate::model_catalog::BUNDLED_BASE_MODEL_ID
        );
        assert_eq!(
            config.general.playground_selected_models,
            ["whisper_cpp_small_en"]
        );
        assert_eq!(config.general.model_paths["faster_whisper"], artifact);
        assert_eq!(
            config.general.managed_models["faster_whisper_tiny_en"].unknown["future_receipt"],
            serde_json::json!({"preserved": true})
        );
        assert_eq!(
            config.general.managed_runtimes["faster_whisper"].unknown["future_runtime_receipt"],
            serde_json::json!({"preserved": true})
        );
        assert_eq!(
            config.general.unknown["future_general"],
            serde_json::json!({"preserved": true})
        );
        assert_eq!(
            config.unknown["future_root"],
            serde_json::json!({"preserved": true})
        );
        assert_eq!(fs::read(&artifact).unwrap(), sentinel);
        assert_eq!(fs::read(&runtime).unwrap(), b"legacy runner sentinel");

        let rewritten: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            rewritten["general"]["model_paths"]["faster_whisper"],
            serde_json::json!(artifact)
        );
        assert_eq!(
            rewritten["general"]["managed_models"]["faster_whisper_tiny_en"]["future_receipt"],
            serde_json::json!({"preserved": true})
        );
        assert_eq!(
            rewritten["general"]["managed_runtimes"]["faster_whisper"]["future_runtime_receipt"],
            serde_json::json!({"preserved": true})
        );
        assert_eq!(fs::read(&artifact).unwrap(), sentinel);
        assert_eq!(fs::read(&runtime).unwrap(), b"legacy runner sentinel");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn version_two_voice_detection_settings_are_rewritten_and_reload_cleanly() {
        let dir = test_dir("version-two-voice-detection");
        let path = dir.join("config.json");
        fs::write(
            &path,
            br#"{
                "schema_version": 2,
                "recording": {
                    "speech_probability_threshold": 0.35,
                    "manual_activation_rms": 0.1
                }
            }"#,
        )
        .unwrap();

        let migrated = load_from_path(&path).unwrap();
        assert_eq!(
            migrated.recording.speech_detection_mode,
            super::super::SpeechDetectionMode::Ai
        );
        assert_eq!(
            migrated.recording.input_threshold_dbfs,
            super::super::DEFAULT_INPUT_THRESHOLD_DBFS
        );

        let rewritten: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            rewritten["schema_version"],
            Value::from(super::super::CURRENT_SCHEMA_VERSION)
        );
        assert_eq!(rewritten["recording"]["speech_detection_mode"], "ai");
        assert_eq!(
            rewritten["recording"]["input_threshold_dbfs"],
            super::super::DEFAULT_INPUT_THRESHOLD_DBFS
        );
        assert!(
            rewritten["recording"]
                .get("speech_probability_threshold")
                .is_none()
        );
        assert!(
            rewritten["recording"]
                .get("manual_activation_rms")
                .is_none()
        );

        let reloaded = load_from_path(&path).unwrap();
        assert_eq!(
            reloaded.recording.speech_detection_mode,
            migrated.recording.speech_detection_mode
        );
        assert_eq!(
            reloaded.recording.input_threshold_dbfs,
            migrated.recording.input_threshold_dbfs
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn injected_replace_failure_preserves_the_previous_file() {
        let dir = test_dir("replace-failure");
        let path = dir.join("config.json");
        fs::write(&path, b"old-settings").unwrap();

        let error = atomic_write_with_replace(&path, b"new-settings", |_, _| {
            Err(io::Error::other("injected replacement failure"))
        })
        .unwrap_err();

        assert!(error.to_string().contains("atomically write"));
        assert_eq!(fs::read(&path).unwrap(), b"old-settings");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn atomic_replacement_is_observed_as_only_the_old_or_new_content() {
        let dir = test_dir("atomic-visibility");
        let path = dir.join("config.json");
        let old = vec![b'a'; 128 * 1024];
        let new = vec![b'b'; 128 * 1024];
        fs::write(&path, &old).unwrap();
        let reading = Arc::new(AtomicBool::new(true));
        let reader_path = path.clone();
        let reader_old = old.clone();
        let reader_new = new.clone();
        let reader_flag = Arc::clone(&reading);
        let reader = thread::spawn(move || {
            while reader_flag.load(Ordering::Acquire) {
                let content = fs::read(&reader_path).unwrap();
                assert!(content == reader_old || content == reader_new);
            }
        });

        for index in 0..20 {
            atomic_write_bytes(&path, if index % 2 == 0 { &new } else { &old }).unwrap();
        }
        reading.store(false, Ordering::Release);
        reader.join().unwrap();

        let final_content = fs::read(&path).unwrap();
        assert!(final_content == old || final_content == new);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn settings_store_debounces_and_explicitly_flushes_latest_snapshot() {
        let dir = test_dir("store");
        let path = dir.join("config.json");
        let mut store = SettingsStore::new(path.clone(), Duration::from_secs(60));
        let mut first = AppConfig::default();
        first.recording.hotkey = "First".to_owned();
        first.recording.input_threshold_dbfs = -36.0;
        let mut latest = first.clone();
        latest.recording.hotkey = "Latest".to_owned();
        latest.recording.input_threshold_dbfs = -52.0;

        store.schedule(&first);
        store.schedule(&latest);
        assert!(store.has_pending());
        assert!(!store.flush_if_due().unwrap());
        assert!(store.flush().unwrap());
        assert!(!store.has_pending());

        let persisted = load_from_path(&path).unwrap();
        assert_eq!(persisted.recording.hotkey, "Latest");
        assert!((persisted.recording.input_threshold_dbfs + 52.0).abs() < f32::EPSILON);
        assert!(!store.flush().unwrap());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn transactional_save_discards_an_older_scheduled_snapshot() {
        let dir = test_dir("transactional-save");
        let path = dir.join("config.json");
        let mut store = SettingsStore::new(path.clone(), Duration::from_secs(60));
        let mut stale = AppConfig::default();
        stale.recording.hotkey = "Stale".to_owned();
        let mut current = stale.clone();
        current.recording.hotkey = "Persisted transaction".to_owned();

        store.schedule(&stale);
        save_to_path(&path, &current).unwrap();
        store.mark_current_persisted();

        assert!(!store.has_pending());
        assert!(!store.flush().unwrap());
        let persisted: AppConfig = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(persisted.recording.hotkey, "Persisted transaction");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn artifact_config_fingerprint_is_stable_across_restart_and_unrelated_settings() {
        let dir = test_dir("fingerprint");
        let path = dir.join("config.json");
        let mut config = AppConfig::default();
        config.general.model_paths.insert(
            "whisper_cpp_small_en".to_owned(),
            PathBuf::from("models/small.bin"),
        );
        config.general.model_paths.insert(
            "whisper_cpp_base_en".to_owned(),
            PathBuf::from("models/base.bin"),
        );
        config.general.unknown.insert(
            "future_z".to_owned(),
            serde_json::json!({"beta": 2, "alpha": 1}),
        );
        config
            .general
            .unknown
            .insert("future_a".to_owned(), serde_json::json!([3, 2, 1]));
        let expected = artifact_config_fingerprint(&config).unwrap();

        save_to_path(&path, &config).unwrap();
        let restarted = load_from_path(&path).unwrap();

        assert_eq!(artifact_config_fingerprint(&restarted).unwrap(), expected);
        let mut changed = restarted.clone();
        changed.recording.hotkey = "Different".to_owned();
        assert_eq!(artifact_config_fingerprint(&changed).unwrap(), expected);
        changed
            .general
            .excluded_bundled_model_ids
            .push(crate::model_catalog::BUNDLED_BASE_MODEL_ID.to_owned());
        assert_ne!(artifact_config_fingerprint(&changed).unwrap(), expected);
        changed.general.excluded_bundled_model_ids.clear();
        assert_eq!(artifact_config_fingerprint(&changed).unwrap(), expected);
        changed.general.model_paths.insert(
            "whisper_cpp_tiny_en".to_owned(),
            PathBuf::from("models/tiny.bin"),
        );
        assert_ne!(artifact_config_fingerprint(&changed).unwrap(), expected);

        let mut remote_changed = restarted.clone();
        let remote_storage = dir.join("remote-model-storage");
        remote_changed.general.model_storage_dir = remote_storage.clone();
        let repository = "handy-computer/model";
        let revision = "a".repeat(40);
        let filename = "model.gguf";
        let remote_id = crate::config::managed_remote_model_id(repository, &revision, filename)
            .expect("fixture remote ID");
        let remote_path = remote_storage
            .join("huggingface")
            .join("handy-computer")
            .join("model")
            .join(&revision)
            .join(&remote_id)
            .join(filename);
        fs::create_dir_all(remote_path.parent().unwrap()).unwrap();
        fs::write(&remote_path, b"x").unwrap();
        remote_changed.general.managed_remote_models.insert(
            remote_id,
            crate::config::ManagedRemoteModelInstall {
                repository: repository.to_owned(),
                revision,
                filename: filename.to_owned(),
                expected_size_bytes: 1,
                expected_sha256: "b".repeat(64),
                path: remote_path,
                display_name: "Remote fixture".to_owned(),
                description: String::new(),
                languages: vec!["en".to_owned()],
                recommended: false,
                installed_at_unix_seconds: None,
            },
        );
        assert_ne!(
            artifact_config_fingerprint(&remote_changed).unwrap(),
            expected
        );

        let mut imported_changed = restarted;
        let imported_path = dir.join("external.gguf");
        fs::write(&imported_path, b"x").unwrap();
        imported_changed.general.imported_gguf_models.insert(
            format!("local-{}", "c".repeat(64)),
            crate::config::ImportedGgufModelInstall::validated(
                imported_path,
                1,
                "c".repeat(64),
                "Imported fixture".to_owned(),
            ),
        );
        assert_ne!(
            artifact_config_fingerprint(&imported_changed).unwrap(),
            expected
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
