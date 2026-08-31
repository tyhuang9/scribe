#![cfg(not(test))]

//! Dedicated native inference process.
//!
//! This binary is the only production target that includes GGUF and ASR ONNX
//! execution code. The desktop retains its existing same-executable VAD role.

#[allow(
    dead_code,
    reason = "the dedicated worker consumes only the shared policy subset needed by its runtime adapter"
)]
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
#[allow(
    dead_code,
    reason = "the dedicated worker consumes only the shared embedded-runtime subset needed by inference"
)]
#[path = "../embedded_runtime.rs"]
mod embedded_runtime;
#[path = "../gpu_auto_qualification.rs"]
#[allow(
    dead_code,
    reason = "the worker consumes qualification-bound wire types but never selects a release policy"
)]
mod gpu_auto_qualification;
#[allow(
    dead_code,
    reason = "the worker compiles shared verified-pack handshake contracts but never performs discovery"
)]
#[path = "../gpu_worker_pack/mod.rs"]
mod gpu_worker_pack;
#[path = "../inference_server.rs"]
mod inference_server;
#[cfg(all(target_os = "linux", target_arch = "x86_64", target_env = "gnu"))]
#[allow(
    dead_code,
    reason = "the worker shares Linux entrypoint contracts but never launches another worker"
)]
#[path = "../linux_worker_launch.rs"]
mod linux_worker_launch;
#[cfg(any(all(target_os = "macos", feature = "metal-acceleration"), test))]
#[path = "../macos_gpu.rs"]
mod macos_gpu;
#[cfg(any(target_os = "macos", test))]
#[path = "../macos_power.rs"]
mod macos_power;
#[cfg(any(target_os = "macos", test))]
#[path = "../macos_worker_launch.rs"]
mod macos_worker_launch;
#[allow(
    dead_code,
    reason = "the dedicated worker validates only the shared catalog subset needed by an admitted request"
)]
#[path = "../model_catalog.rs"]
mod model_catalog;
#[allow(
    dead_code,
    reason = "the dedicated process excludes desktop-side supervisor paths from its shared worker module"
)]
#[path = "../onnx_worker.rs"]
mod onnx_worker;
#[path = "../prepared_audio.rs"]
mod prepared_audio;
#[path = "../receipt_bundle_catalog.rs"]
mod receipt_bundle_catalog;
#[path = "../runtime_artifact.rs"]
mod runtime_artifact;
#[allow(
    dead_code,
    reason = "the dedicated worker consumes only the shared runtime-contract subset needed by inference"
)]
#[path = "../runtime_contract.rs"]
mod runtime_contract;
#[allow(
    dead_code,
    reason = "the dedicated worker consumes only the shared router subset needed by inference"
)]
#[path = "../runtime_router.rs"]
mod runtime_router;
#[path = "../silero_vad_native.rs"]
mod silero_vad_native;
#[path = "../support_assets.rs"]
mod support_assets;
#[allow(
    dead_code,
    reason = "the dedicated worker consumes only the wire-facing subset of shared worker contracts"
)]
#[path = "../worker_contracts.rs"]
mod worker_contracts;
#[path = "../worker_identity.rs"]
mod worker_identity;
mod transcription {
    pub(crate) use crate::worker_contracts::*;
}

fn main() {
    if let Err(error) = onnx_worker::validate_linux_worker_entrypoint() {
        eprintln!("Scribe inference worker rejected its Linux launch context: {error:#}");
        std::process::exit(1);
    }
    if let Err(error) = onnx_worker::harden_windows_dll_search() {
        eprintln!("Scribe inference worker could not harden native library loading: {error:#}");
        std::process::exit(1);
    }
    std::process::exit(inference_server::run());
}
