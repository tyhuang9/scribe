use eframe::egui::{self, Align, Align2, RichText, Rounding, ScrollArea, Vec2};

use super::{
    controls::{ButtonTone, Icon, button, icon_glyph, paint_focus_ring},
    state::ModelViewModel,
};

const MODEL_OPTION_HEIGHT: f32 = 44.0;
const PICKER_MIN_WIDTH: f32 = 260.0;
const PICKER_MAX_OPTIONS_HEIGHT: f32 = 320.0;
const PICKER_VIEWPORT_INSET: f32 = 16.0;
const PICKER_NON_SCROLLING_HEIGHT: f32 = 128.0;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ReadyModelPickerAction {
    Select(String),
    ManageModels,
}

pub(crate) fn close_ready_model_picker_and_restore_focus(
    ui: &egui::Ui,
    popup_id: egui::Id,
    trigger_id: egui::Id,
) {
    if ui.memory(|memory| memory.is_popup_open(popup_id)) {
        ui.memory_mut(|memory| {
            memory.close_popup();
            memory.request_focus(trigger_id);
        });
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct ReadyModelPickerLayout {
    pub options_viewport: Option<egui::Rect>,
    pub options_content_size: Option<Vec2>,
}

pub(crate) fn show_ready_model_picker(
    ui: &mut egui::Ui,
    popup_id: egui::Id,
    anchor: &egui::Response,
    selected_model_id: Option<&str>,
    models: &[ModelViewModel],
) -> Option<ReadyModelPickerAction> {
    let was_open = ui.memory(|memory| memory.is_popup_open(popup_id));
    let screen_rect = ui.ctx().screen_rect();
    let popup_width = anchor
        .rect
        .width()
        .max(PICKER_MIN_WIDTH)
        .min((screen_rect.width() - PICKER_VIEWPORT_INSET * 2.0).max(anchor.rect.width()));
    let available_above = (anchor.rect.top() - screen_rect.top() - PICKER_VIEWPORT_INSET).max(0.0);
    let available_below =
        (screen_rect.bottom() - anchor.rect.bottom() - PICKER_VIEWPORT_INSET).max(0.0);
    let (placement, available_height) = if available_below >= available_above {
        (egui::AboveOrBelow::Below, available_below)
    } else {
        (egui::AboveOrBelow::Above, available_above)
    };
    let options_max_height = (available_height - PICKER_NON_SCROLLING_HEIGHT)
        .clamp(MODEL_OPTION_HEIGHT, PICKER_MAX_OPTIONS_HEIGHT);
    let mut action = None;

    egui::popup::popup_above_or_below_widget(ui, popup_id, anchor, placement, |ui| {
        ui.set_width(popup_width);
        ui.label(RichText::new("Ready models").strong());
        ui.add_space(4.0);
        #[cfg(test)]
        let mut options_layout = None;

        let ready_models = models
            .iter()
            .filter(|model| model.installed && model.ready)
            .collect::<Vec<_>>();
        if ready_models.is_empty() {
            ui.label(RichText::new("No installed models are ready to use.").weak());
        } else {
            let options = ScrollArea::vertical()
                .id_source(popup_id.with("options-scroll"))
                .auto_shrink([false, true])
                .max_height(options_max_height)
                .show(ui, |ui| {
                    for model in ready_models {
                        let current = selected_model_id == Some(model.id.as_str());
                        let label = if current {
                            format!(
                                "{}  {}  (Current)",
                                icon_glyph(Icon::CheckCircle),
                                model.display_name
                            )
                        } else {
                            model.display_name.clone()
                        };
                        let (rect, _) = ui.allocate_exact_size(
                            Vec2::new(ui.available_width(), MODEL_OPTION_HEIGHT),
                            egui::Sense::hover(),
                        );
                        let response = ui.interact(
                            rect,
                            popup_id.with(("option", model.id.as_str())),
                            egui::Sense::click(),
                        );
                        let visuals = ui.style().interact(&response);
                        ui.painter().rect(
                            rect,
                            Rounding::same(5.0),
                            visuals.bg_fill,
                            visuals.bg_stroke,
                        );
                        ui.painter().text(
                            rect.center(),
                            Align2::CENTER_CENTER,
                            label,
                            egui::FontId::proportional(13.0),
                            visuals.text_color(),
                        );
                        let accessible_name = if current {
                            format!("{}, current model", model.display_name)
                        } else {
                            format!("Select {}", model.display_name)
                        };
                        response.widget_info(|| {
                            egui::WidgetInfo::labeled(
                                egui::WidgetType::Button,
                                accessible_name.clone(),
                            )
                        });
                        ui.ctx().accesskit_node_builder(response.id, |builder| {
                            builder.set_role(egui::accesskit::Role::Button);
                            builder.set_name(accessible_name.clone());
                            builder.set_selected(current);
                            if current {
                                builder.set_description("Current selected model");
                            }
                        });
                        paint_focus_ring(ui, &response, Rounding::same(5.0));
                        if response.has_focus()
                            || ui.input(|input| {
                                input.has_accesskit_action_request(
                                    response.id,
                                    egui::accesskit::Action::Focus,
                                )
                            })
                        {
                            response.scroll_to_me(Some(Align::Center));
                        }
                        if response.enabled() && response.hovered() {
                            response
                                .clone()
                                .on_hover_cursor(egui::CursorIcon::PointingHand);
                        }
                        let keyboard_activated = response.has_focus()
                            && ui.input(|input| {
                                input.key_pressed(egui::Key::Enter)
                                    || input.key_pressed(egui::Key::Space)
                            });
                        if (response.clicked() || keyboard_activated) && !current {
                            action = Some(ReadyModelPickerAction::Select(model.id.clone()));
                            ui.memory_mut(|memory| memory.close_popup());
                        }
                    }
                });
            #[cfg(not(test))]
            let _ = options;
            #[cfg(test)]
            {
                options_layout = Some((options.inner_rect, options.content_size));
            }
        }

        ui.separator();
        let manage = button(ui, "Manage models…", ButtonTone::Text);
        if manage.enabled() && manage.hovered() {
            manage
                .clone()
                .on_hover_cursor(egui::CursorIcon::PointingHand);
        }
        if manage.clicked() {
            action = Some(ReadyModelPickerAction::ManageModels);
            // Navigation changes the entire central route. Do not restore focus
            // to the trigger in the old sidebar/quick-control context, otherwise
            // its focus tooltip is left visible over the Models screen.
            ui.memory_mut(|memory| {
                memory.close_popup();
                memory.surrender_focus(anchor.id);
                memory.surrender_focus(manage.id);
            });
        }
        #[cfg(test)]
        {
            ui.data_mut(|data| {
                data.insert_temp(
                    popup_id.with("test-layout"),
                    ReadyModelPickerLayout {
                        options_viewport: options_layout.map(|(viewport, _)| viewport),
                        options_content_size: options_layout.map(|(_, content)| content),
                    },
                );
            });
        }
    });

    let is_open = ui.memory(|memory| memory.is_popup_open(popup_id));
    if was_open && !is_open && !matches!(action, Some(ReadyModelPickerAction::ManageModels)) {
        ui.memory_mut(|memory| memory.request_focus(anchor.id));
    }
    action
}
