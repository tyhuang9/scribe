use std::{borrow::Cow, ffi::c_void, mem::zeroed, ptr::null_mut, time::Duration};

use eframe::egui::Color32;
use unicode_segmentation::UnicodeSegmentation;
use windows_sys::Win32::Graphics::GdiPlus::{
    FillModeAlternate, FontStyleBold, FontStyleItalic, FontStyleRegular, GdipAddPathArc,
    GdipCloneFontFamily, GdipClosePathFigure, GdipCreateBitmapFromScan0, GdipCreateFont,
    GdipCreateFontFamilyFromName, GdipCreatePath, GdipCreatePen1, GdipCreateSolidFill,
    GdipDeleteBrush, GdipDeleteFont, GdipDeleteFontFamily, GdipDeleteGraphics, GdipDeletePath,
    GdipDeletePen, GdipDeletePrivateFontCollection, GdipDeleteStringFormat, GdipDisposeImage,
    GdipDrawLine, GdipDrawPath, GdipDrawString, GdipFillEllipse, GdipFillPath,
    GdipGetFontCollectionFamilyCount, GdipGetFontCollectionFamilyList,
    GdipGetGenericFontFamilySansSerif, GdipGetImageGraphicsContext, GdipGraphicsClear,
    GdipMeasureString, GdipNewPrivateFontCollection, GdipPrivateAddMemoryFont,
    GdipSetSmoothingMode, GdipSetStringFormatFlags, GdipSetTextRenderingHint,
    GdipStringFormatGetGenericTypographic, GdiplusShutdown, GdiplusStartup, GdiplusStartupInput,
    GpBitmap, GpBrush, GpFont, GpFontCollection, GpFontFamily, GpGraphics, GpImage, GpPath, GpPen,
    GpSolidFill, GpStringFormat, Ok as GDI_PLUS_OK, RectF, SmoothingModeAntiAlias8x8,
    StringFormatFlagsMeasureTrailingSpaces, StringFormatFlagsNoWrap,
    TextRenderingHintAntiAliasGridFit, UnitPixel,
};

use super::super::controller::{OverlayMode, OverlayPhase, OverlayRecovery, OverlayViewState};
use crate::ui::ThemePalette;

const PIXEL_FORMAT_32BPP_PARGB: i32 = 0x000E_200B;
const MAX_PREVIEW_GRAPHEMES: usize = 512;
const MAX_MESSAGE_GRAPHEMES: usize = 256;

const LIVE_WIDTH: f32 = 600.0;
const LIVE_HEIGHT: f32 = 62.0;
const COMPACT_WIDTH: f32 = 320.0;
const COMPACT_HEIGHT: f32 = 52.0;
const CONTROL_SIZE: f32 = 44.0;

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
    tentative_text: Argb,
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
                surface: Argb::new(218, 25, 31, 42),
                border: Argb::new(76, 220, 229, 242),
                inner_highlight: Argb::new(42, 255, 255, 255),
                text: Argb::from_color(palette.text),
                muted_text: Argb::new(255, 202, 211, 224),
                tentative_text: Argb::new(255, 166, 180, 202),
                waveform: Argb::from_color(palette.recording_waveform),
                meter_active: Argb::from_color(palette.success),
                meter_inactive: Argb::new(255, 128, 142, 162),
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
                tentative_text: Argb::new(255, 72, 84, 102),
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
            .and_then(|width| usize::try_from(height).ok().map(|height| width * height))
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or(RasterError::InvalidDimensions)?;
        Ok(Self {
            width,
            height,
            pixels: vec![0; length],
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
    Italic,
    Monospace,
    Phosphor,
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

    fn tail(&self, keep: usize) -> Self {
        let total = self.grapheme_count();
        slice_styled_line(self, total.saturating_sub(keep), total, false)
    }
}

pub(super) struct NativeRasterizer {
    // Fields drop in declaration order: release the private family before GDI+ shutdown.
    phosphor: PrivateFont,
    _gdiplus: GdiPlusSession,
}

impl NativeRasterizer {
    pub(super) fn new() -> Result<Self, RasterError> {
        let gdiplus = GdiPlusSession::start()?;
        let phosphor = PrivateFont::phosphor_regular()?;
        Ok(Self {
            phosphor,
            _gdiplus: gdiplus,
        })
    }

    pub(super) fn render_display(
        &self,
        state: &OverlayViewState,
        dark_mode: bool,
        width: i32,
        height: i32,
    ) -> Result<LayeredFrame, RasterError> {
        let mut frame = LayeredFrame::transparent(width, height)?;
        let logical_size = match state.mode {
            OverlayMode::Live => (LIVE_WIDTH, LIVE_HEIGHT),
            OverlayMode::Minimal | OverlayMode::Off => (COMPACT_WIDTH, COMPACT_HEIGHT),
        };
        let scale = (width as f32 / logical_size.0)
            .min(height as f32 / logical_size.1)
            .max(0.1);
        let mut canvas = Canvas::new(self, &mut frame.pixels, width, height)?;
        let colors = NativeColors::for_theme(dark_mode);
        draw_capsule(&mut canvas, state.mode, scale, colors)?;
        match state.mode {
            OverlayMode::Live => draw_live(&mut canvas, state, scale, colors)?,
            OverlayMode::Minimal | OverlayMode::Off => {
                draw_compact(&mut canvas, state, scale, colors)?
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
        OverlayMode::Minimal | OverlayMode::Off => (COMPACT_WIDTH, COMPACT_HEIGHT, 4.0, 2.0, 1.0),
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
        y + 1.5 * scale,
        x + width - radius * 0.45,
        y + 1.5 * scale,
        scale.max(1.0),
        colors.inner_highlight,
    )
}

fn draw_live(
    canvas: &mut Canvas<'_>,
    state: &OverlayViewState,
    scale: f32,
    colors: NativeColors,
) -> Result<(), RasterError> {
    let center_y = LIVE_HEIGHT * scale / 2.0;
    let level = normalized_level(state);
    let visual_level = if state.reduced_motion { 0.55 } else { level };
    let center_x = 31.0 * scale;
    let halo_radius = (2.5 + visual_level * 3.5) * scale;
    let waveform_alpha = (16.0 + visual_level * 48.0).round() as u8;
    let waveform_rgb = colors.waveform.0;
    canvas.fill_ellipse(
        center_x - halo_radius,
        center_y - halo_radius,
        halo_radius * 2.0,
        halo_radius * 2.0,
        Argb((waveform_rgb & 0x00FF_FFFF) | ((waveform_alpha as u32) << 24)),
    )?;
    canvas.draw_centered_text(
        egui_phosphor::regular::WAVEFORM,
        center_x,
        (center_y - 15.0 * scale) - 1.0 * scale,
        30.0 * scale,
        30.0 * scale,
        27.0 * scale,
        TextStyle::Phosphor,
        colors.waveform,
    )?;

    let elapsed = state
        .elapsed
        .map(format_elapsed)
        .unwrap_or_else(|| "00:00".to_owned());
    canvas.draw_text(
        &elapsed,
        56.0 * scale,
        20.5 * scale,
        48.0 * scale,
        23.0 * scale,
        13.0 * scale,
        TextStyle::Monospace,
        colors.muted_text,
    )?;
    canvas.draw_line(
        111.0 * scale,
        19.0 * scale,
        111.0 * scale,
        43.0 * scale,
        scale.max(1.0),
        colors.border,
    )?;

    let max_width = 426.0 * scale;
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
        fit_tail(
            canvas,
            &line,
            max_width,
            13.0 * scale,
            MAX_PREVIEW_GRAPHEMES,
        )?
    };
    canvas.draw_styled_line(
        &line,
        123.0 * scale,
        20.5 * scale,
        max_width,
        23.0 * scale,
        13.0 * scale,
    )?;
    Ok(())
}

fn draw_compact(
    canvas: &mut Canvas<'_>,
    state: &OverlayViewState,
    scale: f32,
    colors: NativeColors,
) -> Result<(), RasterError> {
    let center_y = COMPACT_HEIGHT * scale / 2.0;
    let phase = phase_color(state.phase);
    canvas.fill_ellipse(
        20.0 * scale,
        center_y - 4.0 * scale,
        8.0 * scale,
        8.0 * scale,
        phase,
    )?;
    let label = if state.phase == OverlayPhase::Listening {
        "Scribe is recording"
    } else {
        state.phase.label()
    };
    canvas.draw_text(
        label,
        34.0 * scale,
        16.0 * scale,
        126.0 * scale,
        22.0 * scale,
        13.0 * scale,
        TextStyle::Bold,
        colors.text,
    )?;

    let level = normalized_level(state);
    for index in 0..4 {
        let threshold = (index + 1) as f32 / 4.0;
        let active = level >= threshold * 0.78;
        let normalized_height = if active { threshold } else { 0.22 };
        let height = (20.0 * normalized_height).max(4.0) * scale;
        let x = (164.0 + index as f32 * 9.0) * scale;
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
    if let Some(elapsed) = state.elapsed {
        canvas.draw_text(
            &format_elapsed(elapsed),
            207.0 * scale,
            16.5 * scale,
            53.0 * scale,
            21.0 * scale,
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
            color: colors.text,
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
            color: colors.text,
            style: TextStyle::Regular,
        });
    }
    if !tentative.is_empty() {
        sections.push(StyledSection {
            text: tentative.clone(),
            color: colors.tentative_text,
            style: TextStyle::Italic,
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

fn fit_tail(
    canvas: &mut Canvas<'_>,
    line: &StyledLine,
    max_width: f32,
    font_size: f32,
    limit: usize,
) -> Result<StyledLine, RasterError> {
    let total = line.grapheme_count().min(limit);
    binary_search_fit(total, |keep| line.tail(keep), canvas, max_width, font_size)
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

fn format_elapsed(elapsed: Duration) -> String {
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
        Ok(measured.Width.max(0.0))
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
        y: f32,
        width: f32,
        height: f32,
        font_size: f32,
    ) -> Result<(), RasterError> {
        let mut cursor = x;
        let right = x + width;
        for section in &line.sections {
            if cursor >= right {
                break;
            }
            let measured = self.draw_text(
                &section.text,
                cursor,
                y,
                right - cursor,
                height,
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
            TextStyle::Regular | TextStyle::Bold | TextStyle::Italic => "Segoe UI",
            TextStyle::Phosphor => unreachable!("Phosphor is handled above"),
        };
        let font_style = match style {
            TextStyle::Bold => FontStyleBold,
            TextStyle::Italic => FontStyleItalic,
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

impl StringFormat {
    fn new() -> Result<Self, RasterError> {
        let mut format = null_mut();
        status(
            unsafe { GdipStringFormatGetGenericTypographic(&mut format) },
            "create string format",
        )?;
        status(
            unsafe {
                GdipSetStringFormatFlags(
                    format,
                    StringFormatFlagsNoWrap | StringFormatFlagsMeasureTrailingSpaces,
                )
            },
            "configure string format",
        )?;
        Ok(Self(format))
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
    use std::sync::Mutex;

    use super::*;
    use crate::{overlay::controller::OverlayAudioLevel, transcription::SessionId};

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
            audio_level: OverlayAudioLevel::new(0.65, 0.82),
            transcript: super::super::super::controller::OverlayTranscript {
                committed: "The native overlay keeps the latest committed phrase".to_owned(),
                tentative: " and this tentative ending".to_owned(),
                revision: 7,
            },
            elapsed: Some(Duration::from_secs(12)),
            ..OverlayViewState::default()
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
    fn transcript_tail_is_grapheme_safe_and_keeps_tentative_style() {
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
        let tail = line.tail(4);
        assert!(tail.text().starts_with('…'));
        assert!(tail.text().ends_with("🧑🏽‍💻"));
        assert_eq!(tail.sections.last().unwrap().style, TextStyle::Italic);
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
    fn reduced_motion_freezes_waveform_level_without_changing_audio_state() {
        let mut quiet = state(OverlayMode::Live);
        quiet.reduced_motion = true;
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
}
