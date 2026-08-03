//! Runtime-neutral transcription contracts and the Phase 1 legacy bridge.
//!
//! Application code should depend on [`TranscriptionService`] and the types in
//! this module rather than on a concrete STT backend. The current adapters are
//! deliberately kept behind one private batch bridge until a later phase
//! replaces them with the consolidated runtime implementation.

// Phase 1 establishes the complete stable contract before native streaming,
// lifecycle wiring, and capability UI are introduced in later phases.
#![allow(dead_code)]

use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};

use crate::config::{self, AppConfig};
use crate::models::{SttModelInfo, TranscriptResult as LegacyTranscriptResult};

/// Identifies one user dictation session.
///
/// The application allocates monotonically increasing values. The service only
/// carries the value through its outcome so callers can reject stale work.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub struct SessionId(pub u64);

/// Identifies one transcription request within or across sessions.
///
/// The application allocates monotonically increasing values. The service only
/// carries the value through its outcome so callers can reject stale work.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash)]
pub struct RequestId(pub u64);

/// A runtime-neutral reference to a configured model catalog entry.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct ModelId(String);

impl ModelId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_inner(self) -> String {
        self.0
    }
}

impl AsRef<str> for ModelId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for ModelId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<String> for ModelId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl From<&str> for ModelId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

/// Normalized final transcript returned by a speech engine.
#[derive(Clone, Debug, PartialEq)]
pub struct Transcript {
    pub text: String,
    pub segments: Vec<TranscriptSegment>,
    /// `None` means the selected legacy backend did not report a language.
    pub detected_language: Option<String>,
    /// `None` means the selected runtime did not report audio-timeline
    /// duration. Phase 1 legacy adapters report only decode wall-clock time,
    /// which is retained separately on [`TranscriptionOutcome`].
    pub duration_ms: Option<u128>,
}

/// A portion of a normalized transcript.
///
/// Timing and confidence are optional because the current command-line
/// adapters do not consistently provide them for every configured backend.
#[derive(Clone, Debug, PartialEq)]
pub struct TranscriptSegment {
    pub text: String,
    pub start_ms: Option<u64>,
    pub end_ms: Option<u64>,
    pub confidence: Option<f32>,
}

/// Caller-selected decoding behavior.
///
///
/// Phase 1 represents the options needed by the future common contract, but
/// the legacy command-line route only accepts its default behavior. The
/// service rejects an unsupported non-default option instead of ignoring it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct TranscriptionOptions {
    pub language: Option<String>,
    pub translate_to_english: bool,
    pub enable_timestamps: bool,
    pub initial_prompt: Option<String>,
}

/// Features that the selected model/backend can currently expose.
///
/// `timestamps` means final results may include timestamp metadata; it does
/// not mean that the Phase 1 legacy bridge can enable timestamps on request.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RuntimeCapabilities {
    pub streaming: bool,
    pub translation: bool,
    pub timestamps: bool,
    pub language_detection: bool,
    pub confidence_scores: bool,
    pub custom_vocabulary: bool,
    /// Empty until a backend's language support is verified through this
    /// common contract rather than inferred from catalog prose.
    pub supported_languages: Vec<String>,
}

/// A streaming decoder update with stable and revisable portions separated.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StreamUpdate {
    pub committed: String,
    pub tentative: String,
}

/// Common synchronous, file-based batch speech engine contract.
///
/// The existing application records WAV files and the legacy adapters consume
/// paths, so this phase deliberately does not introduce shared audio
/// preparation or an in-memory audio requirement.
pub trait SpeechEngine: Send {
    fn load(&mut self) -> Result<()>;

    fn transcribe_file(
        &mut self,
        audio_path: &Path,
        options: &TranscriptionOptions,
    ) -> Result<Transcript>;

    fn capabilities(&self) -> RuntimeCapabilities;

    fn health_check(&mut self) -> Result<()>;

    fn cancel(&mut self) -> Result<()>;

    fn unload(&mut self) -> Result<()>;
}

/// Optional extension for engines that can decode incrementally.
pub trait StreamingSpeechEngine: SpeechEngine {
    fn start_stream(&mut self, options: &TranscriptionOptions) -> Result<Box<dyn SpeechStream>>;
}

/// A live speech-decoding session.
pub trait SpeechStream: Send {
    fn push_audio(&mut self, samples: &[f32]) -> Result<StreamUpdate>;

    fn finalize(self: Box<Self>) -> Result<Transcript>;

    fn cancel(self: Box<Self>) -> Result<()>;
}

/// A file transcription request that preserves application correlation IDs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TranscriptionRequest {
    pub session_id: SessionId,
    pub request_id: RequestId,
    pub audio_path: PathBuf,
    /// A stable catalog identifier, resolved against the service configuration.
    pub model_id: ModelId,
    /// Optional per-request override for a configured model location.
    pub model_path: Option<PathBuf>,
    pub options: TranscriptionOptions,
}

impl TranscriptionRequest {
    pub fn new(
        session_id: SessionId,
        request_id: RequestId,
        audio_path: PathBuf,
        model_id: impl Into<ModelId>,
    ) -> Self {
        Self {
            session_id,
            request_id,
            audio_path,
            model_id: model_id.into(),
            model_path: None,
            options: TranscriptionOptions::default(),
        }
    }
}

/// A normalized completed transcription with UI-facing diagnostics retained.
#[derive(Clone, Debug, PartialEq)]
pub struct TranscriptionOutcome {
    pub session_id: SessionId,
    pub request_id: RequestId,
    pub model_id: ModelId,
    pub model_name: String,
    /// A human-readable label for diagnostics and the model playground.
    pub backend_label: String,
    pub transcript: Transcript,
    /// Wall-clock processing time reported by the selected legacy adapter.
    ///
    /// This is deliberately distinct from [`Transcript::duration_ms`], which
    /// represents utterance duration on the audio timeline.
    pub processing_duration_ms: Option<u128>,
    pub stdout: String,
    pub stderr: String,
}

/// Application-facing boundary for all current file transcription work.
#[derive(Clone, Debug)]
pub struct TranscriptionService {
    config: AppConfig,
}

impl TranscriptionService {
    pub fn new(config: AppConfig) -> Self {
        Self { config }
    }

    /// Returns the conservative feature set for a configured model.
    pub fn capabilities_for(&self, model_id: &ModelId) -> Result<RuntimeCapabilities> {
        let model = self.resolve_model(model_id, None)?;
        Ok(capabilities_for_legacy_model(&model))
    }

    /// Transcribes one recorded file through the private legacy batch bridge.
    pub fn transcribe_file(&self, request: TranscriptionRequest) -> Result<TranscriptionOutcome> {
        let model = self.resolve_model(&request.model_id, request.model_path)?;
        let mut engine = LegacyBatchAdapter::new(self.config.clone(), model);
        engine.load()?;
        let transcription = engine.transcribe_file(&request.audio_path, &request.options);
        let unload_result = engine.unload();
        let transcript = transcription?;
        unload_result?;
        let diagnostics = engine.take_diagnostics().ok_or_else(|| {
            anyhow!("legacy transcription completed without diagnostics; this is a service bug")
        })?;
        validate_response_model_id(&request.model_id, &diagnostics)?;

        Ok(TranscriptionOutcome {
            session_id: request.session_id,
            request_id: request.request_id,
            model_id: diagnostics.model_id,
            model_name: diagnostics.model_name,
            backend_label: diagnostics.backend_label,
            transcript,
            processing_duration_ms: diagnostics.processing_duration_ms,
            stdout: diagnostics.stdout,
            stderr: diagnostics.stderr,
        })
    }

    fn resolve_model(
        &self,
        model_id: &ModelId,
        model_path: Option<PathBuf>,
    ) -> Result<SttModelInfo> {
        let mut model = config::configured_models(&self.config)
            .into_iter()
            .find(|model| model.id == model_id.as_str())
            .ok_or_else(|| anyhow!("unknown configured transcription model: {model_id}"))?;

        if let Some(model_path) = model_path {
            model.local_path = Some(model_path);
        }

        Ok(model)
    }
}

/// The sole Phase 1 adapter for the pre-existing command-line backend path.
///
/// It intentionally delegates to `stt::transcribe_with_config` unchanged so
/// all existing configured model paths and runtime resolution behavior remain
/// intact during extraction.
struct LegacyBatchAdapter {
    config: AppConfig,
    model: SttModelInfo,
    diagnostics: Option<LegacyDiagnostics>,
}

impl LegacyBatchAdapter {
    fn new(config: AppConfig, model: SttModelInfo) -> Self {
        Self {
            config,
            model,
            diagnostics: None,
        }
    }

    fn take_diagnostics(&mut self) -> Option<LegacyDiagnostics> {
        self.diagnostics.take()
    }
}

impl SpeechEngine for LegacyBatchAdapter {
    fn load(&mut self) -> Result<()> {
        // The legacy route starts a fresh child process for each request, so
        // there is no persistent engine to preload or validate here.
        Ok(())
    }

    fn transcribe_file(
        &mut self,
        audio_path: &Path,
        options: &TranscriptionOptions,
    ) -> Result<Transcript> {
        validate_legacy_options(options)?;

        let result = crate::stt::transcribe_with_config(
            &self.config,
            audio_path.to_path_buf(),
            self.model.clone(),
        )?;
        let (transcript, diagnostics) = map_legacy_result(result);
        self.diagnostics = Some(diagnostics);
        Ok(transcript)
    }

    fn capabilities(&self) -> RuntimeCapabilities {
        capabilities_for_legacy_model(&self.model)
    }

    fn health_check(&mut self) -> Result<()> {
        Err(anyhow!(
            "legacy command-line health checks are not implemented in Phase 1"
        ))
    }

    fn cancel(&mut self) -> Result<()> {
        Err(anyhow!(
            "cancellation is not supported by the Phase 1 legacy transcription path"
        ))
    }

    fn unload(&mut self) -> Result<()> {
        // Each legacy invocation is a child process, so there is no loaded
        // in-process engine state to release.
        Ok(())
    }
}

#[derive(Debug)]
struct LegacyDiagnostics {
    model_id: ModelId,
    model_name: String,
    backend_label: String,
    processing_duration_ms: Option<u128>,
    stdout: String,
    stderr: String,
}

fn map_legacy_result(result: LegacyTranscriptResult) -> (Transcript, LegacyDiagnostics) {
    let transcript = Transcript {
        text: result.text,
        segments: result
            .segments
            .into_iter()
            .map(|segment| TranscriptSegment {
                text: segment.text,
                start_ms: segment.start_ms,
                end_ms: segment.end_ms,
                confidence: None,
            })
            .collect(),
        detected_language: None,
        duration_ms: None,
    };
    let diagnostics = LegacyDiagnostics {
        model_id: result.model_id.into(),
        model_name: result.model_name,
        backend_label: result.backend,
        processing_duration_ms: result.duration_ms,
        stdout: result.stdout,
        stderr: result.stderr,
    };

    (transcript, diagnostics)
}

fn validate_response_model_id(
    requested_model_id: &ModelId,
    diagnostics: &LegacyDiagnostics,
) -> Result<()> {
    if diagnostics.model_id != *requested_model_id {
        return Err(anyhow!(
            "legacy transcription returned model {} for request model {}",
            diagnostics.model_id,
            requested_model_id
        ));
    }

    Ok(())
}

fn validate_legacy_options(options: &TranscriptionOptions) -> Result<()> {
    if options.language.is_some() {
        return Err(anyhow!(
            "language selection is not supported by the Phase 1 legacy transcription path"
        ));
    }
    if options.translate_to_english {
        return Err(anyhow!(
            "translation is not supported by the Phase 1 legacy transcription path"
        ));
    }
    if options.enable_timestamps {
        return Err(anyhow!(
            "requesting timestamps is not supported by the Phase 1 legacy transcription path"
        ));
    }
    if options.initial_prompt.is_some() {
        return Err(anyhow!(
            "initial prompts are not supported by the Phase 1 legacy transcription path"
        ));
    }

    Ok(())
}

fn capabilities_for_legacy_model(model: &SttModelInfo) -> RuntimeCapabilities {
    RuntimeCapabilities {
        // Only the current Vosk and faster-whisper adapters reliably expose
        // timestamp values. whisper.cpp strips its text timing and the sherpa
        // family currently reports null segment bounds.
        timestamps: matches!(model.backend.as_str(), "faster-whisper" | "Vosk"),
        ..RuntimeCapabilities::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::TranscriptSegment as LegacyTranscriptSegment;

    fn legacy_result() -> LegacyTranscriptResult {
        LegacyTranscriptResult {
            model_id: "faster_whisper_tiny_en".to_owned(),
            model_name: "faster-whisper tiny.en".to_owned(),
            backend: "faster-whisper".to_owned(),
            text: "hello world".to_owned(),
            segments: vec![LegacyTranscriptSegment {
                start_ms: Some(12),
                end_ms: Some(345),
                text: "hello world".to_owned(),
            }],
            duration_ms: Some(678),
            stdout: "runner output".to_owned(),
            stderr: "runner diagnostic".to_owned(),
        }
    }

    #[test]
    fn legacy_result_mapping_preserves_metadata_and_keeps_processing_time_separate() {
        let (transcript, diagnostics) = map_legacy_result(legacy_result());

        assert_eq!(transcript.text, "hello world");
        assert_eq!(transcript.duration_ms, None);
        assert_eq!(transcript.detected_language, None);
        assert_eq!(
            transcript.segments,
            vec![TranscriptSegment {
                text: "hello world".to_owned(),
                start_ms: Some(12),
                end_ms: Some(345),
                confidence: None,
            }]
        );
        assert_eq!(diagnostics.model_id, ModelId::new("faster_whisper_tiny_en"));
        assert_eq!(diagnostics.model_name, "faster-whisper tiny.en");
        assert_eq!(diagnostics.backend_label, "faster-whisper");
        assert_eq!(diagnostics.processing_duration_ms, Some(678));
        assert_eq!(diagnostics.stdout, "runner output");
        assert_eq!(diagnostics.stderr, "runner diagnostic");
    }

    #[test]
    fn legacy_result_mapping_preserves_unknown_processing_time_and_timestamps() {
        let mut result = legacy_result();
        result.duration_ms = None;
        result.segments = vec![LegacyTranscriptSegment {
            start_ms: None,
            end_ms: None,
            text: "unknown timing".to_owned(),
        }];

        let (transcript, diagnostics) = map_legacy_result(result);

        assert_eq!(transcript.duration_ms, None);
        assert_eq!(diagnostics.processing_duration_ms, None);
        assert_eq!(transcript.segments[0].start_ms, None);
        assert_eq!(transcript.segments[0].end_ms, None);
    }

    #[test]
    fn default_options_request_only_legacy_supported_behavior() {
        assert_eq!(
            TranscriptionOptions::default(),
            TranscriptionOptions {
                language: None,
                translate_to_english: false,
                enable_timestamps: false,
                initial_prompt: None,
            }
        );
        assert!(validate_legacy_options(&TranscriptionOptions::default()).is_ok());
    }

    #[test]
    fn legacy_options_fail_instead_of_being_silently_ignored() {
        let unsupported_options = [
            TranscriptionOptions {
                language: Some("en".to_owned()),
                ..TranscriptionOptions::default()
            },
            TranscriptionOptions {
                translate_to_english: true,
                ..TranscriptionOptions::default()
            },
            TranscriptionOptions {
                enable_timestamps: true,
                ..TranscriptionOptions::default()
            },
            TranscriptionOptions {
                initial_prompt: Some("domain vocabulary".to_owned()),
                ..TranscriptionOptions::default()
            },
        ];

        for options in unsupported_options {
            assert!(validate_legacy_options(&options).is_err());
        }
    }

    #[test]
    fn capabilities_are_conservative_for_every_legacy_backend() {
        for model in config::configured_models(&AppConfig::default()) {
            let capabilities = capabilities_for_legacy_model(&model);
            let timestamps_expected = matches!(model.backend.as_str(), "faster-whisper" | "Vosk");

            assert_eq!(
                capabilities.timestamps, timestamps_expected,
                "{} timestamp capability",
                model.backend
            );
            assert!(!capabilities.streaming, "{} streaming", model.backend);
            assert!(!capabilities.translation, "{} translation", model.backend);
            assert!(
                !capabilities.language_detection,
                "{} language detection",
                model.backend
            );
            assert!(
                !capabilities.confidence_scores,
                "{} confidence scores",
                model.backend
            );
            assert!(
                !capabilities.custom_vocabulary,
                "{} custom vocabulary",
                model.backend
            );
            assert!(capabilities.supported_languages.is_empty());
        }
    }

    #[test]
    fn service_rejects_unknown_models_without_needing_a_runtime() {
        let service = TranscriptionService::new(AppConfig::default());
        let error = service
            .transcribe_file(TranscriptionRequest::new(
                SessionId(4),
                RequestId(9),
                PathBuf::from("missing.wav"),
                "not-a-configured-model",
            ))
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("unknown configured transcription model")
        );
    }

    #[test]
    fn service_returns_legacy_adapter_option_errors_without_needing_a_runtime() {
        let service = TranscriptionService::new(AppConfig::default());
        let mut request = TranscriptionRequest::new(
            SessionId(4),
            RequestId(10),
            PathBuf::from("missing.wav"),
            "whisper_cpp_tiny_en",
        );
        request.options.initial_prompt = Some("domain vocabulary".to_owned());

        let error = service.transcribe_file(request).unwrap_err();

        assert!(error.to_string().contains("initial prompts"));
    }

    #[test]
    fn legacy_adapter_reports_unimplemented_health_check_without_a_runtime() {
        let model = config::configured_models(&AppConfig::default())
            .into_iter()
            .find(|model| model.id == "whisper_cpp_tiny_en")
            .expect("whisper.cpp tiny model exists");
        let mut adapter = LegacyBatchAdapter::new(AppConfig::default(), model);

        let error = adapter.health_check().unwrap_err();

        assert!(error.to_string().contains("not implemented"));
    }

    #[test]
    fn legacy_adapter_has_explicit_stateless_load_and_unsupported_cancel_semantics() {
        let model = config::configured_models(&AppConfig::default())
            .into_iter()
            .find(|model| model.id == "whisper_cpp_tiny_en")
            .expect("whisper.cpp tiny model exists");
        let mut adapter = LegacyBatchAdapter::new(AppConfig::default(), model);

        adapter
            .load()
            .expect("legacy adapter has no persistent load");
        let error = adapter.cancel().unwrap_err();
        adapter
            .unload()
            .expect("legacy adapter has no persistent unload");

        assert!(error.to_string().contains("cancellation is not supported"));
    }

    #[test]
    fn model_id_exposes_a_neutral_stable_reference() {
        let model_id = ModelId::new("whisper_cpp_tiny_en");

        assert_eq!(model_id.as_str(), "whisper_cpp_tiny_en");
        assert_eq!(model_id.to_string(), "whisper_cpp_tiny_en");
        assert_eq!(model_id.into_inner(), "whisper_cpp_tiny_en");
    }

    #[test]
    fn legacy_response_model_must_match_the_requested_model() {
        let (_, diagnostics) = map_legacy_result(legacy_result());
        let error = validate_response_model_id(&ModelId::new("whisper_cpp_tiny_en"), &diagnostics)
            .unwrap_err();

        assert!(error.to_string().contains("returned model"));
        assert!(error.to_string().contains("faster_whisper_tiny_en"));
    }

    #[test]
    fn stream_update_owns_its_value_data() {
        let original = StreamUpdate {
            committed: "settled".to_owned(),
            tentative: "draft".to_owned(),
        };
        let mut copy = original.clone();
        copy.tentative.push_str(" revision");

        assert_eq!(original.committed, "settled");
        assert_eq!(original.tentative, "draft");
        assert_eq!(copy.tentative, "draft revision");
    }

    #[test]
    fn request_and_outcome_keep_correlation_ids() {
        let request = TranscriptionRequest::new(
            SessionId(11),
            RequestId(29),
            PathBuf::from("audio.wav"),
            "whisper_cpp_tiny_en",
        );
        let outcome = TranscriptionOutcome {
            session_id: request.session_id,
            request_id: request.request_id,
            model_id: ModelId::new("whisper_cpp_tiny_en"),
            model_name: "whisper.cpp tiny.en".to_owned(),
            backend_label: "whisper.cpp".to_owned(),
            transcript: Transcript {
                text: "done".to_owned(),
                segments: Vec::new(),
                detected_language: None,
                duration_ms: None,
            },
            processing_duration_ms: None,
            stdout: String::new(),
            stderr: String::new(),
        };

        assert_eq!(outcome.session_id, SessionId(11));
        assert_eq!(outcome.request_id, RequestId(29));
    }

    #[test]
    #[ignore = "requires a local whisper.cpp CLI, GGML model, and JFK WAV fixture; set SCRIBE_WHISPER_CPP_CLI, SCRIBE_WHISPER_CPP_MODEL, and SCRIBE_WHISPER_CPP_AUDIO"]
    fn transcription_service_jfk_smoke_uses_the_whisper_cpp_facade() {
        let whisper_cli = PathBuf::from(
            std::env::var_os("SCRIBE_WHISPER_CPP_CLI")
                .expect("set SCRIBE_WHISPER_CPP_CLI to the pinned whisper.cpp CLI"),
        );
        let model_path = PathBuf::from(
            std::env::var_os("SCRIBE_WHISPER_CPP_MODEL")
                .expect("set SCRIBE_WHISPER_CPP_MODEL to the pinned GGML model"),
        );
        let audio_path = PathBuf::from(
            std::env::var_os("SCRIBE_WHISPER_CPP_AUDIO")
                .expect("set SCRIBE_WHISPER_CPP_AUDIO to the JFK WAV fixture"),
        );

        let config = AppConfig {
            whisper_executable_path: Some(whisper_cli),
            ..AppConfig::default()
        };
        let service = TranscriptionService::new(config);
        let session_id = SessionId(701);
        let request_id = RequestId(1701);
        let mut request =
            TranscriptionRequest::new(session_id, request_id, audio_path, "whisper_cpp_base_en");
        request.model_path = Some(model_path);

        let outcome = service
            .transcribe_file(request)
            .expect("whisper.cpp facade smoke transcription succeeds");

        assert!(!outcome.transcript.text.trim().is_empty());
        assert_eq!(outcome.session_id, session_id);
        assert_eq!(outcome.request_id, request_id);
        assert_eq!(outcome.model_id, ModelId::new("whisper_cpp_base_en"));
        assert_eq!(outcome.model_name, "whisper.cpp base.en");
        assert_eq!(outcome.backend_label, "whisper.cpp");
    }
}
