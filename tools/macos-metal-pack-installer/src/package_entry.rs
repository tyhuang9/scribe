//! Dedicated native Installer entry. The enclosing authenticated PKG/Installer
//! is trusted; argv/UID limit scope but never authenticate the caller.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]
use anyhow::Result;
#[path = "../../../src/macos_installer_invocation.rs"]
mod invocation;

pub(crate) fn compiled_source() -> Result<crate::admission::EmbeddedSource<'static>> {
    crate::admission::admit_embedded([
        include_bytes!(concat!(env!("OUT_DIR"), "/package-manifest.json")),
        include_bytes!(concat!(env!("OUT_DIR"), "/package-signature.json")),
        include_bytes!(concat!(env!("OUT_DIR"), "/package-worker.bin")),
    ])
}
pub(crate) fn run() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
        invocation::validate_invocation(&arguments, unsafe { libc::geteuid() })?;
        let mut operation = crate::native::NativeOperations::production_embedded()?;
        crate::installer_transaction::run_transaction(&mut operation, "Metal")?;
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    anyhow::bail!("Metal package entry is supported only on macOS")
}

#[cfg(test)]
mod tests {
    #[test]
    fn metal_package_empty_compiled_source_denies_without_protected_access() {
        assert!(super::compiled_source().is_err());
    }
}
