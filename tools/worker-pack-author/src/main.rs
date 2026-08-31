#![cfg(not(test))]

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
mod onnx_worker {
    pub(crate) use crate::worker_identity::{
        DESKTOP_BUILD_ID, INFERENCE_WORKER_BUILD_ID, PROTOCOL_VERSION, WORKER_ABI_VERSION,
    };
}
#[path = "../../../src/worker_identity.rs"]
mod worker_identity;
#[path = "../../../src/worker_pack_authoring.rs"]
mod worker_pack_authoring;

use worker_pack_authoring::{
    AuthorRequest, AuthoringBackend, SigningMode, author_pack, check_production_signing_key,
    verify_fixture_pack,
};

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
        .ok_or_else(|| anyhow!("expected author, verify-fixture, or check-production-key"))?;
    let options = parse_options(arguments.collect())?;
    match command.to_str() {
        Some("author") => run_author(&options),
        Some("verify-fixture") => {
            require_exact_options(&options, &["--pack-root"])?;
            let descriptor =
                verify_fixture_pack(&PathBuf::from(required(&options, "--pack-root")?))?;
            println!("{}", serde_json::to_string(&descriptor)?);
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
        _ => bail!("expected author, verify-fixture, or check-production-key"),
    }
}

fn run_author(options: &BTreeMap<String, OsString>) -> Result<()> {
    let fixture = options.contains_key("--fixture-signing");
    if fixture {
        require_exact_options(
            options,
            &[
                "--backend",
                "--fixture-signing",
                "--pack-id",
                "--pack-root",
                "--pack-version",
                "--provider",
                "--security-epoch",
                "--worker-path",
            ],
        )?;
    } else {
        require_exact_options(
            options,
            &[
                "--backend",
                "--key-id",
                "--pack-id",
                "--pack-root",
                "--pack-version",
                "--private-key",
                "--provider",
                "--security-epoch",
                "--worker-path",
            ],
        )?;
    }
    let backend_value = required_utf8(options, "--backend")?;
    let backend = AuthoringBackend::parse(backend_value)
        .ok_or_else(|| anyhow!("backend must be cuda or vulkan"))?;
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
    let request = AuthorRequest {
        pack_root: PathBuf::from(required(options, "--pack-root")?),
        pack_id: required_utf8(options, "--pack-id")?.to_owned(),
        pack_version: required_utf8(options, "--pack-version")?.to_owned(),
        security_epoch,
        backend,
        provider: required_utf8(options, "--provider")?.to_owned(),
        worker_path: required_utf8(options, "--worker-path")?.to_owned(),
        signing,
    };
    println!("{}", serde_json::to_string(&author_pack(&request)?)?);
    Ok(())
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
