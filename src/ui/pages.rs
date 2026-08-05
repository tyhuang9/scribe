use std::path::Path;

use eframe::egui::{self, Align, Layout, RichText};

fn semantic_heading(ui: &mut egui::Ui, text: RichText) -> egui::Response {
    let response = ui.label(text);
    ui.ctx().accesskit_node_builder(response.id, |builder| {
        builder.set_role(egui::accesskit::Role::Heading);
    });
    response
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum HistoryPageAction {
    CopyFinal,
    CopyRaw,
}

pub(crate) fn history_page(
    ui: &mut egui::Ui,
    transcript: &str,
    raw_transcript: &str,
) -> Option<HistoryPageAction> {
    ui.heading("Session transcript");
    ui.label(
        "Persistent history is not active yet. This view shows the finalized transcript and, when different, preserved raw model text from the current app session.",
    );
    ui.add_space(12.0);
    if transcript.trim().is_empty() {
        ui.group(|ui| {
            ui.label(RichText::new("No finalized transcript in this session").strong());
            ui.label("Finish a dictation to make its final text available here.");
        });
        return None;
    }

    let copy_final = ui
        .group(|ui| {
            semantic_heading(ui, RichText::new("Latest finalized text").strong());
            ui.add_space(6.0);
            ui.label(transcript);
            ui.add_space(8.0);
            let mut copy = false;
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                copy = ui
                    .add_sized(
                        [184.0, 44.0],
                        egui::Button::new("Copy finalized transcript"),
                    )
                    .clicked();
            });
            copy
        })
        .inner;
    if copy_final {
        return Some(HistoryPageAction::CopyFinal);
    }

    if !raw_transcript.trim().is_empty() && raw_transcript != transcript {
        ui.add_space(12.0);
        let copy_raw = ui
            .group(|ui| {
                semantic_heading(ui, RichText::new("Raw model text").strong());
                ui.label("Preserved before local cleanup.");
                ui.add_space(6.0);
                ui.label(raw_transcript);
                ui.add_space(8.0);
                let mut copy = false;
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    copy = ui
                        .add_sized([156.0, 44.0], egui::Button::new("Copy raw transcript"))
                        .clicked();
                });
                copy
            })
            .inner;
        if copy_raw {
            return Some(HistoryPageAction::CopyRaw);
        }
    }
    None
}

pub(crate) fn about_page(ui: &mut egui::Ui, model_dir: &Path, config_path: Option<&Path>) {
    ui.heading("Scribe");
    ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
    ui.add_space(12.0);
    ui.group(|ui| {
        ui.label(RichText::new("Local-first privacy").strong());
        ui.label(
            "Microphone audio and speech preparation stay in native Rust workers. Scribe does not use a cloud speech service.",
        );
    });
    ui.add_space(12.0);
    ui.group(|ui| {
        ui.label(RichText::new("Local paths").strong());
        ui.label(format!("Models: {}", model_dir.display()));
        ui.label(match config_path {
            Some(path) => format!("Settings: {}", path.display()),
            None => "Settings: platform path unavailable".to_owned(),
        });
    });
}
