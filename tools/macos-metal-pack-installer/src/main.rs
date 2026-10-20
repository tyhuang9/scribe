//! Installer-source admission only. No install command, elevation, or runtime caller.
use anyhow::{Result, bail};

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

fn run() -> Result<()> {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    run_arguments(&arguments)
}

fn run_arguments(arguments: &[std::ffi::OsString]) -> Result<()> {
    if arguments == ["describe"] {
        let release = admission::compiled_release()?;
        println!(
            "{}",
            serde_json::json!({
                "schemaVersion": 1,
                "component": "protected-metal-source-admission",
                "appVersion": env!("CARGO_PKG_VERSION"),
                "buildRevision": env!("SCRIBE_BUILD_REVISION"),
                "releaseSecurityEpoch": release.release_security_epoch,
                "releaseEntryCount": release.entries.len(),
                "installsApplication": false,
                "writesProtectedStore": false,
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
    bail!(
        "usage: scribe-macos-metal-pack-installer describe | verify-source --source-pack ABSOLUTE_PATH"
    )
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn metal_admission_cli_rejects_install_and_runtime_authority_options() {
        for arguments in [
            vec![],
            vec!["install", "--source-pack", "/candidate"],
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
}
