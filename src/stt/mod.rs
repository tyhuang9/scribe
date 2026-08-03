use std::cell::Cell;
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

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

static CANCELLATION_GENERATION: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static REQUEST_CANCELLATION: Cell<Option<CancellationSnapshot>> = const { Cell::new(None) };
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CancellationSnapshot(u64);

struct ThreadCancellationScope(Option<CancellationSnapshot>);

impl Drop for ThreadCancellationScope {
    fn drop(&mut self) {
        REQUEST_CANCELLATION.set(self.0);
    }
}

fn install_cancellation_snapshot(snapshot: CancellationSnapshot) -> ThreadCancellationScope {
    ThreadCancellationScope(REQUEST_CANCELLATION.replace(Some(snapshot)))
}

pub(crate) fn current_cancellation_snapshot() -> CancellationSnapshot {
    REQUEST_CANCELLATION
        .get()
        .unwrap_or_else(cancellation_snapshot)
}

#[derive(Default)]
struct ActiveProcessState {
    requests: usize,
    processes: HashMap<u32, usize>,
}

pub(crate) struct RegisteredRequest;

impl Drop for RegisteredRequest {
    fn drop(&mut self) {
        let (state, changed) = active_legacy_state();
        let mut state = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.requests = state.requests.saturating_sub(1);
        changed.notify_all();
    }
}

struct RegisteredProcess {
    pid: u32,
}

impl Drop for RegisteredProcess {
    fn drop(&mut self) {
        let (state, changed) = active_legacy_state();
        let mut state = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(container) = state.processes.remove(&self.pid) {
            close_process_container(container);
        }
        changed.notify_all();
    }
}

fn active_legacy_state() -> &'static (Mutex<ActiveProcessState>, Condvar) {
    static STATE: OnceLock<(Mutex<ActiveProcessState>, Condvar)> = OnceLock::new();
    STATE.get_or_init(|| (Mutex::new(ActiveProcessState::default()), Condvar::new()))
}

#[cfg(test)]
pub(crate) fn cancellation_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub(crate) fn cancellation_snapshot() -> CancellationSnapshot {
    CancellationSnapshot(CANCELLATION_GENERATION.load(Ordering::Acquire))
}

fn cancellation_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::Interrupted,
        "transcription request was cancelled",
    )
}

fn is_cancelled(snapshot: CancellationSnapshot) -> bool {
    CANCELLATION_GENERATION.load(Ordering::Acquire) != snapshot.0
}

pub(crate) fn register_cancellable_request(
    snapshot: CancellationSnapshot,
) -> io::Result<RegisteredRequest> {
    let (state, _) = active_legacy_state();
    let mut state = state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if is_cancelled(snapshot) {
        return Err(cancellation_error());
    }
    state.requests = state
        .requests
        .checked_add(1)
        .ok_or_else(|| io::Error::other("active transcription request count overflow"))?;
    Ok(RegisteredRequest)
}

pub(crate) fn run_cancellable_command(
    command: &mut Command,
    snapshot: CancellationSnapshot,
) -> io::Result<Output> {
    if is_cancelled(snapshot) {
        return Err(cancellation_error());
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let container = match create_process_container(&child) {
        Ok(container) => container,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    };
    let (state, _) = active_legacy_state();
    let mut state = state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if is_cancelled(snapshot) {
        drop(state);
        terminate_process_container(container);
        let _ = child.wait();
        close_process_container(container);
        return Err(cancellation_error());
    }
    let pid = child.id();
    state.processes.insert(pid, container);
    drop(state);
    let registration = RegisteredProcess { pid };
    let output = child.wait_with_output();
    drop(registration);
    output
}

pub(crate) fn cancel_active_processes() {
    CANCELLATION_GENERATION.fetch_add(1, Ordering::AcqRel);
    let (state, _) = active_legacy_state();
    let state = state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for process in state.processes.values().copied() {
        terminate_process_container(process);
    }
}

pub(crate) fn cancel_active_processes_and_wait(timeout: Duration) -> bool {
    cancel_active_processes();
    let deadline = Instant::now() + timeout;
    let (state, changed) = active_legacy_state();
    let mut state = state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    while state.requests != 0 || !state.processes.is_empty() {
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            return false;
        };
        let (next, wait) = changed
            .wait_timeout(state, remaining)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state = next;
        if wait.timed_out() && (state.requests != 0 || !state.processes.is_empty()) {
            return false;
        }
    }
    true
}

#[cfg(windows)]
fn create_process_container(child: &Child) -> io::Result<usize> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return Err(io::Error::last_os_error());
        }
        let mut limits = std::mem::zeroed::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            std::ptr::addr_of!(limits).cast(),
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        ) == 0
            || AssignProcessToJobObject(job, child.as_raw_handle()) == 0
        {
            let error = io::Error::last_os_error();
            CloseHandle(job);
            return Err(error);
        }
        Ok(job as usize)
    }
}

#[cfg(unix)]
fn create_process_container(child: &Child) -> io::Result<usize> {
    Ok(child.id() as usize)
}

#[cfg(windows)]
fn terminate_process_container(job: usize) {
    use windows_sys::Win32::System::JobObjects::TerminateJobObject;
    unsafe {
        TerminateJobObject(job as *mut std::ffi::c_void, 1);
    }
}

#[cfg(unix)]
fn terminate_process_container(pid: usize) {
    unsafe {
        libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
    }
}

#[cfg(windows)]
fn close_process_container(job: usize) {
    use windows_sys::Win32::Foundation::CloseHandle;
    unsafe {
        CloseHandle(job as *mut std::ffi::c_void);
    }
}

#[cfg(unix)]
fn close_process_container(_pid: usize) {}

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
    cancellation: CancellationSnapshot,
) -> Result<TranscriptResult> {
    let _scope = install_cancellation_snapshot(cancellation);
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
    fn cancellation_terminates_a_registered_process_tree() {
        let _test_lock = cancellation_test_lock();
        let (tx, rx) = crossbeam_channel::bounded(1);
        let snapshot = cancellation_snapshot();
        std::thread::spawn(move || {
            #[cfg(windows)]
            let mut command = {
                let mut command = Command::new("powershell.exe");
                command.args([
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    "Start-Sleep -Milliseconds 300; $child = Start-Process -FilePath powershell.exe -ArgumentList @('-NoLogo','-NoProfile','-NonInteractive','-Command','Start-Sleep -Seconds 30') -NoNewWindow -PassThru; Wait-Process -Id $child.Id",
                ]);
                command
            };
            #[cfg(unix)]
            let mut command = {
                let mut command = Command::new("sh");
                command.args(["-c", "sleep 0.3; sleep 30 & wait"]);
                command
            };
            let _ = tx.send(run_cancellable_command(&mut command, snapshot));
        });

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while active_legacy_state()
            .0
            .lock()
            .map(|state| state.processes.is_empty())
            .unwrap_or(true)
            && std::time::Instant::now() < deadline
        {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            active_legacy_state()
                .0
                .lock()
                .is_ok_and(|state| !state.processes.is_empty()),
            "legacy process was not registered"
        );
        std::thread::sleep(Duration::from_millis(700));

        assert!(cancel_active_processes_and_wait(Duration::from_secs(3)));

        let output = rx
            .recv_timeout(std::time::Duration::from_secs(3))
            .expect("cancelled process acknowledged termination")
            .expect("legacy process launched");
        assert!(!output.status.success());
    }

    #[test]
    fn cancellation_before_registration_rejects_the_stale_request() {
        let _test_lock = cancellation_test_lock();
        let snapshot = cancellation_snapshot();
        cancel_active_processes();

        let registration_error = register_cancellable_request(snapshot)
            .err()
            .expect("stale request registration must fail");
        assert_eq!(registration_error.kind(), io::ErrorKind::Interrupted);

        #[cfg(windows)]
        let mut command = {
            let mut command = Command::new("cmd.exe");
            command.args(["/C", "exit", "0"]);
            command
        };
        #[cfg(unix)]
        let mut command = Command::new("true");
        let spawn_error = run_cancellable_command(&mut command, snapshot).unwrap_err();
        assert_eq!(spawn_error.kind(), io::ErrorKind::Interrupted);
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
