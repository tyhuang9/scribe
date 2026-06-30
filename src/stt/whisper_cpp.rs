use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use anyhow::{Context, Result, anyhow};

use crate::models::{SttModelInfo, TranscriptResult, TranscriptSegment, default_model_catalog};

use super::SttBackend;

pub struct WhisperCppBackend {
    executable_path: Option<PathBuf>,
    options: WhisperCppOptions,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WhisperCppOptions {
    pub use_gpu: bool,
    pub gpu_device: u32,
    pub cuda_backend_path: Option<PathBuf>,
    pub cuda_library_paths: Vec<PathBuf>,
}

impl Default for WhisperCppOptions {
    fn default() -> Self {
        Self {
            use_gpu: true,
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
        let executable = self
            .executable_path
            .clone()
            .ok_or_else(|| anyhow!("configure the whisper.cpp executable path first"))?;
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

        let started = Instant::now();
        let mut command = Command::new(&executable);
        command.args(whisper_cli_args(&model_path, &audio_path, &self.options));
        apply_whisper_environment(&mut command, &self.options)?;
        let output = command
            .output()
            .with_context(|| format!("failed to run {}", executable.display()))?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            return Err(anyhow!(
                "whisper.cpp failed with status {}\n{}",
                output.status,
                stderr.trim()
            ));
        }
        if self.options.use_gpu && whisper_reported_no_gpu(&stderr) {
            return Err(anyhow!(
                "CUDA GPU mode was requested, but the selected whisper.cpp executable did not find a GPU. Build whisper.cpp with GGML_CUDA=1 and select that executable, or switch compute mode to CPU only."
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

    if options.use_gpu {
        args.push(OsString::from("-dev"));
        args.push(OsString::from(options.gpu_device.to_string()));
    } else {
        args.push(OsString::from("-ng"));
    }

    args
}

fn apply_whisper_environment(command: &mut Command, options: &WhisperCppOptions) -> Result<()> {
    if !options.use_gpu {
        return Ok(());
    }

    if let Some(backend_path) = &options.cuda_backend_path {
        if !backend_path.as_os_str().is_empty() {
            command.env("GGML_BACKEND_PATH", backend_path);
        }
    }
    if let Some(library_path) = joined_library_path(&options.cuda_library_paths)? {
        command.env("LD_LIBRARY_PATH", library_path);
    }

    Ok(())
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
    if let Some(end) = line.find(']') {
        if line.starts_with('[') && line[..=end].contains("-->") {
            return line[end + 1..].trim().to_owned();
        }
    }
    line.to_owned()
}

#[cfg(test)]
mod tests {
    use crate::models::default_model_catalog;
    use crate::stt::SttBackend;

    use super::*;

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
                use_gpu: true,
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
    fn whisper_args_can_disable_gpu() {
        let args = whisper_cli_args(
            Path::new("/models/ggml-base.en.bin"),
            Path::new("/tmp/audio.wav"),
            &WhisperCppOptions {
                use_gpu: false,
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

        assert!(env::split_paths(&joined).any(|path| path == PathBuf::from("/opt/cuda")));
        assert!(env::split_paths(&joined).any(|path| path == PathBuf::from("/opt/cublas")));
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
                use_gpu: true,
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
