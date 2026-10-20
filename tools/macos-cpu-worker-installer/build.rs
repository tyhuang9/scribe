#[allow(
    dead_code,
    reason = "build validation uses only the shared revision predicate"
)]
#[path = "../../build_support/build_revision.rs"]
mod build_revision;
#[allow(
    dead_code,
    reason = "build validation uses only the shared CPU digest predicate"
)]
#[path = "../../src/macos_worker_layout.rs"]
mod macos_worker_layout;
#[cfg(feature = "installer-package-entry")]
#[path = "src/package_payload.rs"]
mod package_payload;
#[cfg(feature = "installer-package-entry")]
#[path = "src/package_snapshot.rs"]
mod package_snapshot;

fn main() {
    println!("cargo:rerun-if-changed=../../src/macos_worker_layout.rs");
    println!("cargo:rerun-if-changed=../../build_support/build_revision.rs");
    for name in ["SCRIBE_BUNDLED_WORKER_SHA256", "SCRIBE_BUILD_REVISION"] {
        println!("cargo:rerun-if-env-changed={name}");
        match std::env::var(name) {
            Ok(value) => {
                let valid = if name == "SCRIBE_BUNDLED_WORKER_SHA256" {
                    macos_worker_layout::validate_cpu_digest(&value).is_ok()
                } else {
                    build_revision::is_valid_build_revision(&value)
                };
                assert!(valid, "invalid compiled installer identity: {name}");
                println!("cargo:rustc-env={name}={value}");
            }
            Err(std::env::VarError::NotPresent)
                if std::env::var("PROFILE").as_deref() != Ok("release") => {}
            Err(_) => panic!("release installer requires compiled identity: {name}"),
        }
    }
    println!("cargo:rerun-if-env-changed=SCRIBE_MACOS_CPU_PACKAGE_WORKER");
    #[cfg(feature = "installer-package-entry")]
    emit_package_worker();
}

#[cfg(feature = "installer-package-entry")]
fn emit_package_worker() {
    use std::{env, fs, path::PathBuf};
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo output required"));
    let temporary = output.join(format!("package-worker-{}.tmp", std::process::id()));
    let destination = output.join("package-worker.bin");
    if let Some(source) = env::var_os("SCRIBE_MACOS_CPU_PACKAGE_WORKER") {
        let source = PathBuf::from(source);
        package_snapshot::validate_source_path(&source).expect("invalid package worker build path");
        println!("cargo:rerun-if-changed={}", source.display());
        let digest = env::var("SCRIBE_BUNDLED_WORKER_SHA256")
            .expect("package worker requires the compiled final-image anchor");
        env::var("SCRIBE_BUILD_REVISION").expect("package worker requires the compiled revision");
        package_snapshot::snapshot(&source, &temporary, &digest)
            .expect("CPU package worker snapshot rejected");
    } else {
        assert!(
            env::var("PROFILE").as_deref() != Ok("release"),
            "release package entry requires SCRIBE_MACOS_CPU_PACKAGE_WORKER"
        );
        // Source tests may compile without a real image. This can never pass
        // runtime admission, and release compilation forbids this case.
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .expect("could not create empty source-test payload");
    }
    // Cargo's output directory and compiler are trusted build inputs. Replace
    // only this generated output; compilation stops on every snapshot failure.
    fs::rename(&temporary, &destination).expect("could not publish compiled worker snapshot");
}
