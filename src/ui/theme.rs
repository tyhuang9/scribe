use eframe::egui::{self, Color32, Ui};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ThemePalette {
    pub shell_bg: Color32,
    pub content_bg: Color32,
    pub sidebar_bg: Color32,
    pub card_bg: Color32,
    pub panel_bg: Color32,
    pub disabled_bg: Color32,
    pub active_card_bg: Color32,
    pub text: Color32,
    pub muted_text: Color32,
    pub tertiary_text: Color32,
    pub border: Color32,
    pub border_strong: Color32,
    pub slider_track_border: Color32,
    pub slider_threshold_fill: Color32,
    pub slider_remainder_fill: Color32,
    pub slider_live_below: Color32,
    pub slider_live_above: Color32,
    pub primary: Color32,
    pub accent: Color32,
    /// Purple track for the compact recording-mode selector.
    pub segmented_control_bg: Color32,
    /// Foreground for the selected segment on the current card surface.
    pub segmented_control_selected_text: Color32,
    /// Package green for fills and dots; use `success_text` for text on light surfaces.
    pub success: Color32,
    pub success_text: Color32,
    pub warning: Color32,
    pub error: Color32,
    /// Accessible error copy on `error_pale`.
    pub error_text: Color32,
    /// Foreground used on the solid danger fill.
    pub danger_button_text: Color32,
    pub error_pale: Color32,
    pub error_border: Color32,
    pub neutral_notice_text: Color32,
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
            shell_bg: Color32::from_rgb(230, 232, 234),
            content_bg: Color32::from_rgb(247, 249, 251),
            sidebar_bg: Color32::WHITE,
            card_bg: Color32::WHITE,
            panel_bg: Color32::from_rgb(248, 250, 252),
            disabled_bg: Color32::from_rgb(242, 244, 247),
            active_card_bg: Color32::from_rgb(239, 246, 255),
            text: Color32::from_rgb(29, 33, 42),
            muted_text: Color32::from_rgb(85, 95, 109),
            tertiary_text: Color32::from_rgb(89, 101, 118),
            border: Color32::from_rgb(226, 232, 240),
            border_strong: Color32::from_rgb(203, 213, 225),
            slider_track_border: Color32::from_rgb(116, 128, 145),
            slider_threshold_fill: Color32::from_rgb(147, 197, 253),
            slider_remainder_fill: Color32::from_rgb(226, 232, 240),
            slider_live_below: Color32::from_rgb(29, 78, 216),
            slider_live_above: Color32::from_rgb(6, 118, 71),
            primary: Color32::from_rgb(6, 10, 18),
            accent: Color32::from_rgb(37, 99, 235),
            segmented_control_bg: Color32::from_rgb(124, 58, 237),
            segmented_control_selected_text: Color32::from_rgb(109, 40, 217),
            success: Color32::from_rgb(18, 183, 106),
            success_text: Color32::from_rgb(6, 118, 71),
            warning: Color32::from_rgb(146, 64, 14),
            error: Color32::from_rgb(217, 45, 32),
            error_text: Color32::from_rgb(151, 27, 31),
            danger_button_text: Color32::WHITE,
            error_pale: Color32::from_rgb(254, 228, 226),
            error_border: Color32::from_rgb(253, 162, 155),
            neutral_notice_text: Color32::from_rgb(71, 84, 103),
            primary_button_bg: Color32::from_rgb(23, 27, 36),
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
            disabled_bg: Color32::from_rgb(36, 42, 54),
            active_card_bg: Color32::from_rgb(25, 42, 68),
            text: Color32::from_rgb(236, 241, 247),
            muted_text: Color32::from_rgb(156, 166, 179),
            tertiary_text: Color32::from_rgb(135, 146, 160),
            border: Color32::from_rgb(53, 61, 76),
            border_strong: Color32::from_rgb(76, 86, 104),
            slider_track_border: Color32::from_rgb(112, 126, 147),
            slider_threshold_fill: Color32::from_rgb(30, 64, 108),
            slider_remainder_fill: Color32::from_rgb(53, 61, 76),
            slider_live_below: Color32::from_rgb(96, 165, 250),
            slider_live_above: Color32::from_rgb(74, 222, 128),
            primary: Color32::from_rgb(247, 250, 252),
            accent: Color32::from_rgb(96, 165, 250),
            segmented_control_bg: Color32::from_rgb(124, 58, 237),
            segmented_control_selected_text: Color32::from_rgb(196, 181, 253),
            success: Color32::from_rgb(74, 222, 128),
            success_text: Color32::from_rgb(134, 239, 172),
            warning: Color32::from_rgb(251, 191, 36),
            error: Color32::from_rgb(248, 113, 113),
            error_text: Color32::from_rgb(254, 202, 202),
            danger_button_text: Color32::from_rgb(17, 19, 24),
            error_pale: Color32::from_rgb(77, 33, 36),
            error_border: Color32::from_rgb(153, 62, 65),
            neutral_notice_text: Color32::from_rgb(196, 205, 217),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_semantic_text_tokens_meet_aa_on_white() {
        let palette = ThemePalette::light();

        assert!(contrast_ratio(palette.success_text, palette.card_bg) >= 4.5);
        assert!(contrast_ratio(palette.warning, palette.card_bg) >= 4.5);
        assert!(contrast_ratio(palette.error_text, palette.error_pale) >= 4.5);
        assert!(contrast_ratio(palette.danger_button_text, palette.error) >= 4.5);
        assert!(contrast_ratio(palette.tertiary_text, palette.card_bg) >= 4.5);
        assert!(contrast_ratio(palette.tertiary_text, palette.content_bg) >= 4.5);
    }

    #[test]
    fn dark_danger_notice_and_button_foregrounds_meet_aa() {
        let palette = ThemePalette::dark();

        assert!(contrast_ratio(palette.error_text, palette.error_pale) >= 4.5);
        assert!(contrast_ratio(palette.danger_button_text, palette.error) >= 4.5);
    }

    #[test]
    fn recording_mode_toggle_colours_meet_aa_in_both_themes() {
        for palette in [ThemePalette::light(), ThemePalette::dark()] {
            assert!(
                contrast_ratio(palette.primary_button_text, palette.segmented_control_bg) >= 4.5
            );
            assert!(
                contrast_ratio(palette.segmented_control_selected_text, palette.card_bg) >= 4.5
            );
        }
    }

    #[test]
    fn live_meter_fill_has_non_text_contrast_against_both_track_regions() {
        for palette in [ThemePalette::light(), ThemePalette::dark()] {
            assert!(
                contrast_ratio(palette.slider_live_above, palette.slider_threshold_fill) >= 3.0
            );
            assert!(
                contrast_ratio(palette.slider_live_above, palette.slider_remainder_fill) >= 3.0
            );
        }
    }

    fn contrast_ratio(a: Color32, b: Color32) -> f64 {
        let a = relative_luminance(a);
        let b = relative_luminance(b);
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    fn relative_luminance(color: Color32) -> f64 {
        let channel = |value: u8| {
            let value = f64::from(value) / 255.0;
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(color.r()) + 0.7152 * channel(color.g()) + 0.0722 * channel(color.b())
    }
}
