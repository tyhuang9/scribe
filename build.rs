use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::PathBuf;

use serde::Deserialize;

const MAX_ARCHIVE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_UNPACKED_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const RUNTIME_IDS: &[&str] = &[
    "whisper_cpp",
    "faster_whisper",
    "vosk",
    "sherpa_onnx",
    "moonshine",
    "parakeet",
    "voice_intent_llama_cpp",
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    schema_version: u32,
    catalog_version: String,
    artifacts: Vec<Artifact>,
    #[serde(default)]
    intent_models: Vec<IntentModel>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Artifact {
    runtime_id: String,
    version: String,
    os: String,
    arch: String,
    device: String,
    url: String,
    sha256: String,
    size_bytes: u64,
    unpacked_size_bytes: u64,
    entrypoint: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IntentModel {
    runtime_id: String,
    tier: String,
    model_id: String,
    version: String,
    upstream_repository: String,
    upstream_revision: String,
    upstream_filename: String,
    license: String,
    #[serde(default)]
    url: Option<String>,
    sha256: String,
    size_bytes: u64,
    managed_relative_path: String,
}

fn main() {
    const CATALOG_ENV: &str = "SCRIBE_RUNTIME_ARTIFACT_CATALOG";
    let source = env::var_os(CATALOG_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("runtime-artifacts.default.json"));
    let destination = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set"))
        .join("runtime-artifacts.json");

    println!("cargo:rerun-if-env-changed={CATALOG_ENV}");
    println!("cargo:rerun-if-changed={}", source.display());
    let contents = fs::read_to_string(&source).unwrap_or_else(|err| {
        panic!(
            "failed to read runtime artifact catalog {}: {err}",
            source.display()
        )
    });
    validate_catalog(&contents).unwrap_or_else(|err| {
        panic!(
            "refusing to embed invalid runtime artifact catalog {}: {err}",
            source.display()
        )
    });
    fs::write(&destination, contents).unwrap_or_else(|err| {
        panic!(
            "failed to embed runtime artifact catalog {}: {err}",
            source.display()
        )
    });
}

fn validate_catalog(contents: &str) -> Result<(), String> {
    let catalog: Catalog = serde_json::from_str(contents).map_err(|err| err.to_string())?;
    if !matches!(catalog.schema_version, 1 | 2) || catalog.catalog_version.trim().is_empty() {
        return Err("unsupported schema or empty catalog version".to_owned());
    }
    let mut keys = HashSet::new();
    for artifact in catalog.artifacts {
        if !RUNTIME_IDS.contains(&artifact.runtime_id.as_str()) {
            return Err(format!("unsupported runtime id {:?}", artifact.runtime_id));
        }
        if artifact.version.is_empty()
            || artifact.version.len() > 128
            || !artifact.version.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-')
            })
        {
            return Err(format!("unsafe version for {}", artifact.runtime_id));
        }
        if !matches!(artifact.os.as_str(), "linux" | "macos" | "windows")
            || !matches!(artifact.arch.as_str(), "x86_64" | "aarch64")
            || !matches!(artifact.device.as_str(), "cpu" | "gpu")
        {
            return Err(format!(
                "unsupported platform tuple for {}",
                artifact.runtime_id
            ));
        }
        if artifact.device == "gpu"
            && !matches!(
                artifact.runtime_id.as_str(),
                "whisper_cpp" | "faster_whisper"
            )
        {
            return Err(format!(
                "{} does not support GPU packs",
                artifact.runtime_id
            ));
        }
        validate_url(&artifact.url)?;
        if artifact.sha256.len() != 64
            || !artifact
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(format!("invalid SHA-256 for {}", artifact.runtime_id));
        }
        if artifact.size_bytes == 0
            || artifact.size_bytes > MAX_ARCHIVE_BYTES
            || artifact.unpacked_size_bytes == 0
            || artifact.unpacked_size_bytes > MAX_UNPACKED_BYTES
        {
            return Err(format!("invalid size limits for {}", artifact.runtime_id));
        }
        validate_entrypoint(&artifact.entrypoint)?;
        let key = (
            artifact.runtime_id.clone(),
            artifact.os.clone(),
            artifact.arch.clone(),
            artifact.device.clone(),
        );
        if !keys.insert(key) {
            return Err(format!(
                "duplicate artifact tuple for {}",
                artifact.runtime_id
            ));
        }
    }
    if catalog.schema_version == 1 && !catalog.intent_models.is_empty() {
        return Err("schema version 1 cannot contain voice intent models".to_owned());
    }
    let mut intent_model_ids = HashSet::new();
    let mut intent_model_tiers = HashSet::new();
    for model in catalog.intent_models {
        if model.runtime_id != "voice_intent_llama_cpp" {
            return Err(format!(
                "unsupported intent runtime id {:?}",
                model.runtime_id
            ));
        }
        if !matches!(model.tier.as_str(), "compact" | "balanced") {
            return Err(format!("unsupported intent model tier {:?}", model.tier));
        }
        validate_identifier(&model.model_id, "intent model id")?;
        validate_immutable_identifier(&model.version, "intent model version")?;
        validate_repository(&model.upstream_repository)?;
        if model.upstream_revision.len() != 40
            || !model
                .upstream_revision
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(
                "intent model upstream revision must be a pinned lowercase Git revision".to_owned(),
            );
        }
        validate_entrypoint(&model.upstream_filename)?;
        validate_gguf_path(&model.upstream_filename, "intent model upstream filename")?;
        validate_immutable_identifier(&model.license, "intent model license")?;
        if let Some(url) = &model.url {
            validate_url(url)?;
        }
        validate_sha256(&model.sha256, "intent model")?;
        if model.size_bytes == 0 || model.size_bytes > MAX_ARCHIVE_BYTES {
            return Err(format!(
                "invalid size limit for intent model {}",
                model.model_id
            ));
        }
        validate_entrypoint(&model.managed_relative_path)?;
        validate_gguf_path(&model.managed_relative_path, "intent model managed path")?;
        if !intent_model_ids.insert(model.model_id.clone())
            || !intent_model_tiers.insert((model.runtime_id, model.tier))
        {
            return Err("duplicate intent model id or runtime/tier tuple".to_owned());
        }
    }
    Ok(())
}

fn validate_identifier(value: &str, label: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(format!("{label} is unsafe"));
    }
    Ok(())
}

fn validate_immutable_identifier(value: &str, label: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-'))
    {
        return Err(format!("{label} is unsafe"));
    }
    Ok(())
}

fn validate_repository(value: &str) -> Result<(), String> {
    let mut parts = value.split('/');
    let Some(owner) = parts.next() else {
        return Err("intent model upstream repository is invalid".to_owned());
    };
    let Some(repository) = parts.next() else {
        return Err("intent model upstream repository is invalid".to_owned());
    };
    if parts.next().is_some()
        || owner.is_empty()
        || repository.is_empty()
        || !owner
            .bytes()
            .chain(repository.bytes())
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err("intent model upstream repository is invalid".to_owned());
    }
    Ok(())
}

fn validate_sha256(value: &str, label: &str) -> Result<(), String> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(format!("invalid SHA-256 for {label}"));
    }
    Ok(())
}

fn validate_gguf_path(value: &str, label: &str) -> Result<(), String> {
    if value
        .rsplit_once('.')
        .is_none_or(|(_, extension)| !extension.eq_ignore_ascii_case("gguf"))
    {
        return Err(format!("{label} must end in .gguf"));
    }
    Ok(())
}

fn validate_url(value: &str) -> Result<(), String> {
    let parsed = url::Url::parse(value).map_err(|err| format!("invalid URL: {err}"))?;
    let host = parsed.host_str().unwrap_or_default().to_ascii_lowercase();
    let loopback = match parsed.host() {
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        _ => false,
    };
    let reserved = host == "localhost"
        || host.ends_with(".localhost")
        || loopback
        || host.ends_with(".invalid")
        || host.ends_with(".test")
        || host.ends_with(".example")
        || ["example.com", "example.net", "example.org"]
            .iter()
            .any(|value| host == *value || host.ends_with(&format!(".{value}")));
    if parsed.scheme() != "https"
        || host.is_empty()
        || reserved
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(
            "artifact URL must be a real HTTPS URL without credentials, query, or fragment"
                .to_owned(),
        );
    }
    Ok(())
}

fn validate_entrypoint(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.starts_with('/')
        || value.contains('\\')
        || value.split('/').any(|part| {
            part.is_empty()
                || matches!(part, "." | "..")
                || part.contains(':')
                || part.ends_with(' ')
                || part.ends_with('.')
                || part.chars().any(char::is_control)
        })
    {
        return Err("entrypoint must be a normalized portable relative path".to_owned());
    }
    for component in value.split('/') {
        let base = component.split('.').next().unwrap_or_default();
        if matches!(
            base.to_ascii_uppercase().as_str(),
            "CON"
                | "PRN"
                | "AUX"
                | "NUL"
                | "COM1"
                | "COM2"
                | "COM3"
                | "COM4"
                | "COM5"
                | "COM6"
                | "COM7"
                | "COM8"
                | "COM9"
                | "LPT1"
                | "LPT2"
                | "LPT3"
                | "LPT4"
                | "LPT5"
                | "LPT6"
                | "LPT7"
                | "LPT8"
                | "LPT9"
        ) {
            return Err("entrypoint contains a reserved Windows path component".to_owned());
        }
    }
    Ok(())
}
