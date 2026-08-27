use eframe::egui::{self, Color32, Frame, Margin, Rounding, Sense, Stroke, Vec2};

use crate::branding;

use super::{
    controls::{Icon, focus_tooltip, icon_glyph, paint_focus_ring},
    model_picker::{
        ReadyModelPickerAction, close_ready_model_picker_and_restore_focus, show_ready_model_picker,
    },
    screens::ScreenAction,
    state::{ModelViewModel, ResolvedTheme},
    theme_palette,
};

/// Painted width of the full navigation panel, including both horizontal margins.
pub(crate) const FULL_SIDEBAR_WIDTH: f32 = 214.0;
const FULL_SIDEBAR_HORIZONTAL_MARGIN: f32 = 12.0;
const FULL_SIDEBAR_CONTENT_WIDTH: f32 = FULL_SIDEBAR_WIDTH - FULL_SIDEBAR_HORIZONTAL_MARGIN * 2.0;
pub(crate) const COMPACT_RAIL_WIDTH: f32 = 66.0;
const COMPACT_RAIL_HORIZONTAL_MARGIN: f32 = 11.0;
const COMPACT_RAIL_CONTENT_WIDTH: f32 = COMPACT_RAIL_WIDTH - COMPACT_RAIL_HORIZONTAL_MARGIN * 2.0;
pub(crate) const COMPACT_NAV_BREAKPOINT: f32 = 1_000.0;
const SIDEBAR_MODEL_PICKER_ID: &str = "sidebar-ready-model-picker";

#[derive(Clone, Copy, Default)]
pub(crate) struct SidebarModelView<'a> {
    pub selected_model_id: Option<&'a str>,
    pub models: &'a [ModelViewModel],
    pub disabled_reason: Option<&'a str>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum AppPage {
    #[default]
    Transcribe,
    General,
    Models,
    History,
    Advanced,
    About,
    Debug,
}

impl AppPage {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Transcribe => "Transcribe",
            Self::General => "Settings",
            Self::Models => "Models",
            Self::History => "History",
            Self::Advanced => "Advanced",
            Self::About => "About",
            Self::Debug => "Debug",
        }
    }

    fn icon(self) -> Icon {
        match self {
            Self::Transcribe => Icon::Microphone,
            Self::General | Self::Advanced => Icon::Settings,
            Self::Models => Icon::Models,
            Self::History => Icon::History,
            Self::About => Icon::About,
            Self::Debug => Icon::Debug,
        }
    }

    pub(crate) fn visible(self, _debug_enabled: bool) -> bool {
        matches!(
            self,
            Self::Transcribe | Self::Models | Self::History | Self::General
        )
    }

    fn is_settings(self) -> bool {
        self == Self::General
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NavigationMode {
    Full,
    Compact,
}

pub(crate) fn navigation_mode(width: f32) -> NavigationMode {
    if width >= COMPACT_NAV_BREAKPOINT {
        NavigationMode::Full
    } else {
        NavigationMode::Compact
    }
}

pub(crate) fn show_navigation(
    ctx: &egui::Context,
    current: &mut AppPage,
    debug_enabled: bool,
    resolved_theme: ResolvedTheme,
    model: SidebarModelView<'_>,
) -> (ScreenAction, Option<ReadyModelPickerAction>) {
    if !current.visible(debug_enabled) {
        *current = AppPage::Transcribe;
    }
    let mode = navigation_mode(ctx.screen_rect().width());
    let colors = theme_palette(ctx);
    let (horizontal_margin, content_width) = match mode {
        NavigationMode::Full => (FULL_SIDEBAR_HORIZONTAL_MARGIN, FULL_SIDEBAR_CONTENT_WIDTH),
        NavigationMode::Compact => (COMPACT_RAIL_HORIZONTAL_MARGIN, COMPACT_RAIL_CONTENT_WIDTH),
    };
    let mut model_action = None;
    let mut theme_action = ScreenAction::None;
    let navigation = egui::SidePanel::left("navigation")
        .frame(
            Frame::none()
                .fill(colors.sidebar_bg)
                .stroke(Stroke::new(1.0, colors.border))
                .inner_margin(Margin::symmetric(horizontal_margin, 16.0)),
        )
        .resizable(false)
        .exact_width(content_width)
        .show(ctx, |ui| {
            brand(ui, mode, colors.text, colors.muted_text);
            ui.add_space(22.0);
            nav_button(
                ui,
                current,
                AppPage::Transcribe,
                mode,
                colors.active_card_bg,
                colors.text,
                colors.muted_text,
            );
            ui.add_space(4.0);
            nav_button(
                ui,
                current,
                AppPage::Models,
                mode,
                colors.active_card_bg,
                colors.text,
                colors.muted_text,
            );
            ui.add_space(4.0);
            nav_button(
                ui,
                current,
                AppPage::History,
                mode,
                colors.active_card_bg,
                colors.text,
                colors.muted_text,
            );
            ui.add_space(4.0);
            nav_button(
                ui,
                current,
                AppPage::General,
                mode,
                colors.active_card_bg,
                colors.text,
                colors.muted_text,
            );
            ui.add_space(12.0);
            ui.separator();
            ui.add_space(12.0);
            let popup_id = egui::Id::new(SIDEBAR_MODEL_PICKER_ID);
            let restore_disabled_picker_focus = model.disabled_reason.is_some()
                && ui.memory(|memory| memory.is_popup_open(popup_id));
            if restore_disabled_picker_focus {
                close_ready_model_picker_and_restore_focus(
                    ui,
                    popup_id,
                    egui::Id::new("active-model-trigger"),
                );
            }
            let response = active_model_button(
                ui,
                mode,
                model.selected_model_id,
                model.models,
                model.disabled_reason,
                colors.active_card_bg,
                colors.text,
                colors.muted_text,
            );
            if response.enabled() {
                if response.clicked()
                    || (response.has_focus()
                        && ui.input(|input| {
                            input.key_pressed(egui::Key::Enter)
                                || input.key_pressed(egui::Key::Space)
                        }))
                {
                    ui.memory_mut(|memory| memory.toggle_popup(popup_id));
                }
                if let Some(action) = show_ready_model_picker(
                    ui,
                    popup_id,
                    &response,
                    model.selected_model_id,
                    model.models,
                ) {
                    model_action = Some(action);
                }
            } else {
                close_ready_model_picker_and_restore_focus(ui, popup_id, response.id);
                if restore_disabled_picker_focus {
                    ui.memory_mut(|memory| memory.request_focus(response.id));
                }
                ui.ctx()
                    .accesskit_node_builder(response.id, |builder| builder.set_expanded(false));
            }
            ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
                let (icon, accessible_name) = match resolved_theme {
                    ResolvedTheme::Dark => (Icon::Sun, "Switch to light theme"),
                    ResolvedTheme::Light => (Icon::Moon, "Switch to dark theme"),
                };
                let response = theme_icon_button(
                    ui,
                    icon,
                    accessible_name,
                    colors.active_card_bg,
                    colors.text,
                    colors.muted_text,
                );
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Button, accessible_name)
                });
                ui.ctx().accesskit_node_builder(response.id, |builder| {
                    builder.set_role(egui::accesskit::Role::Button);
                    builder.set_name(accessible_name);
                    builder.set_description(format!(
                        "Current appearance is {}. Activating saves {} as the theme.",
                        if resolved_theme == ResolvedTheme::Dark {
                            "dark"
                        } else {
                            "light"
                        },
                        if resolved_theme == ResolvedTheme::Dark {
                            "Light"
                        } else {
                            "Dark"
                        }
                    ));
                });
                if response.clicked()
                    || (response.has_focus()
                        && ui.input(|input| {
                            input.key_pressed(egui::Key::Enter)
                                || input.key_pressed(egui::Key::Space)
                        }))
                {
                    theme_action = ScreenAction::ToggleResolvedTheme(resolved_theme);
                }
            });
        });
    ctx.accesskit_node_builder(navigation.response.id, |builder| {
        builder.set_role(egui::accesskit::Role::Navigation);
        builder.set_name("Main navigation");
    });
    (theme_action, model_action)
}

fn active_model_name<'a>(selected_model_id: Option<&str>, models: &'a [ModelViewModel]) -> &'a str {
    selected_model_id
        .and_then(|id| {
            models
                .iter()
                .find(|model| model.id == id)
                .map(|model| model.display_name.as_str())
        })
        .unwrap_or("No model selected")
}

#[allow(clippy::too_many_arguments)]
fn active_model_button(
    ui: &mut egui::Ui,
    mode: NavigationMode,
    selected_model_id: Option<&str>,
    models: &[ModelViewModel],
    disabled_reason: Option<&str>,
    hover_fill: Color32,
    text: Color32,
    muted: Color32,
) -> egui::Response {
    let name = active_model_name(selected_model_id, models);
    let was_enabled = ui.is_enabled();
    ui.set_enabled(was_enabled && disabled_reason.is_none());
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 44.0), Sense::hover());
    let response = ui.interact(rect, egui::Id::new("active-model-trigger"), Sense::click());
    ui.set_enabled(was_enabled);
    let open = ui.memory(|memory| memory.is_popup_open(egui::Id::new(SIDEBAR_MODEL_PICKER_ID)));
    let fill = if !response.enabled() {
        hover_fill.gamma_multiply(0.2)
    } else if response.hovered() || response.has_focus() || open {
        hover_fill.gamma_multiply(0.55)
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, Rounding::same(5.0), fill);

    match mode {
        NavigationMode::Full => {
            ui.painter().text(
                egui::pos2(rect.min.x + 22.0, rect.center().y),
                egui::Align2::CENTER_CENTER,
                icon_glyph(Icon::Cpu),
                egui::FontId::proportional(22.0),
                if response.enabled() { text } else { muted },
            );
            ui.painter().text(
                egui::pos2(rect.min.x + 44.0, rect.center().y - 8.0),
                egui::Align2::LEFT_CENTER,
                "Active model",
                egui::FontId::proportional(11.0),
                muted,
            );
            let value_rect = egui::Rect::from_min_max(
                egui::pos2(rect.min.x + 44.0, rect.center().y + 1.0),
                egui::pos2(rect.max.x - 22.0, rect.max.y - 2.0),
            );
            let mut value_ui =
                ui.child_ui(value_rect, egui::Layout::left_to_right(egui::Align::Center));
            value_ui.set_enabled(response.enabled());
            value_ui.add(
                egui::Label::new(egui::RichText::new(name).size(13.0).strong()).truncate(true),
            );
            ui.painter().text(
                egui::pos2(rect.max.x - 10.0, rect.center().y),
                egui::Align2::CENTER_CENTER,
                icon_glyph(if open {
                    Icon::ChevronUp
                } else {
                    Icon::ChevronDown
                }),
                egui::FontId::proportional(14.0),
                muted,
            );
        }
        NavigationMode::Compact => {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                icon_glyph(Icon::Cpu),
                egui::FontId::proportional(22.0),
                if response.enabled() { text } else { muted },
            );
        }
    }

    let accessible_name = format!("Change active model: {name}");
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, accessible_name.clone())
    });
    ui.ctx().accesskit_node_builder(response.id, |builder| {
        builder.set_role(egui::accesskit::Role::Button);
        builder.set_name(accessible_name.clone());
        builder
            .set_description(disabled_reason.unwrap_or("Opens the installed ready-model picker."));
        builder.set_expanded(open);
        if !response.enabled() {
            builder.set_disabled();
        }
    });
    paint_focus_ring(ui, &response, Rounding::same(5.0));
    if response.enabled() && response.hovered() {
        response
            .clone()
            .on_hover_cursor(egui::CursorIcon::PointingHand);
    }
    focus_tooltip(ui, &response, &accessible_name);
    response.clone().on_hover_text(accessible_name);
    response
}

fn brand(ui: &mut egui::Ui, mode: NavigationMode, text: Color32, _muted: Color32) {
    ui.horizontal(|ui| {
        branding::show_app_icon(ui, 34.0, mode == NavigationMode::Compact);
        if mode == NavigationMode::Full {
            ui.add_space(6.0);
            let heading = ui.label(
                egui::RichText::new(branding::WORDMARK)
                    .size(24.0)
                    .color(text)
                    .strong(),
            );
            ui.ctx().accesskit_node_builder(heading.id, |builder| {
                builder.set_role(egui::accesskit::Role::Heading);
                builder.set_name("Scribe");
                builder.set_description(branding::TAGLINE);
            });
        }
    });
}

fn nav_button(
    ui: &mut egui::Ui,
    current: &mut AppPage,
    page: AppPage,
    mode: NavigationMode,
    selected: Color32,
    text: Color32,
    muted: Color32,
) {
    let active = if page.is_settings() {
        current.is_settings()
    } else {
        *current == page
    };
    let response = match mode {
        NavigationMode::Full => nav_full_button(ui, page, active, selected, text, muted),
        NavigationMode::Compact => {
            nav_icon_button(ui, page.icon(), page.label(), active, selected, text, muted)
        }
    };
    response
        .widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, active, page.label()));
    ui.ctx().accesskit_node_builder(response.id, |builder| {
        builder.set_role(egui::accesskit::Role::Button);
        builder.set_name(page.label());
    });
    if response.clicked() {
        *current = page;
    }
}

fn nav_full_button(
    ui: &mut egui::Ui,
    page: AppPage,
    active: bool,
    selected: Color32,
    text: Color32,
    muted: Color32,
) -> egui::Response {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 44.0), Sense::click());
    let fill = if active {
        selected
    } else if response.hovered() {
        selected.gamma_multiply(0.45)
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, Rounding::same(5.0), fill);
    ui.painter().text(
        rect.min + Vec2::new(23.0, 21.0),
        egui::Align2::CENTER_CENTER,
        icon_glyph(page.icon()),
        egui::FontId::proportional(22.0),
        if active { text } else { muted },
    );
    ui.painter().text(
        rect.min + Vec2::new(46.0, 21.0),
        egui::Align2::LEFT_CENTER,
        page.label(),
        egui::FontId::proportional(15.0),
        if active { text } else { muted },
    );
    paint_focus_ring(ui, &response, Rounding::same(5.0));
    response
}

fn nav_icon_button(
    ui: &mut egui::Ui,
    icon: Icon,
    accessible_name: &str,
    active: bool,
    selected: Color32,
    text: Color32,
    muted: Color32,
) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 44.0), Sense::click());
    let fill = if active {
        selected
    } else if response.hovered() {
        selected.gamma_multiply(0.45)
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, Rounding::same(5.0), fill);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        icon_glyph(icon),
        egui::FontId::proportional(22.0),
        if active || response.hovered() {
            text
        } else {
            muted
        },
    );
    paint_focus_ring(ui, &response, Rounding::same(5.0));
    focus_tooltip(ui, &response, accessible_name);
    response.on_hover_text(accessible_name)
}

fn theme_icon_button(
    ui: &mut egui::Ui,
    icon: Icon,
    accessible_name: &str,
    hover_fill: Color32,
    text: Color32,
    muted: Color32,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(44.0), Sense::click());
    let fill = if response.hovered() {
        hover_fill.gamma_multiply(0.45)
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, Rounding::same(5.0), fill);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        icon_glyph(icon),
        egui::FontId::proportional(22.0),
        if response.hovered() || response.has_focus() {
            text
        } else {
            muted
        },
    );
    paint_focus_ring(ui, &response, Rounding::same(5.0));
    focus_tooltip(ui, &response, accessible_name);
    response.on_hover_text(accessible_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready_sidebar_models() -> Vec<ModelViewModel> {
        vec![
            ModelViewModel {
                id: "base.en".into(),
                display_name: "Whisper Base English with a deliberately long display name".into(),
                installed: true,
                ready: true,
                ..Default::default()
            },
            ModelViewModel {
                id: "tiny.en".into(),
                display_name: "Whisper Tiny".into(),
                installed: true,
                ready: true,
                ..Default::default()
            },
            ModelViewModel {
                id: "broken.en".into(),
                display_name: "Broken model".into(),
                installed: true,
                ready: false,
                ..Default::default()
            },
        ]
    }

    fn render_navigation(
        ctx: &egui::Context,
        width: f32,
        height: f32,
        models: &[ModelViewModel],
        selected_model_id: Option<&str>,
        disabled_reason: Option<&str>,
        events: Vec<egui::Event>,
    ) -> (egui::FullOutput, Option<ReadyModelPickerAction>) {
        let mut action = None;
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(width, height),
                )),
                focused: true,
                events,
                ..Default::default()
            },
            |ctx| {
                let mut page = AppPage::History;
                let (_, model_action) = show_navigation(
                    ctx,
                    &mut page,
                    false,
                    ResolvedTheme::Light,
                    SidebarModelView {
                        selected_model_id,
                        models,
                        disabled_reason,
                    },
                );
                action = model_action;
            },
        );
        (output, action)
    }

    fn key_event(key: egui::Key) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }
    }

    #[test]
    fn debug_page_is_fail_closed_when_disabled() {
        let ctx = egui::Context::default();
        let mut page = AppPage::Debug;
        let _ = ctx.run(Default::default(), |ctx| {
            show_navigation(
                ctx,
                &mut page,
                false,
                ResolvedTheme::Light,
                SidebarModelView::default(),
            );
        });
        assert_eq!(page, AppPage::Transcribe);
    }

    #[test]
    fn navigation_switches_at_the_package_compact_breakpoint() {
        assert_eq!(navigation_mode(1_180.0), NavigationMode::Full);
        assert_eq!(navigation_mode(960.0), NavigationMode::Compact);
    }

    #[test]
    fn full_sidebar_painted_width_includes_frame_margins() {
        let ctx = egui::Context::default();
        let mut painted_width = 0.0;
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(1_180.0, 815.0),
                )),
                ..Default::default()
            },
            |ctx| {
                let colors = theme_palette(ctx);
                let panel = egui::SidePanel::left("width-test")
                    .frame(
                        Frame::none()
                            .fill(colors.sidebar_bg)
                            .stroke(Stroke::new(1.0, colors.border))
                            .inner_margin(Margin::symmetric(FULL_SIDEBAR_HORIZONTAL_MARGIN, 16.0)),
                    )
                    .resizable(false)
                    .exact_width(FULL_SIDEBAR_CONTENT_WIDTH)
                    .show(ctx, |_ui| {});
                painted_width = panel.response.rect.width();
            },
        );
        assert!((painted_width - FULL_SIDEBAR_WIDTH).abs() < f32::EPSILON);
    }

    #[test]
    fn compact_rail_painted_width_preserves_a_44_point_target() {
        assert_eq!(COMPACT_RAIL_CONTENT_WIDTH, 44.0);
        let ctx = egui::Context::default();
        let mut painted_width = 0.0;
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(960.0, 680.0),
                )),
                ..Default::default()
            },
            |ctx| {
                let colors = theme_palette(ctx);
                let panel = egui::SidePanel::left("compact-width-test")
                    .frame(
                        Frame::none()
                            .fill(colors.sidebar_bg)
                            .stroke(Stroke::new(1.0, colors.border))
                            .inner_margin(Margin::symmetric(COMPACT_RAIL_HORIZONTAL_MARGIN, 16.0)),
                    )
                    .resizable(false)
                    .exact_width(COMPACT_RAIL_CONTENT_WIDTH)
                    .show(ctx, |ui| {
                        let response = nav_icon_button(
                            ui,
                            Icon::Microphone,
                            "Transcribe",
                            true,
                            colors.active_card_bg,
                            colors.text,
                            colors.muted_text,
                        );
                        assert!(response.rect.width() >= 44.0);
                        assert!(response.rect.height() >= 44.0);
                    });
                painted_width = panel.response.rect.width();
            },
        );
        assert!((painted_width - COMPACT_RAIL_WIDTH).abs() < f32::EPSILON);
    }

    #[test]
    fn full_and_compact_navigation_expose_named_controls() {
        for width in [1_180.0, 960.0] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            let mut page = AppPage::Transcribe;
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(width, 680.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    show_navigation(
                        ctx,
                        &mut page,
                        false,
                        ResolvedTheme::Light,
                        SidebarModelView::default(),
                    );
                },
            );
            let update = output.platform_output.accesskit_update.unwrap();
            for expected in ["Transcribe", "Models", "History", "Settings"] {
                assert!(
                    update
                        .nodes
                        .iter()
                        .any(|(_, node)| node.role() == egui::accesskit::Role::Button
                            && node.name() == Some(expected))
                );
            }
        }
    }

    #[test]
    fn navigation_app_icon_preserves_full_and_compact_brand_semantics() {
        for (width, expects_image, expects_heading) in
            [(1_180.0, false, true), (960.0, true, false)]
        {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            let mut page = AppPage::Transcribe;
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(width, 680.0),
                    )),
                    ..Default::default()
                },
                |ctx| {
                    show_navigation(
                        ctx,
                        &mut page,
                        false,
                        ResolvedTheme::Light,
                        SidebarModelView::default(),
                    );
                },
            );
            let update = output.platform_output.accesskit_update.unwrap();
            assert_eq!(
                update.nodes.iter().any(|(_, node)| {
                    node.role() == egui::accesskit::Role::Image
                        && node.name() == Some("Scribe logo")
                }),
                expects_image
            );
            assert_eq!(
                update.nodes.iter().any(|(_, node)| {
                    node.role() == egui::accesskit::Role::Heading && node.name() == Some("Scribe")
                }),
                expects_heading
            );
        }
    }

    #[test]
    fn navigation_exposes_only_the_four_primary_destinations() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut page = AppPage::Transcribe;
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(960.0, 680.0),
                )),
                ..Default::default()
            },
            |ctx| {
                show_navigation(
                    ctx,
                    &mut page,
                    false,
                    ResolvedTheme::Light,
                    SidebarModelView::default(),
                );
            },
        );
        let update = output.platform_output.accesskit_update.unwrap();
        let names = update
            .nodes
            .iter()
            .filter(|(_, node)| node.role() == egui::accesskit::Role::Button)
            .map(|(_, node)| node.name())
            .collect::<Vec<_>>();
        assert!(names.contains(&Some("Transcribe")));
        assert!(names.contains(&Some("Models")));
        assert!(names.contains(&Some("History")));
        assert!(names.contains(&Some("Settings")));
        assert!(!names.contains(&Some("More navigation")));
        assert!(!names.contains(&Some("About")));
        assert!(!names.contains(&Some("Advanced")));
        assert!(!names.contains(&Some("Debug")));
    }

    #[test]
    fn full_and_compact_sidebars_expose_a_distinct_44_point_active_model_trigger() {
        assert_ne!(icon_glyph(Icon::Cpu), icon_glyph(Icon::Models));
        let models = ready_sidebar_models();
        for (width, expected_width) in [(1_180.0, 190.0), (960.0, 44.0)] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            crate::ui::controls::configure_accessible_style(&ctx);
            ctx.memory_mut(|memory| memory.request_focus(egui::Id::new("active-model-trigger")));
            let (output, action) = render_navigation(
                &ctx,
                width,
                680.0,
                &models,
                Some("base.en"),
                None,
                Vec::new(),
            );
            assert_eq!(action, None);
            let update = output.platform_output.accesskit_update.unwrap();
            let expected_name =
                "Change active model: Whisper Base English with a deliberately long display name";
            let trigger = update
                .nodes
                .iter()
                .find_map(|(_, node)| {
                    (node.role() == egui::accesskit::Role::Button
                        && node.name() == Some(expected_name))
                    .then_some(node)
                })
                .expect("active model trigger");
            let bounds = trigger.bounds().expect("active model trigger bounds");
            assert!((bounds.width() - expected_width).abs() <= 1.0);
            assert!((bounds.height() - 44.0).abs() <= 1.0);
            assert_eq!(trigger.is_expanded(), Some(false));
            assert!(
                update
                    .nodes
                    .iter()
                    .filter(|(_, node)| node.name() == Some(expected_name))
                    .count()
                    >= 2,
                "the focused trigger must expose its full-name tooltip"
            );
            assert!(update.nodes.iter().any(|(_, node)| {
                node.role() == egui::accesskit::Role::Button && node.name() == Some("Models")
            }));
            if width >= COMPACT_NAV_BREAKPOINT {
                let value = update
                    .nodes
                    .iter()
                    .find_map(|(_, node)| {
                        (node.name()
                            == Some("Whisper Base English with a deliberately long display name"))
                        .then_some(node)
                    })
                    .expect("full sidebar active-model value");
                assert!(value.bounds().expect("value bounds").x1 <= bounds.x1 - 20.0);
            }
        }
    }

    #[test]
    fn sidebar_picker_supports_keyboard_current_ready_only_manage_escape_and_focus_restore() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        crate::ui::controls::configure_accessible_style(&ctx);
        let models = ready_sidebar_models();
        let trigger_id = egui::Id::new("active-model-trigger");
        ctx.memory_mut(|memory| memory.request_focus(trigger_id));
        let (opened, action) = render_navigation(
            &ctx,
            1_180.0,
            680.0,
            &models,
            Some("base.en"),
            None,
            vec![key_event(egui::Key::Enter)],
        );
        assert_eq!(action, None);
        let update = opened.platform_output.accesskit_update.unwrap();
        assert!(update.nodes.iter().any(|(_, node)| {
            node.name()
                == Some("Whisper Base English with a deliberately long display name, current model")
                && node.is_selected() == Some(true)
        }));
        assert!(update.nodes.iter().any(|(_, node)| {
            node.name() == Some("Select Whisper Tiny") && node.is_selected() == Some(false)
        }));
        assert!(
            !update
                .nodes
                .iter()
                .any(|(_, node)| node.name() == Some("Broken model"))
        );
        let manage_target = update
            .nodes
            .iter()
            .find_map(|(id, node)| (node.name() == Some("Manage models…")).then_some(*id))
            .expect("Manage models action");

        let option_id = egui::Id::new(SIDEBAR_MODEL_PICKER_ID).with(("option", "tiny.en"));
        ctx.memory_mut(|memory| memory.request_focus(option_id));
        let (_, action) = render_navigation(
            &ctx,
            1_180.0,
            680.0,
            &models,
            Some("base.en"),
            None,
            vec![key_event(egui::Key::Space)],
        );
        assert_eq!(
            action,
            Some(ReadyModelPickerAction::Select("tiny.en".into()))
        );
        assert_eq!(ctx.memory(|memory| memory.focused()), Some(trigger_id));

        ctx.memory_mut(|memory| {
            memory.open_popup(egui::Id::new(SIDEBAR_MODEL_PICKER_ID));
        });
        let (_, action) = render_navigation(
            &ctx,
            1_180.0,
            680.0,
            &models,
            Some("base.en"),
            None,
            vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Default,
                    target: manage_target,
                    data: None,
                },
            )],
        );
        assert_eq!(action, Some(ReadyModelPickerAction::ManageModels));
        assert_ne!(
            ctx.memory(|memory| memory.focused()),
            Some(trigger_id),
            "Manage models must not leave focus on the sidebar trigger after navigation"
        );

        ctx.memory_mut(|memory| {
            memory.open_popup(egui::Id::new(SIDEBAR_MODEL_PICKER_ID));
            memory.request_focus(option_id);
        });
        let (_, action) = render_navigation(
            &ctx,
            1_180.0,
            680.0,
            &models,
            Some("base.en"),
            None,
            vec![key_event(egui::Key::Escape)],
        );
        assert_eq!(action, None);
        assert!(
            !ctx.memory(|memory| { memory.is_popup_open(egui::Id::new(SIDEBAR_MODEL_PICKER_ID)) })
        );
        assert_eq!(ctx.memory(|memory| memory.focused()), Some(trigger_id));
    }

    #[test]
    fn sidebar_model_trigger_preserves_no_model_and_disabled_semantics() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        crate::ui::controls::configure_accessible_style(&ctx);
        let (no_model, action) = render_navigation(&ctx, 960.0, 680.0, &[], None, None, Vec::new());
        assert_eq!(action, None);
        assert!(
            no_model
                .platform_output
                .accesskit_update
                .unwrap()
                .nodes
                .iter()
                .any(|(_, node)| { node.name() == Some("Change active model: No model selected") })
        );

        let reason = "Wait for the active recording before changing models.";
        let models = ready_sidebar_models();
        let disabled_ctx = egui::Context::default();
        disabled_ctx.enable_accesskit();
        let popup_id = egui::Id::new(SIDEBAR_MODEL_PICKER_ID);
        disabled_ctx.memory_mut(|memory| {
            memory.open_popup(popup_id);
            memory.request_focus(popup_id.with(("option", "tiny.en")));
        });
        let (disabled, action) = render_navigation(
            &disabled_ctx,
            960.0,
            680.0,
            &models,
            Some("base.en"),
            Some(reason),
            vec![key_event(egui::Key::Enter)],
        );
        assert_eq!(action, None);
        let update = disabled.platform_output.accesskit_update.unwrap();
        let trigger = update
            .nodes
            .iter()
            .find_map(|(_, node)| {
                (node.name()
                    == Some(
                        "Change active model: Whisper Base English with a deliberately long display name",
                    ))
                .then_some(node)
            })
            .expect("disabled active model trigger");
        assert!(trigger.is_disabled());
        assert_eq!(trigger.description(), Some(reason));
        assert_eq!(trigger.is_expanded(), Some(false));
        assert!(!disabled_ctx.memory(|memory| memory.is_popup_open(popup_id)));
        assert_eq!(
            disabled_ctx.memory(|memory| memory.focused()),
            Some(egui::Id::new("active-model-trigger"))
        );
        assert!(
            !update
                .nodes
                .iter()
                .any(|(_, node)| node.name() == Some("Select Whisper Tiny"))
        );
    }

    #[test]
    fn sidebar_picker_closes_outside_and_contains_a_long_list_in_the_viewport() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        crate::ui::controls::configure_accessible_style(&ctx);
        let models = (0..24)
            .map(|index| ModelViewModel {
                id: format!("ready-{index}"),
                display_name: format!("Ready model {index}"),
                installed: true,
                ready: true,
                ..Default::default()
            })
            .collect::<Vec<_>>();
        let popup_id = egui::Id::new(SIDEBAR_MODEL_PICKER_ID);
        ctx.memory_mut(|memory| memory.open_popup(popup_id));
        let (_, action) = render_navigation(
            &ctx,
            960.0,
            320.0,
            &models,
            Some("ready-0"),
            None,
            Vec::new(),
        );
        assert_eq!(action, None);
        let layout = ctx
            .data(|data| {
                data.get_temp::<crate::ui::model_picker::ReadyModelPickerLayout>(
                    popup_id.with("test-layout"),
                )
            })
            .expect("picker layout diagnostics");
        let screen = ctx.screen_rect();
        let popup_rect = ctx
            .memory(|memory| memory.area_rect(popup_id))
            .expect("picker area bounds");
        assert!(
            screen.contains_rect(popup_rect),
            "picker {popup_rect:?} must remain within screen {screen:?}"
        );
        let options_viewport = layout.options_viewport.expect("options viewport");
        let content_size = layout.options_content_size.expect("options content size");
        assert!(content_size.y > options_viewport.height());
        assert!(options_viewport.height() <= 160.0 + 1.0);

        let outside = egui::pos2(700.0, 300.0);
        ctx.memory_mut(|memory| {
            memory.request_focus(popup_id.with(("option", "ready-0")));
        });
        let _ = render_navigation(
            &ctx,
            960.0,
            320.0,
            &models,
            Some("ready-0"),
            None,
            vec![
                egui::Event::PointerMoved(outside),
                egui::Event::PointerButton {
                    pos: outside,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        let _ = render_navigation(
            &ctx,
            960.0,
            320.0,
            &models,
            Some("ready-0"),
            None,
            vec![
                egui::Event::PointerMoved(outside),
                egui::Event::PointerButton {
                    pos: outside,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
        assert!(!ctx.memory(|memory| memory.is_popup_open(popup_id)));
        assert_eq!(
            ctx.memory(|memory| memory.focused()),
            Some(egui::Id::new("active-model-trigger"))
        );
    }

    #[test]
    fn theme_toggle_is_icon_only_and_accesskit_activatable_without_displacing_model_picker() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let models = ready_sidebar_models();
        let render = |events| {
            let mut theme_action = ScreenAction::None;
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(960.0, 680.0),
                    )),
                    focused: true,
                    events,
                    ..Default::default()
                },
                |ctx| {
                    let mut page = AppPage::History;
                    (theme_action, _) = show_navigation(
                        ctx,
                        &mut page,
                        false,
                        ResolvedTheme::Light,
                        SidebarModelView {
                            selected_model_id: Some("base.en"),
                            models: &models,
                            disabled_reason: None,
                        },
                    );
                },
            );
            (output, theme_action)
        };

        let (initial, action) = render(Vec::new());
        assert_eq!(action, ScreenAction::None);
        let update = initial
            .platform_output
            .accesskit_update
            .expect("sidebar controls should expose an AccessKit update");
        let theme_target = update
            .nodes
            .iter()
            .find_map(|(id, node)| (node.name() == Some("Switch to dark theme")).then_some(*id))
            .expect("icon-only theme target");
        assert!(update.nodes.iter().any(|(_, node)| {
            node.name()
                == Some("Change active model: Whisper Base English with a deliberately long display name")
        }));

        let (_, action) = render(vec![egui::Event::AccessKitActionRequest(
            egui::accesskit::ActionRequest {
                action: egui::accesskit::Action::Default,
                target: theme_target,
                data: None,
            },
        )]);
        assert_eq!(
            action,
            ScreenAction::ToggleResolvedTheme(ResolvedTheme::Light)
        );
    }
}
