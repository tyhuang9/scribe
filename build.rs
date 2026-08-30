use std::fs;
#[cfg(all(windows, feature = "vulkan-acceleration"))]
use std::path::PathBuf;
use std::process::Command;

use sha2::{Digest, Sha256};

const SILERO_VAD_ASSET: &str = "resources/silero-vad/silero_vad.int8.onnx";
const SILERO_VAD_SIZE: usize = 212_860;
const SILERO_VAD_SHA256: &str = "c36d490aff5ab924ca6c7aeec4d8f6bd3d22db6fa17611b9c5b17eae58ac3a20";

fn main() {
    reject_multiple_gpu_features();
    emit_build_revision();
    emit_bundled_worker_trust_anchor();
    require_windows_static_crt();
    #[cfg(all(windows, feature = "vulkan-acceleration"))]
    prepare_windows_vulkan_import_library();
    prepare_macos_native_shims();
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

fn reject_multiple_gpu_features() {
    let enabled = [
        (
            "cuda-acceleration",
            std::env::var_os("CARGO_FEATURE_CUDA_ACCELERATION").is_some(),
        ),
        (
            "vulkan-acceleration",
            std::env::var_os("CARGO_FEATURE_VULKAN_ACCELERATION").is_some(),
        ),
        (
            "metal-acceleration",
            std::env::var_os("CARGO_FEATURE_METAL_ACCELERATION").is_some(),
        ),
    ]
    .into_iter()
    .filter_map(|(name, enabled)| enabled.then_some(name))
    .collect::<Vec<_>>();
    assert!(
        enabled.len() <= 1,
        "GPU acceleration features are mutually exclusive: {}",
        enabled.join(", ")
    );
}

fn prepare_macos_native_shims() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let metal_enabled = std::env::var_os("CARGO_FEATURE_METAL_ACCELERATION").is_some();
    let building_worker = std::env::var("SCRIBE_BUILDING_WORKER").ok();
    assert!(
        !metal_enabled || target_os == "macos",
        "metal-acceleration requires a macOS target"
    );
    assert!(
        !metal_enabled || building_worker.as_deref() == Some("1"),
        "metal-acceleration may be linked only into the dedicated worker build"
    );
    if target_os != "macos" {
        return;
    }

    println!("cargo:rerun-if-changed=native/scribe_macos_power_shim.h");
    println!("cargo:rerun-if-changed=native/scribe_macos_power_shim.c");
    let deployment_target = std::env::var("MACOSX_DEPLOYMENT_TARGET")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "13.0".to_owned());
    cc::Build::new()
        .file("native/scribe_macos_power_shim.c")
        .include("native")
        .flag(&format!("-mmacosx-version-min={deployment_target}"))
        .warnings(true)
        .compile("scribe_macos_power_shim");
    println!("cargo:rustc-link-lib=framework=CoreFoundation");
    println!("cargo:rustc-link-lib=framework=IOKit");
    println!("cargo:rustc-link-arg=-mmacosx-version-min={deployment_target}");

    if !metal_enabled {
        return;
    }
    println!("cargo:rerun-if-changed=native/scribe_macos_gpu_shim.h");
    println!("cargo:rerun-if-changed=native/scribe_macos_gpu_shim.m");
    cc::Build::new()
        .file("native/scribe_macos_gpu_shim.m")
        .include("native")
        .flag("-fobjc-arc")
        .flag(&format!("-mmacosx-version-min={deployment_target}"))
        .warnings(true)
        .compile("scribe_macos_gpu_shim");
    println!("cargo:rustc-link-lib=framework=Metal");
    println!("cargo:rustc-link-lib=framework=Foundation");
}

fn emit_bundled_worker_trust_anchor() {
    println!("cargo:rerun-if-env-changed=SCRIBE_BUNDLED_WORKER_SHA256");
    println!("cargo:rerun-if-env-changed=SCRIBE_BUILDING_WORKER");
    let profile = std::env::var("PROFILE").unwrap_or_default();
    let building_worker = std::env::var("SCRIBE_BUILDING_WORKER").ok();
    if building_worker.as_deref().is_some_and(|value| value != "1") {
        panic!("SCRIBE_BUILDING_WORKER, when present, must equal 1");
    }
    let digest = std::env::var("SCRIBE_BUNDLED_WORKER_SHA256").ok();
    if building_worker.as_deref() == Some("1") {
        assert!(
            digest.is_none(),
            "release worker build must clear SCRIBE_BUNDLED_WORKER_SHA256 before compilation"
        );
        return;
    }
    if profile == "release" && digest.is_none() {
        panic!(
            "release desktop build requires SCRIBE_BUNDLED_WORKER_SHA256 from the exact previously built worker"
        );
    }
    let Some(digest) = digest else {
        println!("cargo:warning=development desktop build has no bundled-worker SHA-256 anchor");
        return;
    };
    assert!(
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')),
        "SCRIBE_BUNDLED_WORKER_SHA256 must be a lowercase SHA-256 digest"
    );
    println!("cargo:rustc-env=SCRIBE_BUNDLED_WORKER_SHA256={digest}");
}

fn emit_build_revision() {
    println!("cargo:rerun-if-env-changed=SCRIBE_BUILD_REVISION");
    println!("cargo:rerun-if-changed=.git/HEAD");
    let revision = std::env::var("SCRIBE_BUILD_REVISION")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            Command::new("git")
                .args(["rev-parse", "--verify", "HEAD"])
                .output()
                .ok()
                .filter(|output| output.status.success())
                .and_then(|output| String::from_utf8(output.stdout).ok())
                .map(|value| value.trim().to_owned())
        })
        .unwrap_or_else(|| {
            let mut digest = Sha256::new();
            for path in [
                "Cargo.lock",
                "build.rs",
                "src/onnx_worker.rs",
                "src/worker_contracts.rs",
            ] {
                digest.update(fs::read(path).unwrap_or_default());
            }
            format!("source-{:x}", digest.finalize())
        });
    assert!(
        revision.len() >= 12 && revision.len() <= 96 && revision.is_ascii(),
        "SCRIBE_BUILD_REVISION must be a 12-96 character ASCII build identity"
    );
    println!("cargo:rustc-env=SCRIBE_BUILD_REVISION={revision}");
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
