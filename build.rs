use std::fs;
#[cfg(all(windows, feature = "vulkan-acceleration"))]
use std::path::PathBuf;

use sha2::{Digest, Sha256};

const SILERO_VAD_ASSET: &str = "resources/silero-vad/silero_vad.int8.onnx";
const SILERO_VAD_SIZE: usize = 212_860;
const SILERO_VAD_SHA256: &str = "c36d490aff5ab924ca6c7aeec4d8f6bd3d22db6fa17611b9c5b17eae58ac3a20";

fn main() {
    require_windows_static_crt();
    #[cfg(all(windows, feature = "vulkan-acceleration"))]
    prepare_windows_vulkan_import_library();
    verify_silero_vad_asset();

    println!("cargo:rerun-if-changed=native/sherpa_vad_shim.cc");
    println!("cargo:rerun-if-changed=native/sherpa-onnx-v1.13.5/voice_activity_detector_abi.h");

    cc::Build::new()
        .cpp(true)
        .file("native/sherpa_vad_shim.cc")
        .include("native")
        .flag_if_supported("/std:c++17")
        .flag_if_supported("-std=c++17")
        .warnings(true)
        .compile("scribe_sherpa_vad_shim");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if matches!(target_os.as_str(), "linux" | "android") {
        println!("cargo:rustc-link-lib=dl");
    }
}

#[cfg(all(windows, feature = "vulkan-acceleration"))]
fn prepare_windows_vulkan_import_library() {
    println!("cargo:rerun-if-env-changed=VULKAN_SDK");

    let sdk_root = std::env::var_os("VULKAN_SDK")
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
        .unwrap_or_else(|| {
            panic!(
                "vulkan-acceleration requires VULKAN_SDK to name an installed Khronos Vulkan SDK"
            )
        });
    let sdk_library = sdk_root.join("Lib").join("vulkan-1.lib");
    if !sdk_library.is_file() {
        panic!(
            "vulkan-acceleration requires the Khronos import library at VULKAN_SDK\\Lib\\vulkan-1.lib"
        );
    }

    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo must set OUT_DIR"));
    let link_library = out_dir.join("vulkan.lib");

    // transcribe-cpp-sys 0.1.3 emits `-l vulkan`, while the Windows SDK names
    // the same import library `vulkan-1.lib`. Keep the compatibility alias in
    // Cargo's private build output; never modify or bundle the installed SDK.
    fs::copy(&sdk_library, &link_library).unwrap_or_else(|error| {
        panic!("could not prepare the Vulkan SDK import library for linking: {error}")
    });
    println!("cargo:rerun-if-changed={}", sdk_library.display());
    println!("cargo:rustc-link-search=native={}", out_dir.display());
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
