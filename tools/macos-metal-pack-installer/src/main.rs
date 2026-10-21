//! Installer-owned copy-only source. No elevation, activation, or runtime caller.
#[cfg(any(not(feature = "installer-package-entry"), target_os = "macos", test))]
use anyhow::{Result, bail};

#[cfg(target_os = "macos")]
#[allow(dead_code, reason = "Metal consumes only admitted ancestor mechanics")]
#[path = "../../../src/macos_installer_bootstrap.rs"]
mod installer_bootstrap;
#[cfg(target_os = "macos")]
#[allow(
    dead_code,
    reason = "shared installer primitives have CPU and Metal consumers"
)]
#[path = "../../../src/macos_installer_fs.rs"]
mod installer_fs;
#[path = "../../../src/macos_installer_transaction.rs"]
mod installer_transaction;

#[allow(
    dead_code,
    reason = "shared verifier includes runtime and other-platform APIs"
)]
#[path = "../../../src/gpu_worker_pack/manifest.rs"]
mod manifest;
#[path = "../../../src/gpu_worker_pack/release_authority.rs"]
mod release_authority;
#[allow(
    dead_code,
    reason = "shared verifier uses only compiled GPU compatibility"
)]
#[path = "../../../src/worker_compatibility.rs"]
mod worker_compatibility;
#[path = "../../../src/worker_identity.rs"]
mod worker_identity;
mod onnx_worker {
    pub(crate) use crate::worker_identity::{
        DESKTOP_BUILD_ID, INFERENCE_WORKER_BUILD_ID, PROTOCOL_VERSION, WORKER_ABI_VERSION,
    };
}
mod admission;
#[cfg(target_os = "macos")]
#[allow(
    dead_code,
    reason = "admission uses only shared descriptor and directory primitives"
)]
#[path = "../../../src/macos_worker_fs.rs"]
mod macos_worker_fs;
#[cfg(target_os = "macos")]
mod native;
#[cfg(feature = "installer-package-entry")]
mod package_entry;
#[cfg(all(test, feature = "installer-package-entry"))]
mod package_snapshot;

#[cfg(not(feature = "installer-package-entry"))]
fn run() -> Result<()> {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    run_arguments(&arguments)
}

#[cfg(not(feature = "installer-package-entry"))]
fn run_arguments(arguments: &[std::ffi::OsString]) -> Result<()> {
    if arguments == ["describe"] {
        let release = admission::compiled_release()?;
        println!(
            "{}",
            serde_json::json!({
                "schemaVersion": 1,
                "component": "protected-metal-writer",
                "appVersion": env!("CARGO_PKG_VERSION"),
                "buildRevision": env!("SCRIBE_BUILD_REVISION"),
                "releaseSecurityEpoch": release.release_security_epoch,
                "releaseEntryCount": release.entries.len(),
                "installsApplication": false,
                "supportsProtectedCopy": true,
            })
        );
        return Ok(());
    }
    if arguments.len() == 3 && arguments[0] == "verify-source" && arguments[1] == "--source-pack" {
        #[cfg(target_os = "macos")]
        {
            let admitted = admission::admit_source(std::path::Path::new(&arguments[2]))?;
            admitted.reverify()?;
            println!("Release-included Metal source verified; no files installed.");
            return Ok(());
        }
        #[cfg(not(target_os = "macos"))]
        bail!("production Metal source admission is supported only on macOS");
    }
    if arguments.len() == 3 && arguments[0] == "install" && arguments[1] == "--source-pack" {
        #[cfg(target_os = "macos")]
        {
            let mut operation =
                native::NativeOperations::production(std::path::Path::new(&arguments[2]))?;
            let outcome = installer_transaction::run_transaction(&mut operation, "Metal")?;
            println!("{outcome:?}");
            return Ok(());
        }
        #[cfg(not(target_os = "macos"))]
        bail!("protected Metal installation is supported only on macOS");
    }
    bail!(
        "usage: scribe-macos-metal-pack-installer describe | verify-source --source-pack ABSOLUTE_PATH | install --source-pack ABSOLUTE_PATH"
    )
}

#[cfg(any(target_os = "macos", test))]
fn require_installer_root(effective_uid: u32) -> Result<()> {
    if effective_uid != 0 {
        bail!("Metal worker installation requires the trusted root installer");
    }
    Ok(())
}

fn main() {
    #[cfg(feature = "installer-package-entry")]
    let result = package_entry::run();
    #[cfg(not(feature = "installer-package-entry"))]
    let result = run();
    if let Err(error) = result {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[cfg(not(feature = "installer-package-entry"))]
    fn metal_admission_cli_rejects_invalid_install_and_runtime_authority_options() {
        for arguments in [
            vec![],
            vec!["install", "--source-worker", "/candidate"],
            vec!["describe", "--release-authority", "/authority"],
            vec!["verify-source", "--trust-root", "/key"],
            vec![
                "verify-source",
                "--source-pack",
                "/candidate",
                "--epoch",
                "1",
            ],
            vec![
                "verify-source",
                "--source-pack",
                "/candidate",
                "--destination",
                "/root",
            ],
        ] {
            let arguments = arguments
                .into_iter()
                .map(std::ffi::OsString::from)
                .collect::<Vec<_>>();
            assert!(super::run_arguments(&arguments).is_err());
        }
    }
    #[test]
    fn metal_install_requires_root_without_touching_a_candidate() {
        assert!(super::require_installer_root(0).is_ok());
        for uid in [1, 501, u32::MAX] {
            assert!(super::require_installer_root(uid).is_err());
        }
    }
}
