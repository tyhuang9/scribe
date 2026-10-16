//! Source-level guards for the read-only Linux GPU release epoch authority.

fn normalized(source: &str) -> String {
    source.replace("\r\n", "\n")
}

#[test]
fn linux_release_epoch_authority_is_fixed_private_and_read_only() {
    let source = normalized(include_str!("gpu_worker_pack/linux_release_epoch.rs"));
    let production = source
        .split("    #[cfg(test)]\n    mod tests")
        .next()
        .unwrap();
    for required in [
        "const AUTHORITY_PATH: &str = \"/var/lib/scribe/gpu-release-security-epoch-v1\"",
        "b\"scribe-linux-gpu-release-security-epoch-v1:x86_64-unknown-linux-gnu:\"",
        "const MAX_RELEASE_SECURITY_EPOCH: u64 = 9_007_199_254_740_991",
        "const AUTHORITY_COMPONENTS: &[&CStr] = &[c\"var\", c\"lib\", c\"scribe\"]",
        "const AUTHORITY_FILE_NAME: &CStr = c\"gpu-release-security-epoch-v1\"",
        "File::open(\"/\")",
        "O_RDONLY | O_NONBLOCK | O_CLOEXEC | O_NOFOLLOW",
        "RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS | RESOLVE_NO_MAGICLINKS",
    ] {
        assert!(
            source.contains(required),
            "missing authority invariant: {required}"
        );
    }
    for forbidden in [
        "std::env::var",
        "std::env::var_os",
        "OpenOptions",
        "create_dir",
        "create_new",
        "set_len",
        "remove_file",
        "rename(",
    ] {
        assert!(
            !production.contains(forbidden),
            "production epoch reader contains mutation or override seam: {forbidden}"
        );
    }
}

#[test]
fn lease_retains_and_rebinds_every_authority_object() {
    let source = normalized(include_str!("gpu_worker_pack/linux_release_epoch.rs"));
    for required in [
        "struct RetainedDirectory",
        "struct RetainedFloor",
        "directories: Vec<RetainedDirectory>",
        "floor: RetainedFloor",
        "floor_bytes: Vec<u8>",
        "self.recheck_retained()?",
        "let first = open_bound(",
        "self.require_same_binding(&first)?",
        "let final_binding = open_bound(",
        "self.require_same_binding(&final_binding)?",
        "DirectorySnapshot::from_observed(observed) != retained.snapshot",
        "FloorSnapshot::from_observed(observed) != self.floor.snapshot",
        "bytes != self.floor_bytes",
        "file.read_at(&mut bytes[filled..], filled as u64)",
    ] {
        assert!(
            source.contains(required),
            "missing lease invariant: {required}"
        );
    }
    assert!(!source.contains("SeekFrom::Start"));
}

#[test]
fn reader_grants_no_pack_authentication_or_update_authority() {
    let source = normalized(include_str!("gpu_worker_pack/linux_release_epoch.rs"));
    let module = normalized(include_str!("gpu_worker_pack/mod.rs"));
    let worker = normalized(include_str!("onnx_worker.rs"));
    assert!(source.contains("authenticated_candidate_epoch"));
    assert!(
        source
            .contains("This module consumes an epoch only after a future caller has authenticated")
    );
    assert!(module.contains("mod linux_release_epoch;"));
    assert!(!module.contains("pub(crate) mod linux_release_epoch;"));
    assert!(!module.contains("pub(crate) use linux_release_epoch"));
    assert!(!worker.contains("LinuxReleaseEpochLease"));
    for forbidden in [
        "verify_signature",
        "public_key",
        "fn append",
        "fn advance",
        "flock",
        "fcntl_lock",
    ] {
        assert!(
            !source.contains(forbidden),
            "epoch reader acquired unrelated authority: {forbidden}"
        );
    }
}

#[test]
fn portable_and_native_epoch_suites_are_discovered_on_both_ubuntu_lanes() {
    let workflow = normalized(include_str!("../.github/workflows/linux-worker-launch.yml"));
    let script = normalized(include_str!("../scripts/test-linux-worker-launch.sh"));
    assert!(workflow.contains("os: [ubuntu-22.04, ubuntu-24.04]"));
    assert!(workflow.contains("'src/**'"));
    for required in [
        "require_test_discovery",
        "require_exact_test_set",
        "src/gpu_worker_pack/linux_release_epoch.rs",
        "linux-release-epoch-tests",
        "${#epoch_portable_tests[@]} != 13",
        "${#epoch_native_tests[@]} != 14",
        "tests::canonical_floor_and_equal_candidate_are_accepted",
        "linux::tests::missing_authority_is_rejected",
        "linux::tests::concurrent_rechecks_use_independent_positional_reads",
        "linux::tests::fixture_reservation_rejects_directory_and_symlink_collisions_without_mutation",
        "timeout 60s \"$test_root/linux-release-epoch-tests\" --test-threads=1",
        "src/linux_release_epoch_architecture_guard.rs",
        "linux-release-epoch-architecture-tests",
        "${#epoch_architecture_tests[@]} != 4",
        "linux_release_epoch_authority_is_fixed_private_and_read_only",
    ] {
        assert!(script.contains(required), "missing suite hook: {required}");
    }
}
