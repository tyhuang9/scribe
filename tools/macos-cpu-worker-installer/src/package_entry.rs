//! Dedicated native Installer operation. Argv limits scope, not caller trust.
//! Authentication of this exact executable remains signed-PKG/Installer TCB.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]
use anyhow::{Context, Result, bail};
use std::ffi::OsString;

const IMAGE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/package-worker.bin"));

fn validate_invocation(arguments: &[OsString], effective_uid: u32) -> Result<()> {
    if effective_uid != 0 {
        bail!("CPU package entry requires the trusted root installer");
    }
    if !matches!(arguments.len(), 3 | 4) {
        bail!("CPU package entry requires the exact Installer invocation");
    }
    // Package path is unused diagnostic context, NEVER a source or authority.
    let package = arguments[0].as_encoded_bytes();
    if package.len() < 2
        || package.len() > 4096
        || package[0] != b'/'
        || package.ends_with(b"/")
        || package.iter().any(|b| b.is_ascii_control())
        || package[1..]
            .split(|b| *b == b'/')
            .any(|part| part.is_empty() || matches!(part, b"." | b".."))
        || arguments[1] != "/"
        || arguments[2] != "/"
        || arguments.get(3).is_some_and(|root| root != "/")
    {
        bail!("CPU package entry permits only the live boot-volume root");
    }
    Ok(())
}

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
        validate_invocation(&arguments, unsafe { libc::geteuid() })?;
        let mut operation = crate::native::NativeOperations::production_embedded()?;
        crate::transaction::install(&mut operation)?;
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    bail!("CPU package entry is supported only on macOS")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(items: &[&str]) -> Vec<OsString> {
        items.iter().map(OsString::from).collect()
    }
    #[test]
    fn package_entry_accepts_only_documented_live_volume_context() {
        validate_invocation(&args(&["/private/installer/Scribe.pkg", "/", "/"]), 0).unwrap();
        validate_invocation(
            &args(&["/Volumes/Install Media/Scribe.pkg", "/", "/", "/"]),
            0,
        )
        .unwrap();
        // No package lookup occurs: this path deliberately does not exist.
        validate_invocation(&args(&["/not-a-real-package", "/", "/"]), 0).unwrap();
    }
    #[test]
    fn package_entry_rejects_nonroot_before_interpreting_arguments() {
        for uid in [1, 501, u32::MAX] {
            let error = validate_invocation(&[], uid).unwrap_err();
            assert!(error.to_string().contains("root installer"));
        }
    }
    #[test]
    fn package_entry_rejects_cli_modes_and_argument_ambiguity() {
        for items in [
            vec![],
            vec!["describe"],
            vec!["install", "--source-worker", "/candidate"],
            vec!["/package", "/", "/", "/", "extra"],
            vec!["/package", "/"],
            vec!["/package", "/", "/", "--destination"],
            vec!["/package", "--digest", "/"],
        ] {
            assert!(validate_invocation(&args(&items), 0).is_err());
        }
    }
    #[test]
    fn package_entry_rejects_aliases_and_nonboot_destinations() {
        for package in [
            "relative.pkg",
            "/",
            "//package",
            "/a/../package",
            "/a/./package",
            "/a//package",
            "/a/package/",
            "/a/line\nbreak",
        ] {
            assert!(validate_invocation(&args(&[package, "/", "/"]), 0).is_err());
        }
        for position in 1..4 {
            for path in [
                "",
                "/Volumes/Other",
                "/System/Volumes/Data",
                "/Applications",
                "/./",
                "//",
            ] {
                let mut items = vec!["/package", "/", "/", "/"];
                items[position] = path;
                assert!(validate_invocation(&args(&items), 0).is_err());
            }
        }
        assert!(
            validate_invocation(&args(&[&format!("/{}", "a".repeat(4096)), "/", "/"]), 0).is_err()
        );
    }
}
