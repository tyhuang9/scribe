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
}
