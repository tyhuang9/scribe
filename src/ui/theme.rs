use eframe::egui::{self, Color32, Ui};

use crate::branding::{
    DEEP_INK, DEEP_NAVY, ICE_MIST, LIVE_CORAL, NAVY_SURFACE, SCRIBE_TEAL, SOFT_AQUA, TEAL_ACCENT,
    WARM_SAND,
};

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
    /// Idle input/control boundary with at least 3:1 contrast on control surfaces.
    pub control_border: Color32,
    pub slider_track_border: Color32,
    pub slider_threshold_fill: Color32,
    pub slider_remainder_fill: Color32,
    pub slider_live_below: Color32,
    pub slider_live_above: Color32,
    /// Outer marker ring that stays distinct from the idle track.
    pub sensitivity_marker_on_track: Color32,
    /// Inner marker ring that stays distinct from the teal live-input fill.
    pub sensitivity_marker_on_live: Color32,
    /// Short model-card meter colors. `meter_track` is intentionally neutral.
    pub meter_track: Color32,
    pub meter_rating_1: Color32,
    pub meter_rating_2: Color32,
    pub meter_rating_3: Color32,
    pub meter_rating_4: Color32,
    pub meter_rating_5: Color32,
    pub primary: Color32,
    pub accent: Color32,
    /// Generic selection fill. Dark mode uses a deeper semantic teal so egui's
    /// shared selected-text/focus stroke can remain visible in both roles.
    pub selection_fill: Color32,
    /// Foreground for generic selected text and egui's coupled focus frame.
    pub selection_text: Color32,
    /// Scribe teal recording mark shared by foreground and native overlay renderers.
    pub recording_waveform: Color32,
    /// Theme-coherent teal/neutral track for the compact recording-mode selector.
    pub segmented_control_bg: Color32,
    /// Foreground for the selected segment on the current card surface.
    pub segmented_control_selected_text: Color32,
    /// Distinguishable inactive toggle track and border on card surfaces.
    pub inactive_toggle_track: Color32,
    /// Package green for fills and dots; use `success_text` for text on light surfaces.
    pub success: Color32,
    pub success_text: Color32,
    pub warning: Color32,
    /// Exact Live Coral fill for destructive controls and recording/error dots.
    pub error_fill: Color32,
    /// Accessible error copy on `error_pale`.
    pub error_text: Color32,
    /// Foreground used on the solid danger fill.
    pub danger_button_text: Color32,
    pub error_pale: Color32,
    pub error_border: Color32,
    pub neutral_notice_text: Color32,
    pub primary_button_bg: Color32,
    pub primary_button_text: Color32,
    /// A neutral high-contrast inverse control, reserved for stable Install actions.
    pub inverse_neutral_bg: Color32,
    pub inverse_neutral_text: Color32,
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
            shell_bg: ICE_MIST,
            content_bg: Color32::from_rgb(247, 251, 251),
            sidebar_bg: Color32::WHITE,
            card_bg: Color32::WHITE,
            panel_bg: ICE_MIST,
            disabled_bg: Color32::from_rgb(241, 247, 247),
            active_card_bg: Color32::from_rgb(216, 238, 237),
            text: DEEP_INK,
            muted_text: Color32::from_rgb(64, 91, 110),
            tertiary_text: Color32::from_rgb(82, 105, 122),
            border: SOFT_AQUA,
            border_strong: SCRIBE_TEAL,
            control_border: Color32::from_rgb(88, 114, 126),
            slider_track_border: SCRIBE_TEAL,
            slider_threshold_fill: ICE_MIST,
            slider_remainder_fill: Color32::from_rgb(250, 252, 252),
            slider_live_below: SCRIBE_TEAL,
            slider_live_above: Color32::from_rgb(50, 107, 75),
            sensitivity_marker_on_track: DEEP_INK,
            sensitivity_marker_on_live: Color32::WHITE,
            meter_track: ICE_MIST,
            meter_rating_1: Color32::from_rgb(220, 38, 38),
            meter_rating_2: Color32::from_rgb(194, 65, 12),
            meter_rating_3: Color32::from_rgb(161, 98, 7),
            meter_rating_4: Color32::from_rgb(77, 124, 15),
            meter_rating_5: Color32::from_rgb(6, 118, 71),
            primary: DEEP_INK,
            accent: SCRIBE_TEAL,
            selection_fill: SCRIBE_TEAL,
            selection_text: DEEP_INK,
            recording_waveform: SCRIBE_TEAL,
            segmented_control_bg: SCRIBE_TEAL,
            segmented_control_selected_text: DEEP_INK,
            inactive_toggle_track: Color32::from_rgb(88, 114, 126),
            success: Color32::from_rgb(57, 123, 90),
            success_text: Color32::from_rgb(40, 97, 69),
            warning: Color32::from_rgb(123, 80, 36),
            error_fill: LIVE_CORAL,
            error_text: Color32::from_rgb(132, 46, 38),
            danger_button_text: DEEP_INK,
            error_pale: Color32::from_rgb(255, 240, 237),
            error_border: LIVE_CORAL,
            neutral_notice_text: Color32::from_rgb(64, 91, 110),
            primary_button_bg: SCRIBE_TEAL,
            primary_button_text: DEEP_INK,
            inverse_neutral_bg: DEEP_INK,
            inverse_neutral_text: Color32::WHITE,
        }
    }

    pub(crate) fn dark() -> Self {
        Self {
            shell_bg: DEEP_NAVY,
            content_bg: DEEP_NAVY,
            sidebar_bg: NAVY_SURFACE,
            card_bg: Color32::from_rgb(11, 43, 66),
            panel_bg: Color32::from_rgb(9, 38, 61),
            disabled_bg: Color32::from_rgb(23, 52, 74),
            active_card_bg: Color32::from_rgb(18, 65, 77),
            text: ICE_MIST,
            muted_text: SOFT_AQUA,
            tertiary_text: Color32::from_rgb(145, 185, 187),
            border: Color32::from_rgb(36, 81, 102),
            border_strong: TEAL_ACCENT,
            control_border: Color32::from_rgb(100, 133, 143),
            slider_track_border: TEAL_ACCENT,
            slider_threshold_fill: Color32::from_rgb(23, 58, 76),
            slider_remainder_fill: Color32::from_rgb(36, 71, 93),
            slider_live_below: TEAL_ACCENT,
            slider_live_above: WARM_SAND,
            sensitivity_marker_on_track: ICE_MIST,
            sensitivity_marker_on_live: DEEP_NAVY,
            meter_track: Color32::from_rgb(23, 58, 76),
            meter_rating_1: LIVE_CORAL,
            meter_rating_2: Color32::from_rgb(240, 160, 112),
            meter_rating_3: WARM_SAND,
            meter_rating_4: Color32::from_rgb(168, 210, 159),
            meter_rating_5: TEAL_ACCENT,
            primary: ICE_MIST,
            accent: TEAL_ACCENT,
            selection_fill: Color32::from_rgb(23, 111, 116),
            selection_text: ICE_MIST,
            recording_waveform: TEAL_ACCENT,
            segmented_control_bg: TEAL_ACCENT,
            segmented_control_selected_text: ICE_MIST,
            inactive_toggle_track: Color32::from_rgb(100, 133, 143),
            success: TEAL_ACCENT,
            success_text: Color32::from_rgb(157, 223, 183),
            warning: WARM_SAND,
            error_fill: LIVE_CORAL,
            error_text: Color32::from_rgb(255, 210, 203),
            danger_button_text: DEEP_NAVY,
            error_pale: Color32::from_rgb(82, 45, 49),
            error_border: LIVE_CORAL,
            neutral_notice_text: SOFT_AQUA,
            primary_button_bg: TEAL_ACCENT,
            primary_button_text: DEEP_NAVY,
            inverse_neutral_bg: ICE_MIST,
            inverse_neutral_text: DEEP_NAVY,
        }
    }

    pub(crate) fn meter_rating(self, rating: u8) -> Color32 {
        match rating.clamp(1, 5) {
            1 => self.meter_rating_1,
            2 => self.meter_rating_2,
            3 => self.meter_rating_3,
            4 => self.meter_rating_4,
            _ => self.meter_rating_5,
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
        assert!(contrast_ratio(palette.danger_button_text, palette.error_fill) >= 4.5);
        assert!(contrast_ratio(palette.tertiary_text, palette.card_bg) >= 4.5);
        assert!(contrast_ratio(palette.tertiary_text, palette.content_bg) >= 4.5);
    }

    #[test]
    fn dark_danger_notice_and_button_foregrounds_meet_aa() {
        let palette = ThemePalette::dark();

        assert!(contrast_ratio(palette.error_text, palette.error_pale) >= 4.5);
        assert!(contrast_ratio(palette.danger_button_text, palette.error_fill) >= 4.5);
    }

    #[test]
    fn recording_mode_toggle_uses_semantic_colours_with_aa_contrast() {
        let light = ThemePalette::light();
        assert_eq!(light.segmented_control_bg, light.accent);
        assert_eq!(light.segmented_control_selected_text, light.text);

        let dark = ThemePalette::dark();
        assert_eq!(dark.segmented_control_selected_text, dark.text);

        for palette in [light, dark] {
            assert!(
                contrast_ratio(palette.primary_button_text, palette.segmented_control_bg) >= 4.5
            );
            assert!(
                contrast_ratio(palette.segmented_control_selected_text, palette.card_bg) >= 4.5
            );
            assert!(contrast_ratio(palette.card_bg, palette.segmented_control_bg) >= 3.0);
        }
    }

    #[test]
    fn inactive_toggle_track_contrasts_with_card_surfaces_in_both_themes() {
        for palette in [ThemePalette::light(), ThemePalette::dark()] {
            assert!(contrast_ratio(palette.inactive_toggle_track, palette.card_bg) >= 3.0);
        }
    }

    #[test]
    fn recording_waveform_uses_reference_teal_tokens() {
        assert_eq!(ThemePalette::light().recording_waveform, SCRIBE_TEAL);
        assert_eq!(ThemePalette::dark().recording_waveform, TEAL_ACCENT);
    }

    #[test]
    fn brand_actions_pair_exact_raw_fills_with_accessible_foregrounds() {
        let light = ThemePalette::light();
        assert_eq!(light.primary_button_bg, SCRIBE_TEAL);
        assert!(contrast_ratio(light.primary_button_text, light.primary_button_bg) >= 4.5);
        assert_eq!(light.error_fill, LIVE_CORAL);
        assert!(contrast_ratio(light.danger_button_text, light.error_fill) >= 4.5);
        assert!(contrast_ratio(light.error_text, light.card_bg) >= 4.5);

        let dark = ThemePalette::dark();
        assert_eq!(dark.primary_button_bg, TEAL_ACCENT);
        assert!(contrast_ratio(dark.primary_button_text, dark.primary_button_bg) >= 4.5);
        assert_eq!(dark.warning, WARM_SAND);
        assert_eq!(dark.error_fill, LIVE_CORAL);
        assert!(contrast_ratio(dark.error_text, dark.card_bg) >= 4.5);
    }

    #[test]
    fn generic_selection_and_control_boundary_tokens_meet_contrast_requirements() {
        for palette in [ThemePalette::light(), ThemePalette::dark()] {
            assert!(contrast_ratio(palette.selection_text, palette.selection_fill) >= 4.5);
            for surface in [palette.card_bg, palette.panel_bg, palette.content_bg] {
                assert!(contrast_ratio(palette.selection_text, surface) >= 3.0);
                assert!(contrast_ratio(palette.control_border, surface) >= 3.0);
            }
        }
    }

    #[test]
    fn both_live_meter_fill_states_have_non_text_contrast_against_track_regions() {
        for palette in [ThemePalette::light(), ThemePalette::dark()] {
            for fill in [palette.slider_live_below, palette.slider_live_above] {
                assert!(contrast_ratio(fill, palette.slider_threshold_fill) >= 3.0);
                assert!(contrast_ratio(fill, palette.slider_remainder_fill) >= 3.0);
            }
        }
    }

    #[test]
    fn sensitivity_marker_rings_remain_distinct_from_both_input_bar_states() {
        for palette in [ThemePalette::light(), ThemePalette::dark()] {
            assert!(
                contrast_ratio(
                    palette.sensitivity_marker_on_track,
                    palette.slider_remainder_fill
                ) >= 3.0
            );
            for fill in [palette.slider_live_below, palette.slider_live_above] {
                let outer_ring_contrast = contrast_ratio(palette.sensitivity_marker_on_track, fill);
                let inner_ring_contrast = contrast_ratio(palette.sensitivity_marker_on_live, fill);
                assert!(outer_ring_contrast.max(inner_ring_contrast) >= 3.0);
            }
        }
    }

    #[test]
    fn model_card_meter_tokens_have_five_accessible_bins_in_both_themes() {
        for palette in [ThemePalette::light(), ThemePalette::dark()] {
            let bins = (1..=5)
                .map(|rating| palette.meter_rating(rating))
                .collect::<Vec<_>>();
            assert_eq!(bins.len(), 5);
            assert!(bins.windows(2).all(|pair| pair[0] != pair[1]));
            for color in bins {
                assert!(contrast_ratio(color, palette.meter_track) >= 3.0);
            }
        }
    }

    #[test]
    fn inverse_neutral_install_tokens_meet_aa_in_both_themes() {
        for palette in [ThemePalette::light(), ThemePalette::dark()] {
            assert!(
                contrast_ratio(palette.inverse_neutral_text, palette.inverse_neutral_bg) >= 4.5
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
