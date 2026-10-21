//! Dedicated native Installer operation. Argv limits scope, not caller trust.
//! Authentication of this exact executable remains signed-PKG/Installer TCB.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[cfg(not(target_os = "macos"))]
use anyhow::bail;
use anyhow::{Context, Result};
#[path = "../../../src/macos_installer_invocation.rs"]
mod invocation;

const IMAGE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/package-worker.bin"));

pub(crate) fn compiled_worker() -> Result<&'static [u8]> {
    let digest = option_env!("SCRIBE_BUNDLED_WORKER_SHA256")
        .context("CPU package entry has no compiled image anchor")?;
    option_env!("SCRIBE_BUILD_REVISION").context("CPU package entry has no compiled revision")?;
    crate::package_payload::verify(IMAGE, digest)?;
    Ok(IMAGE)
}

pub(crate) fn run() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
        invocation::validate_invocation(&arguments, unsafe { libc::geteuid() })?;
        let mut operation = crate::native::NativeOperations::production_embedded()?;
        crate::transaction::install(&mut operation)?;
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    bail!("CPU package entry is supported only on macOS")
}
