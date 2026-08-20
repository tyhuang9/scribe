use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Mutex},
};

use accesskit_windows::{Adapter, UiaInitMarker};
use eframe::egui::accesskit::{
    Action, ActionHandler, ActionRequest, DefaultActionVerb, Live, NodeBuilder, NodeClassSet,
    NodeId, Rect, Role, Tree, TreeUpdate,
};

use super::{
    ControlActionBridge, WindowRole,
    layout::{ControlLayout, DisplayLayout, PhysicalRect},
    raster::format_elapsed,
};
use crate::overlay::{
    controller::{OverlayMode, OverlayPhase, OverlayViewState},
    platform::OverlayWindowBounds,
    view::{live_accessible_text, live_overlay_announcement},
};

const DISPLAY_ROOT_ID: NodeId = NodeId(0xD100);
const DISPLAY_STATUS_ID: NodeId = NodeId(0xD101);
const DISPLAY_METER_ID: NodeId = NodeId(0xD102);
const DISPLAY_PREVIEW_ID: NodeId = NodeId(0xD103);
const DISPLAY_ANNOUNCEMENT_ID: NodeId = NodeId(0xD104);
const DISPLAY_ELAPSED_ID: NodeId = NodeId(0xD105);
const CONTROL_ROOT_ID: NodeId = NodeId(0xC100);
pub(super) const CONTROL_BUTTON_ID: NodeId = NodeId(0xC101);

pub(super) const CANCEL_RECORDING_LABEL: &str = "Cancel recording and discard it";

pub(super) struct NativeAccessibility {
    adapter: Adapter,
    latest_tree: Arc<Mutex<TreeUpdate>>,
    role: WindowRole,
}

impl NativeAccessibility {
    pub(super) fn install(
        hwnd: windows_sys::Win32::Foundation::HWND,
        role: WindowRole,
        action_bridge: Option<Arc<ControlActionBridge>>,
    ) -> Option<Self> {
        let initial = hidden_tree(role);
        let latest_tree = Arc::new(Mutex::new(initial));
        let source_tree = Arc::clone(&latest_tree);
        let handler: Box<dyn ActionHandler + Send> = match role {
            WindowRole::Display => Box::new(PassiveActionHandler),
            WindowRole::Control => Box::new(ControlActionHandler { action_bridge }),
        };
        let adapter = catch_unwind(AssertUnwindSafe(|| {
            Adapter::new(
                accesskit_windows::HWND(hwnd as isize),
                source_tree
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone(),
                false,
                handler,
                UiaInitMarker::new(),
            )
        }))
        .ok()?;
        Some(Self {
            adapter,
            latest_tree,
            role,
        })
    }

    pub(super) fn handle_wm_getobject(
        &self,
        wparam: windows_sys::Win32::Foundation::WPARAM,
        lparam: windows_sys::Win32::Foundation::LPARAM,
    ) -> Option<windows_sys::Win32::Foundation::LRESULT> {
        self.adapter
            .handle_wm_getobject(
                accesskit_windows::WPARAM(wparam),
                accesskit_windows::LPARAM(lparam),
            )
            .map(|result| {
                let result: accesskit_windows::LRESULT = result.into();
                result.0
            })
    }

    pub(super) fn update(
        &self,
        state: Option<&OverlayViewState>,
        visible: bool,
        control_enabled: bool,
        bounds: Option<OverlayWindowBounds>,
    ) -> bool {
        let update = match (self.role, state) {
            (WindowRole::Display, Some(state)) => display_tree(state, visible, bounds),
            (WindowRole::Control, _) => control_tree(visible && control_enabled, bounds),
            (_, None) => hidden_tree(self.role),
        };
        if let Ok(mut latest) = self.latest_tree.lock() {
            *latest = update.clone();
        } else {
            return false;
        }
        catch_unwind(AssertUnwindSafe(|| {
            self.adapter.update(update).raise();
        }))
        .is_ok()
    }
}

struct PassiveActionHandler;

impl ActionHandler for PassiveActionHandler {
    fn do_action(&mut self, _request: ActionRequest) {}
}

struct ControlActionHandler {
    action_bridge: Option<Arc<ControlActionBridge>>,
}

impl ActionHandler for ControlActionHandler {
    fn do_action(&mut self, request: ActionRequest) {
        if request.target == CONTROL_BUTTON_ID
            && request.action == Action::Default
            && let Some(bridge) = &self.action_bridge
        {
            bridge.emit_abandon();
        }
    }
}

fn hidden_tree(role: WindowRole) -> TreeUpdate {
    let (root_id, name) = match role {
        WindowRole::Display => (DISPLAY_ROOT_ID, "Scribe recording overlay"),
        WindowRole::Control => (CONTROL_ROOT_ID, CANCEL_RECORDING_LABEL),
    };
    let mut classes = NodeClassSet::new();
    let mut root = NodeBuilder::new(Role::Window);
    root.set_name(name);
    root.set_hidden();
    TreeUpdate {
        nodes: vec![(root_id, root.build(&mut classes))],
        tree: Some(Tree::new(root_id)),
        focus: root_id,
    }
}

fn accesskit_rect(rect: PhysicalRect) -> Rect {
    Rect::new(
        f64::from(rect.x0),
        f64::from(rect.y0),
        f64::from(rect.x1),
        f64::from(rect.y1),
    )
}

fn display_tree(
    state: &OverlayViewState,
    visible: bool,
    bounds: Option<OverlayWindowBounds>,
) -> TreeUpdate {
    let Some(layout) = bounds.and_then(|bounds| DisplayLayout::from_bounds(state.mode, bounds))
    else {
        return hidden_tree(WindowRole::Display);
    };
    if !visible {
        return hidden_tree(WindowRole::Display);
    }
    let mut classes = NodeClassSet::new();
    let preview_visible = state.mode == OverlayMode::Live;
    let elapsed_visible = preview_visible || state.elapsed.is_some();
    let mut children = vec![DISPLAY_STATUS_ID, DISPLAY_METER_ID];
    if elapsed_visible {
        children.push(DISPLAY_ELAPSED_ID);
    }
    if preview_visible {
        children.push(DISPLAY_PREVIEW_ID);
    }
    let announcement = live_overlay_announcement(state);
    if preview_visible && announcement.is_some() {
        children.push(DISPLAY_ANNOUNCEMENT_ID);
    }

    let mut root = NodeBuilder::new(Role::Window);
    root.set_name("Scribe recording overlay");
    root.set_children(children);
    root.set_bounds(accesskit_rect(layout.root));

    let status_name = if state.phase == OverlayPhase::Listening {
        "Scribe is recording"
    } else {
        state.phase.label()
    };
    let mut status = NodeBuilder::new(Role::StaticText);
    status.set_name(status_name);
    status.set_bounds(accesskit_rect(layout.status));

    let level = state
        .audio_level
        .rms
        .max(state.audio_level.peak * 0.7)
        .clamp(0.0, 1.0);
    let mut meter = NodeBuilder::new(Role::ProgressIndicator);
    meter.set_name("Microphone input level");
    meter.set_description(status_name);
    meter.set_numeric_value((level * 100.0).round() as f64);
    meter.set_min_numeric_value(0.0);
    meter.set_max_numeric_value(100.0);
    meter.set_bounds(accesskit_rect(layout.meter));

    let mut nodes = vec![
        (DISPLAY_ROOT_ID, root.build(&mut classes)),
        (DISPLAY_STATUS_ID, status.build(&mut classes)),
        (DISPLAY_METER_ID, meter.build(&mut classes)),
    ];
    if elapsed_visible {
        let mut elapsed = NodeBuilder::new(Role::StaticText);
        elapsed.set_name(format!(
            "Elapsed time {}",
            format_elapsed(state.elapsed.unwrap_or_default())
        ));
        elapsed.set_bounds(accesskit_rect(layout.elapsed));
        nodes.push((DISPLAY_ELAPSED_ID, elapsed.build(&mut classes)));
    }
    if preview_visible {
        let mut preview = NodeBuilder::new(Role::StaticText);
        preview.set_name(live_accessible_text(state));
        preview.set_bounds(accesskit_rect(
            layout.preview.expect("live layout includes preview bounds"),
        ));
        if state.error.is_some() || state.notice.is_some() {
            preview.set_live(Live::Polite);
        }
        nodes.push((DISPLAY_PREVIEW_ID, preview.build(&mut classes)));
    }
    if preview_visible && let Some(announcement) = announcement {
        let mut live = NodeBuilder::new(Role::StaticText);
        live.set_name(announcement);
        live.set_live(Live::Polite);
        live.set_bounds(accesskit_rect(
            layout
                .preview
                .expect("live layout includes announcement bounds"),
        ));
        nodes.push((DISPLAY_ANNOUNCEMENT_ID, live.build(&mut classes)));
    }
    TreeUpdate {
        nodes,
        tree: Some(Tree::new(DISPLAY_ROOT_ID)),
        focus: DISPLAY_ROOT_ID,
    }
}

fn control_tree(visible: bool, bounds: Option<OverlayWindowBounds>) -> TreeUpdate {
    let Some(layout) = bounds.and_then(ControlLayout::from_bounds) else {
        return hidden_tree(WindowRole::Control);
    };
    if !visible {
        return hidden_tree(WindowRole::Control);
    }
    let mut classes = NodeClassSet::new();
    let mut root = NodeBuilder::new(Role::Window);
    root.set_name(CANCEL_RECORDING_LABEL);
    root.set_children(vec![CONTROL_BUTTON_ID]);
    root.set_bounds(accesskit_rect(layout.root));

    let mut button = NodeBuilder::new(Role::Button);
    button.set_name(CANCEL_RECORDING_LABEL);
    button.set_description(CANCEL_RECORDING_LABEL);
    button.set_default_action_verb(DefaultActionVerb::Press);
    button.add_action(Action::Default);
    button.set_bounds(accesskit_rect(layout.button));

    TreeUpdate {
        nodes: vec![
            (CONTROL_ROOT_ID, root.build(&mut classes)),
            (CONTROL_BUTTON_ID, button.build(&mut classes)),
        ],
        tree: Some(Tree::new(CONTROL_ROOT_ID)),
        focus: CONTROL_ROOT_ID,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlay::controller::OverlayTranscript;

    fn display_bounds(mode: OverlayMode) -> OverlayWindowBounds {
        match mode {
            OverlayMode::Live => OverlayWindowBounds {
                x: -1120,
                y: 1119,
                width: 750,
                height: 78,
            },
            OverlayMode::Minimal | OverlayMode::Off => OverlayWindowBounds {
                x: 905,
                y: 1272,
                width: 400,
                height: 65,
            },
        }
    }

    fn control_bounds() -> OverlayWindowBounds {
        OverlayWindowBounds {
            x: 1590,
            y: 1283,
            width: 55,
            height: 55,
        }
    }

    #[test]
    fn hidden_trees_expose_no_live_region_or_cancel_button() {
        for role in [WindowRole::Display, WindowRole::Control] {
            let tree = hidden_tree(role);
            assert_eq!(tree.nodes.len(), 1);
            assert!(tree.nodes[0].1.is_hidden());
            assert!(tree.nodes.iter().all(|(_, node)| node.live().is_none()));
            assert!(tree.nodes.iter().all(|(id, _)| *id != CONTROL_BUTTON_ID));
        }
    }

    #[test]
    fn visible_control_has_exact_name_and_default_action() {
        let tree = control_tree(true, Some(control_bounds()));
        let root = tree
            .nodes
            .iter()
            .find(|(id, _)| *id == CONTROL_ROOT_ID)
            .map(|(_, node)| node)
            .expect("control root");
        let button = tree
            .nodes
            .iter()
            .find(|(id, _)| *id == CONTROL_BUTTON_ID)
            .map(|(_, node)| node)
            .expect("cancel button");
        assert_eq!(button.name(), Some(CANCEL_RECORDING_LABEL));
        assert_eq!(button.description(), Some(CANCEL_RECORDING_LABEL));
        assert!(button.supports_action(Action::Default));
        let expected = Rect::new(0.0, 0.0, 55.0, 55.0);
        assert_eq!(root.bounds(), Some(expected));
        assert_eq!(button.bounds(), Some(expected));
    }

    #[test]
    fn visible_live_tree_preserves_preview_and_announcement_semantics() {
        let state = OverlayViewState {
            mode: OverlayMode::Live,
            phase: OverlayPhase::Listening,
            transcript: OverlayTranscript {
                committed: "committed".to_owned(),
                tentative: " tentative".to_owned(),
                revision: 3,
            },
            transcript_announcement: Some("Committed transcript: committed".to_owned()),
            ..OverlayViewState::default()
        };
        let tree = display_tree(&state, true, Some(display_bounds(OverlayMode::Live)));
        let live_nodes: Vec<_> = tree
            .nodes
            .iter()
            .filter(|(_, node)| node.live() == Some(Live::Polite))
            .collect();
        assert_eq!(live_nodes.len(), 1);
        assert_eq!(
            live_nodes[0].1.name(),
            Some("Committed transcript: committed")
        );
        assert!(tree.nodes.iter().any(|(id, node)| {
            *id == DISPLAY_PREVIEW_ID
                && node.name()
                    == Some("Committed transcript: committed. Tentative transcript:  tentative")
        }));
        assert!(tree.nodes.iter().any(|(id, node)| {
            *id == DISPLAY_ELAPSED_ID
                && node.name() == Some("Elapsed time 00:00")
                && node.live().is_none()
        }));
    }

    #[test]
    fn compact_tree_has_no_orphaned_preview_or_live_nodes() {
        let state = OverlayViewState {
            mode: OverlayMode::Minimal,
            phase: OverlayPhase::Listening,
            transcript_announcement: Some("Committed transcript: hidden compact text".to_owned()),
            ..OverlayViewState::default()
        };
        let tree = display_tree(&state, true, Some(display_bounds(OverlayMode::Minimal)));
        let root = tree
            .nodes
            .iter()
            .find(|(id, _)| *id == DISPLAY_ROOT_ID)
            .map(|(_, node)| node)
            .expect("display root");
        assert_eq!(root.children().len(), tree.nodes.len() - 1);
        assert!(tree.nodes.iter().all(|(_, node)| node.live().is_none()));
        assert!(
            tree.nodes
                .iter()
                .all(|(id, _)| *id != DISPLAY_PREVIEW_ID && *id != DISPLAY_ANNOUNCEMENT_ID)
        );
        assert!(tree.nodes.iter().all(|(id, _)| *id != DISPLAY_ELAPSED_ID));
    }

    #[test]
    fn visible_live_nodes_use_current_physical_layout_bounds() {
        let state = OverlayViewState {
            mode: OverlayMode::Live,
            phase: OverlayPhase::Listening,
            elapsed: Some(std::time::Duration::from_secs(12)),
            transcript_announcement: Some("Committed transcript: test".to_owned()),
            ..OverlayViewState::default()
        };
        let tree = display_tree(&state, true, Some(display_bounds(OverlayMode::Live)));
        let node = |id| {
            tree.nodes
                .iter()
                .find(|(node_id, _)| *node_id == id)
                .map(|(_, node)| node)
                .expect("expected accessibility node")
        };
        assert_eq!(
            node(DISPLAY_ROOT_ID).bounds(),
            Some(Rect::new(0.0, 0.0, 750.0, 78.0))
        );
        assert_eq!(
            node(DISPLAY_STATUS_ID).bounds(),
            Some(Rect::new(20.0, 18.75, 57.5, 56.25))
        );
        assert_eq!(
            node(DISPLAY_METER_ID).bounds(),
            node(DISPLAY_STATUS_ID).bounds()
        );
        assert_eq!(node(DISPLAY_ELAPSED_ID).name(), Some("Elapsed time 00:12"));
        assert_eq!(
            node(DISPLAY_ELAPSED_ID).bounds(),
            Some(Rect::new(70.0, 25.625, 130.0, 54.375))
        );
        assert_eq!(
            node(DISPLAY_PREVIEW_ID).bounds(),
            Some(Rect::new(153.75, 25.625, 686.25, 54.375))
        );
        assert_eq!(
            node(DISPLAY_ANNOUNCEMENT_ID).bounds(),
            node(DISPLAY_PREVIEW_ID).bounds()
        );
        assert_eq!(
            tree.nodes
                .iter()
                .filter(|(_, node)| node.live() == Some(Live::Polite))
                .count(),
            1
        );
    }

    #[test]
    fn compact_elapsed_semantics_are_bounded_and_not_live() {
        let state = OverlayViewState {
            mode: OverlayMode::Minimal,
            phase: OverlayPhase::Listening,
            elapsed: Some(std::time::Duration::from_secs(65)),
            ..OverlayViewState::default()
        };
        let tree = display_tree(&state, true, Some(display_bounds(OverlayMode::Minimal)));
        let elapsed = tree
            .nodes
            .iter()
            .find(|(id, _)| *id == DISPLAY_ELAPSED_ID)
            .map(|(_, node)| node)
            .expect("elapsed node");
        assert_eq!(elapsed.name(), Some("Elapsed time 01:05"));
        assert_eq!(
            elapsed.bounds(),
            Some(Rect::new(258.75, 20.625, 325.0, 46.875))
        );
        assert!(tree.nodes.iter().all(|(_, node)| node.live().is_none()));
    }
}
