use std::collections::HashSet;
use std::fs;
use std::io::{Read, Seek, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;
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
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StagedRuntimeArtifact {
    pub(crate) root: PathBuf,
    pub(crate) entrypoint: PathBuf,
}

#[derive(Deserialize)]
struct RuntimeArtifactManifest {
    manifest_version: u32,
    runtime_id: String,
    version: String,
    platform: String,
    device: RuntimeDevicePack,
    entrypoint: PathBuf,
    portable: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RuntimeArtifactCatalog {
    schema_version: u32,
    catalog_version: String,
    artifacts: Vec<RuntimeArtifact>,
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

    fn validate(&self) -> Result<(), String> {
        if self.schema_version != 1 {
            return Err(format!(
                "unsupported runtime artifact catalog schema {}",
                self.schema_version
            ));
        }
        if self.catalog_version.trim().is_empty() {
            return Err("runtime artifact catalog version is empty".to_owned());
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
            || runtime_catalog::backend_spec_for_runtime_id(&self.runtime_id).is_none()
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
        let backend = runtime_catalog::backend_spec_for_runtime_id(&self.runtime_id)
            .expect("validated runtime id has a backend");
        if self.device == RuntimeDevicePack::Gpu && !backend.device_support.supports_gpu() {
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
        Ok(())
    }
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

pub(crate) fn download_and_stage(
    artifact: &RuntimeArtifact,
    target_root: &Path,
) -> Result<StagedRuntimeArtifact, String> {
    let deadline = Instant::now() + MAX_DOWNLOAD_DURATION;
    let agent = ureq::AgentBuilder::new()
        .redirects(0)
        .timeout_connect(std::time::Duration::from_secs(15))
        .timeout_read(std::time::Duration::from_secs(60))
        .timeout_write(std::time::Duration::from_secs(60))
        .build();
    let response = agent
        .get(&artifact.url)
        .call()
        .map_err(|err| format!("runtime artifact request failed: {err}"))?;
    if response
        .header("content-encoding")
        .is_some_and(|encoding| !encoding.eq_ignore_ascii_case("identity"))
    {
        return Err("runtime artifact response must not use content encoding".to_owned());
    }
    if let Some(length) = response.header("content-length") {
        let length = length
            .parse::<u64>()
            .map_err(|_| "runtime artifact Content-Length is invalid".to_owned())?;
        if length != artifact.size_bytes {
            return Err(format!(
                "runtime artifact Content-Length mismatch: expected {}, received {length}",
                artifact.size_bytes
            ));
        }
    }
    stage_from_reader_until(
        artifact,
        target_root,
        response.into_reader(),
        Some(deadline),
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

fn stage_from_reader_until(
    artifact: &RuntimeArtifact,
    target_root: &Path,
    mut reader: impl Read,
    deadline: Option<Instant>,
) -> Result<StagedRuntimeArtifact, String> {
    let parent = target_root
        .parent()
        .ok_or_else(|| format!("runtime target {} has no parent", target_root.display()))?;
    fs::create_dir_all(parent)
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
    use std::io::Cursor;
    use zip::write::SimpleFileOptions;

    fn catalog_json(artifacts: &str) -> String {
        format!(
            r#"{{"schema_version":1,"catalog_version":"2026.07.28","artifacts":[{artifacts}]}}"#
        )
    }

    fn artifact(runtime_id: &str, os: &str, arch: &str, device: &str) -> String {
        format!(
            r#"{{"runtime_id":"{runtime_id}","version":"1.2.3","os":"{os}","arch":"{arch}","device":"{device}","url":"https://github.com/scribe-runtime-tests/releases/download/1.2.3/{runtime_id}.zip","sha256":"{}","size_bytes":123,"unpacked_size_bytes":456,"entrypoint":"bin/runtime"}}"#,
            "a".repeat(64)
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
        }
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
