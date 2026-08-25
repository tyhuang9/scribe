//! Debug-only demo audio preparation and transcription bootstrap.
//!
//! This stays outside `ui` so the presentation layer receives only the
//! resulting transcript, never native audio data.

use std::{path::Path, sync::Arc};

use crate::{
    config,
    model_catalog::BUNDLED_BASE_MODEL_ID,
    prepared_audio::PreparedAudio,
    transcription::{RequestId, SessionId, TranscriptionRequest, TranscriptionService},
};

/// Resolves the explicitly requested debug-only demo audio to genuine local
/// transcription output. The UI timing is simulated later, but its text comes
/// from the same service used by the desktop application.
pub(crate) fn transcribe_demo_audio_from_env() -> Result<Option<String>, String> {
    let Some(path) = std::env::var_os("SCRIBE_DEMO_AUDIO") else {
        return Ok(None);
    };
    transcribe_demo_audio(Path::new(&path)).map(Some)
}

fn transcribe_demo_audio(path: &Path) -> Result<String, String> {
    let audio = PreparedAudio::from_wav_path(path)
        .map(Arc::new)
        .map_err(|error| format!("could not prepare demo WAV: {error}"))?;
    let (config, _) = config::load_config()
        .map_err(|error| format!("could not load the local Scribe configuration: {error}"))?;
    let service = TranscriptionService::new(config);
    let mut request = TranscriptionRequest::new(
        SessionId(900_001),
        RequestId(900_001),
        audio,
        BUNDLED_BASE_MODEL_ID,
    );
    request.model_path = std::env::var_os("SCRIBE_DEMO_MODEL").map(Into::into);
    let outcome = service
        .transcribe(request)
        .map_err(|error| format!("could not transcribe the demo WAV: {error}"))?;
    let transcript = outcome.transcript.text.trim();
    if transcript.is_empty() {
        return Err("the demo WAV did not contain recognizable speech".to_owned());
    }
    Ok(transcript.to_owned())
}
