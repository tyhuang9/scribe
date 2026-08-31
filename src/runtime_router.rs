//! Static native runtime boundary for GGUF inference.
//!
//! Production inference reaches this module only inside the private
//! same-executable inference child. GGUF models are owned by the statically
//! linked `transcribe-cpp` adapter; receipt-backed ONNX bundles are owned by
//! native Sherpa in `onnx_worker`.
//! worker-only native runtime

use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
#[cfg(test)]
use std::time::Duration;
use std::time::Instant;

use sha2::{Digest, Sha256};
use transcribe_cpp::CancelToken;

use crate::embedded_runtime::EmbeddedRuntime;
use crate::model_catalog::{
    ArtifactFormat, RuntimeRequirement, RuntimeVersion, runtime_model_manifest,
};
use crate::prepared_audio::{PREPARED_SAMPLE_RATE, PreparedAudio};
use crate::runtime_artifact::{RuntimeArtifact, RuntimeModel};
use crate::runtime_contract::TRANSCRIBE_CPP_VERSION;
pub(crate) use crate::runtime_contract::{
    NativeRuntimeDiagnostics, RuntimeError, RuntimeExecution, RuntimeLoadExecution,
};
use crate::transcription::{
    AccelerationPreference, ModelId, RuntimeCapabilities, SpeechEngine, TranscriptionOptions,
};

/// Deliberately private: concrete runtime selection never crosses the router.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RuntimeKind {
    TranscribeCpp,
}

const TRANSCRIBE_CPP_RUNTIME_VERSION: RuntimeVersion = RuntimeVersion {
    major: 1,
    minor: 9,
    patch: 1,
};

fn runtime_kind_for_model(model_id: &ModelId) -> Option<RuntimeKind> {
    let manifest = runtime_model_manifest(model_id)?;
    match manifest.runtime {
        RuntimeRequirement::PrimaryNative
            if manifest.artifact_filename.ends_with(".gguf")
                && TRANSCRIBE_CPP_RUNTIME_VERSION >= manifest.minimum_runtime_version =>
        {
            Some(RuntimeKind::TranscribeCpp)
        }
        RuntimeRequirement::PrimaryNative => None,
    }
}

fn runtime_kind_for_artifact(artifact: &RuntimeArtifact) -> Option<RuntimeKind> {
    match artifact {
        RuntimeArtifact::Gguf(model) if model.format == ArtifactFormat::Gguf => {
            Some(RuntimeKind::TranscribeCpp)
        }
        RuntimeArtifact::Gguf(_) | RuntimeArtifact::OnnxBundle(_) => None,
    }
}

fn embedded_runtime_location() -> PathBuf {
    PathBuf::from(format!(
        "<statically linked transcribe-cpp {TRANSCRIBE_CPP_VERSION}>"
    ))
}

#[cfg(test)]
#[derive(Clone)]
pub(crate) struct RuntimeActivity {
    inner: Arc<Mutex<RuntimeActivityState>>,
}

#[cfg(test)]
struct RuntimeActivityState {
    active_requests: usize,
    generation: u64,
    idle_since: Instant,
}

#[cfg(test)]
impl Default for RuntimeActivity {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(RuntimeActivityState {
                active_requests: 0,
                generation: 0,
                idle_since: Instant::now(),
            })),
        }
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IdleTimeoutAction {
    Unload,
    Defer(Duration),
}

#[cfg(test)]
impl RuntimeActivity {
    pub(crate) fn acquire_request(&self) -> Result<RuntimeActivityLease, RuntimeError> {
        let mut state = self.inner.lock().map_err(|_| RuntimeError::Poisoned)?;
        state.active_requests = state.active_requests.saturating_add(1);
        Ok(RuntimeActivityLease {
            activity: self.clone(),
            generation: state.generation,
            refresh_idle_on_release: false,
            released: false,
        })
    }

    pub(crate) fn mark_command_complete(&self) {
        self.mark_command_complete_at(Instant::now());
    }

    fn mark_command_complete_at(&self, now: Instant) {
        if let Ok(mut state) = self.inner.lock()
            && state.active_requests == 0
        {
            state.idle_since = now;
        }
    }

    pub(crate) fn timeout_action(&self, ttl: Duration) -> IdleTimeoutAction {
        self.timeout_action_at(Instant::now(), ttl)
    }

    fn timeout_action_at(&self, now: Instant, ttl: Duration) -> IdleTimeoutAction {
        let Ok(state) = self.inner.lock() else {
            return IdleTimeoutAction::Defer(ttl);
        };
        if state.active_requests != 0 {
            return IdleTimeoutAction::Defer(ttl);
        }
        let idle_for = now.saturating_duration_since(state.idle_since);
        if idle_for >= ttl {
            IdleTimeoutAction::Unload
        } else {
            IdleTimeoutAction::Defer(ttl - idle_for)
        }
    }

    fn force_release_requests(&self) {
        if let Ok(mut state) = self.inner.lock() {
            state.active_requests = 0;
            state.generation = state.generation.wrapping_add(1);
            state.idle_since = Instant::now();
        }
    }
}

#[cfg(test)]
pub(crate) struct RuntimeActivityLease {
    activity: RuntimeActivity,
    generation: u64,
    refresh_idle_on_release: bool,
    released: bool,
}

#[cfg(test)]
impl RuntimeActivityLease {
    pub(crate) fn complete_successfully(&mut self) {
        self.refresh_idle_on_release = true;
        self.release_at(Instant::now());
    }

    fn release_at(&mut self, now: Instant) {
        if self.released {
            return;
        }
        if let Ok(mut state) = self.activity.inner.lock()
            && state.generation == self.generation
        {
            state.active_requests = state.active_requests.saturating_sub(1);
            if self.refresh_idle_on_release && state.active_requests == 0 {
                state.idle_since = now;
            }
        }
        self.released = true;
    }
}

#[cfg(test)]
impl Drop for RuntimeActivityLease {
    fn drop(&mut self) {
        self.release_at(Instant::now());
    }
}

#[derive(Clone)]
pub(crate) struct RuntimeRouter {
    inner: Arc<Mutex<RouterState>>,
    cancel_generation: Arc<AtomicU64>,
    embedded_cancellation: Arc<Mutex<Option<CancelToken>>>,
    #[cfg(test)]
    runtime_activity: RuntimeActivity,
}

struct EmbeddedCancellationContext {
    token: Arc<Mutex<Option<CancelToken>>>,
    generation: Arc<AtomicU64>,
    snapshot: u64,
}

impl RuntimeRouter {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(RouterState::default())),
            cancel_generation: Arc::new(AtomicU64::new(0)),
            embedded_cancellation: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            runtime_activity: RuntimeActivity::default(),
        }
    }

    #[allow(
        dead_code,
        reason = "the test-compiled full router retains its production adapter query surface"
    )]
    pub(crate) fn handles_model_id(model_id: &ModelId) -> bool {
        runtime_kind_for_model(model_id).is_some()
    }

    #[allow(
        dead_code,
        reason = "the test-compiled full router retains its production capability query surface"
    )]
    pub(crate) fn capabilities_for_model(model_id: &ModelId) -> Option<RuntimeCapabilities> {
        runtime_kind_for_model(model_id)
            .map(|RuntimeKind::TranscribeCpp| static_runtime_capabilities())
    }

    #[allow(
        dead_code,
        reason = "the test-compiled full router retains its embedded-runtime capability query"
    )]
    pub(crate) fn embedded_runtime_capabilities() -> RuntimeCapabilities {
        static_runtime_capabilities()
    }

    pub(crate) fn transcribe(
        &self,
        artifact: RuntimeArtifact,
        preference: AccelerationPreference,
        audio: &PreparedAudio,
        options: &TranscriptionOptions,
        cancellation_snapshot: u64,
    ) -> Result<RuntimeExecution, RuntimeError> {
        if audio.sample_rate != PREPARED_SAMPLE_RATE
            || audio.samples.is_empty()
            || audio
                .samples
                .iter()
                .any(|sample| !sample.is_finite() || !(-1.0..=1.0).contains(sample))
        {
            return Err(RuntimeError::InvalidAudio {
                sample_rate_hz: audio.sample_rate,
                channels: 1,
            });
        }
        let model_id = artifact.model_id();
        let kind = runtime_kind_for_artifact(&artifact)
            .ok_or_else(|| RuntimeError::UnsupportedModel(model_id.clone()))?;
        let RuntimeArtifact::Gguf(model) = artifact else {
            return Err(RuntimeError::UnsupportedModel(model_id));
        };
        match kind {
            RuntimeKind::TranscribeCpp => self
                .inner
                .lock()
                .map_err(|_| RuntimeError::Poisoned)?
                .transcribe_embedded(
                    model,
                    preference,
                    audio,
                    options,
                    EmbeddedCancellationContext {
                        token: Arc::clone(&self.embedded_cancellation),
                        generation: Arc::clone(&self.cancel_generation),
                        snapshot: cancellation_snapshot,
                    },
                ),
        }
    }

    pub(crate) fn load(
        &self,
        artifact: RuntimeArtifact,
        preference: AccelerationPreference,
    ) -> Result<RuntimeLoadExecution, RuntimeError> {
        let model_id = artifact.model_id();
        let kind = runtime_kind_for_artifact(&artifact)
            .ok_or_else(|| RuntimeError::UnsupportedModel(model_id.clone()))?;
        let RuntimeArtifact::Gguf(model) = artifact else {
            return Err(RuntimeError::UnsupportedModel(model_id));
        };
        match kind {
            RuntimeKind::TranscribeCpp => self
                .inner
                .lock()
                .map_err(|_| RuntimeError::Poisoned)?
                .load_embedded(model, preference, Arc::clone(&self.embedded_cancellation)),
        }
    }

    #[cfg(test)]
    pub(crate) fn health_check(
        &self,
        artifact: RuntimeArtifact,
        preference: AccelerationPreference,
    ) -> Result<(), RuntimeError> {
        self.load(artifact, preference).map(|_| ())
    }

    pub(crate) fn cancel_active(&self) {
        self.cancel_generation.fetch_add(1, Ordering::AcqRel);
        if let Ok(active) = self.embedded_cancellation.lock()
            && let Some(token) = active.as_ref()
        {
            token.cancel();
        }
    }

    pub(crate) fn cancellation_snapshot(&self) -> u64 {
        self.cancel_generation.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn runtime_activity(&self) -> RuntimeActivity {
        self.runtime_activity.clone()
    }

    pub(crate) fn unload_all(&self) -> Result<(), RuntimeError> {
        let mut state = self.inner.lock().map_err(|_| RuntimeError::Poisoned)?;
        let result = if let Some(runtime) = state.embedded.as_mut() {
            SpeechEngine::unload(runtime)
                .map_err(|error| RuntimeError::Engine(format!("{error:#}")))
        } else {
            Ok(())
        };
        state.discard_embedded_runtime(&self.embedded_cancellation);
        #[cfg(test)]
        self.runtime_activity.force_release_requests();
        result
    }
}

impl std::fmt::Debug for RuntimeRouter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimeRouter")
            .finish_non_exhaustive()
    }
}

#[derive(Default)]
struct RouterState {
    embedded: Option<EmbeddedRuntime>,
    embedded_model: Option<RuntimeModel>,
}

fn embedded_request_is_warm(
    current_model: Option<&RuntimeModel>,
    current_runtime: Option<(&Path, AccelerationPreference, bool)>,
    requested_model: &RuntimeModel,
    requested_preference: AccelerationPreference,
) -> bool {
    current_model == Some(requested_model)
        && current_runtime.is_some_and(|(path, preference, loaded)| {
            path == requested_model.path && preference == requested_preference && loaded
        })
}

impl RouterState {
    fn discard_embedded_runtime(&mut self, cancellation: &Arc<Mutex<Option<CancelToken>>>) {
        self.embedded = None;
        self.embedded_model = None;
        match cancellation.lock() {
            Ok(mut active) => *active = None,
            Err(poisoned) => *poisoned.into_inner() = None,
        }
    }

    fn embedded_is_warm(&self, model: &RuntimeModel, preference: AccelerationPreference) -> bool {
        embedded_request_is_warm(
            self.embedded_model.as_ref(),
            self.embedded.as_ref().map(|runtime| {
                (
                    runtime.model_path(),
                    runtime.preference(),
                    runtime.is_loaded(),
                )
            }),
            model,
            preference,
        )
    }

    fn embedded_runtime(
        &mut self,
        model: &RuntimeModel,
        preference: AccelerationPreference,
        cancellation: Arc<Mutex<Option<CancelToken>>>,
    ) -> Result<&mut EmbeddedRuntime, RuntimeError> {
        let reusable = self.embedded_model.as_ref() == Some(model)
            && self.embedded.as_ref().is_some_and(|runtime| {
                runtime.model_path() == model.path && runtime.preference() == preference
            });
        if !reusable {
            if let Some(runtime) = self.embedded.as_mut() {
                SpeechEngine::unload(runtime)
                    .map_err(|error| RuntimeError::Engine(format!("{error:#}")))?;
            }
            self.discard_embedded_runtime(&cancellation);
            self.embedded = Some(EmbeddedRuntime::new(model.path.clone(), preference));
            self.embedded_model = Some(model.clone());
            let token = self
                .embedded
                .as_ref()
                .expect("the embedded runtime was initialized")
                .cancellation_handle();
            match cancellation.lock() {
                Ok(mut active) => *active = Some(token),
                Err(poisoned) => {
                    *poisoned.into_inner() = None;
                    self.discard_embedded_runtime(&cancellation);
                    return Err(RuntimeError::Poisoned);
                }
            }
        }
        Ok(self
            .embedded
            .as_mut()
            .expect("the embedded runtime was initialized"))
    }

    fn load_embedded(
        &mut self,
        model: RuntimeModel,
        preference: AccelerationPreference,
        cancellation: Arc<Mutex<Option<CancelToken>>>,
    ) -> Result<RuntimeLoadExecution, RuntimeError> {
        let warm_reused = self.embedded_is_warm(&model, preference);
        verify_embedded_runtime_model(&model, warm_reused)?;
        let load_started = Instant::now();
        let loaded = {
            let runtime = self.embedded_runtime(&model, preference, Arc::clone(&cancellation))?;
            SpeechEngine::load(runtime).map(|()| {
                (
                    runtime
                        .resolved_acceleration()
                        .cloned()
                        .expect("a successfully loaded embedded runtime resolves acceleration"),
                    runtime
                        .detected_architecture()
                        .expect("a successfully loaded embedded runtime reports its architecture"),
                    SpeechEngine::capabilities(runtime),
                )
            })
        };
        let (resolved_acceleration, detected_architecture, capabilities) = match loaded {
            Ok(loaded) => loaded,
            Err(error) => {
                self.discard_embedded_runtime(&cancellation);
                return Err(RuntimeError::Engine(format!("{error:#}")));
            }
        };
        Ok(RuntimeLoadExecution {
            diagnostics: NativeRuntimeDiagnostics {
                resolved_acceleration,
                runtime_location: embedded_runtime_location(),
                warm_reused,
                model_load_duration_ms: if warm_reused {
                    0
                } else {
                    load_started.elapsed().as_millis()
                },
            },
            detected_architecture,
            capabilities,
        })
    }

    fn transcribe_embedded(
        &mut self,
        model: RuntimeModel,
        preference: AccelerationPreference,
        audio: &PreparedAudio,
        options: &TranscriptionOptions,
        cancellation: EmbeddedCancellationContext,
    ) -> Result<RuntimeExecution, RuntimeError> {
        let warm_reused = self.embedded_is_warm(&model, preference);
        let cancellation_token = Arc::clone(&cancellation.token);
        verify_embedded_runtime_model(&model, warm_reused)?;
        let load_started = Instant::now();
        let load_result = {
            let runtime = self.embedded_runtime(&model, preference, cancellation.token)?;
            SpeechEngine::load(runtime)
        };
        if let Err(error) = load_result {
            self.discard_embedded_runtime(&cancellation_token);
            return Err(RuntimeError::Engine(format!("{error:#}")));
        }
        let model_load_duration_ms = if warm_reused {
            0
        } else {
            load_started.elapsed().as_millis()
        };
        let (result, resolved_acceleration, processing_duration_ms) = {
            let runtime = self
                .embedded
                .as_mut()
                .expect("successful embedded load retains its runtime");
            let processing_started = Instant::now();
            let result = runtime.transcribe_with_cancellation(
                audio,
                options,
                &cancellation.generation,
                cancellation.snapshot,
            );
            (
                result,
                runtime.resolved_acceleration().cloned(),
                processing_started.elapsed().as_millis(),
            )
        };
        let transcript = match result {
            Ok(transcript) => transcript,
            Err(error) => {
                self.discard_embedded_runtime(&cancellation_token);
                return Err(RuntimeError::Engine(format!("{error:#}")));
            }
        };
        Ok(RuntimeExecution {
            transcript,
            diagnostics: NativeRuntimeDiagnostics {
                resolved_acceleration: resolved_acceleration
                    .expect("a successful embedded decode resolves acceleration"),
                runtime_location: embedded_runtime_location(),
                warm_reused,
                model_load_duration_ms,
            },
            processing_duration_ms,
        })
    }
}

fn verify_embedded_runtime_model(
    model: &RuntimeModel,
    warm_reused: bool,
) -> Result<(), RuntimeError> {
    if model.format != ArtifactFormat::Gguf {
        return Err(RuntimeError::UnsupportedModel(model.id.clone()));
    }
    if warm_reused {
        return Ok(());
    }
    let metadata =
        std::fs::metadata(&model.path).map_err(|error| RuntimeError::ArtifactIntegrity {
            path: model.path.clone(),
            message: error.to_string(),
        })?;
    if !metadata.is_file() || metadata.len() != model.expected_size_bytes {
        return Err(RuntimeError::ArtifactIntegrity {
            path: model.path.clone(),
            message: format!(
                "expected {} regular-file bytes, got {}",
                model.expected_size_bytes,
                metadata.len()
            ),
        });
    }
    let actual = sha256_file(&model.path).map_err(|error| RuntimeError::ArtifactIntegrity {
        path: model.path.clone(),
        message: error.to_string(),
    })?;
    if !actual.eq_ignore_ascii_case(&model.expected_sha256) {
        return Err(RuntimeError::ArtifactIntegrity {
            path: model.path.clone(),
            message: format!("expected {}, got {actual}", model.expected_sha256),
        });
    }
    Ok(())
}

fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

#[allow(
    dead_code,
    reason = "the test-compiled full router retains the production static capability builder"
)]
fn static_runtime_capabilities() -> RuntimeCapabilities {
    RuntimeCapabilities {
        cancellation: true,
        timestamps: true,
        supported_languages: vec!["en".to_owned()],
        ..RuntimeCapabilities::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_selection_requires_a_current_gguf_catalog_manifest() {
        assert_eq!(
            runtime_kind_for_model(&ModelId::new("whisper_cpp_base_en")),
            Some(RuntimeKind::TranscribeCpp)
        );
        assert_eq!(runtime_kind_for_model(&ModelId::new("retired-model")), None);
    }

    #[test]
    fn idle_timeout_defers_during_active_requests() {
        let activity = RuntimeActivity::default();
        let lease = activity.acquire_request().unwrap();
        assert_eq!(
            activity.timeout_action(Duration::from_millis(1)),
            IdleTimeoutAction::Defer(Duration::from_millis(1))
        );
        drop(lease);
    }

    #[test]
    fn artifact_integrity_rejects_wrong_bytes() {
        let root = std::env::temp_dir().join(format!(
            "scribe-static-gguf-integrity-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("model.gguf");
        std::fs::write(&path, b"not the pinned artifact").unwrap();
        let model = RuntimeModel {
            id: ModelId::new("fixture"),
            path,
            format: ArtifactFormat::Gguf,
            expected_size_bytes: 23,
            expected_sha256: "0".repeat(64),
        };
        assert!(matches!(
            verify_embedded_runtime_model(&model, false),
            Err(RuntimeError::ArtifactIntegrity { .. })
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn runtime_location_identifies_the_static_adapter() {
        let location = embedded_runtime_location();
        let label = location.to_string_lossy();
        assert!(label.contains("statically linked transcribe-cpp"));
        assert!(!label.contains(".dll"));
    }
}
