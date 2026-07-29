use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;

use crate::config::{self, AppConfig, WhisperComputeMode};
use crate::live_preview::PreviewCancellation;
use crate::models::{SttModelInfo, TranscriptResult, TranscriptSegment, default_model_catalog};
use crate::runtime_artifacts::{self, RuntimeDevicePack};

use super::SttBackend;

pub const PREVIEW_TRANSCRIPTION_TIMEOUT: Duration = Duration::from_secs(30);
const PREVIEW_PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(20);

pub struct WhisperCppBackend {
    executable_path: Option<PathBuf>,
    options: WhisperCppOptions,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WhisperCppOptions {
    pub compute_mode: WhisperComputeMode,
    pub gpu_device: u32,
    pub cuda_backend_path: Option<PathBuf>,
    pub cuda_library_paths: Vec<PathBuf>,
}

impl Default for WhisperCppOptions {
    fn default() -> Self {
        Self {
            compute_mode: WhisperComputeMode::Auto,
            gpu_device: 0,
            cuda_backend_path: None,
            cuda_library_paths: Vec::new(),
        }
    }
}

impl WhisperCppBackend {
    pub fn new(executable_path: Option<PathBuf>, options: WhisperCppOptions) -> Self {
        Self {
            executable_path,
            options,
        }
    }

    pub fn transcribe_preview(
        &self,
        audio_path: PathBuf,
        model: SttModelInfo,
        cancellation: &PreviewCancellation,
    ) -> Result<TranscriptResult> {
        let (executable, model_path) = self.validate_inputs(&audio_path, &model)?;
        let started = Instant::now();
        let mut command = self.command(&executable, &model_path, &audio_path)?;
        let output =
            run_preview_command(&mut command, cancellation, PREVIEW_TRANSCRIPTION_TIMEOUT)?;
        self.result_from_output(model, output, started)
    }

    fn validate_inputs(
        &self,
        audio_path: &Path,
        model: &SttModelInfo,
    ) -> Result<(PathBuf, PathBuf)> {
        let executable = self.executable_path.clone().ok_or_else(|| {
            anyhow!(
                "whisper.cpp runtime is not installed. Install a whisper.cpp model/runtime from Models, or set SCRIBE_WHISPER_CPP_CLI for development."
            )
        })?;
        let model_path = model
            .local_path
            .clone()
            .ok_or_else(|| anyhow!("download {} before transcribing", model.name))?;

        if !executable.exists() {
            return Err(anyhow!(
                "whisper.cpp executable does not exist: {}",
                executable.display()
            ));
        }
        if !model_path.exists() {
            return Err(anyhow!(
                "model file does not exist for {}: {}",
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
        Ok((executable, model_path))
    }

    fn command(&self, executable: &Path, model_path: &Path, audio_path: &Path) -> Result<Command> {
        let mut command = Command::new(executable);
        command.args(whisper_cli_args(model_path, audio_path, &self.options));
        apply_whisper_environment(&mut command, executable, &self.options)?;
        Ok(command)
    }

    fn result_from_output(
        &self,
        model: SttModelInfo,
        output: Output,
        started: Instant,
    ) -> Result<TranscriptResult> {
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            return Err(anyhow!(
                "whisper.cpp failed with status {}\n{}",
                output.status,
                stderr.trim()
            ));
        }
        if self.options.compute_mode == WhisperComputeMode::PreferGpu
            && whisper_reported_no_gpu(&stderr)
        {
            return Err(anyhow!(
                "GPU mode was requested, but the managed whisper.cpp runtime did not find a GPU. Switch transcription device to Auto or CPU only."
            ));
        }

        let text = parse_final_text(&stdout);
        let text = if text.trim().is_empty() {
            stdout.trim().to_owned()
        } else {
            text
        };

        Ok(TranscriptResult {
            model_id: model.id,
            model_name: model.name,
            backend: "whisper.cpp".to_owned(),
            segments: vec![TranscriptSegment {
                start_ms: None,
                end_ms: None,
                text: text.clone(),
            }],
            text,
            duration_ms: Some(started.elapsed().as_millis()),
            stdout,
            stderr,
        })
    }
}

impl SttBackend for WhisperCppBackend {
    fn id(&self) -> &str {
        "whisper.cpp"
    }

    fn list_models(&self) -> Vec<SttModelInfo> {
        default_model_catalog()
            .into_iter()
            .filter(|model| model.backend == "whisper.cpp")
            .collect()
    }

    fn transcribe(&self, audio_path: PathBuf, model: SttModelInfo) -> Result<TranscriptResult> {
        let (executable, model_path) = self.validate_inputs(&audio_path, &model)?;
        let started = Instant::now();
        let mut command = self.command(&executable, &model_path, &audio_path)?;
        let output = command
            .output()
            .with_context(|| format!("failed to run {}", executable.display()))?;
        self.result_from_output(model, output, started)
    }
}

fn run_preview_command(
    command: &mut Command,
    cancellation: &PreviewCancellation,
    timeout: Duration,
) -> Result<Output> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .context("failed to start whisper.cpp preview")?;
    let stdout = child.stdout.take().expect("preview stdout should be piped");
    let stderr = child.stderr.take().expect("preview stderr should be piped");
    let stdout_reader = match thread::Builder::new()
        .name("whisper-preview-stdout".to_owned())
        .spawn(move || read_pipe(stdout))
    {
        Ok(reader) => reader,
        Err(error) => {
            terminate_and_reap(&mut child);
            return Err(error).context("failed to start whisper.cpp preview stdout reader");
        }
    };
    let stderr_reader = match thread::Builder::new()
        .name("whisper-preview-stderr".to_owned())
        .spawn(move || read_pipe(stderr))
    {
        Ok(reader) => reader,
        Err(error) => {
            terminate_and_reap(&mut child);
            let _ = stdout_reader.join();
            return Err(error).context("failed to start whisper.cpp preview stderr reader");
        }
    };
    let started = Instant::now();

    let status = loop {
        if cancellation.is_cancelled() {
            terminate_and_reap(&mut child);
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err(anyhow!("live preview was cancelled"));
        }
        if started.elapsed() >= timeout {
            terminate_and_reap(&mut child);
            let _ = stdout_reader.join();
            let _ = stderr_reader.join();
            return Err(anyhow!(
                "live preview exceeded its {} second timeout",
                timeout.as_secs_f32()
            ));
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                terminate_and_reap(&mut child);
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(error).context("failed to poll whisper.cpp preview");
            }
        }
        thread::sleep(PREVIEW_PROCESS_POLL_INTERVAL.min(timeout));
    };

    let stdout = stdout_reader
        .join()
        .map_err(|_| anyhow!("whisper.cpp preview stdout reader stopped unexpectedly"))?;
    let stderr = stderr_reader
        .join()
        .map_err(|_| anyhow!("whisper.cpp preview stderr reader stopped unexpectedly"))?;
    Ok(Output {
        status,
        stdout: stdout.context("failed to read whisper.cpp preview stdout")?,
        stderr: stderr.context("failed to read whisper.cpp preview stderr")?,
    })
}

fn read_pipe(mut pipe: impl Read) -> std::io::Result<Vec<u8>> {
    let mut output = Vec::new();
    pipe.read_to_end(&mut output)?;
    Ok(output)
}

fn terminate_and_reap(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

pub fn resolve_whisper_cpp_executable(config: &AppConfig) -> Option<PathBuf> {
    let bundled = bundled_runtime_root()
        .into_iter()
        .flat_map(|root| whisper_runtime_candidates(&root))
        .find(|path| path.exists());
    let managed_gpu = verified_managed_gpu_executable(config);
    let (cpu_dev, gpu_dev) = development_runtime_paths(config);

    let bundled_gpu = bundled
        .as_ref()
        .is_some_and(|path| runtime_manifest_gpu_capable(path));
    select_compute_runtime(
        config.whisper_compute_mode,
        bundled,
        bundled_gpu,
        managed_gpu,
        first_existing_path(cpu_dev),
        first_existing_path(gpu_dev),
    )
}

pub fn resolve_whisper_cpp_packaged_executable(config: &AppConfig) -> Option<PathBuf> {
    resolve_whisper_cpp_executable_from_candidates(
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
    [config::managed_runtime_path(config, "whisper.cpp")]
        .into_iter()
        .flatten()
        .collect()
}

fn development_runtime_paths(config: &AppConfig) -> (Vec<PathBuf>, Vec<PathBuf>) {
    if !cfg!(unix)
        || (!cfg!(debug_assertions) && env::var_os("SCRIBE_ALLOW_DEV_RUNTIME_INSTALL").is_none())
    {
        return (Vec::new(), Vec::new());
    }
    let cpu = [
        env::var_os("SCRIBE_WHISPER_CPP_CLI").map(PathBuf::from),
        config.whisper_executable_path.clone(),
    ]
    .into_iter()
    .flatten()
    .collect();
    let gpu = env::var_os("SCRIBE_WHISPER_CUDA_CLI")
        .map(PathBuf::from)
        .into_iter()
        .collect();
    (cpu, gpu)
}

fn verified_managed_gpu_executable(config: &AppConfig) -> Option<PathBuf> {
    let install = config.managed_runtimes.get("whisper_cpp")?;
    let artifact =
        runtime_artifacts::embedded_artifact("whisper_cpp", RuntimeDevicePack::Gpu).ok()??;
    if !runtime_artifacts::managed_install_matches_artifact(install, &artifact) {
        return None;
    }
    resolve_whisper_cpp_executable_from_candidates([], managed_runtime_roots(config), [])
}

fn select_compute_runtime(
    mode: WhisperComputeMode,
    bundled: Option<PathBuf>,
    bundled_gpu_capable: bool,
    managed_gpu: Option<PathBuf>,
    cpu_development: Option<PathBuf>,
    gpu_development: Option<PathBuf>,
) -> Option<PathBuf> {
    match mode {
        WhisperComputeMode::Cpu => bundled.or(cpu_development),
        WhisperComputeMode::Auto => managed_gpu.or(bundled).or(cpu_development),
        WhisperComputeMode::PreferGpu => bundled
            .filter(|_| bundled_gpu_capable)
            .or(managed_gpu)
            .or(gpu_development),
    }
}

#[derive(Deserialize)]
struct WhisperRuntimeManifest {
    #[serde(default)]
    device: Option<String>,
    #[serde(default)]
    cuda_bundled: bool,
}

fn runtime_manifest_gpu_capable(executable: &Path) -> bool {
    let Some(root) = executable
        .parent()
        .filter(|parent| parent.file_name().is_some_and(|name| name == "bin"))
        .and_then(Path::parent)
    else {
        return false;
    };
    fs::read_to_string(root.join("runtime-manifest.json"))
        .ok()
        .and_then(|contents| serde_json::from_str::<WhisperRuntimeManifest>(&contents).ok())
        .is_some_and(|manifest| manifest.cuda_bundled || manifest.device.as_deref() == Some("gpu"))
}

pub(crate) fn resolve_whisper_cpp_executable_from_candidates(
    bundled_roots: impl IntoIterator<Item = PathBuf>,
    managed_roots: impl IntoIterator<Item = PathBuf>,
    dev_paths: impl IntoIterator<Item = PathBuf>,
) -> Option<PathBuf> {
    first_existing_path(
        bundled_roots
            .into_iter()
            .flat_map(|root| whisper_runtime_candidates(&root))
            .chain(
                managed_roots
                    .into_iter()
                    .flat_map(|root| whisper_runtime_candidates(&root)),
            )
            .chain(dev_paths),
    )
}

fn whisper_runtime_candidates(root: &Path) -> Vec<PathBuf> {
    if root.as_os_str().is_empty() {
        return Vec::new();
    }
    if root.is_file() {
        return vec![root.to_path_buf()];
    }

    whisper_cli_binary_names()
        .iter()
        .flat_map(|&binary_name| {
            [
                root.join("runtimes")
                    .join("whisper.cpp")
                    .join("bin")
                    .join(binary_name),
                root.join("runtimes")
                    .join("whisper_cpp")
                    .join("bin")
                    .join(binary_name),
                root.join("bin").join(binary_name),
                root.join(binary_name),
            ]
        })
        .collect()
}

fn whisper_cli_binary_names() -> &'static [&'static str] {
    if cfg!(windows) {
        &["whisper-cli.exe", "main.exe"]
    } else {
        &["whisper-cli", "main"]
    }
}

fn first_existing_path(paths: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    let mut seen = Vec::new();
    for path in paths {
        if path.as_os_str().is_empty() || seen.iter().any(|seen_path| seen_path == &path) {
            continue;
        }
        seen.push(path.clone());
        if path.exists() {
            return Some(path);
        }
    }
    None
}

fn whisper_cli_args(
    model_path: &Path,
    audio_path: &Path,
    options: &WhisperCppOptions,
) -> Vec<OsString> {
    let mut args = vec![
        OsString::from("-m"),
        model_path.as_os_str().to_owned(),
        OsString::from("-f"),
        audio_path.as_os_str().to_owned(),
        OsString::from("-nt"),
    ];

    match options.compute_mode {
        WhisperComputeMode::Auto => {}
        WhisperComputeMode::PreferGpu => {
            args.push(OsString::from("-dev"));
            args.push(OsString::from(options.gpu_device.to_string()));
        }
        WhisperComputeMode::Cpu => {
            args.push(OsString::from("-ng"));
        }
    }

    args
}

fn apply_whisper_environment(
    command: &mut Command,
    executable_path: &Path,
    options: &WhisperCppOptions,
) -> Result<()> {
    let include_cuda_paths = options.compute_mode != WhisperComputeMode::Cpu;
    let runtime_paths = bundled_runtime_library_paths(executable_path, include_cuda_paths);
    let configured_paths = if options.compute_mode == WhisperComputeMode::Cpu {
        Vec::new()
    } else {
        options.cuda_library_paths.clone()
    };

    if options.compute_mode != WhisperComputeMode::Cpu
        && let Some(backend_path) =
            bundled_cuda_backend_path(executable_path).or_else(|| options.cuda_backend_path.clone())
        && !backend_path.as_os_str().is_empty()
    {
        command.env("GGML_BACKEND_PATH", backend_path);
    }

    if let Some(library_path) = joined_library_path(
        runtime_paths
            .into_iter()
            .chain(configured_paths)
            .collect::<Vec<_>>()
            .as_slice(),
    )? {
        command.env("LD_LIBRARY_PATH", library_path);
    }

    Ok(())
}

fn bundled_runtime_library_paths(executable_path: &Path, include_cuda_paths: bool) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(bin_dir) = executable_path.parent() {
        paths.push(bin_dir.to_path_buf());
        if let Some(runtime_root) = bin_dir.parent() {
            paths.push(runtime_root.join("lib"));
            if include_cuda_paths {
                paths.push(runtime_root.join("cuda"));
                paths.push(runtime_root.join("cuda_v13"));
                paths.push(runtime_root.join("cuda_v12"));
            }
        }
    }
    paths.into_iter().filter(|path| path.exists()).collect()
}

fn bundled_cuda_backend_path(executable_path: &Path) -> Option<PathBuf> {
    let bin_dir = executable_path.parent()?;
    let runtime_root = bin_dir.parent()?;
    first_existing_path([
        runtime_root.join("cuda").join("libggml-cuda.so"),
        runtime_root.join("cuda_v13").join("libggml-cuda.so"),
        runtime_root.join("cuda_v12").join("libggml-cuda.so"),
        bin_dir.join("libggml-cuda.so"),
    ])
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
        .with_context(|| "failed to build LD_LIBRARY_PATH for whisper.cpp")
}

fn whisper_reported_no_gpu(stderr: &str) -> bool {
    stderr
        .lines()
        .any(|line| line.to_ascii_lowercase().contains("no gpu found"))
}

pub(crate) fn parse_final_text(stdout: &str) -> String {
    stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| !line.starts_with("whisper_"))
        .map(strip_timestamp_prefix)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn strip_timestamp_prefix(line: &str) -> String {
    if let Some(end) = line.find(']')
        && line.starts_with('[')
        && line[..=end].contains("-->")
    {
        return line[end + 1..].trim().to_owned();
    }
    line.to_owned()
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::fs;
    use std::time::Duration;

    use crate::models::default_model_catalog;
    use crate::stt::SttBackend;

    use super::*;

    #[test]
    #[ignore]
    fn preview_process_helper() {
        match env::var("SCRIBE_PREVIEW_TEST_CHILD").as_deref() {
            Ok("success") => {
                println!("preview-success");
                eprintln!("preview-stderr");
            }
            Ok("sleep") => thread::sleep(Duration::from_secs(5)),
            _ => {}
        }
    }

    fn preview_test_command(mode: &str) -> Command {
        let mut command = Command::new(env::current_exe().unwrap());
        command
            .args([
                "--ignored",
                "--exact",
                "stt::whisper_cpp::tests::preview_process_helper",
                "--nocapture",
            ])
            .env("SCRIBE_PREVIEW_TEST_CHILD", mode);
        command
    }

    #[test]
    fn preview_process_completes_and_captures_both_pipes() {
        let cancellation = PreviewCancellation::new();
        let output = run_preview_command(
            &mut preview_test_command("success"),
            &cancellation,
            Duration::from_secs(2),
        )
        .unwrap();

        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("preview-success"));
        assert!(String::from_utf8_lossy(&output.stderr).contains("preview-stderr"));
    }

    #[test]
    fn preview_process_cancellation_kills_and_reaps_the_child() {
        let cancellation = PreviewCancellation::new();
        let cancellation_signal = cancellation.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(40));
            cancellation_signal.cancel();
        });
        let started = Instant::now();
        let error = run_preview_command(
            &mut preview_test_command("sleep"),
            &cancellation,
            Duration::from_secs(2),
        )
        .unwrap_err();

        assert!(error.to_string().contains("cancelled"));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn preview_process_timeout_kills_and_reaps_the_child() {
        let cancellation = PreviewCancellation::new();
        let started = Instant::now();
        let error = run_preview_command(
            &mut preview_test_command("sleep"),
            &cancellation,
            Duration::from_millis(60),
        )
        .unwrap_err();

        assert!(error.to_string().contains("timeout"));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn parse_final_text_removes_timestamps_and_diagnostics() {
        let stdout = r#"
            whisper_init_from_file_with_params_no_state: loading model
            [00:00:00.000 --> 00:00:01.000]  First sentence.
            [00:00:01.000 --> 00:00:02.000]  Second sentence.
        "#;

        assert_eq!(
            parse_final_text(stdout),
            "First sentence.\nSecond sentence."
        );
    }

    #[test]
    fn parse_final_text_keeps_plain_lines() {
        assert_eq!(parse_final_text("hello world"), "hello world");
    }

    #[test]
    fn whisper_args_select_cuda_device_when_gpu_is_enabled() {
        let args = whisper_cli_args(
            Path::new("/models/ggml-small.en.bin"),
            Path::new("/tmp/audio.wav"),
            &WhisperCppOptions {
                compute_mode: WhisperComputeMode::PreferGpu,
                gpu_device: 1,
                ..WhisperCppOptions::default()
            },
        );

        assert_eq!(
            args,
            vec![
                OsString::from("-m"),
                OsString::from("/models/ggml-small.en.bin"),
                OsString::from("-f"),
                OsString::from("/tmp/audio.wav"),
                OsString::from("-nt"),
                OsString::from("-dev"),
                OsString::from("1"),
            ]
        );
    }

    #[test]
    fn whisper_args_auto_defers_device_choice_to_runtime() {
        let args = whisper_cli_args(
            Path::new("/models/ggml-base.en.bin"),
            Path::new("/tmp/audio.wav"),
            &WhisperCppOptions {
                compute_mode: WhisperComputeMode::Auto,
                gpu_device: 0,
                ..WhisperCppOptions::default()
            },
        );

        assert_eq!(
            args,
            vec![
                OsString::from("-m"),
                OsString::from("/models/ggml-base.en.bin"),
                OsString::from("-f"),
                OsString::from("/tmp/audio.wav"),
                OsString::from("-nt"),
            ]
        );
    }

    #[test]
    fn whisper_args_can_disable_gpu() {
        let args = whisper_cli_args(
            Path::new("/models/ggml-base.en.bin"),
            Path::new("/tmp/audio.wav"),
            &WhisperCppOptions {
                compute_mode: WhisperComputeMode::Cpu,
                gpu_device: 0,
                ..WhisperCppOptions::default()
            },
        );

        assert_eq!(
            args,
            vec![
                OsString::from("-m"),
                OsString::from("/models/ggml-base.en.bin"),
                OsString::from("-f"),
                OsString::from("/tmp/audio.wav"),
                OsString::from("-nt"),
                OsString::from("-ng"),
            ]
        );
    }

    #[test]
    fn detects_gpu_fallback_message() {
        assert!(whisper_reported_no_gpu(
            "whisper_backend_init_gpu: device 0: CPU (type: 0)\nwhisper_backend_init_gpu: no GPU found"
        ));
        assert!(!whisper_reported_no_gpu(
            "whisper_backend_init_gpu: device 0: NVIDIA GeForce"
        ));
    }

    #[test]
    fn joins_cuda_library_paths() {
        let paths = vec![PathBuf::from("/opt/cuda"), PathBuf::from("/opt/cublas")];
        let joined = joined_library_path(&paths).unwrap().unwrap();

        assert!(env::split_paths(&joined).any(|path| path == *"/opt/cuda"));
        assert!(env::split_paths(&joined).any(|path| path == *"/opt/cublas"));
    }

    #[test]
    fn bundled_runtime_paths_include_staged_bin_and_cuda_dirs() {
        let root = test_runtime_root("bundled-library-paths");
        let runtime_root = root.join("runtimes").join("whisper_cpp");
        let bin_dir = runtime_root.join("bin");
        let cuda_dir = runtime_root.join("cuda");
        fs::create_dir_all(&bin_dir).unwrap();
        fs::create_dir_all(&cuda_dir).unwrap();
        let executable = bin_dir.join(whisper_cli_binary_names()[0]);
        write_test_runtime(&executable);

        let paths = bundled_runtime_library_paths(&executable, true);

        assert!(paths.contains(&bin_dir));
        assert!(paths.contains(&cuda_dir));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn whisper_cpu_environment_excludes_bundled_cuda_paths() {
        let root = test_runtime_root("cpu-excludes-cuda");
        let runtime_root = root.join("runtimes").join("whisper_cpp");
        let bin_dir = runtime_root.join("bin");
        let lib_dir = runtime_root.join("lib");
        let cuda_dir = runtime_root.join("cuda");
        let cuda_backend = cuda_dir.join("libggml-cuda.so");
        let executable = bin_dir.join(whisper_cli_binary_names()[0]);
        write_test_runtime(&executable);
        fs::create_dir_all(&lib_dir).unwrap();
        write_test_runtime(&cuda_backend);

        let mut command = Command::new("whisper-cli");
        apply_whisper_environment(
            &mut command,
            &executable,
            &WhisperCppOptions {
                compute_mode: WhisperComputeMode::Cpu,
                cuda_backend_path: Some(cuda_backend),
                cuda_library_paths: vec![cuda_dir.clone()],
                ..WhisperCppOptions::default()
            },
        )
        .unwrap();

        assert!(command_env(&command, "GGML_BACKEND_PATH").is_none());
        let library_path = command_env(&command, "LD_LIBRARY_PATH").unwrap();
        assert!(env::split_paths(&library_path).any(|path| path == bin_dir));
        assert!(env::split_paths(&library_path).any(|path| path == lib_dir));
        assert!(!env::split_paths(&library_path).any(|path| path == cuda_dir));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn whisper_environment_prefers_bundled_cuda_backend_over_configured_host_path() {
        let root = test_runtime_root("prefers-bundled-cuda");
        let runtime_root = root.join("runtimes").join("whisper_cpp");
        let bin_dir = runtime_root.join("bin");
        let cuda_backend = runtime_root.join("cuda").join("libggml-cuda.so");
        let executable = bin_dir.join(whisper_cli_binary_names()[0]);
        write_test_runtime(&executable);
        write_test_runtime(&cuda_backend);

        let mut command = Command::new("whisper-cli");
        apply_whisper_environment(
            &mut command,
            &executable,
            &WhisperCppOptions {
                compute_mode: WhisperComputeMode::PreferGpu,
                cuda_backend_path: Some(PathBuf::from(
                    "/usr/local/lib/ollama/cuda/libggml-cuda.so",
                )),
                cuda_library_paths: vec![PathBuf::from("/usr/local/lib/ollama")],
                ..WhisperCppOptions::default()
            },
        )
        .unwrap();

        assert_eq!(
            command_env(&command, "GGML_BACKEND_PATH").as_deref(),
            Some(cuda_backend.as_os_str())
        );
        let library_path = command_env(&command, "LD_LIBRARY_PATH").unwrap();
        assert!(env::split_paths(&library_path).any(|path| path == bin_dir));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn resolver_prefers_bundled_before_managed_and_dev_paths() {
        let root = test_runtime_root("prefers-bundled");
        let bundled_root = root.join("bundled");
        let managed_root = root.join("managed");
        let dev_runtime = root.join("dev").join(whisper_cli_binary_names()[0]);
        let bundled_runtime = bundled_root
            .join("runtimes")
            .join("whisper.cpp")
            .join("bin")
            .join(whisper_cli_binary_names()[0]);
        let managed_runtime = managed_root.join("bin").join(whisper_cli_binary_names()[0]);
        write_test_runtime(&bundled_runtime);
        write_test_runtime(&managed_runtime);
        write_test_runtime(&dev_runtime);

        let resolved = resolve_whisper_cpp_executable_from_candidates(
            [bundled_root],
            [managed_root],
            [dev_runtime],
        );

        assert_eq!(resolved, Some(bundled_runtime));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn resolver_uses_managed_runtime_before_dev_paths() {
        let root = test_runtime_root("managed-before-dev");
        let managed_root = root.join("managed");
        let dev_runtime = root.join("dev").join(whisper_cli_binary_names()[0]);
        let managed_runtime = managed_root.join("bin").join(whisper_cli_binary_names()[0]);
        write_test_runtime(&managed_runtime);
        write_test_runtime(&dev_runtime);

        let resolved =
            resolve_whisper_cpp_executable_from_candidates([], [managed_root], [dev_runtime]);

        assert_eq!(resolved, Some(managed_runtime));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn resolver_accepts_direct_runtime_file_paths() {
        let root = test_runtime_root("direct-file");
        let managed_runtime = root.join("managed-runtime");
        write_test_runtime(&managed_runtime);

        let resolved =
            resolve_whisper_cpp_executable_from_candidates([], [managed_runtime.clone()], []);

        assert_eq!(resolved, Some(managed_runtime));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn compute_policy_uses_gpu_only_when_selected_and_verified() {
        let bundled = PathBuf::from("bundled-cpu");
        let managed_gpu = PathBuf::from("managed-gpu");
        let cpu_dev = PathBuf::from("cpu-dev");
        let gpu_dev = PathBuf::from("gpu-dev");

        assert_eq!(
            select_compute_runtime(
                WhisperComputeMode::Cpu,
                Some(bundled.clone()),
                false,
                Some(managed_gpu.clone()),
                Some(cpu_dev.clone()),
                Some(gpu_dev.clone()),
            ),
            Some(bundled.clone())
        );
        assert_eq!(
            select_compute_runtime(
                WhisperComputeMode::Auto,
                Some(bundled.clone()),
                false,
                Some(managed_gpu.clone()),
                Some(cpu_dev.clone()),
                Some(gpu_dev.clone()),
            ),
            Some(managed_gpu.clone())
        );
        assert_eq!(
            select_compute_runtime(
                WhisperComputeMode::Auto,
                Some(bundled.clone()),
                false,
                None,
                Some(cpu_dev.clone()),
                Some(gpu_dev.clone()),
            ),
            Some(bundled.clone())
        );
        assert_eq!(
            select_compute_runtime(
                WhisperComputeMode::PreferGpu,
                Some(bundled),
                false,
                None,
                Some(cpu_dev),
                Some(gpu_dev.clone()),
            ),
            Some(gpu_dev)
        );
        assert_eq!(
            select_compute_runtime(
                WhisperComputeMode::PreferGpu,
                Some(PathBuf::from("bundled-cpu")),
                false,
                None,
                Some(PathBuf::from("cpu-dev")),
                None,
            ),
            None
        );
    }

    #[test]
    fn managed_gpu_requires_exact_trusted_artifact_metadata() {
        let artifact = runtime_artifacts::RuntimeArtifact {
            runtime_id: "whisper_cpp".to_owned(),
            version: "1.2.3".to_owned(),
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
            device: RuntimeDevicePack::Gpu,
            url: "https://github.com/scribe-runtime-tests/whisper.zip".to_owned(),
            sha256: "a".repeat(64),
            size_bytes: 1,
            unpacked_size_bytes: 1,
            entrypoint: PathBuf::from("bin/whisper-cli"),
        };
        let mut install = config::ManagedRuntimeInstall::new(PathBuf::from("whisper-cli"));
        install.source = Some(artifact.url.clone());
        install.version = Some(artifact.version.clone());
        install.sha256 = Some(artifact.sha256.clone());
        install.platform = Some(format!("{}-{}", artifact.os, artifact.arch));
        install.device = Some("gpu".to_owned());

        assert!(runtime_artifacts::managed_install_matches_artifact(
            &install, &artifact
        ));
        install.sha256 = Some("b".repeat(64));
        assert!(!runtime_artifacts::managed_install_matches_artifact(
            &install, &artifact
        ));
    }

    fn test_runtime_root(name: &str) -> PathBuf {
        let root = env::temp_dir().join(format!(
            "scribe-whisper-runtime-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn write_test_runtime(path: &Path) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b"whisper runtime").unwrap();
    }

    fn command_env(command: &Command, key: &str) -> Option<OsString> {
        command.get_envs().find_map(|(env_key, env_value)| {
            if env_key == OsStr::new(key) {
                env_value.map(OsString::from)
            } else {
                None
            }
        })
    }

    #[test]
    #[ignore = "requires a local CUDA-capable whisper.cpp executable, model, sample audio, and GPU access"]
    fn whisper_cuda_smoke_uses_configured_backend() {
        let executable = PathBuf::from(
            env::var_os("SCRIBE_WHISPER_CUDA_CLI")
                .expect("set SCRIBE_WHISPER_CUDA_CLI to whisper-cli"),
        );
        let model_path = PathBuf::from(
            env::var_os("SCRIBE_WHISPER_CUDA_MODEL")
                .expect("set SCRIBE_WHISPER_CUDA_MODEL to a ggml model"),
        );
        let audio_path = PathBuf::from(
            env::var_os("SCRIBE_WHISPER_CUDA_AUDIO")
                .expect("set SCRIBE_WHISPER_CUDA_AUDIO to a wav file"),
        );
        let cuda_backend_path = env::var_os("SCRIBE_WHISPER_CUDA_BACKEND").map(PathBuf::from);
        let cuda_library_paths = env::var_os("SCRIBE_WHISPER_CUDA_LIBRARY_PATHS")
            .map(|paths| env::split_paths(&paths).collect())
            .unwrap_or_default();

        let mut model = default_model_catalog()
            .into_iter()
            .find(|model| model.id == "whisper_cpp_small_en")
            .expect("whisper.cpp small model exists in catalog");
        model.local_path = Some(model_path);

        let backend = WhisperCppBackend::new(
            Some(executable),
            WhisperCppOptions {
                compute_mode: WhisperComputeMode::PreferGpu,
                gpu_device: 0,
                cuda_backend_path,
                cuda_library_paths,
            },
        );
        let result = backend.transcribe(audio_path, model).unwrap();

        assert!(!whisper_reported_no_gpu(&result.stderr));
        assert!(result.stderr.contains("using CUDA"));
        assert!(result.text.to_lowercase().contains("ask not"));
    }
}
