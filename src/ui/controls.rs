use eframe::egui::{self, FontFamily, FontId, TextStyle, Vec2};

const PRIMARY_TARGET_HEIGHT: f32 = 44.0;

pub(crate) fn minimum_primary_target_height() -> f32 {
    PRIMARY_TARGET_HEIGHT
}

pub(crate) fn configure_accessible_style(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = Vec2::new(8.0, 8.0);
    style.spacing.button_padding = Vec2::new(12.0, 8.0);
    style.spacing.interact_size = Vec2::new(44.0, PRIMARY_TARGET_HEIGHT);
    style.text_styles.insert(
        TextStyle::Heading,
        FontId::new(24.0, FontFamily::Proportional),
    );
    style
        .text_styles
        .insert(TextStyle::Body, FontId::new(14.0, FontFamily::Proportional));
    style.text_styles.insert(
        TextStyle::Button,
        FontId::new(13.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Small,
        FontId::new(12.0, FontFamily::Proportional),
    );
    ctx.set_style(style);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primary_targets_meet_wcag_pointer_size() {
        assert!(minimum_primary_target_height() >= 44.0);
        let ctx = egui::Context::default();
        configure_accessible_style(&ctx);
        assert!(ctx.style().spacing.interact_size.y >= 44.0);
    }
}
