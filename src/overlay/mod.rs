mod controller;
#[cfg(target_os = "windows")]
mod native_windows;
pub mod platform;
#[cfg(test)]
mod preview_parity;
mod view;

// Keep one facade for future runtime-neutral event wiring even though Phase 5
// integration currently names only the controller, mode, and phase directly.
pub use controller::{
    OverlayAudioLevel, OverlayController, OverlayMode, OverlayPhase, OverlayPresentation,
    OverlayRecovery, OverlayTranscript, OverlayViewState,
};
pub use platform::{
    CapturedTarget, OverlayPosition, capture_foreground_target,
    captured_target_application_identity, overlay_focus_safety_available,
};
pub use view::{OverlayAction, OverlayDiagnostic, show_overlay_viewport};

pub(crate) fn shutdown_overlay_viewport() {
    #[cfg(target_os = "windows")]
    native_windows::shutdown_overlay_viewport();
}
