mod controls;
#[cfg(all(feature = "ui-harness", debug_assertions))]
mod harness;
mod pages;
mod production;
mod screens;
mod shell;
mod state;
mod theme;

pub(crate) use controls::{configure_accessible_style, minimum_primary_target_height};
pub(crate) use pages::{HistoryPageAction, HistoryPageState, about_page, history_page};
pub(crate) use production::{
    ModelReadiness, recording_mode, settings_save_state, transcription_state,
};
pub(crate) use screens::{
    RecordingSettingsView, ScreenAction, ScreenView, render_screen,
    scroll_focused_control_into_view, show_route_scroll,
};
pub(crate) use shell::{AppPage, show_navigation};
pub(crate) use state::{
    ComparisonPhase, ComparisonResult, ComparisonResultPhase, LocalGgufImportView,
    MicrophonePermission, ModelCapabilities, ModelComparisonState, ModelCompatibility, ModelDialog,
    ModelDownloadState, ModelLanguageFilter, ModelManagementState, ModelSizeTier, ModelSpeedTier,
    ModelViewModel, RecordingMode, RemoteCatalogActionKind, RemoteCatalogActionView,
    RemoteCatalogEntryView, RemoteCatalogFilters, RemoteCatalogSort, RemoteCatalogStatusKind,
    RemoteCatalogStatusView, RemoteCatalogVariantView, RemoteCatalogView, SettingsTab, UiRoute,
};

#[cfg(test)]
pub(crate) use state::RemoteCatalogSizeTier;
pub(crate) use theme::{ThemePalette, theme_palette, ui_palette};

#[cfg(all(feature = "ui-harness", debug_assertions))]
pub(crate) use harness::{UiHarnessApp, fixture_from_env};
