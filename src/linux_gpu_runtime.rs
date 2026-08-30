//! Fail-closed runtime contract for the future Linux GPU worker lane.
//!
//! This module only recognizes the reviewed Linux platform envelope. It does
//! not discover devices, load GPU libraries, or make CPU routing unavailable.

pub(crate) const LINUX_GPU_RUNTIME_MANIFEST: &str =
    include_str!("../runtime-manifests/gpu-runtime-linux-x86_64.json");

pub(crate) fn supports_linux_gpu_runtime(
    operating_system: &str,
    architecture: &str,
    distribution: &str,
    release: &str,
    glibc: &str,
    kernel: &str,
) -> bool {
    operating_system == "linux"
        && architecture == "x86_64"
        && distribution == "ubuntu"
        && matches!(release, "22.04" | "24.04")
        && version_at_least(glibc, (2, 35))
        && version_at_least(kernel, (5, 15))
}

fn version_at_least(value: &str, minimum: (u16, u16)) -> bool {
    let mut parts = value.split('.');
    let Some(major) = parts.next().and_then(|part| part.parse::<u16>().ok()) else {
        return false;
    };
    let Some(minor) = parts.next().and_then(|part| part.parse::<u16>().ok()) else {
        return false;
    };
    (major, minor) >= minimum
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_gpu_runtime_accepts_only_the_reviewed_platform_floor() {
        assert!(supports_linux_gpu_runtime(
            "linux", "x86_64", "ubuntu", "22.04", "2.35", "5.15"
        ));
        assert!(supports_linux_gpu_runtime(
            "linux", "x86_64", "ubuntu", "24.04", "2.39", "6.8"
        ));
        for candidate in [
            ("linux", "aarch64", "ubuntu", "24.04", "2.39", "6.8"),
            ("linux", "x86_64", "debian", "12", "2.39", "6.8"),
            ("linux", "x86_64", "ubuntu", "20.04", "2.35", "5.15"),
            ("linux", "x86_64", "ubuntu", "22.04", "2.34", "5.15"),
            ("linux", "x86_64", "ubuntu", "22.04", "2.35", "5.14"),
        ] {
            assert!(!supports_linux_gpu_runtime(
                candidate.0,
                candidate.1,
                candidate.2,
                candidate.3,
                candidate.4,
                candidate.5
            ));
        }
    }

    #[test]
    fn runtime_manifest_is_canonical_and_records_both_provider_floors() {
        assert_eq!(
            LINUX_GPU_RUNTIME_MANIFEST.trim_end(),
            r#"{"schema_version":1,"target_os":"linux","target_arch":"x86_64","abi":"gnu","supported_ubuntu_versions":["22.04","24.04"],"minimum_glibc":"2.35","minimum_kernel":"5.15","cuda":{"toolkit_version":"12.8","nvcc_version":"12.8.93","provider":"transcribe-cpp-ggml-cuda","minimum_driver_version":"570.26"},"vulkan":{"loader_version":"1.4.357.0","minimum_api_version":"1.2","provider":"transcribe-cpp-ggml-vulkan"}}"#
        );
    }
}
