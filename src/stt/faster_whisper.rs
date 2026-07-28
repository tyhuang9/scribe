use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;

use crate::config::{self, AppConfig, WhisperComputeMode};
use crate::models::{SttModelInfo, TranscriptResult, TranscriptSegment, default_model_catalog};

use super::SttBackend;

pub struct FasterWhisperBackend {
    executable_path: Option<PathBuf>,
    options: FasterWhisperOptions,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FasterWhisperOptions {
    pub compute_mode: WhisperComputeMode,
    pub gpu_device: u32,
    pub cuda_library_paths: Vec<PathBuf>,
}

impl Default for FasterWhisperOptions {
    fn default() -> Self {
        Self {
            compute_mode: WhisperComputeMode::Auto,
            gpu_device: 0,
            cuda_library_paths: Vec::new(),
        }
    }
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

impl FasterWhisperBackend {
    pub fn new(executable_path: Option<PathBuf>, options: FasterWhisperOptions) -> Self {
        Self {
            executable_path,
            options,
        }
    }
}

impl SttBackend for FasterWhisperBackend {
    fn id(&self) -> &str {
        "faster-whisper"
    }

    fn list_models(&self) -> Vec<SttModelInfo> {
        default_model_catalog()
            .into_iter()
            .filter(|model| model.backend == "faster-whisper")
            .collect()
    }

    fn transcribe(&self, audio_path: PathBuf, model: SttModelInfo) -> Result<TranscriptResult> {
        let executable = self.executable_path.clone().ok_or_else(|| {
            anyhow!(
                "faster-whisper runtime is not installed. Install the faster-whisper runtime from Models, or set SCRIBE_FASTER_WHISPER_CLI for development."
            )
        })?;
        let model_path = model
            .local_path
            .clone()
            .ok_or_else(|| anyhow!("download {} before transcribing", model.name))?;

        if !executable.exists() {
            return Err(anyhow!(
                "faster-whisper runner does not exist: {}",
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
                "faster-whisper model directory is incomplete for {}: {}. Reinstall this model from Models.",
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
        let mut command = Command::new(&executable);
        command.args(faster_whisper_args(&model_path, &audio_path, &self.options));
        apply_faster_whisper_environment(&mut command, &executable, &self.options)?;
        let output = command
            .output()
            .with_context(|| format!("failed to run {}", executable.display()))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            return Err(anyhow!(
                "faster-whisper failed with status {}\n{}",
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
        let text = if parsed.text.trim().is_empty() {
            stdout.trim().to_owned()
        } else {
            parsed.text
        };
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
            backend: "faster-whisper".to_owned(),
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

pub fn resolve_faster_whisper_executable(config: &AppConfig) -> Option<PathBuf> {
    resolve_faster_whisper_executable_from_candidates(
        bundled_runtime_root(),
        managed_runtime_roots(config),
        dev_runtime_paths(),
    )
}

pub fn resolve_faster_whisper_packaged_executable(config: &AppConfig) -> Option<PathBuf> {
    resolve_faster_whisper_executable_from_candidates(
        bundled_runtime_root(),
        managed_runtime_roots(config),
        [],
    )
}

fn bundled_runtime_root() -> Option<PathBuf> {
    env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
}

fn managed_runtime_roots(config: &AppConfig) -> Vec<PathBuf> {
    [config::managed_runtime_path(config, "faster-whisper")]
        .into_iter()
        .flatten()
        .collect()
}

fn dev_runtime_paths() -> Vec<PathBuf> {
    [env::var_os("SCRIBE_FASTER_WHISPER_CLI").map(PathBuf::from)]
        .into_iter()
        .flatten()
        .collect()
}

pub(crate) fn resolve_faster_whisper_executable_from_candidates(
    bundled_roots: impl IntoIterator<Item = PathBuf>,
    managed_roots: impl IntoIterator<Item = PathBuf>,
    dev_paths: impl IntoIterator<Item = PathBuf>,
) -> Option<PathBuf> {
    first_existing_path(
        bundled_roots
            .into_iter()
            .flat_map(|root| faster_whisper_runtime_candidates(&root))
            .chain(
                managed_roots
                    .into_iter()
                    .flat_map(|root| faster_whisper_runtime_candidates(&root)),
            )
            .chain(dev_paths),
    )
}

fn faster_whisper_runtime_candidates(root: &Path) -> Vec<PathBuf> {
    if root.as_os_str().is_empty() {
        return Vec::new();
    }
    if root.is_file() {
        return vec![root.to_path_buf()];
    }

    faster_whisper_runner_names()
        .iter()
        .flat_map(|&binary_name| {
            [
                root.join("runtimes")
                    .join("faster_whisper")
                    .join("bin")
                    .join(binary_name),
                root.join("runtimes")
                    .join("faster-whisper")
                    .join("bin")
                    .join(binary_name),
                root.join("bin").join(binary_name),
                root.join(binary_name),
            ]
        })
        .collect()
}

fn faster_whisper_runner_names() -> &'static [&'static str] {
    if cfg!(windows) {
        &["scribe-faster-whisper.exe", "scribe-faster-whisper.bat"]
    } else {
        &["scribe-faster-whisper"]
    }
}

fn first_existing_path(paths: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    let mut seen = Vec::new();
    for path in paths {
        if path.as_os_str().is_empty() || seen.iter().any(|seen_path| seen_path == &path) {
            continue;
        }
        seen.push(path.clone());
        if is_faster_whisper_runtime_usable(&path) {
            return Some(path);
        }
    }
    None
}

pub(crate) fn is_faster_whisper_runtime_usable(path: &Path) -> bool {
    if !path.exists() {
        return false;
    }
    if !is_packaged_runner_path(path) {
        return true;
    }
    let Some(runtime_root) = packaged_runtime_root(path) else {
        return false;
    };
    if crate::runtime_artifacts::is_portable_runtime_entrypoint("faster_whisper", path) {
        return true;
    }
    runtime_root
        .join("bin")
        .join("faster_whisper_runner.py")
        .is_file()
        && runtime_root.join(venv_python_relative_path()).is_file()
}

fn is_packaged_runner_path(path: &Path) -> bool {
    path.parent()
        .and_then(|parent| parent.file_name())
        .is_some_and(|name| name == "bin")
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| faster_whisper_runner_names().contains(&name))
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

fn faster_whisper_args(
    model_path: &Path,
    audio_path: &Path,
    options: &FasterWhisperOptions,
) -> Vec<OsString> {
    vec![
        OsString::from("transcribe"),
        OsString::from("--model"),
        model_path.as_os_str().to_owned(),
        OsString::from("--audio"),
        audio_path.as_os_str().to_owned(),
        OsString::from("--device-mode"),
        OsString::from(device_mode_arg(options.compute_mode)),
        OsString::from("--gpu-device"),
        OsString::from(options.gpu_device.to_string()),
    ]
}

fn apply_faster_whisper_environment(
    command: &mut Command,
    executable_path: &Path,
    options: &FasterWhisperOptions,
) -> Result<()> {
    if options.compute_mode == WhisperComputeMode::Cpu {
        return Ok(());
    }

    if let Some(library_path) = joined_library_path(
        bundled_runtime_library_paths(executable_path)
            .into_iter()
            .chain(options.cuda_library_paths.clone())
            .collect::<Vec<_>>()
            .as_slice(),
    )? {
        command.env("LD_LIBRARY_PATH", library_path);
    }

    Ok(())
}

fn bundled_runtime_library_paths(executable_path: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(bin_dir) = executable_path.parent() {
        paths.push(bin_dir.to_path_buf());
        if let Some(runtime_root) = bin_dir.parent() {
            paths.push(runtime_root.join("lib"));
            paths.push(runtime_root.join("cuda"));
            paths.push(runtime_root.join("cuda_v12"));
            paths.push(runtime_root.join("cuda_v13"));
        }
    }
    paths.into_iter().filter(|path| path.exists()).collect()
}

fn joined_library_path(paths: &[PathBuf]) -> Result<Option<OsString>> {
    let mut values = paths
        .iter()
        .filter(|path| !path.as_os_str().is_empty())
        .cloned()
        .collect::<Vec<_>>();

    if let Some(existing) = env::var_os("LD_LIBRARY_PATH") {
        values.extend(env::split_paths(&existing));
    }
    if values.is_empty() {
        return Ok(None);
    }

    env::join_paths(values)
        .map(Some)
        .with_context(|| "failed to build LD_LIBRARY_PATH for faster-whisper")
}

fn device_mode_arg(mode: WhisperComputeMode) -> &'static str {
    match mode {
        WhisperComputeMode::Auto => "auto",
        WhisperComputeMode::PreferGpu => "gpu",
        WhisperComputeMode::Cpu => "cpu",
    }
}

fn parse_runner_output(stdout: &str) -> Result<RunnerOutput> {
    serde_json::from_str(stdout.trim())
        .with_context(|| "failed to parse faster-whisper JSON output")
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
    fn faster_whisper_args_map_compute_modes() {
        let args = faster_whisper_args(
            Path::new("/models/tiny"),
            Path::new("/tmp/audio.wav"),
            &FasterWhisperOptions {
                compute_mode: WhisperComputeMode::PreferGpu,
                gpu_device: 2,
                ..FasterWhisperOptions::default()
            },
        );

        let args = args
            .into_iter()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect::<Vec<_>>();

        assert!(args.windows(2).any(|pair| pair == ["--device-mode", "gpu"]));
        assert!(args.windows(2).any(|pair| pair == ["--gpu-device", "2"]));
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
    fn rejects_incomplete_model_directory_before_running_runner() {
        let root = test_runtime_root("incomplete-model");
        let runner = root.join("bin").join(faster_whisper_runner_names()[0]);
        let model_dir = root.join("model");
        let audio_path = root.join("audio.wav");
        write_test_runtime(&runner);
        fs::create_dir_all(&model_dir).unwrap();
        fs::write(model_dir.join("config.json"), b"{}").unwrap();
        fs::write(&audio_path, b"wav").unwrap();

        let mut model = default_model_catalog()
            .into_iter()
            .find(|model| model.id == "faster_whisper_tiny_en")
            .expect("faster-whisper tiny model exists in catalog");
        model.local_path = Some(model_dir.clone());

        let backend = FasterWhisperBackend::new(Some(runner), FasterWhisperOptions::default());
        let err = backend
            .transcribe(audio_path, model)
            .unwrap_err()
            .to_string();

        assert!(err.contains("incomplete"));
        assert!(err.contains(&model_dir.display().to_string()));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn resolver_prefers_bundled_before_managed_and_dev_paths() {
        let root = test_runtime_root("prefers-bundled");
        let bundled_root = root.join("bundled");
        let managed_root = root.join("managed");
        let dev_runtime = root.join("dev").join(faster_whisper_runner_names()[0]);
        let bundled_runtime = bundled_root
            .join("runtimes")
            .join("faster_whisper")
            .join("bin")
            .join(faster_whisper_runner_names()[0]);
        let managed_runtime = managed_root
            .join("bin")
            .join(faster_whisper_runner_names()[0]);
        write_test_runtime(&bundled_runtime);
        write_test_runtime(&managed_runtime);
        write_test_runtime(&dev_runtime);

        let resolved = resolve_faster_whisper_executable_from_candidates(
            [bundled_root],
            [managed_root],
            [dev_runtime],
        );

        assert_eq!(resolved, Some(bundled_runtime));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn resolver_skips_broken_packaged_runtime_before_dev_path() {
        let root = test_runtime_root("skips-broken-packaged");
        let managed_root = root.join("managed");
        let broken_runtime = managed_root
            .join("bin")
            .join(faster_whisper_runner_names()[0]);
        let dev_runtime = root.join("dev").join("scribe-faster-whisper-dev");
        fs::create_dir_all(broken_runtime.parent().unwrap()).unwrap();
        fs::write(&broken_runtime, b"broken faster-whisper runtime").unwrap();
        write_test_runtime(&dev_runtime);

        let resolved = resolve_faster_whisper_executable_from_candidates(
            [],
            [managed_root],
            [dev_runtime.clone()],
        );

        assert_eq!(resolved, Some(dev_runtime));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn faster_whisper_environment_includes_cuda_library_paths() {
        let root = test_runtime_root("cuda-library-paths");
        let runtime_root = root.join("runtimes").join("faster_whisper");
        let bin_dir = runtime_root.join("bin");
        let cuda_dir = runtime_root.join("cuda_v12");
        let executable = bin_dir.join(faster_whisper_runner_names()[0]);
        write_test_runtime(&executable);
        fs::create_dir_all(&cuda_dir).unwrap();

        let configured_path = PathBuf::from("/opt/scribe-cuda");
        let mut command = Command::new("scribe-faster-whisper");
        apply_faster_whisper_environment(
            &mut command,
            &executable,
            &FasterWhisperOptions {
                compute_mode: WhisperComputeMode::PreferGpu,
                gpu_device: 0,
                cuda_library_paths: vec![configured_path.clone()],
            },
        )
        .unwrap();

        let library_path = command_env(&command, "LD_LIBRARY_PATH").unwrap();
        assert!(env::split_paths(&library_path).any(|path| path == cuda_dir));
        assert!(env::split_paths(&library_path).any(|path| path == configured_path));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn faster_whisper_cpu_environment_skips_cuda_library_paths() {
        let root = test_runtime_root("cpu-skips-cuda-library-paths");
        let runtime_root = root.join("runtimes").join("faster_whisper");
        let bin_dir = runtime_root.join("bin");
        let cuda_dir = runtime_root.join("cuda_v12");
        let executable = bin_dir.join(faster_whisper_runner_names()[0]);
        write_test_runtime(&executable);
        fs::create_dir_all(&cuda_dir).unwrap();

        let mut command = Command::new("scribe-faster-whisper");
        apply_faster_whisper_environment(
            &mut command,
            &executable,
            &FasterWhisperOptions {
                compute_mode: WhisperComputeMode::Cpu,
                gpu_device: 0,
                cuda_library_paths: vec![PathBuf::from("/opt/scribe-cuda")],
            },
        )
        .unwrap();

        assert!(command_env(&command, "LD_LIBRARY_PATH").is_none());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[ignore = "requires a local faster-whisper runner, downloaded model directory, and sample audio"]
    fn faster_whisper_smoke_uses_configured_runner() {
        let runner = env::var_os("SCRIBE_FASTER_WHISPER_CLI")
            .map(PathBuf::from)
            .expect("SCRIBE_FASTER_WHISPER_CLI points to a faster-whisper runner");
        let model_path = env::var_os("SCRIBE_FASTER_WHISPER_MODEL")
            .map(PathBuf::from)
            .expect("SCRIBE_FASTER_WHISPER_MODEL points to a downloaded faster-whisper model");
        let audio_path = env::var_os("SCRIBE_FASTER_WHISPER_AUDIO")
            .map(PathBuf::from)
            .expect("SCRIBE_FASTER_WHISPER_AUDIO points to a WAV file");

        let mut model = default_model_catalog()
            .into_iter()
            .find(|model| model.id == "faster_whisper_tiny_en")
            .expect("faster-whisper tiny model exists in catalog");
        model.local_path = Some(model_path);

        let backend = FasterWhisperBackend::new(
            Some(runner),
            FasterWhisperOptions {
                compute_mode: WhisperComputeMode::Cpu,
                gpu_device: 0,
                ..FasterWhisperOptions::default()
            },
        );
        let result = backend.transcribe(audio_path, model).unwrap();

        assert_eq!(result.backend, "faster-whisper");
        assert!(result.text.to_ascii_lowercase().contains("country"));
    }

    fn test_runtime_root(name: &str) -> PathBuf {
        env::temp_dir().join(format!(
            "scribe-faster-whisper-runtime-{name}-{}",
            std::process::id()
        ))
    }

    #[test]
    fn portable_standalone_runtime_does_not_require_a_venv() {
        let root = test_runtime_root("portable-standalone");
        let executable = root.join("bin").join(faster_whisper_runner_names()[0]);
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        fs::write(&executable, b"standalone").unwrap();
        fs::write(
            root.join("runtime-manifest.json"),
            serde_json::json!({
                "manifest_version": 1,
                "runtime_id": "faster_whisper",
                "version": "1.2.1",
                "platform": format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
                "device": "cpu",
                "entrypoint": format!("bin/{}", faster_whisper_runner_names()[0]),
                "portable": true
            })
            .to_string(),
        )
        .unwrap();

        assert!(is_faster_whisper_runtime_usable(&executable));
        assert!(!root.join(venv_python_relative_path()).exists());
        let _ = fs::remove_dir_all(root);
    }

    fn write_test_runtime(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"faster-whisper runtime").unwrap();
        if let Some(runtime_root) =
            packaged_runtime_root(path).filter(|_| is_packaged_runner_path(path))
        {
            let runner = runtime_root.join("bin").join("faster_whisper_runner.py");
            let python = runtime_root.join(venv_python_relative_path());
            fs::create_dir_all(runner.parent().unwrap()).unwrap();
            fs::create_dir_all(python.parent().unwrap()).unwrap();
            fs::write(runner, b"runner").unwrap();
            fs::write(python, b"python").unwrap();
        }
    }

    fn command_env(command: &Command, key: &str) -> Option<OsString> {
        command.get_envs().find_map(|(name, value)| {
            if name == key {
                value.map(|value| value.to_os_string())
            } else {
                None
            }
        })
    }
}
