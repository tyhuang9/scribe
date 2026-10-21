//! Compile-time CPU provenance shared by the desktop and installer.

pub(crate) const CPU_ROOT: &str = "/Library/Application Support/Scribe/CPU";

#[cfg(target_os = "macos")]
pub(super) const CPU_WORKER_SHA256: &str = match option_env!("SCRIBE_BUNDLED_WORKER_SHA256") {
    Some(value) => value,
    None => "",
};
#[cfg(target_os = "macos")]
const CPU_BUILD_REVISION: &str = match option_env!("SCRIBE_BUILD_REVISION") {
    Some(value) => value,
    None => "",
};

#[cfg(target_os = "macos")]
#[path = "macos_retained_marker.rs"]
mod retained_marker;

// Build scripts also include the layout validator before they emit target-crate
// environment values. Empty source-check markers grant no admission authority.
#[cfg(target_os = "macos")]
retained_marker::retain_macos_marker!(RETAINED_CPU_ROOT, CPU_ROOT);
#[cfg(target_os = "macos")]
retained_marker::retain_macos_marker!(RETAINED_CPU_WORKER_SHA256, CPU_WORKER_SHA256);
#[cfg(target_os = "macos")]
retained_marker::retain_macos_marker!(RETAINED_CPU_REVISION, CPU_BUILD_REVISION);
