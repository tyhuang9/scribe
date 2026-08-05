mod controller;
pub mod platform;
mod view;

// Keep one facade for future runtime-neutral event wiring even though Phase 5
// integration currently names only the controller, mode, and phase directly.
#[allow(unused_imports)]
pub use controller::{
    OverlayAudioLevel, OverlayController, OverlayError, OverlayMode, OverlayPhase, OverlayRecovery,
    OverlayTranscript, OverlayViewState,
};
pub use platform::{
    CapturedTarget, OverlayPosition, capture_foreground_target,
    captured_target_application_identity, overlay_focus_safety_available, reduced_motion_preferred,
};
pub use view::show_overlay_viewport;
