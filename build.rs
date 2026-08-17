use std::fs;

use sha2::{Digest, Sha256};

const SILERO_VAD_ASSET: &str = "resources/silero-vad/silero_vad.int8.onnx";
const SILERO_VAD_SIZE: usize = 212_860;
const SILERO_VAD_SHA256: &str = "c36d490aff5ab924ca6c7aeec4d8f6bd3d22db6fa17611b9c5b17eae58ac3a20";

fn main() {
    require_windows_static_crt();
    verify_silero_vad_asset();

    println!("cargo:rerun-if-changed=native/whisper_shim.c");
    println!("cargo:rerun-if-changed=native/whisper-f049fff/whisper.h");
    println!("cargo:rerun-if-changed=native/whisper-f049fff/ggml.h");
    println!("cargo:rerun-if-changed=native/whisper-f049fff/ggml-cpu.h");
    println!("cargo:rerun-if-changed=native/whisper-f049fff/ggml-backend.h");
    println!("cargo:rerun-if-changed=native/whisper-f049fff/ggml-alloc.h");

    cc::Build::new()
        .file("native/whisper_shim.c")
        .include("native")
        .warnings(true)
        .compile("scribe_whisper_shim");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if matches!(target_os.as_str(), "linux" | "android") {
        println!("cargo:rustc-link-lib=dl");
    }
}

fn verify_silero_vad_asset() {
    println!("cargo:rerun-if-changed={SILERO_VAD_ASSET}");
    let bytes = fs::read(SILERO_VAD_ASSET)
        .unwrap_or_else(|error| panic!("could not read bundled Silero VAD asset: {error}"));
    assert_eq!(
        bytes.len(),
        SILERO_VAD_SIZE,
        "bundled Silero VAD asset size changed"
    );
    let actual = format!("{:x}", Sha256::digest(&bytes));
    assert_eq!(
        actual, SILERO_VAD_SHA256,
        "bundled Silero VAD asset SHA-256 changed"
    );
}

fn require_windows_static_crt() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os != "windows" || target_env != "msvc" {
        return;
    }

    let target_features = std::env::var("CARGO_CFG_TARGET_FEATURE").unwrap_or_default();
    if !target_features
        .split(',')
        .any(|feature| feature == "crt-static")
    {
        panic!(
            "Windows native dependencies require the static MSVC CRT; preserve `-C target-feature=+crt-static` from .cargo/config.toml when overriding target rustflags"
        );
    }
}
