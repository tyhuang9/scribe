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

    fn scaled(self, scale: f32) -> Self {
        Self::new(
            self.x0 * scale,
            self.y0 * scale,
            self.x1 * scale,
            self.y1 * scale,
        )
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
    pub status: PhysicalRect,
    pub recording_mark: PhysicalRect,
    pub status_text: Option<PhysicalRect>,
    pub meter: PhysicalRect,
    pub elapsed: PhysicalRect,
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
                let recording_mark = PhysicalRect::new(16.0, 15.0, 46.0, 45.0).scaled(scale);
                Self {
                    scale,
                    root,
                    status: recording_mark,
                    recording_mark,
                    status_text: None,
                    meter: recording_mark,
                    elapsed: PhysicalRect::new(56.0, 20.5, 104.0, 43.5).scaled(scale),
                    preview: Some(PhysicalRect::new(123.0, 20.5, 549.0, 43.5).scaled(scale)),
                }
            }
            OverlayMode::Minimal | OverlayMode::Off => Self {
                scale,
                root,
                status: PhysicalRect::new(20.0, 16.0, 160.0, 38.0).scaled(scale),
                recording_mark: PhysicalRect::new(20.0, 22.0, 28.0, 30.0).scaled(scale),
                status_text: Some(PhysicalRect::new(34.0, 16.0, 160.0, 38.0).scaled(scale)),
                meter: PhysicalRect::new(164.0, 16.0, 198.0, 36.0).scaled(scale),
                elapsed: PhysicalRect::new(207.0, 16.5, 260.0, 37.5).scaled(scale),
                preview: None,
            },
        })
    }
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
    use crate::overlay::view::{CONTROL_SIZE, window_spec};

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
        assert_eq!(
            layout.elapsed.translated(bounds.x, bounds.y),
            PhysicalRect::new(-1050.0, 1144.625, -990.0, 1173.375)
        );
        assert_eq!(
            layout.preview.unwrap().translated(bounds.x, bounds.y),
            PhysicalRect::new(-966.25, 1144.625, -433.75, 1173.375)
        );
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
