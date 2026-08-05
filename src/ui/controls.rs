use eframe::egui::{
    self, Button, Color32, FontFamily, FontId, Frame, Margin, Response, Rounding, Sense, Stroke,
    TextStyle, Ui, Vec2,
};

use super::theme::ui_palette;

const PRIMARY_TARGET_HEIGHT: f32 = 44.0;

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
        ButtonTone::Danger => (colors.error, Stroke::NONE, Color32::WHITE),
        ButtonTone::Text => (Color32::TRANSPARENT, Stroke::NONE, colors.text),
    };
    ui.add(
        Button::new(label.into().color(text))
            .fill(fill)
            .stroke(stroke)
            .rounding(Rounding::same(5.0))
            .min_size(Vec2::new(0.0, 40.0)),
    )
}

pub(crate) fn icon_button(ui: &mut Ui, icon: Icon, accessible_name: &str) -> Response {
    let colors = ui_palette(ui);
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(40.0), Sense::click());
    ui.painter().rect(
        rect,
        Rounding::same(5.0),
        if response.hovered() {
            colors.panel_bg
        } else {
            Color32::TRANSPARENT
        },
        Stroke::new(1.0, colors.border),
    );
    ui.painter().text(
        rect.center(),
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
    response.on_hover_text(accessible_name)
}

pub(crate) fn card(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui)) {
    let colors = ui_palette(ui);
    Frame::none()
        .fill(colors.card_bg)
        .stroke(Stroke::new(1.0, colors.border))
        .rounding(Rounding::same(6.0))
        .inner_margin(Margin::same(16.0))
        .show(ui, add_contents);
}

pub(crate) fn badge(ui: &mut Ui, text: &str, dot: Option<Color32>) {
    let colors = ui_palette(ui);
    Frame::none()
        .fill(colors.panel_bg)
        .stroke(Stroke::new(1.0, colors.border))
        .rounding(Rounding::same(999.0))
        .inner_margin(Margin::symmetric(8.0, 4.0))
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if let Some(dot) = dot {
                    let (rect, _) = ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
                    ui.painter().circle_filled(rect.center(), 3.0, dot);
                }
                ui.label(
                    egui::RichText::new(text)
                        .small()
                        .color(colors.muted_text)
                        .strong(),
                );
            });
        });
}

pub(crate) fn keycap(ui: &mut Ui, text: &str) {
    let colors = ui_palette(ui);
    Frame::none()
        .fill(colors.card_bg)
        .stroke(Stroke::new(1.0, colors.border_strong))
        .rounding(Rounding::same(3.0))
        .inner_margin(Margin::symmetric(7.0, 4.0))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(text)
                    .small()
                    .monospace()
                    .color(colors.text),
            );
        });
}

pub(crate) fn notice(ui: &mut Ui, text: &str, error: bool) {
    let colors = ui_palette(ui);
    let (fill, stroke, color, icon) = if error {
        (
            colors.error_pale,
            colors.error_border,
            colors.error,
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
    Frame::none()
        .fill(fill)
        .stroke(Stroke::new(1.0, stroke))
        .rounding(Rounding::same(5.0))
        .inner_margin(Margin::same(12.0))
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    egui::RichText::new(icon_glyph(icon))
                        .size(18.0)
                        .color(color),
                );
                ui.label(egui::RichText::new(text).color(color));
            });
        });
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
}
