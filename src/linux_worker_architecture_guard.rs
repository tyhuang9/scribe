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
    let production = launcher.split("#[cfg(test)]\nmod tests").next().unwrap();
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
            !production.contains(forbidden),
            "production Linux launcher contains forbidden fallback: {forbidden}"
        );
    }
}

#[test]
fn manifest_preserves_fhs_paths_and_excludes_desktop_from_authority() {
    let manifest = include_str!("../runtime-manifests/linux-worker-install-contract-x86_64.json");
    let build = include_str!("../build.rs");
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
    assert!(build.contains("verify_linux_worker_install_contract();"));
    assert!(build.contains("fn verify_linux_worker_install_contract()"));
    assert!(
        build.contains(
            "Linux worker install contract must preserve the reviewed canonical FHS layout"
        )
    );
}

#[test]
fn desktop_and_worker_route_only_linux_inference_authority_to_execveat() {
    let main = include_str!("main.rs");
    let worker_entrypoint = include_str!("bin/scribe-inference-worker.rs");
    let supervisor = include_str!("onnx_worker.rs");
    let packs = include_str!("gpu_worker_pack/mod.rs");

    let exact_cfg = "all(target_os = \"linux\", target_arch = \"x86_64\", target_env = \"gnu\")";
    assert!(main.contains(exact_cfg));
    assert!(main.contains("mod linux_worker_launch;"));
    assert!(worker_entrypoint.contains(exact_cfg));
    assert!(worker_entrypoint.contains("onnx_worker::validate_linux_worker_entrypoint()"));

    let launch = supervisor
        .split("impl WorkerLauncher for OsWorkerLauncher")
        .nth(1)
        .unwrap()
        .split("fn bind_worker_process_tree_or_terminate")
        .next()
        .unwrap();
    let descriptor_branch = launch
        .find("crate::linux_worker_launch::launch_verified_worker")
        .unwrap();
    let missing_authority_denial = launch
        .find("Linux inference worker resolved without descriptor launch authority")
        .unwrap();
    let generic_command = launch.find("Command::new").unwrap();
    assert!(descriptor_branch < missing_authority_denial);
    assert!(missing_authority_denial < generic_command);
    assert!(launch[..generic_command].contains("return Ok(SpawnedWorker"));
    assert!(supervisor.contains("InstalledWorkerAuthority::open_production"));
    assert!(supervisor.contains("option_env!(\"SCRIBE_BUNDLED_WORKER_SHA256\")"));

    let adapter_index = packs
        .find("impl crate::linux_worker_launch::LinuxExecAuthority")
        .unwrap();
    let adapter_cfg = &packs[adapter_index.saturating_sub(180)..adapter_index];
    assert!(adapter_cfg.contains("test,"));
    assert!(adapter_cfg.contains("target_os = \"linux\""));
    assert!(packs.contains("pub(crate) fn production_registry() -> ProductionPackRegistry {\n    ProductionPackRegistry::empty()"));
}
