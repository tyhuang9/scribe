//! Source-level guard for the Linux-only descriptor-bound worker boundary.

#[test]
fn linux_launcher_is_exactly_scoped_and_has_no_path_spawn_fallback() {
    let launcher = include_str!("linux_worker_launch.rs");
    assert!(launcher.contains(
        "#![cfg(all(target_os = \"linux\", target_arch = \"x86_64\", target_env = \"gnu\"))]"
    ));
    for required in [
        "pub(crate) const INSTALL_ROOT: &str = \"/usr/lib/scribe\"",
        "pub(crate) const WORKER_NAME: &str = \"scribe-inference-worker\"",
        "const RESOLVE_NO_MAGICLINKS: u64 = 0x02",
        "const RESOLVE_NO_SYMLINKS: u64 = 0x04",
        "const RESOLVE_BENEATH: u64 = 0x08",
        "RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS",
        "const SYS_EXECVEAT: c_long = 322",
        "const SYS_CLOSE_RANGE: c_long = 436",
        "const SYS_OPENAT2: c_long = 437",
        "AT_EMPTY_PATH",
        "PR_SET_PDEATHSIG",
        "PR_SET_NO_NEW_PRIVS",
        "setpgid(0, 0)",
        "getppid() != parent_pid",
        "fchdir(ROOT_FD)",
        "SYS_CLOSE_RANGE",
        "SYS_EXECVEAT",
        "Linux worker exec handshake timed out",
        "cleanup_failed_child(pid)",
    ] {
        assert!(
            launcher.contains(required),
            "missing Linux launcher token: {required}"
        );
    }
    for forbidden in [
        "Command::new",
        ".spawn()",
        "std::fs::canonicalize",
        "std::env::current_exe",
        "SCRIBE_LINUX_WORKER_ROOT",
        "SCRIBE_LINUX_WORKER_PATH",
        "/opt/scribe",
    ] {
        assert!(
            !launcher[..launcher.find("#[cfg(test)]").unwrap()].contains(forbidden),
            "production Linux launcher contains forbidden fallback: {forbidden}"
        );
    }
}

#[test]
fn manifest_preserves_fhs_paths_and_excludes_desktop_from_authority() {
    let manifest = include_str!("../runtime-manifests/linux-worker-install-contract-x86_64.json");
    assert_eq!(
        manifest.trim_end(),
        "{\"schema_version\":1,\"target\":\"x86_64-unknown-linux-gnu\",\"desktop_path\":\"/usr/bin/local-transcriber\",\"authority_root\":\"/usr/lib/scribe\",\"worker_relative_path\":\"scribe-inference-worker\",\"future_pack_root\":\"workers/packs\"}"
    );
    let launcher = include_str!("linux_worker_launch.rs");
    let production = launcher
        .split("pub(crate) fn open_production")
        .nth(1)
        .unwrap()
        .split("#[cfg(test)]")
        .next()
        .unwrap();
    assert!(production.contains("File::open(\"/\")"));
    assert!(production.contains("INSTALL_COMPONENTS"));
    assert!(!production.contains("DESKTOP_PATH"));
    assert!(!production.contains("std::env"));
}
