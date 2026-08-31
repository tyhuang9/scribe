//! Source-level guards for worker-local Linux GPU identity routing.

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
        "{\"schema_version\":1,\"mode\":\"default_deny\",\"target_os\":\"linux\",\"target_arch\":\"x86_64\",\"entries\":[]}\n"
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
