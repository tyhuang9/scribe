use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use eframe::egui::{self, RichText};
use unicode_segmentation::UnicodeSegmentation;

use crate::history::{HistoryRecord, HistoryStatus};

use super::{
    controls::{card, search_field},
    theme::ui_palette,
};

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
    ToggleTranscript(i64),
    ToggleDetails(i64),
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
    pub model_names: &'a HashMap<String, String>,
    pub expanded_transcripts: &'a HashSet<i64>,
    pub expanded_details: &'a HashSet<i64>,
    pub focus_search: bool,
    pub focus_delete_confirmation: bool,
    pub focus_more_action: Option<i64>,
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
        model_names,
        expanded_transcripts,
        expanded_details,
        focus_search,
        focus_delete_confirmation,
        focus_more_action,
    } = state;
    let mut action = None;
    const HISTORY_ACTION_WIDTH: f32 = 86.0;
    const HISTORY_CONTROL_HEIGHT: f32 = 44.0;
    const MINIMUM_INLINE_SEARCH_WIDTH: f32 = 160.0;
    let row_spacing = ui.spacing().item_spacing.x;
    let single_row_minimum = MINIMUM_INLINE_SEARCH_WIDTH + HISTORY_ACTION_WIDTH + row_spacing;
    let single_row = ui.available_width() >= single_row_minimum;
    let mut search_response = None;
    ui.vertical(|ui| {
        if single_row {
            ui.horizontal(|ui| {
                let search_width =
                    ui.available_width() - HISTORY_ACTION_WIDTH - ui.spacing().item_spacing.x;
                search_response = Some(search_field(
                    ui,
                    search_width,
                    "history-search",
                    search,
                    "Search history",
                    "Search transcript, model, or app",
                    "Filters saved history as you type.",
                ));
                let refresh_clicked = ui
                    .add_sized(
                        [HISTORY_ACTION_WIDTH, HISTORY_CONTROL_HEIGHT],
                        egui::Button::new("Refresh"),
                    )
                    .clicked();
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
                "Filters saved history as you type.",
            ));
            ui.add_space(ui.spacing().item_spacing.y);
            // Keep Refresh on a second row before it can force the search
            // field below its usable width. On very narrow routes the wrapped
            // layout keeps the 44px control contained.
            let action_width =
                HISTORY_ACTION_WIDTH.min(ui.available_width().max(HISTORY_CONTROL_HEIGHT));
            ui.horizontal_wrapped(|ui| {
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
    if search_response.changed {
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
    let results_id = ui.make_persistent_id("history-results");
    ui.ctx().accesskit_node_builder(results_id, |builder| {
        builder.set_role(egui::accesskit::Role::Group);
        builder.set_name("History results");
    });
    let ctx = ui.ctx().clone();
    ctx.with_accessibility_parent(results_id, || {
    if loading && records.is_empty() {
        ui.spinner();
        ui.label("Loading local history…");
    } else if records.is_empty() {
        ui.group(|ui| {
            ui.label(RichText::new("No matching history entries").strong());
            ui.label(
                "Completed and failed dictations appear here when history storage is enabled.",
            );
        });
    } else {
    for record in records {
        let time_label = relative_time_label(record.created_at_ms);
        let entry_context = format!(
            "{} history entry, {}, model {}",
            history_status_label(record.status),
            time_label,
            record.model_id
        );
        let display_text = record
            .final_text
            .as_deref()
            .filter(|text| !text.trim().is_empty())
            .unwrap_or(record.raw_text.as_str());
        let raw_is_distinct = record
            .final_text
            .as_deref()
            .is_some_and(|final_text| {
                !record.raw_text.trim().is_empty()
                    && record.raw_text.trim() != final_text.trim()
            });
        let (preview, is_truncated) = transcript_preview(display_text);
        let show_full_transcript = expanded_transcripts.contains(&record.id);
        let details_visible = expanded_details.contains(&record.id);
        let model_name = model_names
            .get(&record.model_id)
            .map(String::as_str)
            .unwrap_or(record.model_id.as_str());

        card(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                semantic_heading(
                    ui,
                    RichText::new(format!(
                        "{} — {}",
                        history_status_label(record.status),
                        time_label
                    ))
                    .color(history_status_color(ui, record.status))
                    .strong(),
                );
                if let Some(source_app) = record.source_app.as_deref() {
                    ui.label(RichText::new(source_app).color(ui.visuals().weak_text_color()));
                }
                if record.pinned {
                    ui.label(
                        RichText::new(format!("{} Pinned", egui_phosphor::regular::PUSH_PIN))
                            .color(ui.visuals().weak_text_color()),
                    );
                }
            });

            ui.add_space(6.0);
            if display_text.trim().is_empty() {
                ui.label(match record.status {
                    HistoryStatus::Pending => "Transcription is pending.",
                    HistoryStatus::Failed => "No transcript was produced.",
                    HistoryStatus::Completed => "The completed transcript is empty.",
                });
            } else {
                ui.label(if show_full_transcript {
                    display_text
                } else {
                    preview.as_str()
                });
            }
            if is_truncated {
                let response = ui.add_sized(
                    [164.0, 44.0],
                    egui::Button::new(if show_full_transcript {
                        "Show less"
                    } else {
                        "Show full transcript"
                    }),
                );
                set_accessible_description(
                    ui,
                    &response,
                    &format!(
                        "{} for {entry_context}",
                        if show_full_transcript {
                            "Shows the shortened transcript"
                        } else {
                            "Shows the complete transcript"
                        }
                    ),
                );
                if response.clicked() {
                    action = Some(HistoryPageAction::ToggleTranscript(record.id));
                }
            }
            if let Some(failure) = record.failure.as_deref() {
                ui.colored_label(ui.visuals().error_fg_color, failure);
            }

            if details_visible {
                ui.add_space(8.0);
                ui.separator();
                ui.add_space(8.0);
                ui.label(RichText::new("Details").strong());
                ui.label(format!("Model: {model_name}"));
                if let Some(output_outcome) = record.output_outcome.as_deref() {
                    ui.label(format!("Output: {}", output_outcome.replace('_', " ")));
                }
                ui.horizontal_wrapped(|ui| {
                    if let Some(duration) = record.metrics.audio_duration_ms {
                        ui.label(format!("Audio: {}", format_duration(duration)));
                    }
                    if let Some(duration) = record.metrics.processing_duration_ms {
                        ui.label(format!("Processing: {}", format_duration(duration)));
                    }
                    if let Some(rtf) = record.metrics.realtime_factor {
                        ui.label(format!("RTF: {rtf:.2}"));
                    }
                    if record.retry_count > 0 {
                        ui.label(format!("Retries: {}", record.retry_count));
                    }
                });
                if raw_is_distinct {
                    ui.add_space(4.0);
                    ui.label(RichText::new("Raw transcript").strong());
                    ui.label(&record.raw_text);
                    let copy_raw = ui.add_sized([112.0, 44.0], egui::Button::new("Copy raw"));
                    set_accessible_description(
                        ui,
                        &copy_raw,
                        &format!("Copies the raw transcript for {entry_context}"),
                    );
                    if copy_raw.clicked() {
                        action = Some(HistoryPageAction::Copy {
                            text: record.raw_text.clone(),
                            label: "Raw transcript",
                        });
                    }
                }
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
                            builder.set_description(format!(
                                "Paste is already armed for {entry_context}. Focus the destination and press the configured shortcut within 30 seconds."
                            ));
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
                if record.audio_path.is_some() {
                    let (label, enabled, description, page_action) = if playing == Some(record.id) {
                        if playback_stopping {
                            (
                                "Stopping…",
                                false,
                                format!("Playback is stopping for {entry_context}"),
                                None,
                            )
                        } else {
                            (
                                "Stop",
                                true,
                                format!("Stops playback for {entry_context}"),
                                Some(HistoryPageAction::StopPlayback),
                            )
                        }
                    } else {
                        (
                            "Play",
                            !work_active && playing.is_none(),
                            format!("Plays retained audio for {entry_context}"),
                            Some(HistoryPageAction::Play(record.id)),
                        )
                    };
                    let response = ui.add_enabled(
                        enabled,
                        egui::Button::new(label).min_size([88.0, 44.0].into()),
                    );
                    set_accessible_description(ui, &response, &description);
                    if response.clicked()
                        && let Some(page_action) = page_action
                    {
                        action = Some(page_action);
                    }
                }

                let more = ui.menu_button("More actions", |ui| {
                        let pin = ui.add_enabled(
                            !work_active,
                            egui::Button::new(if record.pinned { "Unpin" } else { "Pin" })
                                .min_size([112.0, 44.0].into()),
                        );
                        if pin.clicked() {
                            action = Some(HistoryPageAction::TogglePinned {
                                id: record.id,
                                pinned: !record.pinned,
                            });
                            ui.close_menu();
                        }
                        let details = ui.add_sized(
                            [144.0, 44.0],
                            egui::Button::new(if details_visible {
                                "Hide details"
                            } else {
                                "Show details"
                            }),
                        );
                        if details.clicked() {
                            action = Some(HistoryPageAction::ToggleDetails(record.id));
                            ui.close_menu();
                        }
                        if record.status == HistoryStatus::Failed && record.audio_path.is_some() {
                            let retry = ui.add_enabled(
                                !work_active,
                                egui::Button::new("Retry").min_size([112.0, 44.0].into()),
                            );
                            if retry.clicked() {
                                action = Some(HistoryPageAction::Retry(record.id));
                                ui.close_menu();
                            }
                        }
                        if record.audio_path.is_some() {
                            let delete_audio = ui.add_enabled(
                                !work_active && playing != Some(record.id),
                                egui::Button::new("Delete retained audio")
                                    .min_size([176.0, 44.0].into()),
                            );
                            if delete_audio.clicked() {
                                action = Some(HistoryPageAction::DeleteAudio(record.id));
                                ui.close_menu();
                            }
                        }
                        ui.separator();
                        let delete_entry = ui.add_enabled(
                            !work_active,
                            egui::Button::new("Delete entry").min_size([144.0, 44.0].into()),
                        );
                        if delete_entry.clicked() {
                            action = Some(HistoryPageAction::RequestDelete(record.id));
                            ui.close_menu();
                        }
                    },
                );
                set_accessible_description(
                    ui,
                    &more.response,
                    &format!("Shows additional actions for {entry_context}"),
                );
                if focus_more_action == Some(record.id) {
                    more.response.request_focus();
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
                ui.horizontal_wrapped(|ui| {
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
    }
    });
    action
}

fn history_status_label(status: HistoryStatus) -> &'static str {
    match status {
        HistoryStatus::Pending => "Pending",
        HistoryStatus::Completed => "Completed",
        HistoryStatus::Failed => "Failed",
    }
}

fn history_status_color(ui: &egui::Ui, status: HistoryStatus) -> egui::Color32 {
    match status {
        HistoryStatus::Pending => ui.visuals().warn_fg_color,
        HistoryStatus::Completed => ui_palette(ui).success_text,
        HistoryStatus::Failed => ui.visuals().error_fg_color,
    }
}

fn transcript_preview(transcript: &str) -> (String, bool) {
    const PREVIEW_GRAPHEME_LIMIT: usize = 240;
    let mut graphemes = transcript.graphemes(true);
    let preview = graphemes
        .by_ref()
        .take(PREVIEW_GRAPHEME_LIMIT)
        .collect::<String>();
    let truncated = graphemes.next().is_some();
    (
        if truncated {
            format!("{preview}…")
        } else {
            preview
        },
        truncated,
    )
}

fn format_duration(duration_ms: u64) -> String {
    if duration_ms < 1_000 {
        format!("{duration_ms} ms")
    } else {
        format!("{:.1} s", duration_ms as f64 / 1_000.0)
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

    fn render_history(
        ctx: &egui::Context,
        search: &mut String,
        focus_search: bool,
        focus_before_render: Option<egui::Id>,
        events: Vec<egui::Event>,
        width: f32,
    ) -> (egui::FullOutput, Option<HistoryPageAction>, egui::Id) {
        let mut action = None;
        let mut search_id = egui::Id::NULL;
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, 320.0),
                )),
                focused: true,
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    search_id = ui.make_persistent_id("history-search");
                    if let Some(id) = focus_before_render {
                        ui.memory_mut(|memory| memory.request_focus(id));
                    }
                    action = history_page(ui, state(search, focus_search));
                });
            },
        );
        (output, action, search_id)
    }

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
            model_names: &HashMap::new(),
            expanded_transcripts: &HashSet::new(),
            expanded_details: &HashSet::new(),
            focus_search,
            focus_delete_confirmation: false,
            focus_more_action: None,
        }
    }

    #[test]
    fn history_search_applies_the_typed_query_once_as_it_is_edited() {
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
                events: vec![egui::Event::Text("meeting".into())],
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

        let _ = ctx.run(raw(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                action = history_page(ui, state(&mut search, false));
            });
        });
        assert_eq!(
            action, None,
            "an unchanged live-search query must not trigger a second reload"
        );
    }

    #[test]
    fn history_search_and_refresh_wrap_without_escaping_a_narrow_route() {
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
            for name in ["Search history", "Refresh"] {
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
            assert!(
                nodes.iter().all(|(_, node)| node.name() != Some("Search")),
                "History filters as it is edited and must not expose a redundant Search button"
            );
        }
    }

    #[test]
    fn history_pointer_and_keyboard_clear_return_focus_exactly_once() {
        #[derive(Clone, Copy, Debug)]
        enum Activation {
            Click,
            Key(egui::Key),
        }

        for activation in [
            Activation::Click,
            Activation::Key(egui::Key::Enter),
            Activation::Key(egui::Key::Space),
        ] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            configure_accessible_style(&ctx);
            let mut search = "meeting".to_owned();
            let (initial, _, _) = render_history(&ctx, &mut search, false, None, Vec::new(), 375.0);
            let initial_nodes = &initial
                .platform_output
                .accesskit_update
                .as_ref()
                .expect("history controls should update AccessKit")
                .nodes;
            let node_id = |name: &str| {
                initial_nodes
                    .iter()
                    .find_map(|(id, node)| (node.name() == Some(name)).then_some(*id))
                    .unwrap_or_else(|| panic!("missing History control: {name}"))
            };
            let clear_node = node_id("Clear Search history");
            let action = match activation {
                Activation::Click => {
                    let bounds = initial
                        .platform_output
                        .accesskit_update
                        .as_ref()
                        .expect("history controls should update AccessKit")
                        .nodes
                        .iter()
                        .find_map(|(_, node)| {
                            (node.name() == Some("Clear Search history")).then(|| node.bounds())
                        })
                        .flatten()
                        .expect("populated history search needs a clear target");
                    let point = egui::pos2(
                        ((bounds.x0 + bounds.x1) / 2.0) as f32,
                        ((bounds.y0 + bounds.y1) / 2.0) as f32,
                    );
                    let (_, pressed, _) = render_history(
                        &ctx,
                        &mut search,
                        false,
                        None,
                        vec![
                            egui::Event::PointerMoved(point),
                            egui::Event::PointerButton {
                                pos: point,
                                button: egui::PointerButton::Primary,
                                pressed: true,
                                modifiers: egui::Modifiers::NONE,
                            },
                        ],
                        375.0,
                    );
                    assert_eq!(pressed, None, "pointer press must not clear early");
                    render_history(
                        &ctx,
                        &mut search,
                        false,
                        None,
                        vec![
                            egui::Event::PointerMoved(point),
                            egui::Event::PointerButton {
                                pos: point,
                                button: egui::PointerButton::Primary,
                                pressed: false,
                                modifiers: egui::Modifiers::NONE,
                            },
                        ],
                        375.0,
                    )
                    .1
                }
                Activation::Key(key) => {
                    let (_, focus_action, _) = render_history(
                        &ctx,
                        &mut search,
                        false,
                        None,
                        vec![egui::Event::AccessKitActionRequest(
                            egui::accesskit::ActionRequest {
                                action: egui::accesskit::Action::Focus,
                                target: clear_node,
                                data: None,
                            },
                        )],
                        375.0,
                    );
                    assert_eq!(focus_action, None);
                    let (_, settled_action, _) =
                        render_history(&ctx, &mut search, false, None, Vec::new(), 375.0);
                    assert_eq!(settled_action, None);
                    render_history(
                        &ctx,
                        &mut search,
                        false,
                        None,
                        vec![egui::Event::Key {
                            key,
                            physical_key: None,
                            pressed: true,
                            repeat: false,
                            modifiers: egui::Modifiers::NONE,
                        }],
                        375.0,
                    )
                    .1
                }
            };

            assert_eq!(
                action,
                Some(HistoryPageAction::ClearSearch),
                "{activation:?} must clear History"
            );
            let mut clear_count = 1;
            search.clear();
            let (focused, next_action, _) =
                render_history(&ctx, &mut search, true, None, Vec::new(), 375.0);
            assert_eq!(next_action, None);
            let update = focused
                .platform_output
                .accesskit_update
                .as_ref()
                .expect("focused history search should update AccessKit");
            let focused_node = update
                .nodes
                .iter()
                .find_map(|(id, node)| (*id == update.focus).then_some(node))
                .expect("focused History node should be present");
            assert_eq!(focused_node.name(), Some("Search history"));
            let (_, repeated_action, _) =
                render_history(&ctx, &mut search, false, None, Vec::new(), 375.0);
            if repeated_action == Some(HistoryPageAction::ClearSearch) {
                clear_count += 1;
            }
            assert_eq!(clear_count, 1, "a clear gesture must be handled once");
        }
    }

    #[test]
    fn history_inline_controls_fit_at_the_minimum_search_width_threshold() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut search = "meeting".to_owned();
        // CentralPanel contributes 8px on each side, leaving exactly
        // 160px search + 8px gap + 86px Refresh in the route.
        let width = 270.0;
        let (output, action, _) = render_history(&ctx, &mut search, false, None, Vec::new(), width);
        assert_eq!(action, None);
        let nodes = &output
            .platform_output
            .accesskit_update
            .expect("history controls should update AccessKit")
            .nodes;
        let bounds = |name: &str| {
            nodes
                .iter()
                .find_map(|(_, node)| (node.name() == Some(name)).then(|| node.bounds()))
                .flatten()
                .unwrap_or_else(|| panic!("missing bounds for {name}"))
        };
        let search_bounds = bounds("Search history");
        let clear_bounds = bounds("Clear Search history");
        let refresh_bounds = bounds("Refresh");
        assert_eq!(search_bounds.y0, refresh_bounds.y0);
        assert_eq!(clear_bounds.y0, refresh_bounds.y0);
        assert!(clear_bounds.x1 < refresh_bounds.x0);
        assert!(refresh_bounds.x1 <= f64::from(width));
    }

    #[test]
    fn transcript_preview_limits_by_grapheme_without_splitting_combining_text() {
        let transcript = "e\u{301}".repeat(241);
        let (preview, truncated) = transcript_preview(&transcript);

        assert!(truncated);
        assert!(preview.ends_with('…'));
        let visible = preview
            .strip_suffix('…')
            .expect("preview should have an ellipsis");
        assert_eq!(visible.graphemes(true).count(), 240);
        assert_eq!(visible, "e\u{301}".repeat(240));
    }

    #[test]
    fn expanded_history_details_expose_friendly_metadata_and_raw_copy() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let record = HistoryRecord {
            id: 7,
            created_at_ms: 1,
            updated_at_ms: 1,
            completed_at_ms: Some(1),
            status: HistoryStatus::Completed,
            raw_text: "raw transcript".to_owned(),
            final_text: Some("clean transcript".to_owned()),
            model_id: "whisper_cpp_base_en".to_owned(),
            metrics: crate::history::HistoryMetrics {
                audio_duration_ms: Some(1_250),
                processing_duration_ms: Some(500),
                realtime_factor: Some(0.4),
            },
            pinned: true,
            source_app: Some("Notes".to_owned()),
            audio_path: None,
            failure: None,
            retry_count: 2,
            output_outcome: Some("pasted_safely".to_owned()),
        };
        let model_names = HashMap::from([(
            "whisper_cpp_base_en".to_owned(),
            "Whisper Base — English".to_owned(),
        )]);
        let expanded_details = HashSet::from([record.id]);
        let mut search = String::new();
        let output = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                history_page(
                    ui,
                    HistoryPageState {
                        search: &mut search,
                        records: std::slice::from_ref(&record),
                        has_more: false,
                        loading: false,
                        error: None,
                        confirm_delete: None,
                        work_active: false,
                        playing: None,
                        playback_stopping: false,
                        armed_repaste: None,
                        model_names: &model_names,
                        expanded_transcripts: &HashSet::new(),
                        expanded_details: &expanded_details,
                        focus_search: false,
                        focus_delete_confirmation: false,
                        focus_more_action: None,
                    },
                );
            });
        });
        let nodes = &output
            .platform_output
            .accesskit_update
            .expect("expanded details should update AccessKit")
            .nodes;
        for expected in [
            "Model: Whisper Base — English",
            "Output: pasted safely",
            "Audio: 1.2 s",
            "Processing: 500 ms",
            "Retries: 2",
            "Raw transcript",
            "Copy raw",
            "More actions",
        ] {
            assert!(
                nodes.iter().any(|(_, node)| node.name() == Some(expected)),
                "missing expanded History detail: {expected}"
            );
        }
    }
}
