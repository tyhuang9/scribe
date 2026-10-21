//! Shared identities for the desktop, inference worker, and pack authoring tool.

pub(crate) const PROTOCOL_VERSION: u8 = 5;
pub(crate) const WORKER_ABI_VERSION: u16 = 1;
pub(crate) const DESKTOP_BUILD_ID: &str = concat!(
    "local-transcriber@",
    env!("CARGO_PKG_VERSION"),
    "#",
    env!("SCRIBE_BUILD_REVISION")
);
pub(crate) const INFERENCE_WORKER_BUILD_ID: &str = concat!(
    "scribe-inference-worker@",
    env!("CARGO_PKG_VERSION"),
    "#",
    env!("SCRIBE_BUILD_REVISION")
);

#[cfg(target_os = "macos")]
#[path = "macos_retained_marker.rs"]
mod retained_marker;

// Runtime string construction need not leave a contiguous literal in optimized
// code. Package inspection requires the complete bytes in each final slice.
#[cfg(target_os = "macos")]
retained_marker::retain_macos_marker!(RETAINED_DESKTOP_BUILD_ID, DESKTOP_BUILD_ID);
#[cfg(target_os = "macos")]
retained_marker::retain_macos_marker!(
    RETAINED_INFERENCE_WORKER_BUILD_ID,
    INFERENCE_WORKER_BUILD_ID
);
