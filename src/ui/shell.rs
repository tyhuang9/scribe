use eframe::egui::{self, Align, Color32, Frame, Layout, Margin, Rounding, Sense, Stroke, Vec2};

use super::{
    controls::{Icon, icon_glyph, paint_focus_ring},
    theme_palette,
};

pub(crate) const FULL_SIDEBAR_WIDTH: f32 = 214.0;
pub(crate) const COMPACT_RAIL_WIDTH: f32 = 66.0;
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

    pub(crate) fn visible(self, debug_enabled: bool) -> bool {
        self != Self::Debug || debug_enabled
    }

    fn is_settings(self) -> bool {
        matches!(self, Self::General | Self::Advanced)
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

pub(crate) fn show_navigation(ctx: &egui::Context, current: &mut AppPage, debug_enabled: bool) {
    if !current.visible(debug_enabled) {
        *current = AppPage::Transcribe;
    }
    let mode = navigation_mode(ctx.screen_rect().width());
    let colors = theme_palette(ctx);
    let navigation = egui::SidePanel::left("navigation")
        .frame(
            Frame::none()
                .fill(colors.sidebar_bg)
                .stroke(Stroke::new(1.0, colors.border))
                .inner_margin(Margin::symmetric(12.0, 16.0)),
        )
        .resizable(false)
        .exact_width(match mode {
            NavigationMode::Full => FULL_SIDEBAR_WIDTH,
            NavigationMode::Compact => COMPACT_RAIL_WIDTH,
        })
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
            ui.add_space(8.0);
            secondary_navigation(
                ui,
                current,
                debug_enabled,
                mode,
                colors.active_card_bg,
                colors.text,
                colors.muted_text,
            );
            ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
                nav_button(
                    ui,
                    current,
                    AppPage::General,
                    mode,
                    colors.active_card_bg,
                    colors.text,
                    colors.muted_text,
                );
            });
        });
    ctx.accesskit_node_builder(navigation.response.id, |builder| {
        builder.set_role(egui::accesskit::Role::Navigation);
        builder.set_name("Main navigation");
    });
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

fn secondary_navigation(
    ui: &mut egui::Ui,
    current: &mut AppPage,
    debug_enabled: bool,
    mode: NavigationMode,
    selected: Color32,
    text: Color32,
    muted: Color32,
) {
    if mode == NavigationMode::Full {
        let disclosure = egui::CollapsingHeader::new("More")
            .id_source("secondary-navigation")
            .default_open(false)
            .show(ui, |ui| {
                nav_button(ui, current, AppPage::History, mode, selected, text, muted);
                nav_button(ui, current, AppPage::About, mode, selected, text, muted);
                nav_button(ui, current, AppPage::Advanced, mode, selected, text, muted);
                if debug_enabled {
                    nav_button(ui, current, AppPage::Debug, mode, selected, text, muted);
                }
            });
        ui.ctx()
            .accesskit_node_builder(disclosure.header_response.id, |builder| {
                builder.set_expanded(disclosure.body_response.is_some());
            });
    } else {
        let more = nav_icon_button(ui, Icon::About, "More navigation", false, selected, muted);
        if more.clicked() {
            ui.memory_mut(|memory| memory.toggle_popup(more.id));
        }
        let expanded = ui.memory(|memory| memory.is_popup_open(more.id));
        ui.ctx().accesskit_node_builder(more.id, |builder| {
            builder.set_role(egui::accesskit::Role::Button);
            builder.set_name("More navigation");
            builder.set_expanded(expanded);
        });
        egui::popup_below_widget(ui, more.id, &more, |ui| {
            if menu_item(ui, "History") {
                *current = AppPage::History;
            }
            if menu_item(ui, "About") {
                *current = AppPage::About;
            }
            if menu_item(ui, "Advanced") {
                *current = AppPage::Advanced;
            }
            if debug_enabled && menu_item(ui, "Debug") {
                *current = AppPage::Debug;
            }
        });
    }
}

fn menu_item(ui: &mut egui::Ui, label: &str) -> bool {
    ui.add_sized([160.0, 40.0], egui::Button::new(label))
        .clicked()
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
            nav_icon_button(ui, page.icon(), page.label(), active, selected, muted)
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
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 42.0), Sense::click());
    let fill = if active {
        selected
    } else if response.hovered() {
        Color32::from_white_alpha(18)
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
    muted: Color32,
) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(ui.available_width(), 42.0), Sense::click());
    let fill = if active {
        selected
    } else if response.hovered() {
        Color32::from_white_alpha(18)
    } else {
        Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, Rounding::same(5.0), fill);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        icon_glyph(icon),
        egui::FontId::proportional(22.0),
        muted,
    );
    paint_focus_ring(ui, &response, Rounding::same(5.0));
    response.on_hover_text(accessible_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_page_is_fail_closed_when_disabled() {
        let ctx = egui::Context::default();
        let mut page = AppPage::Debug;
        let _ = ctx.run(Default::default(), |ctx| {
            show_navigation(ctx, &mut page, false)
        });
        assert_eq!(page, AppPage::Transcribe);
    }

    #[test]
    fn navigation_switches_at_the_package_compact_breakpoint() {
        assert_eq!(navigation_mode(1_180.0), NavigationMode::Full);
        assert_eq!(navigation_mode(960.0), NavigationMode::Compact);
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
                |ctx| show_navigation(ctx, &mut page, false),
            );
            let update = output.platform_output.accesskit_update.unwrap();
            for expected in ["Transcribe", "Models", "Settings"] {
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
    fn compact_more_control_has_name_and_expanded_semantics() {
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
            |ctx| show_navigation(ctx, &mut page, false),
        );
        assert!(
            output
                .platform_output
                .accesskit_update
                .unwrap()
                .nodes
                .iter()
                .any(|(_, node)| {
                    node.role() == egui::accesskit::Role::Button
                        && node.name() == Some("More navigation")
                        && node.is_expanded() == Some(false)
                })
        );
    }
}
