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

use crate::manifest::{
    APP_PROTOCOL_VERSION, Compatibility, EMBEDDED_MINIMUM_SECURITY_EPOCH, MANIFEST_NAME,
    MAX_FILE_BYTES, MAX_FILES, PACK_SCHEMA_VERSION, PackBackend, PackManifest, PackVerifier,
    PayloadEntry, ProductionTrustRoot, RUNTIME_ABI_VERSION, SIGNATURE_NAME, StoreComponent,
    TrustRoot, compute_pack_digest, hash_exact_length, is_link_or_reparse, open_regular_no_follow,
    reject_hardlink, reject_named_streams, validate_build_identity, validate_identifier,
    validate_inventory, validate_relative_path, validate_root,
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
}

pub(crate) const AUTHOR_TARGET_CONTRACT: &str = "allowed authoring targets are cuda or vulkan on windows/x86_64, or metal on macos/aarch64 or macos/x86_64; backend, OS, and architecture values are lowercase and case-sensitive";

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
    validate_build_identity(crate::worker_identity::DESKTOP_BUILD_ID, "app build")?;
    validate_build_identity(
        crate::worker_identity::INFERENCE_WORKER_BUILD_ID,
        "worker build",
    )?;

    let (key_id, key_pair) = match &request.signing {
        SigningMode::Fixture => (
            FIXTURE_KEY_ID.to_owned(),
            Ed25519KeyPair::from_seed_unchecked(&FIXTURE_SEED)
                .map_err(|_| anyhow!("fixture signing key is invalid"))?,
        ),
        SigningMode::Production {
            key_id,
            private_key_path,
        } => (
            key_id.clone(),
            production_key_pair(key_id, private_key_path)?,
        ),
    };

    let payload = inventory_payload(&request.pack_root)?;
    if !payload
        .iter()
        .any(|entry| entry.path == request.worker_path)
    {
        bail!("declared worker path is absent from the payload inventory");
    }
    let installed_payload_bytes = payload.iter().try_fold(0_u64, |total, entry| {
        total
            .checked_add(entry.size_bytes)
            .ok_or_else(|| anyhow!("payload byte count overflowed"))
    })?;
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
    let manifest_bytes = serde_json::to_vec(&manifest)?;
    let signature = DetachedSignature {
        schema_version: PACK_SCHEMA_VERSION,
        key_id: key_id.clone(),
        signature_hex: encode_hex(key_pair.sign(&manifest_bytes).as_ref()),
    };
    let signature_bytes = serde_json::to_vec(&signature)?;

    let manifest_path = request.pack_root.join(MANIFEST_NAME);
    let signature_path = request.pack_root.join(SIGNATURE_NAME);
    write_new_envelope(&manifest_path, &manifest_bytes)
        .context("could not create canonical worker-pack manifest")?;
    if let Err(error) = write_new_envelope(&signature_path, &signature_bytes) {
        let _ = fs::remove_file(&manifest_path);
        return Err(error).context("could not create canonical worker-pack signature");
    }

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
            target_os: &request.target_os,
            target_arch: &request.target_arch,
            allowed_backends: &allowed_backends,
        },
    );
    let verified = verifier.verify(&request.pack_root).inspect_err(|_| {
        let _ = fs::remove_file(&signature_path);
        let _ = fs::remove_file(&manifest_path);
    })?;
    if verified.pack_digest != manifest.pack_digest {
        let _ = fs::remove_file(&signature_path);
        let _ = fs::remove_file(&manifest_path);
        bail!("authored pack digest changed during self-verification");
    }

    Ok(AuthoredPack {
        pack_id: request.pack_id.clone(),
        pack_version: request.pack_version.clone(),
        pack_digest: manifest.pack_digest,
        key_id,
        payload_files: manifest.payload.len(),
        installed_payload_bytes,
    })
}

pub(crate) fn verify_fixture_pack(root: &Path) -> Result<AuthoredPack> {
    let manifest = manifest_from_root(root)?;
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
            "windows",
            "x86_64"
        ) | (AuthoringBackend::Metal, "macos", "aarch64" | "x86_64")
    );
    if !accepted {
        bail!("invalid backend/target combination; {AUTHOR_TARGET_CONTRACT}");
    }
    Ok(())
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

fn inventory_payload(root: &Path) -> Result<Vec<PayloadEntry>> {
    validate_root(root)?;
    if root.join(MANIFEST_NAME).exists() || root.join(SIGNATURE_NAME).exists() {
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
            validate_relative_path(&relative)?;
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
            if payload.len() >= MAX_FILES {
                bail!("pack payload exceeds the maximum file count");
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

fn manifest_from_root(root: &Path) -> Result<PackManifest> {
    let bytes = fs::read(root.join(MANIFEST_NAME)).context("could not read fixture manifest")?;
    let manifest: PackManifest = serde_json::from_slice(&bytes)?;
    if serde_json::to_vec(&manifest)? != bytes {
        bail!("fixture manifest is not canonical JSON");
    }
    Ok(manifest)
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
            let mut request = fixture_request(&root);
            request.backend = backend;
            request.provider = provider.to_owned();
            request.target_os = target_os.to_owned();
            request.target_arch = target_arch.to_owned();
            request.worker_path = worker_path.to_owned();
            let authored = author_pack(&request).unwrap();
            let manifest = manifest_from_root(&root).unwrap();
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
            ("linux", AuthoringBackend::Metal, "linux", "x86_64"),
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
