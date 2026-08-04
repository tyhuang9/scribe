use eframe::egui::{self, Color32, Ui};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ThemePalette {
    pub shell_bg: Color32,
    pub content_bg: Color32,
    pub sidebar_bg: Color32,
    pub card_bg: Color32,
    pub panel_bg: Color32,
    pub active_card_bg: Color32,
    pub text: Color32,
    pub muted_text: Color32,
    pub border: Color32,
    pub border_strong: Color32,
    pub primary: Color32,
    pub accent: Color32,
    pub success: Color32,
    pub warning: Color32,
    pub error: Color32,
    pub primary_button_bg: Color32,
    pub primary_button_text: Color32,
}

impl ThemePalette {
    pub(crate) fn from_visuals(visuals: &egui::Visuals) -> Self {
        if visuals.dark_mode {
            Self::dark()
        } else {
            Self::light()
        }
    }

    pub(crate) fn light() -> Self {
        Self {
            shell_bg: Color32::from_rgb(247, 249, 251),
            content_bg: Color32::from_rgb(247, 249, 251),
            sidebar_bg: Color32::WHITE,
            card_bg: Color32::WHITE,
            panel_bg: Color32::from_rgb(248, 250, 252),
            active_card_bg: Color32::from_rgb(239, 246, 255),
            text: Color32::from_rgb(29, 33, 42),
            muted_text: Color32::from_rgb(85, 95, 109),
            border: Color32::from_rgb(226, 232, 240),
            border_strong: Color32::from_rgb(203, 213, 225),
            primary: Color32::from_rgb(6, 10, 18),
            accent: Color32::from_rgb(37, 99, 235),
            success: Color32::from_rgb(22, 163, 74),
            warning: Color32::from_rgb(202, 138, 4),
            error: Color32::from_rgb(220, 38, 38),
            primary_button_bg: Color32::from_rgb(6, 10, 18),
            primary_button_text: Color32::WHITE,
        }
    }

    pub(crate) fn dark() -> Self {
        Self {
            shell_bg: Color32::from_rgb(15, 18, 24),
            content_bg: Color32::from_rgb(15, 18, 24),
            sidebar_bg: Color32::from_rgb(20, 24, 32),
            card_bg: Color32::from_rgb(26, 31, 41),
            panel_bg: Color32::from_rgb(22, 27, 36),
            active_card_bg: Color32::from_rgb(25, 42, 68),
            text: Color32::from_rgb(236, 241, 247),
            muted_text: Color32::from_rgb(156, 166, 179),
            border: Color32::from_rgb(53, 61, 76),
            border_strong: Color32::from_rgb(76, 86, 104),
            primary: Color32::from_rgb(247, 250, 252),
            accent: Color32::from_rgb(96, 165, 250),
            success: Color32::from_rgb(74, 222, 128),
            warning: Color32::from_rgb(251, 191, 36),
            error: Color32::from_rgb(248, 113, 113),
            primary_button_bg: Color32::from_rgb(37, 99, 235),
            primary_button_text: Color32::WHITE,
        }
    }
}

pub(crate) fn theme_palette(ctx: &egui::Context) -> ThemePalette {
    ThemePalette::from_visuals(&ctx.style().visuals)
}

pub(crate) fn ui_palette(ui: &Ui) -> ThemePalette {
    ThemePalette::from_visuals(ui.visuals())
}
