use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Mutex, OnceLock};

use anyhow::{Result, anyhow};

use crate::config::AppConfig;
use crate::models::{
    ModelInstallStatus, ModelRuntimeStatus, SttModelInfo, TranscriptResult, backend_capabilities,
};
use crate::runtime_catalog;

pub mod faster_whisper;
pub mod sherpa_onnx;
pub mod vosk;
pub mod whisper_cpp;

struct RegisteredProcess {
    pid: u32,
}

impl Drop for RegisteredProcess {
    fn drop(&mut self) {
        if let Ok(mut processes) = active_legacy_processes().lock() {
            processes.remove(&self.pid);
        }
    }
}

fn active_legacy_processes() -> &'static Mutex<HashMap<u32, usize>> {
    static PROCESSES: OnceLock<Mutex<HashMap<u32, usize>>> = OnceLock::new();
    PROCESSES.get_or_init(|| Mutex::new(HashMap::new()))
}

fn register_legacy_process(child: &Child) -> RegisteredProcess {
    let pid = child.id();
    #[cfg(windows)]
    let native_handle = {
        use std::os::windows::io::AsRawHandle;
        child.as_raw_handle() as usize
    };
    #[cfg(unix)]
    let native_handle = pid as usize;
    if let Ok(mut processes) = active_legacy_processes().lock() {
        processes.insert(pid, native_handle);
    }
    RegisteredProcess { pid }
}

pub(crate) fn run_cancellable_command(command: &mut Command) -> io::Result<Output> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let registration = register_legacy_process(&child);
    let output = child.wait_with_output();
    drop(registration);
    output
}

pub(crate) fn cancel_active_processes() {
    let processes = active_legacy_processes()
        .lock()
        .map(|processes| processes.values().copied().collect::<Vec<_>>())
        .unwrap_or_default();
    for process in processes {
        terminate_process(process);
    }
}

#[cfg(windows)]
fn terminate_process(native_handle: usize) {
    use windows_sys::Win32::System::Threading::TerminateProcess;
    unsafe {
        TerminateProcess(native_handle as *mut std::ffi::c_void, 1);
    }
}

#[cfg(unix)]
fn terminate_process(pid: usize) {
    unsafe {
        libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
    }
}

pub trait SttBackend: Send + Sync {
    fn id(&self) -> &str;
    fn list_models(&self) -> Vec<SttModelInfo>;
    fn transcribe(&self, audio_path: PathBuf, model: SttModelInfo) -> Result<TranscriptResult>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SttProviderAdapter {
    pub backend: &'static str,
    pub runtime_id: &'static str,
    pub model_install_supported: bool,
    pub runtime_install_supported: bool,
    pub transcription_supported: bool,
    pub device_detection_supported: bool,
}

pub fn provider_adapters() -> &'static [SttProviderAdapter] {
    static PROVIDER_ADAPTERS: OnceLock<Vec<SttProviderAdapter>> = OnceLock::new();
    PROVIDER_ADAPTERS.get_or_init(|| {
        runtime_catalog::backend_specs()
            .iter()
            .map(|spec| SttProviderAdapter {
                backend: spec.backend,
                runtime_id: spec.runtime_id,
                model_install_supported: spec.model_install_supported,
                runtime_install_supported: spec.runtime_install_supported,
                transcription_supported: spec.transcription_supported,
                device_detection_supported: spec.device_detection_supported,
            })
            .collect()
    })
}

pub fn provider_for_backend(backend: &str) -> Option<&'static SttProviderAdapter> {
    provider_adapters()
        .iter()
        .find(|provider| provider.backend == backend)
}

/// Transitional runtime-package validation kept inside the private legacy
/// bridge. New inference selection belongs exclusively to `RuntimeRouter`.
pub(crate) fn runtime_entrypoint_is_usable(runtime_id: &str, path: &Path) -> bool {
    match runtime_id {
        "whisper_cpp" => path.is_file(),
        "faster_whisper" => faster_whisper::is_faster_whisper_runtime_usable(path),
        "vosk" => vosk::is_vosk_runtime_usable(path),
        "sherpa_onnx" | "moonshine" | "parakeet" => {
            sherpa_onnx::is_sherpa_family_runtime_usable(runtime_id, path)
        }
        _ => false,
    }
}

impl SttProviderAdapter {
    pub fn runtime_status(self, config: &AppConfig) -> ModelRuntimeStatus {
        match self.backend {
            "whisper.cpp" => {
                if whisper_cpp::resolve_whisper_cpp_executable(config).is_some() {
                    ModelRuntimeStatus::Ready
                } else {
                    ModelRuntimeStatus::MissingConfiguration
                }
            }
            "faster-whisper" => {
                if faster_whisper::resolve_faster_whisper_executable(config).is_some() {
                    ModelRuntimeStatus::Ready
                } else {
                    ModelRuntimeStatus::MissingConfiguration
                }
            }
            "Vosk" => {
                if vosk::resolve_vosk_executable(config).is_some() {
                    ModelRuntimeStatus::Ready
                } else {
                    ModelRuntimeStatus::MissingConfiguration
                }
            }
            "sherpa-onnx" | "Moonshine" | "Parakeet" => {
                if sherpa_onnx::resolve_executable_for_backend(config, self.backend).is_some() {
                    ModelRuntimeStatus::Ready
                } else {
                    ModelRuntimeStatus::MissingConfiguration
                }
            }
            _ => ModelRuntimeStatus::NotImplemented,
        }
    }

    pub fn model_install_status(self, model: &SttModelInfo) -> ModelInstallStatus {
        model.install_status.clone()
    }

    pub fn can_install_model(self, model: &SttModelInfo) -> bool {
        self.model_install_supported && model.download_model.is_some()
    }

    pub fn can_uninstall_model(self, model: &SttModelInfo) -> bool {
        model.install_status == ModelInstallStatus::Installed
    }
}

pub fn transcribe_with_config(
    config: &AppConfig,
    audio_path: PathBuf,
    model: SttModelInfo,
) -> Result<TranscriptResult> {
    match model.backend.as_str() {
        "whisper.cpp" => {
            let provider = provider_for_backend("whisper.cpp")
                .ok_or_else(|| anyhow!("missing whisper.cpp provider adapter"))?;
            let backend = whisper_cpp::WhisperCppBackend::new(
                whisper_cpp::resolve_whisper_cpp_executable(config),
                whisper_cpp::WhisperCppOptions {
                    compute_mode: config.performance.acceleration_preference,
                    gpu_device: config.performance.whisper_gpu_device,
                    cuda_backend_path: config.performance.whisper_cuda_backend_path.clone(),
                    cuda_library_paths: config.performance.whisper_cuda_library_paths.clone(),
                },
            );
            let capabilities = backend_capabilities(provider.backend);
            if !capabilities.runnable {
                return Err(anyhow!(
                    "{} managed runtime is not bundled yet",
                    model.backend
                ));
            }
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
        "faster-whisper" => {
            let provider = provider_for_backend("faster-whisper")
                .ok_or_else(|| anyhow!("missing faster-whisper provider adapter"))?;
            let backend = faster_whisper::FasterWhisperBackend::new(
                faster_whisper::resolve_faster_whisper_executable(config),
                faster_whisper::FasterWhisperOptions {
                    compute_mode: config.performance.acceleration_preference,
                    gpu_device: config.performance.whisper_gpu_device,
                    cuda_library_paths: config.performance.whisper_cuda_library_paths.clone(),
                },
            );
            let capabilities = backend_capabilities(provider.backend);
            if !capabilities.runnable {
                return Err(anyhow!(
                    "{} managed runtime is not bundled yet",
                    model.backend
                ));
            }
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
        "Vosk" => {
            let provider = provider_for_backend("Vosk")
                .ok_or_else(|| anyhow!("missing Vosk provider adapter"))?;
            let backend = vosk::VoskBackend::new(vosk::resolve_vosk_executable(config));
            let capabilities = backend_capabilities(provider.backend);
            if !capabilities.runnable {
                return Err(anyhow!(
                    "{} managed runtime is not bundled yet",
                    model.backend
                ));
            }
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
        "sherpa-onnx" | "Moonshine" | "Parakeet" => {
            let provider = provider_for_backend(&model.backend)
                .ok_or_else(|| anyhow!("unsupported STT backend: {}", model.backend))?;
            let backend = sherpa_onnx::SherpaOnnxBackend::new(
                &model.backend,
                sherpa_onnx::resolve_executable_for_backend(config, &model.backend),
            );
            let capabilities = backend_capabilities(provider.backend);
            if !capabilities.runnable {
                return Err(anyhow!(
                    "{} managed runtime is not bundled yet",
                    model.backend
                ));
            }
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
        backend => Err(anyhow!("unsupported STT backend: {backend}")),
    }
}

#[cfg(test)]
mod tests {
    use crate::config;
    use crate::models::default_model_catalog;

    use super::*;

    #[test]
    fn cancellation_terminates_a_registered_legacy_process() {
        let (tx, rx) = crossbeam_channel::bounded(1);
        std::thread::spawn(move || {
            #[cfg(windows)]
            let mut command = {
                let mut command = Command::new("powershell.exe");
                command.args([
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    "Start-Sleep -Seconds 30",
                ]);
                command
            };
            #[cfg(unix)]
            let mut command = {
                let mut command = Command::new("sleep");
                command.arg("30");
                command
            };
            let _ = tx.send(run_cancellable_command(&mut command));
        });

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while active_legacy_processes()
            .lock()
            .map(|processes| processes.is_empty())
            .unwrap_or(true)
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            active_legacy_processes()
                .lock()
                .is_ok_and(|processes| !processes.is_empty()),
            "legacy process was not registered"
        );

        cancel_active_processes();

        let output = rx
            .recv_timeout(std::time::Duration::from_secs(3))
            .expect("cancelled process acknowledged termination")
            .expect("legacy process launched");
        assert!(!output.status.success());
    }

    #[test]
    fn provider_adapters_cover_catalog_backends() {
        for model in default_model_catalog() {
            let provider = provider_for_backend(&model.backend)
                .unwrap_or_else(|| panic!("missing provider for {}", model.backend));
            assert_eq!(
                provider.runtime_id,
                config::runtime_id_for_backend(&model.backend)
            );
        }
    }

    #[test]
    fn provider_model_hooks_match_current_runtime_phase() {
        let whisper = provider_for_backend("whisper.cpp").unwrap();
        let faster_whisper = provider_for_backend("faster-whisper").unwrap();
        let models = default_model_catalog();
        let mut whisper_model = models
            .iter()
            .find(|model| model.backend == "whisper.cpp")
            .unwrap()
            .clone();
        whisper_model.install_status = ModelInstallStatus::Installed;
        let faster_model = models
            .iter()
            .find(|model| model.backend == "faster-whisper")
            .unwrap();

        assert!(whisper.can_install_model(&whisper_model));
        assert!(whisper.can_uninstall_model(&whisper_model));
        assert!(whisper.transcription_supported);
        assert!(!whisper.device_detection_supported);

        assert!(faster_whisper.can_install_model(faster_model));
        assert!(!faster_whisper.can_uninstall_model(faster_model));
        assert!(faster_whisper.transcription_supported);

        let vosk = provider_for_backend("Vosk").unwrap();
        let vosk_model = models.iter().find(|model| model.backend == "Vosk").unwrap();
        assert!(vosk.can_install_model(vosk_model));
        assert!(vosk.runtime_install_supported);
        assert!(vosk.transcription_supported);

        for backend in ["sherpa-onnx", "Moonshine", "Parakeet"] {
            let provider = provider_for_backend(backend).unwrap();
            let model = models
                .iter()
                .find(|model| model.backend == backend)
                .unwrap();
            assert!(provider.can_install_model(model), "{backend} can install");
            assert!(
                provider.runtime_install_supported,
                "{backend} runtime install"
            );
            assert!(
                provider.transcription_supported,
                "{backend} transcription support"
            );
        }
    }

    #[test]
    fn provider_runtime_status_uses_managed_resolver_for_whisper_cpp() {
        let config = AppConfig::default();
        let whisper = provider_for_backend("whisper.cpp").unwrap();
        let faster_whisper = provider_for_backend("faster-whisper").unwrap();
        let vosk = provider_for_backend("Vosk").unwrap();
        let sherpa = provider_for_backend("sherpa-onnx").unwrap();
        let moonshine = provider_for_backend("Moonshine").unwrap();
        let parakeet = provider_for_backend("Parakeet").unwrap();

        assert_eq!(
            whisper.runtime_status(&config),
            ModelRuntimeStatus::MissingConfiguration
        );
        assert_eq!(
            faster_whisper.runtime_status(&config),
            ModelRuntimeStatus::MissingConfiguration
        );
        assert_eq!(
            vosk.runtime_status(&config),
            ModelRuntimeStatus::MissingConfiguration
        );
        assert_eq!(
            sherpa.runtime_status(&config),
            ModelRuntimeStatus::MissingConfiguration
        );
        assert_eq!(
            moonshine.runtime_status(&config),
            ModelRuntimeStatus::MissingConfiguration
        );
        assert_eq!(
            parakeet.runtime_status(&config),
            ModelRuntimeStatus::MissingConfiguration
        );
    }
}
