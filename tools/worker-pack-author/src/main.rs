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
#[path = "../../../src/worker_identity.rs"]
mod worker_identity;
#[path = "../../../src/worker_pack_authoring.rs"]
mod worker_pack_authoring;

use manifest::{Compatibility, PackBackend, PackVerifier, ProductionTrustRoot};
use store::PackStore;
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
