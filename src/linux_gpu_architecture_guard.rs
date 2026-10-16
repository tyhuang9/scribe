//! Source-level guards for parent and worker Linux GPU identity routing.

fn normalized(source: &str) -> String {
    source.replace("\r\n", "\n")
}

#[test]
fn linux_gpu_identity_is_pci_only_fixed_root_and_bounded() {
    let source = normalized(include_str!("linux_gpu.rs"));
    for required in [
        "native:pci:{:04x}:{:02x}:{:02x}.{:x}",
        "const PCI_ROOT: &str = \"/sys/bus/pci/devices\"",
        "Path::new(\"/proc/driver/nvidia/gpus\")",
        "Path::new(\"/proc/sys/kernel/osrelease\")",
        "Path::new(\"/sys/module\")",
        "dlopen(c\"libcuda.so.1\".as_ptr(), RTLD_NOW)",
        "c\"cuDeviceGetUuid_v2\"",
        "MAX_GPU_FACTS",
        "MAX_PROVIDER_DEVICES",
        "MAX_DRIVER_IDENTITY_BYTES",
        "MAX_SYSFS_VALUE_BYTES",
        "MAX_PCI_SYSFS_ENTRIES",
        "MAX_NVIDIA_INFORMATION_BYTES",
        "PhysicalDevicePCIBusInfoPropertiesEXT",
        "vk::ExtPciBusInfoFn::name()",
    ] {
        assert!(
            source.contains(required),
            "missing Linux GPU invariant: {required}"
        );
    }
    let production = source.split("#[cfg(test)]\nmod tests").next().unwrap();
    for forbidden in [
        "Command::new",
        "nvidia-smi",
        "std::env::var",
        "std::env::var_os",
        "VULKAN_SDK",
        "VK_ICD_FILENAMES",
        "CUDA_VISIBLE_DEVICES",
        "/dev/dri/by-path",
    ] {
        assert!(
            !production.contains(forbidden),
            "production Linux GPU router contains forbidden authority: {forbidden}"
        );
    }
}

#[test]
fn linux_parent_witness_is_two_sample_provider_free_and_bounded() {
    let source = normalized(include_str!("linux_gpu.rs"));
    for required in [
        "pub(crate) fn discover_parent_device_driver_catalog()",
        "let first = canonical_parent_catalog(source.snapshot()?)?",
        "let second = canonical_parent_catalog(source.snapshot()?)?",
        "Linux GPU parent device or driver facts changed during sampling",
        "stable_device_identity: fact.address.canonical()",
        "vendor: fact.vendor",
        "kernel_release: snapshot.kernel_release",
        "if entries.len() == MAX_PCI_SYSFS_ENTRIES",
        "module_version_is_present(fs::metadata(&module_version_path).map(|_| ()))?",
        "if !value.is_ascii() || value.len() != 12",
        "byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)",
        "fn worker_snapshot(backend: LinuxGpuBackend)",
    ] {
        assert!(
            source.contains(required),
            "missing Linux parent witness invariant: {required}"
        );
    }
    let kernel_sampler = source
        .split("fn kernel_pci_snapshot()")
        .nth(1)
        .unwrap()
        .split("fn worker_snapshot(")
        .next()
        .unwrap();
    assert!(!kernel_sampler.contains("collect::<Result<Vec<_>, _>>()"));
    assert!(!kernel_sampler.contains("module_version_path.exists()"));
    for forbidden in [
        "/proc/driver/nvidia/gpus",
        "nvidia_uuid_for",
        "cuda_pci_uuid_identities",
        "vulkan_pci_driver_identities",
        "ash::",
        "dlopen",
    ] {
        assert!(
            !kernel_sampler.contains(forbidden),
            "Linux parent kernel sampler contains provider authority: {forbidden}"
        );
    }

    let worker_enrichment = source
        .split("fn worker_snapshot(")
        .nth(1)
        .unwrap()
        .split("fn bounded_fixed_file(")
        .next()
        .unwrap();
    for required in [
        "nvidia_uuid_for(fact.address)?",
        "cuda_pci_uuid_identities",
        "vulkan_pci_driver_identities",
    ] {
        assert!(
            worker_enrichment.contains(required),
            "Linux worker enrichment lost required proof: {required}"
        );
    }

    let worker = normalized(include_str!("onnx_worker.rs"));
    let parent_catalog = worker
        .split("struct PackDriverCatalog {\n    catalog: crate::linux_gpu::LinuxGpuParentCatalog")
        .nth(1)
        .unwrap()
        .split("#[cfg(not(any(")
        .next()
        .unwrap();
    for forbidden in [
        "transcribe_cpp",
        "route_provider_devices",
        "cuda_pci_uuid_identities",
        "vulkan_pci_driver_identities",
        "/proc/driver/nvidia/gpus",
    ] {
        assert!(
            !parent_catalog.contains(forbidden),
            "Linux parent PackDriverCatalog invokes provider discovery: {forbidden}"
        );
    }
}

#[test]
fn provider_indexes_are_volatile_and_uuid_aliases_never_leave_routing() {
    let source = normalized(include_str!("linux_gpu.rs"));
    for required in [
        "let first = source.snapshot(backend)?",
        "let second = source.snapshot(backend)?",
        "if first != second",
        "process_indexes.insert(provider.process_index)",
        "stable_addresses.insert(address)",
        "Linux CUDA MIG identities are not physical GPU identities",
        "Linux CUDA device is logical, MIG-partitioned, or conflicts with the physical GPU UUID",
        "Linux Vulkan provider omitted canonical PCI identity",
        "Linux GPU provider maps multiple logical devices to one physical PCI function",
        "nvidia_physical_uuid_alias: Option<String>",
        "stable_device_identity: address.canonical()",
    ] {
        assert!(
            source.contains(required),
            "missing Linux routing invariant: {required}"
        );
    }
    assert!(!source.contains("Serialize"));
    assert!(!source.contains("Deserialize"));
    assert!(source.contains("#[derive(Clone, Eq, PartialEq)]\nstruct LinuxGpuFact {"));
    assert!(
        source.contains(
            "#[derive(Clone, Eq, PartialEq)]\npub(crate) struct ProviderLinuxGpuDevice {"
        )
    );
}

#[test]
fn worker_hello_uses_resolved_linux_identity_and_existing_finish_path() {
    let worker = normalized(include_str!("onnx_worker.rs"));
    let linux = worker
        .split("use crate::linux_gpu::{")
        .nth(1)
        .unwrap()
        .split("#[cfg(all(target_os = \"linux\"")
        .next()
        .unwrap();
    for required in [
        "route_provider_devices(&KernelLinuxGpuFactSource, backend, &provider)",
        "stable_device_identity: routed.stable_device_identity.clone()",
        "driver_version: Some(routed.driver_identity.clone())",
        "return finish_worker_pack_capability(expectation, expected_device_id, devices)",
    ] {
        assert!(
            linux.contains(required),
            "missing worker Hello binding: {required}"
        );
    }
    assert!(worker.contains("install_current_pack_runtime_devices(&capability)"));
    assert!(worker.contains("current.process_index == device.process_index"));
}

#[test]
fn linux_auto_qualification_remains_canonical_default_deny() {
    let manifest = normalized(include_str!(
        "../runtime-manifests/gpu-auto-qualification-linux-x86_64.json"
    ));
    assert_eq!(
        manifest,
        "{\"schema_version\":2,\"mode\":\"default_deny\",\"target_os\":\"linux\",\"target_arch\":\"x86_64\",\"entries\":[]}\n"
    );
}

#[test]
fn linux_gpu_ci_runs_on_both_supported_ubuntu_lanes() {
    let workflow = normalized(include_str!("../.github/workflows/linux-worker-launch.yml"));
    assert!(workflow.contains("os: [ubuntu-22.04, ubuntu-24.04]"));
    let all_source_changes_trigger_ci = workflow.contains("'src/**'");
    for path in [
        "'src/linux_gpu.rs'",
        "'src/linux_gpu_architecture_guard.rs'",
        "'docs/LINUX_GPU_ROUTING.md'",
    ] {
        assert!(
            workflow.contains(path) || (all_source_changes_trigger_ci && path.starts_with("'src/")),
            "missing workflow path trigger: {path}"
        );
    }
    let script = normalized(include_str!("../scripts/test-linux-worker-launch.sh"));
    assert!(script.contains("src/linux_gpu.rs"));
    assert!(script.contains("linux-gpu-routing-tests"));
    assert!(script.contains("src/linux_gpu_architecture_guard.rs"));
    assert!(script.contains("linux-gpu-architecture-tests"));
    assert!(workflow.contains("scripts/linux-gpu-typecheck/Cargo.toml"));
    assert!(workflow.contains("--features vulkan-acceleration"));
    assert!(workflow.contains("--features cuda-acceleration"));
    assert!(workflow.contains("RUSTFLAGS: -Dwarnings"));
    assert!(workflow.contains("CARGO_TARGET_DIR: target/linux-gpu-typecheck"));
}

#[test]
fn linux_gpu_typecheck_is_source_only_locked_and_pinned() {
    let manifest = normalized(include_str!("../scripts/linux-gpu-typecheck/Cargo.toml"));
    let lock = normalized(include_str!("../scripts/linux-gpu-typecheck/Cargo.lock"));
    let harness = normalized(include_str!("../scripts/linux-gpu-typecheck/src/lib.rs"));
    assert!(manifest.contains("ash = { version = \"=0.37.3\", optional = true }"));
    assert!(manifest.contains("cuda-acceleration = []"));
    assert!(manifest.contains("vulkan-acceleration = [\"dep:ash\"]"));
    assert!(manifest.contains("[workspace]"));
    assert!(lock.contains("version = \"0.37.3+1.3.251\""));
    assert!(lock.contains(
        "checksum = \"39e9c3835d686b0a6084ab4234fcd1b07dbf6e4767dce60874b12356a25ecd4a\""
    ));
    assert_eq!(
        harness,
        "#![allow(dead_code)]\n\n#[path = \"../../../src/linux_gpu.rs\"]\nmod linux_gpu;\n"
    );
    for forbidden in [
        concat!("sher", "pa"),
        "transcribe",
        "build.rs",
        "cc =",
        "git =",
    ] {
        assert!(
            !manifest.contains(forbidden),
            "type-check manifest contains forbidden dependency: {forbidden}"
        );
    }
}
