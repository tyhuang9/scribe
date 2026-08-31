#![cfg(not(test))]
#![allow(
    dead_code,
    reason = "shared modules include desktop-side APIs that are intentionally excluded from the dedicated worker"
)]

//! Dedicated native inference process.
//!
//! This binary is the only production target that includes GGUF and ASR ONNX
//! execution code. The desktop retains its existing same-executable VAD role.

#[path = "../backend_policy.rs"]
mod backend_policy;
mod config {
    use anyhow::{Result, anyhow};
    use directories::ProjectDirs;

    pub const MAX_RECORDING_SECONDS: u32 = 600;
    pub(crate) const RECORDING_CAPTURE_SAFETY_ALLOWANCE_SECONDS: u32 = 2;

    pub(crate) fn project_dirs() -> Result<ProjectDirs> {
        ProjectDirs::from("com", "Scribe", "Scribe")
            .ok_or_else(|| anyhow!("could not resolve Scribe application directories"))
    }
}
#[path = "../embedded_runtime.rs"]
mod embedded_runtime;
#[path = "../inference_server.rs"]
mod inference_server;
#[path = "../model_catalog.rs"]
mod model_catalog;
#[path = "../onnx_worker.rs"]
mod onnx_worker;
#[path = "../prepared_audio.rs"]
mod prepared_audio;
#[path = "../receipt_bundle_catalog.rs"]
mod receipt_bundle_catalog;
#[path = "../runtime_artifact.rs"]
mod runtime_artifact;
#[path = "../runtime_contract.rs"]
mod runtime_contract;
#[path = "../runtime_router.rs"]
mod runtime_router;
#[path = "../silero_vad_native.rs"]
mod silero_vad_native;
#[path = "../support_assets.rs"]
mod support_assets;
#[path = "../worker_contracts.rs"]
mod worker_contracts;
mod transcription {
    pub(crate) use crate::worker_contracts::*;
}

fn main() {
    if let Err(error) = onnx_worker::harden_windows_dll_search() {
        eprintln!("Scribe inference worker could not harden native library loading: {error:#}");
        std::process::exit(1);
    }
    std::process::exit(inference_server::run());
}
