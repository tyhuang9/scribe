use std::{cell::Cell, time::Duration};

use eframe::egui::{self, Color32, RichText, Sense, Stroke, ViewportClass};

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

const LIVE_WIDTH: f32 = 600.0;
const LIVE_HEIGHT: f32 = 62.0;
const MINIMAL_WIDTH: f32 = 320.0;
const MINIMAL_HEIGHT: f32 = 52.0;
const WINDOW_MARGIN: f32 = 24.0;
const CONTROL_SIZE: f32 = 44.0;
const CONTROL_CONTENT_GAP: f32 = 8.0;
const CAPSULE_HORIZONTAL_INSET: f32 = 8.0;
const CAPSULE_VERTICAL_INSET: f32 = 8.0;
const CAPSULE_SHADOW_BLUR: f32 = 6.0;
const CAPSULE_SHADOW_OFFSET_Y: f32 = 2.0;
const COMPACT_METER_WIDTH: f32 = 38.0;
const LIVE_WAVEFORM_SIZE: f32 = 30.0;
const MAX_PREVIEW_CHARS: usize = 512;
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

fn is_cancellable(state: &OverlayViewState) -> bool {
    state.session_id.is_some()
        && matches!(
            state.phase,
            OverlayPhase::Preparing | OverlayPhase::Listening
        )
}

fn control_window_bounds(
    display: OverlayWindowBounds,
    display_spec: OverlayWindowSpec,
) -> OverlayWindowBounds {
    let scale = display.width as f32 / display_spec.width_points;
    let size = (CONTROL_SIZE * scale).round() as i32;
    OverlayWindowBounds {
        x: display.x + display.width - size,
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
                    egui::WidgetInfo::labeled(
                        egui::WidgetType::Button,
                        "Cancel recording and discard it.",
                    )
                });
                ui.ctx().accesskit_node_builder(response.id, |builder| {
                    builder.set_name("Cancel recording and discard it.");
                });
                if response
                    .on_hover_text("Cancel recording and discard it.")
                    .clicked()
                {
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
    tentative_text: Color32,
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
            surface: Color32::from_rgba_unmultiplied(24, 29, 38, 230),
            border: Color32::from_rgba_unmultiplied(220, 229, 242, 76),
            text: palette.text,
            muted_text: Color32::from_rgb(202, 211, 224),
            tentative_text: Color32::from_rgb(166, 180, 202),
            waveform: palette.accent,
            meter_active: palette.success,
            meter_inactive: Color32::from_rgb(128, 142, 162),
            error: palette.error,
            warning: palette.warning,
            shadow: Color32::from_black_alpha(96),
        }
    } else {
        OverlayColors {
            surface: Color32::from_rgba_unmultiplied(247, 249, 252, 238),
            border: Color32::from_rgba_unmultiplied(35, 47, 66, 64),
            text: palette.text,
            muted_text: Color32::from_rgb(65, 75, 90),
            tentative_text: Color32::from_rgb(82, 96, 116),
            waveform: palette.accent,
            meter_active: palette.success_text,
            meter_inactive: Color32::from_rgb(100, 112, 132),
            error: palette.error_text,
            warning: palette.warning,
            shadow: Color32::from_black_alpha(54),
        }
    }
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
                        let content_size = egui::vec2(
                            (available.x - CONTROL_SIZE - CONTROL_CONTENT_GAP).max(1.0),
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

fn render_compact_status_row(ui: &mut egui::Ui, state: &OverlayViewState, colors: OverlayColors) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let status_color = phase_color(state.phase);
        let (dot_rect, dot_response) =
            ui.allocate_exact_size(egui::vec2(10.0, 10.0), Sense::hover());
        dot_response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Label, "Recording indicator")
        });
        ui.painter()
            .circle_filled(dot_rect.center(), 4.0, status_color);

        let label = if state.phase == OverlayPhase::Listening {
            "Scribe is recording"
        } else {
            state.phase.label()
        };
        let status = ui.label(RichText::new(label).strong().size(13.0).color(colors.text));
        mark_polite_live_region(ui.ctx(), status.id);
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
        render_waveform_meter(ui, state, colors);
        ui.add_space(8.0);
        let elapsed = state
            .elapsed
            .map(format_elapsed)
            .unwrap_or_else(|| "0:00".into());
        ui.label(
            RichText::new(elapsed)
                .size(13.0)
                .monospace()
                .color(colors.muted_text),
        );
        render_divider(ui, colors);
        let response = ui.label(live_preview_layout(ui, state, colors, ui.available_width()));
        ui.ctx().accesskit_node_builder(response.id, |builder| {
            builder.set_name(live_accessible_text(state));
            if state.error.is_some() || state.notice.is_some() {
                builder.set_live(egui::accesskit::Live::Polite);
            }
        });
        if let Some(announcement_text) = &state.transcript_announcement {
            let announcement = ui.allocate_response(egui::Vec2::ZERO, Sense::hover());
            ui.ctx().accesskit_node_builder(announcement.id, |builder| {
                builder.set_role(egui::accesskit::Role::StaticText);
                builder.set_name(announcement_text.as_str());
                builder.set_live(egui::accesskit::Live::Polite);
            });
        }
    });
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

fn render_waveform_meter(ui: &mut egui::Ui, state: &OverlayViewState, colors: OverlayColors) {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(LIVE_WAVEFORM_SIZE, LIVE_WAVEFORM_SIZE),
        Sense::hover(),
    );
    let level = state.audio_level.rms.max(state.audio_level.peak * 0.7);
    response.widget_info(|| {
        let mut info = egui::WidgetInfo::labeled(
            egui::WidgetType::ProgressIndicator,
            "Microphone input level",
        );
        info.value = Some((level * 100.0).round() as f64);
        info
    });
    ui.ctx().accesskit_node_builder(response.id, |builder| {
        builder.set_role(egui::accesskit::Role::ProgressIndicator);
        builder.set_name("Microphone input level");
        builder.set_description(if state.phase == OverlayPhase::Listening {
            "Scribe is recording"
        } else {
            state.phase.label()
        });
    });
    let waveform = waveform_color(level, colors);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        egui_phosphor::regular::WAVEFORM,
        egui::FontId::proportional(27.0),
        waveform,
    );
}

fn waveform_color(level: f32, colors: OverlayColors) -> Color32 {
    Color32::from_rgba_unmultiplied(
        colors.waveform.r(),
        colors.waveform.g(),
        colors.waveform.b(),
        (96.0 + level * 159.0).round() as u8,
    )
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

fn live_accessible_text(state: &OverlayViewState) -> String {
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
            "Committed transcript: {}. Tentative transcript: {}",
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
    let total_chars = full.text.chars().count();
    let mut low = 0;
    let mut high = total_chars.min(MAX_PREVIEW_CHARS);
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
    let total_chars = full.text.chars().count();
    let mut low = 0;
    let mut high = total_chars.min(256);
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

fn head_layout_job(full: &egui::text::LayoutJob, keep_chars: usize) -> egui::text::LayoutJob {
    let total_chars = full.text.chars().count();
    if keep_chars >= total_chars {
        return full.clone();
    }
    let end = full
        .text
        .char_indices()
        .nth(keep_chars)
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

fn tail_layout_job(full: &egui::text::LayoutJob, keep_chars: usize) -> egui::text::LayoutJob {
    let total_chars = full.text.chars().count();
    if keep_chars >= total_chars {
        return full.clone();
    }
    let start = full
        .text
        .char_indices()
        .nth(total_chars.saturating_sub(keep_chars))
        .map_or(full.text.len(), |(index, _)| index);
    let mut result = full.clone();
    result.text.clear();
    result.sections.clear();
    let first_format = full
        .sections
        .iter()
        .find(|section| section.byte_range.end > start)
        .map(|section| section.format.clone())
        .unwrap_or_default();
    result.append("…", 0.0, first_format);
    for section in &full.sections {
        let section_start = section.byte_range.start.max(start);
        if section_start < section.byte_range.end {
            result.append(
                &full.text[section_start..section.byte_range.end],
                0.0,
                section.format.clone(),
            );
        }
    }
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
            color: colors.text,
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
            color: colors.tentative_text,
            italics: true,
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
                    && node.name() == Some("Cancel recording and discard it."))
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
    fn control_bounds_cover_the_display_right_edge() {
        let control = control_window_bounds(
            OverlayWindowBounds {
                x: 100,
                y: 20,
                width: 320,
                height: 52,
            },
            window_spec(OverlayMode::Minimal),
        );
        assert_eq!(control.x + control.width, 420);
        assert_eq!((control.width, control.height), (44, 44));
    }

    #[test]
    fn painted_capsule_keeps_shadow_room_and_encloses_the_cancel_control() {
        let viewport =
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(LIVE_WIDTH, LIVE_HEIGHT));
        let capsule = painted_capsule_bounds(viewport);
        assert_eq!(capsule.left(), CAPSULE_HORIZONTAL_INSET);
        assert_eq!(capsule.right(), LIVE_WIDTH - CAPSULE_HORIZONTAL_INSET);
        assert_eq!(capsule.top(), CAPSULE_VERTICAL_INSET);
        assert_eq!(capsule.bottom(), LIVE_HEIGHT - CAPSULE_VERTICAL_INSET);
        let shadow_bounds = capsule
            .expand(CAPSULE_SHADOW_BLUR)
            .translate(egui::vec2(0.0, CAPSULE_SHADOW_OFFSET_Y));
        assert!(
            viewport.contains_rect(shadow_bounds),
            "the painted capsule shadow must stay inside its transparent viewport"
        );

        let control = control_window_bounds(
            OverlayWindowBounds {
                x: 0,
                y: 0,
                width: LIVE_WIDTH as i32,
                height: LIVE_HEIGHT as i32,
            },
            window_spec(OverlayMode::Live),
        );
        assert!(control.x >= capsule.left() as i32);
        assert!(control.x + control.width / 2 <= capsule.right() as i32);
        assert!(control.y >= capsule.top() as i32);
        assert!(control.y + control.height <= capsule.bottom() as i32);
    }

    #[test]
    fn waveform_opacity_tracks_microphone_activity() {
        let colors = overlay_colors(&egui::Context::default());
        assert!(waveform_color(1.0, colors).a() > waveform_color(0.0, colors).a());
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
        let boundary = f64::from(LIVE_WIDTH - CONTROL_SIZE - CONTROL_CONTENT_GAP);
        for name in [
            "Microphone input level",
            "0:05",
            "Committed transcript: committed text. Tentative transcript: tentative text",
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
        let boundary = f64::from(MINIMAL_WIDTH - CONTROL_SIZE - CONTROL_CONTENT_GAP);
        let element_bounds = [
            "Recording indicator",
            "Scribe is recording",
            "Microphone input level",
            "1:05",
        ]
        .map(|name| {
            let bounds = nodes
                .iter()
                .find_map(|(_, node)| (node.name() == Some(name)).then(|| node.bounds()).flatten())
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
        assert_eq!(format_elapsed(Duration::from_secs(65)), "1:05");
    }

    #[test]
    fn preview_tail_is_unicode_safe_and_limited_to_two_rendered_rows() {
        let context = egui::Context::default();
        let mut result = None;
        let _ = context.run(egui::RawInput::default(), |context| {
            egui::CentralPanel::default().show(context, |ui| {
                let layout = transcript_layout_for_rows(
                    ui,
                    "one two three four five six seven éééééé",
                    "tentative-unbroken-text-that-must-wrap",
                    72.0,
                    2,
                    overlay_colors(context),
                );
                let rows = ui.fonts(|fonts| fonts.layout_job(layout.clone()).rows.len());
                result = Some((layout, rows));
            });
        });
        let (layout, rows) = result.unwrap();
        assert!(rows <= 2);
        assert!(layout.text.is_char_boundary(layout.text.len()));
        assert!(
            layout.sections.len() >= 2,
            "styled tail should retain formatting"
        );
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
    fn transcript_and_status_are_polite_live_regions_without_controls() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let state = OverlayViewState {
            phase: OverlayPhase::Listening,
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

        assert!(update.nodes.iter().any(|(_, node)| {
            node.live() == Some(egui::accesskit::Live::Polite)
                && node.name() == Some("Committed transcript: hello")
        }));
        assert!(update.nodes.iter().any(|(_, node)| {
            node.live().is_none()
                && node.name() == Some("Committed transcript: hello. Tentative transcript: world")
        }));
        assert!(update.nodes.iter().all(|(_, node)| {
            node.live() != Some(egui::accesskit::Live::Polite)
                || !node
                    .name()
                    .is_some_and(|name| name.contains("Tentative transcript"))
        }));
        assert!(update.nodes.iter().any(|(_, node)| {
            node.role() == egui::accesskit::Role::ProgressIndicator
                && node.name() == Some("Microphone input level")
                && node.numeric_value() == Some(0.0)
        }));
        assert!(
            update
                .nodes
                .iter()
                .all(|(_, node)| node.role() != egui::accesskit::Role::Button)
        );
    }

    #[test]
    fn blocked_preview_error_announces_wait_guidance_instead_of_retry() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let state = OverlayViewState {
            phase: OverlayPhase::Error,
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
                assert!(contrast(colors.tentative_text) >= 4.5);
                assert!(contrast(colors.meter_inactive) >= 3.0);
            }
        }
    }
}
