#[cfg(not(feature = "installer-package-entry"))]
use std::fs::File;
#[cfg(not(feature = "installer-package-entry"))]
use std::io::Read;
use std::path::PathBuf;

#[allow(dead_code, reason = "only the shared revision predicate is needed")]
#[path = "../../build_support/build_revision.rs"]
mod build_revision;
#[cfg(feature = "installer-package-entry")]
#[path = "src/package_snapshot.rs"]
mod package_snapshot;

fn main() {
    println!("cargo:rerun-if-changed=../../Cargo.toml");
    println!("cargo:rerun-if-changed=../../build_support/build_revision.rs");
    // Shared worker identities use CARGO_PKG_VERSION. Fail closed if this
    // isolated tool drifts from the application instead of approving its own
    // unrelated version. Unsupported manifest syntax also fails closed.
    let versions = include_str!("../../Cargo.toml")
        .lines()
        .skip_while(|line| line.trim() != "[package]")
        .skip(1)
        .take_while(|line| !line.trim().starts_with('['))
        .filter_map(|line| line.trim().strip_prefix("version = "))
        .map(|value| serde_json::from_str::<String>(value).expect("literal application version"))
        .collect::<Vec<_>>();
    assert_eq!(versions.as_slice(), [env!("CARGO_PKG_VERSION")]);

    println!("cargo:rerun-if-env-changed=SCRIBE_BUILD_REVISION");
    let revision = match std::env::var("SCRIBE_BUILD_REVISION") {
        Ok(value) if build_revision::is_valid_build_revision(&value) => value,
        Err(std::env::VarError::NotPresent)
            if std::env::var("PROFILE").as_deref() != Ok("release") =>
        {
            "source-installer-admission".to_owned()
        }
        _ => panic!("Metal installer requires a valid compiled SCRIBE_BUILD_REVISION"),
    };
    println!("cargo:rustc-env=SCRIBE_BUILD_REVISION={revision}");

    #[cfg(feature = "installer-package-entry")]
    let mut release_inputs = Vec::new();
    for (variable, output, default) in [
        (
            "SCRIBE_GPU_PACK_CATALOG",
            "catalog.json",
            b"{\"schema_version\":1,\"packs\":[]}".as_slice(),
        ),
        (
            "SCRIBE_GPU_PACK_RELEASE_AUTHORITY",
            "authority.json",
            include_bytes!("../../runtime-manifests/gpu-pack-release-authority-macos-empty.json")
                .as_slice(),
        ),
    ] {
        println!("cargo:rerun-if-env-changed={variable}");
        let bytes = if let Some(path) = std::env::var_os(variable) {
            let path = PathBuf::from(path);
            #[cfg(feature = "installer-package-entry")]
            package_snapshot::validate_path(&path).expect("invalid Metal release build path");
            println!("cargo:rerun-if-changed={}", path.display());
            #[cfg(feature = "installer-package-entry")]
            let bytes = package_snapshot::read_input(&path, 512 * 1024)
                .expect("unsafe Metal release build input");
            #[cfg(not(feature = "installer-package-entry"))]
            let bytes = {
                let mut bytes = Vec::new();
                File::open(path)
                    .expect("read compiled release input")
                    .take(512 * 1024 + 1)
                    .read_to_end(&mut bytes)
                    .expect("read bounded compiled release input");
                assert!(
                    bytes.len() <= 512 * 1024,
                    "compiled release input too large"
                );
                bytes
            };
            bytes
        } else {
            default.to_vec()
        };
        // Match the application's embedding convention: one terminal LF only.
        let bytes = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
        std::fs::write(
            PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join(output),
            bytes,
        )
        .expect("freeze compiled release input");
        #[cfg(feature = "installer-package-entry")]
        release_inputs.push(bytes.to_vec());
    }
    println!(
        "cargo:rerun-if-changed=../../runtime-manifests/gpu-pack-release-authority-macos-empty.json"
    );
    println!("cargo:rerun-if-env-changed=SCRIBE_MACOS_METAL_PACKAGE_PACK");
    println!("cargo:rerun-if-env-changed=SCRIBE_MACOS_METAL_PACKAGE_MANIFEST_SHA256");
    #[cfg(feature = "installer-package-entry")]
    emit_package(&release_inputs);
}

#[cfg(feature = "installer-package-entry")]
fn emit_package(release_inputs: &[Vec<u8>]) {
    use std::{env, fs};
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo output required"));
    let stage = output.join(format!("metal-package-{}", std::process::id()));
    if let Some(pack) = env::var_os("SCRIBE_MACOS_METAL_PACKAGE_PACK") {
        for name in [
            "SCRIBE_GPU_PACK_CATALOG",
            "SCRIBE_GPU_PACK_RELEASE_AUTHORITY",
        ] {
            assert!(
                env::var_os(name).is_some(),
                "Metal package entry requires explicit release inputs"
            );
        }
        package_snapshot::require_release_inputs(&release_inputs[0], &release_inputs[1])
            .expect("Metal package release input rejected");
        let pack = PathBuf::from(pack);
        package_snapshot::validate_path(&pack).expect("invalid Metal package build path");
        println!("cargo:rerun-if-changed={}", pack.display());
        for relative in [
            "pack-manifest.json",
            "pack-manifest.sig",
            "worker/scribe-inference-worker",
        ] {
            println!("cargo:rerun-if-changed={}", pack.join(relative).display());
        }
        let digest = env::var("SCRIBE_MACOS_METAL_PACKAGE_MANIFEST_SHA256")
            .expect("Metal package requires the frozen manifest SHA-256");
        let arch = env::var("CARGO_CFG_TARGET_ARCH").expect("Cargo target architecture required");
        package_snapshot::snapshot(&pack, &stage, &digest, &arch)
            .expect("Metal package snapshot rejected");
    } else {
        assert!(
            env::var("PROFILE").as_deref() != Ok("release"),
            "release Metal package entry requires SCRIBE_MACOS_METAL_PACKAGE_PACK"
        );
        fs::create_dir(&stage).expect("fresh source-test package stage required");
        for name in package_snapshot::OUTPUTS {
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(stage.join(name))
                .expect("empty source-test payload required");
        }
    }
    // All snapshots validated before publication. A partial output rename aborts
    // compilation, never falls back to stale outputs. Cargo output is trusted.
    for name in package_snapshot::OUTPUTS {
        fs::rename(stage.join(name), output.join(name))
            .expect("compiled Metal payload publication failed");
    }
    fs::remove_dir(stage).expect("remove empty invocation-owned build stage");
}
