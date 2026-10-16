//! Source-level guards for the Linux embedded GPU release-authority boundary.

fn normalized(source: &str) -> String {
    source.replace("\r\n", "\n")
}

#[test]
fn build_policy_selects_and_reviews_the_linux_empty_authority_before_publication() {
    let policy = normalized(include_str!("../build_support/linux_release_authority.rs"));
    let build = normalized(include_str!("../build.rs"));
    let portable_tests = normalized(include_str!("../tools/worker-pack-author/src/main.rs"));
    let default = normalized(include_str!(
        "../runtime-manifests/gpu-pack-release-authority-linux-empty.json"
    ));

    assert_eq!(
        default,
        concat!(
            r#"{"schema_version":1,"target":"x86_64-unknown-linux-gnu","catalog_sha256":"c3f19154f1b2265dac92206eae3a35c130a078be46705e1be6032bc442c3b9dc","release_security_epoch":0,"entries":[]}"#,
            "\n"
        )
    );
    for required in [
        "LINUX_DEFAULT_AUTHORITY_PATH",
        "LEGACY_DEFAULT_AUTHORITY_PATH",
        "REVIEWED_LINUX_EMPTY_AUTHORITY",
        "target_os == \"linux\"",
        "target_arch == \"x86_64\" && target_env == \"gnu\"",
        "normalized != REVIEWED_LINUX_EMPTY_AUTHORITY",
        "normalized.last() == Some(&b'\\r')",
        "pub(crate) fn emit_selected_authority(",
        "validate_selected_authority(selection, &normalized)?",
        "publish(&normalized)",
    ] {
        assert!(
            policy.contains(required),
            "missing build-policy invariant: {required}"
        );
    }
    assert!(!policy.contains("std::env::"));
    assert!(!policy.contains("normalized.contains(&b'\\n')"));
    assert!(!policy.contains("normalized.contains(&b'\\r')"));
    let validation = policy
        .find("validate_selected_authority(selection, &normalized)?")
        .unwrap();
    let publication = policy.find("publish(&normalized)").unwrap();
    assert!(
        validation < publication,
        "Linux authority validation must precede publication"
    );
    let emitter = build
        .find("linux_release_authority::emit_selected_authority(")
        .unwrap();
    let output = build
        .find(".join(\"scribe_gpu_pack_release_authority.json\")")
        .unwrap();
    assert!(
        emitter < output,
        "build output must remain inside the validated emitter"
    );
    for required in [
        "assert!(absent_output.is_none(), \"invalid input created output\")",
        "assert_eq!(existing_output.as_slice(), b\"existing-sentinel\")",
        "positive empty metadata reached production publication",
        "b\"{}\\n\\n\".as_slice(), b\"{}\\n\".as_slice()",
        "b\"\\xef\\xbb\\xbf{}\\n\".as_slice(), b\"\\xef\\xbb\\xbf{}\".as_slice()",
    ] {
        assert!(
            portable_tests.contains(required),
            "missing emitter regression proof: {required}"
        );
    }
}

#[test]
fn linux_authority_is_private_semantic_binding_not_pack_or_epoch_admission() {
    let source = normalized(include_str!("gpu_worker_pack/linux_release_authority.rs"));
    for required in [
        "pub(crate) fn validate_catalog_release_binding",
        "#[serde(deny_unknown_fields)]",
        "serde_json::to_vec(&document)",
        "Sha256::digest(catalog_bytes)",
        "const MAX_DOCUMENT_BYTES: usize = 512 * 1024",
        "const MAX_PRODUCTION_PACKS: usize = 8",
        "fn catalog_roots_are_unique(entries: &[PackCatalogEntry]) -> bool",
        "canonical_roots.insert(entry.root.to_ascii_lowercase())",
        "manifest::MAX_FILES + 2",
        "manifest::MAX_AGGREGATE_BYTES",
        "manifest::validate_build_identity(&entry.build_revision",
        "fn canonical_app_version(value: &str) -> bool",
        "canonical_payload_path(worker_relative_path)",
        ".split('/')",
        "matches!(component, \".\" | \"..\")",
        "file.to_ascii_lowercase()",
        "files.binary_search(&required_manifest)",
        "files.binary_search(&required_signature)",
        "files.binary_search(&required_worker)",
        "entry.security_epoch == authority.release_security_epoch",
        "target_os == LINUX_TARGET_OS",
        "target_arch == LINUX_TARGET_ARCH",
        "A positive epoch with no entries is structurally valid future metadata",
    ] {
        assert!(
            source.contains(required),
            "missing runtime invariant: {required}"
        );
    }
    assert!(
        !source.contains("authority.release_security_epoch > 0 && authority.entries.is_empty()")
    );
    for forbidden in [
        "ProductionTrustRoot",
        "verify_signature",
        "LinuxReleaseEpochLease",
        "gpu-release-security-epoch-v1",
        "keychain_access_group",
        "floor_path",
        "std::fs",
        "std::env",
    ] {
        assert!(
            !source.contains(forbidden),
            "semantic binding acquired unrelated authority: {forbidden}"
        );
    }
    assert!(source.contains("does not\n//! authenticate a pack"));
}

#[test]
fn platform_dispatch_preserves_existing_non_linux_release_authority_behavior() {
    let module = normalized(include_str!("gpu_worker_pack/mod.rs"));
    let compact_module = module.split_whitespace().collect::<String>();
    let main = normalized(include_str!("main.rs"));
    let worker = normalized(include_str!("onnx_worker.rs"));
    assert!(module.contains("mod linux_release_authority;"));
    assert!(!module.contains("pub(crate) mod linux_release_authority;"));
    assert!(!module.contains("pub(crate) use linux_release_authority"));
    assert!(compact_module.contains(
        "#[cfg(any(all(target_os=\"linux\",target_arch=\"x86_64\",target_env=\"gnu\"),test))]modlinux_release_authority;"
    ));
    assert!(module.contains("super::linux_release_authority::validate_catalog_release_binding("));
    assert!(module.contains("serde_json::from_slice::<super::PackReleaseAuthority>("));
    assert!(module.contains("super::catalog_matches_release_authority("));
    assert!(compact_module.contains("#[cfg(target_os=\"linux\")]"));
    assert!(compact_module.contains("#[cfg(not(target_os=\"linux\"))]"));
    assert!(main.contains("mod linux_release_authority_architecture_guard;"));
    assert!(!worker.contains("validate_catalog_release_binding"));
    assert!(!worker.contains("LinuxCatalogReleaseBinding"));
}

#[test]
fn portable_authority_suites_are_exactly_discovered_on_both_ubuntu_lanes() {
    let workflow = normalized(include_str!("../.github/workflows/linux-worker-launch.yml"));
    let script = normalized(include_str!("../scripts/test-linux-worker-launch.sh"));
    assert!(workflow.contains("os: [ubuntu-22.04, ubuntu-24.04]"));
    for path in [
        "'build_support/linux_release_authority.rs'",
        "'runtime-manifests/gpu-pack-release-authority-linux-empty.json'",
        "'tools/worker-pack-author/Cargo.toml'",
        "'tools/worker-pack-author/src/main.rs'",
    ] {
        assert_eq!(
            workflow.matches(path).count(),
            2,
            "missing workflow path: {path}"
        );
    }
    for required in [
        "require_exact_named_tests",
        "tools/worker-pack-author/Cargo.toml",
        "SCRIBE_BUILD_REVISION=\"linux-release-authority-tests\"",
        "${#linux_release_authority_tests[@]} != 9",
        "linux_release_authority_default_emission_validates_empty_runtime_binding",
        "linux_release_authority_positive_cuda_and_vulkan_fixture_binds_exactly",
        "linux_release_authority_reviewed_empty_metadata_disables_gpu_support",
        "linux_release_authority_rejects_malformed_and_digest_mismatched_release_metadata",
        "linux_release_authority_rejects_structurally_valid_positive_and_nonempty_release_metadata",
        "linux_release_authority_rejects_tampered_build_catalog_target_abi_provider_epoch_and_inventory",
        "timeout 120s cargo test --locked",
        "cargo test --locked --offline",
        "src/linux_release_authority_architecture_guard.rs",
        "${#linux_release_authority_architecture_tests[@]} != 5",
        "linux_authority_is_private_semantic_binding_not_pack_or_epoch_admission",
        "linux_release_package_consumer_is_data_only_default_deny_and_exactly_tested",
    ] {
        assert!(
            script.contains(required),
            "missing canonical suite hook: {required}"
        );
    }
    assert!(
        workflow
            .contains("cargo fetch --locked --manifest-path tools/worker-pack-author/Cargo.toml")
    );
}

#[test]
fn linux_release_package_consumer_is_data_only_default_deny_and_exactly_tested() {
    let module = normalized(include_str!("gpu_worker_pack/mod.rs"));
    let authority = normalized(include_str!("gpu_worker_pack/linux_release_authority.rs"));
    let portable_tests = normalized(include_str!("../tools/worker-pack-author/src/main.rs"));
    let worker = normalized(include_str!("onnx_worker.rs"));
    let script = normalized(include_str!("../scripts/test-linux-worker-launch.sh"));
    let compact_module = module.split_whitespace().collect::<String>();
    for required in [
        "linux_release_authority::linux_release_policy_decision(",
        "linux_release_authority::LinuxReleasePolicyDecision::SupportDisabled",
        "linux_release_authority::LinuxReleasePolicyDecision::MetadataRejected",
        "PackDiscoveryIssue::LinuxReleaseSupportDisabled",
        "PackDiscoveryIssue::LinuxReleaseMetadataRejected",
        "catalog_generation: None",
        "linux_release_package_policy_projects_to_zero_lease_categorical_discovery",
    ] {
        assert!(
            module.contains(required),
            "missing Linux consumer invariant: {required}"
        );
    }
    assert!(compact_module.contains(
        "#[cfg(all(target_os=\"linux\",target_arch=\"x86_64\",target_env=\"gnu\"))]pub(crate)fndiscover_production_pack_leases()"
    ));
    for required in [
        "pub(crate) enum LinuxReleasePolicyDecision",
        "pub(crate) fn linux_release_policy_decision(",
        "binding.release_security_epoch == 0 && binding.entry_count == 0",
    ] {
        assert!(
            authority.contains(required),
            "missing Linux policy invariant: {required}"
        );
    }
    assert!(!authority.contains("pub enum LinuxReleasePolicyDecision"));
    let helper_start = authority
        .find("pub(crate) fn linux_release_policy_decision(")
        .unwrap();
    let helper_tail = &authority[helper_start..];
    let helper_end = helper_tail.find("\n}\n").unwrap() + 3;
    let helper = &helper_tail[..helper_end];
    for forbidden in [
        "std::fs",
        "LinuxReleaseEpochLease",
        "ProductionTrustRoot",
        "verify_catalog_entries",
        "provider",
        "health",
        "launch",
    ] {
        assert!(
            !helper.contains(forbidden),
            "Linux data-only consumer acquired forbidden behavior: {forbidden}"
        );
    }
    for required in [
        "LinuxReleaseSupportDisabled",
        "LinuxReleaseMetadataRejected",
    ] {
        assert!(
            worker.contains(required),
            "missing stable probe mapping: {required}"
        );
    }
    for required in [
        "linux_release_authority_reviewed_empty_metadata_disables_gpu_support",
        "linux_release_authority_rejects_malformed_and_digest_mismatched_release_metadata",
        "linux_release_authority_rejects_structurally_valid_positive_and_nonempty_release_metadata",
    ] {
        assert!(
            portable_tests.contains(required),
            "missing portable consumer test: {required}"
        );
        assert!(
            script.contains(required),
            "missing canonical consumer test hook: {required}"
        );
    }
    assert!(portable_tests.contains("linux_vulkan_fixture("));
    assert!(script.contains("${#linux_release_authority_tests[@]} != 9"));
    assert!(!script.contains("linux_release_package_consumer_tests"));
    assert!(!script.contains("--bin local-transcriber --features ui-harness"));
}
