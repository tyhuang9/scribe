//! Fail-closed runtime contract for the future Linux GPU worker lane.
//!
//! This module only recognizes the reviewed Linux platform envelope. It does
//! not discover devices, load GPU libraries, or make CPU routing unavailable.

pub(crate) const LINUX_GPU_RUNTIME_MANIFEST: &str =
    include_str!("../runtime-manifests/gpu-runtime-linux-x86_64.json");
const LINUX_GPU_TOOLCHAIN_MANIFEST: &str =
    include_str!("../runtime-manifests/gpu-worker-toolchain-linux-x86_64.json");
const LINUX_AUTO_QUALIFICATION_MANIFEST: &str =
    include_str!("../runtime-manifests/gpu-auto-qualification-linux-x86_64.json");

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
        && version_at_least(glibc, (2, 35), SuffixPolicy::Forbidden)
        && version_at_least(kernel, (5, 15), SuffixPolicy::KernelRelease)
}

#[derive(Clone, Copy)]
enum SuffixPolicy {
    Forbidden,
    KernelRelease,
}

fn version_at_least(value: &str, minimum: (u16, u16), suffix_policy: SuffixPolicy) -> bool {
    if value.is_empty() || value.len() > 96 || !value.is_ascii() {
        return false;
    }

    let numeric = match value.split_once('-') {
        Some((numeric, suffix)) if matches!(suffix_policy, SuffixPolicy::KernelRelease) => {
            if suffix.is_empty()
                || suffix.len() > 64
                || !suffix
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                || !suffix.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-')
                })
            {
                return false;
            }
            numeric
        }
        Some(_) => return false,
        None => value,
    };

    let parts = numeric.split('.').collect::<Vec<_>>();
    if !(2..=3).contains(&parts.len())
        || parts
            .iter()
            .any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return false;
    }
    let Some(major) = parts[0].parse::<u16>().ok() else {
        return false;
    };
    let Some(minor) = parts[1].parse::<u16>().ok() else {
        return false;
    };
    if parts
        .get(2)
        .is_some_and(|patch| patch.parse::<u16>().is_err())
    {
        return false;
    }
    (major, minor) >= minimum
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_gpu_runtime_accepts_only_the_reviewed_platform_floor() {
        assert!(supports_linux_gpu_runtime(
            "linux",
            "x86_64",
            "ubuntu",
            "22.04",
            "2.35",
            "5.15.0-1092-azure"
        ));
        assert!(supports_linux_gpu_runtime(
            "linux", "x86_64", "ubuntu", "24.04", "2.39", "6.8"
        ));
        assert!(supports_linux_gpu_runtime(
            "linux",
            "x86_64",
            "ubuntu",
            "24.04",
            "2.39.0",
            "6.8.0-1030-nvidia-64k"
        ));
        for candidate in [
            ("linux", "aarch64", "ubuntu", "24.04", "2.39", "6.8"),
            ("linux", "x86_64", "debian", "12", "2.39", "6.8"),
            ("linux", "x86_64", "ubuntu", "20.04", "2.35", "5.15"),
            ("linux", "x86_64", "ubuntu", "22.04", "2.34", "5.15"),
            ("linux", "x86_64", "ubuntu", "22.04", "2.35", "5.14"),
            (
                "linux",
                "x86_64",
                "ubuntu",
                "22.04",
                "2.35.not-a-version",
                "5.15",
            ),
            (
                "linux",
                "x86_64",
                "ubuntu",
                "22.04",
                "2.35",
                "5.15.not-a-version",
            ),
            ("linux", "x86_64", "ubuntu", "22.04", "2.35 ", "5.15"),
            ("linux", "x86_64", "ubuntu", "22.04", "2.35", "5.15.0-"),
            (
                "linux",
                "x86_64",
                "ubuntu",
                "22.04",
                "2.35",
                "5.15.0-generic/evil",
            ),
            (
                "linux",
                "x86_64",
                "ubuntu",
                "22.04",
                "2.35",
                "5.15.0-azure evil",
            ),
            (
                "linux",
                "x86_64",
                "ubuntu",
                "22.04",
                "2.35",
                "5.15.0-générique",
            ),
            ("linux", "x86_64", "ubuntu", "22.04", "65536.35", "5.15"),
            ("linux", "x86_64", "ubuntu", "22.04", "2.35", "5.15.0.1"),
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
        assert!(!supports_linux_gpu_runtime(
            "linux",
            "x86_64",
            "ubuntu",
            "22.04",
            "2.35",
            &format!("5.15.0-{}", "a".repeat(65)),
        ));
    }

    #[test]
    fn runtime_manifest_is_canonical_and_records_both_provider_floors() {
        assert_eq!(
            LINUX_GPU_RUNTIME_MANIFEST,
            concat!(
                r#"{"schema_version":1,"target_os":"linux","target_arch":"x86_64","abi":"gnu","supported_ubuntu_versions":["22.04","24.04"],"minimum_glibc":"2.35","minimum_kernel":"5.15","required_primitives":["openat2:RESOLVE_BENEATH+RESOLVE_NO_SYMLINKS+RESOLVE_NO_MAGICLINKS","execveat:AT_EMPTY_PATH","close_range"],"cuda":{"toolkit_version":"12.8","nvcc_version":"12.8.93","provider":"transcribe-cpp-ggml-cuda","minimum_driver_version":"570.26"},"vulkan":{"loader_version":"1.4.357.0","minimum_api_version":"1.2","provider":"transcribe-cpp-ggml-vulkan"}}"#,
                "\n"
            )
        );
        assert_eq!(
            LINUX_GPU_TOOLCHAIN_MANIFEST,
            concat!(
                r#"{"schema_version":1,"target_triple":"x86_64-unknown-linux-gnu","rust":{"release":"1.96.0"},"ubuntu_versions":["22.04","24.04"],"glibc_minimum":"2.35","kernel_minimum":"5.15","cuda":{"toolkit_version":"12.8","nvcc_version":"12.8.93","provider":"transcribe-cpp-ggml-cuda","minimum_driver_version":"570.26"},"vulkan":{"loader_toolchain_version":"1.4.357.0","minimum_api_version":"1.2","provider":"transcribe-cpp-ggml-vulkan"},"build":{"profile":"release","dynamic_backends":false,"openmp":false}}"#,
                "\n"
            )
        );
        assert_eq!(
            LINUX_AUTO_QUALIFICATION_MANIFEST,
            concat!(
                r#"{"schema_version":1,"mode":"default_deny","target_os":"linux","target_arch":"x86_64","entries":[]}"#,
                "\n"
            )
        );
    }
}
