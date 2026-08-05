use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;

use crate::config::{self, AppConfig};
use crate::models::{SttModelInfo, TranscriptResult, TranscriptSegment, default_model_catalog};

use super::SttBackend;

const MIN_VOSK_RUNNER_REVISION: u32 = 3;

pub struct VoskBackend {
    executable_path: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct RunnerOutput {
    text: String,
    #[serde(default)]
    segments: Vec<RunnerSegment>,
    duration_ms: Option<u128>,
}

#[derive(Debug, Deserialize)]
struct RunnerSegment {
    start_ms: Option<u64>,
    end_ms: Option<u64>,
    text: String,
}

#[derive(Debug, Deserialize)]
struct RuntimeManifest {
    runner_revision: Option<u32>,
}

impl VoskBackend {
    pub fn new(executable_path: Option<PathBuf>) -> Self {
        Self { executable_path }
    }
}

impl SttBackend for VoskBackend {
    fn id(&self) -> &str {
        "Vosk"
    }

    fn list_models(&self) -> Vec<SttModelInfo> {
        default_model_catalog()
            .into_iter()
            .filter(|model| model.backend == "Vosk")
            .collect()
    }

    fn transcribe(&self, audio_path: PathBuf, model: SttModelInfo) -> Result<TranscriptResult> {
        let executable = self.executable_path.clone().ok_or_else(|| {
            anyhow!(
                "Vosk runtime is not installed. Install the Vosk runtime from Models, or set SCRIBE_VOSK_CLI for development."
            )
        })?;
        let model_path = model
            .local_path
            .clone()
            .ok_or_else(|| anyhow!("download {} before transcribing", model.name))?;

        if !executable.exists() {
            return Err(anyhow!(
                "Vosk runner does not exist: {}",
                executable.display()
            ));
        }
        if !model_path.exists() {
            return Err(anyhow!(
                "model directory does not exist for {}: {}",
                model.name,
                model_path.display()
            ));
        }
        if !config::is_valid_model_install_path(&model, &model_path) {
            return Err(anyhow!(
                "Vosk model directory is incomplete for {}: {}. Reinstall this model from Models.",
                model.name,
                model_path.display()
            ));
        }
        if !audio_path.exists() {
            return Err(anyhow!(
                "audio file does not exist: {}",
                audio_path.display()
            ));
        }

        let started = Instant::now();
        let output = Command::new(&executable)
            .args(vosk_args(&model_path, &audio_path))
            .output()
            .with_context(|| format!("failed to run {}", executable.display()))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            return Err(anyhow!(
                "Vosk failed with status {}\n{}",
                output.status,
                runner_error_message(&stderr)
            ));
        }

        let parsed = parse_runner_output(&stdout)?;
        let segments = parsed
            .segments
            .into_iter()
            .map(|segment| TranscriptSegment {
                start_ms: segment.start_ms,
                end_ms: segment.end_ms,
                text: segment.text,
            })
            .collect::<Vec<_>>();
        let text = parsed.text;
        let segments = if segments.is_empty() {
            vec![TranscriptSegment {
                start_ms: None,
                end_ms: None,
                text: text.clone(),
            }]
        } else {
            segments
        };

        Ok(TranscriptResult {
            model_id: model.id,
            model_name: model.name,
            backend: "Vosk".to_owned(),
            segments,
            text,
            duration_ms: parsed
                .duration_ms
                .or_else(|| Some(started.elapsed().as_millis())),
            stdout,
            stderr,
        })
    }
}

pub fn resolve_vosk_executable(config: &AppConfig) -> Option<PathBuf> {
    resolve_vosk_executable_from_candidates(
        bundled_runtime_root(),
        managed_runtime_roots(config),
        dev_runtime_paths(),
    )
}

fn bundled_runtime_root() -> Option<PathBuf> {
    env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
}

fn managed_runtime_roots(config: &AppConfig) -> Vec<PathBuf> {
    [config::managed_runtime_path(config, "Vosk")]
        .into_iter()
        .flatten()
        .collect()
}

fn dev_runtime_paths() -> Vec<PathBuf> {
    [env::var_os("SCRIBE_VOSK_CLI").map(PathBuf::from)]
        .into_iter()
        .flatten()
        .collect()
}

pub(crate) fn resolve_vosk_executable_from_candidates(
    bundled_roots: impl IntoIterator<Item = PathBuf>,
    managed_roots: impl IntoIterator<Item = PathBuf>,
    dev_paths: impl IntoIterator<Item = PathBuf>,
) -> Option<PathBuf> {
    first_existing_path(
        bundled_roots
            .into_iter()
            .flat_map(|root| vosk_runtime_candidates(&root))
            .chain(
                managed_roots
                    .into_iter()
                    .flat_map(|root| vosk_runtime_candidates(&root)),
            )
            .chain(dev_paths),
    )
}

fn vosk_runtime_candidates(root: &Path) -> Vec<PathBuf> {
    if root.as_os_str().is_empty() {
        return Vec::new();
    }
    if root.is_file() {
        return vec![root.to_path_buf()];
    }

    vosk_runner_names()
        .iter()
        .flat_map(|&binary_name| {
            [
                root.join("runtimes")
                    .join("vosk")
                    .join("bin")
                    .join(binary_name),
                root.join("bin").join(binary_name),
                root.join(binary_name),
            ]
        })
        .collect()
}

fn vosk_runner_names() -> &'static [&'static str] {
    if cfg!(windows) {
        &["scribe-vosk.exe", "scribe-vosk.bat"]
    } else {
        &["scribe-vosk"]
    }
}

fn first_existing_path(paths: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    let mut seen = Vec::new();
    for path in paths {
        if path.as_os_str().is_empty() || seen.iter().any(|seen_path| seen_path == &path) {
            continue;
        }
        seen.push(path.clone());
        if is_vosk_runtime_usable(&path) {
            return Some(path);
        }
    }
    None
}

pub(crate) fn is_vosk_runtime_usable(path: &Path) -> bool {
    if !path.exists() {
        return false;
    }
    if !is_packaged_runner_path(path) {
        return true;
    }
    let Some(runtime_root) = packaged_runtime_root(path) else {
        return false;
    };
    runtime_root.join("bin").join("vosk_runner.py").is_file()
        && runtime_root.join(venv_python_relative_path()).is_file()
        && vosk_manifest_has_supported_runner(&runtime_root)
}

fn is_packaged_runner_path(path: &Path) -> bool {
    path.parent()
        .and_then(|parent| parent.file_name())
        .is_some_and(|name| name == "bin")
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| vosk_runner_names().contains(&name))
}

fn packaged_runtime_root(path: &Path) -> Option<PathBuf> {
    path.parent()?.parent().map(Path::to_path_buf)
}

fn venv_python_relative_path() -> &'static Path {
    if cfg!(windows) {
        Path::new("venv/Scripts/python.exe")
    } else {
        Path::new("venv/bin/python")
    }
}

fn vosk_manifest_has_supported_runner(runtime_root: &Path) -> bool {
    fs::read_to_string(runtime_root.join("runtime-manifest.json"))
        .ok()
        .and_then(|contents| serde_json::from_str::<RuntimeManifest>(&contents).ok())
        .and_then(|manifest| manifest.runner_revision)
        .is_some_and(|revision| revision >= MIN_VOSK_RUNNER_REVISION)
}

fn vosk_args(model_path: &Path, audio_path: &Path) -> Vec<OsString> {
    vec![
        OsString::from("transcribe"),
        OsString::from("--model"),
        model_path.as_os_str().to_owned(),
        OsString::from("--audio"),
        audio_path.as_os_str().to_owned(),
    ]
}

fn parse_runner_output(stdout: &str) -> Result<RunnerOutput> {
    serde_json::from_str(stdout.trim()).with_context(|| "failed to parse Vosk JSON output")
}

fn runner_error_message(stderr: &str) -> String {
    #[derive(Deserialize)]
    struct RunnerError {
        error: String,
    }

    stderr
        .lines()
        .rev()
        .find_map(|line| serde_json::from_str::<RunnerError>(line).ok())
        .map(|payload| payload.error)
        .unwrap_or_else(|| stderr.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn vosk_args_include_model_and_audio_paths() {
        let args = vosk_args(Path::new("/models/vosk"), Path::new("/tmp/audio.wav"))
            .into_iter()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect::<Vec<_>>();

        assert_eq!(args[0], "transcribe");
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--model", "/models/vosk"])
        );
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--audio", "/tmp/audio.wav"])
        );
    }

    #[test]
    fn parses_runner_json_output() {
        let output = parse_runner_output(
            r#"{"text":"hello world","segments":[{"start_ms":0,"end_ms":1200,"text":"hello world"}],"duration_ms":42}"#,
        )
        .unwrap();

        assert_eq!(output.text, "hello world");
        assert_eq!(output.segments[0].start_ms, Some(0));
        assert_eq!(output.duration_ms, Some(42));
    }

    #[test]
    fn parses_empty_runner_json_as_empty_transcript() {
        let output = parse_runner_output(r#"{"text":"","segments":[],"duration_ms":7}"#).unwrap();

        assert_eq!(output.text, "");
        assert!(output.segments.is_empty());
        assert_eq!(output.duration_ms, Some(7));
    }

    #[test]
    fn missing_runtime_errors_before_model_validation() {
        let mut model = default_model_catalog()
            .into_iter()
            .find(|model| model.id == "vosk_small_en")
            .expect("Vosk model exists in catalog");
        model.local_path = Some(PathBuf::from("/tmp/scribe-missing-vosk-model"));

        let err = VoskBackend::new(None)
            .transcribe(PathBuf::from("/tmp/scribe-missing-audio.wav"), model)
            .unwrap_err()
            .to_string();

        assert!(err.contains("Vosk runtime is not installed"));
        assert!(err.contains("SCRIBE_VOSK_CLI"));
    }

    #[test]
    fn resolver_prefers_bundled_before_managed_and_dev_paths() {
        let root = test_runtime_root("prefers-bundled");
        let bundled_root = root.join("bundled");
        let managed_root = root.join("managed");
        let dev_runtime = root.join("dev").join(vosk_runner_names()[0]);
        let bundled_runtime = bundled_root
            .join("runtimes")
            .join("vosk")
            .join("bin")
            .join(vosk_runner_names()[0]);
        let managed_runtime = managed_root.join("bin").join(vosk_runner_names()[0]);
        write_test_runtime(&bundled_runtime);
        write_test_runtime(&managed_runtime);
        write_test_runtime(&dev_runtime);

        let resolved =
            resolve_vosk_executable_from_candidates([bundled_root], [managed_root], [dev_runtime]);

        assert_eq!(resolved, Some(bundled_runtime));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn resolver_skips_broken_packaged_runtime_before_dev_path() {
        let root = test_runtime_root("skips-broken-packaged");
        let managed_root = root.join("managed");
        let broken_runtime = managed_root.join("bin").join(vosk_runner_names()[0]);
        let dev_runtime = root.join("dev").join("scribe-vosk-dev");
        fs::create_dir_all(broken_runtime.parent().unwrap()).unwrap();
        fs::write(&broken_runtime, b"broken Vosk runtime").unwrap();
        write_test_runtime(&dev_runtime);

        let resolved =
            resolve_vosk_executable_from_candidates([], [managed_root], [dev_runtime.clone()]);

        assert_eq!(resolved, Some(dev_runtime));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn resolver_skips_stale_packaged_runner_revision_before_dev_path() {
        let root = test_runtime_root("skips-stale-revision");
        let managed_root = root.join("managed");
        let managed_runtime = managed_root.join("bin").join(vosk_runner_names()[0]);
        let dev_runtime = root.join("dev").join("scribe-vosk-dev");
        write_test_runtime_with_revision(&managed_runtime, 2);
        write_test_runtime(&dev_runtime);

        let resolved =
            resolve_vosk_executable_from_candidates([], [managed_root], [dev_runtime.clone()]);

        assert_eq!(resolved, Some(dev_runtime));
        let _ = fs::remove_dir_all(root);
    }

    fn test_runtime_root(name: &str) -> PathBuf {
        env::temp_dir().join(format!("scribe-vosk-runtime-{name}-{}", std::process::id()))
    }

    fn write_test_runtime(path: &Path) {
        write_test_runtime_with_revision(path, 3);
    }

    fn write_test_runtime_with_revision(path: &Path, runner_revision: u32) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"Vosk runtime").unwrap();
        if let Some(runtime_root) =
            packaged_runtime_root(path).filter(|_| is_packaged_runner_path(path))
        {
            let runner = runtime_root.join("bin").join("vosk_runner.py");
            let python = runtime_root.join(venv_python_relative_path());
            let manifest = runtime_root.join("runtime-manifest.json");
            fs::create_dir_all(runner.parent().unwrap()).unwrap();
            fs::create_dir_all(python.parent().unwrap()).unwrap();
            fs::write(runner, b"runner").unwrap();
            fs::write(python, b"python").unwrap();
            fs::write(
                manifest,
                format!(r#"{{"runner_revision":{runner_revision}}}"#),
            )
            .unwrap();
        }
    }
}
