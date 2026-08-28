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
        ButtonTone::Danger => (colors.error_fill, Stroke::NONE, colors.danger_button_text),
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
        let contrast_color = if ui.visuals().dark_mode {
            egui::Color32::WHITE
        } else {
            egui::Color32::BLACK
        };
        ui.painter().rect_stroke(
            response.rect.shrink(1.0),
            rounding,
            Stroke::new(3.0, contrast_color),
        );
        ui.painter().rect_stroke(
            response.rect.shrink(3.0),
            rounding,
            Stroke::new(1.0, ui_palette(ui).accent),
        );
    }
}

/// A consistent search affordance for routes that filter local UI content.
///
/// The clear target is shown only for nonempty, sufficiently wide fields;
/// Escape remains available from the focused input at every width. Callers
/// decide whether query changes filter live or need an explicit submit action.
pub(crate) struct SearchFieldResponse {
    pub input: Response,
    #[cfg(test)]
    pub clear: Option<Response>,
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
    // Allocate one fixed surface before adding child controls. This keeps the
    // text edit's route geometry predictable while retaining a restrained,
    // filled search surface. A quiet idle border gives the field a clear
    // boundary without competing with the stronger keyboard focus ring.
    let (surface_rect, _) = ui.allocate_exact_size(
        Vec2::new(width.min(ui.available_width()), PRIMARY_TARGET_HEIGHT),
        Sense::hover(),
    );
    ui.painter()
        .rect_filled(surface_rect, Rounding::same(5.0), colors.panel_bg);
    ui.painter().rect_stroke(
        surface_rect.shrink(0.5),
        Rounding::same(5.0),
        Stroke::new(1.0, colors.border_strong),
    );
    // Keep the Clear slot and accessibility node stable as the first query
    // character is typed. Empty fields leave the target hidden and disabled;
    // at pathological widths Escape remains available from the text input.
    let allocate_clear = surface_rect.width() >= 88.0;
    let clear_enabled = !value.is_empty();
    let standard_layout = surface_rect.width() >= 160.0;
    let minimum_input_width = 1.0;
    let clear_width = if allocate_clear {
        PRIMARY_TARGET_HEIGHT
    } else {
        0.0
    };
    let clear_rect = egui::Rect::from_min_size(
        egui::pos2(surface_rect.right() - clear_width, surface_rect.top()),
        Vec2::new(clear_width, PRIMARY_TARGET_HEIGHT),
    );
    let icon_width = if standard_layout { 20.0 } else { 0.0 };
    let input_left = surface_rect.left() + if standard_layout { 38.0 } else { 0.0 };
    let input_right = (clear_rect.left()
        - if allocate_clear && standard_layout {
            8.0
        } else {
            0.0
        })
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
    // Mirror `Ui::add_sized`, but retain TextEdit's rich output so custom
    // placeholder paint can use the exact galley origin and clip rectangle.
    // This includes TextEdit's own 4px/2px content margin and centered
    // vertical alignment instead of approximating either from `input_rect`.
    let main_dir = input_ui.layout().main_dir();
    let input_output = input_ui
        .allocate_ui_with_layout(
            input_rect.size(),
            Layout::centered_and_justified(main_dir),
            |ui| {
                egui::TextEdit::singleline(value)
                    .id(field_id)
                    .vertical_align(Align::Center)
                    .frame(false)
                    .show(ui)
            },
        )
        .inner;
    let input = input_output.response;
    if value.is_empty() {
        // egui 0.27 paints built-in hint text at the response's top edge even
        // when typed text is vertically aligned. Paint the placeholder at the
        // empty TextEdit galley's actual origin so hint, typed text, and caret
        // share the exact content baseline and internal margins.
        let hint_color = ui.visuals().weak_text_color();
        let hint_galley = ui.painter().layout_no_wrap(
            hint_text.to_owned(),
            egui::TextStyle::Body.resolve(ui.style()),
            hint_color,
        );
        let hint_position = input_output.galley_pos;
        ui.painter()
            .with_clip_rect(input_output.text_clip_rect)
            .galley(hint_position, hint_galley, hint_color);
    }
    let clear = allocate_clear.then(|| {
        let target = clear_rect;
        let response = if clear_enabled {
            ui.interact(target, field_id.with("clear"), Sense::click())
        } else {
            ui.add_enabled_ui(false, |ui| {
                ui.interact(target, field_id.with("clear"), Sense::click())
            })
            .inner
        };
        if clear_enabled && (response.hovered() || response.has_focus()) {
            ui.painter().rect_filled(
                target.shrink(6.0),
                Rounding::same(5.0),
                colors.active_card_bg,
            );
        }
        if clear_enabled {
            ui.painter().text(
                target.center(),
                egui::Align2::CENTER_CENTER,
                icon_glyph(Icon::Close),
                FontId::proportional(18.0),
                colors.muted_text,
            );
        }
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, format!("Clear {accessible_name}"))
        });
        ui.ctx().accesskit_node_builder(response.id, |builder| {
            builder.set_role(egui::accesskit::Role::Button);
            builder.set_name(format!("Clear {accessible_name}"));
            builder.set_description(format!("Clears the current {accessible_name} query."));
            if !clear_enabled {
                builder.set_disabled();
                builder.set_hidden();
            }
        });
        if clear_enabled {
            paint_focus_ring(ui, &response, Rounding::same(5.0));
            focus_tooltip(ui, &response, &format!("Clear {accessible_name}"));
            response.on_hover_text(format!("Clear {accessible_name}"))
        } else {
            response
        }
    });
    let clear_requested = (clear
        .as_ref()
        .is_some_and(|clear| clear.clicked() || (clear.has_focus() && clear_key_pressed))
        || (had_input_focus && escape_pressed))
        && !value.is_empty();

    ui.ctx().accesskit_node_builder(input.id, |builder| {
        builder.set_name(accessible_name);
        builder.set_description(description);
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Icon {
    Waveform,
    Microphone,
    Models,
    Settings,
    History,
    About,
    Debug,
    Sun,
    Moon,
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
        Icon::Sun => regular::SUN,
        Icon::Moon => regular::MOON,
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

    #[derive(Default)]
    struct NoopAccessKitChangeHandler;

    impl accesskit_consumer::TreeChangeHandler for NoopAccessKitChangeHandler {
        fn node_added(&mut self, _node: &accesskit_consumer::Node<'_>) {}

        fn node_updated(
            &mut self,
            _old_node: &accesskit_consumer::DetachedNode,
            _new_node: &accesskit_consumer::Node<'_>,
        ) {
        }

        fn focus_moved(
            &mut self,
            _old_node: Option<&accesskit_consumer::DetachedNode>,
            _new_node: Option<&accesskit_consumer::Node<'_>>,
            _current_state: &accesskit_consumer::TreeState,
        ) {
        }

        fn node_removed(
            &mut self,
            _node: &accesskit_consumer::DetachedNode,
            _current_state: &accesskit_consumer::TreeState,
        ) {
        }
    }

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
    fn cards_allocate_the_available_content_width() {
        let ctx = egui::Context::default();
        let mut available_width = 0.0;
        let mut card_width = 0.0;
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
                });
            },
        );
        assert!(
            card_width >= available_width - 1.0,
            "card={card_width}, available={available_width}"
        );
    }

    #[test]
    fn focused_custom_control_paints_a_contrast_halo_and_accent_ring() {
        for (visuals, halo) in [
            (egui::Visuals::light(), egui::Color32::BLACK),
            (egui::Visuals::dark(), egui::Color32::WHITE),
        ] {
            let ctx = egui::Context::default();
            ctx.set_visuals(visuals);
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
                egui::epaint::Shape::Rect(rect)
                    if rect.stroke.width == 3.0 && rect.stroke.color == halo
            )));
            assert!(output.shapes.iter().any(|shape| matches!(
                shape.shape,
                egui::epaint::Shape::Rect(rect) if rect.stroke.width == 1.0
            )));
        }
    }

    #[test]
    fn search_field_centers_its_input_and_clear_target_in_the_fixed_height_surface() {
        let ctx = egui::Context::default();
        configure_accessible_style(&ctx);
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
                        ui.available_width(),
                        "centered-search-field",
                        &mut query,
                        "Search models",
                        "Search models",
                        "Filters models as you type.",
                    );
                    surface = response.surface;
                    input = response.input.rect;
                    clear = response
                        .clear
                        .expect("populated search should show a clear target")
                        .rect;
                });
            },
        );
        assert_eq!(input.height(), PRIMARY_TARGET_HEIGHT);
        assert_eq!(clear.height(), PRIMARY_TARGET_HEIGHT);
        assert!(
            (input.center().y - surface.center().y).abs() < f32::EPSILON
                && (clear.center().y - surface.center().y).abs() < f32::EPSILON,
            "the text input and clear affordance must remain vertically centered: surface={surface:?}, input={input:?}, clear={clear:?}"
        );
    }

    #[test]
    fn search_field_placeholder_matches_typed_text_and_caret_content_geometry() {
        let hint = "Search models by name, language, or variant";
        let render = |query: &str| {
            let ctx = egui::Context::default();
            configure_accessible_style(&ctx);
            let mut query = query.to_owned();
            let mut input_rect = egui::Rect::NOTHING;
            let mut cursor_stroke = egui::Stroke::NONE;
            let output = ctx.run(
                egui::RawInput {
                    focused: true,
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(600.0, 120.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let field_id = ui.make_persistent_id("centered-search-placeholder");
                        ui.memory_mut(|memory| memory.request_focus(field_id));
                        cursor_stroke = ui.visuals().text_cursor;
                        input_rect = search_field(
                            ui,
                            ui.available_width(),
                            "centered-search-placeholder",
                            &mut query,
                            "Search models",
                            hint,
                            "Filters models as you type.",
                        )
                        .input
                        .rect;
                    });
                },
            );
            (output, input_rect, cursor_stroke)
        };

        let (empty, input_rect, cursor_stroke) = render("");
        let (clip_rect, placeholder_bounds) = empty
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == hint => Some((
                    shape.clip_rect,
                    egui::Rect::from_min_size(text.pos, text.galley.size()),
                )),
                _ => None,
            })
            .expect("search placeholder should be painted as a galley");
        assert_eq!(clip_rect, input_rect.shrink2(Vec2::new(4.0, 2.0)));
        assert!(clip_rect.contains_rect(placeholder_bounds));

        let (typed, typed_input_rect, typed_cursor_stroke) = render("Search");
        assert_eq!(typed_input_rect, input_rect);
        assert_eq!(typed_cursor_stroke, cursor_stroke);
        let (typed_clip_rect, typed_bounds) = typed
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::epaint::Shape::Text(text) if text.galley.text() == "Search" => Some((
                    shape.clip_rect,
                    egui::Rect::from_min_size(text.pos, text.galley.size()),
                )),
                _ => None,
            })
            .expect("typed search value should be painted as a galley");
        // TextEdit expands its painter clip by one pixel so the caret is not
        // cut off; the underlying content clip used by the placeholder stays
        // exact. Both still share the same content origin and baseline.
        assert_eq!(typed_clip_rect, clip_rect.expand(1.0));
        assert_eq!(typed_bounds.min.y, placeholder_bounds.min.y);
        assert_eq!(typed_bounds.height(), placeholder_bounds.height());

        let cursor_bounds = |output: &egui::FullOutput| {
            output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::epaint::Shape::LineSegment { points, stroke }
                        if *stroke == cursor_stroke
                            && (points[0].x - points[1].x).abs() < f32::EPSILON
                            && clip_rect.contains(points[0])
                            && clip_rect.contains(points[1]) =>
                    {
                        Some(egui::Rect::from_two_pos(points[0], points[1]))
                    }
                    _ => None,
                })
                .expect("focused search field should paint a text caret")
        };
        let empty_cursor = cursor_bounds(&empty);
        let typed_cursor = cursor_bounds(&typed);
        assert!(
            (empty_cursor.center().y - placeholder_bounds.center().y).abs() < 0.1,
            "empty caret and placeholder must share a vertical center: caret={empty_cursor:?}, placeholder={placeholder_bounds:?}"
        );
        assert_eq!(typed_cursor.min.y, empty_cursor.min.y);
        assert_eq!(typed_cursor.max.y, empty_cursor.max.y);
        assert!((typed_cursor.center().y - typed_bounds.center().y).abs() < 0.1);
    }

    #[test]
    fn search_field_paints_an_idle_outline_and_a_stronger_focus_outline() {
        let ctx = egui::Context::default();
        configure_accessible_style(&ctx);
        ctx.set_visuals(egui::Visuals::light());
        let mut query = String::new();
        let raw = || egui::RawInput {
            focused: true,
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                Vec2::new(320.0, 120.0),
            )),
            ..Default::default()
        };
        let idle = ctx.run(raw(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                search_field(
                    ui,
                    ui.available_width(),
                    "search-field-outline",
                    &mut query,
                    "Search models",
                    "Search models",
                    "Filters models as you type.",
                );
            });
        });
        assert!(idle.shapes.iter().any(|shape| matches!(
            shape.shape,
            egui::epaint::Shape::Rect(rect)
                if rect.stroke.width == 1.0 && rect.stroke.color == super::super::theme::ThemePalette::light().border_strong
        )));

        let focused = ctx.run(raw(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let field_id = ui.make_persistent_id("search-field-outline");
                ui.memory_mut(|memory| memory.request_focus(field_id));
                search_field(
                    ui,
                    ui.available_width(),
                    "search-field-outline",
                    &mut query,
                    "Search models",
                    "Search models",
                    "Filters models as you type.",
                );
            });
        });
        assert!(focused.shapes.iter().any(|shape| matches!(
            shape.shape,
            egui::epaint::Shape::Rect(rect)
                if rect.stroke.width == 2.0 && rect.stroke.color == super::super::theme::ThemePalette::light().accent
        )));
    }

    #[test]
    fn search_field_keeps_a_stable_labelled_clear_slot_when_empty() {
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
        let empty_clear = empty_nodes
            .iter()
            .find_map(|(_, node)| (node.name() == Some("Clear Search models")).then_some(node))
            .expect("an empty roomy search should retain its Clear node");
        assert!(empty_clear.is_disabled());
        assert!(empty_clear.is_hidden());
        assert!(
            empty_nodes
                .iter()
                .all(|(_, node)| node.name() != Some(icon_glyph(Icon::Search))),
            "the search glyph is decorative, not a focusable/accessibility node"
        );
    }

    #[test]
    fn search_field_clear_node_is_stable_across_empty_typed_and_cleared_updates() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        configure_accessible_style(&ctx);
        let render = |query: &mut String| {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(320.0, 120.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        search_field(
                            ui,
                            ui.available_width(),
                            "stable-clear-node",
                            query,
                            "Search history",
                            "Search transcript, model, or app",
                            "Filters saved history as you type.",
                        );
                    });
                },
            )
            .platform_output
            .accesskit_update
            .expect("search should update AccessKit")
        };
        let clear_node = |update: &egui::accesskit::TreeUpdate| {
            update
                .nodes
                .iter()
                .find(|(_, node)| node.name() == Some("Clear Search history"))
                .map(|(id, node)| (*id, node.clone()))
                .expect("roomy search should retain the Clear node")
        };

        let mut query = String::new();
        let empty = render(&mut query);
        let (clear_id, empty_clear) = clear_node(&empty);
        assert_eq!(empty_clear.role(), egui::accesskit::Role::Button);
        assert!(empty_clear.is_disabled());
        assert!(empty_clear.is_hidden());
        let mut consumer = accesskit_consumer::Tree::new(empty, true);

        query.push('m');
        let typed = render(&mut query);
        let (typed_id, typed_clear) = clear_node(&typed);
        assert_eq!(typed_id, clear_id);
        assert_eq!(typed_clear.role(), egui::accesskit::Role::Button);
        assert!(!typed_clear.is_disabled());
        assert!(!typed_clear.is_hidden());
        consumer.update_and_process_changes(typed, &mut NoopAccessKitChangeHandler);

        query.clear();
        let cleared = render(&mut query);
        let (cleared_id, cleared_node) = clear_node(&cleared);
        assert_eq!(cleared_id, clear_id);
        assert_eq!(cleared_node.role(), egui::accesskit::Role::Button);
        assert!(cleared_node.is_disabled());
        assert!(cleared_node.is_hidden());
        consumer.update_and_process_changes(cleared, &mut NoopAccessKitChangeHandler);
    }

    #[test]
    fn search_field_omits_the_clear_node_below_the_slot_threshold_in_both_states() {
        for initial in ["", "m"] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            configure_accessible_style(&ctx);
            let mut query = initial.to_owned();
            let output = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let response = search_field(
                        ui,
                        45.0,
                        "compact-stable-clear-node",
                        &mut query,
                        "Search history",
                        "Search transcript, model, or app",
                        "Filters saved history as you type.",
                    );
                    assert!(response.clear.is_none());
                });
            });
            assert!(
                output
                    .platform_output
                    .accesskit_update
                    .expect("compact search should update AccessKit")
                    .nodes
                    .iter()
                    .all(|(_, node)| node.name() != Some("Clear Search history"))
            );
        }
    }

    #[test]
    fn search_field_keeps_its_input_and_visible_clear_target_contained_when_compact() {
        for width in [45.0, 120.0, 220.0] {
            let ctx = egui::Context::default();
            let mut query = "base".to_owned();
            let mut surface = egui::Rect::NOTHING;
            let mut input = egui::Rect::NOTHING;
            let mut clear = None;
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
                        clear = response.clear.map(|response| response.rect);
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
            if width < 88.0 {
                assert!(
                    clear.is_none(),
                    "the visual clear action should yield at pathological width {width}"
                );
            } else {
                let clear = clear.expect("roomy compact fields should expose the clear action");
                assert!(
                    surface.contains_rect(clear),
                    "clear action must remain inside the surface at width {width}: surface={surface:?}, clear={clear:?}"
                );
                assert!(
                    clear.width() >= PRIMARY_TARGET_HEIGHT
                        && clear.height() >= PRIMARY_TARGET_HEIGHT,
                    "clear action must retain a 44px target at width {width}: {clear:?}"
                );
            }
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

        let _ = ctx.run(
            egui::RawInput {
                focused: true,
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(320.0, 120.0),
                )),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    clear_requested = search_field(
                        ui,
                        ui.available_width(),
                        "search-field-escape",
                        &mut query,
                        "Search models",
                        "Search models",
                        "Filters models as you type.",
                    )
                    .clear_requested;
                });
            },
        );
        assert!(
            !clear_requested,
            "a single Escape press must request exactly one clear action"
        );
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
                        .expect("roomy search should render its clear action")
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
