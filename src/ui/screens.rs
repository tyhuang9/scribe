//! Shared, backend-neutral egui screen renderers.

use eframe::egui::{self, Align, ComboBox, Grid, Layout, RichText};

use super::{
    controls::{ButtonTone, Icon, badge, button, card, icon_button, keycap, notice},
    state::{
        ModelComparisonState, ModelSizeTier, ModelSpeedTier, ModelViewModel, RecordingMode,
        SettingsSaveState, SettingsTab, TranscriptionPhase, TranscriptionState, UiRoute,
    },
    ui_palette,
};

#[derive(Clone, Debug)]
pub(crate) struct RecordingSettingsView {
    pub duration_label: String,
    pub provisional_feedback: bool,
    pub device_label: String,
    pub input_level: f32,
    pub save_state: SettingsSaveState,
}

impl Default for RecordingSettingsView {
    fn default() -> Self {
        Self {
            duration_label: "30 seconds".into(),
            provisional_feedback: true,
            device_label: "OS default".into(),
            input_level: 0.0,
            save_state: SettingsSaveState::Clean,
        }
    }
}

pub(crate) struct ScreenView<'a> {
    pub route: UiRoute,
    pub transcription: &'a TranscriptionState,
    pub models: &'a [ModelViewModel],
    pub comparison: &'a ModelComparisonState,
    pub recording_settings: &'a RecordingSettingsView,
}

pub(crate) fn show_screen(ui: &mut egui::Ui, view: &ScreenView<'_>) {
    match view.route {
        UiRoute::Transcribe => transcribe(ui, view.transcription, view.models),
        UiRoute::Models => models(ui, view.models, view.comparison),
        UiRoute::Settings(tab) => settings(ui, tab, view.transcription, view.recording_settings),
        UiRoute::History => placeholder(
            ui,
            "History",
            "Local dictation history remains available in production.",
        ),
        UiRoute::About => placeholder(
            ui,
            "About",
            "Scribe keeps audio and transcripts on this device.",
        ),
        UiRoute::Debug => placeholder(
            ui,
            "Debug",
            "Debug tools are available only when explicitly enabled.",
        ),
    }
}

fn header(ui: &mut egui::Ui, title: &str, subtitle: &str) {
    let response = ui.label(RichText::new(title).size(30.0).strong());
    ui.ctx().accesskit_node_builder(response.id, |builder| {
        builder.set_role(egui::accesskit::Role::Heading)
    });
    ui.label(RichText::new(subtitle).color(ui_palette(ui).muted_text));
    ui.add_space(24.0);
}

fn transcribe(ui: &mut egui::Ui, state: &TranscriptionState, models: &[ModelViewModel]) {
    header(ui, "Transcribe", "Audio stays on this device.");
    let selected_name = state
        .selected_model_id
        .as_deref()
        .and_then(|id| {
            models
                .iter()
                .find(|model| model.id == id)
                .map(|model| model.display_name.as_str())
        })
        .unwrap_or("No model selected");
    if state.phase == TranscriptionPhase::NoModel {
        card(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(selected_name).strong());
                ui.separator();
                ui.label("Hotkey:");
                for key in state
                    .hotkey
                    .split('+')
                    .map(str::trim)
                    .filter(|key| !key.is_empty())
                {
                    keycap(ui, key);
                }
            });
        });
        ui.add_space(12.0);
    }
    if state.phase == TranscriptionPhase::NoModel {
        card(ui, |ui| {
            ui.set_min_height(480.0);
            ui.with_layout(Layout::top_down(Align::Center), |ui| {
                ui.add_space(130.0);
                ui.label(
                    RichText::new("Add a speech model to start transcribing")
                        .size(18.0)
                        .strong(),
                );
                ui.label("Your audio stays on this device.");
                ui.add_space(12.0);
                let _ = button(ui, "Add model", ButtonTone::Primary);
            });
        });
        ui.add_space(14.0);
        ui.label("Silence is ignored and won’t replace your transcript.");
        return;
    }
    card(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(selected_name).strong());
            ui.separator();
            ui.label("Hotkey:");
            for key in state
                .hotkey
                .split('+')
                .map(str::trim)
                .filter(|key| !key.is_empty())
            {
                keycap(ui, key);
            }
        });
    });
    ui.add_space(12.0);
    card(ui, |ui| {
        match state.phase {
            TranscriptionPhase::Listening => {
                ui.label(
                    RichText::new(super::controls::icon_glyph(Icon::Stop))
                        .size(24.0)
                        .color(ui_palette(ui).error),
                );
                ui.label(
                    RichText::new("Listening")
                        .color(ui_palette(ui).error)
                        .strong(),
                );
                ui.label(format_elapsed(state.elapsed_ms));
            }
            TranscriptionPhase::Finalizing => {
                ui.spinner();
                ui.label(RichText::new("Finalizing transcript…").strong());
                ui.label("This may take a moment.");
            }
            _ => {
                ui.label(RichText::new(super::controls::icon_glyph(Icon::Microphone)).size(24.0));
                ui.label(RichText::new("Start recording").strong());
                ui.label(match state.recording_mode {
                    RecordingMode::Hold => format!("Hold {} to record", state.hotkey),
                    RecordingMode::PressOnce => format!("Press {} to toggle", state.hotkey),
                });
            }
        }
        ui.separator();
        if let Some(notice_text) = &state.notice {
            notice(
                ui,
                notice_text,
                state.phase == TranscriptionPhase::MicrophoneError,
            );
            ui.add_space(12.0);
            if state.phase == TranscriptionPhase::MicrophoneError {
                ui.horizontal(|ui| {
                    let _ = button(ui, "Open audio settings", ButtonTone::Text);
                    let _ = button(ui, "Try again", ButtonTone::Danger);
                });
            }
        }
        if state.committed_transcript.trim().is_empty() {
            ui.label(
                RichText::new("Your transcript will appear here.")
                    .color(ui_palette(ui).tertiary_text),
            );
        } else {
            ui.label(&state.committed_transcript);
        }
        if !state.provisional_transcript.is_empty() {
            ui.label(
                RichText::new(&state.provisional_transcript).color(ui_palette(ui).tertiary_text),
            );
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            badge(ui, "2 MINS AGO", None);
            badge(ui, "BASE.EN", None);
        });
        ui.add_space(160.0);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let enabled = !matches!(
                state.phase,
                TranscriptionPhase::Listening | TranscriptionPhase::Finalizing
            );
            let _ = ui.add_enabled(enabled, egui::Button::new("Copy"));
            let _ = ui.add_enabled(enabled, egui::Button::new("Clear"));
        });
    });
}

fn models(ui: &mut egui::Ui, models: &[ModelViewModel], comparison: &ModelComparisonState) {
    header(
        ui,
        "Models",
        "Manage the speech models available on this device.",
    );
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
        let _ = button(ui, "+ Add models", ButtonTone::Primary);
        let _ = button(ui, "Compare", ButtonTone::Secondary);
    });
    ui.add_space(16.0);
    for model in models {
        card(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(&model.display_name).strong());
                badge(
                    ui,
                    if model.active { "Active" } else { "Installed" },
                    model.active.then_some(ui_palette(ui).success),
                );
                if model.recommended {
                    badge(ui, "Recommended", None);
                }
                if let Some(ram) = model.estimated_ram_bytes {
                    ui.label(format!("{} MB RAM", ram / 1_000_000));
                }
                ui.label(&model.language_summary);
                ui.label(speed_label(model.speed_tier));
                ui.label(size_label(model.size_tier));
            });
        });
        ui.add_space(8.0);
    }
    card(ui, |ui| {
        ui.label(RichText::new("Compare installed models").strong());
        ui.label("Comparison measures speed and output on this computer.");
        if comparison.expanded {
            ui.add_space(12.0);
            for model in models {
                let mut checked = comparison.selected_model_ids.contains(&model.id);
                let _ = ui.checkbox(&mut checked, &model.display_name);
            }
            let _ = button(ui, "Start test recording", ButtonTone::Primary);
            ui.separator();
            Grid::new("comparison-results")
                .striped(true)
                .show(ui, |ui| {
                    for heading in ["Model", "Duration", "Processing time", "Output", "Accuracy"] {
                        ui.label(RichText::new(heading).strong());
                    }
                    ui.end_row();
                    for model in models {
                        let result = comparison
                            .results
                            .iter()
                            .find(|(id, _)| id == &model.id)
                            .map(|(_, result)| result);
                        ui.label(&model.variant_label);
                        ui.label(
                            comparison
                                .audio_duration_ms
                                .map_or("—".into(), |ms| format!("{} ms", ms)),
                        );
                        ui.label(
                            result
                                .and_then(|r| r.processing_ms)
                                .map_or("—".into(), |ms| format!("{} ms", ms)),
                        );
                        ui.label(
                            result
                                .and_then(|r| r.output.as_deref())
                                .unwrap_or("No data"),
                        );
                        ui.label(if comparison.reference_transcript.is_some() {
                            "Measured"
                        } else {
                            "Add a reference transcript to measure"
                        });
                        ui.end_row();
                    }
                });
        }
    });
}

fn settings(
    ui: &mut egui::Ui,
    active_tab: SettingsTab,
    state: &TranscriptionState,
    settings: &RecordingSettingsView,
) {
    header(
        ui,
        "Settings",
        match settings.save_state {
            SettingsSaveState::Saving => "Saving…",
            SettingsSaveState::Failed => "Couldn’t save changes",
            SettingsSaveState::Saved => "Changes saved",
            _ => "Changes save automatically",
        },
    );
    ui.horizontal(|ui| {
        for (tab, label) in [
            (SettingsTab::General, "General"),
            (SettingsTab::Recording, "Recording"),
            (SettingsTab::Output, "Output"),
            (SettingsTab::Advanced, "Advanced"),
        ] {
            let _ = ui.selectable_label(tab == active_tab, label);
        }
    });
    ui.add_space(16.0);
    card(ui, |ui| {
        ui.label(RichText::new("Recording behavior").strong());
        ui.horizontal(|ui| {
            ui.label("Mode");
            let _ = ui.selectable_label(
                state.recording_mode == RecordingMode::PressOnce,
                "Press once",
            );
            let _ = ui.selectable_label(state.recording_mode == RecordingMode::Hold, "Hold");
        });
        ui.separator();
        ui.horizontal(|ui| {
            ui.label("Duration limit");
            ComboBox::from_id_source("duration-limit")
                .selected_text(&settings.duration_label)
                .show_ui(ui, |_| {});
        });
        ui.separator();
        let mut enabled = settings.provisional_feedback;
        let _ = ui.checkbox(&mut enabled, "Show provisional words while recording");
        ui.label("Improves visual feedback but may use more CPU.");
    });
    ui.add_space(16.0);
    card(ui, |ui| {
        ui.label(RichText::new("Audio input").strong());
        ui.horizontal_wrapped(|ui| {
            ui.label("Device");
            ComboBox::from_id_source("audio-device")
                .selected_text(&settings.device_label)
                .show_ui(ui, |_| {});
            let _ = icon_button(ui, Icon::Refresh, "Refresh devices");
        });
        ui.horizontal(|ui| {
            ui.label("Input level");
            ui.add(egui::ProgressBar::new(settings.input_level).desired_width(260.0));
        });
    });
    ui.add_space(16.0);
    card(ui, |ui| {
        ui.label(RichText::new("Shortcut").strong());
        ui.horizontal(|ui| {
            ui.label("Global record hotkey");
            for key in state
                .hotkey
                .split('+')
                .map(str::trim)
                .filter(|key| !key.is_empty())
            {
                keycap(ui, key);
            }
            let _ = button(ui, "Change shortcut", ButtonTone::Secondary);
        });
    });
}

fn placeholder(ui: &mut egui::Ui, title: &str, message: &str) {
    header(ui, title, message);
    card(ui, |ui| {
        ui.label(message);
    });
}

fn format_elapsed(elapsed_ms: u64) -> String {
    format!(
        "{:02}:{:02}",
        elapsed_ms / 60_000,
        (elapsed_ms / 1_000) % 60
    )
}

fn speed_label(tier: ModelSpeedTier) -> &'static str {
    match tier {
        ModelSpeedTier::VeryFast => "Very Fast",
        ModelSpeedTier::Fast => "Fast",
        ModelSpeedTier::Balanced => "Balanced Speed",
        ModelSpeedTier::AccurateSlow => "Accurate, slower",
        ModelSpeedTier::Unknown => "Speed unknown",
    }
}

fn size_label(tier: ModelSizeTier) -> &'static str {
    match tier {
        ModelSizeTier::Tiny => "Tiny Size",
        ModelSizeTier::Small => "Small Size",
        ModelSizeTier::Base => "Base Size",
        ModelSizeTier::Medium => "Medium Size",
        ModelSizeTier::Large => "Large Size",
        ModelSizeTier::Unknown => "Size unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn elapsed_display_is_deterministic() {
        assert_eq!(format_elapsed(8_000), "00:08");
    }
}
