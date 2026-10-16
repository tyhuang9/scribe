//! Private build-time manifest and signing tool for verified worker packs.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

use anyhow::{Result, anyhow, bail};

#[allow(
    dead_code,
    reason = "the authoring tool reuses the production verifier but not its runtime lease API"
)]
#[path = "../../../src/gpu_worker_pack/manifest.rs"]
mod manifest;
#[allow(
    dead_code,
    reason = "the release tool exposes only bounded store operations needed during packaging"
)]
#[path = "../../../src/gpu_worker_pack/store.rs"]
mod store;
mod onnx_worker {
    pub(crate) use crate::worker_identity::{
        DESKTOP_BUILD_ID, INFERENCE_WORKER_BUILD_ID, PROTOCOL_VERSION, WORKER_ABI_VERSION,
    };
}
#[cfg(test)]
#[allow(
    dead_code,
    reason = "portable tests exercise Linux release/catalog semantics without runtime wiring"
)]
#[path = "../../../src/gpu_worker_pack/linux_release_authority.rs"]
mod linux_release_authority;
#[cfg(test)]
#[allow(
    dead_code,
    reason = "portable tests exercise the pure build selection policy"
)]
#[path = "../../../build_support/linux_release_authority.rs"]
mod linux_release_authority_policy;
#[cfg(test)]
#[path = "../../../build_support/windows_cuda_link.rs"]
mod windows_cuda_link;
#[path = "../../../src/windows_gpu_approved_signing.rs"]
mod windows_gpu_approved_signing;
#[allow(
    dead_code,
    reason = "the authoring tool reuses only GPU policy admission needed by the shared verifier"
)]
#[path = "../../../src/worker_compatibility.rs"]
mod worker_compatibility;
#[path = "../../../src/worker_identity.rs"]
mod worker_identity;
#[path = "../../../src/worker_pack_authoring.rs"]
mod worker_pack_authoring;

use manifest::{Compatibility, PackBackend, PackVerifier, ProductionTrustRoot};
use store::PackStore;
use windows_gpu_approved_signing::{
    inspect_approved_windows_set, sign_approved_windows_set, verify_signed_windows_set,
    verify_signed_windows_set_for_worker_source,
};
use worker_pack_authoring::{
    AUTHOR_TARGET_CONTRACT, AuthorRequest, AuthoringBackend, PrepareRequest, SigningMode,
    author_pack, check_production_signing_key, inspect_prepared_pack, prepare_pack,
    sign_prepared_pack, validate_authoring_target, verify_fixture_pack,
};

const HELP_TEXT: &str = "Scribe worker-pack authoring tool\n\
commands:\n\
  author --backend <cuda|vulkan|metal> [--target-os <windows|linux|macos> --target-arch <x86_64|aarch64>] ...\n\
  prepare-pack --backend <cuda|vulkan|metal> --pack-root <path> ...\n\
  inspect-prepared-pack --pack-root <path>\n\
  sign-prepared-pack --pack-root <path> --expected-manifest-sha256 <sha256> --expected-pack-digest <sha256> (--fixture-signing | --key-id <id> --private-key <path>)\n\
  inspect-approved-windows-set --handoff-root <path> --approval <path> --policy <path>\n\
  sign-approved-windows-set --handoff-root <path> --approval <path> --policy <path> --output-root <path> (PKCS#8 v2 DER key on stdin)\n\
  verify-signed-windows-set --signed-root <path> --policy <path> --toolchain-manifest <path> [--worker-source-revision <40-lowercase-hex>]\n\
  verify-fixture --pack-root <path>\n\
  verify-production-linux --pack-root <path>\n\
  install-production-linux --pack-root <path> --packs-root <path> --state-root <path>\n\
  check-production-key --key-id <id> --private-key <path>\n\
Author targets: cuda or vulkan on windows/x86_64 or linux/x86_64; metal on macos/aarch64 or macos/x86_64.\n\
The target flags may be omitted only for legacy cuda/vulkan authoring, which defaults to windows/x86_64. Values are lowercase and case-sensitive.";

fn main() {
    if let Err(error) = run() {
        eprintln!("Scribe worker-pack tool failed: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let mut arguments = std::env::args_os().skip(1);
    let command = arguments
        .next()
        .ok_or_else(|| anyhow!("expected a documented command or --help\n{HELP_TEXT}"))?;
    let remaining = arguments.collect::<Vec<_>>();
    if matches!(command.to_str(), Some("--help" | "help")) {
        if !remaining.is_empty() {
            bail!("help does not accept options\n{HELP_TEXT}");
        }
        println!("{HELP_TEXT}");
        return Ok(());
    }
    let options = parse_options(remaining)?;
    match command.to_str() {
        Some("author") => run_author(&options),
        Some("prepare-pack") => run_prepare_pack(&options),
        Some("inspect-prepared-pack") => {
            require_exact_options(&options, &["--pack-root"])?;
            let descriptor =
                inspect_prepared_pack(&PathBuf::from(required(&options, "--pack-root")?))?;
            println!("{}", serde_json::to_string(&descriptor)?);
            Ok(())
        }
        Some("sign-prepared-pack") => run_sign_prepared_pack(&options),
        Some("inspect-approved-windows-set") => {
            require_exact_options(&options, &["--approval", "--handoff-root", "--policy"])?;
            let descriptor = inspect_approved_windows_set(
                &PathBuf::from(required(&options, "--handoff-root")?),
                &PathBuf::from(required(&options, "--approval")?),
                &PathBuf::from(required(&options, "--policy")?),
            )?;
            println!("{}", serde_json::to_string(&descriptor)?);
            Ok(())
        }
        Some("sign-approved-windows-set") => {
            require_exact_options(
                &options,
                &["--approval", "--handoff-root", "--output-root", "--policy"],
            )?;
            let mut input = std::io::stdin().lock();
            let receipt = sign_approved_windows_set(
                &PathBuf::from(required(&options, "--handoff-root")?),
                &PathBuf::from(required(&options, "--approval")?),
                &PathBuf::from(required(&options, "--policy")?),
                &PathBuf::from(required(&options, "--output-root")?),
                &mut input,
            )?;
            println!("{}", serde_json::to_string(&receipt)?);
            Ok(())
        }
        Some("verify-signed-windows-set") => run_verify_signed_windows_set(&options),
        Some("verify-fixture") => {
            require_exact_options(&options, &["--pack-root"])?;
            let descriptor =
                verify_fixture_pack(&PathBuf::from(required(&options, "--pack-root")?))?;
            println!("{}", serde_json::to_string(&descriptor)?);
            Ok(())
        }
        Some("verify-production-linux") => {
            require_exact_options(&options, &["--pack-root"])?;
            let verifier = linux_production_verifier();
            let verified = verifier.verify(&PathBuf::from(required(&options, "--pack-root")?))?;
            println!("{}", serde_json::to_string(&verified)?);
            Ok(())
        }
        Some("install-production-linux") => {
            require_exact_options(&options, &["--pack-root", "--packs-root", "--state-root"])?;
            let verifier = linux_production_verifier();
            let packs_root = PathBuf::from(required(&options, "--packs-root")?);
            let state_root = PathBuf::from(required(&options, "--state-root")?);
            let store = PackStore::new(&packs_root, &state_root, &verifier);
            let installed =
                store.stage_and_install(&PathBuf::from(required(&options, "--pack-root")?))?;
            println!("{}", serde_json::to_string(&installed)?);
            Ok(())
        }
        Some("check-production-key") => {
            require_exact_options(&options, &["--key-id", "--private-key"])?;
            check_production_signing_key(
                required_utf8(&options, "--key-id")?,
                &PathBuf::from(required(&options, "--private-key")?),
            )?;
            println!("production signing key matches embedded trust");
            Ok(())
        }
        _ => bail!("expected a documented command or --help\n{HELP_TEXT}"),
    }
}

fn linux_production_verifier() -> PackVerifier<'static> {
    static ALLOWED_BACKENDS: [PackBackend; 2] = [PackBackend::Cuda, PackBackend::Vulkan];
    PackVerifier::new(
        &ProductionTrustRoot,
        Compatibility {
            app_build: worker_identity::DESKTOP_BUILD_ID,
            worker_build: worker_identity::INFERENCE_WORKER_BUILD_ID,
            target_os: "linux",
            target_arch: "x86_64",
            allowed_backends: &ALLOWED_BACKENDS,
        },
    )
}

fn run_verify_signed_windows_set(options: &BTreeMap<String, OsString>) -> Result<()> {
    let receipt = if options.contains_key("--worker-source-revision") {
        require_exact_options(
            options,
            &[
                "--policy",
                "--signed-root",
                "--toolchain-manifest",
                "--worker-source-revision",
            ],
        )?;
        verify_signed_windows_set_for_worker_source(
            &PathBuf::from(required(options, "--signed-root")?),
            &PathBuf::from(required(options, "--policy")?),
            &PathBuf::from(required(options, "--toolchain-manifest")?),
            required_utf8(options, "--worker-source-revision")?,
        )?
    } else {
        require_exact_options(
            options,
            &["--policy", "--signed-root", "--toolchain-manifest"],
        )?;
        verify_signed_windows_set(
            &PathBuf::from(required(options, "--signed-root")?),
            &PathBuf::from(required(options, "--policy")?),
            &PathBuf::from(required(options, "--toolchain-manifest")?),
        )?
    };
    println!("{}", serde_json::to_string(&receipt)?);
    Ok(())
}

fn run_author(options: &BTreeMap<String, OsString>) -> Result<()> {
    let request = author_request_from_options(options)?;
    println!("{}", serde_json::to_string(&author_pack(&request)?)?);
    Ok(())
}

fn run_prepare_pack(options: &BTreeMap<String, OsString>) -> Result<()> {
    let request = prepare_request_from_options(options)?;
    println!("{}", serde_json::to_string(&prepare_pack(&request)?)?);
    Ok(())
}

fn run_sign_prepared_pack(options: &BTreeMap<String, OsString>) -> Result<()> {
    let signing = if options.contains_key("--fixture-signing") {
        require_exact_options(
            options,
            &[
                "--expected-manifest-sha256",
                "--expected-pack-digest",
                "--fixture-signing",
                "--pack-root",
            ],
        )?;
        SigningMode::Fixture
    } else {
        require_exact_options(
            options,
            &[
                "--expected-manifest-sha256",
                "--expected-pack-digest",
                "--key-id",
                "--pack-root",
                "--private-key",
            ],
        )?;
        SigningMode::Production {
            key_id: required_utf8(options, "--key-id")?.to_owned(),
            private_key_path: PathBuf::from(required(options, "--private-key")?),
        }
    };
    let descriptor = sign_prepared_pack(
        &PathBuf::from(required(options, "--pack-root")?),
        &signing,
        required_utf8(options, "--expected-manifest-sha256")?,
        required_utf8(options, "--expected-pack-digest")?,
    )?;
    println!("{}", serde_json::to_string(&descriptor)?);
    Ok(())
}

fn prepare_request_from_options(options: &BTreeMap<String, OsString>) -> Result<PrepareRequest> {
    let has_target_option =
        options.contains_key("--target-os") || options.contains_key("--target-arch");
    let mut expected = vec![
        "--backend",
        "--pack-id",
        "--pack-root",
        "--pack-version",
        "--provider",
        "--security-epoch",
    ];
    if has_target_option {
        expected.extend(["--target-arch", "--target-os"]);
    }
    expected.push("--worker-path");
    require_exact_options(options, &expected).map_err(|_| {
        anyhow!("prepare-pack command has unknown or missing options; {AUTHOR_TARGET_CONTRACT}")
    })?;
    let mut author_options = options.clone();
    author_options.insert("--fixture-signing".to_owned(), OsString::new());
    let request = author_request_from_options(&author_options)?;
    Ok(PrepareRequest {
        pack_root: request.pack_root,
        pack_id: request.pack_id,
        pack_version: request.pack_version,
        security_epoch: request.security_epoch,
        backend: request.backend,
        provider: request.provider,
        target_os: request.target_os,
        target_arch: request.target_arch,
        worker_path: request.worker_path,
    })
}

fn author_request_from_options(options: &BTreeMap<String, OsString>) -> Result<AuthorRequest> {
    let fixture = options.contains_key("--fixture-signing");
    let has_target_option =
        options.contains_key("--target-os") || options.contains_key("--target-arch");
    if fixture {
        let mut expected = vec![
            "--backend",
            "--fixture-signing",
            "--pack-id",
            "--pack-root",
            "--pack-version",
            "--provider",
            "--security-epoch",
        ];
        if has_target_option {
            expected.extend(["--target-arch", "--target-os"]);
        }
        expected.push("--worker-path");
        require_exact_options(options, &expected).map_err(|_| {
            anyhow!("author command has unknown or missing options; {AUTHOR_TARGET_CONTRACT}")
        })?;
    } else {
        let mut expected = vec![
            "--backend",
            "--key-id",
            "--pack-id",
            "--pack-root",
            "--pack-version",
            "--private-key",
            "--provider",
            "--security-epoch",
        ];
        if has_target_option {
            expected.extend(["--target-arch", "--target-os"]);
        }
        expected.push("--worker-path");
        require_exact_options(options, &expected).map_err(|_| {
            anyhow!("author command has unknown or missing options; {AUTHOR_TARGET_CONTRACT}")
        })?;
    }
    let backend_value = required_utf8(options, "--backend")?;
    let backend = AuthoringBackend::parse(backend_value).ok_or_else(|| {
        anyhow!("backend must be cuda, vulkan, or metal; {AUTHOR_TARGET_CONTRACT}")
    })?;
    let (target_os, target_arch) = match (options.get("--target-os"), options.get("--target-arch"))
    {
        (None, None) if matches!(backend, AuthoringBackend::Cuda | AuthoringBackend::Vulkan) => {
            ("windows".to_owned(), "x86_64".to_owned())
        }
        (None, None) => bail!(
            "metal authoring requires --target-os and --target-arch; {AUTHOR_TARGET_CONTRACT}"
        ),
        (Some(target_os), Some(target_arch)) => (
            target_os
                .to_str()
                .ok_or_else(|| anyhow!("option --target-os must be UTF-8"))?
                .to_owned(),
            target_arch
                .to_str()
                .ok_or_else(|| anyhow!("option --target-arch must be UTF-8"))?
                .to_owned(),
        ),
        _ => bail!(
            "--target-os and --target-arch must be supplied together; {AUTHOR_TARGET_CONTRACT}"
        ),
    };
    validate_authoring_target(backend, &target_os, &target_arch)?;
    let security_epoch = required_utf8(options, "--security-epoch")?
        .parse::<u64>()
        .map_err(|_| anyhow!("security epoch must be a canonical unsigned integer"))?;
    let signing = if fixture {
        SigningMode::Fixture
    } else {
        SigningMode::Production {
            key_id: required_utf8(options, "--key-id")?.to_owned(),
            private_key_path: PathBuf::from(required(options, "--private-key")?),
        }
    };
    Ok(AuthorRequest {
        pack_root: PathBuf::from(required(options, "--pack-root")?),
        pack_id: required_utf8(options, "--pack-id")?.to_owned(),
        pack_version: required_utf8(options, "--pack-version")?.to_owned(),
        security_epoch,
        backend,
        provider: required_utf8(options, "--provider")?.to_owned(),
        target_os,
        target_arch,
        worker_path: required_utf8(options, "--worker-path")?.to_owned(),
        signing,
    })
}

fn parse_options(arguments: Vec<OsString>) -> Result<BTreeMap<String, OsString>> {
    let mut options = BTreeMap::new();
    let mut index = 0;
    while index < arguments.len() {
        let name = arguments[index]
            .to_str()
            .filter(|value| value.starts_with("--"))
            .ok_or_else(|| anyhow!("option names must be UTF-8 and begin with --"))?
            .to_owned();
        if options.contains_key(&name) {
            bail!("duplicate option {name}");
        }
        if name == "--fixture-signing" {
            options.insert(name, OsString::new());
            index += 1;
            continue;
        }
        let value = arguments
            .get(index + 1)
            .filter(|value| !value.to_string_lossy().starts_with("--"))
            .ok_or_else(|| anyhow!("option {name} requires a value"))?
            .clone();
        options.insert(name, value);
        index += 2;
    }
    Ok(options)
}

fn required<'a>(options: &'a BTreeMap<String, OsString>, name: &str) -> Result<&'a OsStr> {
    options
        .get(name)
        .map(OsString::as_os_str)
        .ok_or_else(|| anyhow!("missing required option {name}"))
}

fn required_utf8<'a>(options: &'a BTreeMap<String, OsString>, name: &str) -> Result<&'a str> {
    required(options, name)?
        .to_str()
        .ok_or_else(|| anyhow!("option {name} must be UTF-8"))
}

fn require_exact_options(options: &BTreeMap<String, OsString>, expected: &[&str]) -> Result<()> {
    let actual = options.keys().map(String::as_str).collect::<Vec<_>>();
    if actual != expected {
        bail!("command has unknown or missing options");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINUX_TEST_APP_VERSION: &str = "9.8.7";
    const LINUX_TEST_BUILD_REVISION: &str = "linux-release-fixture-revision";
    const LINUX_TEST_APP_PROTOCOL: u16 = 17;
    const LINUX_TEST_RUNTIME_ABI: u16 = 23;
    const LINUX_TEST_EPOCH: u64 = 7;

    fn linux_expected_build() -> linux_release_authority::ExpectedBuild<'static> {
        linux_release_authority::ExpectedBuild {
            app_version: LINUX_TEST_APP_VERSION,
            build_revision: LINUX_TEST_BUILD_REVISION,
            app_protocol_version: LINUX_TEST_APP_PROTOCOL,
            runtime_abi_version: LINUX_TEST_RUNTIME_ABI,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn linux_fixture_entry(
        backend: &str,
        pack_id: &str,
        digest: &str,
        provider: &str,
        security_epoch: u64,
        runtime_abi: u16,
        target_os: &str,
        target_arch: &str,
        worker_path: &str,
        inventory: &[&str],
    ) -> (String, String) {
        let root = format!("workers/packs/{pack_id}/1.0.0/{digest}");
        let files = inventory
            .iter()
            .map(|path| format!("{root}/{path}"))
            .collect::<Vec<_>>();
        let files = serde_json::to_string(&files).unwrap();
        let catalog = format!(
            r#"{{"pack_id":"{pack_id}","pack_version":"1.0.0","pack_digest":"{digest}","security_epoch":{security_epoch},"runtime_abi_version":{runtime_abi},"backend":"{backend}","provider":"{provider}","target_os":"{target_os}","target_arch":"{target_arch}","worker_relative_path":"{worker_path}","root":"{root}","installed_size_bytes":4096,"compressed_size_bytes":2048,"files":{files}}}"#
        );
        let authority = format!(
            r#"{{"app_version":"{LINUX_TEST_APP_VERSION}","build_revision":"{LINUX_TEST_BUILD_REVISION}","app_protocol_version":{LINUX_TEST_APP_PROTOCOL},"pack_id":"{pack_id}","pack_version":"1.0.0","pack_digest":"{digest}","security_epoch":{security_epoch},"runtime_abi_version":{runtime_abi},"backend":"{backend}","provider":"{provider}","worker_relative_path":"{worker_path}","root":"{root}","installed_size_bytes":4096,"compressed_size_bytes":2048,"files":{files}}}"#
        );
        (catalog, authority)
    }

    fn linux_fixture_documents(
        catalog_entries: &[String],
        authority_entries: &[String],
        release_epoch: u64,
        authority_target: &str,
    ) -> (Vec<u8>, Vec<u8>) {
        use sha2::{Digest, Sha256};

        let catalog = format!(
            r#"{{"schema_version":1,"packs":[{}]}}"#,
            catalog_entries.join(",")
        )
        .into_bytes();
        let catalog_sha256 = format!("{:x}", Sha256::digest(&catalog));
        let authority = format!(
            r#"{{"schema_version":1,"target":"{authority_target}","catalog_sha256":"{catalog_sha256}","release_security_epoch":{release_epoch},"entries":[{}]}}"#,
            authority_entries.join(",")
        )
        .into_bytes();
        (catalog, authority)
    }

    fn linux_vulkan_fixture(
        security_epoch: u64,
        runtime_abi: u16,
        target_os: &str,
        target_arch: &str,
        provider: &str,
        worker_path: &str,
        inventory: &[&str],
    ) -> (Vec<u8>, Vec<u8>) {
        let (catalog_entry, authority_entry) = linux_fixture_entry(
            "vulkan",
            "vulkan-pack",
            &"b".repeat(64),
            provider,
            security_epoch,
            runtime_abi,
            target_os,
            target_arch,
            worker_path,
            inventory,
        );
        linux_fixture_documents(
            &[catalog_entry],
            &[authority_entry],
            security_epoch,
            "x86_64-unknown-linux-gnu",
        )
    }

    fn linux_binding(catalog: &[u8], authority: &[u8]) -> bool {
        linux_release_authority::validate_catalog_release_binding(
            catalog,
            authority,
            linux_expected_build(),
        )
        .is_some()
    }

    fn linux_rebind_catalog_sha(authority: &[u8], catalog: &[u8]) -> Vec<u8> {
        use sha2::{Digest, Sha256};

        let authority = std::str::from_utf8(authority).unwrap();
        let marker = "\"catalog_sha256\":\"";
        let digest_start = authority.find(marker).unwrap() + marker.len();
        let digest_end = digest_start + 64;
        format!(
            "{}{:x}{}",
            &authority[..digest_start],
            Sha256::digest(catalog),
            &authority[digest_end..]
        )
        .into_bytes()
    }

    #[test]
    fn linux_release_authority_target_selection_preserves_non_linux_defaults() {
        use linux_release_authority_policy::{
            LEGACY_DEFAULT_AUTHORITY_PATH, LINUX_DEFAULT_AUTHORITY_PATH, select_release_authority,
        };

        let supported = select_release_authority("linux", "x86_64", "gnu");
        assert_eq!(supported.default_path, LINUX_DEFAULT_AUTHORITY_PATH);
        assert!(supported.requires_reviewed_linux_empty);
        assert!(supported.linux_gpu_eligible);

        for (arch, target_env) in [("aarch64", "gnu"), ("x86_64", "musl")] {
            let unsupported = select_release_authority("linux", arch, target_env);
            assert_eq!(unsupported.default_path, LINUX_DEFAULT_AUTHORITY_PATH);
            assert!(unsupported.requires_reviewed_linux_empty);
            assert!(!unsupported.linux_gpu_eligible);
        }

        for (target_os, arch, target_env) in [
            ("windows", "x86_64", "msvc"),
            ("windows", "aarch64", "msvc"),
            ("macos", "x86_64", ""),
            ("macos", "aarch64", ""),
            ("freebsd", "x86_64", ""),
        ] {
            let preserved = select_release_authority(target_os, arch, target_env);
            assert_eq!(preserved.default_path, LEGACY_DEFAULT_AUTHORITY_PATH);
            assert!(!preserved.requires_reviewed_linux_empty);
            assert!(!preserved.linux_gpu_eligible);
        }
    }

    #[test]
    fn linux_release_authority_default_emission_validates_empty_runtime_binding() {
        use linux_release_authority_policy::{
            REVIEWED_LINUX_EMPTY_AUTHORITY, emit_selected_authority, select_release_authority,
        };

        let source = include_bytes!(
            "../../../runtime-manifests/gpu-pack-release-authority-linux-empty.json"
        );
        let mut emitted = Vec::new();
        let selection = select_release_authority("linux", "x86_64", "gnu");
        emit_selected_authority(selection, source, |bytes| emitted.extend_from_slice(bytes))
            .unwrap();
        assert_eq!(emitted.as_slice(), REVIEWED_LINUX_EMPTY_AUTHORITY);

        let binding = linux_release_authority::validate_catalog_release_binding(
            br#"{"schema_version":1,"packs":[]}"#,
            &emitted,
            linux_expected_build(),
        )
        .unwrap();
        assert_eq!(binding.release_security_epoch, 0);
        assert_eq!(binding.entry_count, 0);
    }

    #[test]
    fn linux_release_authority_positive_cuda_and_vulkan_fixture_binds_exactly() {
        // These fixtures prove only structural release/catalog binding. They grant no production
        // publication, signature trust, pack admission, or epoch-floor authority.
        let empty_catalog = br#"{"schema_version":1,"packs":[]}"#;
        let structurally_valid_future_metadata = format!(
            r#"{{"schema_version":1,"target":"x86_64-unknown-linux-gnu","catalog_sha256":"c3f19154f1b2265dac92206eae3a35c130a078be46705e1be6032bc442c3b9dc","release_security_epoch":{LINUX_TEST_EPOCH},"entries":[]}}"#
        );
        let future_binding = linux_release_authority::validate_catalog_release_binding(
            empty_catalog,
            structurally_valid_future_metadata.as_bytes(),
            linux_expected_build(),
        )
        .unwrap();
        assert_eq!(future_binding.release_security_epoch, LINUX_TEST_EPOCH);
        assert_eq!(future_binding.entry_count, 0);

        let inventory = ["bin/worker", "pack-manifest.json", "pack-manifest.sig"];
        let (cuda_catalog, cuda_authority) = linux_fixture_entry(
            "cuda",
            "cuda-pack",
            &"a".repeat(64),
            "transcribe-cpp-ggml-cuda",
            LINUX_TEST_EPOCH,
            LINUX_TEST_RUNTIME_ABI,
            "linux",
            "x86_64",
            "bin/worker",
            &inventory,
        );
        let (vulkan_catalog, vulkan_authority) = linux_fixture_entry(
            "vulkan",
            "vulkan-pack",
            &"b".repeat(64),
            "transcribe-cpp-ggml-vulkan",
            LINUX_TEST_EPOCH,
            LINUX_TEST_RUNTIME_ABI,
            "linux",
            "x86_64",
            "bin/worker",
            &inventory,
        );
        let (catalog, authority) = linux_fixture_documents(
            &[cuda_catalog, vulkan_catalog],
            &[cuda_authority, vulkan_authority],
            LINUX_TEST_EPOCH,
            "x86_64-unknown-linux-gnu",
        );
        let binding = linux_release_authority::validate_catalog_release_binding(
            &catalog,
            &authority,
            linux_expected_build(),
        )
        .unwrap();
        assert_eq!(binding.release_security_epoch, LINUX_TEST_EPOCH);
        assert_eq!(binding.entry_count, 2);

        let (version_one_catalog, version_one_authority) = linux_fixture_entry(
            "vulkan",
            "versioned-pack",
            &"c".repeat(64),
            "transcribe-cpp-ggml-vulkan",
            LINUX_TEST_EPOCH,
            LINUX_TEST_RUNTIME_ABI,
            "linux",
            "x86_64",
            "bin/worker",
            &inventory,
        );
        let (new_digest_catalog, new_digest_authority) = linux_fixture_entry(
            "vulkan",
            "versioned-pack",
            &"d".repeat(64),
            "transcribe-cpp-ggml-vulkan",
            LINUX_TEST_EPOCH,
            LINUX_TEST_RUNTIME_ABI,
            "linux",
            "x86_64",
            "bin/worker",
            &inventory,
        );
        let (version_two_catalog, version_two_authority) = linux_fixture_entry(
            "vulkan",
            "versioned-pack",
            &"e".repeat(64),
            "transcribe-cpp-ggml-vulkan",
            LINUX_TEST_EPOCH,
            LINUX_TEST_RUNTIME_ABI,
            "linux",
            "x86_64",
            "bin/worker",
            &inventory,
        );
        let version_two_catalog = version_two_catalog.replace("1.0.0", "2.0.0");
        let version_two_authority = version_two_authority.replace("1.0.0", "2.0.0");
        let (versioned_catalog, versioned_authority) = linux_fixture_documents(
            &[version_one_catalog, new_digest_catalog, version_two_catalog],
            &[
                version_one_authority,
                new_digest_authority,
                version_two_authority,
            ],
            LINUX_TEST_EPOCH,
            "x86_64-unknown-linux-gnu",
        );
        let versioned_binding = linux_release_authority::validate_catalog_release_binding(
            &versioned_catalog,
            &versioned_authority,
            linux_expected_build(),
        )
        .unwrap();
        assert_eq!(versioned_binding.entry_count, 3);

        let maximum_epoch = 9_007_199_254_740_991_u64;
        let (maximum_epoch_catalog, maximum_epoch_authority) = linux_vulkan_fixture(
            maximum_epoch,
            LINUX_TEST_RUNTIME_ABI,
            "linux",
            "x86_64",
            "transcribe-cpp-ggml-vulkan",
            "bin/worker",
            &inventory,
        );
        let maximum_epoch_binding = linux_release_authority::validate_catalog_release_binding(
            &maximum_epoch_catalog,
            &maximum_epoch_authority,
            linux_expected_build(),
        )
        .unwrap();
        assert_eq!(maximum_epoch_binding.release_security_epoch, maximum_epoch);
    }

    #[test]
    fn linux_release_authority_rejects_nonempty_linux_build_inputs() {
        use linux_release_authority_policy::{emit_selected_authority, select_release_authority};

        let (catalog, authority) = linux_vulkan_fixture(
            LINUX_TEST_EPOCH,
            LINUX_TEST_RUNTIME_ABI,
            "linux",
            "x86_64",
            "transcribe-cpp-ggml-vulkan",
            "bin/worker",
            &["bin/worker", "pack-manifest.json", "pack-manifest.sig"],
        );
        assert!(linux_binding(&catalog, &authority));

        let linux = select_release_authority("linux", "x86_64", "gnu");
        let mut absent_output = None;
        assert!(
            emit_selected_authority(linux, &authority, |bytes| {
                absent_output = Some(bytes.to_vec());
            })
            .is_err()
        );
        assert!(absent_output.is_none(), "invalid input created output");

        let mut existing_output = b"existing-sentinel".to_vec();
        assert!(
            emit_selected_authority(linux, &authority, |bytes| {
                existing_output.clear();
                existing_output.extend_from_slice(bytes);
            })
            .is_err()
        );
        assert_eq!(existing_output.as_slice(), b"existing-sentinel");

        let positive_empty =
            std::str::from_utf8(linux_release_authority_policy::REVIEWED_LINUX_EMPTY_AUTHORITY)
                .unwrap()
                .replace(
                    "\"release_security_epoch\":0",
                    "\"release_security_epoch\":7",
                )
                .into_bytes();
        assert!(
            emit_selected_authority(linux, &positive_empty, |_| {
                panic!("positive empty metadata reached production publication")
            })
            .is_err()
        );

        let tampered_default =
            std::str::from_utf8(linux_release_authority_policy::REVIEWED_LINUX_EMPTY_AUTHORITY)
                .unwrap()
                .replace(
                    "\"release_security_epoch\":0,\"entries\":[]",
                    "\"release_security_epoch\":1,\"entries\":[{}]",
                )
                .into_bytes();
        assert!(
            emit_selected_authority(linux, &tampered_default, |_| {
                panic!("invalid positive default reached publication")
            })
            .is_err()
        );

        for legacy_target in [
            select_release_authority("windows", "x86_64", "msvc"),
            select_release_authority("macos", "x86_64", ""),
            select_release_authority("macos", "aarch64", ""),
        ] {
            for (source, expected) in [
                (b"{}\n\n".as_slice(), b"{}\n".as_slice()),
                (b"{\n}\n".as_slice(), b"{\n}".as_slice()),
                (b"{\r}\n".as_slice(), b"{\r}".as_slice()),
                (b"\xef\xbb\xbf{}\n".as_slice(), b"\xef\xbb\xbf{}".as_slice()),
            ] {
                let mut emitted = Vec::new();
                emit_selected_authority(legacy_target, source, |bytes| {
                    emitted.extend_from_slice(bytes);
                })
                .unwrap();
                assert_eq!(emitted.as_slice(), expected);
            }
        }

        for linux_only_rejected in [b"{}\n\n".as_slice(), b"\xef\xbb\xbf{}\n".as_slice()] {
            assert!(
                emit_selected_authority(linux, linux_only_rejected, |_| {
                    panic!("noncanonical Linux bytes reached publication")
                })
                .is_err()
            );
        }
    }

    #[test]
    fn linux_release_authority_rejects_malformed_noncanonical_and_unbounded_documents() {
        use linux_release_authority_policy::{MAX_AUTHORITY_BYTES, normalize_authority_transport};

        let (catalog, authority) = linux_vulkan_fixture(
            LINUX_TEST_EPOCH,
            LINUX_TEST_RUNTIME_ABI,
            "linux",
            "x86_64",
            "transcribe-cpp-ggml-vulkan",
            "bin/worker",
            &["bin/worker", "pack-manifest.json", "pack-manifest.sig"],
        );
        assert!(linux_binding(&catalog, &authority));

        let authority_text = std::str::from_utf8(&authority).unwrap();
        for rejected in [
            format!(" {authority_text}"),
            authority_text.replacen("\"entries\":", "\"unknown\":0,\"entries\":", 1),
            authority_text.replacen(
                "\"schema_version\":1",
                "\"schema_version\":1,\"schema_version\":1",
                1,
            ),
            authority_text.replacen(
                "\"app_version\":",
                "\"nested_unknown\":0,\"app_version\":",
                1,
            ),
            authority_text.replacen(
                "\"app_version\":\"9.8.7\"",
                "\"app_version\":\"9.8.7\",\"app_version\":\"9.8.7\"",
                1,
            ),
            authority_text.replacen(
                "\"release_security_epoch\":7",
                "\"release_security_epoch\":18446744073709551616",
                1,
            ),
        ] {
            assert!(!linux_binding(&catalog, rejected.as_bytes()));
        }
        let short_revision_authority = authority_text
            .replacen(LINUX_TEST_BUILD_REVISION, "short", 1)
            .into_bytes();
        assert!(
            linux_release_authority::validate_catalog_release_binding(
                &catalog,
                &short_revision_authority,
                linux_release_authority::ExpectedBuild {
                    build_revision: "short",
                    ..linux_expected_build()
                },
            )
            .is_none()
        );
        let unsafe_version_authority = authority_text
            .replacen(LINUX_TEST_APP_VERSION, "unsafe version", 1)
            .into_bytes();
        assert!(
            linux_release_authority::validate_catalog_release_binding(
                &catalog,
                &unsafe_version_authority,
                linux_release_authority::ExpectedBuild {
                    app_version: "unsafe version",
                    ..linux_expected_build()
                },
            )
            .is_none()
        );
        let catalog_text = std::str::from_utf8(&catalog).unwrap();
        for rejected in [
            format!("{catalog_text}\n"),
            catalog_text.replacen("\"packs\":", "\"unknown\":0,\"packs\":", 1),
            catalog_text.replacen(
                "\"schema_version\":1",
                "\"schema_version\":1,\"schema_version\":1",
                1,
            ),
            catalog_text.replacen("\"pack_id\":", "\"nested_unknown\":0,\"pack_id\":", 1),
            catalog_text.replacen(
                "\"pack_id\":\"vulkan-pack\"",
                "\"pack_id\":\"vulkan-pack\",\"pack_id\":\"vulkan-pack\"",
                1,
            ),
        ] {
            let rebound_authority = linux_rebind_catalog_sha(&authority, rejected.as_bytes());
            assert!(!linux_binding(rejected.as_bytes(), &rebound_authority));
        }
        let oversized_document = vec![b'x'; MAX_AUTHORITY_BYTES + 1];
        assert!(!linux_binding(&oversized_document, &authority));
        assert!(!linux_binding(&catalog, &oversized_document));
        assert!(normalize_authority_transport(&oversized_document).is_err());

        for rejected_transport in [b"".as_slice(), b"{}\r\n".as_slice(), b"\xff".as_slice()] {
            assert!(normalize_authority_transport(rejected_transport).is_err());
        }

        let mut nine_catalog_entries = Vec::new();
        let mut nine_authority_entries = Vec::new();
        for index in 0..9 {
            let digest = format!("{index:064x}");
            let (catalog_entry, authority_entry) = linux_fixture_entry(
                "vulkan",
                &format!("bounded-pack-{index}"),
                &digest,
                "transcribe-cpp-ggml-vulkan",
                LINUX_TEST_EPOCH,
                LINUX_TEST_RUNTIME_ABI,
                "linux",
                "x86_64",
                "bin/worker",
                &["bin/worker", "pack-manifest.json", "pack-manifest.sig"],
            );
            nine_catalog_entries.push(catalog_entry);
            nine_authority_entries.push(authority_entry);
        }
        let (maximum_catalog, maximum_authority) = linux_fixture_documents(
            &nine_catalog_entries[..8],
            &nine_authority_entries[..8],
            LINUX_TEST_EPOCH,
            "x86_64-unknown-linux-gnu",
        );
        let maximum_binding = linux_release_authority::validate_catalog_release_binding(
            &maximum_catalog,
            &maximum_authority,
            linux_expected_build(),
        )
        .unwrap();
        assert_eq!(maximum_binding.entry_count, 8);
        let (too_many_catalog, too_many_authority) = linux_fixture_documents(
            &nine_catalog_entries,
            &nine_authority_entries,
            LINUX_TEST_EPOCH,
            "x86_64-unknown-linux-gnu",
        );
        assert!(!linux_binding(&too_many_catalog, &too_many_authority));

        let mut oversized_inventory = (0..=manifest::MAX_FILES)
            .map(|index| format!("payload/file-{index:03}"))
            .collect::<Vec<_>>();
        oversized_inventory.extend([
            "bin/worker".to_owned(),
            "pack-manifest.json".to_owned(),
            "pack-manifest.sig".to_owned(),
        ]);
        oversized_inventory.sort();
        let oversized_inventory = oversized_inventory
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        let (oversized_catalog, oversized_authority) = linux_vulkan_fixture(
            LINUX_TEST_EPOCH,
            LINUX_TEST_RUNTIME_ABI,
            "linux",
            "x86_64",
            "transcribe-cpp-ggml-vulkan",
            "bin/worker",
            &oversized_inventory,
        );
        assert!(!linux_binding(&oversized_catalog, &oversized_authority));

        let unsafe_epoch = 9_007_199_254_740_992_u64;
        let (unsafe_catalog, unsafe_authority) = linux_vulkan_fixture(
            unsafe_epoch,
            LINUX_TEST_RUNTIME_ABI,
            "linux",
            "x86_64",
            "transcribe-cpp-ggml-vulkan",
            "bin/worker",
            &["bin/worker", "pack-manifest.json", "pack-manifest.sig"],
        );
        assert!(!linux_binding(&unsafe_catalog, &unsafe_authority));
    }

    #[test]
    fn linux_release_authority_rejects_tampered_build_catalog_target_abi_provider_epoch_and_inventory()
     {
        let inventory = ["bin/worker", "pack-manifest.json", "pack-manifest.sig"];
        let valid = linux_vulkan_fixture(
            LINUX_TEST_EPOCH,
            LINUX_TEST_RUNTIME_ABI,
            "linux",
            "x86_64",
            "transcribe-cpp-ggml-vulkan",
            "bin/worker",
            &inventory,
        );
        assert!(linux_binding(&valid.0, &valid.1));

        let authority_text = std::str::from_utf8(&valid.1).unwrap();
        for tampered in [
            authority_text.replacen(LINUX_TEST_APP_VERSION, "9.8.8", 1),
            authority_text.replacen(LINUX_TEST_BUILD_REVISION, "other-release-revision", 1),
            authority_text.replacen(
                &format!("\"app_protocol_version\":{LINUX_TEST_APP_PROTOCOL}"),
                "\"app_protocol_version\":18",
                1,
            ),
            authority_text.replacen("x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu", 1),
        ] {
            assert!(!linux_binding(&valid.0, tampered.as_bytes()));
        }

        let catalog_without_matching_hash = std::str::from_utf8(&valid.0)
            .unwrap()
            .replacen(
                "\"target_arch\":\"x86_64\"",
                "\"target_arch\":\"aarch64\"",
                1,
            )
            .into_bytes();
        assert!(!linux_binding(&catalog_without_matching_hash, &valid.1));

        let (catalog_entry, authority_entry) = linux_fixture_entry(
            "vulkan",
            "vulkan-pack",
            &"b".repeat(64),
            "transcribe-cpp-ggml-vulkan",
            LINUX_TEST_EPOCH,
            LINUX_TEST_RUNTIME_ABI,
            "linux",
            "x86_64",
            "bin/worker",
            &inventory,
        );
        for (catalog_entry, authority_entry) in [
            (
                catalog_entry.replacen("\"root\":\"workers/packs/", "\"root\":\"workers/other/", 1),
                authority_entry.replacen(
                    "\"root\":\"workers/packs/",
                    "\"root\":\"workers/other/",
                    1,
                ),
            ),
            (
                catalog_entry.replacen(
                    "\"installed_size_bytes\":4096",
                    "\"installed_size_bytes\":0",
                    1,
                ),
                authority_entry.replacen(
                    "\"installed_size_bytes\":4096",
                    "\"installed_size_bytes\":0",
                    1,
                ),
            ),
            (
                catalog_entry.replacen(
                    "\"compressed_size_bytes\":2048",
                    "\"compressed_size_bytes\":4294967297",
                    1,
                ),
                authority_entry.replacen(
                    "\"compressed_size_bytes\":2048",
                    "\"compressed_size_bytes\":4294967297",
                    1,
                ),
            ),
        ] {
            let (catalog, authority) = linux_fixture_documents(
                &[catalog_entry],
                &[authority_entry],
                LINUX_TEST_EPOCH,
                "x86_64-unknown-linux-gnu",
            );
            assert!(!linux_binding(&catalog, &authority));
        }

        let invalid_component = linux_fixture_entry(
            "vulkan",
            "Invalid-Pack",
            &"b".repeat(64),
            "transcribe-cpp-ggml-vulkan",
            LINUX_TEST_EPOCH,
            LINUX_TEST_RUNTIME_ABI,
            "linux",
            "x86_64",
            "bin/worker",
            &inventory,
        );
        let invalid_digest = linux_fixture_entry(
            "vulkan",
            "vulkan-pack",
            &"x".repeat(64),
            "transcribe-cpp-ggml-vulkan",
            LINUX_TEST_EPOCH,
            LINUX_TEST_RUNTIME_ABI,
            "linux",
            "x86_64",
            "bin/worker",
            &inventory,
        );
        let metal_provider = linux_fixture_entry(
            "metal",
            "metal-pack",
            &"c".repeat(64),
            "transcribe-cpp-ggml-metal",
            LINUX_TEST_EPOCH,
            LINUX_TEST_RUNTIME_ABI,
            "linux",
            "x86_64",
            "bin/worker",
            &inventory,
        );
        for (catalog_entry, authority_entry) in [invalid_component, invalid_digest, metal_provider]
        {
            let (catalog, authority) = linux_fixture_documents(
                &[catalog_entry],
                &[authority_entry],
                LINUX_TEST_EPOCH,
                "x86_64-unknown-linux-gnu",
            );
            assert!(!linux_binding(&catalog, &authority));
        }

        let (catalog_entry, authority_entry) = linux_fixture_entry(
            "vulkan",
            "vulkan-pack",
            &"b".repeat(64),
            "transcribe-cpp-ggml-vulkan",
            LINUX_TEST_EPOCH,
            LINUX_TEST_RUNTIME_ABI,
            "linux",
            "x86_64",
            "bin/worker",
            &inventory,
        );
        let (catalog, mismatched_release_epoch) = linux_fixture_documents(
            &[catalog_entry],
            &[authority_entry],
            LINUX_TEST_EPOCH + 1,
            "x86_64-unknown-linux-gnu",
        );
        assert!(!linux_binding(&catalog, &mismatched_release_epoch));

        let (cuda_catalog, cuda_authority) = linux_fixture_entry(
            "cuda",
            "cuda-pack",
            &"a".repeat(64),
            "transcribe-cpp-ggml-cuda",
            LINUX_TEST_EPOCH,
            LINUX_TEST_RUNTIME_ABI,
            "linux",
            "x86_64",
            "bin/worker",
            &inventory,
        );
        let (vulkan_catalog, vulkan_authority) = linux_fixture_entry(
            "vulkan",
            "vulkan-pack",
            &"b".repeat(64),
            "transcribe-cpp-ggml-vulkan",
            LINUX_TEST_EPOCH,
            LINUX_TEST_RUNTIME_ABI,
            "linux",
            "x86_64",
            "bin/worker",
            &inventory,
        );
        let (ordered_catalog, reordered_authority) = linux_fixture_documents(
            &[cuda_catalog, vulkan_catalog],
            &[vulkan_authority, cuda_authority],
            LINUX_TEST_EPOCH,
            "x86_64-unknown-linux-gnu",
        );
        assert!(!linux_binding(&ordered_catalog, &reordered_authority));

        let mismatch_digest = "1".repeat(64);
        let (mismatch_catalog_entry, mismatch_authority_entry) = linux_fixture_entry(
            "vulkan",
            "mismatch-pack",
            &mismatch_digest,
            "transcribe-cpp-ggml-vulkan",
            LINUX_TEST_EPOCH,
            LINUX_TEST_RUNTIME_ABI,
            "linux",
            "x86_64",
            "bin/worker",
            &inventory,
        );
        let size_mismatch_catalog_entry = mismatch_catalog_entry.replacen(
            "\"installed_size_bytes\":4096",
            "\"installed_size_bytes\":4097",
            1,
        );
        let (size_mismatch_catalog, size_mismatch_authority) = linux_fixture_documents(
            &[size_mismatch_catalog_entry],
            &[mismatch_authority_entry.clone()],
            LINUX_TEST_EPOCH,
            "x86_64-unknown-linux-gnu",
        );
        assert!(!linux_binding(
            &size_mismatch_catalog,
            &size_mismatch_authority
        ));

        let mismatch_root = format!("workers/packs/mismatch-pack/1.0.0/{mismatch_digest}");
        let inventory_needle =
            format!("{mismatch_root}/bin/worker\",\"{mismatch_root}/pack-manifest.json");
        let inventory_replacement = format!(
            "{mismatch_root}/bin/worker\",\"{mismatch_root}/docs/readme\",\"{mismatch_root}/pack-manifest.json"
        );
        let extra_inventory_catalog_entry =
            mismatch_catalog_entry.replacen(&inventory_needle, &inventory_replacement, 1);
        let (extra_inventory_catalog, extra_inventory_authority) = linux_fixture_documents(
            &[extra_inventory_catalog_entry],
            &[mismatch_authority_entry],
            LINUX_TEST_EPOCH,
            "x86_64-unknown-linux-gnu",
        );
        assert!(!linux_binding(
            &extra_inventory_catalog,
            &extra_inventory_authority
        ));

        let (duplicate_catalog_entry, duplicate_authority_entry) = linux_fixture_entry(
            "vulkan",
            "duplicate-root-pack",
            &"f".repeat(64),
            "transcribe-cpp-ggml-vulkan",
            LINUX_TEST_EPOCH,
            LINUX_TEST_RUNTIME_ABI,
            "linux",
            "x86_64",
            "bin/worker",
            &inventory,
        );
        let (duplicate_catalog, duplicate_authority) = linux_fixture_documents(
            &[
                duplicate_catalog_entry.clone(),
                duplicate_catalog_entry.clone(),
            ],
            &[
                duplicate_authority_entry.clone(),
                duplicate_authority_entry.clone(),
            ],
            LINUX_TEST_EPOCH,
            "x86_64-unknown-linux-gnu",
        );
        assert!(!linux_binding(&duplicate_catalog, &duplicate_authority));

        let divergent_catalog_entry = duplicate_catalog_entry
            .replacen("\"backend\":\"vulkan\"", "\"backend\":\"cuda\"", 1)
            .replacen("transcribe-cpp-ggml-vulkan", "transcribe-cpp-ggml-cuda", 1);
        let divergent_authority_entry = duplicate_authority_entry
            .replacen("\"backend\":\"vulkan\"", "\"backend\":\"cuda\"", 1)
            .replacen("transcribe-cpp-ggml-vulkan", "transcribe-cpp-ggml-cuda", 1);
        let (divergent_catalog, divergent_authority) = linux_fixture_documents(
            &[duplicate_catalog_entry, divergent_catalog_entry],
            &[duplicate_authority_entry, divergent_authority_entry],
            LINUX_TEST_EPOCH,
            "x86_64-unknown-linux-gnu",
        );
        assert!(!linux_binding(&divergent_catalog, &divergent_authority));

        for (catalog, authority) in [
            linux_vulkan_fixture(
                LINUX_TEST_EPOCH,
                LINUX_TEST_RUNTIME_ABI + 1,
                "linux",
                "x86_64",
                "transcribe-cpp-ggml-vulkan",
                "bin/worker",
                &inventory,
            ),
            linux_vulkan_fixture(
                LINUX_TEST_EPOCH,
                LINUX_TEST_RUNTIME_ABI,
                "linux",
                "aarch64",
                "transcribe-cpp-ggml-vulkan",
                "bin/worker",
                &inventory,
            ),
            linux_vulkan_fixture(
                LINUX_TEST_EPOCH,
                LINUX_TEST_RUNTIME_ABI,
                "linux",
                "x86_64",
                "unexpected-provider",
                "bin/worker",
                &inventory,
            ),
            linux_vulkan_fixture(
                0,
                LINUX_TEST_RUNTIME_ABI,
                "linux",
                "x86_64",
                "transcribe-cpp-ggml-vulkan",
                "bin/worker",
                &inventory,
            ),
            linux_vulkan_fixture(
                LINUX_TEST_EPOCH,
                LINUX_TEST_RUNTIME_ABI,
                "linux",
                "x86_64",
                "transcribe-cpp-ggml-vulkan",
                "../worker",
                &["../worker", "pack-manifest.json", "pack-manifest.sig"],
            ),
            linux_vulkan_fixture(
                LINUX_TEST_EPOCH,
                LINUX_TEST_RUNTIME_ABI,
                "linux",
                "x86_64",
                "transcribe-cpp-ggml-vulkan",
                "bin//worker",
                &["bin//worker", "pack-manifest.json", "pack-manifest.sig"],
            ),
            linux_vulkan_fixture(
                LINUX_TEST_EPOCH,
                LINUX_TEST_RUNTIME_ABI,
                "linux",
                "x86_64",
                "transcribe-cpp-ggml-vulkan",
                "bin/./worker",
                &["bin/./worker", "pack-manifest.json", "pack-manifest.sig"],
            ),
            linux_vulkan_fixture(
                LINUX_TEST_EPOCH,
                LINUX_TEST_RUNTIME_ABI,
                "linux",
                "x86_64",
                "transcribe-cpp-ggml-vulkan",
                "bin/worker",
                &["bin/worker", "bin/worker", "pack-manifest.sig"],
            ),
            linux_vulkan_fixture(
                LINUX_TEST_EPOCH,
                LINUX_TEST_RUNTIME_ABI,
                "linux",
                "x86_64",
                "transcribe-cpp-ggml-vulkan",
                "bin/worker",
                &["pack-manifest.json", "bin/worker", "pack-manifest.sig"],
            ),
            linux_vulkan_fixture(
                LINUX_TEST_EPOCH,
                LINUX_TEST_RUNTIME_ABI,
                "linux",
                "x86_64",
                "transcribe-cpp-ggml-vulkan",
                "bin/worker",
                &[
                    "Bin/worker",
                    "bin/worker",
                    "pack-manifest.json",
                    "pack-manifest.sig",
                ],
            ),
            linux_vulkan_fixture(
                LINUX_TEST_EPOCH,
                LINUX_TEST_RUNTIME_ABI,
                "linux",
                "x86_64",
                "transcribe-cpp-ggml-vulkan",
                "bin/worker",
                &["bin/worker", "pack-manifest.json"],
            ),
        ] {
            assert!(!linux_binding(&catalog, &authority));
        }
    }

    fn fixture_options(extra: &[&str]) -> BTreeMap<String, OsString> {
        let mut arguments = vec![
            "--backend",
            "vulkan",
            "--fixture-signing",
            "--pack-id",
            "test-pack",
            "--pack-root",
            "test-root",
            "--pack-version",
            "1.0.0",
            "--provider",
            "transcribe-cpp-ggml-vulkan",
            "--security-epoch",
            "1",
            "--worker-path",
            "bin/worker",
        ];
        arguments.extend_from_slice(extra);
        parse_options(arguments.into_iter().map(OsString::from).collect()).unwrap()
    }

    fn explicit_target_options(
        backend: &str,
        target_os: &str,
        target_arch: &str,
    ) -> BTreeMap<String, OsString> {
        let mut options =
            fixture_options(&["--target-os", target_os, "--target-arch", target_arch]);
        options.insert("--backend".to_owned(), OsString::from(backend));
        options
    }

    #[test]
    fn legacy_windows_cuda_and_vulkan_default_to_x86_64() {
        for backend in ["cuda", "vulkan"] {
            let mut options = fixture_options(&[]);
            options.insert("--backend".to_owned(), OsString::from(backend));
            let request = author_request_from_options(&options).unwrap();
            assert_eq!(request.target_os, "windows");
            assert_eq!(request.target_arch, "x86_64");
        }
    }

    #[test]
    fn explicit_macos_metal_targets_accept_both_production_architectures() {
        for arch in ["aarch64", "x86_64"] {
            let options = explicit_target_options("metal", "macos", arch);
            let request = author_request_from_options(&options).unwrap();
            assert_eq!(request.backend, AuthoringBackend::Metal);
            assert_eq!(request.target_os, "macos");
            assert_eq!(request.target_arch, arch);
        }
    }

    #[test]
    fn explicit_linux_gpu_targets_accept_x86_64() {
        for backend in ["cuda", "vulkan"] {
            let options = explicit_target_options(backend, "linux", "x86_64");
            let request = author_request_from_options(&options).unwrap();
            assert_eq!(request.target_os, "linux");
            assert_eq!(request.target_arch, "x86_64");
        }
    }

    #[test]
    fn cli_rejects_missing_or_incoherent_backend_targets() {
        let mut missing_target = fixture_options(&[]);
        missing_target.insert("--backend".to_owned(), OsString::from("metal"));
        assert!(
            author_request_from_options(&missing_target)
                .unwrap_err()
                .to_string()
                .contains(AUTHOR_TARGET_CONTRACT)
        );

        let mut one_target = fixture_options(&["--target-os", "macos"]);
        one_target.insert("--backend".to_owned(), OsString::from("metal"));
        assert!(
            author_request_from_options(&one_target)
                .unwrap_err()
                .to_string()
                .contains(AUTHOR_TARGET_CONTRACT)
        );

        for (backend, target_os, target_arch) in [
            ("metal", "windows", "x86_64"),
            ("cuda", "macos", "aarch64"),
            ("vulkan", "macos", "x86_64"),
            ("metal", "macos", "arm64"),
            ("metal", "linux", "x86_64"),
            ("cuda", "linux", "aarch64"),
            ("vulkan", "linux", "arm64"),
            ("Metal", "macos", "aarch64"),
            ("metal", "MacOS", "aarch64"),
            ("metal", "macos", ""),
        ] {
            let options = explicit_target_options(backend, target_os, target_arch);
            assert!(
                author_request_from_options(&options)
                    .unwrap_err()
                    .to_string()
                    .contains(AUTHOR_TARGET_CONTRACT)
            );
        }
    }

    #[test]
    fn prepare_pack_rejects_signing_options() {
        let fixture = fixture_options(&[]);
        let error = prepare_request_from_options(&fixture)
            .unwrap_err()
            .to_string();
        assert!(error.contains("prepare-pack command has unknown or missing options"));

        let mut production = fixture;
        production.remove("--fixture-signing");
        production.insert("--key-id".to_owned(), OsString::from("production-key"));
        production.insert(
            "--private-key".to_owned(),
            OsString::from("production-key.pk8"),
        );
        let error = prepare_request_from_options(&production)
            .unwrap_err()
            .to_string();
        assert!(error.contains("prepare-pack command has unknown or missing options"));
    }

    #[test]
    fn help_enumerates_the_exact_authoring_contract() {
        assert!(HELP_TEXT.contains("cuda or vulkan on windows/x86_64"));
        assert!(HELP_TEXT.contains("linux/x86_64"));
        assert!(HELP_TEXT.contains("metal on macos/aarch64 or macos/x86_64"));
        assert!(HELP_TEXT.contains("defaults to windows/x86_64"));
        assert!(HELP_TEXT.contains("verify-production-linux"));
        assert!(HELP_TEXT.contains("install-production-linux"));
        assert!(HELP_TEXT.contains(
            "verify-signed-windows-set --signed-root <path> --policy <path> --toolchain-manifest <path> [--worker-source-revision <40-lowercase-hex>]"
        ));
    }

    #[test]
    fn verify_signed_windows_set_cli_rejects_malformed_worker_source_revision() {
        let revisions = vec![String::new(), "A".repeat(40), "a".repeat(39)];
        for revision in revisions {
            let options = parse_options(
                vec![
                    "--policy",
                    "missing-policy.json",
                    "--signed-root",
                    "missing-signed-root",
                    "--toolchain-manifest",
                    "missing-toolchain.json",
                    "--worker-source-revision",
                    revision.as_str(),
                ]
                .into_iter()
                .map(OsString::from)
                .collect(),
            )
            .unwrap();
            let error = run_verify_signed_windows_set(&options)
                .unwrap_err()
                .to_string();
            assert!(error.contains("expected worker source revision"));
        }
    }

    #[test]
    fn linux_production_verifier_has_no_fixture_trust() {
        let root = std::env::temp_dir().join(format!(
            "scribe-linux-production-trust-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::write(root.join("bin/scribe-inference-worker"), b"worker").unwrap();
        let request = AuthorRequest {
            pack_root: root.clone(),
            pack_id: "scribe-vulkan-linux-x64".to_owned(),
            pack_version: "1.0.0-fixture".to_owned(),
            security_epoch: 1,
            backend: AuthoringBackend::Vulkan,
            provider: "transcribe-cpp-ggml-vulkan".to_owned(),
            target_os: "linux".to_owned(),
            target_arch: "x86_64".to_owned(),
            worker_path: "bin/scribe-inference-worker".to_owned(),
            signing: SigningMode::Fixture,
        };
        author_pack(&request).unwrap();
        let error = linux_production_verifier().verify(&root).unwrap_err();
        assert!(matches!(error, manifest::PackVerificationError::UnknownKey));
        std::fs::remove_dir_all(root).unwrap();
    }
}
