use std::collections::HashSet;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{Read, Seek, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(test)]
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{config, runtime_catalog};

const EMBEDDED_CATALOG_JSON: &str =
    include_str!(concat!(env!("OUT_DIR"), "/runtime-artifacts.json"));
const MAX_ARCHIVE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const MAX_UNPACKED_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const MAX_DOWNLOAD_DURATION: Duration = Duration::from_secs(2 * 60 * 60);
const MAX_REDIRECTS: usize = 3;
const MAX_REDIRECT_LOCATION_BYTES: usize = 8 * 1024;
pub(crate) const VOICE_INTENT_LLAMA_CPP_RUNTIME_ID: &str = "voice_intent_llama_cpp";
const LLAMA_CPP_OFFICIAL_URL: &str =
    "https://github.com/ggml-org/llama.cpp/releases/download/b9637/llama-b9637-bin-win-cpu-x64.zip";
const LLAMA_CPP_UPSTREAM_ENTRY_COUNT: usize = 51;
const LLAMA_CPP_UPSTREAM_UNPACKED_SIZE: u64 = 43_983_896;
const LLAMA_CPP_SELECTED_DLL_COUNT: usize = 29;
const LLAMA_CPP_SELECTED_PAYLOAD_SIZE: u64 = 42_545_688;
const LLAMA_CPP_LICENSE: &[u8] = include_bytes!("../assets/licenses/llama.cpp-MIT.txt");
const INTENT_MODEL_LOCK_TIMEOUT: Duration = Duration::from_secs(10);
static INTENT_MODEL_PARTIAL_NONCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DownloadProgress {
    pub(crate) downloaded_bytes: u64,
    pub(crate) total_bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DownloadControl {
    Continue,
    Cancel,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum RuntimeDevicePack {
    Cpu,
    Gpu,
}

impl RuntimeDevicePack {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Gpu => "gpu",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RuntimeArtifact {
    pub(crate) runtime_id: String,
    pub(crate) version: String,
    pub(crate) os: String,
    pub(crate) arch: String,
    pub(crate) device: RuntimeDevicePack,
    pub(crate) url: String,
    pub(crate) sha256: String,
    pub(crate) size_bytes: u64,
    pub(crate) unpacked_size_bytes: u64,
    pub(crate) entrypoint: PathBuf,
    #[serde(default)]
    pub(crate) archive_layout: ArchiveLayout,
    #[serde(default)]
    pub(crate) upstream_repository: Option<String>,
    #[serde(default)]
    pub(crate) upstream_revision: Option<String>,
    #[serde(default)]
    pub(crate) upstream_asset: Option<String>,
    #[serde(default)]
    pub(crate) upstream_sha256: Option<String>,
    #[serde(default)]
    pub(crate) upstream_size_bytes: Option<u64>,
    #[serde(default)]
    pub(crate) license: Option<String>,
    #[serde(default)]
    pub(crate) license_sha256: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ArchiveLayout {
    #[default]
    ScribePortableZipV1,
    UpstreamLlamaCppFlatZipV1,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StagedRuntimeArtifact {
    pub(crate) root: PathBuf,
    pub(crate) entrypoint: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum IntentModelTier {
    Compact,
    Balanced,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IntentModelArtifact {
    pub(crate) runtime_id: String,
    pub(crate) tier: IntentModelTier,
    pub(crate) model_id: String,
    pub(crate) version: String,
    pub(crate) upstream_repository: String,
    pub(crate) upstream_revision: String,
    pub(crate) upstream_filename: String,
    pub(crate) license: String,
    pub(crate) license_sha256: String,
    #[serde(default)]
    pub(crate) url: Option<String>,
    pub(crate) sha256: String,
    pub(crate) size_bytes: u64,
    pub(crate) managed_relative_path: PathBuf,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum IntentModelTransactionPhase {
    Prepared,
    BackedUp,
    Activated,
    AwaitingPersistence,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct IntentModelTransactionJournal {
    version: u32,
    model_id: String,
    phase: IntentModelTransactionPhase,
    had_previous_model: bool,
    previous_sha256: Option<String>,
    previous_size_bytes: Option<u64>,
    previous_install: Option<config::ManagedModelInstall>,
    new_install: Option<config::ManagedModelInstall>,
    expected_sha256: Option<String>,
    expected_size_bytes: Option<u64>,
}

#[derive(Debug)]
struct IntentModelInstallLock {
    _file: File,
    previous_install: Option<config::ManagedModelInstall>,
}

#[derive(Debug)]
pub(crate) struct IntentModelReplacement {
    pub(crate) installed_path: PathBuf,
    model_id: String,
    backup_path: Option<PathBuf>,
    previous_sha256: Option<String>,
    previous_size_bytes: Option<u64>,
    expected_sha256: Option<String>,
    expected_size_bytes: Option<u64>,
    persistence_install: Option<Option<config::ManagedModelInstall>>,
    _lock: IntentModelInstallLock,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RuntimeArtifactManifest {
    manifest_version: u32,
    runtime_id: String,
    version: String,
    platform: String,
    device: RuntimeDevicePack,
    entrypoint: PathBuf,
    portable: bool,
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

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RuntimeArtifactCatalog {
    schema_version: u32,
    catalog_version: String,
    artifacts: Vec<RuntimeArtifact>,
    #[serde(default)]
    intent_models: Vec<IntentModelArtifact>,
}

impl RuntimeArtifactCatalog {
    pub(crate) fn parse(contents: &str) -> Result<Self, String> {
        let catalog: Self = serde_json::from_str(contents)
            .map_err(|err| format!("runtime artifact catalog is invalid JSON: {err}"))?;
        catalog.validate()?;
        Ok(catalog)
    }

    pub(crate) fn select(
        &self,
        runtime_id: &str,
        os: &str,
        arch: &str,
        device: RuntimeDevicePack,
    ) -> Option<&RuntimeArtifact> {
        self.artifacts.iter().find(|artifact| {
            artifact.runtime_id == runtime_id
                && artifact.os == os
                && artifact.arch == arch
                && artifact.device == device
        })
    }

    pub(crate) fn intent_model(&self, tier: IntentModelTier) -> Option<&IntentModelArtifact> {
        self.intent_models.iter().find(|model| model.tier == tier)
    }

    fn validate(&self) -> Result<(), String> {
        if !matches!(self.schema_version, 1 | 2) {
            return Err(format!(
                "unsupported runtime artifact catalog schema {}",
                self.schema_version
            ));
        }
        if self.catalog_version.trim().is_empty() {
            return Err("runtime artifact catalog version is empty".to_owned());
        }
        if self.schema_version == 1 && !self.intent_models.is_empty() {
            return Err(
                "runtime artifact catalog schema 1 cannot contain voice intent models".to_owned(),
            );
        }

        let mut keys = HashSet::new();
        for artifact in &self.artifacts {
            artifact.validate()?;
            let key = (
                artifact.runtime_id.as_str(),
                artifact.os.as_str(),
                artifact.arch.as_str(),
                artifact.device,
            );
            if !keys.insert(key) {
                return Err(format!(
                    "duplicate runtime artifact for {} {}-{} {}",
                    artifact.runtime_id,
                    artifact.os,
                    artifact.arch,
                    artifact.device.as_str()
                ));
            }
        }
        let mut model_ids = HashSet::new();
        let mut model_tiers = HashSet::new();
        for model in &self.intent_models {
            model.validate()?;
            model.validate_approved_catalog_identity()?;
            if !model_ids.insert(model.model_id.as_str())
                || !model_tiers.insert((model.runtime_id.as_str(), model.tier))
            {
                return Err("duplicate voice intent model id or runtime/tier tuple".to_owned());
            }
        }
        Ok(())
    }
}

impl RuntimeArtifact {
    fn validate(&self) -> Result<(), String> {
        if self.runtime_id.is_empty()
            || !self
                .runtime_id
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
            || runtime_device_support(&self.runtime_id).is_none()
        {
            return Err(format!(
                "unsupported runtime artifact id {:?}",
                self.runtime_id
            ));
        }
        if self.version.is_empty()
            || self.version.len() > 128
            || !self.version.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-')
            })
        {
            return Err(format!(
                "{} artifact version is not a safe immutable identifier",
                self.runtime_id
            ));
        }
        if !matches!(self.os.as_str(), "linux" | "macos" | "windows") {
            return Err(format!("unsupported runtime artifact OS {:?}", self.os));
        }
        if !matches!(self.arch.as_str(), "x86_64" | "aarch64") {
            return Err(format!(
                "unsupported runtime artifact architecture {:?}",
                self.arch
            ));
        }
        if self.device == RuntimeDevicePack::Gpu
            && !runtime_device_support(&self.runtime_id)
                .expect("validated runtime id has device support")
                .supports_gpu()
        {
            return Err(format!(
                "{} does not support a GPU runtime artifact",
                self.runtime_id
            ));
        }
        validate_https_url(&self.url)?;
        if self.sha256.len() != 64
            || !self
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(format!(
                "{} artifact SHA-256 must be 64 lowercase hexadecimal characters",
                self.runtime_id
            ));
        }
        if self.size_bytes == 0 {
            return Err(format!(
                "{} artifact size must be positive",
                self.runtime_id
            ));
        }
        if self.size_bytes > MAX_ARCHIVE_BYTES {
            return Err(format!(
                "{} artifact exceeds the {} byte archive limit",
                self.runtime_id, MAX_ARCHIVE_BYTES
            ));
        }
        if self.unpacked_size_bytes == 0 {
            return Err(format!(
                "{} artifact unpacked size must be positive",
                self.runtime_id
            ));
        }
        if self.unpacked_size_bytes > MAX_UNPACKED_BYTES {
            return Err(format!(
                "{} artifact exceeds the {} byte unpacked limit",
                self.runtime_id, MAX_UNPACKED_BYTES
            ));
        }
        validate_relative_entrypoint(&self.entrypoint)
            .map_err(|err| format!("{} artifact entrypoint {err}", self.runtime_id))?;
        self.validate_voice_intent_provenance()?;
        Ok(())
    }

    fn validate_voice_intent_provenance(&self) -> Result<(), String> {
        let provenance = (
            self.upstream_repository.as_deref(),
            self.upstream_revision.as_deref(),
            self.upstream_asset.as_deref(),
            self.upstream_sha256.as_deref(),
            self.upstream_size_bytes,
            self.license.as_deref(),
            self.license_sha256.as_deref(),
        );
        if self.runtime_id != VOICE_INTENT_LLAMA_CPP_RUNTIME_ID {
            if provenance == (None, None, None, None, None, None, None)
                && self.archive_layout == ArchiveLayout::ScribePortableZipV1
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
        if self.version != "b9637"
            || self.os != "windows"
            || self.arch != "x86_64"
            || self.device != RuntimeDevicePack::Cpu
            || self.url != LLAMA_CPP_OFFICIAL_URL
            || self.sha256 != "f7783c2b8c007f95e710ac40f26a24861a80b603b0b739fc54d7c926a4716c1e"
            || self.size_bytes != 16_906_751
            || self.unpacked_size_bytes != LLAMA_CPP_UPSTREAM_UNPACKED_SIZE
            || self.entrypoint != Path::new("bin/llama-server.exe")
            || self.archive_layout != ArchiveLayout::UpstreamLlamaCppFlatZipV1
            || provenance != expected
        {
            return Err(
                "voice_intent_llama_cpp must carry the approved b9637 upstream asset and MIT license provenance"
                    .to_owned(),
            );
        }
        Ok(())
    }
}

impl IntentModelArtifact {
    fn validate(&self) -> Result<(), String> {
        if self.runtime_id != VOICE_INTENT_LLAMA_CPP_RUNTIME_ID {
            return Err(format!(
                "unsupported voice intent runtime id {:?}",
                self.runtime_id
            ));
        }
        validate_safe_identifier(&self.model_id, "voice intent model id")?;
        validate_immutable_identifier(&self.version, "voice intent model version")?;
        validate_upstream_repository(&self.upstream_repository)?;
        if self.upstream_revision.len() != 40
            || !self
                .upstream_revision
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(
                "voice intent model upstream revision must be a pinned lowercase Git revision"
                    .to_owned(),
            );
        }
        validate_relative_entrypoint(Path::new(&self.upstream_filename))
            .map_err(|err| format!("voice intent model upstream filename {err}"))?;
        validate_gguf_path(
            Path::new(&self.upstream_filename),
            "voice intent model upstream filename",
        )?;
        validate_immutable_identifier(&self.license, "voice intent model license")?;
        validate_sha256(&self.license_sha256, "voice intent model license")?;
        if let Some(url) = &self.url {
            validate_https_url(url)?;
        }
        validate_sha256(&self.sha256, "voice intent model")?;
        if self.size_bytes == 0 || self.size_bytes > MAX_ARCHIVE_BYTES {
            return Err(format!(
                "voice intent model {} exceeds the {} byte limit",
                self.model_id, MAX_ARCHIVE_BYTES
            ));
        }
        validate_relative_entrypoint(&self.managed_relative_path)
            .map_err(|err| format!("voice intent model managed path {err}"))?;
        validate_gguf_path(
            &self.managed_relative_path,
            "voice intent model managed path",
        )?;
        Ok(())
    }

    fn validate_approved_catalog_identity(&self) -> Result<(), String> {
        let expected = match self.tier {
            IntentModelTier::Compact => (
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
            IntentModelTier::Balanced => (
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
        };
        if self.runtime_id != VOICE_INTENT_LLAMA_CPP_RUNTIME_ID
            || self.model_id != expected.0
            || self.version != expected.1
            || self.upstream_repository != expected.2
            || self.upstream_revision != expected.3
            || self.upstream_filename != expected.4
            || self.url.as_deref() != Some(expected.5)
            || self.size_bytes != expected.6
            || self.sha256 != expected.7
            || self.managed_relative_path != Path::new(expected.8)
            || self.license != "Apache-2.0"
            || self.license_sha256
                != "5de36594c10839788a8c589443a8ef9d8b8d17c65a1b5807206ae037fc36c6bd"
        {
            return Err(format!(
                "{} voice intent model must match the approved Qwen artifact",
                match self.tier {
                    IntentModelTier::Compact => "compact",
                    IntentModelTier::Balanced => "balanced",
                }
            ));
        }
        Ok(())
    }
}

fn runtime_device_support(runtime_id: &str) -> Option<runtime_catalog::DeviceSupport> {
    runtime_catalog::backend_spec_for_runtime_id(runtime_id)
        .map(|backend| backend.device_support)
        .or_else(|| {
            (runtime_id == VOICE_INTENT_LLAMA_CPP_RUNTIME_ID)
                .then_some(runtime_catalog::DeviceSupport::CpuOnly)
        })
}

fn validate_safe_identifier(value: &str, label: &str) -> Result<(), String> {
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

fn validate_upstream_repository(value: &str) -> Result<(), String> {
    let mut parts = value.split('/');
    let (Some(owner), Some(repository), None) = (parts.next(), parts.next(), parts.next()) else {
        return Err("voice intent model upstream repository is invalid".to_owned());
    };
    if owner.is_empty()
        || repository.is_empty()
        || !owner
            .bytes()
            .chain(repository.bytes())
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err("voice intent model upstream repository is invalid".to_owned());
    }
    Ok(())
}

fn validate_sha256(value: &str, label: &str) -> Result<(), String> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(format!(
            "{label} SHA-256 must be 64 lowercase hexadecimal characters"
        ));
    }
    Ok(())
}

fn validate_gguf_path(path: &Path, label: &str) -> Result<(), String> {
    if path
        .extension()
        .is_none_or(|extension| !extension.eq_ignore_ascii_case("gguf"))
    {
        return Err(format!("{label} must end in .gguf"));
    }
    Ok(())
}

fn validate_https_url(url: &str) -> Result<(), String> {
    let parsed =
        url::Url::parse(url).map_err(|err| format!("runtime artifact URL is invalid: {err}"))?;
    if parsed.scheme() != "https" {
        return Err("runtime artifact URL must use HTTPS".to_owned());
    }
    let host = parsed.host_str().unwrap_or_default().to_ascii_lowercase();
    let loopback = match parsed.host() {
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        _ => false,
    };
    let reserved_host = host == "localhost"
        || host.ends_with(".localhost")
        || loopback
        || host.ends_with(".invalid")
        || host.ends_with(".test")
        || host.ends_with(".example")
        || ["example.com", "example.net", "example.org"]
            .iter()
            .any(|reserved| host == *reserved || host.ends_with(&format!(".{reserved}")));
    if host.is_empty()
        || reserved_host
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(
            "runtime artifact URL must have a valid HTTPS host and no credentials or fragment"
                .to_owned(),
        );
    }
    Ok(())
}

pub(crate) fn validate_relative_entrypoint(path: &Path) -> Result<(), String> {
    if path.as_os_str().is_empty()
        || path.to_string_lossy().contains('\\')
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err("must be a normalized relative path".to_owned());
    }
    for component in path.components() {
        let Component::Normal(component) = component else {
            return Err("must be a normalized relative path".to_owned());
        };
        let component = component.to_string_lossy();
        let base = component.split('.').next().unwrap_or_default();
        if component.contains(':')
            || component.ends_with(' ')
            || component.ends_with('.')
            || component.chars().any(char::is_control)
            || matches!(
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
            )
        {
            return Err("contains a non-portable path component".to_owned());
        }
    }
    Ok(())
}

pub(crate) fn embedded_artifact(
    runtime_id: &str,
    device: RuntimeDevicePack,
) -> Result<Option<RuntimeArtifact>, String> {
    static CATALOG: OnceLock<Result<RuntimeArtifactCatalog, String>> = OnceLock::new();
    let catalog = CATALOG
        .get_or_init(|| RuntimeArtifactCatalog::parse(EMBEDDED_CATALOG_JSON))
        .as_ref()
        .map_err(Clone::clone)?;
    Ok(catalog
        .select(
            runtime_id,
            std::env::consts::OS,
            std::env::consts::ARCH,
            device,
        )
        .cloned())
}

#[allow(dead_code)]
pub(crate) fn embedded_intent_model(
    tier: IntentModelTier,
) -> Result<Option<IntentModelArtifact>, String> {
    static CATALOG: OnceLock<Result<RuntimeArtifactCatalog, String>> = OnceLock::new();
    let catalog = CATALOG
        .get_or_init(|| RuntimeArtifactCatalog::parse(EMBEDDED_CATALOG_JSON))
        .as_ref()
        .map_err(Clone::clone)?;
    Ok(catalog.intent_model(tier).cloned())
}

#[allow(dead_code)]
pub(crate) fn download_and_stage_intent_model(
    model: &IntentModelArtifact,
    managed_root: &Path,
) -> Result<IntentModelReplacement, String> {
    download_and_stage_intent_model_with_progress(model, managed_root, |_| {
        DownloadControl::Continue
    })
}

pub(crate) fn download_and_stage_intent_model_with_progress(
    model: &IntentModelArtifact,
    managed_root: &Path,
    on_progress: impl FnMut(DownloadProgress) -> DownloadControl,
) -> Result<IntentModelReplacement, String> {
    model.validate()?;
    let url = model.url.as_deref().ok_or_else(|| {
        format!(
            "voice intent model {} has no release URL; this catalog only records immutable upstream provenance",
            model.model_id
        )
    })?;
    let deadline = Instant::now() + MAX_DOWNLOAD_DURATION;
    let response = request_official_artifact(
        url,
        OfficialDownloadPolicy::HuggingFaceModel,
        model.size_bytes,
        "voice intent model",
        deadline,
    )?;
    stage_intent_model_from_reader_until_with_progress(
        model,
        managed_root,
        response.body,
        Some(deadline),
        on_progress,
    )
}

#[cfg(test)]
fn stage_intent_model_from_reader(
    model: &IntentModelArtifact,
    managed_root: &Path,
    reader: impl Read,
) -> Result<IntentModelReplacement, String> {
    stage_intent_model_from_reader_until(model, managed_root, reader, None)
}

#[allow(dead_code)]
fn stage_intent_model_from_reader_until(
    model: &IntentModelArtifact,
    managed_root: &Path,
    reader: impl Read,
    deadline: Option<Instant>,
) -> Result<IntentModelReplacement, String> {
    stage_intent_model_from_reader_until_with_progress(
        model,
        managed_root,
        reader,
        deadline,
        |_| DownloadControl::Continue,
    )
}

fn stage_intent_model_from_reader_until_with_progress(
    model: &IntentModelArtifact,
    managed_root: &Path,
    mut reader: impl Read,
    deadline: Option<Instant>,
    mut on_progress: impl FnMut(DownloadProgress) -> DownloadControl,
) -> Result<IntentModelReplacement, String> {
    model.validate()?;
    let target = managed_root.join(&model.managed_relative_path);
    let parent = target.parent().ok_or_else(|| {
        format!(
            "voice intent model target {} has no parent",
            target.display()
        )
    })?;
    crate::durable_fs::create_dir_all(parent)
        .map_err(|err| format!("could not create {}: {err}", parent.display()))?;
    let install_lock = acquire_intent_model_install_lock(model, managed_root)?;
    let partial = unique_intent_model_partial_path(&target);

    let mut owns_partial = false;
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&partial)
            .map_err(|err| {
                format!(
                    "could not exclusively create voice intent model partial {}: {err}",
                    partial.display()
                )
            })?;
        owns_partial = true;
        let mut hasher = Sha256::new();
        let mut downloaded = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];
        report_download_progress(
            &mut on_progress,
            downloaded,
            model.size_bytes,
            "voice intent model",
        )?;
        loop {
            check_download_deadline(deadline, "voice intent model")?;
            let count = reader
                .read(&mut buffer)
                .map_err(|err| format!("voice intent model download failed: {err}"))?;
            check_download_deadline(deadline, "voice intent model")?;
            if count == 0 {
                break;
            }
            downloaded = downloaded
                .checked_add(count as u64)
                .ok_or_else(|| "voice intent model size overflowed".to_owned())?;
            if downloaded > model.size_bytes {
                return Err(format!(
                    "voice intent model size mismatch: expected {} bytes, received more",
                    model.size_bytes
                ));
            }
            hasher.update(&buffer[..count]);
            file.write_all(&buffer[..count])
                .map_err(|err| format!("could not write {}: {err}", partial.display()))?;
            report_download_progress(
                &mut on_progress,
                downloaded,
                model.size_bytes,
                "voice intent model",
            )?;
        }
        file.sync_all()
            .map_err(|err| format!("could not finish {}: {err}", partial.display()))?;
        if downloaded != model.size_bytes {
            return Err(format!(
                "voice intent model size mismatch: expected {} bytes, received {downloaded}",
                model.size_bytes
            ));
        }
        let actual_sha256 = format!("{:x}", hasher.finalize());
        if actual_sha256 != model.sha256 {
            return Err(format!(
                "voice intent model checksum mismatch: expected {}, received {actual_sha256}",
                model.sha256
            ));
        }
        drop(file);
        publish_verified_intent_model(model, target, partial.clone(), install_lock, |_| Ok(()))
    })();

    match result {
        Err(error) => {
            if owns_partial
                && partial.exists()
                && let Err(cleanup) = crate::durable_fs::remove(&partial)
            {
                return Err(format!(
                    "{error}. Could not clean owned partial {}: {cleanup}",
                    partial.display()
                ));
            }
            Err(error)
        }
        success => success,
    }
}

impl IntentModelReplacement {
    pub(crate) fn prepare_persistence(
        &mut self,
        new_install: Option<&config::ManagedModelInstall>,
    ) -> Result<(), String> {
        match new_install {
            Some(install)
                if install.path == self.installed_path
                    && install.sha256 == self.expected_sha256
                    && intent_model_file_matches(
                        &self.installed_path,
                        self.expected_size_bytes,
                        self.expected_sha256.as_deref(),
                    ) => {}
            Some(_) => {
                return Err(format!(
                    "Refusing to persist invalid voice intent model metadata for {}.",
                    self.model_id
                ));
            }
            None if self.expected_sha256.is_none() && !self.installed_path.exists() => {}
            None => {
                return Err(format!(
                    "Refusing to persist removal while voice intent model {} still exists.",
                    self.installed_path.display()
                ));
            }
        }
        write_intent_model_journal(
            &self.installed_path,
            &IntentModelTransactionJournal {
                version: 2,
                model_id: self.model_id.clone(),
                phase: IntentModelTransactionPhase::AwaitingPersistence,
                had_previous_model: self.backup_path.is_some(),
                previous_sha256: self.previous_sha256.clone(),
                previous_size_bytes: self.previous_size_bytes,
                previous_install: self._lock.previous_install.clone(),
                new_install: new_install.cloned(),
                expected_sha256: self.expected_sha256.clone(),
                expected_size_bytes: self.expected_size_bytes,
            },
        )?;
        self.persistence_install = Some(new_install.cloned());
        Ok(())
    }

    pub(crate) fn commit(self) -> Result<(), String> {
        let Some(new_install) = self.persistence_install.as_ref() else {
            return Err(format!(
                "Refusing to finalize voice intent model {} before configuration persistence.",
                self.model_id
            ));
        };
        if !intent_model_committed_state_is_valid(
            &self.installed_path,
            new_install.as_ref(),
            self.expected_size_bytes,
            self.expected_sha256.as_deref(),
        ) {
            return Err(format!(
                "Refusing to finalize invalid voice intent model state for {}.",
                self.model_id
            ));
        }
        if let Some(backup) = self.backup_path.as_deref() {
            remove_intent_model_path(backup)?;
        }
        remove_intent_model_journal(&self.installed_path)
    }

    pub(crate) fn rollback(self) -> Result<(), String> {
        rollback_intent_model_files(
            &self.installed_path,
            self.backup_path.as_deref(),
            self.backup_path.is_some(),
        )?;
        remove_intent_model_journal(&self.installed_path)
    }
}

pub(crate) fn stage_intent_model_removal(
    model: &IntentModelArtifact,
    managed_root: &Path,
) -> Result<IntentModelReplacement, String> {
    model.validate()?;
    let target = managed_root.join(&model.managed_relative_path);
    let parent = target.parent().ok_or_else(|| {
        format!(
            "voice intent model target {} has no parent",
            target.display()
        )
    })?;
    crate::durable_fs::create_dir_all(parent)
        .map_err(|err| format!("could not create {}: {err}", parent.display()))?;
    let install_lock = acquire_intent_model_install_lock(model, managed_root)?;
    publish_intent_model_removal(model, target, install_lock, |_| Ok(()))
}

fn acquire_intent_model_install_lock(
    model: &IntentModelArtifact,
    managed_root: &Path,
) -> Result<IntentModelInstallLock, String> {
    #[cfg(test)]
    {
        acquire_intent_model_install_lock_with_timeout(
            model,
            managed_root,
            None,
            INTENT_MODEL_LOCK_TIMEOUT,
        )
    }
    #[cfg(not(test))]
    {
        let file = lock_intent_model_install(
            &managed_root.join(&model.managed_relative_path),
            INTENT_MODEL_LOCK_TIMEOUT,
            &model.model_id,
        )?;
        let (mut persisted, _) = config::load_config().map_err(|err| {
            format!("Could not load configuration for voice model recovery: {err}")
        })?;
        config::normalize_config(&mut persisted);
        let target = managed_root.join(&model.managed_relative_path);
        let previous_install = persisted
            .managed_models
            .get(&model.model_id)
            .filter(|install| install.path == target)
            .cloned();
        recover_intent_model_transaction(model, &target, previous_install.as_ref())?;
        Ok(IntentModelInstallLock {
            _file: file,
            previous_install,
        })
    }
}

fn acquire_intent_model_install_lock_with_timeout(
    model: &IntentModelArtifact,
    managed_root: &Path,
    current_install: Option<&config::ManagedModelInstall>,
    timeout: Duration,
) -> Result<IntentModelInstallLock, String> {
    let target = managed_root.join(&model.managed_relative_path);
    let file = lock_intent_model_install(&target, timeout, &model.model_id)?;
    recover_intent_model_transaction(model, &target, current_install)?;
    Ok(IntentModelInstallLock {
        _file: file,
        previous_install: current_install.cloned(),
    })
}

fn lock_intent_model_install(
    target: &Path,
    timeout: Duration,
    model_id: &str,
) -> Result<File, String> {
    let parent = target.parent().ok_or_else(|| {
        format!(
            "voice intent model target {} has no parent",
            target.display()
        )
    })?;
    crate::durable_fs::create_dir_all(parent)
        .map_err(|err| format!("could not create {}: {err}", parent.display()))?;
    let lock_path = intent_model_transaction_path(target, "lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|err| {
            format!(
                "could not open voice intent model lock {}: {err}",
                lock_path.display()
            )
        })?;
    let deadline = Instant::now() + timeout;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(50));
            }
            Err(TryLockError::WouldBlock) => {
                return Err(format!(
                    "Another Scribe process is installing or removing voice intent model {model_id}."
                ));
            }
            Err(TryLockError::Error(err)) => {
                return Err(format!(
                    "could not lock voice intent model transaction {}: {err}",
                    lock_path.display()
                ));
            }
        }
    }
}

fn publish_verified_intent_model(
    model: &IntentModelArtifact,
    target: PathBuf,
    partial: PathBuf,
    install_lock: IntentModelInstallLock,
    on_phase: impl FnMut(IntentModelTransactionPhase) -> Result<(), String>,
) -> Result<IntentModelReplacement, String> {
    publish_intent_model_transaction(
        model,
        target,
        Some(partial),
        Some((model.sha256.clone(), model.size_bytes)),
        install_lock,
        on_phase,
    )
}

fn publish_intent_model_removal(
    model: &IntentModelArtifact,
    target: PathBuf,
    install_lock: IntentModelInstallLock,
    on_phase: impl FnMut(IntentModelTransactionPhase) -> Result<(), String>,
) -> Result<IntentModelReplacement, String> {
    publish_intent_model_transaction(model, target, None, None, install_lock, on_phase)
}

fn publish_intent_model_transaction(
    model: &IntentModelArtifact,
    target: PathBuf,
    partial: Option<PathBuf>,
    expected: Option<(String, u64)>,
    install_lock: IntentModelInstallLock,
    mut on_phase: impl FnMut(IntentModelTransactionPhase) -> Result<(), String>,
) -> Result<IntentModelReplacement, String> {
    let backup = intent_model_transaction_path(&target, "backup");
    if backup.exists() {
        return Err(format!(
            "Found an unrecovered voice intent model backup at {}; preserving it for recovery.",
            backup.display()
        ));
    }
    let had_previous_model = target.exists();
    let previous_fingerprint = had_previous_model
        .then(|| intent_model_file_fingerprint(&target))
        .transpose()?;
    let mut journal = IntentModelTransactionJournal {
        version: 2,
        model_id: model.model_id.clone(),
        phase: IntentModelTransactionPhase::Prepared,
        had_previous_model,
        previous_sha256: previous_fingerprint
            .as_ref()
            .map(|(sha256, _)| sha256.clone()),
        previous_size_bytes: previous_fingerprint.as_ref().map(|(_, size)| *size),
        previous_install: install_lock.previous_install.clone(),
        new_install: None,
        expected_sha256: expected.as_ref().map(|(sha256, _)| sha256.clone()),
        expected_size_bytes: expected.as_ref().map(|(_, size)| *size),
    };
    write_intent_model_journal(&target, &journal)?;
    let mut backup_moved = false;
    let mut new_activated = false;
    let publication = (|| {
        on_phase(IntentModelTransactionPhase::Prepared)?;
        if had_previous_model {
            crate::durable_fs::rename(&target, &backup, false).map_err(|err| {
                format!(
                    "could not preserve existing voice intent model {}: {err}",
                    target.display()
                )
            })?;
            backup_moved = true;
        }
        journal.phase = IntentModelTransactionPhase::BackedUp;
        write_intent_model_journal(&target, &journal)?;
        on_phase(IntentModelTransactionPhase::BackedUp)?;
        if let Some(partial) = partial.as_deref() {
            crate::durable_fs::rename(partial, &target, false).map_err(|err| {
                format!(
                    "could not atomically activate voice intent model {}: {err}",
                    target.display()
                )
            })?;
            new_activated = true;
        }
        journal.phase = IntentModelTransactionPhase::Activated;
        write_intent_model_journal(&target, &journal)?;
        on_phase(IntentModelTransactionPhase::Activated)
    })();
    if let Err(message) = publication {
        let mut failures = Vec::new();
        if let Err(rollback) = rollback_failed_intent_model_publication(
            &target,
            &backup,
            had_previous_model,
            backup_moved,
            new_activated,
        ) {
            failures.push(format!("rollback failed: {rollback}"));
        } else if let Err(cleanup) = remove_intent_model_journal(&target) {
            failures.push(format!("journal cleanup failed: {cleanup}"));
        }
        if let Some(partial) = partial.as_deref()
            && partial.exists()
            && let Err(cleanup) = remove_intent_model_path(partial)
        {
            failures.push(format!("partial cleanup failed: {cleanup}"));
        }
        return Err(if failures.is_empty() {
            message
        } else {
            format!("{message}. {}", failures.join("; "))
        });
    }

    Ok(IntentModelReplacement {
        installed_path: target,
        model_id: model.model_id.clone(),
        backup_path: had_previous_model.then_some(backup),
        previous_sha256: previous_fingerprint
            .as_ref()
            .map(|(sha256, _)| sha256.clone()),
        previous_size_bytes: previous_fingerprint.map(|(_, size)| size),
        expected_sha256: expected.as_ref().map(|(sha256, _)| sha256.clone()),
        expected_size_bytes: expected.map(|(_, size)| size),
        persistence_install: None,
        _lock: install_lock,
    })
}

fn rollback_failed_intent_model_publication(
    target: &Path,
    backup: &Path,
    had_previous_model: bool,
    backup_moved: bool,
    new_activated: bool,
) -> Result<(), String> {
    if had_previous_model && backup_moved {
        if !backup.exists() {
            return Err(format!(
                "the previous model backup {} is missing",
                backup.display()
            ));
        }
        if new_activated {
            remove_intent_model_path(target)?;
        }
        crate::durable_fs::rename(backup, target, false).map_err(|err| {
            format!(
                "could not restore previous voice intent model {}: {err}",
                target.display()
            )
        })?;
    } else if !had_previous_model && new_activated {
        remove_intent_model_path(target)?;
    }
    Ok(())
}

fn rollback_intent_model_files(
    target: &Path,
    backup: Option<&Path>,
    had_previous_model: bool,
) -> Result<(), String> {
    match backup {
        Some(backup) if backup.exists() => {
            remove_intent_model_path(target)?;
            crate::durable_fs::rename(backup, target, false).map_err(|err| {
                format!(
                    "could not restore previous voice intent model {}: {err}",
                    target.display()
                )
            })
        }
        Some(backup) => Err(format!(
            "the previous voice intent model backup {} is missing",
            backup.display()
        )),
        None if had_previous_model => Err(format!(
            "the previous voice intent model backup for {} is missing",
            target.display()
        )),
        None => remove_intent_model_path(target),
    }
}

pub(crate) fn recover_intent_model_transactions(config: &config::AppConfig) -> Result<(), String> {
    let managed_root = config::model_storage_dir(config);
    let mut errors = Vec::new();
    for tier in [IntentModelTier::Compact, IntentModelTier::Balanced] {
        let model = match embedded_intent_model(tier) {
            Ok(Some(model)) => model,
            Ok(None) => continue,
            Err(message) => return Err(message),
        };
        let target = managed_root.join(&model.managed_relative_path);
        if !intent_model_recovery_needed(&target) {
            continue;
        }
        let current_install = config.managed_models.get(&model.model_id);
        if let Err(message) = acquire_intent_model_install_lock_with_timeout(
            &model,
            &managed_root,
            current_install,
            INTENT_MODEL_LOCK_TIMEOUT,
        ) {
            errors.push(format!("{}: {message}", model.model_id));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn recover_intent_model_transaction(
    model: &IntentModelArtifact,
    target: &Path,
    current_install: Option<&config::ManagedModelInstall>,
) -> Result<(), String> {
    let backup = intent_model_transaction_path(target, "backup");
    if let Some(journal) = read_intent_model_journal(&model.model_id, target)? {
        match journal.phase {
            IntentModelTransactionPhase::AwaitingPersistence => {
                if current_install == journal.new_install.as_ref() {
                    if !intent_model_committed_state_is_valid(
                        target,
                        journal.new_install.as_ref(),
                        journal.expected_size_bytes,
                        journal.expected_sha256.as_deref(),
                    ) {
                        return Err(format!(
                            "Committed voice intent model files for {} do not match the transaction journal.",
                            model.model_id
                        ));
                    }
                    remove_intent_model_path(&backup)?;
                    remove_intent_model_journal(target)?;
                } else if current_install == journal.previous_install.as_ref() {
                    recover_intent_model_rollback(target, &backup, &journal)?;
                    remove_intent_model_journal(target)?;
                } else {
                    return Err(format!(
                        "Voice intent model transaction metadata for {} does not match the persisted configuration.",
                        model.model_id
                    ));
                }
            }
            IntentModelTransactionPhase::Prepared => {
                if backup.exists() {
                    rollback_intent_model_files(target, Some(&backup), true)?;
                } else if journal.had_previous_model && !target.exists() {
                    return Err(format!(
                        "The previous voice intent model {} is missing during recovery.",
                        model.model_id
                    ));
                }
                remove_intent_model_journal(target)?;
            }
            IntentModelTransactionPhase::BackedUp | IntentModelTransactionPhase::Activated => {
                recover_intent_model_rollback(target, &backup, &journal)?;
                remove_intent_model_journal(target)?;
            }
        }
    } else if backup.exists() {
        return Err(format!(
            "Found an unjournaled voice intent model backup at {}; preserving it for manual recovery.",
            backup.display()
        ));
    }
    cleanup_stale_intent_model_partials(target)
}

fn recover_intent_model_rollback(
    target: &Path,
    backup: &Path,
    journal: &IntentModelTransactionJournal,
) -> Result<(), String> {
    if journal.had_previous_model {
        if backup.exists() {
            rollback_intent_model_files(target, Some(backup), true)
        } else if intent_model_file_matches(
            target,
            journal.previous_size_bytes,
            journal.previous_sha256.as_deref(),
        ) {
            Ok(())
        } else {
            Err(format!(
                "the previous voice intent model backup {} is missing and the restored target does not match its recorded fingerprint",
                backup.display()
            ))
        }
    } else {
        rollback_intent_model_files(target, None, false)
    }
}

fn intent_model_committed_state_is_valid(
    target: &Path,
    install: Option<&config::ManagedModelInstall>,
    expected_size_bytes: Option<u64>,
    expected_sha256: Option<&str>,
) -> bool {
    match install {
        Some(install) => {
            install.path == target
                && install.sha256.as_deref() == expected_sha256
                && intent_model_file_matches(target, expected_size_bytes, expected_sha256)
        }
        None => expected_sha256.is_none() && !target.exists(),
    }
}

fn intent_model_file_matches(
    path: &Path,
    expected_size_bytes: Option<u64>,
    expected_sha256: Option<&str>,
) -> bool {
    let (Some(expected_size_bytes), Some(expected_sha256)) = (expected_size_bytes, expected_sha256)
    else {
        return false;
    };
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    if !file
        .metadata()
        .is_ok_and(|metadata| metadata.len() == expected_size_bytes)
    {
        return false;
    }
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => hasher.update(&buffer[..count]),
            Err(_) => return false,
        }
    }
    format!("{:x}", hasher.finalize()) == expected_sha256
}

fn intent_model_file_fingerprint(path: &Path) -> Result<(String, u64), String> {
    let mut file = File::open(path).map_err(|err| {
        format!(
            "could not open voice intent model {}: {err}",
            path.display()
        )
    })?;
    let metadata = file.metadata().map_err(|err| {
        format!(
            "could not inspect voice intent model {}: {err}",
            path.display()
        )
    })?;
    if !metadata.is_file() {
        return Err(format!(
            "voice intent model {} is not a regular file",
            path.display()
        ));
    }
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|err| {
            format!(
                "could not hash voice intent model {}: {err}",
                path.display()
            )
        })?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok((format!("{:x}", hasher.finalize()), metadata.len()))
}

fn read_intent_model_journal(
    model_id: &str,
    target: &Path,
) -> Result<Option<IntentModelTransactionJournal>, String> {
    let next = intent_model_transaction_path(target, "transaction.next");
    let current = intent_model_transaction_path(target, "transaction");
    if next.exists() {
        match parse_intent_model_journal(&next) {
            Ok(journal) => {
                validate_intent_model_journal(model_id, &next, &journal)?;
                return Ok(Some(journal));
            }
            Err(next_error) if current.exists() => {
                return parse_intent_model_journal(&current)
                    .and_then(|journal| {
                        validate_intent_model_journal(model_id, &current, &journal)?;
                        Ok(journal)
                    })
                    .map(Some)
                    .map_err(|current_error| format!("{next_error} {current_error}"));
            }
            Err(next_error) => return Err(next_error),
        }
    }
    if !current.exists() {
        return Ok(None);
    }
    let journal = parse_intent_model_journal(&current)?;
    validate_intent_model_journal(model_id, &current, &journal)?;
    Ok(Some(journal))
}

fn parse_intent_model_journal(path: &Path) -> Result<IntentModelTransactionJournal, String> {
    let contents = fs::read_to_string(path).map_err(|err| {
        format!(
            "could not read voice intent model transaction {}: {err}",
            path.display()
        )
    })?;
    serde_json::from_str(&contents).map_err(|err| {
        format!(
            "voice intent model transaction {} is invalid: {err}",
            path.display()
        )
    })
}

fn validate_intent_model_journal(
    model_id: &str,
    path: &Path,
    journal: &IntentModelTransactionJournal,
) -> Result<(), String> {
    if journal.version != 2
        || journal.model_id != model_id
        || journal.had_previous_model
            != (journal.previous_sha256.is_some() && journal.previous_size_bytes.is_some())
    {
        return Err(format!(
            "voice intent model transaction {} has an unexpected identity",
            path.display()
        ));
    }
    Ok(())
}

fn write_intent_model_journal(
    target: &Path,
    journal: &IntentModelTransactionJournal,
) -> Result<(), String> {
    let path = intent_model_transaction_path(target, "transaction");
    let next = intent_model_transaction_path(target, "transaction.next");
    remove_intent_model_path(&next)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&next)
        .map_err(|err| {
            format!(
                "could not create voice intent model transaction {}: {err}",
                next.display()
            )
        })?;
    serde_json::to_writer(&mut file, journal)
        .map_err(|err| format!("could not serialize voice intent model transaction: {err}"))?;
    file.write_all(b"\n")
        .map_err(|err| format!("could not write voice intent model transaction: {err}"))?;
    file.sync_all()
        .map_err(|err| format!("could not sync voice intent model transaction: {err}"))?;
    drop(file);
    crate::durable_fs::rename(&next, &path, true).map_err(|err| {
        format!(
            "could not publish voice intent model transaction {}: {err}",
            path.display()
        )
    })
}

fn remove_intent_model_journal(target: &Path) -> Result<(), String> {
    remove_intent_model_path(&intent_model_transaction_path(target, "transaction.next"))?;
    remove_intent_model_path(&intent_model_transaction_path(target, "transaction"))
}

fn remove_intent_model_path(path: &Path) -> Result<(), String> {
    crate::durable_fs::remove(path)
        .map_err(|err| format!("could not remove {}: {err}", path.display()))
}

fn intent_model_transaction_path(target: &Path, suffix: &str) -> PathBuf {
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("model");
    target.with_file_name(format!(".{name}.{suffix}"))
}

fn unique_intent_model_partial_path(target: &Path) -> PathBuf {
    let sequence = INTENT_MODEL_PARTIAL_NONCE.fetch_add(1, Ordering::Relaxed);
    intent_model_transaction_path(
        target,
        &format!("partial-{}-{sequence:016x}", std::process::id()),
    )
}

fn cleanup_stale_intent_model_partials(target: &Path) -> Result<(), String> {
    let Some(parent) = target.parent() else {
        return Ok(());
    };
    if !parent.exists() {
        return Ok(());
    }
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("model");
    let legacy = format!(".{name}.partial");
    let prefix = format!(".{name}.partial-");
    for entry in fs::read_dir(parent).map_err(|err| {
        format!(
            "could not scan {} for stale partials: {err}",
            parent.display()
        )
    })? {
        let entry = entry.map_err(|err| {
            format!(
                "could not inspect a stale model partial in {}: {err}",
                parent.display()
            )
        })?;
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        if file_name == legacy || file_name.starts_with(&prefix) {
            remove_intent_model_path(&entry.path())?;
        }
    }
    Ok(())
}

fn intent_model_recovery_needed(target: &Path) -> bool {
    intent_model_transaction_path(target, "transaction").exists()
        || intent_model_transaction_path(target, "transaction.next").exists()
        || intent_model_transaction_path(target, "backup").exists()
        || target.parent().is_some_and(|parent| {
            let name = target
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("model");
            fs::read_dir(parent).is_ok_and(|entries| {
                entries.filter_map(Result::ok).any(|entry| {
                    entry.file_name().to_str().is_some_and(|candidate| {
                        candidate == format!(".{name}.partial")
                            || candidate.starts_with(&format!(".{name}.partial-"))
                    })
                })
            })
        })
}

fn report_download_progress(
    on_progress: &mut impl FnMut(DownloadProgress) -> DownloadControl,
    downloaded_bytes: u64,
    total_bytes: u64,
    name: &str,
) -> Result<(), String> {
    match on_progress(DownloadProgress {
        downloaded_bytes,
        total_bytes,
    }) {
        DownloadControl::Continue => Ok(()),
        DownloadControl::Cancel => Err(format!("{name} download cancelled")),
    }
}

fn check_download_deadline(deadline: Option<Instant>, name: &str) -> Result<(), String> {
    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
        Err(format!(
            "{name} download exceeded the {} minute deadline",
            MAX_DOWNLOAD_DURATION.as_secs() / 60
        ))
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OfficialDownloadPolicy {
    CatalogArtifact,
    LlamaCppRuntime,
    HuggingFaceModel,
}

struct ArtifactHttpResponse {
    status: u16,
    locations: Vec<String>,
    content_encoding: Option<String>,
    content_length: Option<String>,
    content_type: Option<String>,
    body: Box<dyn Read + Send + Sync>,
}

fn request_official_artifact(
    initial_url: &str,
    policy: OfficialDownloadPolicy,
    expected_size: u64,
    label: &str,
    deadline: Instant,
) -> Result<ArtifactHttpResponse, String> {
    let agent = ureq::AgentBuilder::new()
        .redirects(0)
        .try_proxy_from_env(false)
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(60))
        .timeout_write(Duration::from_secs(60))
        .build();
    follow_artifact_redirects_with(initial_url, policy, expected_size, label, deadline, |url| {
        let response = match agent
            .get(url.as_str())
            .set("Accept-Encoding", "identity")
            .call()
        {
            Ok(response) | Err(ureq::Error::Status(_, response)) => response,
            Err(ureq::Error::Transport(_)) => {
                return Err(format!("{label} request failed"));
            }
        };
        Ok(ArtifactHttpResponse {
            status: response.status(),
            locations: response
                .all("location")
                .into_iter()
                .map(ToOwned::to_owned)
                .collect(),
            content_encoding: response.header("content-encoding").map(ToOwned::to_owned),
            content_length: response.header("content-length").map(ToOwned::to_owned),
            content_type: response.header("content-type").map(ToOwned::to_owned),
            body: response.into_reader(),
        })
    })
}

fn follow_artifact_redirects_with(
    initial_url: &str,
    policy: OfficialDownloadPolicy,
    expected_size: u64,
    label: &str,
    deadline: Instant,
    mut request: impl FnMut(&url::Url) -> Result<ArtifactHttpResponse, String>,
) -> Result<ArtifactHttpResponse, String> {
    let mut current =
        url::Url::parse(initial_url).map_err(|_| format!("{label} origin URL is invalid"))?;
    validate_download_origin(&current, policy, label)?;
    let mut visited = HashSet::new();
    visited.insert(current.as_str().to_owned());

    for hop in 0..=MAX_REDIRECTS {
        check_download_deadline(Some(deadline), label)?;
        let response = request(&current)?;
        if matches!(response.status, 301 | 302 | 303 | 307 | 308) {
            if hop == MAX_REDIRECTS {
                return Err(format!("{label} exceeded the redirect limit"));
            }
            if response.locations.len() != 1 {
                return Err(format!(
                    "{label} redirect must provide exactly one Location header"
                ));
            }
            let location = &response.locations[0];
            if location.is_empty() || location.len() > MAX_REDIRECT_LOCATION_BYTES {
                return Err(format!("{label} redirect Location is invalid"));
            }
            let next = current
                .join(location)
                .map_err(|_| format!("{label} redirect Location is invalid"))?;
            validate_download_redirect(&current, &next, policy, label)?;
            if !visited.insert(next.as_str().to_owned()) {
                return Err(format!("{label} redirect loop detected"));
            }
            current = next;
            continue;
        }
        if response.status != 200 {
            return Err(format!(
                "{label} request returned status {} after {hop} redirects",
                response.status
            ));
        }
        validate_download_response(&response, expected_size, label)?;
        return Ok(response);
    }
    Err(format!("{label} exceeded the redirect limit"))
}

fn validate_download_origin(
    url: &url::Url,
    policy: OfficialDownloadPolicy,
    label: &str,
) -> Result<(), String> {
    validate_download_url(url, false, label)?;
    let matches = match policy {
        OfficialDownloadPolicy::CatalogArtifact => true,
        OfficialDownloadPolicy::LlamaCppRuntime => url.as_str() == LLAMA_CPP_OFFICIAL_URL,
        OfficialDownloadPolicy::HuggingFaceModel => matches!(
            url.as_str(),
            "https://huggingface.co/Qwen/Qwen3-0.6B-GGUF/resolve/ef4088322893040952513f532f736ddeab518403/Qwen3-0.6B-Q8_0.gguf"
                | "https://huggingface.co/Qwen/Qwen3-1.7B-GGUF/resolve/90862c4b9d2787eaed51d12237eafdfe7c5f6077/Qwen3-1.7B-Q8_0.gguf"
        ),
    };
    if matches {
        Ok(())
    } else {
        Err(format!("{label} origin URL is not approved"))
    }
}

fn validate_download_redirect(
    _current: &url::Url,
    next: &url::Url,
    policy: OfficialDownloadPolicy,
    label: &str,
) -> Result<(), String> {
    let host = next.host_str().unwrap_or_default().to_ascii_lowercase();
    let allowed = match policy {
        OfficialDownloadPolicy::CatalogArtifact => false,
        OfficialDownloadPolicy::LlamaCppRuntime => host == "release-assets.githubusercontent.com",
        OfficialDownloadPolicy::HuggingFaceModel => {
            host.len() > ".cdn.hf.co".len() && host.ends_with(".cdn.hf.co")
        }
    };
    validate_download_url(next, allowed, label)?;
    if allowed {
        Ok(())
    } else {
        Err(format!("{label} redirect target is not approved"))
    }
}

fn validate_download_url(url: &url::Url, allow_query: bool, label: &str) -> Result<(), String> {
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    let loopback = match url.host() {
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        _ => false,
    };
    if url.scheme() != "https"
        || host.is_empty()
        || loopback
        || host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".invalid")
        || host.ends_with(".test")
        || host.ends_with(".example")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port_or_known_default() != Some(443)
        || url.fragment().is_some()
        || (!allow_query && url.query().is_some())
    {
        return Err(format!("{label} download URL is unsafe"));
    }
    Ok(())
}

fn validate_download_response(
    response: &ArtifactHttpResponse,
    expected_size: u64,
    label: &str,
) -> Result<(), String> {
    if response
        .content_encoding
        .as_deref()
        .is_some_and(|encoding| !encoding.eq_ignore_ascii_case("identity"))
    {
        return Err(format!("{label} response must not use content encoding"));
    }
    if let Some(length) = response.content_length.as_deref() {
        let length = length
            .parse::<u64>()
            .map_err(|_| format!("{label} Content-Length is invalid"))?;
        if length != expected_size {
            return Err(format!(
                "{label} Content-Length mismatch: expected {expected_size}, received {length}"
            ));
        }
    }
    if response
        .content_type
        .as_deref()
        .is_some_and(|content_type| {
            let media_type = content_type
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase();
            media_type.starts_with("text/")
                || matches!(media_type.as_str(), "application/json" | "application/xml")
        })
    {
        return Err(format!("{label} response has a non-binary content type"));
    }
    Ok(())
}

#[allow(dead_code)]
pub(crate) fn download_and_stage(
    artifact: &RuntimeArtifact,
    target_root: &Path,
) -> Result<StagedRuntimeArtifact, String> {
    download_and_stage_with_progress(artifact, target_root, |_| DownloadControl::Continue)
}

pub(crate) fn download_and_stage_with_progress(
    artifact: &RuntimeArtifact,
    target_root: &Path,
    on_progress: impl FnMut(DownloadProgress) -> DownloadControl,
) -> Result<StagedRuntimeArtifact, String> {
    let deadline = Instant::now() + MAX_DOWNLOAD_DURATION;
    let policy = match artifact.archive_layout {
        ArchiveLayout::UpstreamLlamaCppFlatZipV1 => OfficialDownloadPolicy::LlamaCppRuntime,
        ArchiveLayout::ScribePortableZipV1 => OfficialDownloadPolicy::CatalogArtifact,
    };
    let response = request_official_artifact(
        &artifact.url,
        policy,
        artifact.size_bytes,
        "runtime artifact",
        deadline,
    )?;
    stage_from_reader_until_with_progress(
        artifact,
        target_root,
        response.body,
        Some(deadline),
        on_progress,
    )
}

#[cfg(test)]
fn stage_from_reader(
    artifact: &RuntimeArtifact,
    target_root: &Path,
    reader: impl Read,
) -> Result<StagedRuntimeArtifact, String> {
    stage_from_reader_until(artifact, target_root, reader, None)
}

#[allow(dead_code)]
fn stage_from_reader_until(
    artifact: &RuntimeArtifact,
    target_root: &Path,
    reader: impl Read,
    deadline: Option<Instant>,
) -> Result<StagedRuntimeArtifact, String> {
    stage_from_reader_until_with_progress(artifact, target_root, reader, deadline, |_| {
        DownloadControl::Continue
    })
}

fn stage_from_reader_until_with_progress(
    artifact: &RuntimeArtifact,
    target_root: &Path,
    mut reader: impl Read,
    deadline: Option<Instant>,
    mut on_progress: impl FnMut(DownloadProgress) -> DownloadControl,
) -> Result<StagedRuntimeArtifact, String> {
    let parent = target_root
        .parent()
        .ok_or_else(|| format!("runtime target {} has no parent", target_root.display()))?;
    crate::durable_fs::create_dir_all(parent)
        .map_err(|err| format!("could not create {}: {err}", parent.display()))?;
    let archive_path = transaction_path(target_root, "download").with_extension("zip.partial");
    let stage_root = transaction_path(target_root, "installing");
    remove_path_if_exists(&archive_path)?;
    remove_path_if_exists(&stage_root)?;

    let result = (|| {
        let mut archive = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&archive_path)
            .map_err(|err| format!("could not create {}: {err}", archive_path.display()))?;
        let mut hasher = Sha256::new();
        let mut downloaded = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];
        report_download_progress(
            &mut on_progress,
            downloaded,
            artifact.size_bytes,
            "runtime artifact",
        )?;
        loop {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err(format!(
                    "runtime artifact download exceeded the {} minute deadline",
                    MAX_DOWNLOAD_DURATION.as_secs() / 60
                ));
            }
            let count = reader
                .read(&mut buffer)
                .map_err(|err| format!("runtime artifact download failed: {err}"))?;
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err(format!(
                    "runtime artifact download exceeded the {} minute deadline",
                    MAX_DOWNLOAD_DURATION.as_secs() / 60
                ));
            }
            if count == 0 {
                break;
            }
            downloaded = downloaded
                .checked_add(count as u64)
                .ok_or_else(|| "runtime artifact size overflowed".to_owned())?;
            if downloaded > artifact.size_bytes {
                return Err(format!(
                    "runtime artifact size mismatch: expected {} bytes, received more",
                    artifact.size_bytes
                ));
            }
            hasher.update(&buffer[..count]);
            archive
                .write_all(&buffer[..count])
                .map_err(|err| format!("could not write {}: {err}", archive_path.display()))?;
            report_download_progress(
                &mut on_progress,
                downloaded,
                artifact.size_bytes,
                "runtime artifact",
            )?;
        }
        archive
            .sync_all()
            .map_err(|err| format!("could not finish {}: {err}", archive_path.display()))?;
        if downloaded != artifact.size_bytes {
            return Err(format!(
                "runtime artifact size mismatch: expected {} bytes, received {downloaded}",
                artifact.size_bytes
            ));
        }
        let actual_sha256 = format!("{:x}", hasher.finalize());
        if actual_sha256 != artifact.sha256 {
            return Err(format!(
                "runtime artifact checksum mismatch: expected {}, received {actual_sha256}",
                artifact.sha256
            ));
        }
        drop(archive);

        let archive = fs::File::open(&archive_path)
            .map_err(|err| format!("could not open {}: {err}", archive_path.display()))?;
        extract_archive(artifact, archive, &stage_root)?;
        Ok(StagedRuntimeArtifact {
            entrypoint: stage_root.join(&artifact.entrypoint),
            root: stage_root.clone(),
        })
    })();

    let _ = fs::remove_file(&archive_path);
    if result.is_err() {
        let _ = remove_path_if_exists(&stage_root);
    }
    result
}

fn extract_archive(
    artifact: &RuntimeArtifact,
    reader: impl Read + Seek,
    stage_root: &Path,
) -> Result<(), String> {
    match artifact.archive_layout {
        ArchiveLayout::ScribePortableZipV1 => {
            extract_scribe_portable_archive(artifact, reader, stage_root)
        }
        ArchiveLayout::UpstreamLlamaCppFlatZipV1 => {
            extract_upstream_llama_cpp_archive(artifact, reader, stage_root)
        }
    }
}

fn extract_scribe_portable_archive(
    artifact: &RuntimeArtifact,
    reader: impl Read + Seek,
    stage_root: &Path,
) -> Result<(), String> {
    let mut archive = zip::ZipArchive::new(reader)
        .map_err(|err| format!("runtime artifact is not a readable ZIP archive: {err}"))?;
    validate_archive_entry_count(archive.len())?;
    fs::create_dir(stage_root)
        .map_err(|err| format!("could not create {}: {err}", stage_root.display()))?;
    let mut names = HashSet::new();
    let mut declared_unpacked = 0_u64;
    let mut extracted_unpacked = 0_u64;
    let maximum_unpacked = artifact.unpacked_size_bytes;
    let mut found_entrypoint = false;

    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|err| format!("could not inspect runtime archive entry {index}: {err}"))?;
        let name = entry.name().to_owned();
        if name.contains('\\') {
            return Err(format!("unsafe runtime archive path {name:?}"));
        }
        let relative = entry
            .enclosed_name()
            .ok_or_else(|| format!("unsafe runtime archive path {name:?}"))?
            .to_path_buf();
        if validate_relative_entrypoint(&relative).is_err() {
            return Err(format!("unsafe runtime archive path {name:?}"));
        }
        if !names.insert(relative.clone()) {
            return Err(format!("duplicate runtime archive path {name:?}"));
        }
        if relative
            .file_name()
            .is_some_and(|file_name| file_name.eq_ignore_ascii_case("pyvenv.cfg"))
        {
            return Err(
                "raw Python virtual environments are development-only and cannot be installed as portable runtime artifacts"
                    .to_owned(),
            );
        }
        let unix_type = entry.unix_mode().unwrap_or(0) & 0o170000;
        if !matches!(unix_type, 0 | 0o040000 | 0o100000) {
            return Err(format!(
                "runtime archive entry {name:?} is a link or special file"
            ));
        }
        let declared_size = entry.size();
        declared_unpacked = declared_unpacked
            .checked_add(declared_size)
            .ok_or_else(|| "runtime artifact unpacked size overflowed".to_owned())?;
        if declared_unpacked > maximum_unpacked {
            return Err(format!(
                "runtime artifact exceeds the allowed unpacked size of {maximum_unpacked} bytes"
            ));
        }

        let destination = stage_root.join(&relative);
        if entry.is_dir() {
            if declared_size != 0 {
                return Err(format!(
                    "runtime archive directory {name:?} declares non-zero content"
                ));
            }
            fs::create_dir_all(&destination)
                .map_err(|err| format!("could not create {}: {err}", destination.display()))?;
            continue;
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| format!("could not create {}: {err}", parent.display()))?;
        }
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)
            .map_err(|err| format!("could not create {}: {err}", destination.display()))?;
        copy_archive_entry_bounded(
            &mut entry,
            &mut output,
            &name,
            declared_size,
            &mut extracted_unpacked,
            maximum_unpacked,
        )?;
        output
            .sync_all()
            .map_err(|err| format!("could not finish {}: {err}", destination.display()))?;
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&destination, fs::Permissions::from_mode(mode & 0o777)).map_err(
                |err| {
                    format!(
                        "could not set permissions on {}: {err}",
                        destination.display()
                    )
                },
            )?;
        }
        if relative == artifact.entrypoint {
            found_entrypoint = true;
        }
    }
    if !found_entrypoint {
        return Err(format!(
            "runtime artifact does not contain its expected entrypoint {}",
            artifact.entrypoint.display()
        ));
    }
    if declared_unpacked != artifact.unpacked_size_bytes
        || extracted_unpacked != artifact.unpacked_size_bytes
    {
        return Err(format!(
            "runtime artifact unpacked size mismatch: expected {} bytes, declared {declared_unpacked}, extracted {extracted_unpacked}",
            artifact.unpacked_size_bytes,
        ));
    }
    validate_extracted_manifest(artifact, stage_root)?;
    Ok(())
}

fn extract_upstream_llama_cpp_archive(
    artifact: &RuntimeArtifact,
    reader: impl Read + Seek,
    stage_root: &Path,
) -> Result<(), String> {
    extract_upstream_llama_cpp_archive_with_expectations(
        artifact,
        reader,
        stage_root,
        LlamaArchiveExpectations {
            entry_count: LLAMA_CPP_UPSTREAM_ENTRY_COUNT,
            unpacked_size: LLAMA_CPP_UPSTREAM_UNPACKED_SIZE,
            dll_count: LLAMA_CPP_SELECTED_DLL_COUNT,
            payload_size: LLAMA_CPP_SELECTED_PAYLOAD_SIZE,
        },
    )
}

#[derive(Clone, Copy)]
struct LlamaArchiveExpectations {
    entry_count: usize,
    unpacked_size: u64,
    dll_count: usize,
    payload_size: u64,
}

fn extract_upstream_llama_cpp_archive_with_expectations(
    artifact: &RuntimeArtifact,
    reader: impl Read + Seek,
    stage_root: &Path,
    expected: LlamaArchiveExpectations,
) -> Result<(), String> {
    if artifact.runtime_id != VOICE_INTENT_LLAMA_CPP_RUNTIME_ID
        || artifact.archive_layout != ArchiveLayout::UpstreamLlamaCppFlatZipV1
    {
        return Err(
            "the upstream llama.cpp layout is reserved for the approved voice runtime".to_owned(),
        );
    }
    let mut archive = zip::ZipArchive::new(reader)
        .map_err(|err| format!("llama.cpp artifact is not a readable ZIP archive: {err}"))?;
    if archive.len() != expected.entry_count {
        return Err(format!(
            "llama.cpp archive entry count mismatch: expected {}, received {}",
            expected.entry_count,
            archive.len()
        ));
    }
    fs::create_dir(stage_root)
        .map_err(|err| format!("could not create {}: {err}", stage_root.display()))?;
    let bin_root = stage_root.join("bin");
    fs::create_dir(&bin_root)
        .map_err(|err| format!("could not create {}: {err}", bin_root.display()))?;

    let mut names = HashSet::new();
    let mut declared_unpacked = 0_u64;
    let mut extracted_payload = 0_u64;
    let mut found_server = false;
    let mut dll_count = 0_usize;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|err| format!("could not inspect llama.cpp archive entry {index}: {err}"))?;
        let name = entry.name().to_owned();
        let relative = Path::new(&name);
        if name.is_empty()
            || !name.is_ascii()
            || name.contains(['/', '\\'])
            || validate_relative_entrypoint(relative).is_err()
            || relative.components().count() != 1
            || entry.is_dir()
            || entry.encrypted()
        {
            return Err(format!("unsafe llama.cpp archive entry {name:?}"));
        }
        let folded = name.to_ascii_lowercase();
        if !names.insert(folded.clone()) {
            return Err(format!("duplicate llama.cpp archive entry {name:?}"));
        }
        let unix_type = entry.unix_mode().unwrap_or(0) & 0o170000;
        if !matches!(unix_type, 0 | 0o100000) {
            return Err(format!(
                "llama.cpp archive entry {name:?} is a link or special file"
            ));
        }
        declared_unpacked = declared_unpacked
            .checked_add(entry.size())
            .ok_or_else(|| "llama.cpp archive unpacked size overflowed".to_owned())?;
        if declared_unpacked > artifact.unpacked_size_bytes {
            return Err(format!(
                "llama.cpp archive exceeds the allowed unpacked size of {} bytes",
                artifact.unpacked_size_bytes
            ));
        }

        let selected = folded == "llama-server.exe" || folded.ends_with(".dll");
        if !selected {
            continue;
        }
        found_server |= folded == "llama-server.exe";
        dll_count += usize::from(folded.ends_with(".dll"));
        let destination = bin_root.join(&name);
        let declared_size = entry.size();
        let mut output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)
            .map_err(|err| format!("could not create {}: {err}", destination.display()))?;
        copy_archive_entry_bounded(
            &mut entry,
            &mut output,
            &name,
            declared_size,
            &mut extracted_payload,
            artifact.unpacked_size_bytes,
        )?;
        output
            .sync_all()
            .map_err(|err| format!("could not finish {}: {err}", destination.display()))?;
        drop(output);
        let mut image = File::open(&destination)
            .map_err(|err| format!("could not inspect {}: {err}", destination.display()))?;
        let mut signature = [0_u8; 2];
        image
            .read_exact(&mut signature)
            .map_err(|_| format!("llama.cpp runtime payload is truncated: {name}"))?;
        if signature != *b"MZ" {
            return Err(format!(
                "llama.cpp runtime payload is not a Windows PE image: {name}"
            ));
        }
    }
    if declared_unpacked != expected.unpacked_size
        || declared_unpacked != artifact.unpacked_size_bytes
    {
        return Err(format!(
            "llama.cpp archive unpacked size mismatch: expected {}, received {declared_unpacked}",
            artifact.unpacked_size_bytes
        ));
    }
    if !found_server
        || dll_count != expected.dll_count
        || extracted_payload != expected.payload_size
    {
        return Err("llama.cpp archive runtime payload inventory does not match b9637".to_owned());
    }
    write_pinned_llama_license(stage_root)?;
    write_generated_runtime_manifest(artifact, stage_root)?;
    validate_extracted_manifest(artifact, stage_root)?;
    if !stage_root.join(&artifact.entrypoint).is_file() {
        return Err(format!(
            "llama.cpp runtime did not create {}",
            artifact.entrypoint.display()
        ));
    }
    Ok(())
}

fn write_pinned_llama_license(stage_root: &Path) -> Result<(), String> {
    let digest = format!("{:x}", Sha256::digest(LLAMA_CPP_LICENSE));
    if LLAMA_CPP_LICENSE.len() != 1_078
        || digest != "94f29bbed6a22c35b992c5c6ebf0e7c92f13b836b90f36f461c9cf2f0f1d010d"
    {
        return Err("embedded llama.cpp license does not match the approved bytes".to_owned());
    }
    let path = stage_root.join("LICENSE.llama.cpp");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|err| format!("could not create {}: {err}", path.display()))?;
    file.write_all(LLAMA_CPP_LICENSE)
        .map_err(|err| format!("could not write {}: {err}", path.display()))?;
    file.sync_all()
        .map_err(|err| format!("could not finish {}: {err}", path.display()))
}

fn write_generated_runtime_manifest(
    artifact: &RuntimeArtifact,
    stage_root: &Path,
) -> Result<(), String> {
    let manifest = RuntimeArtifactManifest {
        manifest_version: 1,
        runtime_id: artifact.runtime_id.clone(),
        version: artifact.version.clone(),
        platform: format!("{}-{}", artifact.os, artifact.arch),
        device: artifact.device,
        entrypoint: artifact.entrypoint.clone(),
        portable: true,
        upstream_repository: artifact.upstream_repository.clone(),
        upstream_revision: artifact.upstream_revision.clone(),
        upstream_asset: artifact.upstream_asset.clone(),
        upstream_sha256: artifact.upstream_sha256.clone(),
        upstream_size_bytes: artifact.upstream_size_bytes,
        license: artifact.license.clone(),
        license_sha256: artifact.license_sha256.clone(),
    };
    let mut bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|err| format!("could not serialize runtime manifest: {err}"))?;
    bytes.push(b'\n');
    let path = stage_root.join("runtime-manifest.json");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|err| format!("could not create {}: {err}", path.display()))?;
    file.write_all(&bytes)
        .map_err(|err| format!("could not write {}: {err}", path.display()))?;
    file.sync_all()
        .map_err(|err| format!("could not finish {}: {err}", path.display()))
}

fn copy_archive_entry_bounded(
    mut reader: impl Read,
    mut output: impl Write,
    name: &str,
    declared_size: u64,
    extracted_total: &mut u64,
    maximum_total: u64,
) -> Result<(), String> {
    let mut extracted_entry = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|err| format!("could not read runtime archive entry {name:?}: {err}"))?;
        if count == 0 {
            break;
        }
        extracted_entry = extracted_entry
            .checked_add(count as u64)
            .ok_or_else(|| "runtime archive entry size overflowed".to_owned())?;
        *extracted_total = extracted_total
            .checked_add(count as u64)
            .ok_or_else(|| "runtime artifact unpacked size overflowed".to_owned())?;
        if extracted_entry > declared_size || *extracted_total > maximum_total {
            return Err(format!(
                "runtime archive entry {name:?} exceeds its declared or allowed unpacked size"
            ));
        }
        output
            .write_all(&buffer[..count])
            .map_err(|err| format!("could not write runtime archive entry {name:?}: {err}"))?;
    }
    if extracted_entry != declared_size {
        return Err(format!(
            "runtime archive entry {name:?} size mismatch: declared {declared_size}, extracted {extracted_entry}"
        ));
    }
    Ok(())
}

fn validate_extracted_manifest(
    artifact: &RuntimeArtifact,
    stage_root: &Path,
) -> Result<(), String> {
    let manifest_path = stage_root.join("runtime-manifest.json");
    let contents = fs::read_to_string(&manifest_path).map_err(|err| {
        format!("runtime artifact is missing a readable runtime-manifest.json: {err}")
    })?;
    let manifest: RuntimeArtifactManifest = serde_json::from_str(&contents)
        .map_err(|err| format!("runtime artifact manifest is invalid: {err}"))?;
    let expected_platform = format!("{}-{}", artifact.os, artifact.arch);
    if manifest.manifest_version != 1
        || manifest.runtime_id != artifact.runtime_id
        || manifest.version != artifact.version
        || manifest.platform != expected_platform
        || manifest.device != artifact.device
        || manifest.entrypoint != artifact.entrypoint
        || !manifest.portable
        || manifest.upstream_repository != artifact.upstream_repository
        || manifest.upstream_revision != artifact.upstream_revision
        || manifest.upstream_asset != artifact.upstream_asset
        || manifest.upstream_sha256 != artifact.upstream_sha256
        || manifest.upstream_size_bytes != artifact.upstream_size_bytes
        || manifest.license != artifact.license
        || manifest.license_sha256 != artifact.license_sha256
    {
        return Err(format!(
            "runtime artifact manifest does not match trusted catalog identity for {} {} {} {}",
            artifact.runtime_id,
            artifact.version,
            expected_platform,
            artifact.device.as_str()
        ));
    }
    Ok(())
}

pub(crate) fn is_portable_runtime_entrypoint(runtime_id: &str, executable: &Path) -> bool {
    let Some(root) = executable
        .parent()
        .filter(|parent| parent.file_name().is_some_and(|name| name == "bin"))
        .and_then(Path::parent)
    else {
        return false;
    };
    fs::read_to_string(root.join("runtime-manifest.json"))
        .ok()
        .and_then(|contents| serde_json::from_str::<RuntimeArtifactManifest>(&contents).ok())
        .is_some_and(|manifest| {
            manifest.manifest_version == 1
                && manifest.portable
                && manifest.runtime_id == runtime_id
                && validate_relative_entrypoint(&manifest.entrypoint).is_ok()
                && root.join(manifest.entrypoint) == executable
        })
}

pub(crate) fn managed_install_matches_artifact(
    install: &config::ManagedRuntimeInstall,
    artifact: &RuntimeArtifact,
) -> bool {
    let platform = format!("{}-{}", artifact.os, artifact.arch);
    install.source.as_deref() == Some(artifact.url.as_str())
        && install.version.as_deref() == Some(artifact.version.as_str())
        && install.sha256.as_deref() == Some(artifact.sha256.as_str())
        && install.platform.as_deref() == Some(platform.as_str())
        && install.device.as_deref() == Some(artifact.device.as_str())
}

pub(crate) fn managed_model_install_matches_artifact(
    install: &config::ManagedModelInstall,
    artifact: &IntentModelArtifact,
) -> bool {
    let Some(url) = artifact.url.as_deref() else {
        return false;
    };
    install.source.as_deref() == Some(url)
        && install.version.as_deref() == Some(artifact.version.as_str())
        && install.sha256.as_deref() == Some(artifact.sha256.as_str())
        && install.platform.as_deref() == Some(config::current_platform_key().as_str())
}

fn validate_archive_entry_count(count: usize) -> Result<(), String> {
    const MAX_ARCHIVE_ENTRIES: usize = 100_000;
    if count > MAX_ARCHIVE_ENTRIES {
        Err(format!(
            "runtime artifact contains too many entries: {count} exceeds {MAX_ARCHIVE_ENTRIES}"
        ))
    } else {
        Ok(())
    }
}

fn transaction_path(target_root: &Path, phase: &str) -> PathBuf {
    let name = target_root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("runtime");
    target_root.with_file_name(format!(".{name}.{phase}"))
}

fn remove_path_if_exists(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    let result = if path.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
    result.map_err(|err| format!("could not remove {}: {err}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::io::Cursor;
    use zip::write::SimpleFileOptions;

    fn catalog_json(artifacts: &str) -> String {
        format!(
            r#"{{"schema_version":1,"catalog_version":"2026.07.28","artifacts":[{artifacts}]}}"#
        )
    }

    fn artifact(runtime_id: &str, os: &str, arch: &str, device: &str) -> String {
        let provenance = if runtime_id == VOICE_INTENT_LLAMA_CPP_RUNTIME_ID {
            r#","upstream_repository":"ggml-org/llama.cpp","upstream_revision":"aedb2a5e9ca3d4064148bbb919e0ddc0c1b70ab3","upstream_asset":"llama-b9637-bin-win-cpu-x64.zip","upstream_sha256":"f7783c2b8c007f95e710ac40f26a24861a80b603b0b739fc54d7c926a4716c1e","upstream_size_bytes":16906751,"license":"MIT","license_sha256":"94f29bbed6a22c35b992c5c6ebf0e7c92f13b836b90f36f461c9cf2f0f1d010d""#
        } else {
            ""
        };
        let version = if runtime_id == VOICE_INTENT_LLAMA_CPP_RUNTIME_ID {
            "b9637"
        } else {
            "1.2.3"
        };
        format!(
            r#"{{"runtime_id":"{runtime_id}","version":"{version}","os":"{os}","arch":"{arch}","device":"{device}","url":"https://github.com/scribe-runtime-tests/releases/download/1.2.3/{runtime_id}.zip","sha256":"{}","size_bytes":123,"unpacked_size_bytes":456,"entrypoint":"bin/runtime"{provenance}}}"#,
            "a".repeat(64)
        )
    }

    fn intent_catalog_json(models: &str) -> String {
        format!(
            r#"{{"schema_version":2,"catalog_version":"2026.07.29","artifacts":[],"intent_models":[{models}]}}"#
        )
    }

    fn intent_model(tier: &str, model_id: &str, url: Option<&str>) -> String {
        let (version, repository, revision, filename, approved_url, size, sha, managed_path) =
            match tier {
                "balanced" => (
                    "Qwen3-1.7B",
                    "Qwen/Qwen3-1.7B-GGUF",
                    "90862c4b9d2787eaed51d12237eafdfe7c5f6077",
                    "Qwen3-1.7B-Q8_0.gguf",
                    "https://huggingface.co/Qwen/Qwen3-1.7B-GGUF/resolve/90862c4b9d2787eaed51d12237eafdfe7c5f6077/Qwen3-1.7B-Q8_0.gguf",
                    1_834_426_016_u64,
                    "061b54daade076b5d3362dac252678d17da8c68f07560be70818cace6590cb1a",
                    "voice-intent/Qwen3-1.7B-Q8_0.gguf",
                ),
                _ => (
                    "Qwen3-0.6B",
                    "Qwen/Qwen3-0.6B-GGUF",
                    "ef4088322893040952513f532f736ddeab518403",
                    "Qwen3-0.6B-Q8_0.gguf",
                    "https://huggingface.co/Qwen/Qwen3-0.6B-GGUF/resolve/ef4088322893040952513f532f736ddeab518403/Qwen3-0.6B-Q8_0.gguf",
                    804_753_088_u64,
                    "12fae8b8f78f0360b498d04c8db7d33aff29ab7d8080231f93a17c18119e6735",
                    "voice-intent/Qwen3-0.6B-Q8_0.gguf",
                ),
            };
        let url = url.unwrap_or(approved_url);
        format!(
            r#"{{"runtime_id":"voice_intent_llama_cpp","tier":"{tier}","model_id":"{model_id}","version":"{version}","upstream_repository":"{repository}","upstream_revision":"{revision}","upstream_filename":"{filename}","license":"Apache-2.0","license_sha256":"{}","url":"{url}","sha256":"{sha}","size_bytes":{size},"managed_relative_path":"{managed_path}"}}"#,
            "5de36594c10839788a8c589443a8ef9d8b8d17c65a1b5807206ae037fc36c6bd",
        )
    }

    fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, contents) in entries {
            writer
                .start_file(*name, SimpleFileOptions::default().unix_permissions(0o755))
                .unwrap();
            writer.write_all(contents).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    fn http_response(
        status: u16,
        locations: &[&str],
        content_length: Option<&str>,
        body: &[u8],
    ) -> ArtifactHttpResponse {
        ArtifactHttpResponse {
            status,
            locations: locations.iter().map(|value| (*value).to_owned()).collect(),
            content_encoding: None,
            content_length: content_length.map(ToOwned::to_owned),
            content_type: Some("application/octet-stream".to_owned()),
            body: Box::new(Cursor::new(body.to_vec())),
        }
    }

    #[test]
    fn controlled_redirects_accept_only_expected_official_host_transitions() {
        let mut github = VecDeque::from([
            http_response(
                302,
                &[
                    "https://release-assets.githubusercontent.com/github-production-release-asset/file.zip?sig=secret",
                ],
                Some("0"),
                b"",
            ),
            http_response(200, &[], Some("4"), b"data"),
        ]);
        let response = follow_artifact_redirects_with(
            LLAMA_CPP_OFFICIAL_URL,
            OfficialDownloadPolicy::LlamaCppRuntime,
            4,
            "runtime artifact",
            Instant::now() + Duration::from_secs(1),
            |_| Ok(github.pop_front().unwrap()),
        )
        .unwrap();
        assert_eq!(response.status, 200);

        let compact = "https://huggingface.co/Qwen/Qwen3-0.6B-GGUF/resolve/ef4088322893040952513f532f736ddeab518403/Qwen3-0.6B-Q8_0.gguf";
        let mut hugging_face = VecDeque::from([
            http_response(
                302,
                &["https://us-east-1.cdn.hf.co/model.gguf?X-Amz-Signature=secret"],
                None,
                b"",
            ),
            http_response(200, &[], None, b"data"),
        ]);
        assert!(
            follow_artifact_redirects_with(
                compact,
                OfficialDownloadPolicy::HuggingFaceModel,
                4,
                "voice intent model",
                Instant::now() + Duration::from_secs(1),
                |_| Ok(hugging_face.pop_front().unwrap()),
            )
            .is_ok()
        );

        for target in [
            "http://us-east-1.cdn.hf.co/model.gguf",
            "https://cdn.hf.co.attacker.example/model.gguf",
            "https://huggingface.co.evil.example/model.gguf",
            "https://127.0.0.1/model.gguf",
            "https://user@us-east-1.cdn.hf.co/model.gguf",
            "https://us-east-1.cdn.hf.co:444/model.gguf",
        ] {
            let mut responses = VecDeque::from([http_response(302, &[target], None, b"")]);
            let error = follow_artifact_redirects_with(
                compact,
                OfficialDownloadPolicy::HuggingFaceModel,
                4,
                "voice intent model",
                Instant::now() + Duration::from_secs(1),
                |_| Ok(responses.pop_front().unwrap()),
            )
            .err()
            .unwrap();
            assert!(!error.contains(target));
            assert!(!error.contains("secret"));
        }
    }

    #[test]
    fn controlled_redirects_reject_loops_limits_and_invalid_responses() {
        let compact = "https://huggingface.co/Qwen/Qwen3-0.6B-GGUF/resolve/ef4088322893040952513f532f736ddeab518403/Qwen3-0.6B-Q8_0.gguf";
        let mut loop_responses = VecDeque::from([
            http_response(302, &["https://a.cdn.hf.co/model?token=1"], None, b""),
            http_response(302, &["https://b.cdn.hf.co/model?token=2"], None, b""),
            http_response(302, &["https://a.cdn.hf.co/model?token=1"], None, b""),
        ]);
        let error = follow_artifact_redirects_with(
            compact,
            OfficialDownloadPolicy::HuggingFaceModel,
            4,
            "voice intent model",
            Instant::now() + Duration::from_secs(1),
            |_| Ok(loop_responses.pop_front().unwrap()),
        )
        .err()
        .unwrap();
        assert!(error.contains("loop detected"));

        let mut too_many = VecDeque::from([
            http_response(302, &["https://a.cdn.hf.co/model?hop=1"], None, b""),
            http_response(302, &["https://b.cdn.hf.co/model?hop=2"], None, b""),
            http_response(302, &["https://c.cdn.hf.co/model?hop=3"], None, b""),
            http_response(302, &["https://d.cdn.hf.co/model?hop=4"], None, b""),
        ]);
        let error = follow_artifact_redirects_with(
            compact,
            OfficialDownloadPolicy::HuggingFaceModel,
            4,
            "voice intent model",
            Instant::now() + Duration::from_secs(1),
            |_| Ok(too_many.pop_front().unwrap()),
        )
        .err()
        .unwrap();
        assert!(error.contains("redirect limit"));

        let oversized_location = format!(
            "https://a.cdn.hf.co/{}",
            "a".repeat(MAX_REDIRECT_LOCATION_BYTES)
        );
        for response in [
            http_response(302, &[], None, b""),
            http_response(
                302,
                &["https://a.cdn.hf.co/one", "https://b.cdn.hf.co/two"],
                None,
                b"",
            ),
            http_response(302, &[&oversized_location], None, b""),
            http_response(302, &["https://a.cdn.hf.co/model.gguf#fragment"], None, b""),
            http_response(302, &["https://a.cdn.hf.co:444/model.gguf"], None, b""),
        ] {
            let mut responses = Some(response);
            assert!(
                follow_artifact_redirects_with(
                    compact,
                    OfficialDownloadPolicy::HuggingFaceModel,
                    4,
                    "voice intent model",
                    Instant::now() + Duration::from_secs(1),
                    |_| Ok(responses.take().unwrap()),
                )
                .is_err()
            );
        }

        for response in [
            ArtifactHttpResponse {
                content_encoding: Some("gzip".to_owned()),
                ..http_response(200, &[], Some("4"), b"data")
            },
            ArtifactHttpResponse {
                content_type: Some("text/html".to_owned()),
                ..http_response(200, &[], Some("4"), b"data")
            },
            http_response(200, &[], Some("5"), b"data"),
            http_response(200, &[], Some("four"), b"data"),
            http_response(503, &[], Some("4"), b"data"),
        ] {
            let mut responses = Some(response);
            assert!(
                follow_artifact_redirects_with(
                    compact,
                    OfficialDownloadPolicy::HuggingFaceModel,
                    4,
                    "voice intent model",
                    Instant::now() + Duration::from_secs(1),
                    |_| Ok(responses.take().unwrap()),
                )
                .is_err()
            );
        }
    }

    fn manifest() -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "manifest_version": 1,
            "runtime_id": "whisper_cpp",
            "version": "1.2.3",
            "platform": format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            "device": "cpu",
            "entrypoint": "bin/whisper-cli",
            "portable": true
        }))
        .unwrap()
    }

    fn test_artifact(bytes: &[u8], expected_size: u64, unpacked_size: u64) -> RuntimeArtifact {
        RuntimeArtifact {
            runtime_id: "whisper_cpp".to_owned(),
            version: "1.2.3".to_owned(),
            os: std::env::consts::OS.to_owned(),
            arch: std::env::consts::ARCH.to_owned(),
            device: RuntimeDevicePack::Cpu,
            url: "https://github.com/scribe-runtime-tests/runtime.zip".to_owned(),
            sha256: format!("{:x}", Sha256::digest(bytes)),
            size_bytes: expected_size,
            unpacked_size_bytes: unpacked_size,
            entrypoint: PathBuf::from("bin/whisper-cli"),
            archive_layout: ArchiveLayout::ScribePortableZipV1,
            upstream_repository: None,
            upstream_revision: None,
            upstream_asset: None,
            upstream_sha256: None,
            upstream_size_bytes: None,
            license: None,
            license_sha256: None,
        }
    }

    fn test_official_llama_artifact(unpacked_size: u64) -> RuntimeArtifact {
        RuntimeArtifact {
            runtime_id: VOICE_INTENT_LLAMA_CPP_RUNTIME_ID.to_owned(),
            version: "b9637".to_owned(),
            os: "windows".to_owned(),
            arch: "x86_64".to_owned(),
            device: RuntimeDevicePack::Cpu,
            url: LLAMA_CPP_OFFICIAL_URL.to_owned(),
            sha256: "f7783c2b8c007f95e710ac40f26a24861a80b603b0b739fc54d7c926a4716c1e".to_owned(),
            size_bytes: 16_906_751,
            unpacked_size_bytes: unpacked_size,
            entrypoint: PathBuf::from("bin/llama-server.exe"),
            archive_layout: ArchiveLayout::UpstreamLlamaCppFlatZipV1,
            upstream_repository: Some("ggml-org/llama.cpp".to_owned()),
            upstream_revision: Some("aedb2a5e9ca3d4064148bbb919e0ddc0c1b70ab3".to_owned()),
            upstream_asset: Some("llama-b9637-bin-win-cpu-x64.zip".to_owned()),
            upstream_sha256: Some(
                "f7783c2b8c007f95e710ac40f26a24861a80b603b0b739fc54d7c926a4716c1e".to_owned(),
            ),
            upstream_size_bytes: Some(16_906_751),
            license: Some("MIT".to_owned()),
            license_sha256: Some(
                "94f29bbed6a22c35b992c5c6ebf0e7c92f13b836b90f36f461c9cf2f0f1d010d".to_owned(),
            ),
        }
    }

    #[test]
    fn official_llama_flat_zip_is_normalized_with_pinned_license_and_manifest() {
        let bytes = archive(&[
            ("llama-server.exe", b"MZserver"),
            ("ggml.dll", b"MZdll"),
            ("README.md", b"info"),
        ]);
        let root = temp_target("llama-normalize");
        let artifact = test_official_llama_artifact(17);
        extract_upstream_llama_cpp_archive_with_expectations(
            &artifact,
            Cursor::new(bytes),
            &root,
            LlamaArchiveExpectations {
                entry_count: 3,
                unpacked_size: 17,
                dll_count: 1,
                payload_size: 13,
            },
        )
        .unwrap();

        assert_eq!(
            fs::read(root.join("bin/llama-server.exe")).unwrap(),
            b"MZserver"
        );
        assert_eq!(fs::read(root.join("bin/ggml.dll")).unwrap(), b"MZdll");
        assert!(!root.join("README.md").exists());
        assert_eq!(
            fs::read(root.join("LICENSE.llama.cpp")).unwrap(),
            LLAMA_CPP_LICENSE
        );
        validate_extracted_manifest(&artifact, &root).unwrap();

        let manifest_path = root.join("runtime-manifest.json");
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest["unexpected"] = serde_json::json!(true);
        fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert!(validate_extracted_manifest(&artifact, &root).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn official_llama_transform_rejects_unsafe_names_and_cleans_via_staging() {
        let bytes = archive(&[
            ("../llama-server.exe", b"MZserver"),
            ("ggml.dll", b"MZdll"),
            ("README.md", b"info"),
        ]);
        let root = temp_target("llama-unsafe");
        let artifact = test_official_llama_artifact(17);
        let error = extract_upstream_llama_cpp_archive_with_expectations(
            &artifact,
            Cursor::new(bytes),
            &root,
            LlamaArchiveExpectations {
                entry_count: 3,
                unpacked_size: 17,
                dll_count: 1,
                payload_size: 13,
            },
        )
        .unwrap_err();
        assert!(error.contains("unsafe"));
        fs::remove_dir_all(root).unwrap();
    }

    fn assert_llama_transform_rejected(
        name: &str,
        bytes: Vec<u8>,
        unpacked_size: u64,
        expected: LlamaArchiveExpectations,
    ) {
        let root = temp_target(name);
        let error = extract_upstream_llama_cpp_archive_with_expectations(
            &test_official_llama_artifact(unpacked_size),
            Cursor::new(bytes),
            &root,
            expected,
        )
        .unwrap_err();
        assert!(!error.is_empty());
        if root.exists() {
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn official_llama_transform_rejects_ambiguous_special_and_missing_payloads() {
        for (name, bytes, unpacked_size, expected) in [
            (
                "llama-casefold-duplicate",
                archive(&[
                    ("llama-server.exe", b"MZserver"),
                    ("GGML.dll", b"MZdll"),
                    ("ggml.dll", b"MZdll"),
                    ("README.md", b"info"),
                ]),
                22,
                LlamaArchiveExpectations {
                    entry_count: 4,
                    unpacked_size: 22,
                    dll_count: 2,
                    payload_size: 18,
                },
            ),
            (
                "llama-reserved-name",
                archive(&[
                    ("llama-server.exe", b"MZserver"),
                    ("CON.dll", b"MZdll"),
                    ("README.md", b"info"),
                ]),
                17,
                LlamaArchiveExpectations {
                    entry_count: 3,
                    unpacked_size: 17,
                    dll_count: 1,
                    payload_size: 13,
                },
            ),
            (
                "llama-colon-name",
                archive(&[
                    ("llama-server.exe", b"MZserver"),
                    ("bad:name.dll", b"MZdll"),
                    ("README.md", b"info"),
                ]),
                17,
                LlamaArchiveExpectations {
                    entry_count: 3,
                    unpacked_size: 17,
                    dll_count: 1,
                    payload_size: 13,
                },
            ),
            (
                "llama-backslash-name",
                archive(&[
                    ("llama-server.exe", b"MZserver"),
                    ("nested\\ggml.dll", b"MZdll"),
                    ("README.md", b"info"),
                ]),
                17,
                LlamaArchiveExpectations {
                    entry_count: 3,
                    unpacked_size: 17,
                    dll_count: 1,
                    payload_size: 13,
                },
            ),
            (
                "llama-missing-server",
                archive(&[("ggml.dll", b"MZdll"), ("README.md", b"info")]),
                9,
                LlamaArchiveExpectations {
                    entry_count: 2,
                    unpacked_size: 9,
                    dll_count: 1,
                    payload_size: 5,
                },
            ),
            (
                "llama-missing-dll",
                archive(&[("llama-server.exe", b"MZserver"), ("README.md", b"info")]),
                12,
                LlamaArchiveExpectations {
                    entry_count: 2,
                    unpacked_size: 12,
                    dll_count: 1,
                    payload_size: 8,
                },
            ),
            (
                "llama-wrong-pe",
                archive(&[
                    ("llama-server.exe", b"NOserver"),
                    ("ggml.dll", b"MZdll"),
                    ("README.md", b"info"),
                ]),
                17,
                LlamaArchiveExpectations {
                    entry_count: 3,
                    unpacked_size: 17,
                    dll_count: 1,
                    payload_size: 13,
                },
            ),
        ] {
            assert_llama_transform_rejected(name, bytes, unpacked_size, expected);
        }

        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .add_symlink(
                "linked.dll",
                "ggml.dll",
                SimpleFileOptions::default().unix_permissions(0o777),
            )
            .unwrap();
        let symlink = writer.finish().unwrap().into_inner();
        assert_llama_transform_rejected(
            "llama-symlink",
            symlink,
            8,
            LlamaArchiveExpectations {
                entry_count: 1,
                unpacked_size: 8,
                dll_count: 1,
                payload_size: 8,
            },
        );
    }

    #[test]
    fn official_llama_transform_rejects_entry_raw_and_payload_inventory_mismatches() {
        let bytes = archive(&[
            ("llama-server.exe", b"MZserver"),
            ("ggml.dll", b"MZdll"),
            ("README.md", b"info"),
        ]);
        for (name, artifact_size, expected) in [
            (
                "llama-entry-count",
                17,
                LlamaArchiveExpectations {
                    entry_count: 4,
                    unpacked_size: 17,
                    dll_count: 1,
                    payload_size: 13,
                },
            ),
            (
                "llama-raw-inventory",
                18,
                LlamaArchiveExpectations {
                    entry_count: 3,
                    unpacked_size: 18,
                    dll_count: 1,
                    payload_size: 13,
                },
            ),
            (
                "llama-payload-size",
                17,
                LlamaArchiveExpectations {
                    entry_count: 3,
                    unpacked_size: 17,
                    dll_count: 1,
                    payload_size: 14,
                },
            ),
            (
                "llama-dll-inventory",
                17,
                LlamaArchiveExpectations {
                    entry_count: 3,
                    unpacked_size: 17,
                    dll_count: 2,
                    payload_size: 13,
                },
            ),
        ] {
            assert_llama_transform_rejected(name, bytes.clone(), artifact_size, expected);
        }
    }

    #[test]
    fn special_llama_transform_failure_cleans_outer_staging_transactions() {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file(
                "../llama-server.exe",
                SimpleFileOptions::default().unix_permissions(0o755),
            )
            .unwrap();
        writer.write_all(b"MZserver").unwrap();
        for index in 0..50 {
            writer
                .start_file(
                    format!("ignored-{index}.txt"),
                    SimpleFileOptions::default().unix_permissions(0o644),
                )
                .unwrap();
        }
        let bytes = writer.finish().unwrap().into_inner();
        let mut artifact = test_official_llama_artifact(LLAMA_CPP_UPSTREAM_UNPACKED_SIZE);
        artifact.size_bytes = bytes.len() as u64;
        artifact.sha256 = format!("{:x}", Sha256::digest(&bytes));
        let target = temp_target("llama-outer-cleanup").join(VOICE_INTENT_LLAMA_CPP_RUNTIME_ID);

        let error = stage_from_reader(&artifact, &target, Cursor::new(bytes)).unwrap_err();

        assert!(error.contains("unsafe"));
        assert!(
            transaction_files(target.parent().unwrap(), VOICE_INTENT_LLAMA_CPP_RUNTIME_ID)
                .is_empty()
        );
        fs::remove_dir_all(target.parent().unwrap()).unwrap();
    }

    fn test_intent_model(bytes: &[u8], expected_size: u64) -> IntentModelArtifact {
        IntentModelArtifact {
            runtime_id: VOICE_INTENT_LLAMA_CPP_RUNTIME_ID.to_owned(),
            tier: IntentModelTier::Balanced,
            model_id: "qwen3_1_7b_q8_0".to_owned(),
            version: "Qwen3-1.7B".to_owned(),
            upstream_repository: "Qwen/Qwen3-1.7B-GGUF".to_owned(),
            upstream_revision: "a".repeat(40),
            upstream_filename: "Qwen3-1.7B-Q8_0.gguf".to_owned(),
            license: "Apache-2.0".to_owned(),
            license_sha256: "5de36594c10839788a8c589443a8ef9d8b8d17c65a1b5807206ae037fc36c6bd"
                .to_owned(),
            url: Some("https://github.com/scribe-runtime-tests/Qwen3-1.7B-Q8_0.gguf".to_owned()),
            sha256: format!("{:x}", Sha256::digest(bytes)),
            size_bytes: expected_size,
            managed_relative_path: PathBuf::from("voice-intent/Qwen3-1.7B-Q8_0.gguf"),
        }
    }

    fn test_model_install(
        model: &IntentModelArtifact,
        path: PathBuf,
    ) -> config::ManagedModelInstall {
        let mut install = config::ManagedModelInstall::app_managed(path, "test-release");
        install.source = model.url.clone();
        install.version = Some(model.version.clone());
        install.sha256 = Some(model.sha256.clone());
        install
    }

    #[test]
    fn managed_model_install_must_match_approved_artifact_metadata() {
        let model = test_intent_model(b"model", 5);
        let mut install = test_model_install(&model, PathBuf::from("voice.gguf"));

        assert!(managed_model_install_matches_artifact(&install, &model));

        install.sha256 = Some("0".repeat(64));
        assert!(!managed_model_install_matches_artifact(&install, &model));
        install.sha256 = Some(model.sha256.clone());
        install.version = Some("stale-version".to_owned());
        assert!(!managed_model_install_matches_artifact(&install, &model));
        install.version = Some(model.version.clone());
        install.platform = Some("linux-x86_64".to_owned());
        assert!(!managed_model_install_matches_artifact(&install, &model));
    }

    #[test]
    fn managed_runtime_install_must_match_approved_artifact_metadata() {
        let artifact = test_artifact(b"runtime", 7, 7);
        let mut install =
            config::ManagedRuntimeInstall::app_managed(PathBuf::from("runtime"), &artifact.url);
        install.version = Some(artifact.version.clone());
        install.sha256 = Some(artifact.sha256.clone());
        install.platform = Some(format!("{}-{}", artifact.os, artifact.arch));
        install.device = Some(artifact.device.as_str().to_owned());

        assert!(managed_install_matches_artifact(&install, &artifact));

        install.device = Some("gpu".to_owned());
        assert!(!managed_install_matches_artifact(&install, &artifact));
        install.device = Some(artifact.device.as_str().to_owned());
        install.source = Some("https://example.com/stale-runtime.zip".to_owned());
        assert!(!managed_install_matches_artifact(&install, &artifact));
    }

    #[test]
    fn unpublished_model_artifact_never_matches_an_install() {
        let mut model = test_intent_model(b"model", 5);
        let install = test_model_install(&model, PathBuf::from("voice.gguf"));
        model.url = None;

        assert!(!managed_model_install_matches_artifact(&install, &model));
    }

    fn commit_test_model(
        model: &IntentModelArtifact,
        mut replacement: IntentModelReplacement,
    ) -> PathBuf {
        let path = replacement.installed_path.clone();
        let install = test_model_install(model, path.clone());
        replacement.prepare_persistence(Some(&install)).unwrap();
        replacement.commit().unwrap();
        path
    }

    fn temp_target(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "scribe-runtime-artifact-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn transaction_files(parent: &Path, target_name: &str) -> Vec<PathBuf> {
        fs::read_dir(parent)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(&format!(".{target_name}.")))
            })
            .collect()
    }

    #[test]
    fn checked_in_default_catalog_fails_closed_without_fake_artifacts() {
        let catalog =
            RuntimeArtifactCatalog::parse(include_str!("../runtime-artifacts.default.json"))
                .unwrap();
        assert!(
            catalog
                .select(
                    "vosk",
                    std::env::consts::OS,
                    std::env::consts::ARCH,
                    RuntimeDevicePack::Cpu
                )
                .is_none()
        );
    }

    #[test]
    fn checked_in_catalog_pins_voice_intent_tiers_without_registering_an_stt_provider() {
        let catalog =
            RuntimeArtifactCatalog::parse(include_str!("../runtime-artifacts.default.json"))
                .unwrap();
        let compact = catalog.intent_model(IntentModelTier::Compact).unwrap();
        let balanced = catalog.intent_model(IntentModelTier::Balanced).unwrap();

        assert_eq!(compact.runtime_id, VOICE_INTENT_LLAMA_CPP_RUNTIME_ID);
        assert_eq!(compact.version, "Qwen3-0.6B");
        assert_eq!(compact.size_bytes, 804_753_088);
        assert_eq!(
            compact.sha256,
            "12fae8b8f78f0360b498d04c8db7d33aff29ab7d8080231f93a17c18119e6735"
        );
        assert_eq!(
            compact.upstream_revision,
            "ef4088322893040952513f532f736ddeab518403"
        );
        assert_eq!(balanced.version, "Qwen3-1.7B");
        assert_eq!(balanced.size_bytes, 1_834_426_016);
        assert_eq!(
            balanced.sha256,
            "061b54daade076b5d3362dac252678d17da8c68f07560be70818cace6590cb1a"
        );
        assert_eq!(
            balanced.upstream_revision,
            "90862c4b9d2787eaed51d12237eafdfe7c5f6077"
        );
        assert_eq!(compact.license, "Apache-2.0");
        assert_eq!(balanced.license, "Apache-2.0");
        assert_eq!(
            compact.license_sha256,
            "5de36594c10839788a8c589443a8ef9d8b8d17c65a1b5807206ae037fc36c6bd"
        );
        assert_eq!(balanced.license_sha256, compact.license_sha256);
        assert_eq!(
            compact.url.as_deref(),
            Some(
                "https://huggingface.co/Qwen/Qwen3-0.6B-GGUF/resolve/ef4088322893040952513f532f736ddeab518403/Qwen3-0.6B-Q8_0.gguf"
            )
        );
        assert_eq!(
            balanced.url.as_deref(),
            Some(
                "https://huggingface.co/Qwen/Qwen3-1.7B-GGUF/resolve/90862c4b9d2787eaed51d12237eafdfe7c5f6077/Qwen3-1.7B-Q8_0.gguf"
            )
        );
        let runtime = catalog
            .select(
                VOICE_INTENT_LLAMA_CPP_RUNTIME_ID,
                "windows",
                "x86_64",
                RuntimeDevicePack::Cpu,
            )
            .unwrap();
        assert_eq!(runtime.url, LLAMA_CPP_OFFICIAL_URL);
        assert_eq!(
            runtime.archive_layout,
            ArchiveLayout::UpstreamLlamaCppFlatZipV1
        );
        assert!(
            runtime_catalog::backend_spec_for_runtime_id(VOICE_INTENT_LLAMA_CPP_RUNTIME_ID)
                .is_none()
        );
    }

    #[test]
    #[ignore = "manual network smoke: downloads the exact official llama.cpp runtime and both Qwen models"]
    fn official_voice_artifact_downloads_smoke() {
        if std::env::var("SCRIBE_RUN_OFFICIAL_ARTIFACT_SMOKE").as_deref() != Ok("1") {
            eprintln!("set SCRIBE_RUN_OFFICIAL_ARTIFACT_SMOKE=1 to download all three artifacts");
            return;
        }
        let root = temp_target("official-download-smoke");
        let result = (|| -> Result<(), String> {
            let catalog =
                RuntimeArtifactCatalog::parse(include_str!("../runtime-artifacts.default.json"))?;
            let runtime = catalog
                .select(
                    VOICE_INTENT_LLAMA_CPP_RUNTIME_ID,
                    "windows",
                    "x86_64",
                    RuntimeDevicePack::Cpu,
                )
                .ok_or_else(|| "official Windows x64 llama.cpp runtime is missing".to_owned())?;
            let runtime_target = root.join(VOICE_INTENT_LLAMA_CPP_RUNTIME_ID);
            let staged = download_and_stage(runtime, &runtime_target)?;
            validate_extracted_manifest(runtime, &staged.root)?;
            let server_fingerprint = intent_model_file_fingerprint(&staged.entrypoint)?;
            if server_fingerprint
                != (
                    "06444801bb1dc38a848bb5a527728c4ea14ad2aa45ce7e81a29a5fb5d2560eaf".to_owned(),
                    9_216,
                )
            {
                return Err("normalized llama-server.exe fingerprint mismatch".to_owned());
            }
            remove_path_if_exists(&staged.root)?;

            for tier in [IntentModelTier::Compact, IntentModelTier::Balanced] {
                let model = catalog
                    .intent_model(tier)
                    .ok_or_else(|| format!("official {tier:?} model is missing"))?;
                let replacement = download_and_stage_intent_model(model, &root)?;
                let fingerprint = intent_model_file_fingerprint(&replacement.installed_path)?;
                if fingerprint != (model.sha256.clone(), model.size_bytes) {
                    return Err(format!("downloaded {tier:?} model fingerprint mismatch"));
                }
                replacement.rollback()?;
            }
            Ok(())
        })();
        let cleanup = if root.exists() {
            fs::remove_dir_all(&root)
                .map_err(|error| format!("could not clean smoke root {}: {error}", root.display()))
        } else {
            Ok(())
        };
        if let Err(error) = result {
            panic!("official artifact smoke failed: {error}; cleanup: {cleanup:?}");
        }
        cleanup.unwrap();
    }

    #[test]
    fn accepts_legacy_catalogs_but_requires_schema_two_for_intent_models() {
        assert!(
            RuntimeArtifactCatalog::parse(
                r#"{"schema_version":1,"catalog_version":"legacy","artifacts":[]}"#
            )
            .is_ok()
        );
        let model = intent_model("compact", "qwen3_0_6b_q8_0", None);
        let legacy =
            intent_catalog_json(&model).replace("\"schema_version\":2", "\"schema_version\":1");
        assert!(RuntimeArtifactCatalog::parse(&legacy).is_err());
    }

    #[test]
    fn intent_catalog_rejects_unknown_duplicate_and_mutable_models() {
        let compact = intent_model("compact", "qwen3_0_6b_q8_0", None);
        let balanced = intent_model("balanced", "qwen3_1_7b_q8_0", None);
        assert!(
            RuntimeArtifactCatalog::parse(&intent_catalog_json(&format!("{compact},{compact}")))
                .is_err()
        );
        assert!(
            RuntimeArtifactCatalog::parse(&intent_catalog_json(&format!("{compact},{balanced}")))
                .is_ok()
        );
        for invalid in [
            compact.replace("voice_intent_llama_cpp", "unknown"),
            compact.replace("\"tier\":\"compact\"", "\"tier\":\"gpu\""),
            compact.replace(
                "5de36594c10839788a8c589443a8ef9d8b8d17c65a1b5807206ae037fc36c6bd",
                &"A".repeat(64),
            ),
            intent_model(
                "compact",
                "qwen3_0_6b_q8_0",
                Some("http://github.com/release.gguf"),
            ),
            intent_model(
                "compact",
                "qwen3_0_6b_q8_0",
                Some("https://github.com/release.gguf?mutable=1"),
            ),
        ] {
            assert!(RuntimeArtifactCatalog::parse(&intent_catalog_json(&invalid)).is_err());
        }
    }

    #[test]
    fn auxiliary_runtime_is_cpu_only_without_expanding_the_stt_catalog() {
        let trusted = include_str!("../runtime-artifacts.default.json");
        assert!(RuntimeArtifactCatalog::parse(trusted).is_ok());
        assert!(
            RuntimeArtifactCatalog::parse(&trusted.replace(
                "f7783c2b8c007f95e710ac40f26a24861a80b603b0b739fc54d7c926a4716c1e",
                &"0".repeat(64),
            ))
            .is_err()
        );
        assert!(
            RuntimeArtifactCatalog::parse(
                &trusted.replace(r#""device": "cpu""#, r#""device": "gpu""#,)
            )
            .is_err()
        );
        assert!(
            runtime_catalog::backend_specs()
                .iter()
                .all(|spec| spec.runtime_id != VOICE_INTENT_LLAMA_CPP_RUNTIME_ID)
        );
    }

    #[test]
    fn raw_gguf_staging_verifies_size_hash_exclusivity_and_cleanup() {
        let bytes = b"verified gguf bytes";
        let root = temp_target("model-success");
        let model = test_intent_model(bytes, bytes.len() as u64);
        let staged = commit_test_model(
            &model,
            stage_intent_model_from_reader(&model, &root, Cursor::new(bytes)).unwrap(),
        );
        assert_eq!(fs::read(&staged).unwrap(), bytes);
        assert!(
            !root
                .join("voice-intent/.Qwen3-1.7B-Q8_0.gguf.partial")
                .exists()
        );
        fs::remove_dir_all(root).unwrap();

        for (name, model) in [
            (
                "hash",
                IntentModelArtifact {
                    sha256: "0".repeat(64),
                    ..test_intent_model(bytes, bytes.len() as u64)
                },
            ),
            (
                "truncated",
                test_intent_model(bytes, bytes.len() as u64 + 1),
            ),
            ("oversize", test_intent_model(bytes, bytes.len() as u64 - 1)),
        ] {
            let root = temp_target(name);
            assert!(stage_intent_model_from_reader(&model, &root, Cursor::new(bytes)).is_err());
            assert!(!root.join("voice-intent/Qwen3-1.7B-Q8_0.gguf").exists());
            assert!(
                !root
                    .join("voice-intent/.Qwen3-1.7B-Q8_0.gguf.partial")
                    .exists()
            );
            fs::remove_dir_all(root).unwrap();
        }

        let root = temp_target("stale-partial");
        let partial = root.join("voice-intent/.Qwen3-1.7B-Q8_0.gguf.partial");
        fs::create_dir_all(partial.parent().unwrap()).unwrap();
        fs::write(&partial, b"crashed download").unwrap();
        let model = test_intent_model(bytes, bytes.len() as u64);
        let staged = commit_test_model(
            &model,
            stage_intent_model_from_reader(&model, &root, Cursor::new(bytes)).unwrap(),
        );
        assert_eq!(fs::read(staged).unwrap(), bytes);
        assert!(!partial.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn raw_gguf_staging_honors_deadlines_and_requires_a_release_url() {
        let bytes = b"verified gguf bytes";
        let root = temp_target("model-deadline");
        let error = stage_intent_model_from_reader_until(
            &test_intent_model(bytes, bytes.len() as u64),
            &root,
            Cursor::new(bytes),
            Some(Instant::now()),
        )
        .unwrap_err();
        assert!(error.contains("deadline"));
        assert!(
            !root
                .join("voice-intent/.Qwen3-1.7B-Q8_0.gguf.partial")
                .exists()
        );
        fs::remove_dir_all(root).unwrap();

        let mut no_release = test_intent_model(bytes, bytes.len() as u64);
        no_release.url = None;
        let error =
            download_and_stage_intent_model(&no_release, &temp_target("no-release")).unwrap_err();
        assert!(error.contains("no release URL"));
    }

    #[test]
    fn raw_gguf_staging_reports_progress_and_cancellation_cleans_partial() {
        let bytes = b"verified gguf bytes";
        let model = test_intent_model(bytes, bytes.len() as u64);
        let progress_root = temp_target("model-progress");
        let mut updates = Vec::new();

        let staged = stage_intent_model_from_reader_until_with_progress(
            &model,
            &progress_root,
            Cursor::new(bytes),
            None,
            |progress| {
                updates.push(progress);
                DownloadControl::Continue
            },
        )
        .unwrap();

        assert_eq!(
            updates,
            [
                DownloadProgress {
                    downloaded_bytes: 0,
                    total_bytes: bytes.len() as u64,
                },
                DownloadProgress {
                    downloaded_bytes: bytes.len() as u64,
                    total_bytes: bytes.len() as u64,
                },
            ]
        );
        let staged = commit_test_model(&model, staged);
        assert_eq!(fs::read(staged).unwrap(), bytes);
        fs::remove_dir_all(progress_root).unwrap();

        let cancel_root = temp_target("model-cancel");
        let error = stage_intent_model_from_reader_until_with_progress(
            &model,
            &cancel_root,
            Cursor::new(bytes),
            None,
            |progress| {
                if progress.downloaded_bytes > 0 {
                    DownloadControl::Cancel
                } else {
                    DownloadControl::Continue
                }
            },
        )
        .unwrap_err();

        assert!(error.contains("cancelled"));
        assert!(!cancel_root.join(&model.managed_relative_path).exists());
        assert!(
            !cancel_root
                .join("voice-intent/.Qwen3-1.7B-Q8_0.gguf.partial")
                .exists()
        );
        fs::remove_dir_all(cancel_root).unwrap();
    }

    #[test]
    fn raw_gguf_staging_replaces_an_unpublished_completed_model() {
        let bytes = b"verified gguf bytes";
        let model = test_intent_model(bytes, bytes.len() as u64);
        let root = temp_target("model-interrupted-publication");
        let target = root.join(&model.managed_relative_path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, b"orphaned prior bytes").unwrap();

        let staged = commit_test_model(
            &model,
            stage_intent_model_from_reader(&model, &root, Cursor::new(bytes)).unwrap(),
        );

        assert_eq!(staged, target);
        assert_eq!(fs::read(&staged).unwrap(), bytes);
        assert!(
            !target
                .with_file_name(".Qwen3-1.7B-Q8_0.gguf.partial")
                .exists()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn model_publication_rolls_back_verified_previous_bytes_on_config_failure() {
        let old = b"previous verified gguf";
        let new = b"replacement verified gguf";
        let model = test_intent_model(new, new.len() as u64);
        let root = temp_target("model-config-rollback");
        let target = root.join(&model.managed_relative_path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, old).unwrap();

        let mut replacement =
            stage_intent_model_from_reader(&model, &root, Cursor::new(new)).unwrap();
        let install = test_model_install(&model, target.clone());
        replacement.prepare_persistence(Some(&install)).unwrap();
        replacement.rollback().unwrap();

        assert_eq!(fs::read(&target).unwrap(), old);
        assert!(!intent_model_transaction_path(&target, "backup").exists());
        assert!(!intent_model_transaction_path(&target, "transaction").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn publication_failure_injection_restores_previous_model_and_surfaces_rollback_failure() {
        let old = b"previous verified gguf";
        let new = b"replacement verified gguf";
        let model = test_intent_model(new, new.len() as u64);

        let root = temp_target("model-publish-failure");
        let target = root.join(&model.managed_relative_path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, old).unwrap();
        let lock = acquire_intent_model_install_lock_with_timeout(
            &model,
            &root,
            None,
            Duration::from_millis(10),
        )
        .unwrap();
        let partial = unique_intent_model_partial_path(&target);
        fs::write(&partial, new).unwrap();
        let error = publish_verified_intent_model(&model, target.clone(), partial, lock, |phase| {
            if phase == IntentModelTransactionPhase::Activated {
                Err("injected post-activation failure".to_owned())
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        assert!(error.contains("injected post-activation failure"));
        assert_eq!(fs::read(&target).unwrap(), old);
        fs::remove_dir_all(&root).unwrap();

        let root = temp_target("model-rollback-failure");
        let target = root.join(&model.managed_relative_path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, old).unwrap();
        let lock = acquire_intent_model_install_lock_with_timeout(
            &model,
            &root,
            None,
            Duration::from_millis(10),
        )
        .unwrap();
        let partial = unique_intent_model_partial_path(&target);
        fs::write(&partial, new).unwrap();
        let backup = intent_model_transaction_path(&target, "backup");
        let error = publish_verified_intent_model(&model, target.clone(), partial, lock, |phase| {
            if phase == IntentModelTransactionPhase::Activated {
                fs::remove_file(&backup).unwrap();
                Err("injected failure after losing backup".to_owned())
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        assert!(error.contains("rollback failed"));
        assert!(error.contains("previous model backup"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn model_install_lock_is_exclusive_and_crash_recovery_uses_persisted_record() {
        let old = b"previous verified gguf";
        let new = b"replacement verified gguf";
        let model = test_intent_model(new, new.len() as u64);
        let root = temp_target("model-lock-and-recovery");
        let target = root.join(&model.managed_relative_path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, old).unwrap();
        let previous = config::ManagedModelInstall::new(target.clone());

        let first = acquire_intent_model_install_lock_with_timeout(
            &model,
            &root,
            Some(&previous),
            Duration::from_millis(10),
        )
        .unwrap();
        let error = acquire_intent_model_install_lock_with_timeout(
            &model,
            &root,
            Some(&previous),
            Duration::from_millis(10),
        )
        .unwrap_err();
        assert!(error.contains("Another Scribe process"));
        drop(first);

        let lock = acquire_intent_model_install_lock_with_timeout(
            &model,
            &root,
            Some(&previous),
            Duration::from_millis(10),
        )
        .unwrap();
        let partial = unique_intent_model_partial_path(&target);
        fs::write(&partial, new).unwrap();
        let mut replacement =
            publish_verified_intent_model(&model, target.clone(), partial, lock, |_| Ok(()))
                .unwrap();
        let install = test_model_install(&model, target.clone());
        replacement.prepare_persistence(Some(&install)).unwrap();
        drop(replacement); // Simulate process exit after publication, before transaction cleanup.

        let recovered = acquire_intent_model_install_lock_with_timeout(
            &model,
            &root,
            Some(&previous),
            Duration::from_millis(10),
        )
        .unwrap();
        assert_eq!(fs::read(&target).unwrap(), old);
        assert!(!intent_model_transaction_path(&target, "backup").exists());
        drop(recovered);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovery_finishes_rollback_after_backup_was_already_restored() {
        let old = b"previous verified gguf";
        let new = b"replacement verified gguf";
        let model = test_intent_model(new, new.len() as u64);
        let root = temp_target("model-rollback-idempotent");
        let target = root.join(&model.managed_relative_path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, old).unwrap();
        let previous = config::ManagedModelInstall::new(target.clone());
        let lock = acquire_intent_model_install_lock_with_timeout(
            &model,
            &root,
            Some(&previous),
            Duration::from_millis(10),
        )
        .unwrap();
        let partial = unique_intent_model_partial_path(&target);
        fs::write(&partial, new).unwrap();
        let mut replacement =
            publish_verified_intent_model(&model, target.clone(), partial, lock, |_| Ok(()))
                .unwrap();
        let install = test_model_install(&model, target.clone());
        replacement.prepare_persistence(Some(&install)).unwrap();
        drop(replacement);

        let backup = intent_model_transaction_path(&target, "backup");
        rollback_intent_model_files(&target, Some(&backup), true).unwrap();
        assert_eq!(fs::read(&target).unwrap(), old);
        assert!(!backup.exists());
        assert!(intent_model_transaction_path(&target, "transaction").exists());

        let recovered = acquire_intent_model_install_lock_with_timeout(
            &model,
            &root,
            Some(&previous),
            Duration::from_millis(10),
        )
        .unwrap();
        assert_eq!(fs::read(&target).unwrap(), old);
        assert!(!intent_model_transaction_path(&target, "transaction").exists());
        drop(recovered);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn persisted_model_record_completes_recovery_and_stale_unique_partial_is_removed() {
        let old = b"previous verified gguf";
        let new = b"replacement verified gguf";
        let model = test_intent_model(new, new.len() as u64);
        let root = temp_target("model-recovery-commit");
        let target = root.join(&model.managed_relative_path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, old).unwrap();
        let previous = config::ManagedModelInstall::new(target.clone());
        let lock = acquire_intent_model_install_lock_with_timeout(
            &model,
            &root,
            Some(&previous),
            Duration::from_millis(10),
        )
        .unwrap();
        let partial = unique_intent_model_partial_path(&target);
        fs::write(&partial, new).unwrap();
        let mut replacement =
            publish_verified_intent_model(&model, target.clone(), partial, lock, |_| Ok(()))
                .unwrap();
        let install = test_model_install(&model, target.clone());
        replacement.prepare_persistence(Some(&install)).unwrap();
        drop(replacement);

        let stale_partial = unique_intent_model_partial_path(&target);
        fs::write(&stale_partial, b"crashed partial").unwrap();
        let recovered = acquire_intent_model_install_lock_with_timeout(
            &model,
            &root,
            Some(&install),
            Duration::from_millis(10),
        )
        .unwrap();
        assert_eq!(fs::read(&target).unwrap(), new);
        assert!(!intent_model_transaction_path(&target, "backup").exists());
        assert!(!stale_partial.exists());
        drop(recovered);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn model_removal_stages_files_until_config_commit_and_can_restore_them() {
        let bytes = b"verified gguf bytes";
        let model = test_intent_model(bytes, bytes.len() as u64);
        let root = temp_target("model-removal");
        let target = root.join(&model.managed_relative_path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(&target, bytes).unwrap();

        let mut removal = stage_intent_model_removal(&model, &root).unwrap();
        assert!(!target.exists());
        assert!(intent_model_transaction_path(&target, "backup").exists());
        removal.prepare_persistence(None).unwrap();
        removal.rollback().unwrap();
        assert_eq!(fs::read(&target).unwrap(), bytes);

        let mut removal = stage_intent_model_removal(&model, &root).unwrap();
        removal.prepare_persistence(None).unwrap();
        removal.commit().unwrap();
        assert!(!target.exists());
        assert!(!intent_model_transaction_path(&target, "backup").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn selects_exact_runtime_platform_and_device_tuple() {
        let json = catalog_json(
            &[
                artifact("whisper_cpp", "windows", "x86_64", "cpu"),
                artifact("whisper_cpp", "windows", "x86_64", "gpu"),
                artifact("whisper_cpp", "linux", "x86_64", "gpu"),
            ]
            .join(","),
        );
        let catalog = RuntimeArtifactCatalog::parse(&json).unwrap();

        assert_eq!(
            catalog
                .select("whisper_cpp", "windows", "x86_64", RuntimeDevicePack::Gpu)
                .unwrap()
                .device,
            RuntimeDevicePack::Gpu
        );
        assert!(
            catalog
                .select("whisper_cpp", "windows", "aarch64", RuntimeDevicePack::Gpu)
                .is_none()
        );
    }

    #[test]
    fn rejects_duplicate_tuple_and_invalid_security_fields() {
        let duplicate = artifact("vosk", "windows", "x86_64", "cpu");
        assert!(
            RuntimeArtifactCatalog::parse(&catalog_json(&format!("{duplicate},{duplicate}")))
                .unwrap_err()
                .contains("duplicate")
        );

        for (field, replacement) in [
            ("https://", "http://"),
            (&"a".repeat(64), &"A".repeat(64)),
            ("bin/runtime", "../runtime"),
            ("bin/runtime", "bin/CON"),
        ] {
            let invalid = artifact("vosk", "windows", "x86_64", "cpu").replace(field, replacement);
            assert!(RuntimeArtifactCatalog::parse(&catalog_json(&invalid)).is_err());
        }

        let missing_sizes = artifact("vosk", "windows", "x86_64", "cpu")
            .replace(",\"size_bytes\":123,\"unpacked_size_bytes\":456", "");
        assert!(RuntimeArtifactCatalog::parse(&catalog_json(&missing_sizes)).is_err());
        let malformed_url = artifact("vosk", "windows", "x86_64", "cpu")
            .replace("https://github.com", "https://:443");
        assert!(RuntimeArtifactCatalog::parse(&catalog_json(&malformed_url)).is_err());
        let reserved_url = artifact("vosk", "windows", "x86_64", "cpu")
            .replace("github.com", "artifacts.example.invalid");
        assert!(RuntimeArtifactCatalog::parse(&catalog_json(&reserved_url)).is_err());
        let loopback_url =
            artifact("vosk", "windows", "x86_64", "cpu").replace("github.com", "127.1.2.3");
        assert!(RuntimeArtifactCatalog::parse(&catalog_json(&loopback_url)).is_err());
        let query_url =
            artifact("vosk", "windows", "x86_64", "cpu").replace("vosk.zip", "vosk.zip?mutable=1");
        assert!(RuntimeArtifactCatalog::parse(&catalog_json(&query_url)).is_err());
        for oversized in [
            artifact("vosk", "windows", "x86_64", "cpu").replace(
                "\"size_bytes\":123",
                &format!("\"size_bytes\":{}", MAX_ARCHIVE_BYTES + 1),
            ),
            artifact("vosk", "windows", "x86_64", "cpu").replace(
                "\"unpacked_size_bytes\":456",
                &format!("\"unpacked_size_bytes\":{}", MAX_UNPACKED_BYTES + 1),
            ),
        ] {
            assert!(RuntimeArtifactCatalog::parse(&catalog_json(&oversized)).is_err());
        }
        assert!(validate_archive_entry_count(100_001).is_err());
    }

    #[test]
    fn rejects_unknown_ids_platforms_and_unsupported_gpu_packs() {
        for invalid in [
            artifact("unknown", "windows", "x86_64", "cpu"),
            artifact("vosk", "freebsd", "x86_64", "cpu"),
            artifact("vosk", "windows", "riscv64", "cpu"),
            artifact("vosk", "windows", "x86_64", "gpu"),
        ] {
            assert!(RuntimeArtifactCatalog::parse(&catalog_json(&invalid)).is_err());
        }
    }

    #[test]
    fn verified_archive_stages_entrypoint_and_cleans_download() {
        let manifest = manifest();
        let bytes = archive(&[
            ("bin/whisper-cli", b"runtime"),
            ("lib/library", b"lib"),
            ("runtime-manifest.json", &manifest),
        ]);
        let artifact = test_artifact(&bytes, bytes.len() as u64, 10 + manifest.len() as u64);
        let target = temp_target("success").join("whisper_cpp");

        let staged = stage_from_reader(&artifact, &target, Cursor::new(bytes)).unwrap();

        assert_eq!(fs::read(&staged.entrypoint).unwrap(), b"runtime");
        assert_eq!(
            transaction_files(target.parent().unwrap(), "whisper_cpp"),
            std::slice::from_ref(&staged.root)
        );
        fs::remove_dir_all(target.parent().unwrap()).unwrap();
    }

    #[test]
    fn overall_download_deadline_aborts_and_cleans_partial_state() {
        let bytes = archive(&[("bin/whisper-cli", b"runtime")]);
        let artifact = test_artifact(&bytes, bytes.len() as u64, 7);
        let target = temp_target("deadline").join("whisper_cpp");

        let error =
            stage_from_reader_until(&artifact, &target, Cursor::new(bytes), Some(Instant::now()))
                .unwrap_err();

        assert!(error.contains("deadline"));
        assert!(transaction_files(target.parent().unwrap(), "whisper_cpp").is_empty());
        fs::remove_dir_all(target.parent().unwrap()).unwrap();
    }

    #[test]
    fn runtime_staging_reports_progress_and_cancellation_cleans_transactions() {
        let bytes = archive(&[("bin/whisper-cli", b"runtime")]);
        let artifact = test_artifact(&bytes, bytes.len() as u64, 7);
        let target = temp_target("runtime-cancel").join("whisper_cpp");
        let mut updates = Vec::new();

        let error = stage_from_reader_until_with_progress(
            &artifact,
            &target,
            Cursor::new(&bytes),
            None,
            |progress| {
                updates.push(progress);
                if progress.downloaded_bytes > 0 {
                    DownloadControl::Cancel
                } else {
                    DownloadControl::Continue
                }
            },
        )
        .unwrap_err();

        assert!(error.contains("cancelled"));
        assert_eq!(
            updates,
            [
                DownloadProgress {
                    downloaded_bytes: 0,
                    total_bytes: bytes.len() as u64,
                },
                DownloadProgress {
                    downloaded_bytes: bytes.len() as u64,
                    total_bytes: bytes.len() as u64,
                },
            ]
        );
        assert!(transaction_files(target.parent().unwrap(), "whisper_cpp").is_empty());
        fs::remove_dir_all(target.parent().unwrap()).unwrap();
    }

    #[test]
    fn bounded_extraction_enforces_actual_entry_and_total_bytes() {
        let mut total = 0;
        let mut output = Vec::new();
        let oversized_entry = copy_archive_entry_bounded(
            Cursor::new(b"five!"),
            &mut output,
            "oversized",
            4,
            &mut total,
            10,
        )
        .unwrap_err();
        assert!(oversized_entry.contains("declared or allowed"));
        assert!(output.is_empty());

        let mut total = 0;
        copy_archive_entry_bounded(Cursor::new(b"abc"), Vec::new(), "first", 3, &mut total, 4)
            .unwrap();
        let total_error =
            copy_archive_entry_bounded(Cursor::new(b"de"), Vec::new(), "second", 2, &mut total, 4)
                .unwrap_err();
        assert!(total_error.contains("declared or allowed"));

        let short =
            copy_archive_entry_bounded(Cursor::new(b"abc"), Vec::new(), "short", 4, &mut 0, 10)
                .unwrap_err();
        assert!(short.contains("size mismatch"));
    }

    #[test]
    fn checksum_and_expected_size_mismatches_clean_partial_files() {
        let bytes = archive(&[("bin/whisper-cli", b"runtime")]);
        for (name, artifact) in [
            (
                "checksum",
                RuntimeArtifact {
                    sha256: "0".repeat(64),
                    ..test_artifact(&bytes, bytes.len() as u64, 7)
                },
            ),
            ("size", test_artifact(&bytes, bytes.len() as u64 + 1, 7)),
        ] {
            let target = temp_target(name).join("whisper_cpp");
            assert!(stage_from_reader(&artifact, &target, Cursor::new(&bytes)).is_err());
            assert!(transaction_files(target.parent().unwrap(), "whisper_cpp").is_empty());
            fs::remove_dir_all(target.parent().unwrap()).unwrap();
        }
    }

    #[test]
    fn unsafe_archive_paths_and_links_are_rejected_and_cleaned() {
        let traversal = archive(&[("../escape", b"bad"), ("bin/whisper-cli", b"runtime")]);
        let ads = archive(&[("data/file:ads", b"bad"), ("bin/whisper-cli", b"runtime")]);
        let reserved = archive(&[("data/CON", b"bad"), ("bin/whisper-cli", b"runtime")]);
        let trailing = archive(&[("data/trailing.", b"bad"), ("bin/whisper-cli", b"runtime")]);
        let control = archive(&[("data/bad\nname", b"bad"), ("bin/whisper-cli", b"runtime")]);
        let mut link_writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        link_writer
            .add_symlink("bin/whisper-cli", "../target", SimpleFileOptions::default())
            .unwrap();
        let link = link_writer.finish().unwrap().into_inner();

        for (name, bytes) in [
            ("traversal", traversal),
            ("ads", ads),
            ("reserved", reserved),
            ("trailing", trailing),
            ("control", control),
            ("link", link),
        ] {
            let artifact = test_artifact(&bytes, bytes.len() as u64, 7);
            let target = temp_target(name).join("whisper_cpp");
            assert!(stage_from_reader(&artifact, &target, Cursor::new(bytes)).is_err());
            assert!(transaction_files(target.parent().unwrap(), "whisper_cpp").is_empty());
            fs::remove_dir_all(target.parent().unwrap()).unwrap();
        }
    }

    #[test]
    fn production_artifacts_reject_raw_python_virtual_environments() {
        let bytes = archive(&[
            ("pyvenv.cfg", b"home = /build/python"),
            ("bin/whisper-cli", b"runtime"),
        ]);
        let artifact = test_artifact(&bytes, bytes.len() as u64, 34);
        let target = temp_target("raw-venv").join("whisper_cpp");

        let error = stage_from_reader(&artifact, &target, Cursor::new(bytes)).unwrap_err();

        assert!(error.contains("development-only"));
        assert!(transaction_files(target.parent().unwrap(), "whisper_cpp").is_empty());
        fs::remove_dir_all(target.parent().unwrap()).unwrap();
    }

    #[test]
    fn manifest_identity_must_exactly_match_the_trusted_catalog() {
        let valid = String::from_utf8(manifest()).unwrap();
        let variants = [
            ("missing", None),
            (
                "runtime",
                Some(valid.replace("whisper_cpp", "faster_whisper")),
            ),
            ("version", Some(valid.replace("1.2.3", "9.9.9"))),
            (
                "platform",
                Some(valid.replace(
                    &format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
                    "different-platform",
                )),
            ),
            ("device", Some(valid.replace("\"cpu\"", "\"gpu\""))),
            (
                "entrypoint",
                Some(valid.replace("bin/whisper-cli", "bin/other")),
            ),
        ];

        for (name, manifest) in variants {
            let mut entries = vec![("bin/whisper-cli", b"runtime".as_slice())];
            let manifest_bytes = manifest.as_ref().map(String::as_bytes);
            if let Some(contents) = manifest_bytes {
                entries.push(("runtime-manifest.json", contents));
            }
            let bytes = archive(&entries);
            let unpacked = entries
                .iter()
                .map(|(_, contents)| contents.len() as u64)
                .sum();
            let artifact = test_artifact(&bytes, bytes.len() as u64, unpacked);
            let target = temp_target(name).join("whisper_cpp");

            assert!(stage_from_reader(&artifact, &target, Cursor::new(bytes)).is_err());
            assert!(transaction_files(target.parent().unwrap(), "whisper_cpp").is_empty());
            fs::remove_dir_all(target.parent().unwrap()).unwrap();
        }
    }
}
