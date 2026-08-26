use eframe::egui::{self, Color32, Pos2, Rect, Response, Rounding, Sense, Stroke, Vec2};

pub(crate) const WORDMARK: &str = "scribe";
pub(crate) const TAGLINE: &str = "Lightning-fast local transcription that stays out of your way.";

// Canonical raw identity colors. Semantic UI palettes may derive additional
// contrast-safe colors, but these values must stay byte-for-byte aligned with
// the Scribe identity board.
pub(crate) const DEEP_INK: Color32 = Color32::from_rgb(0x08, 0x23, 0x3A);
pub(crate) const SCRIBE_TEAL: Color32 = Color32::from_rgb(0x2D, 0x97, 0x9C);
pub(crate) const SOFT_AQUA: Color32 = Color32::from_rgb(0xAC, 0xDB, 0xD9);
pub(crate) const ICE_MIST: Color32 = Color32::from_rgb(0xEA, 0xF5, 0xF5);
pub(crate) const WARM_SAND: Color32 = Color32::from_rgb(0xE9, 0xD1, 0xB1);
pub(crate) const LIVE_CORAL: Color32 = Color32::from_rgb(0xFD, 0x81, 0x6F);
pub(crate) const DEEP_NAVY: Color32 = Color32::from_rgb(0x06, 0x1C, 0x2E);
pub(crate) const NAVY_SURFACE: Color32 = DEEP_INK;
pub(crate) const TEAL_ACCENT: Color32 = Color32::from_rgb(0x7C, 0xCB, 0xC9);

const BAR_X: [f32; 7] = [
    20.5 / 128.0,
    35.5 / 128.0,
    49.5 / 128.0,
    64.5 / 128.0,
    78.5 / 128.0,
    92.5 / 128.0,
    107.5 / 128.0,
];
const BAR_HEIGHT: [f32; 7] = [
    48.0 / 128.0,
    80.0 / 128.0,
    100.0 / 128.0,
    120.0 / 128.0,
    100.0 / 128.0,
    80.0 / 128.0,
    48.0 / 128.0,
];
const BAR_WIDTH: f32 = 11.0 / 128.0;
const S_STROKE_WIDTH: f32 = 16.6 / 128.0;
const S_CURVES: [[(f32, f32); 4]; 3] = [
    [(0.70, 0.28), (0.61, 0.16), (0.35, 0.18), (0.30, 0.37)],
    [(0.30, 0.37), (0.26, 0.54), (0.72, 0.47), (0.68, 0.66)],
    [(0.68, 0.66), (0.65, 0.84), (0.37, 0.84), (0.28, 0.70)],
];

pub(crate) fn show_mark(ui: &mut egui::Ui, size: f32, announce: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    paint_mark(ui.painter(), rect, ui.visuals().dark_mode);
    if announce {
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, "Scribe logo"));
        ui.ctx().accesskit_node_builder(response.id, |builder| {
            builder.set_role(egui::accesskit::Role::Image);
            builder.set_name("Scribe logo");
            builder.set_description(TAGLINE);
        });
    }
    response
}

pub(crate) fn paint_mark(painter: &egui::Painter, rect: Rect, dark_mode: bool) {
    let size = rect.width().min(rect.height());
    let rect = Rect::from_center_size(rect.center(), Vec2::splat(size));
    let bar_primary = if dark_mode { TEAL_ACCENT } else { SCRIBE_TEAL };
    let bar_secondary = SOFT_AQUA;

    for (index, (&x, &height)) in BAR_X.iter().zip(&BAR_HEIGHT).enumerate() {
        let bar_width = size * BAR_WIDTH;
        let bar_height = size * height;
        let center = Pos2::new(rect.left() + size * x, rect.center().y);
        let bar = Rect::from_center_size(center, Vec2::new(bar_width, bar_height));
        let fill = if matches!(index, 1 | 5) {
            bar_secondary
        } else {
            bar_primary
        };
        painter.rect_filled(bar, Rounding::same(bar_width / 2.0), fill);
    }

    let points = s_curve_points(rect, 10);
    let stroke_width = size * S_STROKE_WIDTH;
    painter.add(egui::Shape::line(
        points.clone(),
        Stroke::new(stroke_width, Color32::WHITE),
    ));
    if let (Some(first), Some(last)) = (points.first(), points.last()) {
        painter.circle_filled(*first, stroke_width / 2.0, Color32::WHITE);
        painter.circle_filled(*last, stroke_width / 2.0, Color32::WHITE);
    }
}

/// Generates the shared window/tray icon without image-decoding dependencies.
/// The icon is supersampled so small system-tray sizes keep smooth capsule and
/// letter edges.
pub(crate) fn app_icon_rgba(size: u32) -> Vec<u8> {
    const SAMPLES: u32 = 4;
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let mut channels = [0_u32; 4];
            for sample_y in 0..SAMPLES {
                for sample_x in 0..SAMPLES {
                    let px = (x as f32 + (sample_x as f32 + 0.5) / SAMPLES as f32) / size as f32;
                    let py = (y as f32 + (sample_y as f32 + 0.5) / SAMPLES as f32) / size as f32;
                    let sample = icon_sample(px, py);
                    for (total, value) in channels.iter_mut().zip(sample) {
                        *total += u32::from(value);
                    }
                }
            }
            let divisor = SAMPLES * SAMPLES;
            rgba.extend(channels.map(|value| (value / divisor) as u8));
        }
    }
    rgba
}

fn icon_sample(x: f32, y: f32) -> [u8; 4] {
    if !inside_rounded_rect(x, y, 0.04, 0.04, 0.96, 0.96, 0.19) {
        return [0, 0, 0, 0];
    }

    let mut color = DEEP_NAVY.to_array();
    for (index, (&center_x, &height)) in BAR_X.iter().zip(&BAR_HEIGHT).enumerate() {
        let half_width = BAR_WIDTH / 2.0;
        let half_height = height / 2.0;
        if inside_rounded_rect(
            x,
            y,
            center_x - half_width,
            0.5 - half_height,
            center_x + half_width,
            0.5 + half_height,
            half_width,
        ) {
            color = if matches!(index, 1 | 5) {
                SOFT_AQUA.to_array()
            } else {
                TEAL_ACCENT.to_array()
            };
        }
    }

    if point_near_s_curve(x, y, 0.065) {
        Color32::WHITE.to_array()
    } else {
        color
    }
}

fn inside_rounded_rect(
    x: f32,
    y: f32,
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
    radius: f32,
) -> bool {
    let nearest_x = x.clamp(left + radius, right - radius);
    let nearest_y = y.clamp(top + radius, bottom - radius);
    let dx = x - nearest_x;
    let dy = y - nearest_y;
    x >= left && x <= right && y >= top && y <= bottom && dx * dx + dy * dy <= radius * radius
}

fn point_near_s_curve(x: f32, y: f32, radius: f32) -> bool {
    let target = Pos2::new(x, y);
    let rect = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
    let points = s_curve_points(rect, 16);
    points
        .windows(2)
        .any(|segment| distance_to_segment(target, segment[0], segment[1]) <= radius)
}

fn distance_to_segment(point: Pos2, start: Pos2, end: Pos2) -> f32 {
    let segment = end - start;
    let length_squared = segment.length_sq();
    if length_squared <= f32::EPSILON {
        return point.distance(start);
    }
    let t = ((point - start).dot(segment) / length_squared).clamp(0.0, 1.0);
    point.distance(start + segment * t)
}

fn s_curve_points(rect: Rect, segments_per_curve: usize) -> Vec<Pos2> {
    let mut points = Vec::with_capacity(segments_per_curve * S_CURVES.len() + 1);
    for (curve_index, curve) in S_CURVES.into_iter().enumerate() {
        for step in 0..=segments_per_curve {
            if curve_index > 0 && step == 0 {
                continue;
            }
            let t = step as f32 / segments_per_curve as f32;
            let one_minus_t = 1.0 - t;
            let coefficients = [
                one_minus_t.powi(3),
                3.0 * one_minus_t.powi(2) * t,
                3.0 * one_minus_t * t.powi(2),
                t.powi(3),
            ];
            let x = curve
                .iter()
                .zip(coefficients)
                .map(|((x, _), coefficient)| x * coefficient)
                .sum::<f32>();
            let y = curve
                .iter()
                .zip(coefficients)
                .map(|((_, y), coefficient)| y * coefficient)
                .sum::<f32>();
            points.push(Pos2::new(
                rect.left() + rect.width() * x,
                rect.top() + rect.height() * y,
            ));
        }
    }
    points
}

#[cfg(test)]
mod tests {
    use super::*;

    const MARK_SVG: &str = include_str!("../assets/branding/scribe-mark.svg");
    const LIGHT_LOCKUP_SVG: &str = include_str!("../assets/branding/scribe-lockup-light.svg");
    const DARK_LOCKUP_SVG: &str = include_str!("../assets/branding/scribe-lockup-dark.svg");

    fn attribute<'a>(tag: &'a str, name: &str) -> &'a str {
        let prefix = format!("{name}=\"");
        let start = tag.find(&prefix).expect("SVG attribute") + prefix.len();
        let end = tag[start..].find('"').expect("closing quote") + start;
        &tag[start..end]
    }

    fn mark_rects(svg: &str) -> Vec<(f32, f32, f32, f32)> {
        svg.match_indices("<rect")
            .filter_map(|(start, _)| {
                let end = svg[start..].find("/>")? + start + 2;
                let tag = &svg[start..end];
                (attribute(tag, "width") == "11").then(|| {
                    (
                        attribute(tag, "x").parse().unwrap(),
                        attribute(tag, "y").parse().unwrap(),
                        attribute(tag, "width").parse().unwrap(),
                        attribute(tag, "height").parse().unwrap(),
                    )
                })
            })
            .collect()
    }

    fn bar_centers_for_fill(svg: &str, fill: &str) -> Vec<f32> {
        let group_start = svg
            .find(&format!("<g fill=\"{fill}\">"))
            .expect("brand color group");
        let group_end = svg[group_start..].find("</g>").expect("color group close") + group_start;
        let mut centers = mark_rects(&svg[group_start..group_end])
            .into_iter()
            .map(|(x, _, width, _)| (x + width / 2.0) / 128.0)
            .collect::<Vec<_>>();
        centers.sort_by(f32::total_cmp);
        centers
    }

    fn path_data(svg: &str) -> &str {
        let start = svg.find("<path").expect("S path");
        let end = svg[start..].find("/>").expect("path close") + start + 2;
        attribute(&svg[start..end], "d")
    }

    fn path_numbers(path: &str) -> Vec<f32> {
        path.split(|character: char| {
            !(character.is_ascii_digit() || matches!(character, '.' | '-'))
        })
        .filter(|part| !part.is_empty())
        .map(|part| part.parse().expect("numeric SVG path coordinate"))
        .collect()
    }

    fn assert_mark_geometry(svg: &str) {
        let mut rects = mark_rects(svg);
        rects.sort_by(|left, right| left.0.total_cmp(&right.0));
        assert_eq!(rects.len(), BAR_X.len());
        for ((x, y, width, height), (&center_x, &normalized_height)) in
            rects.into_iter().zip(BAR_X.iter().zip(&BAR_HEIGHT))
        {
            assert!(((x + width / 2.0) / 128.0 - center_x).abs() <= f32::EPSILON);
            assert!((height / 128.0 - normalized_height).abs() <= f32::EPSILON);
            assert!((width / 128.0 - BAR_WIDTH).abs() <= f32::EPSILON);
            assert!(((y + height / 2.0) / 128.0 - 0.5).abs() <= f32::EPSILON);
        }

        let actual = path_numbers(path_data(svg));
        let expected = S_CURVES
            .iter()
            .enumerate()
            .flat_map(|(curve_index, curve)| curve.iter().skip(usize::from(curve_index > 0)))
            .flat_map(|(x, y)| [x * 128.0, y * 128.0])
            .collect::<Vec<_>>();
        assert_eq!(actual.len(), expected.len());
        for (actual, expected) in actual.into_iter().zip(expected) {
            assert!((actual - expected).abs() <= 0.06);
        }
        let path_start = svg.find("<path").unwrap();
        let path_end = svg[path_start..].find("/>").unwrap() + path_start + 2;
        let path = &svg[path_start..path_end];
        let stroke_width: f32 = attribute(path, "stroke-width").parse().unwrap();
        assert!((stroke_width / 128.0 - S_STROKE_WIDTH).abs() <= 0.001);
        assert_eq!(attribute(path, "stroke"), "#fff");
    }

    #[test]
    fn raw_palette_matches_the_identity_contract() {
        assert_eq!(DEEP_INK.to_array(), [0x08, 0x23, 0x3A, 0xFF]);
        assert_eq!(SCRIBE_TEAL.to_array(), [0x2D, 0x97, 0x9C, 0xFF]);
        assert_eq!(SOFT_AQUA.to_array(), [0xAC, 0xDB, 0xD9, 0xFF]);
        assert_eq!(ICE_MIST.to_array(), [0xEA, 0xF5, 0xF5, 0xFF]);
        assert_eq!(WARM_SAND.to_array(), [0xE9, 0xD1, 0xB1, 0xFF]);
        assert_eq!(LIVE_CORAL.to_array(), [0xFD, 0x81, 0x6F, 0xFF]);
        assert_eq!(DEEP_NAVY.to_array(), [0x06, 0x1C, 0x2E, 0xFF]);
        assert_eq!(NAVY_SURFACE.to_array(), [0x08, 0x23, 0x3A, 0xFF]);
        assert_eq!(TEAL_ACCENT.to_array(), [0x7C, 0xCB, 0xC9, 0xFF]);
    }

    #[test]
    fn generated_icon_has_valid_dimensions_transparency_and_brand_pixels() {
        for size in [16, 32, 128] {
            let rgba = app_icon_rgba(size);
            assert_eq!(rgba.len(), (size * size * 4) as usize);
            assert_eq!(&rgba[0..4], &[0, 0, 0, 0]);
            assert!(
                rgba.chunks_exact(4)
                    .any(|pixel| pixel == DEEP_NAVY.to_array())
            );
            assert!(
                rgba.chunks_exact(4)
                    .any(|pixel| pixel == TEAL_ACCENT.to_array())
            );
            assert!(
                rgba.chunks_exact(4)
                    .any(|pixel| pixel == Color32::WHITE.to_array())
            );
        }
    }

    #[test]
    fn canonical_svg_mark_and_lockups_match_the_native_identity_geometry() {
        for (svg, primary_fill) in [
            (MARK_SVG, "#2D979C"),
            (LIGHT_LOCKUP_SVG, "#2D979C"),
            (DARK_LOCKUP_SVG, "#7CCBC9"),
        ] {
            assert_mark_geometry(svg);
            assert_eq!(
                bar_centers_for_fill(svg, primary_fill),
                [BAR_X[0], BAR_X[2], BAR_X[3], BAR_X[4], BAR_X[6]]
            );
            assert_eq!(bar_centers_for_fill(svg, "#ACDBD9"), [BAR_X[1], BAR_X[5]]);
        }
        assert_eq!(path_data(MARK_SVG), path_data(LIGHT_LOCKUP_SVG));
        assert_eq!(path_data(MARK_SVG), path_data(DARK_LOCKUP_SVG));
        assert!(MARK_SVG.contains("fill=\"#2D979C\""));
        assert!(LIGHT_LOCKUP_SVG.contains("fill=\"#2D979C\""));
        assert!(DARK_LOCKUP_SVG.contains("fill=\"#7CCBC9\""));
    }

    #[test]
    fn canonical_root_lockups_match_the_theme_and_wordmark_contract() {
        assert!(LIGHT_LOCKUP_SVG.contains("fill=\"#08233A\""));
        assert!(DARK_LOCKUP_SVG.contains("fill=\"#061C2E\""));
        assert!(DARK_LOCKUP_SVG.contains("fill=\"#08233A\""));
        assert!(DARK_LOCKUP_SVG.contains("fill=\"#EAF5F5\""));
        for svg in [LIGHT_LOCKUP_SVG, DARK_LOCKUP_SVG] {
            assert!(svg.contains("<title id=\"title\">scribe</title>"));
            assert!(svg.contains(">scribe</text>"));
            assert!(svg.contains(TAGLINE));
        }
    }
}
