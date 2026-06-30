use std::path::PathBuf;

use anyhow::{Result, anyhow};

use crate::config::AppConfig;
use crate::models::{SttModelInfo, TranscriptResult};

pub mod whisper_cpp;

pub trait SttBackend: Send + Sync {
    fn id(&self) -> &str;
    fn list_models(&self) -> Vec<SttModelInfo>;
    fn transcribe(&self, audio_path: PathBuf, model: SttModelInfo) -> Result<TranscriptResult>;
}

pub fn transcribe_with_config(
    config: &AppConfig,
    audio_path: PathBuf,
    model: SttModelInfo,
) -> Result<TranscriptResult> {
    match model.backend.as_str() {
        "whisper.cpp" => {
            let backend = whisper_cpp::WhisperCppBackend::new(
                whisper_cpp::resolve_whisper_cpp_executable(config),
                whisper_cpp::WhisperCppOptions {
                    use_gpu: config.whisper_compute_mode.uses_gpu(),
                    gpu_device: config.whisper_gpu_device,
                    cuda_backend_path: config.whisper_cuda_backend_path.clone(),
                    cuda_library_paths: config.whisper_cuda_library_paths.clone(),
                },
            );
            let backend_id = backend.id().to_owned();
            if !backend
                .list_models()
                .iter()
                .any(|available_model| available_model.id == model.id)
            {
                return Err(anyhow!(
                    "{backend_id} does not advertise support for {}",
                    model.name
                ));
            }
            backend.transcribe(audio_path, model)
        }
        "Vosk" | "sherpa-onnx" | "faster-whisper" | "Moonshine" | "Parakeet" => Err(anyhow!(
            "{} managed runtime is not bundled yet",
            model.backend
        )),
        backend => Err(anyhow!("unsupported STT backend: {backend}")),
    }
}
