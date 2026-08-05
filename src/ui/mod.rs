#[allow(dead_code)]
mod controls;
#[cfg(all(feature = "ui-harness", debug_assertions))]
mod harness;
mod pages;
#[allow(dead_code)]
mod screens;
mod shell;
#[allow(dead_code)]
mod state;
mod theme;

pub(crate) use controls::{configure_accessible_style, minimum_primary_target_height};
pub(crate) use pages::{HistoryPageAction, HistoryPageState, about_page, history_page};
pub(crate) use shell::{AppPage, show_navigation};
pub(crate) use theme::{ThemePalette, theme_palette, ui_palette};

#[cfg(all(feature = "ui-harness", debug_assertions))]
pub(crate) use harness::{UiHarnessApp, fixture_from_env};
