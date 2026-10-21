//! Installer-only scope check; argv and UID never authenticate an Installer caller.
use anyhow::{Result, bail};
use std::ffi::OsString;

pub(crate) fn validate_invocation(arguments: &[OsString], effective_uid: u32) -> Result<()> {
    if effective_uid != 0 {
        bail!("package entry requires the trusted root installer");
    }
    if !matches!(arguments.len(), 3 | 4) {
        bail!("package entry requires the exact Installer invocation");
    }
    // Package path is unused context, NEVER opened as a source or authority.
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
        bail!("package entry permits only the live boot-volume root");
    }
    Ok(())
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
        validate_invocation(&args(&["/not-a-real-package", "/", "/"]), 0).unwrap();
    }
    #[test]
    fn package_entry_rejects_nonroot_before_interpreting_arguments() {
        for uid in [1, 501, u32::MAX] {
            assert!(
                validate_invocation(&[], uid)
                    .unwrap_err()
                    .to_string()
                    .contains("root installer")
            );
        }
    }
    #[test]
    fn package_entry_rejects_cli_modes_and_argument_ambiguity() {
        for items in [
            vec![],
            vec!["describe"],
            vec!["install", "--source-worker", "/candidate"],
            vec!["install", "--source-pack", "/candidate"],
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
