#[path = "../build_support/windows_application_manifest.rs"]
mod windows_application_manifest;

use std::path::{Path, PathBuf};
use windows_application_manifest::{MANIFEST_PATH, MANIFEST_SHA256, cargo_directives};

fn absolute_path(suffix: &str) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(format!("C:/source/{suffix}"))
    } else {
        PathBuf::from(format!("/source/{suffix}"))
    }
}

#[test]
fn both_windows_binaries_embed_id_one_independent_of_provider_features() {
    let path = absolute_path("with spaces/用户/application.manifest");
    let directives = cargo_directives("windows", "msvc", &path).unwrap();
    assert_eq!(directives.len(), 6);
    for binary in ["local-transcriber", "scribe-inference-worker"] {
        assert!(directives.contains(&format!(
            "cargo:rustc-link-arg-bin={binary}=/MANIFEST:EMBED,ID=1"
        )));
        assert!(directives.contains(&format!(
            "cargo:rustc-link-arg-bin={binary}=/MANIFESTINPUT:{}",
            path.display()
        )));
    }
    assert!(directives.iter().all(|line| !line.contains("link-arg=")));
}

#[test]
fn other_targets_never_receive_msvc_link_flags_or_path_restrictions() {
    for (os, abi) in [("linux", "gnu"), ("macos", ""), ("windows", "gnu")] {
        let directives = cargo_directives(os, abi, Path::new("relative\npath")).unwrap();
        assert_eq!(directives.len(), 2);
        assert!(
            directives
                .iter()
                .all(|line| line.starts_with("cargo:rerun-if-changed="))
        );
    }
}

#[test]
fn manifest_and_helper_changes_invalidate_cargo() {
    let directives =
        cargo_directives("windows", "msvc", &absolute_path("application.manifest")).unwrap();
    assert!(directives.contains(&format!("cargo:rerun-if-changed={MANIFEST_PATH}")));
    assert!(directives.contains(
        &"cargo:rerun-if-changed=build_support/windows_application_manifest.rs".to_owned()
    ));
    use sha2::{Digest, Sha256};
    assert_eq!(
        format!(
            "{:x}",
            Sha256::digest(include_bytes!("../resources/windows/application.manifest"))
        ),
        MANIFEST_SHA256
    );
}

#[test]
fn relative_controls_quotes_and_verbatim_paths_are_rejected() {
    assert!(cargo_directives("windows", "msvc", Path::new("application.manifest")).is_err());
    for suffix in [
        "bad\npath",
        "bad\rpath",
        "bad\0path",
        "bad\tpath",
        "bad\"path",
    ] {
        assert!(cargo_directives("windows", "msvc", &absolute_path(suffix)).is_err());
    }
    assert!(
        cargo_directives("windows", "msvc", Path::new(r"\\?\C:\application.manifest")).is_err()
    );
}

#[test]
fn input_path_bound_counts_utf16_and_reserves_the_terminator() {
    let prefix = absolute_path("");
    let prefix_units = prefix.to_str().unwrap().encode_utf16().count();
    let at_limit = PathBuf::from(format!(
        "{}{}",
        prefix.display(),
        "x".repeat(259 - prefix_units)
    ));
    assert!(cargo_directives("windows", "msvc", &at_limit).is_ok());
    let too_long = PathBuf::from(format!("{}x", at_limit.display()));
    assert!(cargo_directives("windows", "msvc", &too_long).is_err());
    // A non-BMP character occupies two Windows code units, not one.
    let non_bmp = PathBuf::from(format!(
        "{}{}🦀",
        prefix.display(),
        "x".repeat(258 - prefix_units)
    ));
    assert_eq!(non_bmp.to_str().unwrap().encode_utf16().count(), 260);
    assert!(cargo_directives("windows", "msvc", &non_bmp).is_err());
}

#[cfg(unix)]
#[test]
fn non_unicode_path_cannot_be_emitted_as_a_cargo_directive() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    let path = PathBuf::from(OsString::from_vec(b"/source/\xff.manifest".to_vec()));
    assert!(cargo_directives("windows", "msvc", &path).is_err());
}

#[cfg(windows)]
#[test]
fn unpaired_windows_surrogate_cannot_be_emitted_as_a_cargo_directive() {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    let mut units: Vec<u16> = "C:/source/".encode_utf16().collect();
    units.push(0xd800);
    let path = PathBuf::from(OsString::from_wide(&units));
    assert!(cargo_directives("windows", "msvc", &path).is_err());
}
