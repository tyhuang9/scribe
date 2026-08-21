use std::{borrow::Cow, ffi::c_void, mem::zeroed, ptr::null_mut, sync::Mutex, time::Duration};

use eframe::egui::Color32;
use unicode_segmentation::UnicodeSegmentation;
use windows_sys::Win32::Graphics::GdiPlus::{
    FillModeAlternate, FontStyleBold, FontStyleRegular, GdipAddPathArc, GdipCloneFontFamily,
    GdipClosePathFigure, GdipCreateBitmapFromScan0, GdipCreateFont, GdipCreateFontFamilyFromName,
    GdipCreatePath, GdipCreatePen1, GdipCreateSolidFill, GdipDeleteBrush, GdipDeleteFont,
    GdipDeleteFontFamily, GdipDeleteGraphics, GdipDeletePath, GdipDeletePen,
    GdipDeletePrivateFontCollection, GdipDeleteStringFormat, GdipDisposeImage, GdipDrawLine,
    GdipDrawPath, GdipDrawString, GdipFillEllipse, GdipFillPath, GdipGetFontCollectionFamilyCount,
    GdipGetFontCollectionFamilyList, GdipGetGenericFontFamilySansSerif,
    GdipGetImageGraphicsContext, GdipGraphicsClear, GdipMeasureString,
    GdipNewPrivateFontCollection, GdipPrivateAddMemoryFont, GdipSetSmoothingMode,
    GdipSetStringFormatFlags, GdipSetTextRenderingHint, GdipStringFormatGetGenericTypographic,
    GdiplusShutdown, GdiplusStartup, GdiplusStartupInput, GpBitmap, GpBrush, GpFont,
    GpFontCollection, GpFontFamily, GpGraphics, GpImage, GpPath, GpPen, GpSolidFill,
    GpStringFormat, Ok as GDI_PLUS_OK, RectF, SmoothingModeAntiAlias8x8,
    StringFormatFlagsMeasureTrailingSpaces, StringFormatFlagsNoWrap,
    TextRenderingHintAntiAliasGridFit, UnitPixel,
};

use super::{
    super::{
        controller::{OverlayMode, OverlayPhase, OverlayRecovery, OverlayViewState},
        platform::OverlayWindowBounds,
        view::{CONTROL_SIZE, LIVE_HEIGHT, LIVE_WIDTH, MINIMAL_HEIGHT, MINIMAL_WIDTH},
    },
    layout::DisplayLayout,
};
use crate::ui::ThemePalette;

const PIXEL_FORMAT_32BPP_PARGB: i32 = 0x000E_200B;
const MAX_PREVIEW_GRAPHEMES: usize = 512;
const MAX_MESSAGE_GRAPHEMES: usize = 256;
const BASELINE_METRIC_CAPACITY: usize = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Argb(u32);

impl Argb {
    const TRANSPARENT: Self = Self(0);

    const fn new(alpha: u8, red: u8, green: u8, blue: u8) -> Self {
        Self(((alpha as u32) << 24) | ((red as u32) << 16) | ((green as u32) << 8) | blue as u32)
    }

    fn from_color(color: Color32) -> Self {
        Self::new(color.a(), color.r(), color.g(), color.b())
    }
}

#[derive(Clone, Copy, Debug)]
struct NativeColors {
    surface: Argb,
    border: Argb,
    inner_highlight: Argb,
    text: Argb,
    muted_text: Argb,
    waveform: Argb,
    meter_active: Argb,
    meter_inactive: Argb,
    error: Argb,
    warning: Argb,
    shadow: Argb,
}

impl NativeColors {
    fn for_theme(dark_mode: bool) -> Self {
        let palette = if dark_mode {
            ThemePalette::dark()
        } else {
            ThemePalette::light()
        };
        if dark_mode {
            Self {
                surface: Argb::new(184, 56, 57, 65),
                border: Argb::new(36, 220, 229, 242),
                inner_highlight: Argb::new(18, 255, 255, 255),
                text: Argb::from_color(palette.text),
                muted_text: Argb::new(255, 210, 210, 216),
                // Overlay-specific accessible variant of the reference purple.
                waveform: Argb::new(255, 178, 162, 255),
                meter_active: Argb::from_color(palette.success),
                meter_inactive: Argb::new(255, 180, 180, 188),
                error: Argb::from_color(palette.error),
                warning: Argb::from_color(palette.warning),
                shadow: Argb::new(96, 0, 0, 0),
            }
        } else {
            Self {
                surface: Argb::new(228, 248, 250, 253),
                border: Argb::new(64, 35, 47, 66),
                inner_highlight: Argb::new(156, 255, 255, 255),
                text: Argb::from_color(palette.text),
                muted_text: Argb::new(255, 65, 75, 90),
                waveform: Argb::from_color(palette.recording_waveform),
                meter_active: Argb::from_color(palette.success_text),
                meter_inactive: Argb::new(255, 100, 112, 132),
                error: Argb::from_color(palette.error_text),
                warning: Argb::from_color(palette.warning),
                shadow: Argb::new(54, 0, 0, 0),
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct LayeredFrame {
    pub width: i32,
    pub height: i32,
    /// Top-down premultiplied BGRA pixels.
    pub pixels: Vec<u8>,
}

impl LayeredFrame {
    fn transparent(width: i32, height: i32) -> Result<Self, RasterError> {
        if width <= 0 || height <= 0 {
            return Err(RasterError::InvalidDimensions);
        }
        let length = usize::try_from(width)
            .ok()
            .and_then(|width| {
                usize::try_from(height)
                    .ok()
                    .and_then(|height| width.checked_mul(height))
            })
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or(RasterError::InvalidDimensions)?;
        let mut pixels = Vec::new();
        pixels
            .try_reserve_exact(length)
            .map_err(|_| RasterError::InvalidDimensions)?;
        pixels.resize(length, 0);
        Ok(Self {
            width,
            height,
            pixels,
        })
    }

    #[cfg(test)]
    fn alpha_at(&self, x: i32, y: i32) -> u8 {
        let offset = ((y * self.width + x) * 4 + 3) as usize;
        self.pixels[offset]
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TextStyle {
    Regular,
    Bold,
    Monospace,
    Phosphor,
}

/// Representative ascender/descender runs used only to establish stable font
/// baselines. They are deliberately fixed: transcript text is never retained
/// in the native rasterizer cache.
fn baseline_sample(style: TextStyle) -> &'static str {
    match style {
        TextStyle::Regular | TextStyle::Bold => "Agjpqy",
        TextStyle::Monospace => "00:12",
        TextStyle::Phosphor => egui_phosphor::regular::WAVEFORM,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StyledSection {
    text: String,
    color: Argb,
    style: TextStyle,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct StyledLine {
    sections: Vec<StyledSection>,
}

impl StyledLine {
    fn plain(text: impl Into<String>, color: Argb) -> Self {
        Self {
            sections: vec![StyledSection {
                text: text.into(),
                color,
                style: TextStyle::Regular,
            }],
        }
    }

    fn text(&self) -> String {
        self.sections
            .iter()
            .map(|section| section.text.as_str())
            .collect()
    }

    fn grapheme_count(&self) -> usize {
        self.text().graphemes(true).count()
    }

    fn head(&self, keep: usize) -> Self {
        slice_styled_line(self, 0, keep, true)
    }
}

pub(super) struct NativeRasterizer {
    // Fields drop in declaration order: release the private family before GDI+ shutdown.
    phosphor: PrivateFont,
    baseline_metrics: Mutex<BaselineMetricCache>,
    #[cfg(test)]
    baseline_probe_count: std::sync::atomic::AtomicUsize,
    _gdiplus: GdiPlusSession,
}

#[derive(Clone, Copy, Debug)]
struct BaselineMetric {
    style: TextStyle,
    font_size_bits: u32,
    ink_center_y: f32,
}

#[derive(Debug)]
struct BaselineMetricCache {
    entries: [Option<BaselineMetric>; BASELINE_METRIC_CAPACITY],
    next_eviction: usize,
}

impl Default for BaselineMetricCache {
    fn default() -> Self {
        Self {
            entries: [None; BASELINE_METRIC_CAPACITY],
            next_eviction: 0,
        }
    }
}

impl BaselineMetricCache {
    fn get(&self, style: TextStyle, font_size: f32) -> Option<f32> {
        self.entries
            .iter()
            .flatten()
            .find(|metric| metric.style == style && metric.font_size_bits == font_size.to_bits())
            .map(|metric| metric.ink_center_y)
    }

    fn insert(&mut self, style: TextStyle, font_size: f32, ink_center_y: f32) {
        if let Some(slot) = self.entries.iter_mut().find(|entry| entry.is_none()) {
            *slot = Some(BaselineMetric {
                style,
                font_size_bits: font_size.to_bits(),
                ink_center_y,
            });
            return;
        }
        self.entries[self.next_eviction] = Some(BaselineMetric {
            style,
            font_size_bits: font_size.to_bits(),
            ink_center_y,
        });
        self.next_eviction = (self.next_eviction + 1) % BASELINE_METRIC_CAPACITY;
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.iter().flatten().count()
    }
}

impl NativeRasterizer {
    pub(super) fn new() -> Result<Self, RasterError> {
        let gdiplus = GdiPlusSession::start()?;
        let phosphor = PrivateFont::phosphor_regular()?;
        Ok(Self {
            phosphor,
            baseline_metrics: Mutex::new(BaselineMetricCache::default()),
            #[cfg(test)]
            baseline_probe_count: std::sync::atomic::AtomicUsize::new(0),
            _gdiplus: gdiplus,
        })
    }

    fn baseline_ink_center_y(&self, font_size: f32, style: TextStyle) -> Result<f32, RasterError> {
        if let Some(center) = self
            .baseline_metrics
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(style, font_size)
        {
            return Ok(center);
        }

        let center = self.measure_baseline_ink_center_y(font_size, style)?;
        #[cfg(test)]
        self.baseline_probe_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut metrics = self
            .baseline_metrics
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Ok(metrics.get(style, font_size).unwrap_or_else(|| {
            metrics.insert(style, font_size, center);
            center
        }))
    }

    fn measure_baseline_ink_center_y(
        &self,
        font_size: f32,
        style: TextStyle,
    ) -> Result<f32, RasterError> {
        let sample = baseline_sample(style);
        let width = (font_size * 16.0).ceil().clamp(1.0, 32_768.0) as i32;
        let height = (font_size * 4.0).ceil().max(1.0) as i32;
        let mut probe = LayeredFrame::transparent(width, height)?;
        let mut canvas = Canvas::new(self, &mut probe.pixels, width, height)?;
        canvas.draw_text(
            sample,
            0.0,
            0.0,
            width as f32,
            height as f32,
            font_size,
            style,
            Argb::new(255, 255, 255, 255),
        )?;
        drop(canvas);
        let mut first = None;
        let mut last = None;
        for y in 0..height {
            if (0..width).any(|x| probe.pixels[((y * width + x) * 4 + 3) as usize] > 0) {
                first.get_or_insert(y);
                last = Some(y);
            }
        }
        let (first, last) = first.zip(last).ok_or(RasterError::TextTooLong)?;
        Ok((first + last + 1) as f32 / 2.0)
    }

    #[cfg(test)]
    fn baseline_cache_stats(&self) -> (usize, usize) {
        (
            self.baseline_metrics
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .len(),
            self.baseline_probe_count
                .load(std::sync::atomic::Ordering::Relaxed),
        )
    }

    pub(super) fn render_display(
        &self,
        state: &OverlayViewState,
        dark_mode: bool,
        width: i32,
        height: i32,
    ) -> Result<LayeredFrame, RasterError> {
        let mut frame = LayeredFrame::transparent(width, height)?;
        let layout = DisplayLayout::from_bounds(
            state.mode,
            OverlayWindowBounds {
                x: 0,
                y: 0,
                width,
                height,
            },
        )
        .ok_or(RasterError::InvalidDimensions)?;
        let scale = layout.scale;
        let mut canvas = Canvas::new(self, &mut frame.pixels, width, height)?;
        let colors = NativeColors::for_theme(dark_mode);
        draw_capsule(&mut canvas, state.mode, scale, colors)?;
        match state.mode {
            OverlayMode::Live => draw_live(&mut canvas, state, &layout, colors)?,
            OverlayMode::Minimal | OverlayMode::Off => {
                draw_compact(&mut canvas, state, &layout, colors)?
            }
        }
        drop(canvas);
        Ok(frame)
    }

    pub(super) fn render_control(
        &self,
        dark_mode: bool,
        width: i32,
        height: i32,
    ) -> Result<LayeredFrame, RasterError> {
        let mut frame = LayeredFrame::transparent(width, height)?;
        let scale = (width as f32 / CONTROL_SIZE)
            .min(height as f32 / CONTROL_SIZE)
            .max(0.1);
        let mut canvas = Canvas::new(self, &mut frame.pixels, width, height)?;
        let colors = NativeColors::for_theme(dark_mode);
        canvas.draw_centered_text(
            egui_phosphor::regular::X,
            width as f32 / 2.0,
            10.0 * scale,
            width as f32,
            24.0 * scale,
            20.0 * scale,
            TextStyle::Phosphor,
            colors.text,
        )?;
        drop(canvas);
        Ok(frame)
    }
}

fn draw_capsule(
    canvas: &mut Canvas<'_>,
    mode: OverlayMode,
    scale: f32,
    colors: NativeColors,
) -> Result<(), RasterError> {
    let (logical_width, logical_height, vertical_inset, shadow_extent, shadow_offset) = match mode {
        OverlayMode::Live => (LIVE_WIDTH, LIVE_HEIGHT, 8.0, 5.0, 2.0),
        OverlayMode::Minimal | OverlayMode::Off => (MINIMAL_WIDTH, MINIMAL_HEIGHT, 4.0, 2.0, 1.0),
    };
    let x = 8.0 * scale;
    let y = vertical_inset * scale;
    let width = (logical_width - 16.0) * scale;
    let height = (logical_height - vertical_inset * 2.0) * scale;
    let radius = height / 2.0;
    for ring in (1..=3).rev() {
        let extent = shadow_extent * scale * ring as f32 / 3.0;
        let alpha = ((colors.shadow.0 >> 24) as u8 / (ring as u8 + 2)).max(4);
        canvas.fill_rounded_rect(
            x - extent,
            y - extent + shadow_offset * scale,
            width + extent * 2.0,
            height + extent * 2.0,
            radius + extent,
            Argb::new(alpha, 0, 0, 0),
        )?;
    }
    canvas.fill_rounded_rect(x, y, width, height, radius, colors.surface)?;
    canvas.stroke_rounded_rect(x, y, width, height, radius, scale.max(1.0), colors.border)?;
    canvas.draw_line(
        x + radius * 0.45,
        y + scale,
        x + width - radius * 0.45,
        y + scale,
        (0.5 * scale).max(1.0),
        colors.inner_highlight,
    )
}

fn draw_live(
    canvas: &mut Canvas<'_>,
    state: &OverlayViewState,
    layout: &DisplayLayout,
    colors: NativeColors,
) -> Result<(), RasterError> {
    draw_live_brand_mark(canvas, layout, colors)?;
    draw_live_elapsed(canvas, state, layout, colors)?;
    if !state.live_preview_available {
        return Ok(());
    }
    draw_live_divider(canvas, layout, colors)?;
    draw_live_preview(canvas, state, layout, colors)
}

fn draw_live_brand_mark(
    canvas: &mut Canvas<'_>,
    layout: &DisplayLayout,
    colors: NativeColors,
) -> Result<(), RasterError> {
    let scale = layout.scale;
    let center_x = layout.recording_mark.center_x();
    canvas.draw_centered_text_in_rect(
        egui_phosphor::regular::WAVEFORM,
        center_x,
        layout.recording_mark.width(),
        layout.recording_mark,
        27.0 * scale,
        TextStyle::Phosphor,
        colors.waveform,
    )
}

fn draw_live_elapsed(
    canvas: &mut Canvas<'_>,
    state: &OverlayViewState,
    layout: &DisplayLayout,
    colors: NativeColors,
) -> Result<(), RasterError> {
    let scale = layout.scale;
    let elapsed = state
        .elapsed
        .map(format_elapsed)
        .unwrap_or_else(|| "00:00".to_owned());
    canvas.draw_text_centered_in_rect(
        &elapsed,
        layout.elapsed.x0,
        layout.elapsed.width(),
        layout.elapsed,
        13.0 * scale,
        TextStyle::Regular,
        colors.muted_text,
    )?;
    Ok(())
}

fn draw_live_divider(
    canvas: &mut Canvas<'_>,
    layout: &DisplayLayout,
    colors: NativeColors,
) -> Result<(), RasterError> {
    let scale = layout.scale;
    let divider = layout
        .divider_line
        .expect("live layout includes divider line bounds");
    canvas.draw_line(
        divider.center_x(),
        divider.y0,
        divider.center_x(),
        divider.y1,
        scale.max(1.0),
        colors.border,
    )
}

fn draw_live_preview(
    canvas: &mut Canvas<'_>,
    state: &OverlayViewState,
    layout: &DisplayLayout,
    colors: NativeColors,
) -> Result<(), RasterError> {
    let scale = layout.scale;
    let preview = layout.preview.expect("live layout includes preview bounds");
    let max_width = preview.width();
    let line = live_line(state, colors);
    let line = if state.error.is_some() || state.notice.is_some() {
        fit_head(
            canvas,
            &line,
            max_width,
            13.0 * scale,
            MAX_MESSAGE_GRAPHEMES,
        )?
    } else {
        fit_head(
            canvas,
            &line,
            max_width,
            13.0 * scale,
            MAX_PREVIEW_GRAPHEMES,
        )?
    };
    canvas.draw_styled_line(&line, preview.x0, max_width, preview, 13.0 * scale)
}

fn draw_compact(
    canvas: &mut Canvas<'_>,
    state: &OverlayViewState,
    layout: &DisplayLayout,
    colors: NativeColors,
) -> Result<(), RasterError> {
    draw_compact_status_indicator(canvas, state, layout)?;
    draw_compact_status_text(canvas, state, layout, colors)?;
    draw_compact_meter(canvas, state, layout, colors)?;
    draw_compact_elapsed(canvas, state, layout, colors)
}

fn draw_compact_status_indicator(
    canvas: &mut Canvas<'_>,
    state: &OverlayViewState,
    layout: &DisplayLayout,
) -> Result<(), RasterError> {
    let phase = phase_color(state.phase);
    canvas.fill_ellipse(
        layout.recording_mark.x0,
        layout.recording_mark.y0,
        layout.recording_mark.width(),
        layout.recording_mark.height(),
        phase,
    )
}

fn draw_compact_status_text(
    canvas: &mut Canvas<'_>,
    state: &OverlayViewState,
    layout: &DisplayLayout,
    colors: NativeColors,
) -> Result<(), RasterError> {
    let scale = layout.scale;
    let label = if state.phase == OverlayPhase::Listening {
        "Scribe is recording"
    } else {
        state.phase.label()
    };
    let status_text = layout
        .status_text
        .expect("compact layout includes status text bounds");
    canvas.draw_text_centered_in_rect(
        label,
        status_text.x0,
        status_text.width(),
        status_text,
        13.0 * scale,
        TextStyle::Bold,
        colors.text,
    )?;
    Ok(())
}

fn draw_compact_meter(
    canvas: &mut Canvas<'_>,
    state: &OverlayViewState,
    layout: &DisplayLayout,
    colors: NativeColors,
) -> Result<(), RasterError> {
    let scale = layout.scale;
    let center_y = layout.recording_mark.center_y();
    let level = normalized_level(state);
    for index in 0..4 {
        let threshold = (index + 1) as f32 / 4.0;
        let active = level >= threshold * 0.78;
        let normalized_height = if active { threshold } else { 0.22 };
        let height = (layout.meter.height() * normalized_height).max(4.0 * scale);
        let x = layout.meter.x0 + index as f32 * 9.0 * scale;
        canvas.fill_rounded_rect(
            x,
            center_y - height / 2.0,
            7.0 * scale,
            height,
            2.0 * scale,
            if active {
                colors.meter_active
            } else {
                colors.meter_inactive
            },
        )?;
    }
    Ok(())
}

fn draw_compact_elapsed(
    canvas: &mut Canvas<'_>,
    state: &OverlayViewState,
    layout: &DisplayLayout,
    colors: NativeColors,
) -> Result<(), RasterError> {
    let scale = layout.scale;
    if let Some(elapsed) = state.elapsed {
        canvas.draw_text_centered_in_rect(
            &format_elapsed(elapsed),
            layout.elapsed.x0,
            layout.elapsed.width(),
            layout.elapsed,
            12.0 * scale,
            TextStyle::Monospace,
            colors.muted_text,
        )?;
    }
    Ok(())
}

fn normalized_level(state: &OverlayViewState) -> f32 {
    state
        .audio_level
        .rms
        .max(state.audio_level.peak * 0.7)
        .clamp(0.0, 1.0)
}

fn live_line(state: &OverlayViewState, colors: NativeColors) -> StyledLine {
    if let Some(error) = &state.error {
        let suffix = match error.recovery {
            OverlayRecovery::None => "",
            OverlayRecovery::Retry => " You can retry.",
            OverlayRecovery::WaitForPreview => " Wait for the current preview worker to exit.",
        };
        return StyledLine::plain(format!("{}{suffix}", error.message), colors.error);
    }
    if let Some(notice) = &state.notice {
        return StyledLine::plain(notice, colors.warning);
    }
    let committed = &state.transcript.committed;
    let tentative = &state.transcript.tentative;
    if committed.is_empty() && tentative.is_empty() {
        return StyledLine::plain(state.phase.label(), colors.muted_text);
    }
    let mut sections = Vec::new();
    if !committed.is_empty() {
        sections.push(StyledSection {
            text: committed.clone(),
            color: colors.muted_text,
            style: TextStyle::Regular,
        });
    }
    if !committed.is_empty()
        && !tentative.is_empty()
        && !committed.ends_with(char::is_whitespace)
        && !tentative.starts_with(char::is_whitespace)
        && !tentative.starts_with(is_left_binding_punctuation)
    {
        sections.push(StyledSection {
            text: " ".to_owned(),
            color: colors.muted_text,
            style: TextStyle::Regular,
        });
    }
    if !tentative.is_empty() {
        sections.push(StyledSection {
            text: tentative.clone(),
            color: colors.muted_text,
            style: TextStyle::Regular,
        });
    }
    StyledLine { sections }
}

fn fit_head(
    canvas: &mut Canvas<'_>,
    line: &StyledLine,
    max_width: f32,
    font_size: f32,
    limit: usize,
) -> Result<StyledLine, RasterError> {
    let total = line.grapheme_count().min(limit);
    binary_search_fit(total, |keep| line.head(keep), canvas, max_width, font_size)
}

fn binary_search_fit(
    total: usize,
    candidate: impl Fn(usize) -> StyledLine,
    canvas: &mut Canvas<'_>,
    max_width: f32,
    font_size: f32,
) -> Result<StyledLine, RasterError> {
    let mut low = 0;
    let mut high = total;
    let mut best = candidate(0);
    while low <= high {
        let keep = low + (high - low) / 2;
        let next = candidate(keep);
        if canvas.measure_styled_line(&next, font_size)? <= max_width {
            best = next;
            low = keep.saturating_add(1);
        } else if keep == 0 {
            break;
        } else {
            high = keep - 1;
        }
    }
    Ok(best)
}

fn slice_styled_line(line: &StyledLine, start: usize, end: usize, head: bool) -> StyledLine {
    let text = line.text();
    let total = text.graphemes(true).count();
    if start == 0 && end >= total {
        return line.clone();
    }
    let start_byte = grapheme_byte_index(&text, start);
    let end_byte = grapheme_byte_index(&text, end);
    let mut sections = Vec::new();
    let mut section_start = 0;
    for section in &line.sections {
        let section_end = section_start + section.text.len();
        let overlap_start = start_byte.max(section_start);
        let overlap_end = end_byte.min(section_end);
        if overlap_start < overlap_end {
            sections.push(StyledSection {
                text: section.text[overlap_start - section_start..overlap_end - section_start]
                    .to_owned(),
                color: section.color,
                style: section.style,
            });
        }
        section_start = section_end;
    }
    let ellipsis_style = if head {
        sections.last().cloned()
    } else {
        sections.first().cloned()
    }
    .or_else(|| line.sections.first().cloned())
    .unwrap_or(StyledSection {
        text: String::new(),
        color: Argb::new(255, 255, 255, 255),
        style: TextStyle::Regular,
    });
    let ellipsis = StyledSection {
        text: "…".to_owned(),
        ..ellipsis_style
    };
    if head {
        sections.push(ellipsis);
    } else {
        sections.insert(0, ellipsis);
    }
    StyledLine { sections }
}

fn grapheme_byte_index(text: &str, grapheme: usize) -> usize {
    text.grapheme_indices(true)
        .nth(grapheme)
        .map_or(text.len(), |(index, _)| index)
}

fn is_left_binding_punctuation(character: char) -> bool {
    matches!(
        character,
        '.' | ',' | '!' | '?' | ':' | ';' | '%' | ')' | ']' | '}' | '…'
    )
}

fn phase_color(phase: OverlayPhase) -> Argb {
    match phase {
        OverlayPhase::Error => Argb::new(255, 239, 108, 104),
        OverlayPhase::Success => Argb::new(255, 91, 201, 158),
        OverlayPhase::Hidden => Argb::TRANSPARENT,
        _ => Argb::new(255, 105, 169, 255),
    }
}

pub(super) fn format_elapsed(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    format!("{:02}:{:02}", seconds / 60, seconds % 60)
}

struct Canvas<'a> {
    graphics: *mut GpGraphics,
    bitmap: *mut GpBitmap,
    rasterizer: &'a NativeRasterizer,
    _pixels: &'a mut [u8],
}

impl<'a> Canvas<'a> {
    fn new(
        rasterizer: &'a NativeRasterizer,
        pixels: &'a mut [u8],
        width: i32,
        height: i32,
    ) -> Result<Self, RasterError> {
        let mut bitmap = null_mut();
        let stride = width.checked_mul(4).ok_or(RasterError::InvalidDimensions)?;
        status(
            unsafe {
                GdipCreateBitmapFromScan0(
                    width,
                    height,
                    stride,
                    PIXEL_FORMAT_32BPP_PARGB,
                    pixels.as_mut_ptr(),
                    &mut bitmap,
                )
            },
            "create bitmap",
        )?;
        let mut graphics = null_mut();
        if let Err(error) = status(
            unsafe { GdipGetImageGraphicsContext(bitmap.cast::<GpImage>(), &mut graphics) },
            "create graphics",
        ) {
            unsafe {
                GdipDisposeImage(bitmap.cast::<GpImage>());
            }
            return Err(error);
        }
        let mut canvas = Self {
            graphics,
            bitmap,
            rasterizer,
            _pixels: pixels,
        };
        canvas.initialize()?;
        Ok(canvas)
    }

    fn initialize(&mut self) -> Result<(), RasterError> {
        status(
            unsafe { GdipSetSmoothingMode(self.graphics, SmoothingModeAntiAlias8x8) },
            "set smoothing",
        )?;
        status(
            unsafe { GdipSetTextRenderingHint(self.graphics, TextRenderingHintAntiAliasGridFit) },
            "set text rendering",
        )?;
        status(
            unsafe { GdipGraphicsClear(self.graphics, Argb::TRANSPARENT.0) },
            "clear bitmap",
        )
    }

    fn fill_rounded_rect(
        &mut self,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        radius: f32,
        color: Argb,
    ) -> Result<(), RasterError> {
        let path = RoundedPath::new(x, y, width, height, radius)?;
        with_brush(color, |brush| {
            status(
                unsafe { GdipFillPath(self.graphics, brush, path.0) },
                "fill rounded rectangle",
            )
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn stroke_rounded_rect(
        &mut self,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        radius: f32,
        stroke_width: f32,
        color: Argb,
    ) -> Result<(), RasterError> {
        let path = RoundedPath::new(x, y, width, height, radius)?;
        with_pen(color, stroke_width, |pen| {
            status(
                unsafe { GdipDrawPath(self.graphics, pen, path.0) },
                "stroke rounded rectangle",
            )
        })
    }

    fn fill_ellipse(
        &mut self,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        color: Argb,
    ) -> Result<(), RasterError> {
        with_brush(color, |brush| {
            status(
                unsafe { GdipFillEllipse(self.graphics, brush, x, y, width, height) },
                "fill ellipse",
            )
        })
    }

    fn draw_line(
        &mut self,
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        width: f32,
        color: Argb,
    ) -> Result<(), RasterError> {
        with_pen(color, width.max(1.0), |pen| {
            status(
                unsafe { GdipDrawLine(self.graphics, pen, x1, y1, x2, y2) },
                "draw line",
            )
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_centered_text_in_rect(
        &mut self,
        text: &str,
        center_x: f32,
        width: f32,
        rect: super::layout::PhysicalRect,
        font_size: f32,
        style: TextStyle,
        color: Argb,
    ) -> Result<(), RasterError> {
        let measured = self.measure_text(text, font_size, style)?;
        self.draw_text_centered_in_rect(
            text,
            center_x - measured.min(width) / 2.0,
            width,
            rect,
            font_size,
            style,
            color,
        )?;
        Ok(())
    }

    /// The cancel control has independent hit-target geometry and intentionally
    /// retains its established glyph placement. Overlay content uses the
    /// centerline-aware variant above.
    #[allow(clippy::too_many_arguments)]
    fn draw_centered_text(
        &mut self,
        text: &str,
        center_x: f32,
        y: f32,
        width: f32,
        height: f32,
        font_size: f32,
        style: TextStyle,
        color: Argb,
    ) -> Result<(), RasterError> {
        let measured = self.measure_text(text, font_size, style)?;
        self.draw_text(
            text,
            center_x - measured.min(width) / 2.0,
            y,
            width,
            height,
            font_size,
            style,
            color,
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_text_centered_in_rect(
        &mut self,
        text: &str,
        x: f32,
        width: f32,
        rect: super::layout::PhysicalRect,
        font_size: f32,
        style: TextStyle,
        color: Argb,
    ) -> Result<f32, RasterError> {
        // Font/style/scale baseline metrics are bounded and reused by the
        // rasterizer, avoiding text-dependent allocation or transcript
        // retention on every meter repaint.
        let y = rect.center_y() - self.rasterizer.baseline_ink_center_y(font_size, style)?;
        self.draw_text(text, x, y, width, rect.height(), font_size, style, color)
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_text(
        &mut self,
        text: &str,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        font_size: f32,
        style: TextStyle,
        color: Argb,
    ) -> Result<f32, RasterError> {
        let font = Font::new(self.rasterizer, style, font_size)?;
        let format = StringFormat::new()?;
        let wide: Vec<u16> = text.encode_utf16().collect();
        let length = i32::try_from(wide.len()).map_err(|_| RasterError::TextTooLong)?;
        let layout = RectF {
            X: x,
            Y: y,
            Width: width,
            Height: height,
        };
        let mut measured: RectF = unsafe { zeroed() };
        status(
            unsafe {
                GdipMeasureString(
                    self.graphics,
                    wide.as_ptr(),
                    length,
                    font.font,
                    &layout,
                    format.0,
                    &mut measured,
                    null_mut(),
                    null_mut(),
                )
            },
            "measure text",
        )?;
        with_brush(color, |brush| {
            status(
                unsafe {
                    GdipDrawString(
                        self.graphics,
                        wide.as_ptr(),
                        length,
                        font.font,
                        &layout,
                        format.0,
                        brush,
                    )
                },
                "draw text",
            )
        })?;
        Ok(measured.Width.max(0.0))
    }

    fn measure_text(
        &mut self,
        text: &str,
        font_size: f32,
        style: TextStyle,
    ) -> Result<f32, RasterError> {
        Ok(self
            .measure_text_bounds(text, font_size, style)?
            .Width
            .max(0.0))
    }

    fn measure_text_bounds(
        &mut self,
        text: &str,
        font_size: f32,
        style: TextStyle,
    ) -> Result<RectF, RasterError> {
        let font = Font::new(self.rasterizer, style, font_size)?;
        let format = StringFormat::new()?;
        let wide: Vec<u16> = text.encode_utf16().collect();
        let length = i32::try_from(wide.len()).map_err(|_| RasterError::TextTooLong)?;
        let layout = RectF {
            X: 0.0,
            Y: 0.0,
            Width: 32_768.0,
            Height: font_size * 2.5,
        };
        let mut measured: RectF = unsafe { zeroed() };
        status(
            unsafe {
                GdipMeasureString(
                    self.graphics,
                    wide.as_ptr(),
                    length,
                    font.font,
                    &layout,
                    format.0,
                    &mut measured,
                    null_mut(),
                    null_mut(),
                )
            },
            "measure text",
        )?;
        Ok(measured)
    }

    fn measure_styled_line(
        &mut self,
        line: &StyledLine,
        font_size: f32,
    ) -> Result<f32, RasterError> {
        line.sections.iter().try_fold(0.0, |width, section| {
            self.measure_text(&section.text, font_size, section.style)
                .map(|section_width| width + section_width)
        })
    }

    fn draw_styled_line(
        &mut self,
        line: &StyledLine,
        x: f32,
        width: f32,
        rect: super::layout::PhysicalRect,
        font_size: f32,
    ) -> Result<(), RasterError> {
        let mut cursor = x;
        let right = x + width;
        for section in &line.sections {
            if cursor >= right {
                break;
            }
            let measured = self.draw_text_centered_in_rect(
                &section.text,
                cursor,
                right - cursor,
                rect,
                font_size,
                section.style,
                section.color,
            )?;
            cursor += measured;
        }
        Ok(())
    }
}

impl Drop for Canvas<'_> {
    fn drop(&mut self) {
        unsafe {
            GdipDeleteGraphics(self.graphics);
            GdipDisposeImage(self.bitmap.cast::<GpImage>());
        }
    }
}

struct RoundedPath(*mut GpPath);

impl RoundedPath {
    fn new(x: f32, y: f32, width: f32, height: f32, radius: f32) -> Result<Self, RasterError> {
        let mut path = null_mut();
        status(
            unsafe { GdipCreatePath(FillModeAlternate, &mut path) },
            "create rounded path",
        )?;
        let path = Self(path);
        let radius = radius.min(width / 2.0).min(height / 2.0).max(0.5);
        let diameter = radius * 2.0;
        for (arc_x, arc_y, start) in [
            (x, y, 180.0),
            (x + width - diameter, y, 270.0),
            (x + width - diameter, y + height - diameter, 0.0),
            (x, y + height - diameter, 90.0),
        ] {
            status(
                unsafe { GdipAddPathArc(path.0, arc_x, arc_y, diameter, diameter, start, 90.0) },
                "add rounded path arc",
            )?;
        }
        status(unsafe { GdipClosePathFigure(path.0) }, "close rounded path")?;
        Ok(path)
    }
}

impl Drop for RoundedPath {
    fn drop(&mut self) {
        unsafe {
            GdipDeletePath(self.0);
        }
    }
}

struct Font {
    family: *mut GpFontFamily,
    font: *mut GpFont,
    owns_family: bool,
}

impl Font {
    fn new(
        rasterizer: &NativeRasterizer,
        style: TextStyle,
        size: f32,
    ) -> Result<Self, RasterError> {
        if style == TextStyle::Phosphor {
            return Self::from_borrowed_family(rasterizer.phosphor.family, FontStyleRegular, size);
        }
        let family_name = match style {
            TextStyle::Monospace => "Consolas",
            TextStyle::Regular | TextStyle::Bold => "Segoe UI",
            TextStyle::Phosphor => unreachable!("Phosphor is handled above"),
        };
        let font_style = match style {
            TextStyle::Bold => FontStyleBold,
            TextStyle::Regular | TextStyle::Monospace => FontStyleRegular,
            TextStyle::Phosphor => unreachable!("Phosphor is handled above"),
        };
        Self::from_named_family(&wide_null(family_name), font_style, size)
    }

    fn from_named_family(
        family_name: &[u16],
        font_style: i32,
        size: f32,
    ) -> Result<Self, RasterError> {
        let mut family = null_mut();
        let named_status =
            unsafe { GdipCreateFontFamilyFromName(family_name.as_ptr(), null_mut(), &mut family) };
        if named_status != GDI_PLUS_OK {
            status(
                unsafe { GdipGetGenericFontFamilySansSerif(&mut family) },
                "resolve fallback font family",
            )?;
        }
        Self::create(family, font_style, size, true)
    }

    fn from_borrowed_family(
        family: *mut GpFontFamily,
        font_style: i32,
        size: f32,
    ) -> Result<Self, RasterError> {
        if family.is_null() {
            return Err(RasterError::MissingPhosphorFamily);
        }
        let mut cloned_family = null_mut();
        status(
            unsafe { GdipCloneFontFamily(family, &mut cloned_family) },
            "clone Phosphor font family",
        )?;
        Self::create(cloned_family, font_style, size, true)
    }

    fn create(
        family: *mut GpFontFamily,
        font_style: i32,
        size: f32,
        owns_family: bool,
    ) -> Result<Self, RasterError> {
        let mut font = null_mut();
        if let Err(error) = status(
            unsafe { GdipCreateFont(family, size, font_style, UnitPixel, &mut font) },
            "create font",
        ) {
            if owns_family {
                unsafe {
                    GdipDeleteFontFamily(family);
                }
            }
            return Err(error);
        }
        Ok(Self {
            family,
            font,
            owns_family,
        })
    }
}

impl Drop for Font {
    fn drop(&mut self) {
        unsafe {
            GdipDeleteFont(self.font);
            if self.owns_family {
                GdipDeleteFontFamily(self.family);
            }
        }
    }
}

struct StringFormat(*mut GpStringFormat);

fn configure_owned_resource<T, E>(
    resource: T,
    configure: impl FnOnce(&T) -> Result<(), E>,
) -> Result<T, E> {
    configure(&resource)?;
    Ok(resource)
}

impl StringFormat {
    fn new() -> Result<Self, RasterError> {
        let mut format = null_mut();
        status(
            unsafe { GdipStringFormatGetGenericTypographic(&mut format) },
            "create string format",
        )?;
        configure_owned_resource(Self(format), |format| {
            status(
                unsafe {
                    GdipSetStringFormatFlags(
                        format.0,
                        StringFormatFlagsNoWrap | StringFormatFlagsMeasureTrailingSpaces,
                    )
                },
                "configure string format",
            )
        })
    }
}

impl Drop for StringFormat {
    fn drop(&mut self) {
        unsafe {
            GdipDeleteStringFormat(self.0);
        }
    }
}

fn with_brush<T>(
    color: Argb,
    use_brush: impl FnOnce(*mut GpBrush) -> Result<T, RasterError>,
) -> Result<T, RasterError> {
    let mut brush: *mut GpSolidFill = null_mut();
    status(
        unsafe { GdipCreateSolidFill(color.0, &mut brush) },
        "create brush",
    )?;
    let result = use_brush(brush.cast::<GpBrush>());
    unsafe {
        GdipDeleteBrush(brush.cast::<GpBrush>());
    }
    result
}

fn with_pen<T>(
    color: Argb,
    width: f32,
    use_pen: impl FnOnce(*mut GpPen) -> Result<T, RasterError>,
) -> Result<T, RasterError> {
    let mut pen = null_mut();
    status(
        unsafe { GdipCreatePen1(color.0, width, UnitPixel, &mut pen) },
        "create pen",
    )?;
    let result = use_pen(pen);
    unsafe {
        GdipDeletePen(pen);
    }
    result
}

struct GdiPlusSession(usize);

impl GdiPlusSession {
    fn start() -> Result<Self, RasterError> {
        let input = GdiplusStartupInput {
            GdiplusVersion: 1,
            DebugEventCallback: 0,
            SuppressBackgroundThread: 0,
            SuppressExternalCodecs: 1,
        };
        let mut token = 0;
        let result = unsafe { GdiplusStartup(&mut token, &input, null_mut()) };
        if result == GDI_PLUS_OK {
            Ok(Self(token))
        } else {
            Err(RasterError::GdiPlus("start GDI+", result))
        }
    }
}

impl Drop for GdiPlusSession {
    fn drop(&mut self) {
        unsafe {
            GdiplusShutdown(self.0);
        }
    }
}

struct PrivateFont {
    collection: *mut GpFontCollection,
    family: *mut GpFontFamily,
    _font_data: Cow<'static, [u8]>,
}

impl PrivateFont {
    fn phosphor_regular() -> Result<Self, RasterError> {
        let font_data = egui_phosphor::Variant::Regular.font_data().font;
        let length = i32::try_from(font_data.len()).map_err(|_| RasterError::TextTooLong)?;
        let mut collection = null_mut();
        status(
            unsafe { GdipNewPrivateFontCollection(&mut collection) },
            "create private Phosphor font collection",
        )?;
        if let Err(error) = status(
            unsafe {
                GdipPrivateAddMemoryFont(collection, font_data.as_ptr().cast::<c_void>(), length)
            },
            "load Phosphor font",
        ) {
            unsafe {
                GdipDeletePrivateFontCollection(&mut collection);
            }
            return Err(error);
        }

        let mut family_count = 0;
        if let Err(error) = status(
            unsafe { GdipGetFontCollectionFamilyCount(collection, &mut family_count) },
            "count Phosphor font families",
        ) {
            unsafe {
                GdipDeletePrivateFontCollection(&mut collection);
            }
            return Err(error);
        }
        if family_count < 1 {
            unsafe {
                GdipDeletePrivateFontCollection(&mut collection);
            }
            return Err(RasterError::MissingPhosphorFamily);
        }
        let mut family = null_mut();
        let mut found = 0;
        if let Err(error) = status(
            unsafe { GdipGetFontCollectionFamilyList(collection, 1, &mut family, &mut found) },
            "resolve Phosphor font family",
        ) {
            unsafe {
                GdipDeletePrivateFontCollection(&mut collection);
            }
            return Err(error);
        }
        if found != 1 || family.is_null() {
            unsafe {
                GdipDeletePrivateFontCollection(&mut collection);
            }
            return Err(RasterError::MissingPhosphorFamily);
        }
        // The family-list API returns collection-owned handles. Clone the selected family so
        // this object can release it exactly once before releasing the collection.
        let mut owned_family = null_mut();
        if let Err(error) = status(
            unsafe { GdipCloneFontFamily(family, &mut owned_family) },
            "retain Phosphor font family",
        ) {
            unsafe {
                GdipDeletePrivateFontCollection(&mut collection);
            }
            return Err(error);
        }
        Ok(Self {
            collection,
            family: owned_family,
            _font_data: font_data,
        })
    }
}

impl Drop for PrivateFont {
    fn drop(&mut self) {
        unsafe {
            GdipDeleteFontFamily(self.family);
            GdipDeletePrivateFontCollection(&mut self.collection);
        }
    }
}

fn status(code: i32, operation: &'static str) -> Result<(), RasterError> {
    if code == GDI_PLUS_OK {
        Ok(())
    } else {
        Err(RasterError::GdiPlus(operation, code))
    }
}

fn wide_null(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[derive(Clone, Debug, thiserror::Error)]
pub(super) enum RasterError {
    #[error("overlay frame dimensions are invalid")]
    InvalidDimensions,
    #[error("overlay text exceeds the native API length limit")]
    TextTooLong,
    #[error("the embedded Phosphor font has no usable font family")]
    MissingPhosphorFamily,
    #[error("GDI+ could not {0} (status {1})")]
    GdiPlus(&'static str, i32),
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, fmt::Write as _, path::Path, sync::Mutex};

    use super::super::layout::PhysicalRect;
    use super::*;
    use crate::{
        overlay::{
            controller::OverlayAudioLevel,
            platform::{OverlayPosition, PhysicalWorkArea, calculate_window_bounds},
            view::window_spec,
        },
        transcription::SessionId,
    };
    use sha2::{Digest, Sha256};

    static RASTER_TEST_MUTEX: Mutex<()> = Mutex::new(());

    fn with_rasterizer<T>(operation: impl FnOnce(&NativeRasterizer) -> T) -> T {
        let _guard = RASTER_TEST_MUTEX
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let rasterizer = NativeRasterizer::new().expect("initialize native rasterizer");
        operation(&rasterizer)
    }

    fn state(mode: OverlayMode) -> OverlayViewState {
        OverlayViewState {
            session_id: Some(SessionId(42)),
            mode,
            phase: OverlayPhase::Listening,
            live_preview_available: mode == OverlayMode::Live,
            audio_level: OverlayAudioLevel::new(0.65, 0.82),
            transcript: super::super::super::controller::OverlayTranscript {
                committed: "Clicking the settings icon in the top".to_owned(),
                tentative: "right...".to_owned(),
                revision: 7,
            },
            elapsed: Some(Duration::from_secs(12)),
            ..OverlayViewState::default()
        }
    }

    fn edge_golden_states() -> Vec<(&'static str, OverlayViewState, i32, i32)> {
        vec![
            (
                "live-empty",
                OverlayViewState {
                    session_id: Some(SessionId(43)),
                    mode: OverlayMode::Live,
                    phase: OverlayPhase::Listening,
                    live_preview_available: true,
                    elapsed: Some(Duration::ZERO),
                    ..OverlayViewState::default()
                },
                600,
                62,
            ),
            (
                "live-no-preview",
                OverlayViewState {
                    session_id: Some(SessionId(46)),
                    mode: OverlayMode::Live,
                    phase: OverlayPhase::Listening,
                    live_preview_available: false,
                    elapsed: Some(Duration::from_secs(12)),
                    ..OverlayViewState::default()
                },
                600,
                62,
            ),
            (
                "compact-finalizing",
                OverlayViewState {
                    session_id: Some(SessionId(44)),
                    mode: OverlayMode::Minimal,
                    phase: OverlayPhase::Finalizing,
                    ..OverlayViewState::default()
                },
                320,
                52,
            ),
            (
                "live-error",
                OverlayViewState {
                    session_id: Some(SessionId(45)),
                    mode: OverlayMode::Live,
                    phase: OverlayPhase::Error,
                    live_preview_available: true,
                    error: Some(super::super::super::controller::OverlayError {
                        message: "Microphone unavailable".to_owned(),
                        recovery: OverlayRecovery::Retry,
                    }),
                    ..OverlayViewState::default()
                },
                600,
                62,
            ),
        ]
    }

    fn production_bounds(mode: OverlayMode, dpi: u32) -> OverlayWindowBounds {
        calculate_window_bounds(
            PhysicalWorkArea {
                left: -2_000,
                top: 100,
                right: 2_000,
                bottom: 2_000,
            },
            dpi,
            window_spec(mode),
            OverlayPosition::BottomCenter,
        )
    }

    #[derive(Clone, Copy)]
    enum IsolatedComponent {
        BrandMark,
        Elapsed,
        Divider,
        Preview,
        CompactStatusIndicator,
        CompactStatus,
        CompactMeter,
        CompactElapsed,
    }

    /// Paints exactly one content layer on a canvas that is larger than its
    /// production viewport. This makes edge clipping and neighbor overlap
    /// observable without relying on the capsule background.
    fn isolated_component_frame(
        rasterizer: &NativeRasterizer,
        state: &OverlayViewState,
        layout: &DisplayLayout,
        dark_mode: bool,
        component: IsolatedComponent,
    ) -> LayeredFrame {
        let width = layout.root.width() as i32 + 4;
        let height = layout.root.height() as i32 + 4;
        let mut frame = LayeredFrame::transparent(width, height).unwrap();
        let mut canvas = Canvas::new(rasterizer, &mut frame.pixels, width, height).unwrap();
        let colors = NativeColors::for_theme(dark_mode);
        match component {
            IsolatedComponent::BrandMark => draw_live_brand_mark(&mut canvas, layout, colors),
            IsolatedComponent::Elapsed => draw_live_elapsed(&mut canvas, state, layout, colors),
            IsolatedComponent::Divider => draw_live_divider(&mut canvas, layout, colors),
            IsolatedComponent::Preview => draw_live_preview(&mut canvas, state, layout, colors),
            IsolatedComponent::CompactStatusIndicator => {
                draw_compact_status_indicator(&mut canvas, state, layout)
            }
            IsolatedComponent::CompactStatus => {
                draw_compact_status_text(&mut canvas, state, layout, colors)
            }
            IsolatedComponent::CompactMeter => {
                draw_compact_meter(&mut canvas, state, layout, colors)
            }
            IsolatedComponent::CompactElapsed => {
                draw_compact_elapsed(&mut canvas, state, layout, colors)
            }
        }
        .unwrap();
        drop(canvas);
        frame
    }

    fn live_shell_only_frame(
        rasterizer: &NativeRasterizer,
        state: &OverlayViewState,
        layout: &DisplayLayout,
        dark_mode: bool,
    ) -> LayeredFrame {
        let width = layout.root.width() as i32;
        let height = layout.root.height() as i32;
        let mut frame = LayeredFrame::transparent(width, height).unwrap();
        let mut canvas = Canvas::new(rasterizer, &mut frame.pixels, width, height).unwrap();
        let colors = NativeColors::for_theme(dark_mode);
        draw_capsule(&mut canvas, OverlayMode::Live, layout.scale, colors).unwrap();
        draw_live_brand_mark(&mut canvas, layout, colors).unwrap();
        draw_live_elapsed(&mut canvas, state, layout, colors).unwrap();
        drop(canvas);
        frame
    }

    #[derive(Clone, Copy, Debug)]
    struct InkBounds {
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
    }

    impl InkBounds {
        fn center_y(&self) -> f32 {
            (self.y0 + self.y1 + 1) as f32 / 2.0
        }

        fn intersects(self, other: Self) -> bool {
            self.x0 <= other.x1 && other.x0 <= self.x1 && self.y0 <= other.y1 && other.y0 <= self.y1
        }
    }

    fn component_ink_bounds(content: &LayeredFrame) -> Option<InkBounds> {
        let mut result: Option<InkBounds> = None;
        for y in 0..content.height {
            for x in 0..content.width {
                let offset = ((y * content.width + x) * 4) as usize;
                if content.pixels[offset + 3] > 0 {
                    let bounds = result.get_or_insert(InkBounds {
                        x0: x,
                        y0: y,
                        x1: x,
                        y1: y,
                    });
                    bounds.x0 = bounds.x0.min(x);
                    bounds.y0 = bounds.y0.min(y);
                    bounds.x1 = bounds.x1.max(x);
                    bounds.y1 = bounds.y1.max(y);
                }
            }
        }
        result
    }

    fn assert_component_is_contained_and_centered(
        component: &str,
        content: &LayeredFrame,
        rect: PhysicalRect,
        center_y: f32,
        visual_center_tolerance: f32,
        requires_vertical_edge_margin: bool,
    ) -> InkBounds {
        let ink =
            component_ink_bounds(content).unwrap_or_else(|| panic!("{component} painted no ink"));
        assert!(
            (ink.center_y() - center_y).abs() <= visual_center_tolerance,
            "{component} ink center {} drifted from physical centerline {center_y}: {ink:?}",
            ink.center_y(),
        );
        assert!(
            ink.x0 as f32 + 0.5 >= rect.x0 - 0.5
                && ink.y0 as f32 + 0.5 >= rect.y0 - 0.5
                && ink.x1 as f32 + 0.5 <= rect.x1 + 0.5
                && ink.y1 as f32 + 0.5 <= rect.y1 + 0.5,
            "{component} ink escaped its assigned rectangle: {ink:?} vs {rect:?}"
        );
        if requires_vertical_edge_margin {
            assert!(
                ink.y0 as f32 + 0.5 > rect.y0 && ink.y1 as f32 + 0.5 < rect.y1,
                "{component} ink touched an assigned vertical edge and may be clipped: {ink:?} vs {rect:?}"
            );
        }
        ink
    }

    fn assert_no_adjacent_ink_overlap(components: &[(&str, InkBounds)]) {
        for (index, (name, bounds)) in components.iter().enumerate() {
            for (other_name, other_bounds) in components.iter().skip(index + 1) {
                assert!(
                    !bounds.intersects(*other_bounds),
                    "{name} ink {bounds:?} overlapped {other_name} ink {other_bounds:?}"
                );
            }
        }
    }

    #[test]
    fn live_frame_has_transparent_corners_and_a_painted_capsule() {
        let frame = with_rasterizer(|rasterizer| {
            rasterizer
                .render_display(&state(OverlayMode::Live), true, 600, 62)
                .unwrap()
        });
        assert_eq!(frame.alpha_at(0, 0), 0);
        assert!(frame.alpha_at(300, 31) > 0);
        assert!(frame.pixels.chunks_exact(4).any(|pixel| pixel[3] > 0));
    }

    #[test]
    fn compact_frame_has_transparent_corners_and_a_painted_capsule() {
        let frame = with_rasterizer(|rasterizer| {
            rasterizer
                .render_display(&state(OverlayMode::Minimal), true, 320, 52)
                .unwrap()
        });
        assert_eq!(frame.alpha_at(0, 0), 0);
        assert!(frame.alpha_at(160, 26) > 0);
        assert!(frame.pixels.chunks_exact(4).any(|pixel| pixel[3] > 0));
    }

    #[test]
    fn cached_font_baselines_are_bounded_reused_and_release_evicted_probe_scopes() {
        with_rasterizer(|rasterizer| {
            let live = state(OverlayMode::Live);
            rasterizer.render_display(&live, true, 600, 62).unwrap();
            let first = rasterizer.baseline_cache_stats();
            assert_eq!(first, (2, 2), "one metric per Live font/style pair");

            rasterizer.render_display(&live, false, 600, 62).unwrap();
            assert_eq!(
                rasterizer.baseline_cache_stats(),
                first,
                "theme changes and repeated live frames must reuse cached baselines"
            );

            for index in 0..BASELINE_METRIC_CAPACITY + 3 {
                rasterizer
                    .baseline_ink_center_y(8.0 + index as f32, TextStyle::Regular)
                    .unwrap();
            }
            let (entries, probes) = rasterizer.baseline_cache_stats();
            assert_eq!(entries, BASELINE_METRIC_CAPACITY);
            assert_eq!(
                probes,
                first.1 + BASELINE_METRIC_CAPACITY + 2,
                "only the pre-existing 13 px Regular metric may be reused during eviction"
            );

            // Each evicted probe owned a bitmap, graphics context, and font
            // only for its measurement scope. A normal frame must still
            // render after fixed-capacity churn, exercising their RAII drops.
            rasterizer.render_display(&live, true, 600, 62).unwrap();
            assert_eq!(
                rasterizer.baseline_cache_stats().0,
                BASELINE_METRIC_CAPACITY
            );
        });
    }

    #[test]
    fn standalone_preview_separator_renders_without_a_text_ink_probe() {
        let mut live = state(OverlayMode::Live);
        live.transcript.committed = "Hello".to_owned();
        live.transcript.tentative = "world".to_owned();
        let line = live_line(&live, NativeColors::for_theme(true));
        assert!(line.sections.iter().any(|section| section.text == " "));
        with_rasterizer(|rasterizer| {
            let mut pixels = vec![0; 128 * 64 * 4];
            let mut canvas = Canvas::new(rasterizer, &mut pixels, 128, 64).unwrap();
            assert!(
                canvas.measure_text(" ", 13.0, TextStyle::Regular).unwrap() > 0.0,
                "the standalone separator must retain a measurable advance"
            );
            drop(canvas);
            let frame = rasterizer.render_display(&live, true, 600, 62).unwrap();
            assert!(frame.pixels.chunks_exact(4).any(|pixel| pixel[3] > 0));
            assert_eq!(
                rasterizer.baseline_cache_stats().0,
                2,
                "the separator must reuse the Regular baseline instead of probing its zero-ink glyph"
            );
        });
    }

    #[test]
    fn every_rastered_content_element_uses_the_production_centerline_at_supported_dpi() {
        with_rasterizer(|rasterizer| {
            for mode in [OverlayMode::Live, OverlayMode::Minimal] {
                for dpi in [96, 120, 144, 192] {
                    let bounds = production_bounds(mode, dpi);
                    let layout = DisplayLayout::from_bounds(mode, bounds).unwrap();
                    for dark_mode in [false, true] {
                        let state = state(mode);
                        match mode {
                            OverlayMode::Live => {
                                let waveform_frame = isolated_component_frame(
                                    rasterizer,
                                    &state,
                                    &layout,
                                    dark_mode,
                                    IsolatedComponent::BrandMark,
                                );
                                let waveform = assert_component_is_contained_and_centered(
                                    "Scribe brand mark",
                                    &waveform_frame,
                                    layout.recording_mark,
                                    layout.content_center_y,
                                    0.5,
                                    false,
                                );
                                let elapsed_frame = isolated_component_frame(
                                    rasterizer,
                                    &state,
                                    &layout,
                                    dark_mode,
                                    IsolatedComponent::Elapsed,
                                );
                                let elapsed = assert_component_is_contained_and_centered(
                                    "elapsed time",
                                    &elapsed_frame,
                                    layout.elapsed,
                                    layout.content_center_y,
                                    2.5,
                                    true,
                                );
                                let divider_frame = isolated_component_frame(
                                    rasterizer,
                                    &state,
                                    &layout,
                                    dark_mode,
                                    IsolatedComponent::Divider,
                                );
                                let divider = assert_component_is_contained_and_centered(
                                    "divider",
                                    &divider_frame,
                                    layout.divider.unwrap(),
                                    layout.content_center_y,
                                    0.5,
                                    true,
                                );
                                let preview_frame = isolated_component_frame(
                                    rasterizer,
                                    &state,
                                    &layout,
                                    dark_mode,
                                    IsolatedComponent::Preview,
                                );
                                let preview = assert_component_is_contained_and_centered(
                                    "preview",
                                    &preview_frame,
                                    layout.preview.unwrap(),
                                    layout.content_center_y,
                                    2.5,
                                    true,
                                );
                                assert_no_adjacent_ink_overlap(&[
                                    ("Scribe brand mark", waveform),
                                    ("elapsed time", elapsed),
                                    ("divider", divider),
                                    ("preview", preview),
                                ]);
                            }
                            OverlayMode::Minimal | OverlayMode::Off => {
                                let indicator_frame = isolated_component_frame(
                                    rasterizer,
                                    &state,
                                    &layout,
                                    dark_mode,
                                    IsolatedComponent::CompactStatusIndicator,
                                );
                                let indicator = assert_component_is_contained_and_centered(
                                    "compact status indicator",
                                    &indicator_frame,
                                    layout.recording_mark,
                                    layout.content_center_y,
                                    0.5,
                                    false,
                                );
                                let status_frame = isolated_component_frame(
                                    rasterizer,
                                    &state,
                                    &layout,
                                    dark_mode,
                                    IsolatedComponent::CompactStatus,
                                );
                                let status = assert_component_is_contained_and_centered(
                                    "compact status",
                                    &status_frame,
                                    layout.status_text.unwrap(),
                                    layout.content_center_y,
                                    2.5,
                                    true,
                                );
                                let meter_frame = isolated_component_frame(
                                    rasterizer,
                                    &state,
                                    &layout,
                                    dark_mode,
                                    IsolatedComponent::CompactMeter,
                                );
                                let meter = assert_component_is_contained_and_centered(
                                    "compact meter",
                                    &meter_frame,
                                    layout.meter,
                                    layout.content_center_y,
                                    0.5,
                                    false,
                                );
                                let compact_elapsed_frame = isolated_component_frame(
                                    rasterizer,
                                    &state,
                                    &layout,
                                    dark_mode,
                                    IsolatedComponent::CompactElapsed,
                                );
                                let compact_elapsed = assert_component_is_contained_and_centered(
                                    "compact elapsed time",
                                    &compact_elapsed_frame,
                                    layout.elapsed,
                                    layout.content_center_y,
                                    2.5,
                                    true,
                                );
                                assert_no_adjacent_ink_overlap(&[
                                    ("compact status indicator", indicator),
                                    ("compact status", status),
                                    ("compact meter", meter),
                                    ("compact elapsed time", compact_elapsed),
                                ]);
                            }
                        }
                    }
                }
            }
        });
    }

    #[test]
    fn live_mode_without_a_started_preview_paints_only_the_reference_logo_and_timer_shell() {
        with_rasterizer(|rasterizer| {
            for dpi in [96, 120, 144, 192] {
                let bounds = production_bounds(OverlayMode::Live, dpi);
                let layout = DisplayLayout::from_bounds(OverlayMode::Live, bounds).unwrap();
                for dark_mode in [false, true] {
                    let mut unavailable = state(OverlayMode::Live);
                    unavailable.live_preview_available = false;
                    unavailable.transcript.committed = "must not leak".to_owned();
                    unavailable.transcript.tentative = "into the overlay".to_owned();
                    let rendered = rasterizer
                        .render_display(&unavailable, dark_mode, bounds.width, bounds.height)
                        .unwrap();
                    let shell = live_shell_only_frame(rasterizer, &unavailable, &layout, dark_mode);
                    assert_eq!(
                        rendered, shell,
                        "unavailable live preview painted divider or transcript content at {dpi} DPI"
                    );

                    let available = rasterizer
                        .render_display(
                            &state(OverlayMode::Live),
                            dark_mode,
                            bounds.width,
                            bounds.height,
                        )
                        .unwrap();
                    assert_ne!(
                        available, shell,
                        "started preview must add the divider and transcript at {dpi} DPI"
                    );
                }
            }
        });
    }

    #[test]
    fn control_frame_paints_only_a_transparent_x_surface() {
        let frame = with_rasterizer(|rasterizer| rasterizer.render_control(true, 44, 44).unwrap());
        assert_eq!(frame.alpha_at(0, 0), 0);
        assert!(frame.pixels.chunks_exact(4).any(|pixel| pixel[3] > 0));
        let painted = frame
            .pixels
            .chunks_exact(4)
            .filter(|pixel| pixel[3] > 0)
            .count();
        assert!(painted < 44 * 44 / 3);
    }

    #[test]
    fn reference_contract_fixture_checksums_are_valid() {
        let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("testdata")
            .join("overlay-reference");
        let sums = std::fs::read_to_string(fixture_root.join("SHA256SUMS"))
            .expect("read reference-contract fixture checksums");

        for (line_number, line) in sums.lines().enumerate() {
            let (expected, name) = line
                .split_once("  ")
                .unwrap_or_else(|| panic!("invalid SHA256SUMS line {}", line_number + 1));
            let bytes = std::fs::read(fixture_root.join(name))
                .unwrap_or_else(|error| panic!("read reference-contract fixture {name}: {error}"));
            assert_eq!(
                format!("{:x}", Sha256::digest(bytes)),
                expected,
                "fixture digest mismatch for {name}"
            );
        }
    }

    #[test]
    fn reference_contract_native_overlay_raster_golden_frames_are_pixel_identical() {
        let fixture_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("testdata")
            .join("overlay-reference");
        let scales = [(1, 1, "96"), (5, 4, "120"), (3, 2, "144"), (2, 1, "192")];

        with_rasterizer(|rasterizer| {
            for (numerator, denominator, dpi) in scales {
                for (dark, theme) in [(false, "light"), (true, "dark")] {
                    for (mode, name, logical_width, logical_height) in [
                        (OverlayMode::Live, "live", 600, 62),
                        (OverlayMode::Minimal, "compact", 320, 52),
                    ] {
                        let frame = rasterizer
                            .render_display(
                                &state(mode),
                                dark,
                                logical_width * numerator / denominator,
                                logical_height * numerator / denominator,
                            )
                            .expect("render overlay fixture frame");
                        assert_eq!(
                            frame.pixels,
                            std::fs::read(fixture_root.join(format!("{name}-{theme}-{dpi}.bgra")))
                                .expect("read immutable reference-contract overlay fixture"),
                            "{name} {theme} at {dpi} DPI diverged from the approved reference contract"
                        );
                    }

                    let control = rasterizer
                        .render_control(
                            dark,
                            44 * numerator / denominator,
                            44 * numerator / denominator,
                        )
                        .expect("render cancel-control fixture frame");
                    assert_eq!(
                        control.pixels,
                        std::fs::read(fixture_root.join(format!("cancel-{theme}-{dpi}.bgra")))
                            .expect("read immutable reference-contract cancel-control fixture"),
                        "cancel control {theme} at {dpi} DPI diverged from the approved reference contract"
                    );
                }
            }

            for (name, state, width, height) in edge_golden_states() {
                for (dark, theme) in [(false, "light"), (true, "dark")] {
                    let frame = rasterizer
                        .render_display(&state, dark, width, height)
                        .expect("render edge-state overlay fixture frame");
                    assert_eq!(
                        frame.pixels,
                        std::fs::read(fixture_root.join(format!("{name}-{theme}-96.bgra")))
                            .expect("read immutable reference-contract edge-state fixture"),
                        "{name} {theme} at 96 DPI diverged from the approved reference contract"
                    );
                }
            }
        });
    }

    #[test]
    #[ignore = "explicit fixture-maintenance tool; see testdata/overlay-reference/MANIFEST.md"]
    fn generate_reference_contract_overlay_fixture_candidate() {
        let requested = std::path::PathBuf::from(
            std::env::var_os("SCRIBE_OVERLAY_REFERENCE_OUTPUT_DIR")
                .expect("set SCRIBE_OVERLAY_REFERENCE_OUTPUT_DIR to an external output directory"),
        );
        assert!(requested.is_absolute(), "fixture output must be absolute");
        let parent = requested
            .parent()
            .expect("fixture output must have a parent")
            .canonicalize()
            .expect("fixture output parent must already exist");
        let output = parent.join(
            requested
                .file_name()
                .expect("fixture output must name a directory"),
        );
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .canonicalize()
            .expect("canonicalize repository root");
        assert!(
            !output.starts_with(&repository),
            "fixture output must be outside the repository"
        );
        std::fs::create_dir_all(&output).expect("create external fixture output directory");

        let mut generated = Vec::new();
        let mut write_frame = |name: String, pixels: Vec<u8>| {
            std::fs::write(output.join(&name), &pixels)
                .unwrap_or_else(|error| panic!("write generated fixture {name}: {error}"));
            generated.push((name, format!("{:x}", Sha256::digest(pixels))));
        };

        with_rasterizer(|rasterizer| {
            for (numerator, denominator, dpi) in
                [(1, 1, "96"), (5, 4, "120"), (3, 2, "144"), (2, 1, "192")]
            {
                for (dark, theme) in [(false, "light"), (true, "dark")] {
                    for (mode, name, logical_width, logical_height) in [
                        (OverlayMode::Live, "live", 600, 62),
                        (OverlayMode::Minimal, "compact", 320, 52),
                    ] {
                        let frame = rasterizer
                            .render_display(
                                &state(mode),
                                dark,
                                logical_width * numerator / denominator,
                                logical_height * numerator / denominator,
                            )
                            .expect("render generated fixture frame");
                        write_frame(format!("{name}-{theme}-{dpi}.bgra"), frame.pixels);
                    }
                    let control = rasterizer
                        .render_control(
                            dark,
                            44 * numerator / denominator,
                            44 * numerator / denominator,
                        )
                        .expect("render generated control fixture");
                    write_frame(format!("cancel-{theme}-{dpi}.bgra"), control.pixels);
                }
            }

            for (name, state, width, height) in edge_golden_states() {
                for (dark, theme) in [(false, "light"), (true, "dark")] {
                    let frame = rasterizer
                        .render_display(&state, dark, width, height)
                        .expect("render generated edge-state fixture");
                    write_frame(format!("{name}-{theme}-96.bgra"), frame.pixels);
                }
            }
        });

        generated.sort_unstable_by(|left, right| left.0.cmp(&right.0));
        let mut sums = String::new();
        for (name, digest) in generated {
            writeln!(&mut sums, "{digest}  {name}").expect("format fixture checksum");
        }
        std::fs::write(output.join("SHA256SUMS"), sums).expect("write generated fixture checksums");
    }

    #[test]
    fn gdi_plus_output_is_premultiplied_bgra() {
        let frame = with_rasterizer(|rasterizer| {
            rasterizer
                .render_display(&state(OverlayMode::Live), false, 750, 78)
                .unwrap()
        });
        for pixel in frame.pixels.chunks_exact(4) {
            let alpha = pixel[3];
            assert!(pixel[0] <= alpha, "blue channel exceeded alpha: {pixel:?}");
            assert!(pixel[1] <= alpha, "green channel exceeded alpha: {pixel:?}");
            assert!(pixel[2] <= alpha, "red channel exceeded alpha: {pixel:?}");
        }
    }

    #[test]
    fn transcript_head_is_grapheme_safe_and_keeps_the_committed_prefix_visible() {
        let colors = NativeColors::for_theme(true);
        let state = OverlayViewState {
            transcript: super::super::super::controller::OverlayTranscript {
                committed: "prefix 👨‍👩‍👧‍👦 café".to_owned(),
                tentative: " ending 🧑🏽‍💻".to_owned(),
                ..Default::default()
            },
            phase: OverlayPhase::Listening,
            ..OverlayViewState::default()
        };
        let line = live_line(&state, colors);
        let head = line.head(9);
        assert!(head.text().starts_with("prefix 👨‍👩‍👧‍👦"));
        assert!(head.text().ends_with('…'));
        assert_eq!(head.sections.last().unwrap().style, TextStyle::Regular);
        assert_eq!(head.sections.last().unwrap().color, colors.muted_text);
    }

    #[test]
    fn transcript_composition_preserves_punctuation_binding() {
        let colors = NativeColors::for_theme(true);
        let state = OverlayViewState {
            transcript: super::super::super::controller::OverlayTranscript {
                committed: "Hello".to_owned(),
                tentative: ", world".to_owned(),
                ..Default::default()
            },
            ..OverlayViewState::default()
        };
        assert_eq!(live_line(&state, colors).text(), "Hello, world");
    }

    #[test]
    fn live_brand_mark_is_static_across_audio_levels() {
        let mut quiet = state(OverlayMode::Live);
        quiet.audio_level = OverlayAudioLevel::new(0.0, 0.0);
        let mut loud = quiet.clone();
        loud.audio_level = OverlayAudioLevel::new(1.0, 1.0);
        with_rasterizer(|rasterizer| {
            assert_eq!(
                rasterizer.render_display(&quiet, true, 600, 62).unwrap(),
                rasterizer.render_display(&loud, true, 600, 62).unwrap()
            );
        });
    }

    #[test]
    fn invalid_frame_sizes_fail_closed() {
        with_rasterizer(|rasterizer| {
            assert!(matches!(
                rasterizer.render_control(true, 0, 44),
                Err(RasterError::InvalidDimensions)
            ));
        });
    }

    #[test]
    fn extreme_frame_dimensions_fail_before_allocation() {
        assert!(matches!(
            LayeredFrame::transparent(i32::MAX, i32::MAX),
            Err(RasterError::InvalidDimensions)
        ));
    }

    #[test]
    fn owned_resource_is_dropped_when_configuration_fails() {
        struct DropProbe<'a>(&'a Cell<usize>);

        impl Drop for DropProbe<'_> {
            fn drop(&mut self) {
                self.0.set(self.0.get() + 1);
            }
        }

        let drops = Cell::new(0);
        let result = configure_owned_resource(DropProbe(&drops), |_| Err::<(), _>("injected"));
        assert_eq!(result.err(), Some("injected"));
        assert_eq!(drops.get(), 1);
    }

    #[test]
    fn native_waveform_colors_follow_the_shared_theme_contract() {
        assert_eq!(
            NativeColors::for_theme(false).waveform,
            Argb::from_color(ThemePalette::light().recording_waveform)
        );
        assert_eq!(
            NativeColors::for_theme(true).waveform,
            Argb::new(255, 178, 162, 255)
        );
    }
}
