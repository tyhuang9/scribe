use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use eframe::egui::{self, RichText};

use crate::history::{HistoryRecord, HistoryStatus};

use super::controls::search_field;

fn semantic_heading(ui: &mut egui::Ui, text: RichText) -> egui::Response {
    let response = ui.label(text);
    ui.ctx().accesskit_node_builder(response.id, |builder| {
        builder.set_role(egui::accesskit::Role::Heading);
    });
    response
}

fn set_accessible_description(ui: &egui::Ui, response: &egui::Response, description: &str) {
    ui.ctx().accesskit_node_builder(response.id, |builder| {
        if response.enabled() {
            builder.set_description(description);
        } else {
            builder.set_description(format!(
                "{description}. Unavailable until active dictation, output, history, retry, or playback work finishes."
            ));
        }
    });
}

fn set_collapsing_header_accessibility<R>(
    ctx: &egui::Context,
    response: &egui::containers::CollapsingResponse<R>,
) {
    ctx.accesskit_node_builder(response.header_response.id, |builder| {
        builder.set_expanded(response.body_response.is_some());
    });
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum HistoryPageAction {
    ApplySearch,
    ClearSearch,
    Refresh,
    LoadMore,
    Copy { text: String, label: &'static str },
    ArmRepaste { id: i64, text: String },
    TogglePinned { id: i64, pinned: bool },
    Play(i64),
    StopPlayback,
    Retry(i64),
    DeleteAudio(i64),
    RequestDelete(i64),
    ConfirmDelete(i64),
    CancelDelete,
}

pub(crate) struct HistoryPageState<'a> {
    pub search: &'a mut String,
    pub records: &'a [HistoryRecord],
    pub has_more: bool,
    pub loading: bool,
    pub error: Option<&'a str>,
    pub confirm_delete: Option<i64>,
    pub work_active: bool,
    pub playing: Option<i64>,
    pub playback_stopping: bool,
    pub armed_repaste: Option<i64>,
    pub focus_search: bool,
    pub focus_delete_confirmation: bool,
}

pub(crate) fn history_page(
    ui: &mut egui::Ui,
    state: HistoryPageState<'_>,
) -> Option<HistoryPageAction> {
    let HistoryPageState {
        search,
        records,
        has_more,
        loading,
        error,
        confirm_delete,
        work_active,
        playing,
        playback_stopping,
        armed_repaste,
        focus_search,
        focus_delete_confirmation,
    } = state;
    let mut action = None;
    const HISTORY_ACTION_WIDTH: f32 = 86.0;
    const HISTORY_CONTROL_HEIGHT: f32 = 44.0;
    let row_spacing = ui.spacing().item_spacing.x;
    let single_row_minimum =
        HISTORY_CONTROL_HEIGHT + HISTORY_ACTION_WIDTH * 2.0 + row_spacing * 2.0;
    let single_row = ui.available_width() >= single_row_minimum;
    let mut search_response = None;
    ui.vertical(|ui| {
        if single_row {
            ui.horizontal(|ui| {
                let search_width = ui.available_width()
                    - HISTORY_ACTION_WIDTH * 2.0
                    - ui.spacing().item_spacing.x * 2.0;
                search_response = Some(search_field(
                    ui,
                    search_width,
                    "history-search",
                    search,
                    "Search history",
                    "Search transcript, model, or app",
                    "Enter a query, then choose Search or press Enter to filter saved history.",
                ));
                let search_clicked = ui
                    .add_sized(
                        [HISTORY_ACTION_WIDTH, HISTORY_CONTROL_HEIGHT],
                        egui::Button::new("Search"),
                    )
                    .clicked();
                let refresh_clicked = ui
                    .add_sized(
                        [HISTORY_ACTION_WIDTH, HISTORY_CONTROL_HEIGHT],
                        egui::Button::new("Refresh"),
                    )
                    .clicked();
                if search_clicked {
                    action = Some(HistoryPageAction::ApplySearch);
                }
                if refresh_clicked {
                    action = Some(HistoryPageAction::Refresh);
                }
            });
        } else {
            search_response = Some(search_field(
                ui,
                ui.available_width(),
                "history-search",
                search,
                "Search history",
                "Search transcript, model, or app",
                "Enter a query, then choose Search or press Enter to filter saved history.",
            ));
            ui.add_space(ui.spacing().item_spacing.y);
            // Keep the actions on a second row before they can force the
            // search field below its usable width. On very narrow routes the
            // wrapped layout gives each 86x44 control its own contained row.
            let action_width =
                HISTORY_ACTION_WIDTH.min(ui.available_width().max(HISTORY_CONTROL_HEIGHT));
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_sized(
                        [action_width, HISTORY_CONTROL_HEIGHT],
                        egui::Button::new("Search"),
                    )
                    .clicked()
                {
                    action = Some(HistoryPageAction::ApplySearch);
                }
                if ui
                    .add_sized(
                        [action_width, HISTORY_CONTROL_HEIGHT],
                        egui::Button::new("Refresh"),
                    )
                    .clicked()
                {
                    action = Some(HistoryPageAction::Refresh);
                }
            });
        }
    });
    let search_response = search_response.expect("history search field is always rendered");
    if focus_search {
        search_response.input.request_focus();
    }
    let enter =
        search_response.input.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
    if enter {
        action = Some(HistoryPageAction::ApplySearch);
    }
    if search_response.clear_requested {
        action = Some(HistoryPageAction::ClearSearch);
    }

    let history_status = if loading {
        "Loading local history".to_owned()
    } else if let Some(error) = error {
        format!("History error: {error}")
    } else if records.is_empty() {
        "No matching history entries".to_owned()
    } else if has_more {
        format!(
            "{} history entries loaded; more are available",
            records.len()
        )
    } else {
        format!("{} history entries loaded", records.len())
    };
    let history_status = ui
        .push_id("history-live-status", |ui| ui.label(history_status))
        .inner;
    ui.ctx()
        .accesskit_node_builder(history_status.id, |builder| {
            builder.set_live(egui::accesskit::Live::Polite);
            builder.set_live_atomic();
            if loading {
                builder.set_busy();
            }
        });

    if let Some(error) = error {
        ui.add_space(8.0);
        ui.colored_label(ui.visuals().error_fg_color, error);
    }

    ui.add_space(12.0);
    if loading && records.is_empty() {
        ui.spinner();
        ui.label("Loading local history…");
        return action;
    }
    if records.is_empty() {
        ui.group(|ui| {
            ui.label(RichText::new("No matching history entries").strong());
            ui.label(
                "Completed and failed dictations appear here when history storage is enabled.",
            );
        });
        return action;
    }

    for record in records {
        let time_label = relative_time_label(record.created_at_ms);
        let entry_context = format!(
            "{} history entry, {}, model {}",
            history_status_label(record.status),
            time_label,
            record.model_id
        );
        let card_id = ui.make_persistent_id(("history-card-group", record.id));
        ui.ctx().accesskit_node_builder(card_id, |builder| {
            builder.set_role(egui::accesskit::Role::Group);
            builder.set_name(entry_context.clone());
        });
        let ctx = ui.ctx().clone();
        ctx.with_accessibility_parent(card_id, || {
            ui.group(|ui| {
            ui.horizontal_wrapped(|ui| {
                semantic_heading(
                    ui,
                    RichText::new(format!(
                        "{} - {}",
                        history_status_label(record.status),
                        time_label
                    ))
                    .strong(),
                );
                ui.label(format!("Model: {}", record.model_id));
                if let Some(source_app) = record.source_app.as_deref() {
                    ui.label(format!("App: {source_app}"));
                }
                if record.pinned {
                    ui.label(RichText::new("Pinned").strong());
                }
            });

            ui.add_space(6.0);
            let display_text = record
                .final_text
                .as_deref()
                .filter(|text| !text.trim().is_empty())
                .unwrap_or(record.raw_text.as_str());
            if display_text.trim().is_empty() {
                ui.label(match record.status {
                    HistoryStatus::Pending => "Transcription is pending.",
                    HistoryStatus::Failed => "No transcript was produced.",
                    HistoryStatus::Completed => "The completed transcript is empty.",
                });
            } else {
                ui.label(display_text);
            }
            if let Some(final_text) = record.final_text.as_deref()
                && !record.raw_text.trim().is_empty()
                && record.raw_text.trim() != final_text.trim()
            {
                let disclosure = egui::CollapsingHeader::new("Raw transcript")
                    .id_source(("history-raw", record.id))
                    .show(ui, |ui| {
                        ui.label(&record.raw_text);
                    });
                set_collapsing_header_accessibility(ui.ctx(), &disclosure);
            }
            if let Some(failure) = record.failure.as_deref() {
                ui.colored_label(ui.visuals().error_fg_color, failure);
            }
            if let Some(output_outcome) = record.output_outcome.as_deref() {
                ui.label(format!("Output: {}", output_outcome.replace('_', " ")));
            }

            if record.metrics.audio_duration_ms.is_some()
                || record.metrics.processing_duration_ms.is_some()
                || record.metrics.realtime_factor.is_some()
            {
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    if let Some(duration) = record.metrics.audio_duration_ms {
                        ui.label(format!("Audio: {duration} ms"));
                    }
                    if let Some(duration) = record.metrics.processing_duration_ms {
                        ui.label(format!("Processing: {duration} ms"));
                    }
                    if let Some(rtf) = record.metrics.realtime_factor {
                        ui.label(format!("RTF: {rtf:.2}"));
                    }
                    if record.retry_count > 0 {
                        ui.label(format!("Retries: {}", record.retry_count));
                    }
                });
            }

            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                if !display_text.trim().is_empty() {
                    let response = ui.add_sized([88.0, 44.0], egui::Button::new("Copy"));
                    set_accessible_description(
                        ui,
                        &response,
                        &format!("Copies the transcript for {entry_context}"),
                    );
                    if response.clicked() {
                        action = Some(HistoryPageAction::Copy {
                            text: display_text.to_owned(),
                            label: "Transcript",
                        });
                    }
                }
                if record.final_text.is_some()
                    && !record.raw_text.trim().is_empty()
                    && record.raw_text.trim() != display_text.trim()
                {
                    let response = ui.add_sized([96.0, 44.0], egui::Button::new("Copy raw"));
                    set_accessible_description(
                        ui,
                        &response,
                        &format!("Copies the raw transcript for {entry_context}"),
                    );
                    if response.clicked() {
                        action = Some(HistoryPageAction::Copy {
                            text: record.raw_text.clone(),
                            label: "Raw transcript",
                        });
                    }
                }
                if let Some(final_text) = record
                    .final_text
                    .as_deref()
                    .filter(|text| !text.trim().is_empty())
                {
                    let button_text = if armed_repaste == Some(record.id) {
                        "Paste armed"
                    } else {
                        "Paste again"
                    };
                    let response = ui.add_enabled(
                        !work_active && armed_repaste != Some(record.id),
                        egui::Button::new(button_text).min_size([112.0, 44.0].into()),
                    );
                    if armed_repaste == Some(record.id) {
                        ui.ctx().accesskit_node_builder(response.id, |builder| {
                            builder.set_description(
                                "Paste is already armed. Focus the destination and press the configured shortcut within 30 seconds.",
                            );
                        });
                    } else {
                        set_accessible_description(
                            ui,
                            &response,
                            &format!("Arms a one-time safe paste for {entry_context}"),
                        );
                    }
                    if response.clicked() {
                        action = Some(HistoryPageAction::ArmRepaste {
                            id: record.id,
                            text: final_text.to_owned(),
                        });
                    }
                }
                let pin = ui.add_enabled(
                    !work_active,
                    egui::Button::new(if record.pinned { "Unpin" } else { "Pin" })
                        .min_size([76.0, 44.0].into()),
                );
                set_accessible_description(
                    ui,
                    &pin,
                    &format!("Changes retention protection for {entry_context}"),
                );
                if pin.clicked() {
                    action = Some(HistoryPageAction::TogglePinned {
                        id: record.id,
                        pinned: !record.pinned,
                    });
                }
                if record.audio_path.is_some() {
                    if playing == Some(record.id) {
                        if playback_stopping {
                            let response = ui.add_enabled(
                                false,
                                egui::Button::new("Stopping…").min_size([104.0, 44.0].into()),
                            );
                            set_accessible_description(
                                ui,
                                &response,
                                &format!("Playback is stopping for {entry_context}"),
                            );
                        } else {
                            let response = ui.add_sized([88.0, 44.0], egui::Button::new("Stop"));
                            set_accessible_description(
                                ui,
                                &response,
                                &format!("Stops playback for {entry_context}"),
                            );
                            if response.clicked() {
                                action = Some(HistoryPageAction::StopPlayback);
                            }
                        }
                    } else {
                        let response = ui.add_enabled(
                            !work_active && playing.is_none(),
                            egui::Button::new("Play").min_size([88.0, 44.0].into()),
                        );
                        set_accessible_description(
                            ui,
                            &response,
                            &format!("Plays retained audio for {entry_context}"),
                        );
                        if response.clicked() {
                            action = Some(HistoryPageAction::Play(record.id));
                        }
                    }
                    if record.status == HistoryStatus::Failed {
                        let response = ui.add_enabled(
                            !work_active,
                            egui::Button::new("Retry").min_size([88.0, 44.0].into()),
                        );
                        set_accessible_description(
                            ui,
                            &response,
                            &format!("Retries without pasting for {entry_context}"),
                        );
                        if response.clicked() {
                            action = Some(HistoryPageAction::Retry(record.id));
                        }
                    }
                    let delete_audio = ui.add_enabled(
                        !work_active && playing != Some(record.id),
                        egui::Button::new("Delete audio").min_size([112.0, 44.0].into()),
                    );
                    set_accessible_description(
                        ui,
                        &delete_audio,
                        &format!("Deletes only retained audio for {entry_context}"),
                    );
                    if delete_audio.clicked() {
                        action = Some(HistoryPageAction::DeleteAudio(record.id));
                    }
                }
                let delete_entry = ui.add_enabled(
                    !work_active,
                    egui::Button::new("Delete entry").min_size([112.0, 44.0].into()),
                );
                set_accessible_description(
                    ui,
                    &delete_entry,
                    &format!("Requests permanent deletion of {entry_context}"),
                );
                if delete_entry.clicked() {
                    action = Some(HistoryPageAction::RequestDelete(record.id));
                }
            });

            if confirm_delete == Some(record.id) {
                ui.add_space(8.0);
                let prompt = ui.colored_label(
                    ui.visuals().warn_fg_color,
                    "Delete this transcript and any retained audio? This cannot be undone.",
                );
                ui.ctx().accesskit_node_builder(prompt.id, |builder| {
                    builder.set_role(egui::accesskit::Role::Alert);
                    builder.set_live(egui::accesskit::Live::Assertive);
                    builder.set_live_atomic();
                });
                ui.horizontal(|ui| {
                    let confirm = ui.add_enabled(
                        !work_active,
                        egui::Button::new("Delete permanently").min_size([144.0, 44.0].into()),
                    );
                    set_accessible_description(
                        ui,
                        &confirm,
                        &format!("Permanently deletes {entry_context}"),
                    );
                    if confirm.clicked() {
                        action = Some(HistoryPageAction::ConfirmDelete(record.id));
                    }
                    let cancel = ui.add_sized([88.0, 44.0], egui::Button::new("Cancel"));
                    set_accessible_description(
                        ui,
                        &cancel,
                        &format!("Cancels deletion of {entry_context}"),
                    );
                    if focus_delete_confirmation {
                        cancel.request_focus();
                    }
                    if cancel.clicked() {
                        action = Some(HistoryPageAction::CancelDelete);
                    }
                });
            }
            });
        });
        ui.add_space(12.0);
    }

    if has_more
        && ui
            .add_enabled(
                !loading,
                egui::Button::new(if loading { "Loading…" } else { "Load more" })
                    .min_size([120.0, 44.0].into()),
            )
            .clicked()
    {
        action = Some(HistoryPageAction::LoadMore);
    }
    action
}

fn history_status_label(status: HistoryStatus) -> &'static str {
    match status {
        HistoryStatus::Pending => "Pending",
        HistoryStatus::Completed => "Completed",
        HistoryStatus::Failed => "Failed",
    }
}

fn relative_time_label(created_at_ms: i64) -> String {
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let age_ms = now_ms.saturating_sub(created_at_ms.max(0) as u128);
    let seconds = age_ms / 1_000;
    if seconds < 60 {
        "Just now".to_owned()
    } else if seconds < 3_600 {
        format!("{} min ago", seconds / 60)
    } else if seconds < 86_400 {
        format!("{} hr ago", seconds / 3_600)
    } else {
        format!("{} days ago", seconds / 86_400)
    }
}

pub(crate) fn about_page(ui: &mut egui::Ui, model_dir: &Path, config_path: Option<&Path>) {
    semantic_heading(ui, RichText::new("Scribe").size(24.0).strong());
    ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
    ui.add_space(12.0);
    ui.label(RichText::new("Local-first privacy").strong());
    ui.label(
        "Microphone audio and speech preparation stay in native Rust workers. Scribe does not use a cloud speech service.",
    );
    ui.add_space(12.0);
    ui.label(RichText::new("Local paths").strong());
    ui.label(format!("Models: {}", model_dir.display()));
    ui.label(match config_path {
        Some(path) => format!("Settings: {}", path.display()),
        None => "Settings: platform path unavailable".to_owned(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::configure_accessible_style;

    fn state<'a>(search: &'a mut String, focus_search: bool) -> HistoryPageState<'a> {
        HistoryPageState {
            search,
            records: &[],
            has_more: false,
            loading: false,
            error: None,
            confirm_delete: None,
            work_active: false,
            playing: None,
            playback_stopping: false,
            armed_repaste: None,
            focus_search,
            focus_delete_confirmation: false,
        }
    }

    #[test]
    fn history_search_enter_applies_the_typed_query_once() {
        let ctx = egui::Context::default();
        configure_accessible_style(&ctx);
        let mut search = String::new();
        let raw = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(375.0, 240.0),
            )),
            focused: true,
            ..Default::default()
        };
        let _ = ctx.run(raw(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                assert_eq!(history_page(ui, state(&mut search, true)), None);
            });
        });
        let mut action = None;
        let _ = ctx.run(
            egui::RawInput {
                events: vec![
                    egui::Event::Text("meeting".into()),
                    egui::Event::Key {
                        key: egui::Key::Enter,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
                ..raw()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    action = history_page(ui, state(&mut search, false));
                });
            },
        );
        assert_eq!(search, "meeting");
        assert_eq!(action, Some(HistoryPageAction::ApplySearch));
    }

    #[test]
    fn history_search_actions_wrap_without_escaping_a_narrow_route() {
        for width in [120.0, 220.0] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            configure_accessible_style(&ctx);
            let mut search = "meeting".to_owned();
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width, 320.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        assert_eq!(history_page(ui, state(&mut search, false)), None);
                    });
                },
            );
            let nodes = &output
                .platform_output
                .accesskit_update
                .expect("history controls should update AccessKit")
                .nodes;
            for name in ["Search history", "Search", "Refresh"] {
                let node = nodes
                    .iter()
                    .find_map(|(_, node)| (node.name() == Some(name)).then_some(node))
                    .unwrap_or_else(|| panic!("missing narrow history control: {name}"));
                let bounds = node
                    .bounds()
                    .unwrap_or_else(|| panic!("{name} needs accessibility bounds"));
                assert!(
                    bounds.x0 >= 0.0
                        && bounds.x1 <= f64::from(width)
                        && bounds.y0 >= 0.0
                        && bounds.y1 <= 320.0,
                    "{name} must remain inside the narrow route at width {width}: {bounds:?}"
                );
                assert!(
                    bounds.width() >= 44.0 && bounds.height() >= 44.0,
                    "{name} must retain a 44px target at width {width}: {bounds:?}"
                );
            }
        }
    }
}
