use eframe::egui::{
    self, Align, Color32, FontFamily, FontId, Frame, Layout, Margin, Response, Rounding, Sense,
    Stroke, TextStyle, Ui, Vec2,
};

use super::theme::ui_palette;

const PRIMARY_TARGET_HEIGHT: f32 = 44.0;
const COMPACT_BUTTON_HEIGHT: f32 = 36.0;
const KEYCAP_VERTICAL_PADDING: f32 = 7.0;

pub(crate) fn minimum_primary_target_height() -> f32 {
    PRIMARY_TARGET_HEIGHT
}

pub(crate) fn configure_accessible_style(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
    ctx.set_fonts(fonts);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = Vec2::new(8.0, 8.0);
    style.spacing.button_padding = Vec2::new(12.0, 8.0);
    style.spacing.interact_size = Vec2::new(44.0, PRIMARY_TARGET_HEIGHT);
    style.text_styles.insert(
        TextStyle::Heading,
        FontId::new(24.0, FontFamily::Proportional),
    );
    style
        .text_styles
        .insert(TextStyle::Body, FontId::new(14.0, FontFamily::Proportional));
    style.text_styles.insert(
        TextStyle::Button,
        FontId::new(13.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Small,
        FontId::new(12.0, FontFamily::Proportional),
    );
    ctx.set_style(style);
}

#[derive(Clone, Copy)]
pub(crate) enum ButtonTone {
    Primary,
    Secondary,
    Danger,
    Text,
}

pub(crate) fn button(
    ui: &mut Ui,
    label: impl Into<egui::WidgetText>,
    tone: ButtonTone,
) -> Response {
    let colors = ui_palette(ui);
    let (fill, stroke, text) = match tone {
        ButtonTone::Primary => (
            colors.primary_button_bg,
            Stroke::NONE,
            colors.primary_button_text,
        ),
        ButtonTone::Secondary => (
            colors.card_bg,
            Stroke::new(1.0, colors.border_strong),
            colors.text,
        ),
        ButtonTone::Danger => (colors.error, Stroke::NONE, colors.danger_button_text),
        ButtonTone::Text => (Color32::TRANSPARENT, Stroke::NONE, colors.text),
    };
    let label = label.into().color(text);
    let available_width = (ui.available_width() - 24.0).max(0.0);
    let galley = label.into_galley(ui, None, available_width, TextStyle::Button);
    let visual_size = Vec2::new(
        (galley.size().x + 24.0).max(COMPACT_BUTTON_HEIGHT),
        COMPACT_BUTTON_HEIGHT,
    );
    let target_size = Vec2::new(
        visual_size.x.max(PRIMARY_TARGET_HEIGHT),
        PRIMARY_TARGET_HEIGHT,
    );
    let (target_rect, response) = ui.allocate_exact_size(target_size, Sense::click());
    let visual_rect = egui::Rect::from_center_size(target_rect.center(), visual_size);

    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, galley.text()));
    if ui.is_rect_visible(visual_rect) {
        ui.painter()
            .rect(visual_rect, Rounding::same(5.0), fill, stroke);
        ui.painter()
            .galley(visual_rect.center() - galley.size() * 0.5, galley, text);
    }
    if !response.enabled() {
        ui.ctx().accesskit_node_builder(response.id, |builder| {
            builder.set_disabled();
        });
    }
    paint_focus_ring(ui, &response, Rounding::same(5.0));
    response
}

#[allow(dead_code)]
pub(crate) fn icon_button(ui: &mut Ui, icon: Icon, accessible_name: &str) -> Response {
    let colors = ui_palette(ui);
    let (target_rect, response) =
        ui.allocate_exact_size(Vec2::splat(PRIMARY_TARGET_HEIGHT), Sense::click());
    let visual_rect =
        egui::Rect::from_center_size(target_rect.center(), Vec2::splat(COMPACT_BUTTON_HEIGHT));
    ui.painter().rect(
        visual_rect,
        Rounding::same(5.0),
        if response.hovered() {
            colors.panel_bg
        } else {
            Color32::TRANSPARENT
        },
        Stroke::new(1.0, colors.border),
    );
    ui.painter().text(
        visual_rect.center(),
        egui::Align2::CENTER_CENTER,
        icon_glyph(icon),
        FontId::proportional(20.0),
        colors.muted_text,
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, accessible_name));
    ui.ctx().accesskit_node_builder(response.id, |builder| {
        builder.set_role(egui::accesskit::Role::Button);
        builder.set_name(accessible_name);
    });
    paint_focus_ring(ui, &response, Rounding::same(5.0));
    focus_tooltip(ui, &response, accessible_name);
    response.on_hover_text(accessible_name)
}

pub(crate) fn focus_tooltip(ui: &Ui, response: &Response, text: &str) {
    if response.has_focus() {
        egui::show_tooltip_for(
            ui.ctx(),
            response.id.with("focus-tooltip"),
            &response.rect,
            |ui| {
                ui.label(text);
            },
        );
    }
}

pub(crate) fn paint_focus_ring(ui: &Ui, response: &Response, rounding: Rounding) {
    if response.has_focus() {
        ui.painter().rect_stroke(
            response.rect.shrink(1.0),
            rounding,
            Stroke::new(2.0, ui_palette(ui).accent),
        );
    }
}

pub(crate) fn card(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui)) -> Response {
    let colors = ui_palette(ui);
    let width = ui.available_width();
    ui.allocate_ui_with_layout(Vec2::new(width, 0.0), Layout::top_down(Align::LEFT), |ui| {
        Frame::none()
            .fill(colors.card_bg)
            .stroke(Stroke::new(1.0, colors.border))
            .rounding(Rounding::same(6.0))
            .inner_margin(Margin::same(16.0))
            .show(ui, |ui| {
                ui.set_min_width((width - 32.0).max(0.0));
                add_contents(ui);
            });
    })
    .response
}

pub(crate) fn keycap(ui: &mut Ui, text: &str) {
    let colors = ui_palette(ui);
    Frame::none()
        .fill(colors.card_bg)
        .stroke(Stroke::new(1.0, colors.border_strong))
        .rounding(Rounding::same(3.0))
        .inner_margin(Margin::symmetric(7.0, KEYCAP_VERTICAL_PADDING))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(text)
                    .small()
                    .monospace()
                    .color(colors.text),
            );
        });
}

#[allow(dead_code)]
pub(crate) fn notice(ui: &mut Ui, text: &str, error: bool) -> Response {
    let colors = ui_palette(ui);
    let (fill, stroke, color, icon) = if error {
        (
            colors.error_pale,
            colors.error_border,
            colors.error_text,
            Icon::MicrophoneOff,
        )
    } else {
        (
            colors.panel_bg,
            colors.border,
            colors.neutral_notice_text,
            Icon::Info,
        )
    };
    let width = ui.available_width();
    ui.allocate_ui_with_layout(Vec2::new(width, 0.0), Layout::top_down(Align::LEFT), |ui| {
        Frame::none()
            .fill(fill)
            .stroke(Stroke::new(1.0, stroke))
            .rounding(Rounding::same(5.0))
            .inner_margin(Margin::same(12.0))
            .show(ui, |ui| {
                ui.set_min_width((width - 24.0).max(0.0));
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        egui::RichText::new(icon_glyph(icon))
                            .size(18.0)
                            .color(color),
                    );
                    ui.label(egui::RichText::new(text).color(color));
                });
            });
    })
    .response
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Icon {
    Waveform,
    Microphone,
    Models,
    Settings,
    History,
    About,
    Debug,
    Info,
    Copy,
    ChevronDown,
    ChevronUp,
    Stop,
    MicrophoneOff,
    Keyboard,
    Refresh,
    Cpu,
    Globe,
    Gauge,
    Folder,
    Plus,
}

pub(crate) fn icon_glyph(icon: Icon) -> &'static str {
    use egui_phosphor::regular;

    match icon {
        Icon::Waveform => regular::WAVEFORM,
        Icon::Microphone => regular::MICROPHONE,
        Icon::Models => regular::CUBE,
        Icon::Settings => regular::GEAR,
        Icon::History | Icon::Refresh => regular::ARROW_CLOCKWISE,
        Icon::About | Icon::Info => regular::INFO,
        Icon::Debug => regular::BUG,
        Icon::Copy => regular::COPY,
        Icon::ChevronDown => regular::CARET_DOWN,
        Icon::ChevronUp => regular::CARET_UP,
        Icon::Stop => regular::STOP,
        Icon::MicrophoneOff => regular::MICROPHONE_SLASH,
        Icon::Keyboard => regular::KEYBOARD,
        Icon::Cpu => regular::CPU,
        Icon::Globe => regular::GLOBE,
        Icon::Gauge => regular::GAUGE,
        Icon::Folder => regular::FOLDER,
        Icon::Plus => regular::PLUS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primary_targets_meet_wcag_pointer_size() {
        assert!(minimum_primary_target_height() >= 44.0);
        let ctx = egui::Context::default();
        configure_accessible_style(&ctx);
        assert!(ctx.style().spacing.interact_size.y >= 44.0);
    }

    #[test]
    fn compact_controls_keep_full_size_pointer_targets() {
        let ctx = egui::Context::default();
        configure_accessible_style(&ctx);
        let mut button_target_height = 0.0;
        let mut icon_target_height = 0.0;
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(640.0, 320.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    button_target_height = button(ui, "Save", ButtonTone::Primary).rect.height();
                    icon_target_height = icon_button(ui, Icon::Refresh, "Refresh").rect.height();
                    keycap(ui, "Ctrl");
                });
            },
        );

        assert_eq!(button_target_height, PRIMARY_TARGET_HEIGHT);
        assert_eq!(icon_target_height, PRIMARY_TARGET_HEIGHT);
        let painted_rect_heights: Vec<_> = output
            .shapes
            .iter()
            .filter_map(|shape| match shape.shape {
                egui::epaint::Shape::Rect(ref rect) => Some(rect.rect.height()),
                _ => None,
            })
            .collect();
        assert!(painted_rect_heights
            .iter()
            .any(|height| (*height - COMPACT_BUTTON_HEIGHT).abs() < 0.1));
        assert!(painted_rect_heights
            .iter()
            .any(|height| { *height >= 28.0 && *height <= 30.0 }));
    }

    #[test]
    fn cards_and_notices_allocate_the_available_content_width() {
        let ctx = egui::Context::default();
        let mut available_width = 0.0;
        let mut card_width = 0.0;
        let mut notice_width = 0.0;
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(640.0, 320.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    available_width = ui.available_width();
                    card_width = card(ui, |_| {}).rect.width();
                    notice_width = notice(ui, "A full-width notice", false).rect.width();
                });
            },
        );
        assert!(
            card_width >= available_width - 1.0,
            "card={card_width}, available={available_width}"
        );
        assert!(
            notice_width >= available_width - 1.0,
            "notice={notice_width}, available={available_width}"
        );
    }

    #[test]
    fn focused_custom_control_paints_a_two_point_focus_ring() {
        let ctx = egui::Context::default();
        let output = ctx.run(
            egui::RawInput {
                focused: true,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let (rect, response) =
                        ui.allocate_exact_size(Vec2::new(44.0, 44.0), Sense::click());
                    response.request_focus();
                    paint_focus_ring(ui, &response, Rounding::same(5.0));
                    rect
                });
            },
        );
        assert!(output.shapes.iter().any(|shape| matches!(
            shape.shape,
            egui::epaint::Shape::Rect(rect) if rect.stroke.width == 2.0
        )));
    }
}
