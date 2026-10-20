use std::fs::File;
use std::io::Read;
use std::path::PathBuf;

#[allow(dead_code, reason = "only the shared revision predicate is needed")]
#[path = "../../build_support/build_revision.rs"]
mod build_revision;

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
            println!("cargo:rerun-if-changed={}", path.display());
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
    }
    println!(
        "cargo:rerun-if-changed=../../runtime-manifests/gpu-pack-release-authority-macos-empty.json"
    );
}
