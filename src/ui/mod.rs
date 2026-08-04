mod controls;
mod pages;
mod shell;
mod theme;

pub(crate) use controls::{configure_accessible_style, minimum_primary_target_height};
pub(crate) use pages::{about_page, history_page};
pub(crate) use shell::{AppPage, show_navigation};
pub(crate) use theme::{ThemePalette, theme_palette, ui_palette};
