//! Installer-owned source component; never a runtime elevation service.
#[cfg(not(feature = "installer-package-entry"))]
use anyhow::{Result, bail};

#[cfg(target_os = "macos")]
#[allow(dead_code, reason = "CPU retains its literal ancestor policy wrapper")]
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
    reason = "shared read-only policy consumers use different primitive subsets"
)]
#[path = "../../../src/macos_worker_fs.rs"]
mod macos_worker_fs;
#[allow(
    dead_code,
    reason = "shared fixed CPU layout includes runtime-only helpers"
)]
#[path = "../../../src/macos_worker_layout.rs"]
mod macos_worker_layout;
#[cfg(target_os = "macos")]
mod native;
#[cfg(feature = "installer-package-entry")]
mod package_entry;
#[cfg(feature = "installer-package-entry")]
mod package_payload;
#[cfg(all(test, feature = "installer-package-entry"))]
mod package_snapshot;
mod transaction;

#[cfg(not(feature = "installer-package-entry"))]
fn run() -> Result<()> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if arguments.as_slice() == ["describe"] {
        println!(
            "{}",
            serde_json::json!({
                "schemaVersion": 1,
                "component": "protected-cpu-writer",
                "cpuRoot": macos_worker_layout::CPU_ROOT,
                "workerName": macos_worker_layout::CPU_WORKER_NAME,
                "workerSha256": option_env!("SCRIBE_BUNDLED_WORKER_SHA256"),
                "buildRevision": option_env!("SCRIBE_BUILD_REVISION"),
                "installsApplication": false,
            })
        );
        return Ok(());
    }
    if arguments.len() == 3 && arguments[0] == "install" && arguments[1] == "--source-worker" {
        #[cfg(target_os = "macos")]
        {
            let mut operations =
                native::NativeOperations::production(std::path::Path::new(&arguments[2]))?;
            let outcome = transaction::install(&mut operations)?;
            println!("{outcome:?}");
            return Ok(());
        }
        #[cfg(not(target_os = "macos"))]
        bail!("protected CPU installation is supported only on macOS");
    }
    bail!(
        "usage: scribe-macos-cpu-worker-installer describe | install --source-worker ABSOLUTE_PATH"
    )
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
