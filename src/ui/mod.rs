mod controls;
#[cfg(all(feature = "ui-harness", debug_assertions))]
mod harness;
mod model_picker;
mod pages;
mod production;
mod screens;
mod shell;
mod state;
mod theme;

pub(crate) use controls::{configure_accessible_style, minimum_primary_target_height};
pub(crate) use model_picker::ReadyModelPickerAction;
pub(crate) use pages::{HistoryPageAction, HistoryPageState, about_page, history_page};
pub(crate) use production::{
    ModelReadiness, acceleration_diagnostics, recording_mode, settings_save_state,
    transcription_state,
};
pub(crate) use screens::{
    RecordingSettingsView, ScreenAction, ScreenView, render_screen,
    request_models_route_heading_focus, scroll_focused_control_into_view, show_route_scroll,
};
pub(crate) use shell::{AppPage, SidebarModelView, show_navigation};
pub(crate) use state::{
    AccelerationDiagnosticsView, ComparisonPhase, ComparisonResult, ComparisonResultPhase,
    LocalGgufImportView, MicrophonePermission, ModelCapabilities, ModelCardKey,
    ModelComparisonState, ModelCompatibility, ModelDialog, ModelDownloadState, ModelLanguageFilter,
    ModelManagementState, ModelSizeTier, ModelSpeedTier, ModelViewModel, RecordingMode,
    RemoteCatalogActionKind, RemoteCatalogActionView, RemoteCatalogEntryView, RemoteCatalogFilters,
    RemoteCatalogSort, RemoteCatalogStatusKind, RemoteCatalogStatusView, RemoteCatalogVariantView,
    RemoteCatalogView, ResolvedTheme, SettingsTab, TranscribeNotice, TranscribeRecoveryAction,
    UiRoute,
};

#[cfg(test)]
pub(crate) use state::RemoteCatalogSizeTier;
pub(crate) use theme::{ThemePalette, theme_palette, ui_palette};

#[cfg(all(feature = "ui-harness", debug_assertions))]
pub(crate) use harness::{Fixture, UiHarnessApp, fixture_from_env};
