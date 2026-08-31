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
        "const SYS_MEMFD_CREATE: c_long = 319",
        "const SYS_CLOSE_RANGE: c_long = 436",
        "const SYS_OPENAT2: c_long = 437",
        "MFD_CLOEXEC | MFD_ALLOW_SEALING",
        "F_SEAL_SEAL | F_SEAL_SHRINK | F_SEAL_GROW | F_SEAL_WRITE",
        "let sealed_executable = create_sealed_snapshot(authority)?",
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
        "classify_zero_record_exec_eof(pid)",
        "waitpid(pid, &mut status, WNOHANG)",
        "const WNOWAIT: c_int = 0x0100_0000",
        "waitid(P_PID, state.pid as c_uint, &mut info, options)",
        "child_dup(self.executable.raw(), EXEC_FD, false, 2)",
        "format!(\"{EXECUTABLE_FD_ENV}={EXEC_FD}\")",
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
fn launcher_retains_creator_guardian_and_cleans_the_process_group_after_leader_exit() {
    let launcher = include_str!("linux_worker_launch.rs");
    for required in [
        "struct Guardian {",
        "name(\"scribe-linux-worker-guardian\".to_owned())",
        ".and_then(|()| prepared.fork_exec())",
        "let cleanup_needed = release_rx.recv().unwrap_or(true)",
        "guardian_finalize(pid, cleanup_needed)",
        "recv_timeout(GUARDIAN_SHUTDOWN_TIMEOUT)",
        "leader_reaped: bool",
        "process_group_cleaned: bool",
        "clean_process_group(&mut state)?",
        "kill(-state.pid, SIGKILL)",
        "guardian_outlives_short_lived_launch_helper_thread",
        "leader_exit_cleanup_kills_reported_descendant",
        "parent_death_signal_kills_worker_after_launcher_parent_exits",
        "guardian_retains_eventual_reap_after_bounded_release_wait",
    ] {
        assert!(
            launcher.contains(required),
            "missing Linux lifecycle invariant: {required}"
        );
    }

    let launch = launcher
        .split("fn launch_on_guardian(")
        .nth(1)
        .unwrap()
        .split("fn guardian_finalize")
        .next()
        .unwrap();
    assert!(launch.find(".spawn(move ||").unwrap() < launch.find("prepared.fork_exec()").unwrap());
    assert!(
        launch.find("prepared.fork_exec()").unwrap() < launch.find("release_rx.recv()").unwrap()
    );
}

#[test]
fn leader_exit_is_observed_before_group_cleanup_and_final_reap() {
    let launcher = include_str!("linux_worker_launch.rs");
    let running = launcher
        .split("pub(crate) fn is_running")
        .nth(1)
        .unwrap()
        .split("pub(crate) fn request_cooperative_cancel")
        .next()
        .unwrap();
    let observed = running
        .find("observe_leader_exit(&mut state, true)")
        .unwrap();
    let cleaned = running.find("clean_process_group(&mut state)").unwrap();
    let reaped = running.find("reap_observed_leader(&mut state)").unwrap();
    assert!(observed < cleaned && cleaned < reaped);

    let blocking = launcher
        .split("fn reap_leader(state")
        .nth(1)
        .unwrap()
        .split("fn reap_leader_bounded")
        .next()
        .unwrap();
    assert!(blocking.contains("observe_leader_exit(state, false)?"));
    assert!(
        blocking.find("observe_leader_exit").unwrap()
            < blocking.find("clean_process_group").unwrap()
    );
    assert!(
        blocking.find("clean_process_group").unwrap()
            < blocking.find("reap_observed_leader").unwrap()
    );

    let observation = launcher
        .split("fn observe_leader_exit")
        .nth(1)
        .unwrap()
        .split("fn reap_observed_leader")
        .next()
        .unwrap();
    assert!(observation.contains("WEXITED | WNOWAIT"));
    assert!(observation.contains("if nonblocking { WNOHANG } else { 0 }"));
    assert!(observation.contains("waitid(P_PID, state.pid as c_uint, &mut info, options)"));
}

#[test]
fn linux_worker_hashes_and_closes_the_exact_sealed_image_fd_for_hello() {
    let launcher = include_str!("linux_worker_launch.rs");
    let supervisor = include_str!("onnx_worker.rs");
    assert!(
        launcher.contains(
            "pub(crate) const EXECUTABLE_FD_ENV: &str = \"SCRIBE_PRIVATE_EXECUTABLE_FD\""
        )
    );
    assert!(launcher.contains("child_dup(self.executable.raw(), EXEC_FD, false, 2)"));
    assert!(launcher.contains("SYS_CLOSE_RANGE, 7_u32"));

    let capability = supervisor
        .split("const REQUIRED_SEALS: i32")
        .nth(1)
        .unwrap()
        .split("#[cfg(all(\n    not(test),\n    not(all(target_os")
        .next()
        .unwrap();
    for required in [
        "std::fs::File::from_raw_fd(LINUX_EXECUTABLE_FD)",
        "metadata.file_type().is_file()",
        "metadata.nlink() != 0",
        "libc::fstatfs",
        "libc::F_GET_SEALS",
        "seals != REQUIRED_SEALS",
        "libc::F_DUPFD_CLOEXEC",
        "seek(SeekFrom::Start(0))",
        "sha256_reader(image)",
        "_inherited_image: inherited",
    ] {
        assert!(
            capability.contains(required),
            "missing worker image invariant: {required}"
        );
    }
    assert!(!capability.contains("std::env::current_exe()"));
    assert!(supervisor.contains("worker protocol permits Hello exactly once"));
    assert!(supervisor.contains("std::env::var(LINUX_EXECUTABLE_FD_ENV).as_deref() != Ok(\"3\")"));
    let capability_builder = supervisor
        .split("fn worker_capability(")
        .nth(1)
        .unwrap()
        .split("struct InferenceWorkerFingerprint")
        .next()
        .unwrap();
    assert!(
        capability_builder
            .find("let capability = WorkerCapability")
            .unwrap()
            < capability_builder
                .find("drop(inference_fingerprint)")
                .unwrap()
    );

    let fixture = include_str!("../scripts/linux-worker-launch-fixture.rs");
    assert!(fixture.contains("inherited_image_sha256(3)?"));
    assert!(launcher.contains("IMAGE_SHA256={}"));
}

#[test]
fn sealed_snapshot_and_error_pipe_are_kernel_bounded() {
    let launcher = include_str!("linux_worker_launch.rs");
    let snapshot = launcher
        .split("fn create_sealed_snapshot(")
        .nth(1)
        .unwrap()
        .split("fn set_nonblocking")
        .next()
        .unwrap();
    for required in [
        "SYS_MEMFD_CREATE",
        "MFD_CLOEXEC | MFD_ALLOW_SEALING",
        "expected_executable_length()",
        "expected_executable_sha256()?",
        "F_ADD_SEALS, REQUIRED_MEMFD_SEALS",
        "F_GET_SEALS",
    ] {
        assert!(
            snapshot.contains(required),
            "missing sealed snapshot invariant: {required}"
        );
    }
    assert!(!snapshot.contains("MFD_EXEC"));
    assert!(!snapshot.contains("MFD_NOEXEC_SEAL"));

    let child_failure = launcher
        .split("unsafe fn child_fail(")
        .nth(1)
        .unwrap()
        .split("fn wait_for_exec")
        .next()
        .unwrap();
    assert!(child_failure.contains("while offset < record.len()"));
    assert!(child_failure.contains("offset += written as usize"));
    assert!(child_failure.contains("*__errno_location() == EINTR"));

    let error_pipe = launcher
        .split("fn wait_for_exec(")
        .nth(1)
        .unwrap()
        .split("fn cleanup_failed_child")
        .next()
        .unwrap();
    assert!(error_pipe.contains("classify_zero_record_exec_eof(pid)"));
    assert!(error_pipe.contains("waitpid(pid, &mut status, WNOHANG)"));
    assert!(launcher.contains("sealed_memfd_snapshot_rejects_writes_and_preserves_digest"));
    assert!(
        launcher.contains("concurrent_source_mutation_rejects_or_executes_only_trusted_snapshot")
    );
    assert!(launcher.contains("zero-record child exit"));
}

#[test]
fn linux_launcher_workflow_uses_reviewed_immutable_actions() {
    let workflow = include_str!("../.github/workflows/linux-worker-launch.yml");
    assert!(
        workflow.contains("actions/checkout@fbc6f3992d24b796d5a048ff273f7fcc4a7b6c09 # v5.1.0")
    );
    assert!(workflow.contains("persist-credentials: false"));
    assert!(workflow.contains(
        "dtolnay/rust-toolchain@01ba1edad32c6f80dbcce879d3e0fa5a00b2a84e # 1.96.0 branch reviewed 2026-08-27"
    ));
    for path in ["'.gitattributes'", "'Cargo.toml'", "'Cargo.lock'"] {
        assert!(
            workflow.contains(path),
            "missing workflow path trigger: {path}"
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
