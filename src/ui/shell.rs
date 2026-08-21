use eframe::egui::{self, Color32, Frame, Margin, Rounding, Sense, Stroke, Vec2};

use super::{
    controls::{Icon, focus_tooltip, icon_glyph, paint_focus_ring},
    screens::ScreenAction,
    state::ResolvedTheme,
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
) -> ScreenAction {
    if !current.visible(debug_enabled) {
        *current = AppPage::Transcribe;
    }
    let mode = navigation_mode(ctx.screen_rect().width());
    let colors = theme_palette(ctx);
    let (horizontal_margin, content_width) = match mode {
        NavigationMode::Full => (FULL_SIDEBAR_HORIZONTAL_MARGIN, FULL_SIDEBAR_CONTENT_WIDTH),
        NavigationMode::Compact => (COMPACT_RAIL_HORIZONTAL_MARGIN, COMPACT_RAIL_CONTENT_WIDTH),
    };
    let mut action = ScreenAction::None;
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
                    action = ScreenAction::ToggleResolvedTheme(resolved_theme);
                }
            });
        });
    ctx.accesskit_node_builder(navigation.response.id, |builder| {
        builder.set_role(egui::accesskit::Role::Navigation);
        builder.set_name("Main navigation");
    });
    action
}

fn brand(ui: &mut egui::Ui, mode: NavigationMode, text: Color32, muted: Color32) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(icon_glyph(Icon::Waveform))
                .size(28.0)
                .color(text),
        );
        if mode == NavigationMode::Full {
            ui.add_space(4.0);
            ui.vertical(|ui| {
                let heading = ui.label(
                    egui::RichText::new("Scribe")
                        .size(22.0)
                        .color(text)
                        .strong(),
                );
                ui.ctx().accesskit_node_builder(heading.id, |builder| {
                    builder.set_role(egui::accesskit::Role::Heading);
                });
                ui.label(
                    egui::RichText::new("Local Speech-to-Text")
                        .small()
                        .color(muted),
                );
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
        rect.min + Vec2::new(23.0, 21.0),
        egui::Align2::CENTER_CENTER,
        icon_glyph(icon),
        egui::FontId::proportional(22.0),
        if response.hovered() { text } else { muted },
    );
    paint_focus_ring(ui, &response, Rounding::same(5.0));
    focus_tooltip(ui, &response, accessible_name);
    response.on_hover_text(accessible_name)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn render_navigation(
        ctx: &egui::Context,
        width: f32,
        resolved_theme: ResolvedTheme,
        events: Vec<egui::Event>,
    ) -> (egui::FullOutput, ScreenAction) {
        let mut page = AppPage::Transcribe;
        let mut action = ScreenAction::None;
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(width, 680.0),
                )),
                events,
                ..Default::default()
            },
            |ctx| action = show_navigation(ctx, &mut page, false, resolved_theme),
        );
        (output, action)
    }

    fn named_node<'a>(
        output: &'a egui::FullOutput,
        name: &str,
    ) -> (egui::accesskit::NodeId, &'a egui::accesskit::Node) {
        output
            .platform_output
            .accesskit_update
            .as_ref()
            .expect("navigation should expose an AccessKit update")
            .nodes
            .iter()
            .find(|(_, node)| node.name() == Some(name))
            .map(|(id, node)| (*id, node))
            .unwrap_or_else(|| panic!("missing AccessKit node {name}"))
    }

    #[test]
    fn debug_page_is_fail_closed_when_disabled() {
        let ctx = egui::Context::default();
        let mut page = AppPage::Debug;
        let _ = ctx.run(Default::default(), |ctx| {
            let _ = show_navigation(ctx, &mut page, false, ResolvedTheme::Light);
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
                    let _ = show_navigation(ctx, &mut page, false, ResolvedTheme::Light);
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
                let _ = show_navigation(ctx, &mut page, false, ResolvedTheme::Light);
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
    fn full_and_compact_navigation_expose_bottom_theme_targets() {
        for (width, resolved_theme, name, description) in [
            (
                1_180.0,
                ResolvedTheme::Light,
                "Switch to dark theme",
                "Current appearance is light. Activating saves Dark as the theme.",
            ),
            (
                960.0,
                ResolvedTheme::Dark,
                "Switch to light theme",
                "Current appearance is dark. Activating saves Light as the theme.",
            ),
        ] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            let (output, action) = render_navigation(&ctx, width, resolved_theme, Vec::new());
            assert_eq!(action, ScreenAction::None);
            let (_, node) = named_node(&output, name);
            assert_eq!(node.role(), egui::accesskit::Role::Button);
            assert_eq!(node.description(), Some(description));
            let bounds = node.bounds().expect("theme target bounds");
            assert!(bounds.x1 - bounds.x0 >= 44.0);
            assert!(bounds.y1 - bounds.y0 >= 44.0);
            assert!(bounds.y1 <= 680.0);
            assert!(
                bounds.y0 >= 620.0,
                "theme control should remain in the footer"
            );
        }
    }

    #[test]
    fn expanded_and_compact_theme_toggles_are_icon_only_and_minimal() {
        for width in [1_180.0, 960.0] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            let (output, _) = render_navigation(&ctx, width, ResolvedTheme::Light, Vec::new());
            let (_, node) = named_node(&output, "Switch to dark theme");
            let bounds = node.bounds().expect("theme target bounds");
            assert_eq!(bounds.x1 - bounds.x0, 44.0);
            assert!(!output.shapes.iter().any(|shape| {
                matches!(
                    &shape.shape,
                    egui::epaint::Shape::Text(text)
                        if text.galley.text() == "Dark theme"
                            || text.galley.text() == "Light theme"
                )
            }));
        }
    }

    #[test]
    fn focused_theme_target_accepts_enter_and_space() {
        for key in [egui::Key::Enter, egui::Key::Space] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            let (initial, initial_action) =
                render_navigation(&ctx, 960.0, ResolvedTheme::Light, Vec::new());
            assert_eq!(initial_action, ScreenAction::None);
            let (target, _) = named_node(&initial, "Switch to dark theme");
            let (_, focus_action) = render_navigation(
                &ctx,
                960.0,
                ResolvedTheme::Light,
                vec![egui::Event::AccessKitActionRequest(
                    egui::accesskit::ActionRequest {
                        action: egui::accesskit::Action::Focus,
                        target,
                        data: None,
                    },
                )],
            );
            assert_eq!(focus_action, ScreenAction::None);
            let (_, action) = render_navigation(
                &ctx,
                960.0,
                ResolvedTheme::Light,
                vec![egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
            assert_eq!(
                action,
                ScreenAction::ToggleResolvedTheme(ResolvedTheme::Light)
            );
        }
    }

    #[test]
    fn full_and_compact_theme_targets_accept_pointer_activation() {
        for width in [1_180.0, 960.0] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            let (initial, _) = render_navigation(&ctx, width, ResolvedTheme::Light, Vec::new());
            let (_, node) = named_node(&initial, "Switch to dark theme");
            let bounds = node.bounds().expect("theme target bounds");
            let point = egui::pos2(
                ((bounds.x0 + bounds.x1) / 2.0) as f32,
                ((bounds.y0 + bounds.y1) / 2.0) as f32,
            );
            let (_, pressed) = render_navigation(
                &ctx,
                width,
                ResolvedTheme::Light,
                vec![
                    egui::Event::PointerMoved(point),
                    egui::Event::PointerButton {
                        pos: point,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
            assert_eq!(pressed, ScreenAction::None);
            let (_, released) = render_navigation(
                &ctx,
                width,
                ResolvedTheme::Light,
                vec![
                    egui::Event::PointerMoved(point),
                    egui::Event::PointerButton {
                        pos: point,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
            assert_eq!(
                released,
                ScreenAction::ToggleResolvedTheme(ResolvedTheme::Light)
            );
        }
    }

    #[test]
    fn compact_theme_target_accepts_accesskit_default_action() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let (initial, _) = render_navigation(&ctx, 960.0, ResolvedTheme::Dark, Vec::new());
        let (target, _) = named_node(&initial, "Switch to light theme");
        let (_, action) = render_navigation(
            &ctx,
            960.0,
            ResolvedTheme::Dark,
            vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Default,
                    target,
                    data: None,
                },
            )],
        );
        assert_eq!(
            action,
            ScreenAction::ToggleResolvedTheme(ResolvedTheme::Dark)
        );
    }
}
