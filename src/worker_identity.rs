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
