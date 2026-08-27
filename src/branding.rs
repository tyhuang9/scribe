use eframe::egui::{self, Color32, Pos2, Rect, Response, Sense, Vec2};
use std::{io::Cursor, sync::OnceLock};

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

const APP_ICON_PNG: &[u8] = include_bytes!("../assets/branding/scribe-app-icon.png");
const APP_ICON_SOURCE_WIDTH: u32 = 128;
const APP_ICON_SOURCE_HEIGHT: u32 = 127;
const APP_ICON_TEXTURE_ID: &str = "scribe-app-icon-tile";

struct DecodedAppIcon {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

static DECODED_APP_ICON: OnceLock<DecodedAppIcon> = OnceLock::new();

#[cfg(test)]
const BAR_X: [f32; 7] = [
    20.5 / 128.0,
    35.5 / 128.0,
    49.5 / 128.0,
    64.5 / 128.0,
    78.5 / 128.0,
    92.5 / 128.0,
    107.5 / 128.0,
];
#[cfg(test)]
const BAR_HEIGHT: [f32; 7] = [
    48.0 / 128.0,
    80.0 / 128.0,
    100.0 / 128.0,
    120.0 / 128.0,
    100.0 / 128.0,
    80.0 / 128.0,
    48.0 / 128.0,
];
#[cfg(test)]
const BAR_WIDTH: f32 = 11.0 / 128.0;
#[cfg(test)]
const S_STROKE_WIDTH: f32 = 16.6 / 128.0;
// Absolute control points from the canonical 128-unit SVG path, normalized for
// the SVG parity tests. The SVG's later cubic segments are relative; these
// values include that translation instead of approximating the curve.
#[cfg(test)]
const S_CURVES: [[(f32, f32); 4]; 3] = [
    [
        (89.6 / 128.0, 35.8 / 128.0),
        (78.1 / 128.0, 20.5 / 128.0),
        (49.9 / 128.0, 23.0 / 128.0),
        (43.0 / 128.0, 47.4 / 128.0),
    ],
    [
        (43.0 / 128.0, 47.4 / 128.0),
        (38.2 / 128.0, 68.3 / 128.0),
        (89.1 / 128.0, 58.0 / 128.0),
        (86.5 / 128.0, 83.9 / 128.0),
    ],
    [
        (86.5 / 128.0, 83.9 / 128.0),
        (84.2 / 128.0, 106.5 / 128.0),
        (51.1 / 128.0, 107.4 / 128.0),
        (35.8 / 128.0, 89.6 / 128.0),
    ],
];

pub(crate) fn show_app_icon(ui: &mut egui::Ui, size: f32, announce: bool) -> Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    let texture = app_icon_texture(ui.ctx());
    ui.painter().image(
        texture.id(),
        rect,
        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
        Color32::WHITE,
    );
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

fn app_icon_texture(ctx: &egui::Context) -> egui::TextureHandle {
    let texture_id = egui::Id::new(APP_ICON_TEXTURE_ID);
    if let Some(texture) = ctx.data(|data| data.get_temp::<egui::TextureHandle>(texture_id)) {
        return texture;
    }

    let texture = ctx.load_texture(
        APP_ICON_TEXTURE_ID,
        egui::ColorImage::from_rgba_unmultiplied(
            [
                APP_ICON_SOURCE_WIDTH as usize,
                APP_ICON_SOURCE_WIDTH as usize,
            ],
            &app_icon_rgba(APP_ICON_SOURCE_WIDTH),
        ),
        egui::TextureOptions::LINEAR,
    );
    ctx.data_mut(|data| data.insert_temp(texture_id, texture.clone()));
    texture
}

/// Decodes the approved opaque app tile and resamples it to a square system
/// icon. The canonical PNG stays byte-for-byte unchanged at 128×127; every
/// consumer uses the same area-weighted box normalization.
pub(crate) fn app_icon_rgba(size: u32) -> Vec<u8> {
    assert!(size > 0, "Scribe app icon size must be non-zero");
    let source = decoded_app_icon();
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            rgba.extend(area_resampled_pixel(source, size, x, y));
        }
    }
    rgba
}

fn area_resampled_pixel(source: &DecodedAppIcon, size: u32, x: u32, y: u32) -> [u8; 4] {
    let source_x_start = x * source.width;
    let source_x_end = (x + 1) * source.width;
    let source_y_start = y * source.height;
    let source_y_end = (y + 1) * source.height;
    let mut channels = [0_u64; 4];
    let mut total_weight = 0_u64;

    for source_y in source_y_start / size..source_y_end.div_ceil(size) {
        let y_weight =
            (source_y_end.min((source_y + 1) * size) - source_y_start.max(source_y * size)) as u64;
        for source_x in source_x_start / size..source_x_end.div_ceil(size) {
            let x_weight = (source_x_end.min((source_x + 1) * size)
                - source_x_start.max(source_x * size)) as u64;
            let weight = x_weight * y_weight;
            let pixel_start = (source_y as usize * source.width as usize + source_x as usize) * 4;
            for (channel, value) in channels
                .iter_mut()
                .zip(&source.rgba[pixel_start..pixel_start + 4])
            {
                *channel += u64::from(*value) * weight;
            }
            total_weight += weight;
        }
    }

    channels.map(|value| ((value + total_weight / 2) / total_weight) as u8)
}

fn decoded_app_icon() -> &'static DecodedAppIcon {
    DECODED_APP_ICON.get_or_init(|| {
        decode_app_icon().expect("the checked-in Scribe application icon must be a valid RGBA PNG")
    })
}

fn decode_app_icon() -> Result<DecodedAppIcon, String> {
    let mut decoder = png::Decoder::new(Cursor::new(APP_ICON_PNG));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder
        .read_info()
        .map_err(|error| format!("could not read Scribe application icon: {error}"))?;
    let mut decoded = vec![
        0;
        reader
            .output_buffer_size()
            .ok_or("application icon is too large")?
    ];
    let output = reader
        .next_frame(&mut decoded)
        .map_err(|error| format!("could not decode Scribe application icon: {error}"))?;
    let pixels = &decoded[..output.buffer_size()];
    let rgba = match output.color_type {
        png::ColorType::Rgba => pixels.to_vec(),
        png::ColorType::Rgb => pixels
            .chunks_exact(3)
            .flat_map(|pixel| [pixel[0], pixel[1], pixel[2], u8::MAX])
            .collect(),
        png::ColorType::Grayscale => pixels
            .iter()
            .flat_map(|&value| [value, value, value, u8::MAX])
            .collect(),
        png::ColorType::GrayscaleAlpha => pixels
            .chunks_exact(2)
            .flat_map(|pixel| [pixel[0], pixel[0], pixel[0], pixel[1]])
            .collect(),
        png::ColorType::Indexed => {
            return Err("application icon decoder left an indexed PNG unexpanded".into());
        }
    };

    Ok(DecodedAppIcon {
        width: output.width,
        height: output.height,
        rgba,
    })
}

#[cfg(test)]
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
    use sha2::{Digest, Sha256};

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

    fn path_numbers(segment: &str) -> Vec<f32> {
        let mut values = Vec::new();
        let mut current = String::new();
        for character in segment.chars() {
            if character == '-' {
                if !current.is_empty() {
                    values.push(current.parse().expect("numeric SVG path coordinate"));
                    current.clear();
                }
                current.push(character);
            } else if character.is_ascii_digit() || character == '.' {
                current.push(character);
            } else if !current.is_empty() {
                values.push(current.parse().expect("numeric SVG path coordinate"));
                current.clear();
            }
        }
        if !current.is_empty() {
            values.push(current.parse().expect("numeric SVG path coordinate"));
        }
        values
    }

    fn coordinate(values: &[f32], index: usize) -> (f32, f32) {
        (values[index], values[index + 1])
    }

    fn relative_curve(start: (f32, f32), values: &[f32]) -> [(f32, f32); 4] {
        assert_eq!(values.len(), 6);
        [
            start,
            (start.0 + values[0], start.1 + values[1]),
            (start.0 + values[2], start.1 + values[3]),
            (start.0 + values[4], start.1 + values[5]),
        ]
    }

    fn absolute_s_curves(path: &str) -> [[(f32, f32); 4]; 3] {
        assert!(path.starts_with('M'));
        let absolute_command = path.find('C').expect("absolute cubic command");
        let relative_command = path[absolute_command + 1..]
            .find('c')
            .expect("relative cubic command")
            + absolute_command
            + 1;

        let move_to = path_numbers(&path[1..absolute_command]);
        let absolute = path_numbers(&path[absolute_command + 1..relative_command]);
        // SVG repeats the previous command when another complete coordinate
        // set follows, so one `c` encodes both remaining cubic segments here.
        let relative = path_numbers(&path[relative_command + 1..]);
        assert_eq!(move_to.len(), 2);
        assert_eq!(absolute.len(), 6);
        assert_eq!(relative.len(), 12);

        let first = [
            coordinate(&move_to, 0),
            coordinate(&absolute, 0),
            coordinate(&absolute, 2),
            coordinate(&absolute, 4),
        ];
        let second = relative_curve(first[3], &relative[..6]);
        let third = relative_curve(second[3], &relative[6..]);
        [first, second, third]
    }

    fn sample_absolute_curves(
        curves: [[(f32, f32); 4]; 3],
        segments_per_curve: usize,
    ) -> Vec<Pos2> {
        let mut points = Vec::new();
        for (curve_index, curve) in curves.into_iter().enumerate() {
            for step in 0..=segments_per_curve {
                if curve_index > 0 && step == 0 {
                    continue;
                }
                let t = step as f32 / segments_per_curve as f32;
                let inverse = 1.0 - t;
                let weights = [
                    inverse.powi(3),
                    3.0 * inverse.powi(2) * t,
                    3.0 * inverse * t.powi(2),
                    t.powi(3),
                ];
                points.push(Pos2::new(
                    curve
                        .iter()
                        .zip(weights)
                        .map(|((x, _), weight)| x * weight)
                        .sum(),
                    curve
                        .iter()
                        .zip(weights)
                        .map(|((_, y), weight)| y * weight)
                        .sum(),
                ));
            }
        }
        points
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

        let actual = absolute_s_curves(path_data(svg));
        for (actual_curve, expected_curve) in actual.into_iter().zip(S_CURVES) {
            for ((actual_x, actual_y), (expected_x, expected_y)) in
                actual_curve.into_iter().zip(expected_curve)
            {
                assert!((actual_x - expected_x * 128.0).abs() <= 0.0001);
                assert!((actual_y - expected_y * 128.0).abs() <= 0.0001);
            }
        }
        let native_points =
            s_curve_points(Rect::from_min_max(Pos2::ZERO, Pos2::new(128.0, 128.0)), 10);
        let canonical_points = sample_absolute_curves(actual, 10);
        assert_eq!(native_points.len(), canonical_points.len());
        for (native, canonical) in native_points.into_iter().zip(canonical_points) {
            assert!(native.distance(canonical) <= 0.0001);
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
    fn canonical_app_icon_preserves_the_approved_source_bytes_and_dimensions() {
        assert_eq!(
            format!("{:x}", Sha256::digest(APP_ICON_PNG)),
            "f836d49b93ba3e2027d31e10588fe30f755837f912dc59e0e94c0565ded0aac4"
        );
        let source = decoded_app_icon();
        assert_eq!(
            (source.width, source.height),
            (APP_ICON_SOURCE_WIDTH, APP_ICON_SOURCE_HEIGHT)
        );
        assert_eq!(source.rgba.len(), 128 * 127 * 4);
        assert!(source.rgba.chunks_exact(4).all(|pixel| pixel[3] == u8::MAX));
    }

    #[test]
    fn app_icon_normalization_is_square_deterministic_and_uses_the_supplied_tile() {
        for (size, expected_digest) in [
            (
                16,
                "48741963414c1ebeedb01012da4480b956ee15d37bc601dc60aa32fed0fe87f3",
            ),
            (
                32,
                "977ae7bac1609c6445ec2d6934ddb98798b2ff4138ff52f7d17e61e68a2c1d3c",
            ),
            (
                128,
                "bb6f9c33ee92955f1f8e4369154cd868b8959a210546a484201fa3d26c99a7cb",
            ),
        ] {
            let rgba = app_icon_rgba(size);
            assert_eq!(rgba.len(), (size * size * 4) as usize);
            assert!(rgba.chunks_exact(4).all(|pixel| pixel[3] == u8::MAX));
            assert_eq!(format!("{:x}", Sha256::digest(&rgba)), expected_digest);
        }

        let normalized = app_icon_rgba(128);
        let source = decoded_app_icon();
        assert_eq!(
            &normalized[127 * 128 * 4..128 * 128 * 4],
            &source.rgba[126 * 128 * 4..127 * 128 * 4]
        );
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
