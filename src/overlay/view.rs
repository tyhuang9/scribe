use std::{cell::Cell, time::Duration};

use eframe::egui::{self, Color32, RichText, Sense, Stroke, ViewportClass};
use unicode_segmentation::UnicodeSegmentation;

use super::controller::{
    OverlayMode, OverlayPhase, OverlayPresentation, OverlayRecovery, OverlayViewState,
};
use super::platform::{
    CapturedTarget, OverlayHardeningProfile, OverlayPosition, OverlayWindowBounds,
    OverlayWindowSpec, harden_overlay_window, harden_overlay_window_at, overlay_window_bounds,
};
use crate::{transcription::SessionId, ui::theme_palette};

pub const OVERLAY_VIEWPORT_KEY: &str = "scribe-dictation-overlay";
pub const OVERLAY_WINDOW_TITLE: &str = "Scribe Dictation Overlay";
pub const OVERLAY_CONTROL_VIEWPORT_KEY: &str = "scribe-dictation-overlay-cancel";
pub const OVERLAY_CONTROL_WINDOW_TITLE: &str = "Scribe Dictation Overlay Cancel";
const CANCEL_RECORDING_LABEL: &str = "Cancel recording and discard it";

pub(super) const LIVE_WIDTH: f32 = 600.0;
pub(super) const LIVE_HEIGHT: f32 = 62.0;
pub(super) const MINIMAL_WIDTH: f32 = 320.0;
pub(super) const MINIMAL_HEIGHT: f32 = 52.0;
const WINDOW_MARGIN: f32 = 24.0;
pub(super) const CONTROL_SIZE: f32 = 44.0;
const CONTROL_CONTENT_GAP: f32 = 8.0;
const CAPSULE_HORIZONTAL_INSET: f32 = 8.0;
const LIVE_CONTROL_HORIZONTAL_INSET: f32 = 16.0;
const LIVE_CAPSULE_VERTICAL_INSET: f32 = 8.0;
const COMPACT_CAPSULE_VERTICAL_INSET: f32 = 4.0;
const LIVE_CAPSULE_SHADOW_BLUR: f32 = 6.0;
const COMPACT_CAPSULE_SHADOW_BLUR: f32 = 2.0;
const LIVE_CAPSULE_SHADOW_OFFSET_Y: f32 = 2.0;
const COMPACT_CAPSULE_SHADOW_OFFSET_Y: f32 = 1.0;
const COMPACT_METER_WIDTH: f32 = 38.0;
const LIVE_WAVEFORM_SIZE: f32 = 30.0;
const MAX_PREVIEW_GRAPHEMES: usize = 512;
const MAX_MESSAGE_GRAPHEMES: usize = 256;
const LIVE_PREVIEW_ROWS: usize = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OverlayAction {
    Abandon(SessionId),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct OverlayViewportOutput {
    pub presented: bool,
    pub action: Option<OverlayAction>,
}

pub fn show_overlay_viewport(
    context: &egui::Context,
    state: &OverlayViewState,
    target: Option<&CapturedTarget>,
    position: OverlayPosition,
    presentation: OverlayPresentation,
) -> OverlayViewportOutput {
    #[cfg(target_os = "windows")]
    {
        super::native_windows::show_overlay_viewport(context, state, target, position, presentation)
    }

    #[cfg(not(target_os = "windows"))]
    {
        show_eframe_overlay_viewport(context, state, target, position, presentation)
    }
}

#[cfg_attr(
    target_os = "windows",
    expect(
        dead_code,
        reason = "Windows dispatches to the native layered renderer; this fallback remains compiled for cross-platform contract tests"
    )
)]
fn show_eframe_overlay_viewport(
    context: &egui::Context,
    state: &OverlayViewState,
    target: Option<&CapturedTarget>,
    position: OverlayPosition,
    presentation: OverlayPresentation,
) -> OverlayViewportOutput {
    let spec = window_spec(state.mode);
    let bounds = overlay_window_bounds(target, spec, position);
    let requested_visible = state.is_visible() && presentation.permits_background_overlay();
    // A hidden viewport is created first. It is allowed to become visible only
    // after the native adapter verifies the no-activate/tool-window styles and
    // non-activating placement on the actual HWND.
    let hardened = harden_overlay_window(
        OVERLAY_WINDOW_TITLE,
        target,
        spec,
        position,
        requested_visible,
    );
    let visible = requested_visible && hardened;
    let builder = viewport_builder(state, bounds, context.pixels_per_point(), visible);

    context.show_viewport_immediate(
        overlay_viewport_id(),
        builder,
        |overlay_context, viewport_class| {
            if visible && viewport_class == ViewportClass::Immediate {
                render_overlay(overlay_context, state);
            }
        },
    );

    let post_creation_hardened =
        harden_overlay_window(OVERLAY_WINDOW_TITLE, target, spec, position, visible);
    let presented = visible && post_creation_hardened;
    if visible && !post_creation_hardened {
        context.send_viewport_cmd_to(overlay_viewport_id(), egui::ViewportCommand::Visible(false));
    }
    let action = Cell::new(None);
    let control_visible = presented && is_cancellable(state);
    let control_bounds = bounds.map(|display| control_window_bounds(display, spec));
    let control_hardened = control_bounds.is_some_and(|bounds| {
        harden_overlay_window_at(
            OVERLAY_CONTROL_WINDOW_TITLE,
            bounds,
            control_visible,
            OverlayHardeningProfile::NonActivatingControl,
        )
    });
    context.show_viewport_immediate(
        control_viewport_id(),
        control_viewport_builder(
            control_bounds,
            context.pixels_per_point(),
            control_visible && control_hardened,
        ),
        |control_context, viewport_class| {
            if control_visible
                && control_hardened
                && viewport_class == ViewportClass::Immediate
                && render_cancel_control(control_context)
                && let Some(session_id) = state.session_id
            {
                action.set(Some(OverlayAction::Abandon(session_id)));
            }
        },
    );
    let control_presented = control_visible
        && control_hardened
        && control_bounds.is_some_and(|bounds| {
            harden_overlay_window_at(
                OVERLAY_CONTROL_WINDOW_TITLE,
                bounds,
                true,
                OverlayHardeningProfile::NonActivatingControl,
            )
        });
    if control_visible && !control_presented {
        context.send_viewport_cmd_to(control_viewport_id(), egui::ViewportCommand::Visible(false));
    }
    OverlayViewportOutput {
        presented,
        action: action.get(),
    }
}

pub fn overlay_viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of(OVERLAY_VIEWPORT_KEY)
}

pub fn control_viewport_id() -> egui::ViewportId {
    egui::ViewportId::from_hash_of(OVERLAY_CONTROL_VIEWPORT_KEY)
}

pub(super) fn is_cancellable(state: &OverlayViewState) -> bool {
    state.session_id.is_some()
        && matches!(
            state.phase,
            OverlayPhase::Preparing | OverlayPhase::Listening
        )
}

pub(super) fn control_window_bounds(
    display: OverlayWindowBounds,
    display_spec: OverlayWindowSpec,
) -> OverlayWindowBounds {
    let scale = display.width as f32 / display_spec.width_points;
    let size = (CONTROL_SIZE * scale).round() as i32;
    let control_inset = if display_spec.width_points == LIVE_WIDTH {
        LIVE_CONTROL_HORIZONTAL_INSET
    } else {
        CAPSULE_HORIZONTAL_INSET
    };
    let horizontal_inset = (control_inset * scale).round() as i32;
    OverlayWindowBounds {
        x: display.x + display.width - horizontal_inset - size,
        y: display.y + (display.height - size) / 2,
        width: size,
        height: size,
    }
}

fn control_viewport_builder(
    bounds: Option<OverlayWindowBounds>,
    pixels_per_point: f32,
    visible: bool,
) -> egui::ViewportBuilder {
    let mut builder = egui::ViewportBuilder::default()
        .with_title(OVERLAY_CONTROL_WINDOW_TITLE)
        .with_inner_size(egui::vec2(CONTROL_SIZE, CONTROL_SIZE))
        .with_min_inner_size(egui::vec2(CONTROL_SIZE, CONTROL_SIZE))
        .with_max_inner_size(egui::vec2(CONTROL_SIZE, CONTROL_SIZE))
        .with_resizable(false)
        .with_decorations(false)
        .with_transparent(true)
        .with_active(false)
        .with_visible(visible)
        .with_taskbar(false)
        .with_always_on_top()
        .with_mouse_passthrough(false);
    if let Some(bounds) = bounds {
        let ppp = pixels_per_point.max(0.1);
        builder = builder.with_position(egui::pos2(bounds.x as f32 / ppp, bounds.y as f32 / ppp));
    }
    builder
}

fn render_cancel_control(context: &egui::Context) -> bool {
    let mut clicked = false;
    let colors = overlay_colors(context);
    egui::CentralPanel::default()
        .frame(egui::Frame::none().fill(Color32::TRANSPARENT))
        .show(context, |ui| {
            ui.centered_and_justified(|ui| {
                let response = ui.add_sized(
                    ui.available_size(),
                    egui::Button::new(
                        egui::RichText::new(egui_phosphor::regular::X)
                            .size(20.0)
                            .color(colors.text),
                    )
                    .frame(false),
                );
                response.widget_info(|| {
                    egui::WidgetInfo::labeled(egui::WidgetType::Button, CANCEL_RECORDING_LABEL)
                });
                ui.ctx().accesskit_node_builder(response.id, |builder| {
                    builder.set_name(CANCEL_RECORDING_LABEL);
                });
                if response.on_hover_text(CANCEL_RECORDING_LABEL).clicked() {
                    clicked = true;
                }
            });
        });
    clicked
}

#[derive(Clone, Copy)]
struct OverlayColors {
    surface: Color32,
    border: Color32,
    inner_highlight: Color32,
    text: Color32,
    muted_text: Color32,
    waveform: Color32,
    meter_active: Color32,
    meter_inactive: Color32,
    error: Color32,
    warning: Color32,
    shadow: Color32,
}

fn overlay_colors(context: &egui::Context) -> OverlayColors {
    let palette = theme_palette(context);
    if context.style().visuals.dark_mode {
        OverlayColors {
            // This app-owned tint remains deterministic while its alpha lets
            // the underlying desktop content show through.
            // The egui fallback does not composite native shadow rings beneath
            // the fill, so this tint lands near the reference RGB 89/90/95 on
            // its light backdrop without the native renderer's compensation.
            surface: Color32::from_rgba_unmultiplied(31, 32, 39, 184),
            border: Color32::from_rgba_unmultiplied(220, 229, 242, 36),
            inner_highlight: Color32::from_rgba_unmultiplied(255, 255, 255, 18),
            text: palette.text,
            muted_text: Color32::from_rgb(210, 210, 216),
            // The supplied reference uses a purple brand mark. This slightly
            // lighter overlay-specific token preserves that appearance while
            // keeping the non-text mark at 3:1 over the translucent surface.
            waveform: Color32::from_rgb(178, 162, 255),
            meter_active: palette.success,
            meter_inactive: Color32::from_rgb(180, 180, 188),
            error: palette.error,
            warning: palette.warning,
            shadow: Color32::from_black_alpha(96),
        }
    } else {
        OverlayColors {
            surface: Color32::from_rgba_unmultiplied(248, 250, 253, 228),
            border: Color32::from_rgba_unmultiplied(35, 47, 66, 64),
            inner_highlight: Color32::from_rgba_unmultiplied(255, 255, 255, 156),
            text: palette.text,
            muted_text: Color32::from_rgb(65, 75, 90),
            waveform: palette.recording_waveform,
            meter_active: palette.success_text,
            meter_inactive: Color32::from_rgb(100, 112, 132),
            error: palette.error_text,
            warning: palette.warning,
            shadow: Color32::from_black_alpha(54),
        }
    }
}

pub(super) fn window_spec(mode: OverlayMode) -> OverlayWindowSpec {
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
    visible: bool,
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
        .with_visible(visible)
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
    let colors = overlay_colors(context);
    let vertical_inset = capsule_vertical_inset(state.mode);
    let rounding = (window_spec(state.mode).height_points - vertical_inset * 2.0) / 2.0;
    let shadow_blur = capsule_shadow_blur(state.mode);
    let shadow_offset_y = capsule_shadow_offset_y(state.mode);
    egui::CentralPanel::default()
        .frame(egui::Frame::none().fill(Color32::TRANSPARENT))
        .show(context, |ui| {
            let capsule_rect = painted_capsule_bounds(ui.max_rect(), state.mode);
            ui.allocate_ui_at_rect(capsule_rect, |ui| {
                egui::Frame::none()
                    .fill(colors.surface)
                    .rounding(egui::Rounding::same(rounding))
                    .stroke(Stroke::new(1.0, colors.border))
                    .shadow(egui::epaint::Shadow {
                        offset: egui::vec2(0.0, shadow_offset_y),
                        blur: shadow_blur,
                        spread: 0.0,
                        color: colors.shadow,
                    })
                    .inner_margin(egui::Margin::symmetric(14.0, 8.0))
                    .show(ui, |ui| {
                        let available = ui.available_size();
                        let content_size = egui::vec2(
                            (available.x - reserved_control_width()).max(1.0),
                            available.y,
                        );
                        ui.allocate_ui_with_layout(
                            content_size,
                            egui::Layout::top_down(egui::Align::LEFT),
                            |content| {
                                content.set_min_size(content_size);
                                content.set_max_width(content_size.x);
                                if state.mode == OverlayMode::Live {
                                    render_live_status_row(content, state, colors);
                                } else {
                                    render_compact_status_row(content, state, colors);
                                }
                            },
                        );
                    });
            });
            // The translucent surface must be painted before this subtle top edge;
            // otherwise the frame fill would cover the highlight entirely.
            paint_capsule_inner_highlight(ui, capsule_rect, rounding, colors.inner_highlight);
        });
}

fn capsule_vertical_inset(mode: OverlayMode) -> f32 {
    if mode == OverlayMode::Live {
        LIVE_CAPSULE_VERTICAL_INSET
    } else {
        COMPACT_CAPSULE_VERTICAL_INSET
    }
}

fn capsule_shadow_blur(mode: OverlayMode) -> f32 {
    if mode == OverlayMode::Live {
        LIVE_CAPSULE_SHADOW_BLUR
    } else {
        COMPACT_CAPSULE_SHADOW_BLUR
    }
}

fn capsule_shadow_offset_y(mode: OverlayMode) -> f32 {
    if mode == OverlayMode::Live {
        LIVE_CAPSULE_SHADOW_OFFSET_Y
    } else {
        COMPACT_CAPSULE_SHADOW_OFFSET_Y
    }
}

fn painted_capsule_bounds(viewport: egui::Rect, mode: OverlayMode) -> egui::Rect {
    viewport.shrink2(egui::vec2(
        CAPSULE_HORIZONTAL_INSET,
        capsule_vertical_inset(mode),
    ))
}

fn reserved_control_width() -> f32 {
    CONTROL_SIZE + CONTROL_CONTENT_GAP + CAPSULE_HORIZONTAL_INSET
}

fn paint_capsule_inner_highlight(
    ui: &egui::Ui,
    capsule: egui::Rect,
    rounding: f32,
    color: Color32,
) {
    let horizontal_inset = (rounding * 0.45).min(capsule.width() / 4.0);
    let y = capsule.top() + 1.0;
    ui.painter().line_segment(
        [
            egui::pos2(capsule.left() + horizontal_inset, y),
            egui::pos2(capsule.right() - horizontal_inset, y),
        ],
        Stroke::new(0.5, color),
    );
}

fn render_compact_status_row(ui: &mut egui::Ui, state: &OverlayViewState, colors: OverlayColors) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let status_color = phase_color(state.phase);
        let (dot_rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), Sense::hover());
        ui.painter()
            .circle_filled(dot_rect.center(), 4.0, status_color);

        let label = if state.phase == OverlayPhase::Listening {
            "Scribe is recording"
        } else {
            state.phase.label()
        };
        ui.label(RichText::new(label).strong().size(13.0).color(colors.text));
        render_level_meter(ui, state, COMPACT_METER_WIDTH, colors);

        if let Some(elapsed) = state.elapsed {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(format_elapsed(elapsed))
                        .color(colors.muted_text)
                        .size(12.0)
                        .monospace(),
                );
            });
        }
    });
}

fn render_live_status_row(ui: &mut egui::Ui, state: &OverlayViewState, colors: OverlayColors) {
    ui.horizontal_centered(|ui| {
        render_brand_mark(ui, state, colors);
        ui.add_space(8.0);
        let elapsed = state
            .elapsed
            .map(format_elapsed)
            .unwrap_or_else(|| "00:00".into());
        ui.label(RichText::new(elapsed).size(13.0).color(colors.muted_text));
        if state.live_preview_available {
            render_divider(ui, colors);
            let response = ui.label(live_preview_layout(ui, state, colors, ui.available_width()));
            ui.ctx().accesskit_node_builder(response.id, |builder| {
                builder.set_name(live_accessible_text(state));
                if state.error.is_some() || state.notice.is_some() {
                    builder.set_live(egui::accesskit::Live::Polite);
                }
            });
            if let Some(announcement_text) = live_overlay_announcement(state) {
                let announcement = ui.allocate_response(egui::Vec2::ZERO, Sense::hover());
                ui.ctx().accesskit_node_builder(announcement.id, |builder| {
                    builder.set_role(egui::accesskit::Role::StaticText);
                    builder.set_name(announcement_text);
                    builder.set_live(egui::accesskit::Live::Polite);
                });
            }
        }
    });
}

pub(super) fn live_overlay_announcement(state: &OverlayViewState) -> Option<&str> {
    if !state.live_preview_available || state.error.is_some() || state.notice.is_some() {
        return None;
    }
    if let Some(announcement) = state.transcript_announcement.as_deref() {
        return Some(announcement);
    }
    (state.phase != OverlayPhase::Hidden).then(|| state.phase.label())
}

fn render_divider(ui: &mut egui::Ui, colors: OverlayColors) {
    ui.add_space(2.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(1.0, 24.0), Sense::hover());
    ui.painter().line_segment(
        [rect.center_top(), rect.center_bottom()],
        Stroke::new(1.0, colors.border),
    );
    ui.add_space(6.0);
}

fn render_brand_mark(ui: &mut egui::Ui, state: &OverlayViewState, colors: OverlayColors) {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(LIVE_WAVEFORM_SIZE, LIVE_WAVEFORM_SIZE),
        Sense::hover(),
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, "Scribe"));
    ui.ctx().accesskit_node_builder(response.id, |builder| {
        builder.set_role(egui::accesskit::Role::Image);
        builder.set_name("Scribe");
        builder.set_description(if state.phase == OverlayPhase::Listening {
            "Scribe is recording"
        } else {
            state.phase.label()
        });
    });
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        egui_phosphor::regular::WAVEFORM,
        egui::FontId::proportional(27.0),
        colors.waveform,
    );
}

fn render_level_meter(
    ui: &mut egui::Ui,
    state: &OverlayViewState,
    width: f32,
    colors: OverlayColors,
) {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 20.0), Sense::hover());
    let level = state.audio_level.rms.max(state.audio_level.peak * 0.7);
    response.widget_info(|| {
        let mut info = egui::WidgetInfo::labeled(
            egui::WidgetType::ProgressIndicator,
            "Microphone input level",
        );
        info.value = Some((level * 100.0).round() as f64);
        info
    });
    let bars = 4;
    let gap = 2.0;
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
            colors.meter_active
        } else {
            colors.meter_inactive
        };
        ui.painter().rect_filled(bar, 2.0, color);
    }
}

fn live_preview_layout(
    ui: &egui::Ui,
    state: &OverlayViewState,
    colors: OverlayColors,
    max_width: f32,
) -> egui::text::LayoutJob {
    if let Some(error) = &state.error {
        let message = error_message(error);
        return message_layout_for_rows(ui, &message, colors.error, max_width, LIVE_PREVIEW_ROWS);
    }

    if let Some(notice) = &state.notice {
        return message_layout_for_rows(ui, notice, colors.warning, max_width, LIVE_PREVIEW_ROWS);
    }

    if state.transcript.committed.is_empty() && state.transcript.tentative.is_empty() {
        return message_layout_for_rows(
            ui,
            state.phase.label(),
            colors.muted_text,
            max_width,
            LIVE_PREVIEW_ROWS,
        );
    }

    transcript_layout_for_rows(
        ui,
        &state.transcript.committed,
        &state.transcript.tentative,
        max_width,
        LIVE_PREVIEW_ROWS,
        colors,
    )
}

pub(super) fn live_accessible_text(state: &OverlayViewState) -> String {
    if let Some(error) = &state.error {
        return error_message(error);
    }
    if let Some(notice) = &state.notice {
        return notice.clone();
    }
    if state.transcript.committed.is_empty() && state.transcript.tentative.is_empty() {
        return state.phase.label().to_owned();
    }
    if state.transcript.tentative.is_empty() {
        format!("Committed transcript: {}", state.transcript.committed)
    } else {
        format!(
            "Committed transcript: {}. Live estimate, may change: {}",
            state.transcript.committed, state.transcript.tentative
        )
    }
}

fn error_message(error: &super::controller::OverlayError) -> String {
    let suffix = match error.recovery {
        OverlayRecovery::None => "",
        OverlayRecovery::Retry => " You can retry.",
        OverlayRecovery::WaitForPreview => " Wait for the current preview worker to exit.",
    };
    format!("{}{suffix}", error.message)
}

fn transcript_layout_for_rows(
    ui: &egui::Ui,
    committed: &str,
    tentative: &str,
    max_width: f32,
    max_rows: usize,
    colors: OverlayColors,
) -> egui::text::LayoutJob {
    let full = transcript_layout(committed, tentative, max_width, colors);
    let total_graphemes = full.text.graphemes(true).count();
    let mut low = 0;
    let mut high = total_graphemes.min(MAX_PREVIEW_GRAPHEMES);
    let mut best = head_layout_job(&full, 0);
    while low <= high {
        let keep = low + (high - low) / 2;
        let candidate = head_layout_job(&full, keep);
        let rows = ui.fonts(|fonts| fonts.layout_job(candidate.clone()).rows.len());
        if rows <= max_rows {
            best = candidate;
            low = keep.saturating_add(1);
        } else if keep == 0 {
            break;
        } else {
            high = keep - 1;
        }
    }
    best
}

fn message_layout_for_rows(
    ui: &egui::Ui,
    message: &str,
    color: Color32,
    max_width: f32,
    max_rows: usize,
) -> egui::text::LayoutJob {
    let mut full = egui::text::LayoutJob::default();
    full.append(
        message,
        0.0,
        egui::TextFormat {
            color,
            ..Default::default()
        },
    );
    full.wrap.max_width = max_width;
    let total_graphemes = full.text.graphemes(true).count();
    let mut low = 0;
    let mut high = total_graphemes.min(MAX_MESSAGE_GRAPHEMES);
    let mut best = head_layout_job(&full, 0);
    while low <= high {
        let keep = low + (high - low) / 2;
        let candidate = head_layout_job(&full, keep);
        let rows = ui.fonts(|fonts| fonts.layout_job(candidate.clone()).rows.len());
        if rows <= max_rows {
            best = candidate;
            low = keep.saturating_add(1);
        } else if keep == 0 {
            break;
        } else {
            high = keep - 1;
        }
    }
    best
}

fn head_layout_job(full: &egui::text::LayoutJob, keep_graphemes: usize) -> egui::text::LayoutJob {
    let total_graphemes = full.text.graphemes(true).count();
    if keep_graphemes >= total_graphemes {
        return full.clone();
    }
    let end = full
        .text
        .grapheme_indices(true)
        .nth(keep_graphemes)
        .map_or(full.text.len(), |(index, _)| index);
    let mut result = full.clone();
    result.text.clear();
    result.sections.clear();
    let format = full
        .sections
        .first()
        .map(|section| section.format.clone())
        .unwrap_or_default();
    if end > 0 {
        result.append(&full.text[..end], 0.0, format.clone());
    }
    result.append("…", 0.0, format);
    result
}

fn transcript_layout(
    committed: &str,
    tentative: &str,
    max_width: f32,
    colors: OverlayColors,
) -> egui::text::LayoutJob {
    let mut text = egui::text::LayoutJob::default();
    text.append(
        committed,
        0.0,
        egui::TextFormat {
            color: colors.muted_text,
            ..Default::default()
        },
    );
    if !committed.is_empty()
        && !tentative.is_empty()
        && !committed.ends_with(char::is_whitespace)
        && !tentative.starts_with(char::is_whitespace)
        && !tentative.starts_with(is_left_binding_punctuation)
    {
        text.append(" ", 0.0, egui::TextFormat::default());
    }
    text.append(
        tentative,
        0.0,
        egui::TextFormat {
            color: colors.muted_text,
            ..Default::default()
        },
    );
    text.wrap.max_width = max_width;
    text
}

fn is_left_binding_punctuation(character: char) -> bool {
    matches!(
        character,
        '.' | ',' | '!' | '?' | ':' | ';' | '%' | ')' | ']' | '}' | '…'
    )
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
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_overlay_builder_is_precreatable_and_non_interactive() {
        let state = OverlayViewState::default();
        let builder = viewport_builder(&state, None, 1.0, false);

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

        assert_eq!((live.width_points, live.height_points), (600.0, 62.0));
        assert!((560.0..=640.0).contains(&live.width_points));
        assert!((56.0..=64.0).contains(&live.height_points));
        assert_eq!((minimal.width_points, minimal.height_points), (320.0, 52.0));
    }

    #[test]
    fn cancel_control_is_interactive_and_only_available_while_capturing() {
        let builder = control_viewport_builder(None, 1.0, true);
        assert_eq!(builder.title.as_deref(), Some(OVERLAY_CONTROL_WINDOW_TITLE));
        assert_eq!(builder.mouse_passthrough, Some(false));
        assert_eq!(builder.active, Some(false));
        assert_eq!(
            builder.inner_size,
            Some(egui::vec2(CONTROL_SIZE, CONTROL_SIZE))
        );
        let preparing = OverlayViewState {
            session_id: Some(SessionId(4)),
            phase: OverlayPhase::Preparing,
            ..Default::default()
        };
        assert!(is_cancellable(&preparing));
        assert!(!is_cancellable(&OverlayViewState {
            phase: OverlayPhase::Finalizing,
            ..preparing
        }));
    }

    #[test]
    fn rendered_cancel_control_has_exact_accessible_name_and_full_bounds() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(CONTROL_SIZE, CONTROL_SIZE),
            )),
            ..Default::default()
        };
        let output = context.run(input, |context| {
            assert!(!render_cancel_control(context));
        });
        let nodes = output.platform_output.accesskit_update.unwrap().nodes;
        let control = nodes
            .iter()
            .find_map(|(_, node)| {
                (node.role() == egui::accesskit::Role::Button
                    && node.name() == Some(CANCEL_RECORDING_LABEL))
                .then_some(node)
            })
            .expect("cancel control should be an accessible button");
        let bounds = control
            .bounds()
            .expect("cancel control should expose bounds");
        assert!(bounds.x1 - bounds.x0 >= f64::from(CONTROL_SIZE));
        assert!(bounds.y1 - bounds.y0 >= f64::from(CONTROL_SIZE));
    }

    #[test]
    fn control_bounds_are_inset_from_the_display_right_edge() {
        let control = control_window_bounds(
            OverlayWindowBounds {
                x: 100,
                y: 20,
                width: 320,
                height: 52,
            },
            window_spec(OverlayMode::Minimal),
        );
        assert_eq!(control.x + control.width, 412);
        assert_eq!((control.width, control.height), (44, 44));
    }

    #[test]
    fn live_cancel_control_uses_the_reference_inset_and_shared_center_at_each_dpi() {
        let spec = window_spec(OverlayMode::Live);
        for scale in [1.0, 1.25, 1.5, 2.0] {
            let display = OverlayWindowBounds {
                x: -240,
                y: 80,
                width: (spec.width_points * scale).round() as i32,
                height: (spec.height_points * scale).round() as i32,
            };
            let control = control_window_bounds(display, spec);
            let expected_size = (CONTROL_SIZE * scale).round() as i32;
            let expected_inset = (LIVE_CONTROL_HORIZONTAL_INSET * scale).round() as i32;
            assert_eq!(
                (control.width, control.height),
                (expected_size, expected_size)
            );
            assert_eq!(
                display.x + display.width - control.x - control.width,
                expected_inset
            );
            assert!(
                (control.y * 2 + control.height - (display.y * 2 + display.height)).abs() <= 1,
                "control must remain within half a physical pixel of the display center at {scale}x"
            );
        }
    }

    #[test]
    fn painted_capsules_keep_shadow_room_and_enclose_cancel_controls() {
        for mode in [OverlayMode::Live, OverlayMode::Minimal] {
            let spec = window_spec(mode);
            let viewport = egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(spec.width_points, spec.height_points),
            );
            let capsule = painted_capsule_bounds(viewport, mode);
            assert_eq!(capsule.left(), CAPSULE_HORIZONTAL_INSET);
            assert_eq!(
                capsule.right(),
                spec.width_points - CAPSULE_HORIZONTAL_INSET
            );
            assert_eq!(capsule.top(), capsule_vertical_inset(mode));
            assert_eq!(
                capsule.bottom(),
                spec.height_points - capsule_vertical_inset(mode)
            );
            let shadow_bounds = capsule
                .expand(capsule_shadow_blur(mode))
                .translate(egui::vec2(0.0, capsule_shadow_offset_y(mode)));
            assert!(
                viewport.contains_rect(shadow_bounds),
                "the {mode:?} capsule shadow must stay inside its transparent viewport"
            );

            let control = control_window_bounds(
                OverlayWindowBounds {
                    x: 0,
                    y: 0,
                    width: spec.width_points as i32,
                    height: spec.height_points as i32,
                },
                spec,
            );
            assert!(control.x >= capsule.left() as i32);
            assert!(control.x + control.width <= capsule.right() as i32);
            assert!(control.y >= capsule.top() as i32);
            assert!(control.y + control.height <= capsule.bottom() as i32);
        }
    }

    #[test]
    fn cancel_hit_rect_stays_inside_the_painted_capsule_at_each_dpi() {
        for mode in [OverlayMode::Live, OverlayMode::Minimal] {
            let spec = window_spec(mode);
            for scale in [1.0, 1.25, 1.5, 2.0] {
                let display = OverlayWindowBounds {
                    x: -240,
                    y: 80,
                    width: (spec.width_points * scale).round() as i32,
                    height: (spec.height_points * scale).round() as i32,
                };
                let control = control_window_bounds(display, spec);
                let horizontal_inset = (CAPSULE_HORIZONTAL_INSET * scale).round() as i32;
                let vertical_inset = (capsule_vertical_inset(mode) * scale).round() as i32;
                let capsule = OverlayWindowBounds {
                    x: display.x + horizontal_inset,
                    y: display.y + vertical_inset,
                    width: display.width - horizontal_inset * 2,
                    height: display.height - vertical_inset * 2,
                };

                assert!(
                    control.x >= capsule.x
                        && control.x + control.width <= capsule.x + capsule.width,
                    "{mode:?} control must remain inside the capsule horizontally at {scale}x: {control:?}, {capsule:?}"
                );
                assert!(
                    control.y >= capsule.y
                        && control.y + control.height <= capsule.y + capsule.height,
                    "{mode:?} control must remain inside the capsule vertically at {scale}x: {control:?}, {capsule:?}"
                );
            }
        }
    }

    #[test]
    fn inner_highlight_is_painted_after_the_translucent_surface() {
        let context = egui::Context::default();
        let colors = overlay_colors(&context);
        let output = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(LIVE_WIDTH, LIVE_HEIGHT),
                )),
                ..Default::default()
            },
            |context| render_overlay(context, &OverlayViewState::default()),
        );
        let highlight_index = output
            .shapes
            .iter()
            .rposition(|shape| {
                matches!(
                    shape.shape,
                    egui::epaint::Shape::LineSegment { stroke, .. }
                        if stroke.color == colors.inner_highlight
                )
            })
            .expect("overlay should paint its inner highlight");
        assert!(
            Some(highlight_index) == output.shapes.len().checked_sub(1),
            "the inner highlight must be painted after the translucent surface and overlay content"
        );
    }

    #[test]
    fn display_content_stays_left_of_reserved_control_slot() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(LIVE_WIDTH, LIVE_HEIGHT),
            )),
            ..Default::default()
        };
        let state = OverlayViewState {
            session_id: Some(SessionId(9)),
            mode: OverlayMode::Live,
            phase: OverlayPhase::Listening,
            live_preview_available: true,
            elapsed: Some(Duration::from_secs(5)),
            transcript: super::super::controller::OverlayTranscript {
                committed: "committed text".into(),
                tentative: "tentative text".into(),
                revision: 1,
            },
            ..Default::default()
        };
        let output = context.run(input, |context| render_overlay(context, &state));
        let nodes = output.platform_output.accesskit_update.unwrap().nodes;
        let boundary = f64::from(LIVE_WIDTH - reserved_control_width());
        for name in [
            "Scribe",
            "00:05",
            "Committed transcript: committed text. Live estimate, may change: tentative text",
        ] {
            let bounds = nodes
                .iter()
                .find_map(|(_, node)| (node.name() == Some(name)).then(|| node.bounds()).flatten())
                .unwrap_or_else(|| panic!("missing overlay node {name}"));
            assert!(
                bounds.x1 <= boundary,
                "{name} overlaps control slot: {bounds:?}"
            );
        }
    }

    #[test]
    fn live_fallback_without_a_started_preview_keeps_timer_and_omits_transcript_semantics() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let state = OverlayViewState {
            session_id: Some(SessionId(10)),
            mode: OverlayMode::Live,
            phase: OverlayPhase::Listening,
            live_preview_available: false,
            elapsed: Some(Duration::from_secs(12)),
            transcript: super::super::controller::OverlayTranscript {
                committed: "must not leak".into(),
                tentative: "into the fallback shell".into(),
                revision: 1,
            },
            transcript_announcement: Some("must not be announced".into()),
            ..Default::default()
        };
        let output = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(LIVE_WIDTH, LIVE_HEIGHT),
                )),
                ..Default::default()
            },
            |context| render_overlay(context, &state),
        );
        let nodes = output.platform_output.accesskit_update.unwrap().nodes;
        assert!(nodes.iter().any(|(_, node)| node.name() == Some("00:12")));
        assert!(nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Image && node.name() == Some("Scribe")
        }));
        assert!(nodes.iter().all(|(_, node)| {
            node.live().is_none()
                && !node.name().is_some_and(|name| {
                    name.contains("must not leak") || name.contains("must not be announced")
                })
        }));
    }

    #[test]
    fn constrained_live_overlay_keeps_estimate_semantics_on_the_transcript_in_both_themes() {
        for visuals in [egui::Visuals::light(), egui::Visuals::dark()] {
            let context = egui::Context::default();
            context.set_visuals(visuals);
            context.enable_accesskit();
            let width = 300.0;
            let state = OverlayViewState {
                mode: OverlayMode::Live,
                phase: OverlayPhase::Listening,
                live_preview_available: true,
                transcript: super::super::controller::OverlayTranscript {
                    committed: "a long committed prefix that must be tail truncated".into(),
                    tentative: "and a changing tentative suffix".into(),
                    revision: 1,
                },
                ..Default::default()
            };
            let output = context.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(width, LIVE_HEIGHT),
                    )),
                    ..Default::default()
                },
                |context| render_overlay(context, &state),
            );
            let nodes = output.platform_output.accesskit_update.unwrap().nodes;
            let preview = nodes
                .iter()
                .find_map(|(_, node)| {
                    node.name()
                        .is_some_and(|name| name.starts_with("Committed transcript:"))
                        .then_some(node)
                })
                .expect("constrained overlay must retain transcript content");
            let preview_bounds = preview.bounds().expect("preview bounds");
            let boundary = f64::from(width - reserved_control_width());

            assert!(
                preview
                    .name()
                    .is_some_and(|name| name.contains("Live estimate, may change:"))
            );
            assert!(
                nodes
                    .iter()
                    .all(|(_, node)| node.name() != Some("Live estimate"))
            );
            assert!(preview_bounds.x0 >= 0.0 && preview_bounds.x1 <= boundary);
        }
    }

    #[test]
    fn compact_status_elements_fit_left_of_reserved_control_slot() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(MINIMAL_WIDTH, MINIMAL_HEIGHT),
            )),
            ..Default::default()
        };
        let state = OverlayViewState {
            session_id: Some(SessionId(10)),
            mode: OverlayMode::Minimal,
            phase: OverlayPhase::Listening,
            elapsed: Some(Duration::from_secs(65)),
            audio_level: super::super::controller::OverlayAudioLevel {
                rms: 0.5,
                peak: 0.75,
            },
            ..Default::default()
        };

        let output = context.run(input, |context| render_overlay(context, &state));
        let nodes = output.platform_output.accesskit_update.unwrap().nodes;
        let boundary = f64::from(MINIMAL_WIDTH - reserved_control_width());
        assert!(
            nodes
                .iter()
                .all(|(_, node)| node.name() != Some("Recording indicator")),
            "the phase dot is decorative because adjacent text names the state"
        );
        let element_bounds =
            ["Scribe is recording", "Microphone input level", "01:05"].map(|name| {
                let bounds = nodes
                    .iter()
                    .find_map(|(_, node)| {
                        (node.name() == Some(name)).then(|| node.bounds()).flatten()
                    })
                    .unwrap_or_else(|| panic!("missing compact overlay node {name}"));
                assert!(
                    bounds.x1 <= boundary,
                    "{name} overlaps control slot: {bounds:?}"
                );
                assert!(
                    bounds.y0 >= 0.0 && bounds.y1 <= f64::from(MINIMAL_HEIGHT),
                    "{name} exceeds compact overlay height: {bounds:?}"
                );
                (name, bounds)
            });
        for adjacent in element_bounds.windows(2) {
            assert!(
                adjacent[0].1.x1 <= adjacent[1].1.x0,
                "{} overlaps {}: {:?} and {:?}",
                adjacent[0].0,
                adjacent[1].0,
                adjacent[0].1,
                adjacent[1].1
            );
        }
    }

    #[test]
    fn elapsed_format_does_not_depend_on_wall_clock() {
        assert_eq!(format_elapsed(Duration::from_secs(12)), "00:12");
        assert_eq!(format_elapsed(Duration::from_secs(65)), "01:05");
    }

    #[test]
    fn preview_head_is_unicode_safe_and_limited_to_one_rendered_row() {
        let context = egui::Context::default();
        let mut result = None;
        let _ = context.run(egui::RawInput::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                let layout = transcript_layout_for_rows(
                    ui,
                    "one two three four five six seven éééééé",
                    "tentative-unbroken-text-that-must-wrap",
                    72.0,
                    LIVE_PREVIEW_ROWS,
                    overlay_colors(context),
                );
                let rows = ui.fonts(|fonts| fonts.layout_job(layout.clone()).rows.len());
                result = Some((layout, rows));
            });
        });
        let (layout, rows) = result.unwrap();
        assert!(rows <= LIVE_PREVIEW_ROWS);
        assert!(layout.text.is_char_boundary(layout.text.len()));
        assert!(
            layout.sections.len() >= 2,
            "styled head should retain formatting"
        );
    }

    #[test]
    fn one_row_preview_preserves_the_committed_prefix_and_trails_with_ellipsis() {
        let context = egui::Context::default();
        let original = concat!(
            "prefix prefix prefix prefix prefix prefix ",
            "e\u{301} \u{6f22}\u{5b57} \u{1f1fa}\u{1f1f8} \u{1f44d}\u{1f3fd} \u{1f469}\u{200d}\u{1f4bb}"
        );
        let mut result = None;
        let _ = context.run(egui::RawInput::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                result = Some(transcript_layout_for_rows(
                    ui,
                    original,
                    "",
                    96.0,
                    LIVE_PREVIEW_ROWS,
                    overlay_colors(context),
                ));
            });
        });
        let layout = result.expect("preview layout should render");
        let retained = layout.text.strip_suffix('\u{2026}').unwrap_or(&layout.text);
        assert!(!retained.is_empty(), "one-row preview should retain text");
        assert!(original.starts_with(retained));
        assert!(layout.text.ends_with('\u{2026}'));
        assert!(
            retained.len() == original.len()
                || original
                    .grapheme_indices(true)
                    .any(|(index, _)| index == retained.len()),
            "preview head must end at a grapheme-cluster boundary: {retained:?}"
        );
    }

    #[test]
    fn head_layout_never_splits_combining_or_emoji_graphemes() {
        let colors = overlay_colors(&egui::Context::default());
        for sample in [
            "e\u{301}",
            "\u{6f22}",
            "\u{1f1fa}\u{1f1f8}",
            "\u{1f44d}\u{1f3fd}",
            "\u{1f469}\u{200d}\u{1f4bb}",
        ] {
            let original = format!("{sample} after");
            let full = transcript_layout(&original, "", LIVE_WIDTH, colors);
            for keep in 0..=original.graphemes(true).count() {
                let head = head_layout_job(&full, keep);
                let retained = head.text.strip_suffix('\u{2026}').unwrap_or(&head.text);
                assert!(original.starts_with(retained));
                assert!(
                    retained.len() == original.len()
                        || original
                            .grapheme_indices(true)
                            .any(|(index, _)| index == retained.len()),
                    "{sample:?} was split at {keep} retained graphemes: {retained:?}"
                );
            }
        }
    }

    #[test]
    fn stabilizer_shaped_transcript_has_exactly_one_boundary_space() {
        let colors = overlay_colors(&egui::Context::default());
        let layout = transcript_layout(
            "Schedule a meeting with",
            "Alex tomorrow",
            LIVE_WIDTH,
            colors,
        );
        assert_eq!(layout.text, "Schedule a meeting with Alex tomorrow");

        let already_spaced = transcript_layout("hello ", "world", LIVE_WIDTH, colors);
        assert_eq!(already_spaced.text, "hello world");
    }

    #[test]
    fn standalone_closing_punctuation_binds_to_the_committed_prefix() {
        let colors = overlay_colors(&egui::Context::default());
        for punctuation in [".", ",", "!", "?", ":", ";", "%", ")", "]", "}", "…"] {
            let layout = transcript_layout("hello", punctuation, LIVE_WIDTH, colors);
            assert_eq!(layout.text, format!("hello{punctuation}"));
        }
    }

    #[test]
    fn committed_transcript_is_the_only_polite_live_region_without_controls() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let state = OverlayViewState {
            phase: OverlayPhase::Listening,
            live_preview_available: true,
            transcript: super::super::controller::OverlayTranscript {
                committed: "hello".to_owned(),
                tentative: "world".to_owned(),
                revision: 1,
            },
            transcript_announcement: Some("Committed transcript: hello".to_owned()),
            ..OverlayViewState::default()
        };

        let output = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(LIVE_WIDTH, LIVE_HEIGHT),
                )),
                ..Default::default()
            },
            |context| {
                render_overlay(context, &state);
            },
        );
        let update = output.platform_output.accesskit_update.unwrap();

        let polite_nodes = update
            .nodes
            .iter()
            .filter(|(_, node)| node.live() == Some(egui::accesskit::Live::Polite))
            .collect::<Vec<_>>();
        assert_eq!(polite_nodes.len(), 1);
        assert_eq!(
            polite_nodes[0].1.name(),
            Some("Committed transcript: hello")
        );
        assert!(update.nodes.iter().any(|(_, node)| {
            node.live().is_none()
                && node.name()
                    == Some("Committed transcript: hello. Live estimate, may change: world")
        }));
        assert!(update.nodes.iter().all(|(_, node)| {
            node.live() != Some(egui::accesskit::Live::Polite)
                || !node
                    .name()
                    .is_some_and(|name| name.contains("Live estimate"))
        }));
        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Image && node.name() == Some("Scribe")
        }));
        assert!(
            update
                .nodes
                .iter()
                .all(|(_, node)| node.role() != egui::accesskit::Role::Button)
        );
    }

    #[test]
    fn phase_only_updates_have_one_polite_live_region() {
        for (phase, expected) in [
            (OverlayPhase::Preparing, "Preparing"),
            (OverlayPhase::Listening, "Recording"),
            (OverlayPhase::Finalizing, "Finalizing"),
        ] {
            let context = egui::Context::default();
            context.enable_accesskit();
            let state = OverlayViewState {
                phase,
                live_preview_available: true,
                transcript: super::super::controller::OverlayTranscript {
                    tentative: "tentative words".to_owned(),
                    ..Default::default()
                },
                ..OverlayViewState::default()
            };

            let output = context.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(LIVE_WIDTH, LIVE_HEIGHT),
                    )),
                    ..Default::default()
                },
                |context| render_overlay(context, &state),
            );
            let polite_nodes = output
                .platform_output
                .accesskit_update
                .expect("overlay should expose AccessKit")
                .nodes
                .into_iter()
                .filter(|(_, node)| node.live() == Some(egui::accesskit::Live::Polite))
                .collect::<Vec<_>>();

            assert_eq!(polite_nodes.len(), 1, "{expected} must have one live owner");
            assert_eq!(polite_nodes[0].1.name(), Some(expected));
            assert!(
                !polite_nodes[0]
                    .1
                    .name()
                    .is_some_and(|name| name.contains("tentative words"))
            );
        }
    }

    #[test]
    fn compact_overlay_defers_all_live_announcements_to_root() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let state = OverlayViewState {
            mode: OverlayMode::Minimal,
            phase: OverlayPhase::Listening,
            transcript: super::super::controller::OverlayTranscript {
                committed: "committed words".to_owned(),
                revision: 1,
                ..Default::default()
            },
            transcript_announcement: Some("Committed transcript: committed words".to_owned()),
            ..OverlayViewState::default()
        };

        let output = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(MINIMAL_WIDTH, MINIMAL_HEIGHT),
                )),
                ..Default::default()
            },
            |context| render_overlay(context, &state),
        );
        let update = output
            .platform_output
            .accesskit_update
            .expect("compact overlay should expose AccessKit");

        assert!(update.nodes.iter().all(|(_, node)| node.live().is_none()));
        assert!(
            update
                .nodes
                .iter()
                .any(|(_, node)| node.name() == Some("Scribe is recording"))
        );
    }

    #[test]
    fn errors_and_notices_suppress_stale_transcript_announcements() {
        let cases = [
            (Some("Preview paused"), None, "Preview paused"),
            (
                None,
                Some(super::super::controller::OverlayError {
                    message: "Preview failed".to_owned(),
                    recovery: OverlayRecovery::None,
                }),
                "Preview failed",
            ),
        ];
        for (notice, error, expected) in cases {
            let context = egui::Context::default();
            context.enable_accesskit();
            let state = OverlayViewState {
                phase: if error.is_some() {
                    OverlayPhase::Error
                } else {
                    OverlayPhase::Listening
                },
                live_preview_available: true,
                notice: notice.map(str::to_owned),
                error,
                transcript_announcement: Some("Committed transcript: stale".to_owned()),
                ..OverlayViewState::default()
            };

            let output = context.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(LIVE_WIDTH, LIVE_HEIGHT),
                    )),
                    ..Default::default()
                },
                |context| render_overlay(context, &state),
            );
            let polite_nodes = output
                .platform_output
                .accesskit_update
                .expect("overlay should expose AccessKit")
                .nodes
                .into_iter()
                .filter(|(_, node)| node.live() == Some(egui::accesskit::Live::Polite))
                .collect::<Vec<_>>();
            assert_eq!(
                polite_nodes.len(),
                1,
                "{expected} must be the only live update"
            );
            assert_eq!(polite_nodes[0].1.name(), Some(expected));
        }
    }

    #[test]
    fn blocked_preview_error_announces_wait_guidance_instead_of_retry() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let state = OverlayViewState {
            phase: OverlayPhase::Error,
            live_preview_available: true,
            error: Some(super::super::controller::OverlayError {
                message: "Live preview has not acknowledged cancellation".to_owned(),
                recovery: OverlayRecovery::WaitForPreview,
            }),
            ..OverlayViewState::default()
        };

        let output = context.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(LIVE_WIDTH, LIVE_HEIGHT),
                )),
                ..Default::default()
            },
            |context| {
                render_overlay(context, &state);
            },
        );
        let update = output.platform_output.accesskit_update.unwrap();
        assert!(update.nodes.iter().any(|(_, node)| {
            node.live() == Some(egui::accesskit::Live::Polite)
                && node.name().is_some_and(|name| {
                    name.contains("Wait for the current preview worker to exit")
                        && !name.contains("You can retry")
                })
        }));
    }

    #[test]
    fn glass_overlay_tokens_meet_contrast_requirements_in_both_themes() {
        fn linear(channel: u8) -> f32 {
            let value = f32::from(channel) / 255.0;
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        }
        fn luminance(color: Color32) -> f32 {
            0.2126 * linear(color.r()) + 0.7152 * linear(color.g()) + 0.0722 * linear(color.b())
        }
        fn composite_over(foreground: Color32, background: Color32) -> Color32 {
            let remaining_alpha = u16::from(255 - foreground.a());
            Color32::from_rgb(
                ((u16::from(foreground.r()) * u16::from(foreground.a())
                    + u16::from(background.r()) * remaining_alpha)
                    / 255) as u8,
                ((u16::from(foreground.g()) * u16::from(foreground.a())
                    + u16::from(background.g()) * remaining_alpha)
                    / 255) as u8,
                ((u16::from(foreground.b()) * u16::from(foreground.a())
                    + u16::from(background.b()) * remaining_alpha)
                    / 255) as u8,
            )
        }
        for visuals in [egui::Visuals::light(), egui::Visuals::dark()] {
            let context = egui::Context::default();
            context.set_visuals(visuals);
            let colors = overlay_colors(&context);
            for background in [Color32::BLACK, Color32::WHITE] {
                let surface = composite_over(colors.surface, background);
                let contrast = |foreground: Color32| {
                    let foreground = luminance(foreground);
                    let background = luminance(surface);
                    let (light, dark) = if foreground >= background {
                        (foreground, background)
                    } else {
                        (background, foreground)
                    };
                    (light + 0.05) / (dark + 0.05)
                };
                assert!(contrast(colors.text) >= 4.5);
                assert!(contrast(colors.muted_text) >= 4.5);
                assert!(contrast(colors.meter_inactive) >= 3.0);
                assert_eq!(colors.waveform.a(), 255);
                let waveform_contrast = contrast(colors.waveform);
                assert!(
                    waveform_contrast >= 3.0,
                    "waveform {:?} must remain distinguishable over composited surface {:?} on a {background:?} backdrop (contrast {waveform_contrast})",
                    colors.waveform,
                    surface,
                );
            }
        }
    }
}
