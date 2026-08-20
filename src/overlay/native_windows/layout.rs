use super::super::{
    controller::OverlayMode,
    platform::OverlayWindowBounds,
    view::{LIVE_HEIGHT, LIVE_WIDTH, MINIMAL_HEIGHT, MINIMAL_WIDTH},
};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct PhysicalRect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl PhysicalRect {
    const fn new(x0: f32, y0: f32, x1: f32, y1: f32) -> Self {
        Self { x0, y0, x1, y1 }
    }

    pub fn width(self) -> f32 {
        self.x1 - self.x0
    }

    pub fn height(self) -> f32 {
        self.y1 - self.y0
    }

    pub fn center_x(self) -> f32 {
        (self.x0 + self.x1) / 2.0
    }

    pub fn center_y(self) -> f32 {
        (self.y0 + self.y1) / 2.0
    }

    #[cfg(test)]
    pub fn translated(self, x: i32, y: i32) -> Self {
        let x = x as f32;
        let y = y as f32;
        Self::new(self.x0 + x, self.y0 + y, self.x1 + x, self.y1 + y)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct DisplayLayout {
    pub scale: f32,
    pub root: PhysicalRect,
    /// The vertical center shared by every visible content element.  Keeping
    /// this in physical coordinates prevents a logical-pixel rounding drift
    /// from separating the raster and UI Automation geometry at higher DPI.
    pub content_center_y: f32,
    pub status: PhysicalRect,
    pub recording_mark: PhysicalRect,
    pub status_text: Option<PhysicalRect>,
    pub meter: PhysicalRect,
    pub elapsed: PhysicalRect,
    /// Allocation including the antialiased stroke footprint.
    pub divider: Option<PhysicalRect>,
    /// The GDI+ centerline submitted to `GdipDrawLine`.
    pub divider_line: Option<PhysicalRect>,
    pub preview: Option<PhysicalRect>,
}

impl DisplayLayout {
    pub fn from_bounds(mode: OverlayMode, bounds: OverlayWindowBounds) -> Option<Self> {
        if bounds.width <= 0 || bounds.height <= 0 {
            return None;
        }
        let (logical_width, logical_height) = match mode {
            OverlayMode::Live => (LIVE_WIDTH, LIVE_HEIGHT),
            OverlayMode::Minimal | OverlayMode::Off => (MINIMAL_WIDTH, MINIMAL_HEIGHT),
        };
        let scale =
            (bounds.width as f32 / logical_width).min(bounds.height as f32 / logical_height);
        if !scale.is_finite() || scale <= 0.0 {
            return None;
        }
        let root = PhysicalRect::new(0.0, 0.0, bounds.width as f32, bounds.height as f32);
        Some(match mode {
            OverlayMode::Live => {
                // The window can round one axis differently than the other
                // at fractional DPI.  The physical root is the canonical
                // capsule viewport, so its center—not a rederived logical
                // height—is authoritative for painting and UIA.
                let content_center_y = root.center_y();
                let recording_mark = centered_rect(16.0, 30.0, 30.0, content_center_y, scale);
                let divider_line = centered_rect(110.5, 1.0, 24.0, content_center_y, scale);
                let divider_stroke_radius = scale.max(1.0) / 2.0;
                let divider = PhysicalRect::new(
                    divider_line.x0 - divider_stroke_radius,
                    divider_line.y0 - divider_stroke_radius,
                    divider_line.x1 + divider_stroke_radius,
                    divider_line.y1 + divider_stroke_radius,
                );
                Self {
                    scale,
                    root,
                    content_center_y,
                    status: recording_mark,
                    recording_mark,
                    status_text: None,
                    meter: recording_mark,
                    elapsed: centered_rect(56.0, 48.0, 23.0, content_center_y, scale),
                    divider: Some(divider),
                    divider_line: Some(divider_line),
                    preview: Some(centered_rect(123.0, 426.0, 23.0, content_center_y, scale)),
                }
            }
            OverlayMode::Minimal | OverlayMode::Off => {
                let content_center_y = root.center_y();
                Self {
                    scale,
                    root,
                    content_center_y,
                    status: centered_rect(20.0, 140.0, 22.0, content_center_y, scale),
                    recording_mark: centered_rect(20.0, 8.0, 8.0, content_center_y, scale),
                    status_text: Some(centered_rect(34.0, 126.0, 22.0, content_center_y, scale)),
                    meter: centered_rect(164.0, 34.0, 20.0, content_center_y, scale),
                    elapsed: centered_rect(207.0, 53.0, 21.0, content_center_y, scale),
                    divider: None,
                    divider_line: None,
                    preview: None,
                }
            }
        })
    }
}

/// Builds a physical rectangle from a logical horizontal span and one shared
/// physical centerline.  The vertical dimensions are scaled after the
/// centerline is chosen, so every component remains centered at fractional
/// Windows DPI scales as well.
fn centered_rect(
    logical_x: f32,
    logical_width: f32,
    logical_height: f32,
    physical_center_y: f32,
    scale: f32,
) -> PhysicalRect {
    let physical_height = logical_height * scale;
    PhysicalRect::new(
        logical_x * scale,
        physical_center_y - physical_height / 2.0,
        (logical_x + logical_width) * scale,
        physical_center_y + physical_height / 2.0,
    )
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct ControlLayout {
    pub root: PhysicalRect,
    pub button: PhysicalRect,
}

impl ControlLayout {
    pub fn from_bounds(bounds: OverlayWindowBounds) -> Option<Self> {
        if bounds.width <= 0 || bounds.height <= 0 {
            return None;
        }
        let root = PhysicalRect::new(0.0, 0.0, bounds.width as f32, bounds.height as f32);
        Some(Self { root, button: root })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlay::{
        platform::{OverlayPosition, PhysicalWorkArea, calculate_window_bounds},
        view::{CONTROL_SIZE, window_spec},
    };

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

    #[test]
    fn native_sizes_share_the_public_overlay_contract() {
        let live = window_spec(OverlayMode::Live);
        let compact = window_spec(OverlayMode::Minimal);
        assert_eq!(
            (live.width_points, live.height_points),
            (LIVE_WIDTH, LIVE_HEIGHT)
        );
        assert_eq!(
            (compact.width_points, compact.height_points),
            (MINIMAL_WIDTH, MINIMAL_HEIGHT)
        );
        assert_eq!(CONTROL_SIZE, 44.0);
    }

    #[test]
    fn live_layout_uses_actual_physical_bounds_and_desktop_origin() {
        let bounds = OverlayWindowBounds {
            x: -1120,
            y: 1119,
            width: 750,
            height: 78,
        };
        let layout = DisplayLayout::from_bounds(OverlayMode::Live, bounds).unwrap();
        assert_eq!(layout.scale, 1.25);
        assert_eq!(layout.root, PhysicalRect::new(0.0, 0.0, 750.0, 78.0));
        assert_eq!(layout.content_center_y, 39.0);
        assert_eq!(
            layout.elapsed.translated(bounds.x, bounds.y),
            PhysicalRect::new(-1050.0, 1143.625, -990.0, 1172.375)
        );
        assert_eq!(
            layout.preview.unwrap().translated(bounds.x, bounds.y),
            PhysicalRect::new(-966.25, 1143.625, -433.75, 1172.375)
        );
    }

    #[test]
    fn production_rounded_bounds_keep_every_content_rect_on_the_physical_centerline() {
        for (mode, expected_sizes) in [
            (
                OverlayMode::Live,
                [(600, 62), (750, 78), (900, 93), (1_200, 124)],
            ),
            (
                OverlayMode::Minimal,
                [(320, 52), (400, 65), (480, 78), (640, 104)],
            ),
        ] {
            for (dpi, expected_size) in [96, 120, 144, 192].into_iter().zip(expected_sizes) {
                let bounds = production_bounds(mode, dpi);
                assert_eq!((bounds.width, bounds.height), expected_size);
                let layout = DisplayLayout::from_bounds(mode, bounds).unwrap();
                let expected_center = layout.root.center_y();
                assert_eq!(layout.content_center_y, expected_center);
                let elements = [
                    Some(layout.status),
                    Some(layout.recording_mark),
                    layout.status_text,
                    Some(layout.meter),
                    Some(layout.elapsed),
                    layout.divider,
                    layout.divider_line,
                    layout.preview,
                ];
                for rect in elements.into_iter().flatten() {
                    assert!(
                        (rect.center_y() - expected_center).abs() <= 0.5,
                        "{mode:?} at {dpi} DPI: {:?} drifted from {expected_center}",
                        rect
                    );
                }
            }
        }
    }

    #[test]
    fn control_layout_scales_the_logical_target_to_current_dpi() {
        let bounds_96 = OverlayWindowBounds {
            x: -852,
            y: 1123,
            width: 44,
            height: 44,
        };
        let bounds_120 = OverlayWindowBounds {
            x: 1590,
            y: 1283,
            width: 55,
            height: 55,
        };
        let layout_96 = ControlLayout::from_bounds(bounds_96).unwrap();
        let layout_120 = ControlLayout::from_bounds(bounds_120).unwrap();
        assert_eq!(layout_96.button.width(), CONTROL_SIZE);
        assert_eq!(layout_96.button.height(), CONTROL_SIZE);
        assert_eq!(layout_120.button.width(), 55.0);
        assert_eq!(layout_120.button.height(), 55.0);
        assert_eq!(
            layout_120.button.translated(bounds_120.x, bounds_120.y),
            PhysicalRect::new(1590.0, 1283.0, 1645.0, 1338.0)
        );
    }
}
