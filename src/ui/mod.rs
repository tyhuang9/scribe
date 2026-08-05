mod controls;
mod pages;
mod shell;
mod state;
mod theme;

pub(crate) use controls::{configure_accessible_style, minimum_primary_target_height};
pub(crate) use pages::{HistoryPageAction, HistoryPageState, about_page, history_page};
pub(crate) use shell::{AppPage, show_navigation};
pub(crate) use state::{
    ComparisonPhase, ModelCapabilities, ModelComparisonState, ModelDownloadState, ModelViewModel,
    SettingsSaveState, SettingsTab, TranscriptionEvent, TranscriptionPhase, TranscriptionState,
    UiRoute,
};
pub(crate) use theme::{ThemePalette, theme_palette, ui_palette};
