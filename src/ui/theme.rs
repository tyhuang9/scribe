use eframe::egui::{self, Color32, Ui};

use crate::branding::{DEEP_INK, ICE_MIST, LIVE_CORAL, SCRIBE_TEAL, SOFT_AQUA, WARM_SAND};

// Raw dark application tokens from the approved Scribe identity board. These
// deliberately live apart from `branding`'s light identity constants: the
// palette owns application semantics without changing asset-rendering colors.
pub(crate) const DARK_CHARCOAL: Color32 = Color32::from_rgb(0x12, 0x14, 0x18);
pub(crate) const DARK_SURFACE: Color32 = Color32::from_rgb(0x1A, 0x1D, 0x22);
pub(crate) const DARK_SOFT_TEXT: Color32 = Color32::from_rgb(0xE9, 0xF0, 0xF0);
pub(crate) const DARK_MUTED_GRAY: Color32 = Color32::from_rgb(0x8E, 0x99, 0xA3);
pub(crate) const DARK_SCRIBE_TEAL: Color32 = Color32::from_rgb(0x2D, 0x97, 0x9C);
pub(crate) const DARK_LIVE_CORAL: Color32 = Color32::from_rgb(0xFD, 0x81, 0x6F);
// A neutral one-step elevation from Surface for active model cards and
// information panels. It deliberately avoids the teal interaction accent.
const DARK_ELEVATED_SURFACE: Color32 = Color32::from_rgb(0x23, 0x26, 0x2C);

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
    pub chip_success_text: Color32,
    pub chip_success_fill: Color32,
    pub chip_success_border: Color32,
    pub chip_warning_text: Color32,
    pub chip_warning_fill: Color32,
    pub chip_warning_border: Color32,
    pub chip_error_text: Color32,
    pub chip_error_fill: Color32,
    pub chip_error_border: Color32,
    pub chip_active_text: Color32,
    pub chip_active_fill: Color32,
    pub chip_active_border: Color32,
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
            chip_success_text: Color32::from_rgb(22, 101, 52),
            chip_success_fill: Color32::from_rgb(240, 253, 244),
            chip_success_border: Color32::from_rgb(187, 247, 208),
            chip_warning_text: Color32::from_rgb(146, 64, 14),
            chip_warning_fill: WARM_SAND,
            chip_warning_border: Color32::from_rgb(254, 240, 138),
            chip_error_text: Color32::from_rgb(185, 28, 28),
            chip_error_fill: Color32::from_rgb(254, 242, 242),
            chip_error_border: Color32::from_rgb(254, 202, 202),
            chip_active_text: Color32::from_rgb(23, 111, 116),
            chip_active_fill: ICE_MIST,
            chip_active_border: SCRIBE_TEAL,
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
            shell_bg: DARK_CHARCOAL,
            content_bg: DARK_CHARCOAL,
            sidebar_bg: DARK_SURFACE,
            card_bg: DARK_SURFACE,
            panel_bg: DARK_ELEVATED_SURFACE,
            disabled_bg: Color32::from_rgb(34, 38, 45),
            active_card_bg: Color32::from_rgb(32, 58, 60),
            text: DARK_SOFT_TEXT,
            muted_text: DARK_MUTED_GRAY,
            tertiary_text: Color32::from_rgb(186, 197, 203),
            border: Color32::from_rgb(97, 107, 117),
            border_strong: DARK_SCRIBE_TEAL,
            control_border: Color32::from_rgb(125, 135, 146),
            slider_track_border: DARK_SCRIBE_TEAL,
            slider_threshold_fill: Color32::from_rgb(37, 42, 48),
            slider_remainder_fill: Color32::from_rgb(52, 58, 66),
            slider_live_below: DARK_SCRIBE_TEAL,
            slider_live_above: Color32::from_rgb(242, 194, 123),
            sensitivity_marker_on_track: DARK_SOFT_TEXT,
            sensitivity_marker_on_live: DARK_CHARCOAL,
            meter_track: Color32::from_rgb(37, 42, 48),
            meter_rating_1: DARK_LIVE_CORAL,
            meter_rating_2: Color32::from_rgb(240, 160, 112),
            meter_rating_3: Color32::from_rgb(242, 194, 123),
            meter_rating_4: Color32::from_rgb(168, 210, 159),
            meter_rating_5: DARK_SCRIBE_TEAL,
            primary: DARK_SOFT_TEXT,
            accent: DARK_SCRIBE_TEAL,
            selection_fill: Color32::from_rgb(31, 112, 116),
            selection_text: DARK_SOFT_TEXT,
            recording_waveform: DARK_SCRIBE_TEAL,
            segmented_control_bg: DARK_SCRIBE_TEAL,
            segmented_control_selected_text: DARK_SOFT_TEXT,
            inactive_toggle_track: Color32::from_rgb(104, 114, 123),
            chip_success_text: Color32::from_rgb(178, 228, 203),
            chip_success_fill: Color32::from_rgb(28, 64, 49),
            chip_success_border: Color32::from_rgb(60, 139, 104),
            chip_warning_text: Color32::from_rgb(255, 223, 173),
            chip_warning_fill: Color32::from_rgb(72, 54, 30),
            chip_warning_border: Color32::from_rgb(154, 112, 53),
            chip_error_text: Color32::from_rgb(255, 199, 190),
            chip_error_fill: Color32::from_rgb(79, 47, 53),
            chip_error_border: Color32::from_rgb(185, 79, 74),
            chip_active_text: Color32::from_rgb(119, 209, 211),
            chip_active_fill: Color32::from_rgb(32, 61, 64),
            chip_active_border: DARK_SCRIBE_TEAL,
            success: Color32::from_rgb(178, 228, 203),
            success_text: Color32::from_rgb(178, 228, 203),
            warning: Color32::from_rgb(255, 223, 173),
            error_fill: DARK_LIVE_CORAL,
            error_text: Color32::from_rgb(255, 228, 222),
            danger_button_text: DARK_CHARCOAL,
            error_pale: Color32::from_rgb(74, 45, 49),
            error_border: DARK_LIVE_CORAL,
            neutral_notice_text: DARK_MUTED_GRAY,
            primary_button_bg: DARK_SCRIBE_TEAL,
            primary_button_text: DARK_CHARCOAL,
            inverse_neutral_bg: DARK_SOFT_TEXT,
            inverse_neutral_text: DARK_CHARCOAL,
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

    /// Build the complete egui visual contract for Scribe. Keeping this recipe
    /// beside the semantic palette prevents production and deterministic UI
    /// fixtures from drifting back to egui's stock gray/blue interaction states.
    pub(crate) fn visuals(dark_mode: bool) -> egui::Visuals {
        let mut visuals = if dark_mode {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        let colors = if dark_mode {
            Self::dark()
        } else {
            Self::light()
        };

        visuals.override_text_color = Some(colors.text);
        visuals.selection.bg_fill = colors.selection_fill;
        visuals.selection.stroke = egui::Stroke::new(1.0, colors.selection_text);
        visuals.hyperlink_color = colors.accent;
        visuals.faint_bg_color = colors.disabled_bg;
        visuals.extreme_bg_color = colors.panel_bg;
        visuals.code_bg_color = colors.panel_bg;
        visuals.warn_fg_color = colors.warning;
        visuals.error_fg_color = colors.error_text;
        visuals.panel_fill = colors.content_bg;
        visuals.window_fill = colors.card_bg;
        visuals.window_stroke = egui::Stroke::new(1.0, colors.border);
        visuals.window_shadow.color = brand_shadow(colors.primary, 42);
        visuals.popup_shadow.color = brand_shadow(colors.primary, 56);
        visuals.text_cursor = egui::Stroke::new(2.0, colors.accent);
        visuals.interact_cursor = Some(egui::CursorIcon::PointingHand);

        let rounding = egui::Rounding::same(6.0);
        visuals.widgets.noninteractive = egui::style::WidgetVisuals {
            bg_fill: colors.card_bg,
            weak_bg_fill: colors.panel_bg,
            bg_stroke: egui::Stroke::new(1.0, colors.border),
            rounding,
            fg_stroke: egui::Stroke::new(1.0, colors.text),
            expansion: 0.0,
        };
        visuals.widgets.inactive = egui::style::WidgetVisuals {
            bg_fill: colors.card_bg,
            weak_bg_fill: colors.panel_bg,
            bg_stroke: egui::Stroke::new(1.0, colors.control_border),
            rounding,
            fg_stroke: egui::Stroke::new(1.0, colors.text),
            expansion: 0.0,
        };
        visuals.widgets.hovered = egui::style::WidgetVisuals {
            bg_fill: colors.active_card_bg,
            weak_bg_fill: colors.active_card_bg,
            bg_stroke: egui::Stroke::new(1.5, colors.border_strong),
            rounding,
            fg_stroke: egui::Stroke::new(1.5, colors.text),
            expansion: 0.0,
        };
        visuals.widgets.active = egui::style::WidgetVisuals {
            bg_fill: colors.active_card_bg,
            weak_bg_fill: colors.active_card_bg,
            bg_stroke: egui::Stroke::new(1.5, colors.accent),
            rounding,
            fg_stroke: egui::Stroke::new(1.5, colors.text),
            expansion: 0.0,
        };
        visuals.widgets.open = egui::style::WidgetVisuals {
            bg_fill: colors.active_card_bg,
            weak_bg_fill: colors.active_card_bg,
            bg_stroke: egui::Stroke::new(1.5, colors.accent),
            rounding,
            fg_stroke: egui::Stroke::new(1.5, colors.text),
            expansion: 0.0,
        };

        visuals
    }
}

fn brand_shadow(color: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
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
        assert_eq!(palette.chip_warning_fill, WARM_SAND);
        assert!(contrast_ratio(palette.chip_warning_text, palette.chip_warning_fill) >= 4.5);
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
        assert_eq!(ThemePalette::dark().recording_waveform, DARK_SCRIBE_TEAL);
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
        assert_eq!(dark.primary_button_bg, DARK_SCRIBE_TEAL);
        assert!(contrast_ratio(dark.primary_button_text, dark.primary_button_bg) >= 4.5);
        assert_eq!(dark.warning, Color32::from_rgb(255, 223, 173));
        assert_eq!(dark.error_fill, DARK_LIVE_CORAL);
        assert!(contrast_ratio(dark.error_text, dark.card_bg) >= 4.5);
    }

    #[test]
    fn dark_palette_maps_the_charcoal_identity_board_to_application_semantics() {
        assert_eq!(DARK_CHARCOAL, Color32::from_rgb(0x12, 0x14, 0x18));
        assert_eq!(DARK_SURFACE, Color32::from_rgb(0x1A, 0x1D, 0x22));
        assert_eq!(DARK_SOFT_TEXT, Color32::from_rgb(0xE9, 0xF0, 0xF0));
        assert_eq!(DARK_MUTED_GRAY, Color32::from_rgb(0x8E, 0x99, 0xA3));
        assert_eq!(DARK_SCRIBE_TEAL, Color32::from_rgb(0x2D, 0x97, 0x9C));
        assert_eq!(DARK_LIVE_CORAL, Color32::from_rgb(0xFD, 0x81, 0x6F));

        let palette = ThemePalette::dark();
        assert_eq!(palette.shell_bg, DARK_CHARCOAL);
        assert_eq!(palette.content_bg, DARK_CHARCOAL);
        assert_eq!(palette.sidebar_bg, DARK_SURFACE);
        assert_eq!(palette.card_bg, DARK_SURFACE);
        assert_eq!(palette.panel_bg, DARK_ELEVATED_SURFACE);
        assert_eq!(palette.text, DARK_SOFT_TEXT);
        assert_eq!(palette.muted_text, DARK_MUTED_GRAY);
        assert_eq!(palette.accent, DARK_SCRIBE_TEAL);
        assert_eq!(palette.recording_waveform, DARK_SCRIBE_TEAL);
        assert_eq!(palette.error_fill, DARK_LIVE_CORAL);
    }

    #[test]
    fn dark_model_card_states_use_a_neutral_elevated_panel_surface() {
        let palette = ThemePalette::dark();

        assert_eq!(palette.card_bg, DARK_SURFACE);
        assert_eq!(palette.panel_bg, DARK_ELEVATED_SURFACE);
        assert_ne!(palette.card_bg, palette.panel_bg);
        assert!(palette.panel_bg.r() > palette.card_bg.r());
        assert!(palette.panel_bg.g() > palette.card_bg.g());
        assert!(palette.panel_bg.b() > palette.card_bg.b());
        assert!(contrast_ratio(palette.text, palette.panel_bg) >= 4.5);
        assert!(contrast_ratio(palette.control_border, palette.panel_bg) >= 3.0);
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

        let dark = ThemePalette::dark();
        for boundary in [dark.border, dark.border_strong, dark.control_border] {
            assert!(contrast_ratio(boundary, dark.card_bg) >= 3.0);
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

    #[test]
    fn complete_visuals_recipe_uses_brand_tokens_for_every_widget_state() {
        for (dark_mode, palette) in [(false, ThemePalette::light()), (true, ThemePalette::dark())] {
            let visuals = ThemePalette::visuals(dark_mode);

            assert_eq!(visuals.dark_mode, dark_mode);
            assert_eq!(visuals.panel_fill, palette.content_bg);
            assert_eq!(visuals.window_fill, palette.card_bg);
            assert_eq!(visuals.window_stroke.color, palette.border);
            assert_eq!(visuals.selection.bg_fill, palette.selection_fill);
            assert_eq!(visuals.selection.stroke.color, palette.selection_text);
            assert_eq!(visuals.hyperlink_color, palette.accent);
            assert_eq!(visuals.faint_bg_color, palette.disabled_bg);
            assert_eq!(visuals.extreme_bg_color, palette.panel_bg);
            assert_eq!(visuals.code_bg_color, palette.panel_bg);
            assert_eq!(visuals.warn_fg_color, palette.warning);
            assert_eq!(visuals.error_fg_color, palette.error_text);
            assert_eq!(visuals.text_cursor.color, palette.accent);
            assert_eq!(
                visuals.interact_cursor,
                Some(egui::CursorIcon::PointingHand)
            );

            let states = [
                visuals.widgets.noninteractive,
                visuals.widgets.inactive,
                visuals.widgets.hovered,
                visuals.widgets.active,
                visuals.widgets.open,
            ];
            assert!(
                states
                    .iter()
                    .all(|state| state.fg_stroke.color == palette.text)
            );
            assert_eq!(
                visuals.widgets.noninteractive.weak_bg_fill,
                palette.panel_bg
            );
            assert_eq!(visuals.widgets.noninteractive.bg_fill, palette.card_bg);
            assert_eq!(
                visuals.widgets.noninteractive.bg_stroke.color,
                palette.border
            );
            assert_eq!(visuals.widgets.inactive.weak_bg_fill, palette.panel_bg);
            assert_eq!(visuals.widgets.inactive.bg_fill, palette.card_bg);
            assert_eq!(
                visuals.widgets.inactive.bg_stroke.color,
                palette.control_border
            );
            for state in [
                visuals.widgets.hovered,
                visuals.widgets.active,
                visuals.widgets.open,
            ] {
                assert_eq!(state.bg_fill, palette.active_card_bg);
                assert_eq!(state.weak_bg_fill, palette.active_card_bg);
            }
            assert_eq!(
                visuals.widgets.hovered.bg_stroke.color,
                palette.border_strong
            );
            assert_eq!(visuals.widgets.active.bg_stroke.color, palette.accent);
            assert_eq!(visuals.widgets.open.bg_stroke.color, palette.accent);
        }
    }

    #[test]
    fn interactive_visuals_preserve_text_and_boundary_contrast() {
        for (dark_mode, palette) in [(false, ThemePalette::light()), (true, ThemePalette::dark())] {
            let visuals = ThemePalette::visuals(dark_mode);
            for state in [
                visuals.widgets.inactive,
                visuals.widgets.hovered,
                visuals.widgets.active,
                visuals.widgets.open,
            ] {
                assert!(contrast_ratio(state.fg_stroke.color, state.bg_fill) >= 4.5);
                assert!(contrast_ratio(state.bg_stroke.color, palette.card_bg) >= 3.0);
            }
            assert!(contrast_ratio(visuals.text_cursor.color, palette.card_bg) >= 3.0);
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
