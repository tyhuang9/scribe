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
    #[serde(default)]
    archive_layout: ArchiveLayout,
    #[serde(default)]
    upstream_repository: Option<String>,
    #[serde(default)]
    upstream_revision: Option<String>,
    #[serde(default)]
    upstream_asset: Option<String>,
    #[serde(default)]
    upstream_sha256: Option<String>,
    #[serde(default)]
    upstream_size_bytes: Option<u64>,
    #[serde(default)]
    license: Option<String>,
    #[serde(default)]
    license_sha256: Option<String>,
}

#[derive(Clone, Copy, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum ArchiveLayout {
    #[default]
    ScribePortableZipV1,
    UpstreamLlamaCppFlatZipV1,
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
    license_sha256: String,
    #[serde(default)]
    url: Option<String>,
    sha256: String,
    size_bytes: u64,
    managed_relative_path: String,
}

fn main() {
    const CATALOG_ENV: &str = "SCRIBE_RUNTIME_ARTIFACT_CATALOG";
    const REQUIRE_VOICE_AI_ENV: &str = "SCRIBE_REQUIRE_VOICE_INTENT_ARTIFACTS";
    let source = env::var_os(CATALOG_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("runtime-artifacts.default.json"));
    let destination = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set"))
        .join("runtime-artifacts.json");

    println!("cargo:rerun-if-env-changed={CATALOG_ENV}");
    println!("cargo:rerun-if-env-changed={REQUIRE_VOICE_AI_ENV}");
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
    if env::var(REQUIRE_VOICE_AI_ENV).as_deref() == Ok("1") {
        let target_os = env::var("CARGO_CFG_TARGET_OS").expect("Cargo sets target OS");
        let target_arch = env::var("CARGO_CFG_TARGET_ARCH").expect("Cargo sets target arch");
        validate_required_voice_intent_artifacts(&contents, &target_os, &target_arch)
            .unwrap_or_else(|err| {
                panic!(
                    "voice-AI release catalog {} is incomplete: {err}",
                    source.display()
                )
            });
    }
    fs::write(&destination, contents).unwrap_or_else(|err| {
        panic!(
            "failed to embed runtime artifact catalog {}: {err}",
            source.display()
        )
    });
}

fn validate_required_voice_intent_artifacts(
    contents: &str,
    target_os: &str,
    target_arch: &str,
) -> Result<(), String> {
    let catalog: Catalog = serde_json::from_str(contents).map_err(|err| err.to_string())?;
    if catalog.schema_version != 2 {
        return Err("schema version 2 is required".to_owned());
    }
    let runtime_ready = catalog.artifacts.iter().any(|artifact| {
        artifact.runtime_id == "voice_intent_llama_cpp"
            && artifact.os == target_os
            && artifact.arch == target_arch
            && artifact.device == "cpu"
    });
    if !runtime_ready {
        return Err(format!(
            "missing voice_intent_llama_cpp CPU runtime for {target_os}-{target_arch}"
        ));
    }

    for tier in ["compact", "balanced"] {
        let Some(model) = catalog
            .intent_models
            .iter()
            .find(|model| model.tier == tier)
        else {
            return Err(format!("missing {tier} voice intent model"));
        };
        if model.url.is_none() {
            return Err(format!(
                "{tier} voice intent model lacks a direct release URL"
            ));
        }
    }
    Ok(())
}

pub(crate) fn validate_catalog(contents: &str) -> Result<(), String> {
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
        validate_voice_runtime_provenance(&artifact)?;
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
        validate_sha256(&model.license_sha256, "intent model license")?;
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
        validate_approved_intent_model(&model)?;
        if !intent_model_ids.insert(model.model_id.clone())
            || !intent_model_tiers.insert((model.runtime_id, model.tier))
        {
            return Err("duplicate intent model id or runtime/tier tuple".to_owned());
        }
    }
    Ok(())
}

fn validate_approved_intent_model(model: &IntentModel) -> Result<(), String> {
    let expected = match model.tier.as_str() {
        "compact" => (
            "qwen3_0_6b_q8_0",
            "Qwen3-0.6B",
            "Qwen/Qwen3-0.6B-GGUF",
            "ef4088322893040952513f532f736ddeab518403",
            "Qwen3-0.6B-Q8_0.gguf",
            "https://huggingface.co/Qwen/Qwen3-0.6B-GGUF/resolve/ef4088322893040952513f532f736ddeab518403/Qwen3-0.6B-Q8_0.gguf",
            804_753_088_u64,
            "12fae8b8f78f0360b498d04c8db7d33aff29ab7d8080231f93a17c18119e6735",
            "voice-intent/Qwen3-0.6B-Q8_0.gguf",
        ),
        "balanced" => (
            "qwen3_1_7b_q8_0",
            "Qwen3-1.7B",
            "Qwen/Qwen3-1.7B-GGUF",
            "90862c4b9d2787eaed51d12237eafdfe7c5f6077",
            "Qwen3-1.7B-Q8_0.gguf",
            "https://huggingface.co/Qwen/Qwen3-1.7B-GGUF/resolve/90862c4b9d2787eaed51d12237eafdfe7c5f6077/Qwen3-1.7B-Q8_0.gguf",
            1_834_426_016_u64,
            "061b54daade076b5d3362dac252678d17da8c68f07560be70818cace6590cb1a",
            "voice-intent/Qwen3-1.7B-Q8_0.gguf",
        ),
        _ => return Err(format!("unsupported intent model tier {:?}", model.tier)),
    };
    if model.runtime_id != "voice_intent_llama_cpp"
        || model.model_id != expected.0
        || model.version != expected.1
        || model.upstream_repository != expected.2
        || model.upstream_revision != expected.3
        || model.upstream_filename != expected.4
        || model.url.as_deref() != Some(expected.5)
        || model.size_bytes != expected.6
        || model.sha256 != expected.7
        || model.managed_relative_path != expected.8
        || model.license != "Apache-2.0"
        || model.license_sha256
            != "5de36594c10839788a8c589443a8ef9d8b8d17c65a1b5807206ae037fc36c6bd"
    {
        return Err(format!(
            "{} voice intent model must match the approved Qwen artifact",
            model.tier
        ));
    }
    Ok(())
}

fn validate_voice_runtime_provenance(artifact: &Artifact) -> Result<(), String> {
    let provenance = (
        artifact.upstream_repository.as_deref(),
        artifact.upstream_revision.as_deref(),
        artifact.upstream_asset.as_deref(),
        artifact.upstream_sha256.as_deref(),
        artifact.upstream_size_bytes,
        artifact.license.as_deref(),
        artifact.license_sha256.as_deref(),
    );
    if artifact.runtime_id != "voice_intent_llama_cpp" {
        if provenance == (None, None, None, None, None, None, None)
            && artifact.archive_layout == ArchiveLayout::ScribePortableZipV1
        {
            return Ok(());
        }
        return Err(
            "upstream runtime provenance is reserved for voice_intent_llama_cpp".to_owned(),
        );
    }
    let expected = (
        Some("ggml-org/llama.cpp"),
        Some("aedb2a5e9ca3d4064148bbb919e0ddc0c1b70ab3"),
        Some("llama-b9637-bin-win-cpu-x64.zip"),
        Some("f7783c2b8c007f95e710ac40f26a24861a80b603b0b739fc54d7c926a4716c1e"),
        Some(16_906_751_u64),
        Some("MIT"),
        Some("94f29bbed6a22c35b992c5c6ebf0e7c92f13b836b90f36f461c9cf2f0f1d010d"),
    );
    if artifact.version != "b9637"
        || artifact.os != "windows"
        || artifact.arch != "x86_64"
        || artifact.device != "cpu"
        || artifact.url
            != "https://github.com/ggml-org/llama.cpp/releases/download/b9637/llama-b9637-bin-win-cpu-x64.zip"
        || artifact.sha256 != "f7783c2b8c007f95e710ac40f26a24861a80b603b0b739fc54d7c926a4716c1e"
        || artifact.size_bytes != 16_906_751
        || artifact.unpacked_size_bytes != 43_983_896
        || artifact.entrypoint != "bin/llama-server.exe"
        || artifact.archive_layout != ArchiveLayout::UpstreamLlamaCppFlatZipV1
        || provenance != expected
    {
        return Err(
            "voice_intent_llama_cpp must carry the approved b9637 upstream asset and MIT license provenance"
                .to_owned(),
        );
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
