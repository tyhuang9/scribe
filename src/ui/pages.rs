use std::path::Path;

use eframe::egui::{self, Align, Layout, RichText};

pub(crate) fn history_page(ui: &mut egui::Ui, transcript: &str) -> bool {
    ui.heading("Session transcript");
    ui.label(
        "Persistent history is not active yet. This view shows only the finalized transcript held by the current app session.",
    );
    ui.add_space(12.0);
    if transcript.trim().is_empty() {
        ui.group(|ui| {
            ui.label(RichText::new("No finalized transcript in this session").strong());
            ui.label("Finish a dictation to make its final text available here.");
        });
        return false;
    }

    ui.group(|ui| {
        ui.label(RichText::new("Latest finalized text").strong());
        ui.add_space(6.0);
        ui.label(transcript);
        ui.add_space(8.0);
        let mut copy = false;
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            copy = ui
                .add_sized([88.0, 44.0], egui::Button::new("Copy"))
                .clicked();
        });
        copy
    })
    .inner
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
