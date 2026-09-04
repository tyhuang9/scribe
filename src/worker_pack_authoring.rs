//! Build-time worker-pack inventory and signing.
//!
//! This module is compiled only into the private pack-authoring executable.
//! Production signing accepts an external PKCS#8 key only when its public key
//! is already present in the desktop's reviewed production trust root.

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use ring::signature::{Ed25519KeyPair, KeyPair};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::manifest::{
    APP_PROTOCOL_VERSION, Compatibility, EMBEDDED_MINIMUM_SECURITY_EPOCH, MANIFEST_NAME,
    MAX_FILE_BYTES, MAX_FILES, MAX_MANIFEST_BYTES, PACK_SCHEMA_VERSION, PackBackend, PackManifest,
    PackVerifier, PayloadEntry, ProductionTrustRoot, RUNTIME_ABI_VERSION, SIGNATURE_NAME,
    StoreComponent, TrustRoot, compute_pack_digest, hash_exact_length, is_link_or_reparse,
    open_regular_no_follow, reject_hardlink, reject_named_streams, validate_build_identity,
    validate_identifier, validate_inventory, validate_relative_path, validate_root,
};

pub(crate) const FIXTURE_KEY_ID: &str = "fixture-ed25519-v1";
const FIXTURE_SEED: [u8; 32] = [7; 32];
const MAX_PRIVATE_KEY_BYTES: u64 = 16 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AuthoringBackend {
    Cuda,
    Vulkan,
    Metal,
}

impl AuthoringBackend {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "cuda" => Some(Self::Cuda),
            "vulkan" => Some(Self::Vulkan),
            "metal" => Some(Self::Metal),
            _ => None,
        }
    }

    fn manifest_backend(self) -> PackBackend {
        match self {
            Self::Cuda => PackBackend::Cuda,
            Self::Vulkan => PackBackend::Vulkan,
            Self::Metal => PackBackend::Metal,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Cuda => "cuda",
            Self::Vulkan => "vulkan",
            Self::Metal => "metal",
        }
    }
}

pub(crate) const AUTHOR_TARGET_CONTRACT: &str = "allowed authoring targets are cuda or vulkan on windows/x86_64 or linux/x86_64, or metal on macos/aarch64 or macos/x86_64; backend, OS, and architecture values are lowercase and case-sensitive";

#[derive(Clone, Debug)]
pub(crate) enum SigningMode {
    Fixture,
    Production {
        key_id: String,
        private_key_path: PathBuf,
    },
}

#[derive(Clone, Debug)]
pub(crate) struct AuthorRequest {
    pub(crate) pack_root: PathBuf,
    pub(crate) pack_id: String,
    pub(crate) pack_version: String,
    pub(crate) security_epoch: u64,
    pub(crate) backend: AuthoringBackend,
    pub(crate) provider: String,
    pub(crate) target_os: String,
    pub(crate) target_arch: String,
    pub(crate) worker_path: String,
    pub(crate) signing: SigningMode,
}

#[derive(Clone, Debug)]
pub(crate) struct PrepareRequest {
    pub(crate) pack_root: PathBuf,
    pub(crate) pack_id: String,
    pub(crate) pack_version: String,
    pub(crate) security_epoch: u64,
    pub(crate) backend: AuthoringBackend,
    pub(crate) provider: String,
    pub(crate) target_os: String,
    pub(crate) target_arch: String,
    pub(crate) worker_path: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct PreparedPack {
    pub(crate) schema_version: u16,
    pub(crate) pack_id: String,
    pub(crate) pack_version: String,
    pub(crate) pack_digest: String,
    pub(crate) security_epoch: u64,
    pub(crate) backend: String,
    pub(crate) provider: String,
    pub(crate) target_os: String,
    pub(crate) target_arch: String,
    pub(crate) manifest_sha256: String,
    pub(crate) payload_files: usize,
    pub(crate) installed_payload_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct AuthoredPack {
    pub(crate) pack_id: String,
    pub(crate) pack_version: String,
    pub(crate) pack_digest: String,
    pub(crate) key_id: String,
    pub(crate) payload_files: usize,
    pub(crate) installed_payload_bytes: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DetachedSignature {
    schema_version: u16,
    key_id: String,
    signature_hex: String,
}

struct ExactTrustRoot {
    key_id: String,
    public_key: Vec<u8>,
}

impl TrustRoot for ExactTrustRoot {
    fn public_key(&self, key_id: &str) -> Option<&[u8]> {
        (key_id == self.key_id).then_some(self.public_key.as_slice())
    }
}

pub(crate) fn check_production_signing_key(key_id: &str, private_key_path: &Path) -> Result<()> {
    let _ = production_key_pair(key_id, private_key_path)?;
    Ok(())
}

pub(crate) fn author_pack(request: &AuthorRequest) -> Result<AuthoredPack> {
    // Preserve the legacy one-shot command for fixture and non-Windows callers,
    // but never materialize an unsigned production manifest until the key has
    // already been matched to the embedded trust root.
    if let SigningMode::Production {
        key_id,
        private_key_path,
    } = &request.signing
    {
        check_production_signing_key(key_id, private_key_path)?;
    }
    let prepared_request = PrepareRequest {
        pack_root: request.pack_root.clone(),
        pack_id: request.pack_id.clone(),
        pack_version: request.pack_version.clone(),
        security_epoch: request.security_epoch,
        backend: request.backend,
        provider: request.provider.clone(),
        target_os: request.target_os.clone(),
        target_arch: request.target_arch.clone(),
        worker_path: request.worker_path.clone(),
    };
    let prepared = prepare_pack(&prepared_request)?;
    sign_prepared_pack(
        &request.pack_root,
        &request.signing,
        &prepared.manifest_sha256,
        &prepared.pack_digest,
    )
    .inspect_err(|_| {
        let _ = fs::remove_file(request.pack_root.join(MANIFEST_NAME));
    })
}

pub(crate) fn prepare_pack(request: &PrepareRequest) -> Result<PreparedPack> {
    let manifest = manifest_for_request(request)?;
    let manifest_bytes = serde_json::to_vec(&manifest)?;
    write_new_envelope(&request.pack_root.join(MANIFEST_NAME), &manifest_bytes)
        .context("could not create canonical prepared worker-pack manifest")?;
    inspect_prepared_pack(&request.pack_root).inspect_err(|_| {
        let _ = fs::remove_file(request.pack_root.join(MANIFEST_NAME));
    })
}

pub(crate) fn inspect_prepared_pack(root: &Path) -> Result<PreparedPack> {
    validate_root(root)?;
    if root.join(SIGNATURE_NAME).exists() {
        bail!("prepared pack must not contain a signature envelope");
    }
    let (manifest, manifest_bytes) = manifest_from_root(root, "prepared")?;
    validate_prepared_manifest(&manifest)?;
    let payload = inventory_payload(root, true)?;
    if payload != manifest.payload {
        bail!("prepared pack payload does not match its canonical manifest");
    }
    if !payload
        .iter()
        .any(|entry| entry.path == manifest.worker_path)
    {
        bail!("declared worker path is absent from the payload inventory");
    }
    let installed_payload_bytes = installed_payload_bytes(&payload)?;
    Ok(PreparedPack {
        schema_version: 1,
        pack_id: manifest.pack_id.as_str().to_owned(),
        pack_version: manifest.pack_version.as_str().to_owned(),
        pack_digest: manifest.pack_digest,
        security_epoch: manifest.security_epoch,
        backend: backend_from_manifest(manifest.backend).as_str().to_owned(),
        provider: manifest.provider,
        target_os: manifest.target_os,
        target_arch: manifest.target_arch,
        manifest_sha256: encode_hex(&Sha256::digest(&manifest_bytes)),
        payload_files: payload.len(),
        installed_payload_bytes,
    })
}

pub(crate) fn sign_prepared_pack(
    root: &Path,
    signing: &SigningMode,
    expected_manifest_sha256: &str,
    expected_pack_digest: &str,
) -> Result<AuthoredPack> {
    if !is_canonical_sha256(expected_manifest_sha256) || !is_canonical_sha256(expected_pack_digest)
    {
        bail!("caller-approved prepared-pack digests must be canonical SHA-256 values");
    }
    let prepared = inspect_prepared_pack(root)?;
    if prepared.manifest_sha256 != expected_manifest_sha256
        || prepared.pack_digest != expected_pack_digest
    {
        bail!("prepared pack does not match the caller-approved manifest and pack digests");
    }
    let (manifest, manifest_bytes) = manifest_from_root(root, "prepared")?;
    if encode_hex(&Sha256::digest(&manifest_bytes)) != expected_manifest_sha256
        || manifest.pack_digest != expected_pack_digest
    {
        bail!("prepared pack changed after validation and before signing");
    }
    let (key_id, key_pair) = signing_key_pair(signing)?;
    let signature = DetachedSignature {
        schema_version: PACK_SCHEMA_VERSION,
        key_id: key_id.clone(),
        signature_hex: encode_hex(key_pair.sign(&manifest_bytes).as_ref()),
    };
    let signature_path = root.join(SIGNATURE_NAME);
    write_new_envelope(&signature_path, &serde_json::to_vec(&signature)?)
        .context("could not create canonical worker-pack signature")?;

    let trust = ExactTrustRoot {
        key_id: key_id.clone(),
        public_key: key_pair.public_key().as_ref().to_vec(),
    };
    let allowed_backends = [manifest.backend];
    let verifier = PackVerifier::new(
        &trust,
        Compatibility {
            app_build: crate::worker_identity::DESKTOP_BUILD_ID,
            worker_build: crate::worker_identity::INFERENCE_WORKER_BUILD_ID,
            target_os: &manifest.target_os,
            target_arch: &manifest.target_arch,
            allowed_backends: &allowed_backends,
        },
    );
    let verified = verifier.verify(root).inspect_err(|_| {
        let _ = fs::remove_file(&signature_path);
    })?;
    if verified.pack_digest != prepared.pack_digest {
        let _ = fs::remove_file(&signature_path);
        bail!("signed pack digest changed during self-verification");
    }
    Ok(AuthoredPack {
        pack_id: prepared.pack_id,
        pack_version: prepared.pack_version,
        pack_digest: prepared.pack_digest,
        key_id,
        payload_files: prepared.payload_files,
        installed_payload_bytes: prepared.installed_payload_bytes,
    })
}

fn is_canonical_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(crate) fn verify_fixture_pack(root: &Path) -> Result<AuthoredPack> {
    let (manifest, _) = manifest_from_root(root, "fixture")?;
    let backend = match manifest.backend {
        PackBackend::Cuda => AuthoringBackend::Cuda,
        PackBackend::Vulkan => AuthoringBackend::Vulkan,
        PackBackend::Metal => AuthoringBackend::Metal,
    };
    validate_authoring_target(backend, &manifest.target_os, &manifest.target_arch)?;
    let key_pair = Ed25519KeyPair::from_seed_unchecked(&FIXTURE_SEED)
        .map_err(|_| anyhow!("fixture signing key is invalid"))?;
    let trust = ExactTrustRoot {
        key_id: FIXTURE_KEY_ID.to_owned(),
        public_key: key_pair.public_key().as_ref().to_vec(),
    };
    let allowed_backends = [manifest.backend];
    let verifier = PackVerifier::new(
        &trust,
        Compatibility {
            app_build: crate::worker_identity::DESKTOP_BUILD_ID,
            worker_build: crate::worker_identity::INFERENCE_WORKER_BUILD_ID,
            target_os: &manifest.target_os,
            target_arch: &manifest.target_arch,
            allowed_backends: &allowed_backends,
        },
    );
    let verified = verifier.verify(root)?;
    let payload = manifest.payload;
    let installed_payload_bytes = payload.iter().try_fold(0_u64, |total, entry| {
        total
            .checked_add(entry.size_bytes)
            .ok_or_else(|| anyhow!("payload byte count overflowed"))
    })?;
    Ok(AuthoredPack {
        pack_id: verified.pack_id.as_str().to_owned(),
        pack_version: verified.pack_version.as_str().to_owned(),
        pack_digest: verified.pack_digest,
        key_id: FIXTURE_KEY_ID.to_owned(),
        payload_files: payload.len(),
        installed_payload_bytes,
    })
}

pub(crate) fn validate_authoring_target(
    backend: AuthoringBackend,
    target_os: &str,
    target_arch: &str,
) -> Result<()> {
    let accepted = matches!(
        (backend, target_os, target_arch),
        (
            AuthoringBackend::Cuda | AuthoringBackend::Vulkan,
            "windows" | "linux",
            "x86_64"
        ) | (AuthoringBackend::Metal, "macos", "aarch64" | "x86_64")
    );
    if !accepted {
        bail!("invalid backend/target combination; {AUTHOR_TARGET_CONTRACT}");
    }
    Ok(())
}

fn manifest_for_request(request: &PrepareRequest) -> Result<PackManifest> {
    validate_authoring_target(request.backend, &request.target_os, &request.target_arch)?;
    if request.security_epoch < EMBEDDED_MINIMUM_SECURITY_EPOCH {
        bail!(
            "security epoch {} is below the embedded minimum {}",
            request.security_epoch,
            EMBEDDED_MINIMUM_SECURITY_EPOCH
        );
    }
    let pack_id = StoreComponent::new(request.pack_id.clone())
        .ok_or_else(|| anyhow!("pack ID is not a canonical store component"))?;
    let pack_version = StoreComponent::new(request.pack_version.clone())
        .ok_or_else(|| anyhow!("pack version is not a canonical store component"))?;
    validate_identifier(&request.provider, "provider")?;
    validate_relative_path(&request.worker_path)?;
    if !request.worker_path.is_ascii() {
        bail!("prepared pack worker path must be ASCII");
    }
    validate_build_identity(crate::worker_identity::DESKTOP_BUILD_ID, "app build")?;
    validate_build_identity(
        crate::worker_identity::INFERENCE_WORKER_BUILD_ID,
        "worker build",
    )?;
    let payload = inventory_payload(&request.pack_root, false)?;
    if !payload
        .iter()
        .any(|entry| entry.path == request.worker_path)
    {
        bail!("declared worker path is absent from the payload inventory");
    }
    let mut manifest = PackManifest {
        schema_version: PACK_SCHEMA_VERSION,
        pack_id,
        pack_version,
        pack_digest: "0".repeat(64),
        security_epoch: request.security_epoch,
        app_protocol_version: APP_PROTOCOL_VERSION,
        worker_protocol_version: APP_PROTOCOL_VERSION,
        runtime_abi_version: RUNTIME_ABI_VERSION,
        app_build: crate::worker_identity::DESKTOP_BUILD_ID.to_owned(),
        worker_build: crate::worker_identity::INFERENCE_WORKER_BUILD_ID.to_owned(),
        backend: request.backend.manifest_backend(),
        provider: request.provider.clone(),
        target_os: request.target_os.clone(),
        target_arch: request.target_arch.clone(),
        worker_path: request.worker_path.clone(),
        payload,
    };
    manifest.pack_digest = compute_pack_digest(&manifest)?;
    Ok(manifest)
}

fn validate_prepared_manifest(manifest: &PackManifest) -> Result<()> {
    let backend = backend_from_manifest(manifest.backend);
    validate_authoring_target(backend, &manifest.target_os, &manifest.target_arch)?;
    if manifest.schema_version != PACK_SCHEMA_VERSION
        || manifest.app_protocol_version != APP_PROTOCOL_VERSION
        || manifest.worker_protocol_version != APP_PROTOCOL_VERSION
        || manifest.runtime_abi_version != RUNTIME_ABI_VERSION
    {
        bail!("prepared pack has an incompatible schema, protocol, or runtime ABI");
    }
    if manifest.security_epoch < EMBEDDED_MINIMUM_SECURITY_EPOCH {
        bail!("prepared pack security epoch is below the embedded minimum");
    }
    if !manifest.pack_id.is_canonical() || !manifest.pack_version.is_canonical() {
        bail!("prepared pack has a noncanonical store identity");
    }
    validate_identifier(&manifest.provider, "provider")?;
    validate_relative_path(&manifest.worker_path)?;
    validate_build_identity(&manifest.app_build, "app build")?;
    validate_build_identity(&manifest.worker_build, "worker build")?;
    if manifest.app_build != crate::worker_identity::DESKTOP_BUILD_ID
        || manifest.worker_build != crate::worker_identity::INFERENCE_WORKER_BUILD_ID
    {
        bail!("prepared pack build identity does not match this signing tool");
    }
    validate_inventory(&manifest.payload)?;
    if !manifest.worker_path.is_ascii()
        || manifest.payload.iter().any(|entry| !entry.path.is_ascii())
    {
        bail!("prepared pack paths must be ASCII for Windows ordinal case safety");
    }
    if compute_pack_digest(manifest)? != manifest.pack_digest {
        bail!("prepared pack digest does not match its canonical manifest");
    }
    Ok(())
}

fn backend_from_manifest(backend: PackBackend) -> AuthoringBackend {
    match backend {
        PackBackend::Cuda => AuthoringBackend::Cuda,
        PackBackend::Vulkan => AuthoringBackend::Vulkan,
        PackBackend::Metal => AuthoringBackend::Metal,
    }
}

fn signing_key_pair(signing: &SigningMode) -> Result<(String, Ed25519KeyPair)> {
    match signing {
        SigningMode::Fixture => Ok((
            FIXTURE_KEY_ID.to_owned(),
            Ed25519KeyPair::from_seed_unchecked(&FIXTURE_SEED)
                .map_err(|_| anyhow!("fixture signing key is invalid"))?,
        )),
        SigningMode::Production {
            key_id,
            private_key_path,
        } => Ok((
            key_id.clone(),
            production_key_pair(key_id, private_key_path)?,
        )),
    }
}

fn installed_payload_bytes(payload: &[PayloadEntry]) -> Result<u64> {
    payload.iter().try_fold(0_u64, |total, entry| {
        total
            .checked_add(entry.size_bytes)
            .ok_or_else(|| anyhow!("payload byte count overflowed"))
    })
}

fn production_key_pair(key_id: &str, private_key_path: &Path) -> Result<Ed25519KeyPair> {
    validate_identifier(key_id, "signature key ID")?;
    let embedded = TrustRoot::public_key(&ProductionTrustRoot, key_id).ok_or_else(|| {
        anyhow!(
            "production signing key ID {key_id:?} has no separately reviewed public key embedded in this build"
        )
    })?;
    let private_key = read_bounded_private_key(private_key_path)?;
    let key_pair = Ed25519KeyPair::from_pkcs8(&private_key)
        .map_err(|_| anyhow!("production signing key is not Ed25519 PKCS#8 v2 DER"))?;
    if key_pair.public_key().as_ref() != embedded {
        bail!(
            "external production signing key does not match the embedded public key for {key_id:?}"
        );
    }
    Ok(key_pair)
}

fn read_bounded_private_key(path: &Path) -> Result<Vec<u8>> {
    let file = open_regular_no_follow(path).context("could not open production signing key")?;
    let metadata = file
        .metadata()
        .context("could not inspect production signing key")?;
    reject_hardlink(&file, &metadata, path)?;
    reject_named_streams(path)?;
    if metadata.len() == 0 || metadata.len() > MAX_PRIVATE_KEY_BYTES {
        bail!("production signing key size is outside the accepted bound");
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_PRIVATE_KEY_BYTES + 1)
        .read_to_end(&mut bytes)
        .context("could not read production signing key")?;
    if bytes.len() as u64 != metadata.len() {
        bail!("production signing key changed while it was read");
    }
    Ok(bytes)
}

fn inventory_payload(root: &Path, prepared_manifest_allowed: bool) -> Result<Vec<PayloadEntry>> {
    validate_root(root)?;
    if (!prepared_manifest_allowed && root.join(MANIFEST_NAME).exists())
        || root.join(SIGNATURE_NAME).exists()
    {
        bail!("pack root already contains a manifest or signature envelope");
    }
    let mut pending = vec![(root.to_path_buf(), 0_usize)];
    let mut payload = Vec::new();
    let mut casefolded = BTreeSet::new();
    while let Some((directory, depth)) = pending.pop() {
        for entry in fs::read_dir(&directory).context("could not enumerate pack payload")? {
            let entry = entry.context("could not enumerate pack payload entry")?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).with_context(|| {
                format!("could not inspect pack payload entry {}", path.display())
            })?;
            if is_link_or_reparse(&metadata) {
                bail!(
                    "pack payload contains a link or reparse point: {}",
                    path.display()
                );
            }
            let relative = path
                .strip_prefix(root)
                .map_err(|_| anyhow!("pack payload escaped its root"))?
                .components()
                .map(|component| {
                    component
                        .as_os_str()
                        .to_str()
                        .ok_or_else(|| anyhow!("pack payload path is not UTF-8"))
                })
                .collect::<Result<Vec<_>>>()?
                .join("/");
            if !relative.is_ascii() {
                bail!("prepared pack payload paths must be ASCII");
            }
            let is_prepared_manifest =
                prepared_manifest_allowed && directory == root && relative == MANIFEST_NAME;
            if !is_prepared_manifest {
                validate_relative_path(&relative)?;
            }
            if !casefolded.insert(relative.to_ascii_lowercase()) {
                bail!("pack payload contains a case-insensitive path collision");
            }
            if metadata.is_dir() {
                if depth >= 12 {
                    bail!("pack payload exceeds the maximum directory depth");
                }
                reject_named_streams(&path)?;
                pending.push((path, depth + 1));
                continue;
            }
            if !metadata.is_file() {
                bail!(
                    "pack payload contains a nonregular entry: {}",
                    path.display()
                );
            }
            if metadata.len() > MAX_FILE_BYTES {
                bail!("pack payload file exceeds the maximum size: {relative}");
            }
            let mut file = open_regular_no_follow(&path)?;
            let opened_metadata = file.metadata()?;
            reject_hardlink(&file, &opened_metadata, &path)?;
            reject_named_streams(&path)?;
            if opened_metadata.len() != metadata.len() {
                bail!("pack payload changed while it was inventoried: {relative}");
            }
            if is_prepared_manifest {
                continue;
            }
            if payload.len() >= MAX_FILES {
                bail!("pack payload exceeds the maximum file count");
            }
            payload.push(PayloadEntry {
                path: relative.clone(),
                size_bytes: opened_metadata.len(),
                sha256: hash_exact_length(&mut file, opened_metadata.len(), &relative)?,
            });
        }
    }
    payload.sort_by(|left, right| left.path.cmp(&right.path));
    validate_inventory(&payload)?;
    Ok(payload)
}

fn manifest_from_root(root: &Path, label: &str) -> Result<(PackManifest, Vec<u8>)> {
    let path = root.join(MANIFEST_NAME);
    let file = open_regular_no_follow(&path)
        .with_context(|| format!("could not open {label} manifest"))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("could not inspect {label} manifest"))?;
    reject_hardlink(&file, &metadata, &path)?;
    reject_named_streams(&path)?;
    if metadata.len() == 0 || metadata.len() > MAX_MANIFEST_BYTES {
        bail!("{label} manifest size is outside the accepted bound");
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("could not read {label} manifest"))?;
    if bytes.len() as u64 != metadata.len() {
        bail!("{label} manifest changed while it was read");
    }
    let manifest: PackManifest = serde_json::from_slice(&bytes)?;
    if serde_json::to_vec(&manifest)? != bytes {
        bail!("{label} manifest is not canonical JSON");
    }
    Ok((manifest, bytes))
}

fn write_new_envelope(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_root(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "scribe-authoring-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(root.join("bin")).unwrap();
        fs::write(root.join("bin/scribe-inference-worker.exe"), b"worker").unwrap();
        root
    }

    fn fixture_request(root: &Path) -> AuthorRequest {
        AuthorRequest {
            pack_root: root.to_path_buf(),
            pack_id: "scribe-vulkan-windows-x64".to_owned(),
            pack_version: "0.1.0-fixture".to_owned(),
            security_epoch: 1,
            backend: AuthoringBackend::Vulkan,
            provider: "transcribe-cpp-ggml-vulkan".to_owned(),
            target_os: "windows".to_owned(),
            target_arch: "x86_64".to_owned(),
            worker_path: "bin/scribe-inference-worker.exe".to_owned(),
            signing: SigningMode::Fixture,
        }
    }

    #[test]
    fn fixture_authoring_is_deterministic_and_self_verifying() {
        let first = temp_root("deterministic-a");
        let second = temp_root("deterministic-b");
        let authored_first = author_pack(&fixture_request(&first)).unwrap();
        let authored_second = author_pack(&fixture_request(&second)).unwrap();
        assert_eq!(authored_first, authored_second);
        assert_eq!(
            fs::read(first.join(MANIFEST_NAME)).unwrap(),
            fs::read(second.join(MANIFEST_NAME)).unwrap()
        );
        assert_eq!(
            fs::read(first.join(SIGNATURE_NAME)).unwrap(),
            fs::read(second.join(SIGNATURE_NAME)).unwrap()
        );
        assert_eq!(verify_fixture_pack(&first).unwrap(), authored_first);
        fs::remove_dir_all(first).unwrap();
        fs::remove_dir_all(second).unwrap();
    }

    #[test]
    fn prepared_pack_binds_canonical_manifest_and_payload_before_signing() {
        let root = temp_root("prepared-signing");
        let request = fixture_request(&root);
        let prepared_request = PrepareRequest {
            pack_root: request.pack_root.clone(),
            pack_id: request.pack_id,
            pack_version: request.pack_version,
            security_epoch: request.security_epoch,
            backend: request.backend,
            provider: request.provider,
            target_os: request.target_os,
            target_arch: request.target_arch,
            worker_path: request.worker_path,
        };
        let prepared = prepare_pack(&prepared_request).unwrap();
        assert!(root.join(MANIFEST_NAME).is_file());
        assert!(!root.join(SIGNATURE_NAME).exists());
        assert_eq!(inspect_prepared_pack(&root).unwrap(), prepared);

        let wrong_digest = "0".repeat(64);
        let error = sign_prepared_pack(
            &root,
            &SigningMode::Fixture,
            &wrong_digest,
            &prepared.pack_digest,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("caller-approved manifest and pack digests"));
        assert!(!root.join(SIGNATURE_NAME).exists());

        let error = sign_prepared_pack(
            &root,
            &SigningMode::Fixture,
            &prepared.manifest_sha256,
            &wrong_digest,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("caller-approved manifest and pack digests"));
        assert!(!root.join(SIGNATURE_NAME).exists());

        let authored = sign_prepared_pack(
            &root,
            &SigningMode::Fixture,
            &prepared.manifest_sha256,
            &prepared.pack_digest,
        )
        .unwrap();
        assert_eq!(authored.pack_digest, prepared.pack_digest);
        assert_eq!(verify_fixture_pack(&root).unwrap(), authored);
        let duplicate = sign_prepared_pack(
            &root,
            &SigningMode::Fixture,
            &prepared.manifest_sha256,
            &prepared.pack_digest,
        )
        .unwrap_err()
        .to_string();
        assert!(duplicate.contains("must not contain a signature envelope"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn prepared_pack_rejects_payload_changes_without_writing_a_signature() {
        let root = temp_root("prepared-tamper");
        let request = fixture_request(&root);
        let prepared_request = PrepareRequest {
            pack_root: request.pack_root.clone(),
            pack_id: request.pack_id,
            pack_version: request.pack_version,
            security_epoch: request.security_epoch,
            backend: request.backend,
            provider: request.provider,
            target_os: request.target_os,
            target_arch: request.target_arch,
            worker_path: request.worker_path,
        };
        let prepared = prepare_pack(&prepared_request).unwrap();
        fs::write(root.join("bin/scribe-inference-worker.exe"), b"tampered").unwrap();
        let error = sign_prepared_pack(
            &root,
            &SigningMode::Fixture,
            &prepared.manifest_sha256,
            &prepared.pack_digest,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("payload does not match"));
        assert!(!root.join(SIGNATURE_NAME).exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn prepared_pack_rejects_non_ascii_payload_names() {
        let root = temp_root("prepared-unicode");
        fs::write(root.join("bin/providér.dll"), b"provider").unwrap();
        let request = fixture_request(&root);
        let prepared_request = PrepareRequest {
            pack_root: request.pack_root,
            pack_id: request.pack_id,
            pack_version: request.pack_version,
            security_epoch: request.security_epoch,
            backend: request.backend,
            provider: request.provider,
            target_os: request.target_os,
            target_arch: request.target_arch,
            worker_path: request.worker_path,
        };
        let error = prepare_pack(&prepared_request).unwrap_err().to_string();
        assert!(error.contains("payload paths must be ASCII"));
        assert!(!root.join(MANIFEST_NAME).exists());
        assert!(!root.join(SIGNATURE_NAME).exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn prepared_pack_accepts_the_maximum_payload_file_count() {
        let root = temp_root("prepared-max-files");
        for index in 1..MAX_FILES {
            fs::write(
                root.join("bin").join(format!("payload-{index:03}.dll")),
                b"x",
            )
            .unwrap();
        }
        let request = fixture_request(&root);
        let prepared_request = PrepareRequest {
            pack_root: request.pack_root,
            pack_id: request.pack_id,
            pack_version: request.pack_version,
            security_epoch: request.security_epoch,
            backend: request.backend,
            provider: request.provider,
            target_os: request.target_os,
            target_arch: request.target_arch,
            worker_path: request.worker_path,
        };
        let prepared = prepare_pack(&prepared_request).unwrap();
        assert_eq!(prepared.payload_files, MAX_FILES);
        assert_eq!(
            inspect_prepared_pack(&root).unwrap().payload_files,
            MAX_FILES
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn production_authoring_fails_before_writing_without_embedded_public_key() {
        let root = temp_root("production-closed");
        let mut request = fixture_request(&root);
        request.signing = SigningMode::Production {
            key_id: "scribe-production-ed25519-v1".to_owned(),
            private_key_path: root.join("missing-production-key.pk8"),
        };
        let error = author_pack(&request).unwrap_err().to_string();
        assert!(error.contains("no separately reviewed public key embedded"));
        assert!(!root.join(MANIFEST_NAME).exists());
        assert!(!root.join(SIGNATURE_NAME).exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn fixture_authoring_accepts_each_production_backend_target() {
        for (label, backend, provider, target_os, target_arch, worker_path) in [
            (
                "windows-cuda",
                AuthoringBackend::Cuda,
                "transcribe-cpp-ggml-cuda",
                "windows",
                "x86_64",
                "bin/scribe-inference-worker.exe",
            ),
            (
                "windows-vulkan",
                AuthoringBackend::Vulkan,
                "transcribe-cpp-ggml-vulkan",
                "windows",
                "x86_64",
                "bin/scribe-inference-worker.exe",
            ),
            (
                "linux-cuda",
                AuthoringBackend::Cuda,
                "transcribe-cpp-ggml-cuda",
                "linux",
                "x86_64",
                "bin/scribe-inference-worker",
            ),
            (
                "linux-vulkan",
                AuthoringBackend::Vulkan,
                "transcribe-cpp-ggml-vulkan",
                "linux",
                "x86_64",
                "bin/scribe-inference-worker",
            ),
            (
                "macos-metal-arm",
                AuthoringBackend::Metal,
                "transcribe-cpp-metal",
                "macos",
                "aarch64",
                "bin/scribe-inference-worker.exe",
            ),
            (
                "macos-metal-intel",
                AuthoringBackend::Metal,
                "transcribe-cpp-metal",
                "macos",
                "x86_64",
                "bin/scribe-inference-worker.exe",
            ),
        ] {
            let root = temp_root(label);
            if !root.join(worker_path).exists() {
                fs::write(root.join(worker_path), b"worker").unwrap();
            }
            let mut request = fixture_request(&root);
            request.backend = backend;
            request.provider = provider.to_owned();
            request.target_os = target_os.to_owned();
            request.target_arch = target_arch.to_owned();
            request.worker_path = worker_path.to_owned();
            let authored = author_pack(&request).unwrap();
            let (manifest, _) = manifest_from_root(&root, "fixture").unwrap();
            assert_eq!(manifest.backend, backend.manifest_backend());
            assert_eq!(manifest.target_os, target_os);
            assert_eq!(manifest.target_arch, target_arch);
            assert_eq!(verify_fixture_pack(&root).unwrap(), authored);
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn authoring_rejects_incoherent_or_noncanonical_targets_before_writing() {
        for (label, backend, target_os, target_arch) in [
            (
                "metal-windows",
                AuthoringBackend::Metal,
                "windows",
                "x86_64",
            ),
            ("cuda-macos", AuthoringBackend::Cuda, "macos", "aarch64"),
            ("vulkan-macos", AuthoringBackend::Vulkan, "macos", "x86_64"),
            ("windows-arm", AuthoringBackend::Cuda, "windows", "aarch64"),
            ("macos-armv7", AuthoringBackend::Metal, "macos", "armv7"),
            ("linux-metal", AuthoringBackend::Metal, "linux", "x86_64"),
            ("linux-arm", AuthoringBackend::Cuda, "linux", "aarch64"),
            ("case-os", AuthoringBackend::Metal, "MacOS", "aarch64"),
            ("case-arch", AuthoringBackend::Metal, "macos", "AARCH64"),
            ("empty-os", AuthoringBackend::Metal, "", "aarch64"),
            ("empty-arch", AuthoringBackend::Metal, "macos", ""),
        ] {
            let root = temp_root(label);
            let mut request = fixture_request(&root);
            request.backend = backend;
            request.target_os = target_os.to_owned();
            request.target_arch = target_arch.to_owned();
            let error = author_pack(&request).unwrap_err().to_string();
            assert!(error.contains(AUTHOR_TARGET_CONTRACT));
            assert!(!root.join(MANIFEST_NAME).exists());
            assert!(!root.join(SIGNATURE_NAME).exists());
            fs::remove_dir_all(root).unwrap();
        }
        assert_eq!(AuthoringBackend::parse("Metal"), None);
        assert_eq!(AuthoringBackend::parse(""), None);
    }

    #[cfg(windows)]
    #[test]
    fn authoring_rejects_hardlinked_payload() {
        let root = temp_root("hardlink");
        fs::hard_link(
            root.join("bin/scribe-inference-worker.exe"),
            root.join("bin/worker-alias.exe"),
        )
        .unwrap();
        assert!(author_pack(&fixture_request(&root)).is_err());
        assert!(!root.join(MANIFEST_NAME).exists());
        fs::remove_dir_all(root).unwrap();
    }
}
