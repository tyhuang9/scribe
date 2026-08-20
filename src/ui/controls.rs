use eframe::egui::{
    self, Align, Color32, FontFamily, FontId, Frame, Layout, Margin, Response, Rounding, Sense,
    Stroke, TextStyle, Ui, Vec2,
};

use super::theme::ui_palette;

const PRIMARY_TARGET_HEIGHT: f32 = 44.0;
const COMPACT_BUTTON_HEIGHT: f32 = 32.0;
const KEYCAP_HORIZONTAL_PADDING: f32 = 7.0;
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

pub(crate) fn icon_button(ui: &mut Ui, icon: Icon, accessible_name: &str) -> Response {
    let colors = ui_palette(ui);
    let enabled = ui.is_enabled();
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
        if !enabled {
            builder.set_disabled();
        }
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

/// A consistent search affordance for routes that filter local UI content.
///
/// The clear target is always reserved so adding or removing text never shifts
/// neighbouring controls. Callers decide whether query changes filter live or
/// need an explicit submit action.
pub(crate) struct SearchFieldResponse {
    pub input: Response,
    #[cfg(test)]
    pub clear: Response,
    #[cfg(test)]
    pub surface: egui::Rect,
    pub changed: bool,
    pub clear_requested: bool,
}

pub(crate) fn search_field(
    ui: &mut Ui,
    width: f32,
    id_source: impl std::hash::Hash,
    value: &mut String,
    accessible_name: &str,
    hint_text: &str,
    description: &str,
) -> SearchFieldResponse {
    let colors = ui_palette(ui);
    let field_id = ui.make_persistent_id(id_source);
    // Read Escape before TextEdit can consume its editing/navigation event.
    let escape_pressed = ui.input(|input| input.key_pressed(egui::Key::Escape));
    let clear_key_pressed = ui
        .input(|input| input.key_pressed(egui::Key::Enter) || input.key_pressed(egui::Key::Space));
    // TextEdit surrenders focus for Escape, so preserve its pre-edit focus
    // state to let the route handle the intended clear action.
    let had_input_focus = ui.memory(|memory| memory.has_focus(field_id));
    // Allocate one fixed surface before adding child controls. This exactly
    // preserves the old full-width 44px edit geometry at every route width.
    let (surface_rect, _) = ui.allocate_exact_size(
        Vec2::new(width.min(ui.available_width()), PRIMARY_TARGET_HEIGHT),
        Sense::hover(),
    );
    ui.painter().rect(
        surface_rect,
        Rounding::same(5.0),
        colors.card_bg,
        Stroke::new(1.0, colors.border_strong),
    );
    // On a compact surface the clear target takes the trailing 44px without
    // an inset and the decorative glyph yields first. This keeps both the
    // clear target and a positive text-edit rect contained instead of
    // allowing either child to extend outside a narrow parent.
    let standard_layout = surface_rect.width() >= 160.0;
    let trailing_inset = if standard_layout { 10.0 } else { 0.0 };
    let minimum_input_width = 1.0;
    let clear_width =
        PRIMARY_TARGET_HEIGHT.min((surface_rect.width() - minimum_input_width).max(0.0));
    let clear_rect = egui::Rect::from_min_size(
        egui::pos2(
            surface_rect.right() - trailing_inset - clear_width,
            surface_rect.top(),
        ),
        Vec2::new(clear_width, PRIMARY_TARGET_HEIGHT),
    );
    let icon_width = if standard_layout { 20.0 } else { 0.0 };
    let input_left = surface_rect.left() + if standard_layout { 38.0 } else { 0.0 };
    let input_right = (clear_rect.left() - if standard_layout { 8.0 } else { 0.0 })
        .max(input_left + minimum_input_width)
        .min(surface_rect.right());
    let input_rect = egui::Rect::from_min_max(
        egui::pos2(
            input_left.min(input_right - minimum_input_width),
            surface_rect.top(),
        ),
        egui::pos2(input_right, surface_rect.bottom()),
    );
    // Paint the leading glyph directly so it remains decorative: the text
    // input is the sole semantic, focusable search control. It deliberately
    // disappears before it can crowd the usable text field on a compact row.
    if icon_width > 0.0 {
        let icon_rect = egui::Rect::from_min_size(
            egui::pos2(surface_rect.left() + 10.0, surface_rect.top()),
            Vec2::new(icon_width, PRIMARY_TARGET_HEIGHT),
        );
        ui.painter().text(
            icon_rect.center(),
            egui::Align2::CENTER_CENTER,
            icon_glyph(Icon::Search),
            FontId::proportional(18.0),
            colors.muted_text,
        );
    }
    let mut input_ui = ui.child_ui(input_rect, Layout::left_to_right(Align::Center));
    let input = input_ui.add_sized(
        input_rect.size(),
        egui::TextEdit::singleline(value)
            .id(field_id)
            .hint_text(hint_text)
            .frame(false),
    );
    let mut clear_ui = ui.child_ui(clear_rect, Layout::left_to_right(Align::Center));
    let clear = clear_ui
        .add_enabled_ui(!value.is_empty(), |ui| {
            icon_button(ui, Icon::Close, &format!("Clear {accessible_name}"))
        })
        .inner;
    let clear_requested = !value.is_empty()
        && (clear.clicked()
            || (clear.has_focus() && clear_key_pressed)
            || (had_input_focus && escape_pressed));

    ui.ctx().accesskit_node_builder(input.id, |builder| {
        builder.set_name(accessible_name);
        builder.set_description(description);
    });
    ui.ctx().accesskit_node_builder(clear.id, |builder| {
        builder.set_description(format!("Clears the current {accessible_name} query."));
    });
    if input.has_focus() {
        ui.painter().rect_stroke(
            surface_rect.shrink(1.0),
            Rounding::same(5.0),
            Stroke::new(2.0, colors.accent),
        );
    }

    let changed = input.changed();
    SearchFieldResponse {
        input,
        #[cfg(test)]
        clear,
        #[cfg(test)]
        surface: surface_rect,
        changed,
        clear_requested,
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
        .inner_margin(Margin::symmetric(
            KEYCAP_HORIZONTAL_PADDING,
            KEYCAP_VERTICAL_PADDING,
        ))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(text)
                    .small()
                    .monospace()
                    .color(colors.text),
            );
        });
}

pub(crate) fn keycap_width(ui: &Ui, text: &str) -> f32 {
    ui.painter()
        .layout_no_wrap(
            text.to_owned(),
            FontId::monospace(TextStyle::Small.resolve(ui.style()).size),
            ui_palette(ui).text,
        )
        .size()
        .x
        + KEYCAP_HORIZONTAL_PADDING * 2.0
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

#[allow(dead_code)]
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
    CheckCircle,
    Gauge,
    Folder,
    Plus,
    Download,
    Play,
    Pause,
    Warning,
    Trash,
    Close,
    Search,
    ChevronRight,
    Spinner,
    Streaming,
    WordTimestamps,
    Translation,
    LanguageDetection,
    BatchTranscription,
    Cancellation,
    ConfidenceScores,
    CustomVocabulary,
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
        Icon::CheckCircle => regular::CHECK_CIRCLE,
        Icon::Gauge => regular::GAUGE,
        Icon::Folder => regular::FOLDER,
        Icon::Plus => regular::PLUS,
        Icon::Download => regular::DOWNLOAD,
        Icon::Play => regular::PLAY,
        Icon::Pause => regular::PAUSE,
        Icon::Warning => regular::WARNING,
        Icon::Trash => regular::TRASH,
        Icon::Close => regular::X,
        Icon::Search => regular::MAGNIFYING_GLASS,
        Icon::ChevronRight => regular::CARET_RIGHT,
        Icon::Spinner => regular::CIRCLE_NOTCH,
        Icon::Streaming => regular::WAVEFORM,
        Icon::WordTimestamps => regular::SUBTITLES,
        Icon::Translation => regular::TRANSLATE,
        Icon::LanguageDetection => regular::GLOBE,
        Icon::BatchTranscription => regular::LIST_BULLETS,
        Icon::Cancellation => regular::PROHIBIT,
        Icon::ConfidenceScores => regular::SEAL_CHECK,
        Icon::CustomVocabulary => regular::NOTE_PENCIL,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_phosphor::regular;

    #[test]
    fn primary_targets_meet_wcag_pointer_size() {
        assert!(minimum_primary_target_height() >= 44.0);
        let ctx = egui::Context::default();
        configure_accessible_style(&ctx);
        assert!(ctx.style().spacing.interact_size.y >= 44.0);
    }

    #[test]
    fn model_feature_icons_have_stable_glyph_contracts() {
        assert_eq!(icon_glyph(Icon::Streaming), regular::WAVEFORM);
        assert_eq!(icon_glyph(Icon::WordTimestamps), regular::SUBTITLES);
        assert_eq!(icon_glyph(Icon::Translation), regular::TRANSLATE);
        assert_eq!(icon_glyph(Icon::LanguageDetection), regular::GLOBE);
        assert_eq!(icon_glyph(Icon::BatchTranscription), regular::LIST_BULLETS);
        assert_eq!(icon_glyph(Icon::Cancellation), regular::PROHIBIT);
        assert_eq!(icon_glyph(Icon::ConfidenceScores), regular::SEAL_CHECK);
        assert_eq!(icon_glyph(Icon::CustomVocabulary), regular::NOTE_PENCIL);
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
        assert!(
            painted_rect_heights
                .iter()
                .any(|height| (*height - COMPACT_BUTTON_HEIGHT).abs() < 0.1)
        );
        assert!(
            painted_rect_heights
                .iter()
                .any(|height| { *height >= 28.0 && *height <= 30.0 })
        );
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

    #[test]
    fn search_field_exposes_a_labelled_full_size_clear_action_without_layout_shift() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let mut query = "base".to_owned();
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(320.0, 120.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let response = search_field(
                        ui,
                        ui.available_width(),
                        "search-field-contract",
                        &mut query,
                        "Search models",
                        "Search models",
                        "Filters models as you type.",
                    );
                    assert!(!response.clear_requested);
                });
            },
        );
        let nodes = &output
            .platform_output
            .accesskit_update
            .expect("search field should update AccessKit")
            .nodes;
        let search = nodes
            .iter()
            .find_map(|(_, node)| (node.name() == Some("Search models")).then_some(node))
            .expect("search input should have an accessible name");
        assert_eq!(search.description(), Some("Filters models as you type."));
        let clear = nodes
            .iter()
            .find_map(|(_, node)| (node.name() == Some("Clear Search models")).then_some(node))
            .expect("clear action should have an accessible name");
        assert!(
            clear.bounds().expect("clear action needs bounds").height() >= 44.0,
            "clear action must retain a 44px pointer target"
        );

        let empty_ctx = egui::Context::default();
        empty_ctx.enable_accesskit();
        configure_accessible_style(&empty_ctx);
        let mut empty_query = String::new();
        let empty_output = empty_ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(320.0, 120.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let response = search_field(
                        ui,
                        ui.available_width(),
                        "empty-search-field-contract",
                        &mut empty_query,
                        "Search models",
                        "Search models",
                        "Filters models as you type.",
                    );
                    assert!(!response.clear_requested);
                });
            },
        );
        let empty_nodes = &empty_output
            .platform_output
            .accesskit_update
            .expect("empty search field should update AccessKit")
            .nodes;
        let disabled_clear = empty_nodes
            .iter()
            .find_map(|(_, node)| (node.name() == Some("Clear Search models")).then_some(node))
            .expect("empty search still reserves its clear target");
        assert!(disabled_clear.is_disabled());
        assert_eq!(
            disabled_clear
                .bounds()
                .expect("disabled clear action needs bounds")
                .height(),
            clear
                .bounds()
                .expect("enabled clear action needs bounds")
                .height(),
            "empty and populated search fields must reserve identical clear geometry"
        );
        assert!(
            empty_nodes
                .iter()
                .all(|(_, node)| node.name() != Some(icon_glyph(Icon::Search))),
            "the search glyph is decorative, not a focusable/accessibility node"
        );
    }

    #[test]
    fn search_field_keeps_its_input_and_clear_target_contained_when_compact() {
        for width in [45.0, 53.0, 54.0, 80.0, 100.0] {
            let ctx = egui::Context::default();
            let mut query = "base".to_owned();
            let mut surface = egui::Rect::NOTHING;
            let mut input = egui::Rect::NOTHING;
            let mut clear = egui::Rect::NOTHING;
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(320.0, 120.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let response = search_field(
                            ui,
                            width,
                            ("compact-search-field", width.to_bits()),
                            &mut query,
                            "Search models",
                            "Search models",
                            "Filters models as you type.",
                        );
                        surface = response.surface;
                        input = response.input.rect;
                        clear = response.clear.rect;
                    });
                },
            );
            assert!(
                input.width() > 0.0 && input.height() > 0.0,
                "search input must remain positive at width {width}: {input:?}"
            );
            assert!(
                surface.contains_rect(input),
                "input must remain inside the surface at width {width}: surface={surface:?}, input={input:?}"
            );
            assert!(
                surface.contains_rect(clear),
                "clear action must remain inside the surface at width {width}: surface={surface:?}, clear={clear:?}"
            );
            assert!(
                clear.width() >= PRIMARY_TARGET_HEIGHT && clear.height() >= PRIMARY_TARGET_HEIGHT,
                "clear action must retain its 44px target at width {width}: {clear:?}"
            );
        }
    }

    #[test]
    fn search_field_escape_requests_a_clear_only_when_its_input_has_focus() {
        let ctx = egui::Context::default();
        configure_accessible_style(&ctx);
        let mut query = "base".to_owned();
        let mut clear_requested = false;
        let _ = ctx.run(
            egui::RawInput {
                focused: true,
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(320.0, 120.0),
                )),
                events: vec![egui::Event::Key {
                    key: egui::Key::Escape,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let id = ui.make_persistent_id("search-field-escape");
                    ui.memory_mut(|memory| memory.request_focus(id));
                    let response = search_field(
                        ui,
                        ui.available_width(),
                        "search-field-escape",
                        &mut query,
                        "Search models",
                        "Search models",
                        "Filters models as you type.",
                    );
                    clear_requested = response.clear_requested;
                });
            },
        );
        assert!(clear_requested);
    }

    #[test]
    fn search_field_clear_supports_enter_and_space_activation() {
        for key in [egui::Key::Enter, egui::Key::Space] {
            let ctx = egui::Context::default();
            configure_accessible_style(&ctx);
            let mut query = "base".to_owned();
            let mut clear_id = egui::Id::NULL;
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(320.0, 120.0),
                    )),
                    focused: true,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        clear_id = search_field(
                            ui,
                            ui.available_width(),
                            "search-field-keyboard-clear",
                            &mut query,
                            "Search models",
                            "Search models",
                            "Filters models as you type.",
                        )
                        .clear
                        .id;
                    });
                },
            );
            let mut clear_requested = false;
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(320.0, 120.0),
                    )),
                    focused: true,
                    events: vec![egui::Event::Key {
                        key,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: egui::Modifiers::NONE,
                    }],
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        ui.memory_mut(|memory| memory.request_focus(clear_id));
                        clear_requested = search_field(
                            ui,
                            ui.available_width(),
                            "search-field-keyboard-clear",
                            &mut query,
                            "Search models",
                            "Search models",
                            "Filters models as you type.",
                        )
                        .clear_requested;
                    });
                },
            );
            assert!(clear_requested, "{key:?} should activate the clear action");
        }
    }
}
