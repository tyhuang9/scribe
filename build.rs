use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

const SILERO_VAD_ASSET: &str = "resources/silero-vad/silero_vad.int8.onnx";
const SILERO_VAD_SIZE: usize = 212_860;
const SILERO_VAD_SHA256: &str = "c36d490aff5ab924ca6c7aeec4d8f6bd3d22db6fa17611b9c5b17eae58ac3a20";
const MACOS_KEYCHAIN_NAMESPACE_MANIFEST: &str =
    "runtime-manifests/gpu-keychain-namespace-macos-release.json";
const LINUX_WORKER_INSTALL_CONTRACT: &str =
    "runtime-manifests/linux-worker-install-contract-x86_64.json";
const LINUX_RELEASE_PACKAGE_CONTRACT: &str = "runtime-manifests/linux-release-package-x86_64.json";

fn main() {
    reject_multiple_gpu_features();
    emit_build_revision();
    embed_gpu_pack_release_authority();
    emit_bundled_worker_trust_anchor();
    require_windows_static_crt();
    #[cfg(all(windows, feature = "vulkan-acceleration"))]
    prepare_windows_vulkan_import_library();
    prepare_macos_native_shims();
    verify_linux_worker_install_contract();
    verify_linux_release_package_contract();
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

fn verify_linux_release_package_contract() {
    const EXPECTED: &str = concat!(
        r#"{"schema_version":1,"target":"x86_64-unknown-linux-gnu","package_format":"deb","package_name":"scribe","desktop_path":"usr/bin/local-transcriber","authority_root":"usr/lib/scribe","cpu_worker_path":"usr/lib/scribe/scribe-inference-worker","pack_root":"usr/lib/scribe/workers/packs","catalog_path":"usr/lib/scribe/worker-pack-catalog.json","inventory_path":"usr/lib/scribe/linux-release-inventory.json","production_trust":"empty","gpu_packs":[]}"#,
        "\n"
    );
    println!("cargo:rerun-if-changed={LINUX_RELEASE_PACKAGE_CONTRACT}");
    let contract = fs::read_to_string(LINUX_RELEASE_PACKAGE_CONTRACT)
        .expect("could not read the Linux release package contract");
    assert_eq!(
        contract, EXPECTED,
        "Linux release package contract must remain canonical and production-default-deny"
    );
}

fn verify_linux_worker_install_contract() {
    const EXPECTED: &str = concat!(
        r#"{"schema_version":1,"target":"x86_64-unknown-linux-gnu","desktop_path":"/usr/bin/local-transcriber","authority_root":"/usr/lib/scribe","worker_relative_path":"scribe-inference-worker","future_pack_root":"workers/packs"}"#,
        "\n"
    );
    println!("cargo:rerun-if-changed={LINUX_WORKER_INSTALL_CONTRACT}");
    let contract = fs::read_to_string(LINUX_WORKER_INSTALL_CONTRACT)
        .expect("could not read the Linux worker install contract");
    assert_eq!(
        contract, EXPECTED,
        "Linux worker install contract must preserve the reviewed canonical FHS layout"
    );
}

fn canonical_macos_keychain_access_group(value: &str) -> bool {
    let (team_id, suffix) = value.split_at_checked(10).unwrap_or(("", ""));
    suffix == ".com.scribe.local-transcriber"
        && team_id
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

fn reviewed_macos_keychain_access_group() -> String {
    const PREFIX: &str = r#"{"schema_version":1,"keychain_access_group":""#;
    const SUFFIX: &str = r#""}"#;
    const REVIEWED_NAME: &str = "SCRIBE_REVIEWED_MACOS_GPU_ROLLBACK_KEYCHAIN_ACCESS_GROUP";

    println!("cargo:rerun-if-changed={MACOS_KEYCHAIN_NAMESPACE_MANIFEST}");
    let source = fs::read_to_string(MACOS_KEYCHAIN_NAMESPACE_MANIFEST).unwrap_or_else(|error| {
        panic!("could not read the reviewed macOS Keychain namespace manifest: {error}")
    });
    let canonical = source.strip_suffix('\n').unwrap_or(&source);
    assert!(
        !canonical.ends_with('\r') && !canonical.contains('\n'),
        "reviewed macOS Keychain namespace manifest must be canonical single-line JSON"
    );
    let group = canonical
        .strip_prefix(PREFIX)
        .and_then(|value| value.strip_suffix(SUFFIX))
        .expect("reviewed macOS Keychain namespace manifest has an invalid schema or encoding");
    assert!(
        group.is_empty() || canonical_macos_keychain_access_group(group),
        "reviewed macOS Keychain namespace must be empty or the exact Scribe access group"
    );
    println!("cargo:rustc-env={REVIEWED_NAME}={group}");
    group.to_owned()
}

fn validated_macos_keychain_access_group(reviewed_group: &str) -> Option<String> {
    const NAME: &str = "SCRIBE_MACOS_GPU_ROLLBACK_KEYCHAIN_ACCESS_GROUP";
    println!("cargo:rerun-if-env-changed={NAME}");
    let value = std::env::var(NAME).ok().filter(|value| !value.is_empty())?;
    assert!(
        canonical_macos_keychain_access_group(&value),
        "{NAME} must match TEAMID.com.scribe.local-transcriber with a 10-character uppercase alphanumeric Team ID"
    );
    assert!(
        !reviewed_group.is_empty() && value == reviewed_group,
        "{NAME} must exactly match the non-empty source-reviewed macOS Keychain namespace"
    );
    println!("cargo:rustc-env={NAME}={value}");
    Some(value)
}

fn embed_gpu_pack_release_authority() {
    const DEFAULT_AUTHORITY: &str = "runtime-manifests/gpu-pack-release-authority-macos-empty.json";
    const MAX_AUTHORITY_BYTES: usize = 512 * 1024;

    println!("cargo:rerun-if-env-changed=SCRIBE_GPU_PACK_RELEASE_AUTHORITY");
    println!("cargo:rerun-if-changed={DEFAULT_AUTHORITY}");
    let source = std::env::var_os("SCRIBE_GPU_PACK_RELEASE_AUTHORITY")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_AUTHORITY));
    if source.as_path() != Path::new(DEFAULT_AUTHORITY) {
        println!("cargo:rerun-if-changed={}", source.display());
    }
    let mut bytes = fs::read(&source).unwrap_or_else(|error| {
        panic!(
            "could not read the GPU pack release authority {}: {error}",
            source.display()
        )
    });
    assert!(
        !bytes.is_empty() && bytes.len() <= MAX_AUTHORITY_BYTES,
        "GPU pack release authority is empty or exceeds its build-time bound"
    );
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    assert!(
        !bytes.is_empty() && bytes.last() != Some(&b'\r') && std::str::from_utf8(&bytes).is_ok(),
        "GPU pack release authority must be canonical UTF-8 JSON with at most one trailing LF"
    );
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("Cargo must set OUT_DIR"));
    fs::write(
        out_dir.join("scribe_gpu_pack_release_authority.json"),
        bytes,
    )
    .expect("could not embed the GPU pack release authority");
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
    let reviewed_keychain_access_group = reviewed_macos_keychain_access_group();
    let _keychain_access_group =
        validated_macos_keychain_access_group(&reviewed_keychain_access_group);
    println!("cargo:rustc-check-cfg=cfg(scribe_macos_keychain_authority)");
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
        .flag(format!("-mmacosx-version-min={deployment_target}"))
        .warnings(true)
        .compile("scribe_macos_power_shim");
    println!("cargo:rustc-link-lib=framework=CoreFoundation");
    println!("cargo:rustc-link-lib=framework=IOKit");
    println!("cargo:rustc-link-arg=-mmacosx-version-min={deployment_target}");

    if building_worker.as_deref() != Some("1") {
        println!("cargo:rerun-if-changed=native/scribe_macos_keychain_epoch.h");
        println!("cargo:rerun-if-changed=native/scribe_macos_keychain_epoch.c");
        cc::Build::new()
            .file("native/scribe_macos_keychain_epoch.c")
            .include("native")
            .flag(format!("-mmacosx-version-min={deployment_target}"))
            .warnings(true)
            .compile("scribe_macos_keychain_epoch");
        println!("cargo:rustc-cfg=scribe_macos_keychain_authority");
        println!("cargo:rustc-link-lib=framework=Security");
    }

    if !metal_enabled {
        return;
    }
    println!("cargo:rerun-if-changed=native/scribe_macos_gpu_shim.h");
    println!("cargo:rerun-if-changed=native/scribe_macos_gpu_shim.m");
    cc::Build::new()
        .file("native/scribe_macos_gpu_shim.m")
        .include("native")
        .flag("-fobjc-arc")
        .flag(format!("-mmacosx-version-min={deployment_target}"))
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
