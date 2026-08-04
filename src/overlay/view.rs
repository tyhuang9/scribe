use std::time::Duration;

use eframe::egui::{self, Color32, RichText, Sense, Stroke, ViewportClass};

use super::controller::{OverlayMode, OverlayPhase, OverlayViewState};
use super::platform::{
    CapturedTarget, OverlayPosition, OverlayWindowBounds, OverlayWindowSpec, harden_overlay_window,
    overlay_window_bounds,
};

pub const OVERLAY_VIEWPORT_KEY: &str = "scribe-dictation-overlay";
pub const OVERLAY_WINDOW_TITLE: &str = "Scribe Dictation Overlay";

const LIVE_WIDTH: f32 = 440.0;
const LIVE_HEIGHT: f32 = 140.0;
const MINIMAL_WIDTH: f32 = 276.0;
const MINIMAL_HEIGHT: f32 = 52.0;
const WINDOW_MARGIN: f32 = 24.0;

pub fn show_overlay_viewport(
    context: &egui::Context,
    state: &OverlayViewState,
    target: Option<&CapturedTarget>,
    position: OverlayPosition,
) {
    let spec = window_spec(state.mode);
    let bounds = overlay_window_bounds(target, spec, position);
    let builder = viewport_builder(state, bounds, context.pixels_per_point());
    let visible = state.is_visible();

    context.show_viewport_immediate(
        overlay_viewport_id(),
        builder,
        |overlay_context, viewport_class| {
            if visible && viewport_class == ViewportClass::Immediate {
                render_overlay(overlay_context, state);
            }
        },
    );

    harden_overlay_window(OVERLAY_WINDOW_TITLE, target, spec, position, visible);
}

pub fn overlay_viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of(OVERLAY_VIEWPORT_KEY)
}

fn window_spec(mode: OverlayMode) -> OverlayWindowSpec {
    match mode {
        OverlayMode::Live => OverlayWindowSpec {
            width_points: LIVE_WIDTH,
            height_points: LIVE_HEIGHT,
            margin_points: WINDOW_MARGIN,
        },
        OverlayMode::Minimal | OverlayMode::Off => OverlayWindowSpec {
            width_points: MINIMAL_WIDTH,
            height_points: MINIMAL_HEIGHT,
            margin_points: WINDOW_MARGIN,
        },
    }
}

fn viewport_builder(
    state: &OverlayViewState,
    physical_bounds: Option<OverlayWindowBounds>,
    pixels_per_point: f32,
) -> egui::ViewportBuilder {
    let spec = window_spec(state.mode);
    let size = egui::vec2(spec.width_points, spec.height_points);
    let mut builder = egui::ViewportBuilder::default()
        .with_title(OVERLAY_WINDOW_TITLE)
        .with_inner_size(size)
        .with_min_inner_size(size)
        .with_max_inner_size(size)
        .with_resizable(false)
        .with_decorations(false)
        .with_transparent(true)
        .with_active(false)
        .with_visible(state.is_visible())
        .with_taskbar(false)
        .with_close_button(false)
        .with_minimize_button(false)
        .with_maximize_button(false)
        .with_always_on_top()
        .with_mouse_passthrough(true);

    if let Some(bounds) = physical_bounds {
        let pixels_per_point = pixels_per_point.max(0.1);
        builder = builder.with_position(egui::pos2(
            bounds.x as f32 / pixels_per_point,
            bounds.y as f32 / pixels_per_point,
        ));
    }
    builder
}

fn render_overlay(context: &egui::Context, state: &OverlayViewState) {
    let background = Color32::from_rgba_unmultiplied(20, 25, 34, 245);
    egui::CentralPanel::default()
        .frame(egui::Frame::none().fill(Color32::TRANSPARENT))
        .show(context, |ui| {
            egui::Frame::none()
                .fill(background)
                .rounding(egui::Rounding::same(14.0))
                .stroke(Stroke::new(1.0, Color32::from_rgb(59, 70, 88)))
                .inner_margin(egui::Margin::symmetric(14.0, 10.0))
                .show(ui, |ui| {
                    ui.set_min_size(ui.available_size());
                    render_status_row(ui, state);
                    if state.mode == OverlayMode::Live {
                        ui.add_space(8.0);
                        render_live_content(ui, state);
                    }
                });
        });
}

fn render_status_row(ui: &mut egui::Ui, state: &OverlayViewState) {
    ui.horizontal(|ui| {
        let status_color = phase_color(state.phase);
        let (dot_rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), Sense::hover());
        ui.painter()
            .circle_filled(dot_rect.center(), 4.0, status_color);

        let status = ui.label(
            RichText::new(state.phase.label())
                .strong()
                .color(Color32::WHITE),
        );
        mark_polite_live_region(ui.ctx(), status.id);

        ui.add_space(6.0);
        render_level_meter(ui, state);

        if let Some(elapsed) = state.elapsed {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(format_elapsed(elapsed)).color(Color32::LIGHT_GRAY));
            });
        }
    });
}

fn render_level_meter(ui: &mut egui::Ui, state: &OverlayViewState) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(74.0, 24.0), Sense::hover());
    let level = state.audio_level.rms.max(state.audio_level.peak * 0.7);
    let bars = 7;
    let gap = 3.0;
    let bar_width = (rect.width() - gap * (bars - 1) as f32) / bars as f32;

    for index in 0..bars {
        let threshold = (index + 1) as f32 / bars as f32;
        let active = level >= threshold * 0.78;
        let normalized_height = if active { threshold } else { 0.22 };
        let height = (rect.height() * normalized_height).max(4.0);
        let left = rect.left() + index as f32 * (bar_width + gap);
        let bar = egui::Rect::from_min_max(
            egui::pos2(left, rect.bottom() - height),
            egui::pos2(left + bar_width, rect.bottom()),
        );
        let color = if active {
            Color32::from_rgb(91, 201, 158)
        } else {
            Color32::from_rgb(64, 76, 94)
        };
        ui.painter().rect_filled(bar, 2.0, color);
    }
}

fn render_live_content(ui: &mut egui::Ui, state: &OverlayViewState) {
    if let Some(error) = &state.error {
        let suffix = if error.recoverable {
            " You can retry."
        } else {
            ""
        };
        let response = ui.label(
            RichText::new(format!("{}{suffix}", error.message))
                .color(Color32::from_rgb(255, 174, 168)),
        );
        mark_polite_live_region(ui.ctx(), response.id);
        return;
    }

    if state.transcript.committed.is_empty() && state.transcript.tentative.is_empty() {
        return;
    }

    let mut text = egui::text::LayoutJob::default();
    text.append(
        &state.transcript.committed,
        0.0,
        egui::TextFormat {
            color: Color32::WHITE,
            ..Default::default()
        },
    );
    text.append(
        &state.transcript.tentative,
        0.0,
        egui::TextFormat {
            color: Color32::from_rgb(162, 173, 190),
            italics: true,
            ..Default::default()
        },
    );
    text.wrap.max_width = ui.available_width();
    let response = ui.label(text);
    mark_polite_live_region(ui.ctx(), response.id);
}

fn mark_polite_live_region(context: &egui::Context, id: egui::Id) {
    context.accesskit_node_builder(id, |builder| {
        builder.set_live(egui::accesskit::Live::Polite);
    });
}

fn phase_color(phase: OverlayPhase) -> Color32 {
    match phase {
        OverlayPhase::Error => Color32::from_rgb(239, 108, 104),
        OverlayPhase::Success => Color32::from_rgb(91, 201, 158),
        OverlayPhase::Hidden => Color32::TRANSPARENT,
        _ => Color32::from_rgb(105, 169, 255),
    }
}

fn format_elapsed(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_overlay_builder_is_precreatable_and_non_interactive() {
        let state = OverlayViewState::default();
        let builder = viewport_builder(&state, None, 1.0);

        assert_eq!(builder.title.as_deref(), Some(OVERLAY_WINDOW_TITLE));
        assert_eq!(builder.visible, Some(false));
        assert_eq!(builder.active, Some(false));
        assert_eq!(builder.taskbar, Some(false));
        assert_eq!(builder.decorations, Some(false));
        assert_eq!(builder.resizable, Some(false));
        assert_eq!(builder.mouse_passthrough, Some(true));
        assert_eq!(
            builder.window_level,
            Some(egui::viewport::WindowLevel::AlwaysOnTop)
        );
        assert_eq!(builder.inner_size, builder.min_inner_size);
        assert_eq!(builder.inner_size, builder.max_inner_size);
    }

    #[test]
    fn live_and_minimal_modes_have_deterministic_sizes() {
        let live = window_spec(OverlayMode::Live);
        let minimal = window_spec(OverlayMode::Minimal);

        assert_eq!((live.width_points, live.height_points), (440.0, 140.0));
        assert_eq!((minimal.width_points, minimal.height_points), (276.0, 52.0));
    }

    #[test]
    fn elapsed_format_does_not_depend_on_wall_clock() {
        assert_eq!(format_elapsed(Duration::from_secs(65)), "1:05");
    }

    #[test]
    fn transcript_and_status_are_polite_live_regions_without_controls() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let state = OverlayViewState {
            phase: OverlayPhase::Listening,
            transcript: super::super::controller::OverlayTranscript {
                committed: "hello".to_owned(),
                tentative: " world".to_owned(),
                revision: 1,
            },
            ..OverlayViewState::default()
        };

        let output = context.run(egui::RawInput::default(), |context| {
            render_overlay(context, &state);
        });
        let update = output.platform_output.accesskit_update.unwrap();

        assert!(update.nodes.iter().any(|(_, node)| {
            node.live() == Some(egui::accesskit::Live::Polite)
                && node.name().is_some_and(|name| name.contains("hello"))
        }));
        assert!(
            update
                .nodes
                .iter()
                .all(|(_, node)| node.role() != egui::accesskit::Role::Button)
        );
    }
}
