//! Thin mappings between the live application runtime and backend-neutral screens.

use crate::backend_policy::{BackendFailureCategory, BackendSelectionReason, BackendSkipReason};
use crate::models::TranscriptionStatus;
use crate::transcription::ResolvedAcceleration;

use super::state::{
    AccelerationDiagnosticsView, MicrophonePermission, RecordingMode, SettingsSaveState,
    TranscribeNotice, TranscriptionPhase, TranscriptionState,
};

#[cfg(test)]
use crate::backend_policy::{
    BackendFallback, BackendKind, BackendPackIdentity, BackendSelection, BackendTarget,
    DeviceClass, DeviceIdentity, GpuVendor, PowerPolicyDecision, PowerSource, ProviderIdentity,
    SkippedBackend,
};

/// Projects private runtime selection into labels safe for the settings UI.
/// The projection deliberately omits stable identities, digests, paths, and
/// native error details.
pub(crate) fn acceleration_diagnostics(
    resolved: Option<&ResolvedAcceleration>,
    retry_gpu_available: bool,
) -> Option<AccelerationDiagnosticsView> {
    let selection = resolved?.selection.as_ref()?;
    let target = &selection.target;
    let skipped_reasons = selection
        .skipped_targets
        .iter()
        .map(|skipped| {
            format!(
                "{} ({}) — {}",
                skipped.target.backend.label(),
                skipped.target.display_name,
                skipped.reason.label()
            )
        })
        .collect::<Vec<_>>();
    let quarantine_status = if selection
        .skipped_targets
        .iter()
        .any(|skipped| skipped.reason == BackendSkipReason::Quarantined)
    {
        "A GPU is temporarily quarantined".to_owned()
    } else {
        "No GPU quarantine is active".to_owned()
    };
    let fallback_status = if !selection.fallback_history.is_empty() {
        "A bounded fallback was used".to_owned()
    } else if selection.reason == BackendSelectionReason::AutoCpuFallback {
        "CPU fallback is active".to_owned()
    } else if !selection.fallback_targets.is_empty() {
        "A bounded fallback is available".to_owned()
    } else {
        "No fallback was needed".to_owned()
    };
    let fallback_details = selection
        .fallback_history
        .iter()
        .enumerate()
        .map(|(index, fallback)| {
            let next = selection
                .fallback_history
                .get(index + 1)
                .map(|next| &next.target)
                .unwrap_or(target);
            format!(
                "{} ({}) failed: {}; next: {} ({})",
                fallback.target.backend.label(),
                fallback.target.display_name,
                fallback_category_label(fallback.category),
                next.backend.label(),
                next.display_name,
            )
        })
        .collect::<Vec<_>>();
    let pack = target.pack.as_ref();
    Some(AccelerationDiagnosticsView {
        selected_backend: target.backend.label().to_owned(),
        selected_device: target.display_name.clone(),
        selection_reason: selection.reason.label().to_owned(),
        skipped_reasons,
        pack_id: pack.map(|pack| pack.pack_id.clone()),
        pack_version: pack.map(|pack| pack.pack_version.clone()),
        driver: target.driver_version.clone(),
        power_source: power_source_label(selection.power_source).to_owned(),
        power_policy: selection.power_policy.label().to_owned(),
        quarantine_status,
        fallback_status,
        fallback_details,
        retry_gpu_available,
        retry_gpu_in_flight: false,
        retry_gpu_status: None,
    })
}

fn power_source_label(source: crate::backend_policy::PowerSource) -> &'static str {
    match source {
        crate::backend_policy::PowerSource::Ac => "Plugged in",
        crate::backend_policy::PowerSource::Battery => "Battery",
        crate::backend_policy::PowerSource::Unknown => "Unknown",
    }
}

fn fallback_category_label(category: BackendFailureCategory) -> &'static str {
    match category {
        BackendFailureCategory::BackendUnavailable => "backend unavailable",
        BackendFailureCategory::InitializationFailed => "initialization failed",
        BackendFailureCategory::OutOfMemory => "out of memory",
        BackendFailureCategory::DeviceLost => "device lost",
        BackendFailureCategory::WorkerFailed => "worker failed",
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ModelReadiness {
    Ready,
    Loading,
    Error,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn transcription_state(
    status: TranscriptionStatus,
    selected_model_id: Option<String>,
    model_readiness: ModelReadiness,
    requesting_microphone: bool,
    no_speech: bool,
    elapsed_ms: u64,
    transcript: String,
    provisional_transcript: String,
    notice: Option<TranscribeNotice>,
    hotkey: String,
    recording_mode: RecordingMode,
    microphone_permission: MicrophonePermission,
) -> TranscriptionState {
    let has_selected_model = selected_model_id.is_some();
    let phase = match (status, has_selected_model, requesting_microphone, no_speech) {
        (_, false, false, _) => TranscriptionPhase::NoModel,
        (_, _, true, _) => TranscriptionPhase::RequestingMicrophone,
        (_, _, false, true) => TranscriptionPhase::NoSpeech,
        (TranscriptionStatus::Listening, _, _, _) => TranscriptionPhase::Listening,
        (TranscriptionStatus::Transcribing, _, _, _) => TranscriptionPhase::Finalizing,
        (TranscriptionStatus::Error, _, _, _)
            if microphone_permission == MicrophonePermission::Denied =>
        {
            TranscriptionPhase::MicrophoneError
        }
        _ => match model_readiness {
            ModelReadiness::Ready => TranscriptionPhase::Ready,
            ModelReadiness::Loading => TranscriptionPhase::ModelLoading,
            ModelReadiness::Error => TranscriptionPhase::ModelError,
        },
    };

    TranscriptionState {
        phase,
        selected_model_id,
        committed_transcript: transcript,
        provisional_transcript,
        elapsed_ms,
        notice,
        microphone_permission,
        recording_mode,
        hotkey,
        ..Default::default()
    }
}

pub(crate) fn recording_mode(hold_to_talk: bool) -> RecordingMode {
    if hold_to_talk {
        RecordingMode::Hold
    } else {
        RecordingMode::PressOnce
    }
}

pub(crate) fn settings_save_state(
    persistence_pending: bool,
    last_error: bool,
) -> SettingsSaveState {
    if last_error {
        SettingsSaveState::Failed
    } else if persistence_pending {
        SettingsSaveState::Saving
    } else {
        SettingsSaveState::Clean
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_live_capture_and_failure_phases_without_losing_transcript() {
        let listening = transcription_state(
            TranscriptionStatus::Listening,
            Some("base.en".into()),
            ModelReadiness::Ready,
            false,
            false,
            1_000,
            "Saved text".into(),
            "Partial".into(),
            None,
            "Ctrl+Shift+Space".into(),
            RecordingMode::PressOnce,
            MicrophonePermission::Granted,
        );
        assert_eq!(listening.phase, TranscriptionPhase::Listening);
        assert_eq!(listening.committed_transcript, "Saved text");

        let denied = transcription_state(
            TranscriptionStatus::Error,
            Some("base.en".into()),
            ModelReadiness::Error,
            false,
            false,
            0,
            "Saved text".into(),
            String::new(),
            Some(TranscribeNotice::information(
                "Scribe couldn\u{2019}t access your microphone",
            )),
            "Ctrl+Shift+Space".into(),
            RecordingMode::PressOnce,
            MicrophonePermission::Denied,
        );
        assert_eq!(denied.phase, TranscriptionPhase::MicrophoneError);
        assert_eq!(denied.committed_transcript, "Saved text");
    }

    #[test]
    fn maps_persistence_and_recording_preferences() {
        assert_eq!(recording_mode(false), RecordingMode::PressOnce);
        assert_eq!(recording_mode(true), RecordingMode::Hold);
        assert_eq!(settings_save_state(true, false), SettingsSaveState::Saving);
        assert_eq!(settings_save_state(false, true), SettingsSaveState::Failed);
    }

    #[test]
    fn preserves_a_configured_model_while_its_runtime_is_not_ready() {
        let loading = transcription_state(
            TranscriptionStatus::Idle,
            Some("base.en".into()),
            ModelReadiness::Loading,
            false,
            false,
            0,
            String::new(),
            String::new(),
            None,
            "Ctrl+Shift+Space".into(),
            RecordingMode::PressOnce,
            MicrophonePermission::Unknown,
        );
        assert_eq!(loading.phase, TranscriptionPhase::ModelLoading);
        assert_eq!(loading.selected_model_id.as_deref(), Some("base.en"));

        let failed = transcription_state(
            TranscriptionStatus::Idle,
            Some("base.en".into()),
            ModelReadiness::Error,
            false,
            false,
            0,
            String::new(),
            String::new(),
            None,
            "Ctrl+Shift+Space".into(),
            RecordingMode::PressOnce,
            MicrophonePermission::Unknown,
        );
        assert_eq!(failed.phase, TranscriptionPhase::ModelError);
        assert_eq!(failed.selected_model_id.as_deref(), Some("base.en"));

        let no_model = transcription_state(
            TranscriptionStatus::Idle,
            None,
            ModelReadiness::Error,
            false,
            false,
            0,
            String::new(),
            String::new(),
            None,
            "Ctrl+Shift+Space".into(),
            RecordingMode::PressOnce,
            MicrophonePermission::Unknown,
        );
        assert_eq!(no_model.phase, TranscriptionPhase::NoModel);
    }

    #[test]
    fn unrelated_global_errors_do_not_become_transcribe_model_failures() {
        let state = transcription_state(
            TranscriptionStatus::Error,
            Some("base.en".into()),
            ModelReadiness::Ready,
            false,
            false,
            0,
            "Keep this text".into(),
            String::new(),
            None,
            "Ctrl+Space".into(),
            RecordingMode::PressOnce,
            MicrophonePermission::Granted,
        );

        assert_eq!(state.phase, TranscriptionPhase::Ready);
        assert!(state.notice.is_none());
        assert_eq!(state.committed_transcript, "Keep this text");
    }

    #[test]
    fn acceleration_diagnostics_projects_labels_without_private_runtime_identity() {
        let target = BackendTarget {
            backend: BackendKind::Vulkan,
            provider_id: ProviderIdentity::new("provider-stable-secret"),
            driver_version: Some("32.0.16.1088".into()),
            device_id: DeviceIdentity::new("stable-device-secret"),
            display_name: "Studio GPU".into(),
            vendor: GpuVendor::Amd,
            device_class: DeviceClass::DiscreteGpu,
            memory_total_bytes: 8 * 1024 * 1024 * 1024,
            memory_available_bytes: 7 * 1024 * 1024 * 1024,
            pack: Some(BackendPackIdentity {
                pack_id: "scribe-gpu".into(),
                pack_version: "1.2.3".into(),
                pack_digest: "pack-digest-secret".into(),
                security_epoch: 7,
                runtime_abi: 4,
            }),
            process_index: Some(3),
        };
        let selection = BackendSelection {
            requested: crate::transcription::AccelerationPreference::Gpu,
            target: target.clone(),
            reason: BackendSelectionReason::RequestedGpu,
            power_source: PowerSource::Ac,
            power_policy: PowerPolicyDecision::Unrestricted,
            qualification_policy_version: 2,
            fallback_targets: Vec::new(),
            fallback_history: vec![BackendFallback {
                target: BackendTarget {
                    backend: BackendKind::Cuda,
                    display_name: "Office GPU".into(),
                    ..target.clone()
                },
                category: BackendFailureCategory::OutOfMemory,
            }],
            skipped_targets: vec![SkippedBackend {
                target: BackendTarget {
                    display_name: "Office GPU".into(),
                    ..target.clone()
                },
                reason: BackendSkipReason::Quarantined,
            }],
        };
        let resolved = ResolvedAcceleration {
            requested: crate::transcription::AccelerationPreference::Gpu,
            resolved: crate::transcription::ComputeDevice::Gpu {
                name: "Studio GPU".into(),
            },
            diagnostic: Some("raw native error must stay private".into()),
            selection: Some(selection),
        };

        let view = acceleration_diagnostics(Some(&resolved), true).expect("selection projects");
        assert_eq!(view.selected_backend, "Vulkan");
        assert_eq!(view.selected_device, "Studio GPU");
        assert_eq!(view.pack_id.as_deref(), Some("scribe-gpu"));
        assert_eq!(view.pack_version.as_deref(), Some("1.2.3"));
        assert_eq!(view.driver.as_deref(), Some("32.0.16.1088"));
        assert_eq!(view.power_source, "Plugged in");
        assert_eq!(
            view.fallback_details,
            ["CUDA (Office GPU) failed: out of memory; next: Vulkan (Studio GPU)"]
        );
        assert!(
            view.skipped_reasons
                .iter()
                .any(|reason| reason.contains("temporarily quarantined"))
        );
        let visible = format!("{view:?}");
        for private_value in [
            "provider-stable-secret",
            "stable-device-secret",
            "pack-digest-secret",
            "raw native error",
        ] {
            assert!(!visible.contains(private_value), "leaked {private_value}");
        }
    }
}
