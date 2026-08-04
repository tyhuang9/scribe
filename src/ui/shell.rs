use eframe::egui::{
    self, Align, Button, Color32, FontId, Frame, Layout, Margin, RichText, Rounding, Stroke,
};

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
            Self::General => "General",
            Self::Models => "Models",
            Self::History => "History",
            Self::Advanced => "Advanced",
            Self::About => "About",
            Self::Debug => "Debug",
        }
    }

    pub(crate) fn visible(self, debug_enabled: bool) -> bool {
        self != Self::Debug || debug_enabled
    }

    pub(crate) fn normal_pages() -> [Self; 6] {
        [
            Self::Transcribe,
            Self::General,
            Self::Models,
            Self::History,
            Self::Advanced,
            Self::About,
        ]
    }
}

#[derive(Clone, Copy)]
struct NavigationPalette {
    sidebar: Color32,
    selected: Color32,
    text: Color32,
    muted: Color32,
    border: Color32,
    brand: Color32,
}

impl NavigationPalette {
    fn from_context(ctx: &egui::Context) -> Self {
        if ctx.style().visuals.dark_mode {
            Self {
                sidebar: Color32::from_rgb(20, 24, 32),
                selected: Color32::from_rgb(26, 31, 41),
                text: Color32::from_rgb(236, 241, 247),
                muted: Color32::from_rgb(156, 166, 179),
                border: Color32::from_rgb(53, 61, 76),
                brand: Color32::from_rgb(247, 250, 252),
            }
        } else {
            Self {
                sidebar: Color32::WHITE,
                selected: Color32::WHITE,
                text: Color32::from_rgb(29, 33, 42),
                muted: Color32::from_rgb(85, 95, 109),
                border: Color32::from_rgb(203, 213, 225),
                brand: Color32::from_rgb(6, 10, 18),
            }
        }
    }
}

pub(crate) fn show_navigation(ctx: &egui::Context, current: &mut AppPage, debug_enabled: bool) {
    if !current.visible(debug_enabled) {
        *current = AppPage::Transcribe;
    }
    let colors = NavigationPalette::from_context(ctx);
    egui::SidePanel::left("navigation")
        .frame(
            Frame::none()
                .fill(colors.sidebar)
                .stroke(Stroke::new(1.0, colors.border))
                .inner_margin(Margin::symmetric(14.0, 16.0)),
        )
        .resizable(false)
        .exact_width(200.0)
        .show(ctx, |ui| {
            ui.label(
                RichText::new("Scribe")
                    .font(FontId::proportional(20.0))
                    .color(colors.brand)
                    .strong(),
            );
            ui.label(RichText::new("Local-First STT").small().color(colors.muted));
            ui.add_space(18.0);
            for page in AppPage::normal_pages() {
                navigation_button(ui, current, page, colors);
            }
            if debug_enabled {
                ui.add_space(8.0);
                navigation_button(ui, current, AppPage::Debug, colors);
            }
            ui.with_layout(Layout::bottom_up(Align::LEFT), |ui| {
                ui.label(
                    RichText::new("Audio stays on this device")
                        .small()
                        .color(colors.muted),
                );
            });
        });
}

fn navigation_button(
    ui: &mut egui::Ui,
    current: &mut AppPage,
    page: AppPage,
    colors: NavigationPalette,
) {
    let selected = *current == page;
    let response = ui.add_sized(
        [ui.available_width(), 44.0],
        Button::new(RichText::new(page.label()).color(if selected {
            colors.text
        } else {
            colors.muted
        }))
        .fill(if selected {
            colors.selected
        } else {
            colors.sidebar
        })
        .stroke(if selected {
            Stroke::new(1.0, colors.border)
        } else {
            Stroke::NONE
        })
        .rounding(Rounding::same(6.0)),
    );
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::Button, selected, page.label())
    });
    if response.clicked() {
        *current = page;
    }
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
    fn required_navigation_pages_are_present() {
        assert_eq!(
            AppPage::normal_pages().map(AppPage::label),
            [
                "Transcribe",
                "General",
                "Models",
                "History",
                "Advanced",
                "About"
            ]
        );
    }
}
