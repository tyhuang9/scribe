use std::{
    cell::Cell,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

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
pub(super) const MINIMAL_WIDTH: f32 = 200.0;
pub(super) const MINIMAL_HEIGHT: f32 = LIVE_HEIGHT;
const WINDOW_MARGIN: f32 = 24.0;
pub(super) const CONTROL_SIZE: f32 = 44.0;
const CONTROL_CONTENT_GAP: f32 = 8.0;
const CAPSULE_HORIZONTAL_INSET: f32 = 8.0;
const CONTROL_HORIZONTAL_INSET: f32 = 16.0;
const CAPSULE_VERTICAL_INSET: f32 = 8.0;
const CAPSULE_SHADOW_BLUR: f32 = 6.0;
const CAPSULE_SHADOW_OFFSET_Y: f32 = 2.0;
const LIVE_WAVEFORM_SIZE: f32 = 30.0;
const MAX_PREVIEW_GRAPHEMES: usize = 512;
const MAX_MESSAGE_GRAPHEMES: usize = 256;
const LIVE_PREVIEW_ROWS: usize = 1;
const PROGRESS_GLYPHS: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];

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

/// A compact, visual status affordance for lifecycle work. It intentionally
/// stays textual so the native GDI+ and egui renderers share the same copy.
/// Screen readers use `status_text` without the decorative glyph.
pub(super) fn phase_status_label_with_motion(
    phase: OverlayPhase,
    progress_animation_enabled: bool,
) -> String {
    if !phase.is_progressing() {
        return phase.status_text().to_owned();
    }
    let glyph = if progress_animation_enabled && overlay_animations_enabled() {
        let elapsed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        PROGRESS_GLYPHS[(elapsed / 125) as usize % PROGRESS_GLYPHS.len()]
    } else {
        "○"
    };
    format!("{glyph} {}", phase.status_text())
}

#[cfg(target_os = "windows")]
fn overlay_animations_enabled() -> bool {
    use std::ffi::c_void;

    use windows_sys::Win32::UI::WindowsAndMessaging::SystemParametersInfoW;

    const SPI_GETCLIENTAREAANIMATION: u32 = 0x1042;
    let mut enabled = 0i32;
    unsafe {
        SystemParametersInfoW(
            SPI_GETCLIENTAREAANIMATION,
            0,
            &mut enabled as *mut i32 as *mut c_void,
            0,
        ) != 0
            && enabled != 0
    }
}

#[cfg(not(target_os = "windows"))]
fn overlay_animations_enabled() -> bool {
    true
}

pub(super) fn control_window_bounds(
    display: OverlayWindowBounds,
    display_spec: OverlayWindowSpec,
) -> OverlayWindowBounds {
    let scale = display.width as f32 / display_spec.width_points;
    let size = (CONTROL_SIZE * scale).round() as i32;
    let horizontal_inset = (CONTROL_HORIZONTAL_INSET * scale).round() as i32;
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
    text: Color32,
    muted_text: Color32,
    waveform: Color32,
    success: Color32,
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
            // the fill, so this tint lands near the reference's neutral-gray
            // surface on its light backdrop without the native renderer's
            // shadow compensation.
            surface: Color32::from_rgba_unmultiplied(25, 26, 33, 184),
            border: Color32::from_rgba_unmultiplied(220, 229, 242, 36),
            text: palette.text,
            muted_text: Color32::from_rgb(210, 210, 216),
            // The supplied reference uses a purple brand mark. This slightly
            // lighter overlay-specific token preserves that appearance while
            // keeping the non-text mark at 3:1 over the translucent surface.
            waveform: Color32::from_rgb(178, 162, 255),
            success: palette.success_text,
            error: Color32::from_rgb(255, 200, 200),
            warning: Color32::from_rgb(255, 222, 170),
            shadow: Color32::from_black_alpha(96),
        }
    } else {
        OverlayColors {
            surface: Color32::from_rgba_unmultiplied(248, 250, 253, 228),
            border: Color32::from_rgba_unmultiplied(35, 47, 66, 64),
            text: palette.text,
            muted_text: Color32::from_rgb(65, 75, 90),
            waveform: palette.recording_waveform,
            success: palette.success_text,
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
    let rounding = (window_spec(state.mode).height_points - CAPSULE_VERTICAL_INSET * 2.0) / 2.0;
    egui::CentralPanel::default()
        .frame(egui::Frame::none().fill(Color32::TRANSPARENT))
        .show(context, |ui| {
            let capsule_rect = painted_capsule_bounds(ui.max_rect());
            ui.allocate_ui_at_rect(capsule_rect, |ui| {
                egui::Frame::none()
                    .fill(colors.surface)
                    .rounding(egui::Rounding::same(rounding))
                    .stroke(Stroke::new(1.0, colors.border))
                    .shadow(egui::epaint::Shadow {
                        offset: egui::vec2(0.0, CAPSULE_SHADOW_OFFSET_Y),
                        blur: CAPSULE_SHADOW_BLUR,
                        spread: 0.0,
                        color: colors.shadow,
                    })
                    .inner_margin(egui::Margin::symmetric(14.0, 8.0))
                    .show(ui, |ui| {
                        let available = ui.available_size();
                        let control_reserved_width = if is_cancellable(state) {
                            reserved_control_width()
                        } else {
                            CAPSULE_HORIZONTAL_INSET
                        };
                        let content_size = egui::vec2(
                            (available.x - control_reserved_width).max(1.0),
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
        });
}

fn painted_capsule_bounds(viewport: egui::Rect) -> egui::Rect {
    viewport.shrink2(egui::vec2(CAPSULE_HORIZONTAL_INSET, CAPSULE_VERTICAL_INSET))
}

fn reserved_control_width() -> f32 {
    CONTROL_SIZE + CONTROL_CONTENT_GAP + CAPSULE_HORIZONTAL_INSET
}

fn render_compact_status_row(ui: &mut egui::Ui, state: &OverlayViewState, colors: OverlayColors) {
    ui.horizontal_centered(|ui| {
        render_brand_mark(ui, state, colors);
        ui.add_space(8.0);
        let (label, color) = if state.error.is_some() || state.phase == OverlayPhase::Error {
            ("Error".to_owned(), colors.error)
        } else if state.notice.is_some() {
            ("Notice".to_owned(), colors.warning)
        } else if state.phase == OverlayPhase::Listening {
            (
                state
                    .elapsed
                    .map(format_elapsed)
                    .unwrap_or_else(|| "00:00".to_owned()),
                colors.muted_text,
            )
        } else {
            (
                phase_status_label_with_motion(state.phase, state.progress_animation_enabled),
                phase_status_color(state.phase, colors),
            )
        };
        let response = ui.label(RichText::new(label).size(13.0).color(color));
        ui.ctx().accesskit_node_builder(response.id, |builder| {
            builder.set_name(compact_accessible_text(state));
            if state.error.is_some()
                || state.notice.is_some()
                || state.phase_announcement.is_some()
                || state.phase == OverlayPhase::Error
            {
                builder.set_live(egui::accesskit::Live::Polite);
            }
        });
    });
}

fn render_live_status_row(ui: &mut egui::Ui, state: &OverlayViewState, colors: OverlayColors) {
    ui.horizontal_centered(|ui| {
        render_brand_mark(ui, state, colors);
        ui.add_space(8.0);
        let elapsed = if state.phase == OverlayPhase::Listening {
            state
                .elapsed
                .map(format_elapsed)
                .unwrap_or_else(|| "00:00".into())
        } else {
            format!(
                "Recorded {}",
                state
                    .elapsed
                    .map(format_elapsed)
                    .unwrap_or_else(|| "00:00".into())
            )
        };
        ui.label(RichText::new(elapsed).size(13.0).color(colors.muted_text));
        if state.shows_live_transcript()
            || state.error.is_some()
            || state.notice.is_some()
            || state.phase != OverlayPhase::Listening
        {
            render_divider(ui, colors);
            let layout = live_preview_layout(ui, state, colors, ui.available_width());
            let follows_transcript_tail = layout.halign == egui::Align::RIGHT;
            let response = if follows_transcript_tail {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(layout)
                })
                .inner
            } else {
                ui.label(layout)
            };
            ui.ctx().accesskit_node_builder(response.id, |builder| {
                builder.set_name(live_accessible_text(state));
                if state.error.is_some() || state.notice.is_some() {
                    builder.set_live(egui::accesskit::Live::Polite);
                }
            });
        }
        if let Some(announcement_text) = live_overlay_announcement(state) {
            let announcement = ui.allocate_response(egui::Vec2::ZERO, Sense::hover());
            ui.ctx().accesskit_node_builder(announcement.id, |builder| {
                builder.set_role(egui::accesskit::Role::StaticText);
                builder.set_name(announcement_text);
                builder.set_live(egui::accesskit::Live::Polite);
            });
        }
    });
}

pub(super) fn live_overlay_announcement(state: &OverlayViewState) -> Option<&str> {
    if state.error.is_some() || state.notice.is_some() {
        return None;
    }
    if let Some(announcement) = state.phase_announcement.as_deref() {
        return Some(announcement);
    }
    if !state.shows_live_transcript() {
        return None;
    }
    if let Some(announcement) = state.transcript_announcement.as_deref() {
        return Some(announcement);
    }
    None
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
    let (accessible_name, accessible_description) = status_mark_accessibility(state);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, accessible_name));
    ui.ctx().accesskit_node_builder(response.id, |builder| {
        builder.set_role(egui::accesskit::Role::Image);
        builder.set_name(accessible_name);
        builder.set_description(accessible_description);
    });
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        status_mark_glyph(state),
        egui::FontId::proportional(27.0),
        status_mark_color(state, colors),
    );
}

pub(super) fn status_mark_glyph(state: &OverlayViewState) -> &'static str {
    if state.phase == OverlayPhase::Success {
        egui_phosphor::regular::CHECK_CIRCLE
    } else {
        egui_phosphor::regular::WAVEFORM
    }
}

pub(super) fn status_mark_accessibility(state: &OverlayViewState) -> (&'static str, &'static str) {
    if state.phase == OverlayPhase::Success {
        (
            "Scribe completion indicator",
            "Scribe completed successfully",
        )
    } else if state.phase == OverlayPhase::Listening {
        ("Scribe", "Scribe is recording")
    } else {
        ("Scribe", state.phase.status_text())
    }
}

fn status_mark_color(state: &OverlayViewState, colors: OverlayColors) -> Color32 {
    if state.phase == OverlayPhase::Success {
        colors.success
    } else {
        colors.waveform
    }
}

fn phase_status_color(phase: OverlayPhase, colors: OverlayColors) -> Color32 {
    if phase == OverlayPhase::Success {
        colors.success
    } else {
        colors.muted_text
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

    if !state.phase.shows_live_transcript() {
        return message_layout_for_rows(
            ui,
            &phase_status_label_with_motion(state.phase, state.progress_animation_enabled),
            phase_status_color(state.phase, colors),
            max_width,
            LIVE_PREVIEW_ROWS,
        );
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
    if !state.phase.shows_live_transcript() {
        return state.phase.status_text().to_owned();
    }
    if state.transcript.committed.is_empty() && state.transcript.tentative.is_empty() {
        return state.phase.status_text().to_owned();
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

pub(super) fn compact_accessible_text(state: &OverlayViewState) -> String {
    if let Some(error) = &state.error {
        return error_message(error);
    }
    if let Some(notice) = &state.notice {
        return notice.clone();
    }
    if state.phase == OverlayPhase::Error {
        return state.phase.status_text().to_owned();
    }
    if state.phase != OverlayPhase::Listening {
        return state.phase.status_text().to_owned();
    }
    format!(
        "Elapsed time {}",
        format_elapsed(state.elapsed.unwrap_or_default())
    )
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
    let full_rows = ui.fonts(|fonts| fonts.layout_job(full.clone()).rows.len());
    if total_graphemes <= MAX_PREVIEW_GRAPHEMES && full_rows <= max_rows {
        return full;
    }
    let mut low = 0;
    let mut high = total_graphemes.min(MAX_PREVIEW_GRAPHEMES);
    let mut best = tail_layout_job(&full, 0);
    while low <= high {
        let keep = low + (high - low) / 2;
        let candidate = tail_layout_job(&full, keep);
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

fn tail_layout_job(full: &egui::text::LayoutJob, keep_graphemes: usize) -> egui::text::LayoutJob {
    let total_graphemes = full.text.graphemes(true).count();
    if keep_graphemes >= total_graphemes {
        let mut result = full.clone();
        result.halign = egui::Align::RIGHT;
        return result;
    }
    let start_grapheme = total_graphemes.saturating_sub(keep_graphemes);
    let start = full
        .text
        .grapheme_indices(true)
        .nth(start_grapheme)
        .map_or(full.text.len(), |(index, _)| index);
    let mut result = full.clone();
    result.text.clear();
    result.sections.clear();
    result.halign = egui::Align::RIGHT;
    for section in &full.sections {
        let overlap_start = start.max(section.byte_range.start);
        let overlap_end = full.text.len().min(section.byte_range.end);
        if overlap_start < overlap_end {
            result.append(
                &full.text[overlap_start..overlap_end],
                0.0,
                section.format.clone(),
            );
        }
    }
    result
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

fn format_elapsed(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlay::preview_parity::{
        HorizontalAnchor, PARITY_GRAPHEMES, PREVIEW_PARITY_CASES, PreviewInput,
        assert_text_contract, long_message,
    };

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
        assert_eq!((minimal.width_points, minimal.height_points), (200.0, 62.0));
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
    fn progress_status_labels_include_the_lifecycle_copy() {
        assert!(
            phase_status_label_with_motion(OverlayPhase::Processing, true)
                .ends_with("Transcribing…")
        );
        assert!(
            phase_status_label_with_motion(OverlayPhase::Finalizing, true)
                .ends_with("Finishing recording…")
        );
        assert_eq!(
            phase_status_label_with_motion(OverlayPhase::Success, true),
            "Done"
        );
        assert_eq!(
            phase_status_label_with_motion(OverlayPhase::Processing, false),
            "○ Transcribing…"
        );
    }

    #[test]
    fn success_uses_a_completion_mark_and_done_in_both_overlay_modes_and_themes() {
        let success = OverlayViewState {
            phase: OverlayPhase::Success,
            elapsed: Some(Duration::from_secs(12)),
            phase_announcement: Some("Done".to_owned()),
            ..Default::default()
        };
        assert_eq!(
            status_mark_glyph(&success),
            egui_phosphor::regular::CHECK_CIRCLE
        );
        assert_eq!(
            status_mark_accessibility(&success),
            (
                "Scribe completion indicator",
                "Scribe completed successfully"
            )
        );

        for visuals in [egui::Visuals::light(), egui::Visuals::dark()] {
            for (mode, size) in [
                (OverlayMode::Live, egui::vec2(LIVE_WIDTH, LIVE_HEIGHT)),
                (
                    OverlayMode::Minimal,
                    egui::vec2(MINIMAL_WIDTH, MINIMAL_HEIGHT),
                ),
            ] {
                let context = egui::Context::default();
                context.set_visuals(visuals.clone());
                context.enable_accesskit();
                let state = OverlayViewState {
                    mode,
                    ..success.clone()
                };
                let output = context.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                        ..Default::default()
                    },
                    |context| render_overlay(context, &state),
                );
                let nodes = output
                    .platform_output
                    .accesskit_update
                    .expect("success overlay should expose AccessKit")
                    .nodes;

                assert!(nodes.iter().any(|(_, node)| {
                    node.role() == egui::accesskit::Role::Image
                        && node.name() == Some("Scribe completion indicator")
                        && node.description() == Some("Scribe completed successfully")
                }));
                assert!(
                    nodes.iter().any(|(_, node)| node.name() == Some("Done")),
                    "{mode:?} success state should retain its visible completion text"
                );
            }
        }
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
                width: 200,
                height: 62,
            },
            window_spec(OverlayMode::Minimal),
        );
        assert_eq!(control.x + control.width, 284);
        assert_eq!((control.width, control.height), (44, 44));
    }

    #[test]
    fn cancel_control_uses_the_shared_shell_inset_and_center_at_each_dpi() {
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
                let expected_size = (CONTROL_SIZE * scale).round() as i32;
                let expected_inset = (CONTROL_HORIZONTAL_INSET * scale).round() as i32;
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
                    "{mode:?} control must remain within half a physical pixel of the display center at {scale}x"
                );
            }
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
            let capsule = painted_capsule_bounds(viewport);
            assert_eq!(capsule.left(), CAPSULE_HORIZONTAL_INSET);
            assert_eq!(
                capsule.right(),
                spec.width_points - CAPSULE_HORIZONTAL_INSET
            );
            assert_eq!(capsule.top(), CAPSULE_VERTICAL_INSET);
            assert_eq!(
                capsule.bottom(),
                spec.height_points - CAPSULE_VERTICAL_INSET
            );
            let shadow_bounds = capsule
                .expand(CAPSULE_SHADOW_BLUR)
                .translate(egui::vec2(0.0, CAPSULE_SHADOW_OFFSET_Y));
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
                let vertical_inset = (CAPSULE_VERTICAL_INSET * scale).round() as i32;
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
    fn live_fallback_exposes_capture_errors_when_preview_never_started() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let state = OverlayViewState {
            session_id: Some(SessionId(11)),
            mode: OverlayMode::Live,
            phase: OverlayPhase::Error,
            live_preview_available: false,
            elapsed: Some(Duration::from_secs(12)),
            error: Some(super::super::controller::OverlayError {
                message: "Microphone unavailable".into(),
                recovery: OverlayRecovery::Retry,
            }),
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
        assert!(nodes.iter().any(|(_, node)| {
            node.name() == Some("Microphone unavailable You can retry.")
                && node.live() == Some(egui::accesskit::Live::Polite)
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
                session_id: Some(SessionId(11)),
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
    fn compact_shell_contains_only_the_logo_and_proportional_timer_before_the_control_slot() {
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
            ..Default::default()
        };

        let output = context.run(input, |context| render_overlay(context, &state));
        let nodes = output.platform_output.accesskit_update.unwrap().nodes;
        let boundary = f64::from(MINIMAL_WIDTH - reserved_control_width());
        assert!(nodes.iter().all(|(_, node)| {
            !matches!(
                node.name(),
                Some("Scribe is recording") | Some("Microphone input level")
            )
        }));
        let element_bounds = ["Scribe", "Elapsed time 01:05"].map(|name| {
            let node = nodes
                .iter()
                .find_map(|(_, node)| (node.name() == Some(name)).then_some(node))
                .unwrap_or_else(|| panic!("missing compact overlay node {name}"));
            let bounds = node.bounds().expect("compact node bounds");
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
        assert!(element_bounds[0].1.x1 <= element_bounds[1].1.x0);
    }

    #[test]
    fn elapsed_format_does_not_depend_on_wall_clock() {
        assert_eq!(format_elapsed(Duration::from_secs(12)), "00:12");
        assert_eq!(format_elapsed(Duration::from_secs(65)), "01:05");
    }

    #[test]
    fn preview_tail_is_unicode_safe_and_limited_to_one_rendered_row() {
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
        assert!(!layout.sections.is_empty());
        assert_eq!(layout.halign, egui::Align::RIGHT);
    }

    #[test]
    fn one_row_preview_keeps_the_newest_grapheme_safe_suffix_without_an_ellipsis() {
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
        let retained = layout.text.as_str();
        assert!(!retained.is_empty(), "one-row preview should retain text");
        assert!(original.ends_with(retained));
        assert!(!layout.text.contains('\u{2026}'));
        let retained_start = original.len() - retained.len();
        assert!(
            retained_start == 0
                || original
                    .grapheme_indices(true)
                    .any(|(index, _)| index == retained_start),
            "preview tail must start at a grapheme-cluster boundary: {retained:?}"
        );
        assert_eq!(layout.halign, egui::Align::RIGHT);
    }

    #[test]
    fn transcript_stays_left_aligned_through_exact_fit_then_tail_follows_after_overflow() {
        let context = egui::Context::default();
        let mut result = None;
        let _ = context.run(egui::RawInput::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                let colors = overlay_colors(context);
                let exact_text = "W".repeat(24);
                let unbounded = transcript_layout(&exact_text, "", f32::INFINITY, colors);
                let exact_width = ui.fonts(|fonts| fonts.layout_job(unbounded).size().x);
                let short = transcript_layout_for_rows(
                    ui,
                    "short",
                    "",
                    exact_width,
                    LIVE_PREVIEW_ROWS,
                    colors,
                );
                let exact = transcript_layout_for_rows(
                    ui,
                    &exact_text,
                    "",
                    exact_width,
                    LIVE_PREVIEW_ROWS,
                    colors,
                );
                let first_overflow_text = format!("{exact_text}i");
                let first_overflow = transcript_layout_for_rows(
                    ui,
                    &first_overflow_text,
                    "",
                    exact_width,
                    LIVE_PREVIEW_ROWS,
                    colors,
                );
                let first_width =
                    ui.fonts(|fonts| fonts.layout_job(first_overflow.clone()).size().x);
                let later = (2..=8).find_map(|appended| {
                    let text = format!("{exact_text}{}", "i".repeat(appended));
                    let layout = transcript_layout_for_rows(
                        ui,
                        &text,
                        "",
                        exact_width,
                        LIVE_PREVIEW_ROWS,
                        colors,
                    );
                    let width = ui.fonts(|fonts| fonts.layout_job(layout.clone()).size().x);
                    (layout.halign == egui::Align::RIGHT && width > first_width + 0.1)
                        .then_some((layout, width))
                });
                result = Some((short, exact, first_overflow, first_width, later));
            });
        });

        let (short, exact, first_overflow, first_width, later) = result.unwrap();
        assert_eq!(short.halign, egui::Align::LEFT);
        assert_eq!(exact.halign, egui::Align::LEFT);
        assert_eq!(exact.text, "W".repeat(24));
        assert_eq!(first_overflow.halign, egui::Align::RIGHT);
        assert!(first_overflow.text.ends_with('i'));
        assert!(!first_overflow.text.contains('\u{2026}'));
        let (later, later_width) = later.expect("a later append should widen the retained tail");
        let preview_x1 = 500.0;
        assert!(preview_x1 - later_width < preview_x1 - first_width);
        assert_eq!(later.halign, egui::Align::RIGHT);
    }

    #[test]
    fn egui_preview_matches_the_shared_cross_renderer_parity_contract() {
        let context = egui::Context::default();
        let mut checked = Vec::new();
        let _ = context.run(egui::RawInput::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                let colors = overlay_colors(context);
                let exact_text = "W".repeat(24);
                let exact_width = ui.fonts(|fonts| {
                    fonts
                        .layout_job(transcript_layout(&exact_text, "", f32::INFINITY, colors))
                        .size()
                        .x
                });

                for case in PREVIEW_PARITY_CASES {
                    let (original, layout) = match case.input {
                        PreviewInput::Message => {
                            let original = long_message(case.input);
                            let layout = message_layout_for_rows(
                                ui,
                                &original,
                                colors.muted_text,
                                96.0,
                                LIVE_PREVIEW_ROWS,
                            );
                            (original, layout)
                        }
                        PreviewInput::Error => {
                            let original = long_message(case.input);
                            let state = OverlayViewState {
                                error: Some(super::super::controller::OverlayError {
                                    message: original.clone(),
                                    recovery: OverlayRecovery::None,
                                }),
                                ..OverlayViewState::default()
                            };
                            let layout = live_preview_layout(ui, &state, colors, 96.0);
                            (original, layout)
                        }
                        PreviewInput::Notice => {
                            let original = long_message(case.input);
                            let state = OverlayViewState {
                                notice: Some(original.clone()),
                                ..OverlayViewState::default()
                            };
                            let layout = live_preview_layout(ui, &state, colors, 96.0);
                            (original, layout)
                        }
                        PreviewInput::TranscriptShort => {
                            let original = PARITY_GRAPHEMES.to_owned();
                            let layout = transcript_layout_for_rows(
                                ui,
                                &original,
                                "",
                                exact_width,
                                LIVE_PREVIEW_ROWS,
                                colors,
                            );
                            (original, layout)
                        }
                        PreviewInput::TranscriptExactFit => {
                            let layout = transcript_layout_for_rows(
                                ui,
                                &exact_text,
                                "",
                                exact_width,
                                LIVE_PREVIEW_ROWS,
                                colors,
                            );
                            (exact_text.clone(), layout)
                        }
                        PreviewInput::TranscriptOverflow => {
                            let original = format!("{exact_text}{PARITY_GRAPHEMES}");
                            let layout = transcript_layout_for_rows(
                                ui,
                                &original,
                                "",
                                exact_width,
                                LIVE_PREVIEW_ROWS,
                                colors,
                            );
                            (original, layout)
                        }
                    };

                    let anchor = if layout.halign == egui::Align::RIGHT {
                        HorizontalAnchor::Right
                    } else {
                        HorizontalAnchor::Left
                    };
                    assert_eq!(anchor, case.anchor, "{} anchor", case.name);
                    assert_text_contract(case, &original, &layout.text);
                    let rows = ui.fonts(|fonts| fonts.layout_job(layout).rows.len());
                    assert!(rows <= LIVE_PREVIEW_ROWS, "{} row limit", case.name);
                    checked.push(case.name);
                }
            });
        });
        assert_eq!(checked.len(), PREVIEW_PARITY_CASES.len());
    }

    #[test]
    fn overflowing_preview_words_move_the_egui_label_left_while_its_right_edge_stays_fixed() {
        fn preview_bounds(committed: &str) -> egui::accesskit::Rect {
            let context = egui::Context::default();
            context.enable_accesskit();
            let state = OverlayViewState {
                mode: OverlayMode::Live,
                phase: OverlayPhase::Listening,
                live_preview_available: true,
                elapsed: Some(Duration::from_secs(10)),
                transcript: super::super::controller::OverlayTranscript {
                    committed: committed.to_owned(),
                    revision: 1,
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
            let expected = format!("Committed transcript: {committed}");
            output
                .platform_output
                .accesskit_update
                .unwrap()
                .nodes
                .into_iter()
                .find_map(|(_, node)| {
                    (node.name() == Some(expected.as_str()))
                        .then(|| node.bounds())
                        .flatten()
                })
                .expect("preview bounds")
        }

        fn text_width(text: &str) -> f32 {
            let context = egui::Context::default();
            let mut width = None;
            let _ = context.run(egui::RawInput::default(), |context| {
                egui::CentralPanel::default().show(context, |ui| {
                    let layout =
                        transcript_layout(text, "", f32::INFINITY, overlay_colors(context));
                    width = Some(ui.fonts(|fonts| fonts.layout_job(layout).size().x));
                });
            });
            width.unwrap()
        }

        let short = preview_bounds("short");
        let overflowing_probe = preview_bounds(&"W".repeat(128));
        let preview_x0 = short.x0;
        let preview_x1 = overflowing_probe.x1;
        let preview_width = (preview_x1 - preview_x0) as f32;
        let prefix = (1..128)
            .map(|count| "W".repeat(count))
            .take_while(|text| text_width(text) <= preview_width)
            .last()
            .expect("at least one glyph fits the preview");
        let overflow_count = (1..32)
            .find(|count| text_width(&format!("{prefix}{}", "i".repeat(*count))) > preview_width)
            .expect("narrow glyphs eventually overflow the preview");
        let exact_text = format!("{prefix}{}", "i".repeat(overflow_count - 1));
        let exact = preview_bounds(&exact_text);
        assert!((short.x0 - preview_x0).abs() <= 0.5);
        assert!((exact.x0 - preview_x0).abs() <= 0.5);

        let first = preview_bounds(&format!("{prefix}{}", "i".repeat(overflow_count)));
        assert!((first.x1 - preview_x1).abs() <= 0.5);
        let appended = (overflow_count + 1..=overflow_count + 16)
            .map(|count| preview_bounds(&format!("{prefix}{}", "i".repeat(count))))
            .find(|bounds| bounds.x0 < first.x0 && (bounds.x1 - first.x1).abs() <= 0.5)
            .expect("a later overflowing append should move retained ink left");
        assert!(appended.x0 < first.x0, "{first:?} -> {appended:?}");
        assert!(
            (appended.x1 - first.x1).abs() <= 0.5,
            "{first:?} -> {appended:?}"
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
    fn tail_layout_never_splits_combining_or_emoji_graphemes() {
        let colors = overlay_colors(&egui::Context::default());
        for sample in [
            "e\u{301}",
            "\u{6f22}",
            "\u{1f1fa}\u{1f1f8}",
            "\u{1f44d}\u{1f3fd}",
            "\u{1f469}\u{200d}\u{1f4bb}",
        ] {
            let original = format!("before {sample}");
            let full = transcript_layout(&original, "", LIVE_WIDTH, colors);
            for keep in 0..=original.graphemes(true).count() {
                let tail = tail_layout_job(&full, keep);
                assert!(original.ends_with(&tail.text));
                let start = original.len() - tail.text.len();
                assert!(
                    start == 0
                        || start == original.len()
                        || original
                            .grapheme_indices(true)
                            .any(|(index, _)| index == start),
                    "{sample:?} was split at {keep} retained graphemes: {:?}",
                    tail.text
                );
                assert!(!tail.text.contains('\u{2026}'));
                assert_eq!(tail.halign, egui::Align::RIGHT);
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
            (OverlayPhase::Preparing, "Starting microphone…"),
            (OverlayPhase::Listening, "Recording"),
            (OverlayPhase::Finalizing, "Finishing recording…"),
            (OverlayPhase::Processing, "Transcribing…"),
            (OverlayPhase::Pasting, "Pasting…"),
            (OverlayPhase::Success, "Done"),
        ] {
            let context = egui::Context::default();
            context.enable_accesskit();
            let state = OverlayViewState {
                phase,
                live_preview_available: true,
                phase_announcement: Some(expected.to_owned()),
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
    fn listening_without_a_preview_announces_recording_without_exposing_transcript_nodes() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let state = OverlayViewState {
            mode: OverlayMode::Live,
            phase: OverlayPhase::Listening,
            phase_announcement: Some("Recording".to_owned()),
            transcript: super::super::controller::OverlayTranscript {
                committed: "must not leak".to_owned(),
                tentative: " into accessibility".to_owned(),
                revision: 1,
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
        let update = output.platform_output.accesskit_update.unwrap();
        let polite_nodes = update
            .nodes
            .iter()
            .filter(|(_, node)| node.live() == Some(egui::accesskit::Live::Polite))
            .collect::<Vec<_>>();

        assert_eq!(polite_nodes.len(), 1);
        assert_eq!(polite_nodes[0].1.name(), Some("Recording"));
        assert!(update.nodes.iter().all(|(_, node)| {
            !node
                .name()
                .is_some_and(|name| name.contains("must not leak"))
        }));
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
        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::Image
                && node.name() == Some("Scribe")
                && node.description() == Some("Scribe is recording")
        }));
        assert!(
            update
                .nodes
                .iter()
                .any(|(_, node)| node.name() == Some("Elapsed time 00:00"))
        );
        assert!(update.nodes.iter().all(|(_, node)| {
            !node
                .name()
                .is_some_and(|name| name.contains("committed words"))
        }));
    }

    #[test]
    fn compact_error_and_notice_replace_the_timer_and_announce_the_full_message() {
        let cases = [
            (
                OverlayViewState {
                    mode: OverlayMode::Minimal,
                    phase: OverlayPhase::Error,
                    error: Some(super::super::controller::OverlayError {
                        message: "Microphone unavailable".to_owned(),
                        recovery: OverlayRecovery::Retry,
                    }),
                    ..OverlayViewState::default()
                },
                "Microphone unavailable You can retry.",
            ),
            (
                OverlayViewState {
                    mode: OverlayMode::Minimal,
                    phase: OverlayPhase::Listening,
                    notice: Some("Preview paused while final transcription continues.".to_owned()),
                    ..OverlayViewState::default()
                },
                "Preview paused while final transcription continues.",
            ),
        ];

        for (state, expected) in cases {
            let context = egui::Context::default();
            context.enable_accesskit();
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
            let update = output.platform_output.accesskit_update.unwrap();
            let live = update
                .nodes
                .iter()
                .filter(|(_, node)| node.live() == Some(egui::accesskit::Live::Polite))
                .collect::<Vec<_>>();
            assert_eq!(live.len(), 1);
            assert_eq!(live[0].1.name(), Some(expected));
            assert!(update.nodes.iter().all(|(_, node)| {
                !node.name().is_some_and(|name| {
                    name.starts_with("Committed transcript:")
                        || name.contains("Live estimate, may change")
                })
            }));
        }
    }

    #[test]
    fn compact_lifecycle_status_replaces_the_frozen_timer_once_recording_stops() {
        for (phase, expected) in [
            (OverlayPhase::Finalizing, "Finishing recording…"),
            (OverlayPhase::Processing, "Transcribing…"),
            (OverlayPhase::Pasting, "Pasting…"),
            (OverlayPhase::Success, "Done"),
        ] {
            let context = egui::Context::default();
            context.enable_accesskit();
            let state = OverlayViewState {
                mode: OverlayMode::Minimal,
                phase,
                elapsed: Some(std::time::Duration::from_secs(12)),
                phase_announcement: Some(expected.to_owned()),
                transcript: super::super::controller::OverlayTranscript {
                    committed: "stale words".to_owned(),
                    tentative: " that must not be shown".to_owned(),
                    revision: 1,
                },
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
            let update = output.platform_output.accesskit_update.unwrap();
            let status = update
                .nodes
                .iter()
                .find(|(_, node)| node.live() == Some(egui::accesskit::Live::Polite))
                .expect("phase transition should have one polite announcement")
                .1
                .name();
            assert_eq!(status, Some(expected));
            assert!(update.nodes.iter().all(|(_, node)| {
                !node.name().is_some_and(|name| name.contains("stale words"))
            }));
        }
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
            let [red, green, blue, alpha] = foreground.to_srgba_unmultiplied();
            let remaining_alpha = u16::from(255 - foreground.a());
            Color32::from_rgb(
                ((u16::from(red) * u16::from(alpha) + u16::from(background.r()) * remaining_alpha)
                    / 255) as u8,
                ((u16::from(green) * u16::from(alpha)
                    + u16::from(background.g()) * remaining_alpha)
                    / 255) as u8,
                ((u16::from(blue) * u16::from(alpha) + u16::from(background.b()) * remaining_alpha)
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
                assert!(
                    contrast(colors.text) >= 4.5,
                    "text {:?} must contrast with composited surface {:?} over {background:?}",
                    colors.text,
                    surface
                );
                assert!(
                    contrast(colors.muted_text) >= 4.5,
                    "muted text {:?} must contrast with composited surface {:?} over {background:?}",
                    colors.muted_text,
                    surface
                );
                assert!(
                    contrast(colors.error) >= 4.5,
                    "error text {:?} must contrast with composited surface {:?} over {background:?}",
                    colors.error,
                    surface
                );
                assert!(
                    contrast(colors.warning) >= 4.5,
                    "warning text {:?} must contrast with composited surface {:?} over {background:?}",
                    colors.warning,
                    surface
                );
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
