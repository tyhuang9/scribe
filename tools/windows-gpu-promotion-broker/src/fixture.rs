//! Test-only hostile-input broker model.
//!
//! This module deliberately does not compile into release artifacts. It proves
//! the request, copy, signing, replay, epoch, recovery, and publication state
//! transitions without pretending that a local runner-owned ledger is a
//! production trust boundary.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow, bail};
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{PromotionRequest, validate_sha256, validate_store_component};

const HANDOFF_NAME: &str = "windows-gpu-pack-handoff.json";
const MANIFEST_NAME: &str = "pack-manifest.json";
const SIGNATURE_NAME: &str = "pack-manifest.sig";
const RECEIPT_NAME: &str = "protected-promotion-receipt.json";
const LEDGER_NAME: &str = "fixture-promotion-ledger.jsonl";
const MAX_HANDOFF_BYTES: u64 = 256 * 1024;
const MAX_MANIFEST_BYTES: u64 = 256 * 1024;
const MAX_LEDGER_BYTES: u64 = 4 * 1024 * 1024;
const MAX_FILES: usize = 256;
const MAX_DEPTH: usize = 12;
const MAX_DIRECTORIES: usize = MAX_FILES * MAX_DEPTH;
const MAX_TREE_ENTRIES: usize = MAX_FILES + MAX_DIRECTORIES + 2;
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_AGGREGATE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const RELEASE_SET_DOMAIN: &[u8] = b"scribe-windows-gpu-release-set-v1\0";
const PACK_DIGEST_DOMAIN: &[u8] = b"scribe-gpu-worker-pack-digest-v1\0";
const RECEIPT_DOMAIN: &[u8] = b"scribe-windows-gpu-promotion-receipt-v1\0";
const REQUEST_DOMAIN: &[u8] = b"scribe-windows-gpu-promotion-request-v1\0";
const LEDGER_DOMAIN: &[u8] = b"scribe-windows-gpu-promotion-ledger-record-v1\0";
const FIXTURE_KEY_ID: &str = "fixture-ed25519-v1";
const FIXTURE_SEED: [u8; 32] = [7; 32];

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Backend {
    Cuda,
    Vulkan,
}

impl Backend {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Cuda => "cuda",
            Self::Vulkan => "vulkan",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PayloadEntry {
    path: String,
    size_bytes: u64,
    sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackManifest {
    schema_version: u16,
    pack_id: String,
    pack_version: String,
    pack_digest: String,
    security_epoch: u64,
    app_protocol_version: u16,
    worker_protocol_version: u16,
    runtime_abi_version: u16,
    app_build: String,
    worker_build: String,
    backend: Backend,
    provider: String,
    target_os: String,
    target_arch: String,
    worker_path: String,
    payload: Vec<PayloadEntry>,
}

#[derive(Serialize)]
struct PackDigestMaterial<'a> {
    schema_version: u16,
    pack_id: &'a str,
    pack_version: &'a str,
    security_epoch: u64,
    app_protocol_version: u16,
    worker_protocol_version: u16,
    runtime_abi_version: u16,
    app_build: &'a str,
    worker_build: &'a str,
    backend: &'a Backend,
    provider: &'a str,
    target_os: &'a str,
    target_arch: &'a str,
    worker_path: &'a str,
    payload: &'a [PayloadEntry],
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct HandoffPack {
    backend: Backend,
    pack_root: String,
    pack_id: String,
    pack_version: String,
    pack_digest: String,
    security_epoch: u64,
    provider: String,
    manifest_sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Handoff {
    schema_version: u16,
    source_repository: String,
    source_ref: String,
    source_revision: String,
    workflow_ref: String,
    run_id: String,
    run_attempt: String,
    pack_version: String,
    toolchain_manifest_sha256: String,
    packs: Vec<HandoffPack>,
    release_set_digest: String,
}

#[derive(Serialize)]
struct ReleaseMaterial<'a> {
    schema_version: u16,
    source_repository: &'a str,
    source_ref: &'a str,
    source_revision: &'a str,
    workflow_ref: &'a str,
    run_id: &'a str,
    run_attempt: &'a str,
    pack_version: &'a str,
    toolchain_manifest_sha256: &'a str,
    packs: &'a [HandoffPack],
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PackReceipt {
    schema_version: u16,
    backend: Backend,
    pack_id: String,
    pack_version: String,
    pack_digest: String,
    security_epoch: u64,
    manifest_sha256: String,
    signature_key_id: String,
    signature_envelope_sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReceiptStatement {
    schema_version: u16,
    authority: String,
    request_sha256: String,
    source_repository: String,
    source_ref: String,
    source_revision: String,
    workflow_ref: String,
    workflow_source_sha: String,
    run_id: String,
    run_attempt: String,
    artifact_id: String,
    artifact_digest: String,
    handoff_sha256: String,
    release_set_digest: String,
    toolchain_manifest_sha256: String,
    pack_version: String,
    packs: Vec<PackReceipt>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignedReceipt {
    schema_version: u16,
    statement: ReceiptStatement,
    key_id: String,
    signature_hex: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DetachedSignature {
    schema_version: u16,
    key_id: String,
    signature_hex: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum LedgerKind {
    Genesis,
    Reserved,
    Ready,
    Published,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EpochBinding {
    backend: Backend,
    pack_id: String,
    security_epoch: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LedgerRecord {
    schema_version: u16,
    sequence: u64,
    previous_record_sha256: String,
    kind: LedgerKind,
    release_set_digest: Option<String>,
    request_sha256: Option<String>,
    stage_name: Option<String>,
    output_name: Option<String>,
    epochs: Vec<EpochBinding>,
    record_sha256: String,
}

struct LedgerTransition {
    kind: LedgerKind,
    release_set_digest: Option<String>,
    request_sha256: Option<String>,
    stage_name: Option<String>,
    output_name: Option<String>,
    epochs: Vec<EpochBinding>,
}

impl LedgerTransition {
    fn genesis() -> Self {
        Self {
            kind: LedgerKind::Genesis,
            release_set_digest: None,
            request_sha256: None,
            stage_name: None,
            output_name: None,
            epochs: Vec::new(),
        }
    }

    fn bound(
        kind: LedgerKind,
        release_set_digest: &str,
        request_sha256: &str,
        stage_name: &str,
        output_name: &str,
        epochs: Vec<EpochBinding>,
    ) -> Self {
        Self {
            kind,
            release_set_digest: Some(release_set_digest.to_owned()),
            request_sha256: Some(request_sha256.to_owned()),
            stage_name: Some(stage_name.to_owned()),
            output_name: Some(output_name.to_owned()),
            epochs,
        }
    }
}

#[derive(Serialize)]
struct LedgerMaterial<'a> {
    schema_version: u16,
    sequence: u64,
    previous_record_sha256: &'a str,
    kind: &'a LedgerKind,
    release_set_digest: &'a Option<String>,
    request_sha256: &'a Option<String>,
    stage_name: &'a Option<String>,
    output_name: &'a Option<String>,
    epochs: &'a [EpochBinding],
}

#[derive(Clone, Debug)]
struct ReleaseState {
    kind: LedgerKind,
    request_sha256: String,
    stage_name: String,
    output_name: String,
}

struct LedgerSnapshot {
    last_hash: String,
    next_sequence: u64,
    used: BTreeSet<String>,
    epochs: BTreeMap<(String, String), u64>,
    releases: BTreeMap<String, ReleaseState>,
}

impl Default for LedgerSnapshot {
    fn default() -> Self {
        Self {
            last_hash: "0".repeat(64),
            next_sequence: 0,
            used: BTreeSet::new(),
            epochs: BTreeMap::new(),
            releases: BTreeMap::new(),
        }
    }
}

struct PinnedFile {
    relative: String,
    file: File,
    size: u64,
    sha256: String,
}

struct PinnedPack {
    manifest: PackManifest,
    manifest_bytes: Vec<u8>,
    manifest_file: File,
    files: Vec<PinnedFile>,
    _directories: Vec<File>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SignedPackObservation {
    manifest: PackManifest,
    manifest_sha256: String,
    signature_key_id: String,
    signature_envelope_sha256: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FaultPoint {
    None,
    AfterReserve,
    AfterCudaCopy,
    AfterReady,
    AfterPublish,
}

#[derive(Clone)]
struct FixtureBroker {
    handoff_parent: PathBuf,
    publication_parent: PathBuf,
    state_root: PathBuf,
    process_lock: Arc<Mutex<()>>,
}

impl FixtureBroker {
    fn initialize(
        handoff_parent: PathBuf,
        publication_parent: PathBuf,
        state_root: PathBuf,
    ) -> Result<Self> {
        fs::create_dir_all(&handoff_parent)?;
        fs::create_dir_all(&publication_parent)?;
        fs::create_dir_all(&state_root)?;
        let broker = Self {
            handoff_parent,
            publication_parent,
            state_root,
            process_lock: Arc::new(Mutex::new(())),
        };
        let ledger_path = broker.ledger_path();
        let mut ledger = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&ledger_path)
            .context("fixture ledger already exists or cannot be initialized")?;
        let genesis = new_ledger_record(0, &"0".repeat(64), LedgerTransition::genesis())?;
        write_ledger_record(&mut ledger, &genesis)?;
        Ok(broker)
    }

    fn ledger_path(&self) -> PathBuf {
        self.state_root.join(LEDGER_NAME)
    }

    fn promote(&self, request: &PromotionRequest, fault: FaultPoint) -> Result<()> {
        let _guard = self
            .process_lock
            .lock()
            .map_err(|_| anyhow!("fixture broker lock poisoned"))?;
        request.validate()?;
        let (handoff_root, output_root, output_name) = self.resolve_roots(request)?;
        let mut ledger = open_existing_ledger(&self.ledger_path())?;
        let snapshot = load_ledger(&mut ledger)?;
        if snapshot.used.contains(&request.release_set_digest) {
            bail!("release set was already consumed");
        }
        if output_root.exists() {
            bail!("publication output already exists");
        }
        let request_sha256 = hash_domain(REQUEST_DOMAIN, &request.canonical_json()?);
        let (handoff, mut packs) = inspect_handoff(&handoff_root, request)?;
        validate_epoch_policy(&snapshot, &handoff.packs, request.minimum_security_epoch)?;
        let stage_name = format!(".staging-{}", request.release_set_digest);
        let stage_root = self.publication_parent.join(&stage_name);
        if stage_root.exists() {
            bail!("fixture signer staging root was not fresh");
        }
        let epochs = handoff
            .packs
            .iter()
            .map(|pack| EpochBinding {
                backend: pack.backend.clone(),
                pack_id: pack.pack_id.clone(),
                security_epoch: pack.security_epoch,
            })
            .collect::<Vec<_>>();
        append_transition(
            &mut ledger,
            &snapshot,
            LedgerTransition::bound(
                LedgerKind::Reserved,
                &request.release_set_digest,
                &request_sha256,
                &stage_name,
                &output_name,
                epochs,
            ),
        )?;
        if fault == FaultPoint::AfterReserve {
            bail!("injected fixture fault after reservation");
        }

        fs::create_dir(&stage_root)?;
        let mut copied_packs = Vec::with_capacity(2);
        for (index, pack) in packs.iter_mut().enumerate() {
            let destination = stage_root.join(pack.manifest.backend.as_str());
            let copied = copy_prepared_pack(pack, &destination, request)?;
            if copied.manifest.pack_digest != pack.manifest.pack_digest {
                bail!("copied prepared pack changed before authority use");
            }
            copied_packs.push(copied);
            if index == 0 && fault == FaultPoint::AfterCudaCopy {
                bail!("injected fixture fault after CUDA copy");
            }
        }
        // The fixture authority is intentionally instantiated only after both
        // complete prepared packs have been retained, copied, and revalidated.
        let key_pair = fixture_key_pair()?;
        let mut signed_packs = Vec::with_capacity(2);
        for copied in &copied_packs {
            signed_packs.push(sign_copied_pack(
                copied,
                &stage_root.join(copied.manifest.backend.as_str()),
                &key_pair,
            )?);
        }
        drop(copied_packs);
        let receipt = create_receipt(request, &signed_packs, &request_sha256, &key_pair)?;
        write_new_synced(
            &stage_root.join(RECEIPT_NAME),
            &serde_json::to_vec(&receipt)?,
        )?;
        verify_published_set(&stage_root, request, key_pair.public_key().as_ref())?;

        let snapshot = load_ledger(&mut ledger)?;
        append_transition(
            &mut ledger,
            &snapshot,
            LedgerTransition::bound(
                LedgerKind::Ready,
                &request.release_set_digest,
                &request_sha256,
                &stage_name,
                &output_name,
                Vec::new(),
            ),
        )?;
        if fault == FaultPoint::AfterReady {
            bail!("injected fixture fault after ready transition");
        }
        atomic_publish(&stage_root, &output_root)?;
        if fault == FaultPoint::AfterPublish {
            bail!("injected fixture fault after atomic publication");
        }
        let snapshot = load_ledger(&mut ledger)?;
        append_transition(
            &mut ledger,
            &snapshot,
            LedgerTransition::bound(
                LedgerKind::Published,
                &request.release_set_digest,
                &request_sha256,
                &stage_name,
                &output_name,
                Vec::new(),
            ),
        )?;
        Ok(())
    }

    fn recover(&self) -> Result<()> {
        let _guard = self
            .process_lock
            .lock()
            .map_err(|_| anyhow!("fixture broker lock poisoned"))?;
        let mut ledger = open_existing_ledger(&self.ledger_path())?;
        let snapshot = load_ledger(&mut ledger)?;
        for (digest, state) in snapshot.releases.clone() {
            let stage = self.publication_parent.join(&state.stage_name);
            let output = self.publication_parent.join(&state.output_name);
            match state.kind {
                LedgerKind::Reserved => {
                    if output.exists() {
                        bail!("reserved release unexpectedly has a public output");
                    }
                    remove_fixture_stage(&self.publication_parent, &stage)?;
                }
                LedgerKind::Ready => {
                    if stage.exists() == output.exists() {
                        bail!("ready release has an ambiguous publication state");
                    }
                    if stage.exists() {
                        verify_fixture_output(&stage, &digest, &state.request_sha256)?;
                        atomic_publish(&stage, &output)?;
                    } else {
                        verify_fixture_output(&output, &digest, &state.request_sha256)?;
                    }
                    let current = load_ledger(&mut ledger)?;
                    append_transition(
                        &mut ledger,
                        &current,
                        LedgerTransition::bound(
                            LedgerKind::Published,
                            &digest,
                            &state.request_sha256,
                            &state.stage_name,
                            &state.output_name,
                            Vec::new(),
                        ),
                    )?;
                }
                LedgerKind::Published => {
                    if stage.exists() || !output.exists() {
                        bail!("published release state does not match signer-owned output");
                    }
                    verify_fixture_output(&output, &digest, &state.request_sha256)?;
                }
                LedgerKind::Genesis => bail!("invalid per-release genesis state"),
            }
        }
        Ok(())
    }

    fn resolve_roots(&self, request: &PromotionRequest) -> Result<(PathBuf, PathBuf, String)> {
        let handoff_root = PathBuf::from(&request.handoff_root);
        let requested_output_root = PathBuf::from(&request.output_root);
        let handoff_parent = fs::canonicalize(&self.handoff_parent)?;
        let actual_handoff = fs::canonicalize(&handoff_root)?;
        if actual_handoff.parent() != Some(handoff_parent.as_path()) {
            bail!("handoff is outside the fixture broker intake root");
        }
        let output_name = requested_output_root
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or_else(|| anyhow!("output name is not canonical UTF-8"))?
            .to_owned();
        validate_store_component(&output_name)?;
        let requested_output_parent = requested_output_root
            .parent()
            .ok_or_else(|| anyhow!("output has no existing publication parent"))?;
        let approved_publication_parent = fs::canonicalize(&self.publication_parent)
            .context("fixture broker publication root is unavailable")?;
        let actual_output_parent = fs::canonicalize(requested_output_parent)
            .context("requested publication parent is unavailable")?;
        if actual_output_parent != approved_publication_parent {
            bail!("output is outside the fixture broker publication root");
        }
        let output_root = approved_publication_parent.join(&output_name);
        Ok((actual_handoff, output_root, output_name))
    }
}

fn inspect_handoff(root: &Path, request: &PromotionRequest) -> Result<(Handoff, Vec<PinnedPack>)> {
    let root_handle = open_directory_no_follow(root)?;
    let root_entries = bounded_directory_names(root, 3)?;
    let expected = [
        OsString::from("cuda"),
        OsString::from("vulkan"),
        OsString::from(HANDOFF_NAME),
    ];
    if root_entries != expected {
        bail!("handoff top-level inventory is not exact");
    }
    let handoff_file = open_regular_no_follow(&root.join(HANDOFF_NAME))?;
    let handoff_bytes = read_exact_bounded(&handoff_file, MAX_HANDOFF_BYTES)?;
    if encode_hex(&Sha256::digest(&handoff_bytes)) != request.handoff_sha256 {
        bail!("handoff digest does not match request");
    }
    let handoff: Handoff = parse_canonical(&handoff_bytes, "handoff")?;
    validate_handoff(&handoff, request)?;
    let release_material = ReleaseMaterial {
        schema_version: handoff.schema_version,
        source_repository: &handoff.source_repository,
        source_ref: &handoff.source_ref,
        source_revision: &handoff.source_revision,
        workflow_ref: &handoff.workflow_ref,
        run_id: &handoff.run_id,
        run_attempt: &handoff.run_attempt,
        pack_version: &handoff.pack_version,
        toolchain_manifest_sha256: &handoff.toolchain_manifest_sha256,
        packs: &handoff.packs,
    };
    let release_digest = hash_domain(RELEASE_SET_DOMAIN, &serde_json::to_vec(&release_material)?);
    if release_digest != request.release_set_digest {
        bail!("release-set digest is not canonical");
    }
    let mut packs = Vec::with_capacity(2);
    for handoff_pack in &handoff.packs {
        let pack = inspect_pack(&root.join(handoff_pack.backend.as_str()), request)?;
        validate_pack_binding(&pack, handoff_pack, request)?;
        packs.push(pack);
    }
    drop(root_handle);
    Ok((handoff, packs))
}

fn validate_handoff(handoff: &Handoff, request: &PromotionRequest) -> Result<()> {
    if handoff.schema_version != 1
        || handoff.source_repository != request.source_repository
        || handoff.source_ref != request.source_ref
        || handoff.source_revision != request.source_revision
        || handoff.workflow_ref != request.workflow_ref
        || handoff.run_id != request.run_id
        || handoff.run_attempt != request.run_attempt
        || handoff.pack_version != request.pack_version
        || handoff.toolchain_manifest_sha256 != request.toolchain_manifest_sha256
        || handoff.release_set_digest != request.release_set_digest
    {
        bail!("handoff provenance does not match the promotion request");
    }
    if handoff.packs.len() != 2
        || handoff.packs[0].backend != Backend::Cuda
        || handoff.packs[1].backend != Backend::Vulkan
        || handoff.packs[0].pack_root != "cuda"
        || handoff.packs[1].pack_root != "vulkan"
    {
        bail!("handoff must contain exactly CUDA then Vulkan");
    }
    validate_sha256(&handoff.toolchain_manifest_sha256)?;
    Ok(())
}

fn validate_pack_binding(
    pack: &PinnedPack,
    handoff: &HandoffPack,
    request: &PromotionRequest,
) -> Result<()> {
    let manifest_sha256 = encode_hex(&Sha256::digest(&pack.manifest_bytes));
    let manifest = &pack.manifest;
    if manifest.backend != handoff.backend
        || manifest.pack_id != handoff.pack_id
        || manifest.pack_version != handoff.pack_version
        || manifest.pack_version != request.pack_version
        || manifest.pack_digest != handoff.pack_digest
        || manifest.security_epoch != handoff.security_epoch
        || manifest.provider != handoff.provider
        || manifest_sha256 != handoff.manifest_sha256
    {
        bail!("prepared pack does not match its handoff binding");
    }
    Ok(())
}

fn inspect_pack(root: &Path, request: &PromotionRequest) -> Result<PinnedPack> {
    let root_handle = open_directory_no_follow(root)?;
    let manifest_file = open_regular_no_follow(&root.join(MANIFEST_NAME))?;
    reject_hardlink(&manifest_file)?;
    reject_named_streams(&root.join(MANIFEST_NAME))?;
    let manifest_bytes = read_exact_bounded(&manifest_file, MAX_MANIFEST_BYTES)?;
    let manifest: PackManifest = parse_canonical(&manifest_bytes, "manifest")?;
    validate_manifest(&manifest, request)?;

    let expected_files = manifest
        .payload
        .iter()
        .map(|entry| entry.path.clone())
        .chain(std::iter::once(MANIFEST_NAME.to_owned()))
        .collect::<BTreeSet<_>>();
    let mut expected_directories = BTreeSet::new();
    for entry in &manifest.payload {
        let mut parent = Path::new(&entry.path).parent();
        while let Some(path) = parent {
            if path.as_os_str().is_empty() {
                break;
            }
            expected_directories.insert(path.to_string_lossy().replace('\\', "/"));
            parent = path.parent();
        }
    }
    let mut observed_files = BTreeSet::new();
    let mut observed_casefolded = BTreeSet::new();
    let mut observed_entries = 0_usize;
    let mut directories = vec![root_handle];
    let mut pending = vec![(root.to_path_buf(), 0_usize)];
    while let Some((directory, depth)) = pending.pop() {
        if depth > MAX_DEPTH {
            bail!("pack directory depth exceeds bound");
        }
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            observed_entries = observed_entries
                .checked_add(1)
                .ok_or_else(|| anyhow!("pack tree entry count overflowed"))?;
            if observed_entries > MAX_TREE_ENTRIES {
                bail!("pack tree entry count exceeds bound");
            }
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if is_link_or_reparse(&metadata) {
                bail!("pack contains a link or reparse point");
            }
            let relative = path
                .strip_prefix(root)?
                .components()
                .map(|component| {
                    component
                        .as_os_str()
                        .to_str()
                        .ok_or_else(|| anyhow!("path is not UTF-8"))
                })
                .collect::<Result<Vec<_>>>()?
                .join("/");
            validate_relative_path(&relative, relative == MANIFEST_NAME)?;
            if !observed_casefolded.insert(relative.to_ascii_lowercase()) {
                bail!("pack contains a case-insensitive collision");
            }
            if metadata.is_dir() {
                if !expected_directories.contains(&relative) {
                    bail!("pack contains an unexpected directory");
                }
                reject_named_streams(&path)?;
                directories.push(open_directory_no_follow(&path)?);
                pending.push((path, depth + 1));
            } else if metadata.is_file() {
                if !expected_files.contains(&relative) {
                    bail!("pack contains an unexpected file");
                }
                observed_files.insert(relative);
            } else {
                bail!("pack contains a nonregular entry");
            }
        }
    }
    if observed_files != expected_files {
        bail!("pack file inventory is not exact");
    }

    let mut aggregate = 0_u64;
    let mut files = Vec::with_capacity(manifest.payload.len());
    for entry in &manifest.payload {
        let path = root.join(Path::new(&entry.path));
        let file = open_regular_no_follow(&path)?;
        reject_hardlink(&file)?;
        reject_named_streams(&path)?;
        let size = file.metadata()?.len();
        if size != entry.size_bytes || size > MAX_FILE_BYTES {
            bail!("payload size does not match bounded inventory");
        }
        aggregate = aggregate
            .checked_add(size)
            .ok_or_else(|| anyhow!("payload size overflow"))?;
        if aggregate > MAX_AGGREGATE_BYTES {
            bail!("payload aggregate exceeds bound");
        }
        let actual = hash_file(&file, size)?;
        if actual != entry.sha256 {
            bail!("payload hash does not match inventory");
        }
        files.push(PinnedFile {
            relative: entry.path.clone(),
            file,
            size,
            sha256: actual,
        });
    }
    Ok(PinnedPack {
        manifest,
        manifest_bytes,
        manifest_file,
        files,
        _directories: directories,
    })
}

fn validate_manifest(manifest: &PackManifest, request: &PromotionRequest) -> Result<()> {
    if manifest.schema_version != 1
        || manifest.app_protocol_version != 5
        || manifest.worker_protocol_version != 5
        || manifest.runtime_abi_version != 1
        || manifest.target_os != "windows"
        || manifest.target_arch != "x86_64"
        || manifest.pack_version != request.pack_version
        || manifest.security_epoch < request.minimum_security_epoch
        || manifest.worker_path != "bin/scribe-inference-worker.exe"
        || manifest.payload.is_empty()
        || manifest.payload.len() > MAX_FILES
    {
        bail!("prepared manifest is incompatible with the protected contract");
    }
    validate_store_component(&manifest.pack_id)?;
    validate_store_component(&manifest.pack_version)?;
    validate_sha256(&manifest.pack_digest)?;
    let expected_provider = match manifest.backend {
        Backend::Cuda => "transcribe-cpp-ggml-cuda",
        Backend::Vulkan => "transcribe-cpp-ggml-vulkan",
    };
    let expected_pack_id = match manifest.backend {
        Backend::Cuda => "scribe-cuda-windows-x64",
        Backend::Vulkan => "scribe-vulkan-windows-x64",
    };
    if manifest.pack_id != expected_pack_id
        || manifest.provider != expected_provider
        || manifest.app_build != format!("local-transcriber@0.1.0#{}", request.source_revision)
        || manifest.worker_build
            != format!("scribe-inference-worker@0.1.0#{}", request.source_revision)
    {
        bail!("prepared manifest provider or build identity is not exact");
    }
    let mut previous: Option<&str> = None;
    let mut casefolded = BTreeSet::new();
    let mut worker_seen = false;
    let mut aggregate = 0_u64;
    for entry in &manifest.payload {
        validate_relative_path(&entry.path, false)?;
        validate_sha256(&entry.sha256)?;
        if previous.is_some_and(|value| value >= entry.path.as_str())
            || !casefolded.insert(entry.path.to_ascii_lowercase())
            || entry.size_bytes > MAX_FILE_BYTES
        {
            bail!("prepared manifest inventory is not canonical");
        }
        aggregate = aggregate
            .checked_add(entry.size_bytes)
            .ok_or_else(|| anyhow!("inventory size overflow"))?;
        if aggregate > MAX_AGGREGATE_BYTES {
            bail!("inventory aggregate exceeds bound");
        }
        previous = Some(&entry.path);
        worker_seen |= entry.path == manifest.worker_path;
    }
    validate_relative_path(&manifest.worker_path, false)?;
    if !worker_seen || compute_pack_digest(manifest)? != manifest.pack_digest {
        bail!("prepared manifest digest or worker identity is invalid");
    }
    Ok(())
}

fn compute_pack_digest(manifest: &PackManifest) -> Result<String> {
    let material = PackDigestMaterial {
        schema_version: manifest.schema_version,
        pack_id: &manifest.pack_id,
        pack_version: &manifest.pack_version,
        security_epoch: manifest.security_epoch,
        app_protocol_version: manifest.app_protocol_version,
        worker_protocol_version: manifest.worker_protocol_version,
        runtime_abi_version: manifest.runtime_abi_version,
        app_build: &manifest.app_build,
        worker_build: &manifest.worker_build,
        backend: &manifest.backend,
        provider: &manifest.provider,
        target_os: &manifest.target_os,
        target_arch: &manifest.target_arch,
        worker_path: &manifest.worker_path,
        payload: &manifest.payload,
    };
    Ok(hash_domain(
        PACK_DIGEST_DOMAIN,
        &serde_json::to_vec(&material)?,
    ))
}

fn copy_prepared_pack(
    pack: &mut PinnedPack,
    destination: &Path,
    request: &PromotionRequest,
) -> Result<PinnedPack> {
    fs::create_dir(destination)?;
    for entry in &pack.files {
        if let Some(parent) = Path::new(&entry.relative).parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(destination.join(parent))?;
        }
    }
    copy_retained_file(
        &mut pack.manifest_file,
        pack.manifest_bytes.len() as u64,
        &encode_hex(&Sha256::digest(&pack.manifest_bytes)),
        &destination.join(MANIFEST_NAME),
    )?;
    for entry in &mut pack.files {
        copy_retained_file(
            &mut entry.file,
            entry.size,
            &entry.sha256,
            &destination.join(Path::new(&entry.relative)),
        )?;
    }
    inspect_pack(destination, request)
}

fn sign_copied_pack(
    copied: &PinnedPack,
    destination: &Path,
    key_pair: &Ed25519KeyPair,
) -> Result<SignedPackObservation> {
    let signature = DetachedSignature {
        schema_version: 1,
        key_id: FIXTURE_KEY_ID.to_owned(),
        signature_hex: encode_hex(key_pair.sign(&copied.manifest_bytes).as_ref()),
    };
    write_new_synced(
        &destination.join(SIGNATURE_NAME),
        &serde_json::to_vec(&signature)?,
    )?;
    let verified = inspect_signed_pack(destination, key_pair.public_key().as_ref())?;
    if verified.manifest.pack_digest != copied.manifest.pack_digest {
        bail!("signed pack changed before fixture publication");
    }
    Ok(verified)
}

fn copy_retained_file(
    source: &mut File,
    size: u64,
    expected_sha256: &str,
    target: &Path,
) -> Result<()> {
    source.seek(SeekFrom::Start(0))?;
    let mut destination = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)?;
    let mut hasher = Sha256::new();
    let mut copied = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        copied = copied
            .checked_add(read as u64)
            .ok_or_else(|| anyhow!("copy size overflow"))?;
        if copied > size {
            bail!("retained source grew during copy");
        }
        hasher.update(&buffer[..read]);
        destination.write_all(&buffer[..read])?;
    }
    if copied != size || encode_hex(&hasher.finalize()) != expected_sha256 {
        bail!("retained source changed during copy");
    }
    destination.sync_all()?;
    Ok(())
}

fn inspect_signed_pack(root: &Path, public_key: &[u8]) -> Result<SignedPackObservation> {
    let manifest_file = open_regular_no_follow(&root.join(MANIFEST_NAME))?;
    reject_hardlink(&manifest_file)?;
    reject_named_streams(&root.join(MANIFEST_NAME))?;
    let bytes = read_exact_bounded(&manifest_file, MAX_MANIFEST_BYTES)?;
    let manifest: PackManifest = parse_canonical(&bytes, "signed manifest")?;
    let signature_file = open_regular_no_follow(&root.join(SIGNATURE_NAME))?;
    reject_hardlink(&signature_file)?;
    reject_named_streams(&root.join(SIGNATURE_NAME))?;
    let signature_bytes = read_exact_bounded(&signature_file, 4 * 1024)?;
    let signature: DetachedSignature = parse_canonical(&signature_bytes, "pack signature")?;
    if signature.schema_version != 1 || signature.key_id != FIXTURE_KEY_ID {
        bail!("signed pack authority is not the fixture authority");
    }
    let detached_signature = decode_hex_exact(&signature.signature_hex, 64)?;
    UnparsedPublicKey::new(&ED25519, public_key)
        .verify(&bytes, &detached_signature)
        .map_err(|_| anyhow!("signed pack signature is invalid"))?;
    verify_signed_tree(root, &manifest, public_key)?;
    Ok(SignedPackObservation {
        manifest,
        manifest_sha256: encode_hex(&Sha256::digest(&bytes)),
        signature_key_id: signature.key_id,
        signature_envelope_sha256: encode_hex(&Sha256::digest(&signature_bytes)),
    })
}

fn verify_signed_tree(root: &Path, manifest: &PackManifest, _public_key: &[u8]) -> Result<()> {
    let mut expected = manifest
        .payload
        .iter()
        .map(|entry| entry.path.clone())
        .collect::<BTreeSet<_>>();
    expected.insert(MANIFEST_NAME.to_owned());
    expected.insert(SIGNATURE_NAME.to_owned());
    let mut expected_directories = BTreeSet::new();
    for entry in &manifest.payload {
        let mut parent = Path::new(&entry.path).parent();
        while let Some(path) = parent {
            if path.as_os_str().is_empty() {
                break;
            }
            expected_directories.insert(path.to_string_lossy().replace('\\', "/"));
            parent = path.parent();
        }
    }
    let mut files = BTreeSet::new();
    let mut casefolded = BTreeSet::new();
    let mut observed_entries = 0_usize;
    let mut pending = vec![(root.to_path_buf(), 0_usize)];
    while let Some((directory, depth)) = pending.pop() {
        let _directory = open_directory_no_follow(&directory)?;
        if depth > MAX_DEPTH {
            bail!("signed pack directory depth exceeds bound");
        }
        for entry in fs::read_dir(&directory)? {
            let entry = entry?;
            observed_entries = observed_entries
                .checked_add(1)
                .ok_or_else(|| anyhow!("signed pack tree entry count overflowed"))?;
            if observed_entries > MAX_TREE_ENTRIES {
                bail!("signed pack tree entry count exceeds bound");
            }
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)?;
            if is_link_or_reparse(&metadata) {
                bail!("signed pack contains a link or reparse point");
            }
            let relative = path
                .strip_prefix(root)?
                .components()
                .map(|part| {
                    part.as_os_str()
                        .to_str()
                        .ok_or_else(|| anyhow!("signed path is not UTF-8"))
                })
                .collect::<Result<Vec<_>>>()?
                .join("/");
            if !casefolded.insert(relative.to_ascii_lowercase()) {
                bail!("signed pack contains a case collision");
            }
            if metadata.is_dir() {
                if !expected_directories.contains(&relative) {
                    bail!("signed pack contains unexpected directory");
                }
                reject_named_streams(&path)?;
                pending.push((path, depth + 1));
            } else if metadata.is_file() {
                if !expected.contains(&relative) {
                    bail!("signed pack contains unexpected file");
                }
                files.insert(relative);
            } else {
                bail!("signed pack contains nonregular entry");
            }
        }
    }
    if files != expected {
        bail!("signed pack inventory is not exact");
    }
    let mut aggregate = 0_u64;
    for payload in &manifest.payload {
        let file = open_regular_no_follow(&root.join(Path::new(&payload.path)))?;
        reject_hardlink(&file)?;
        reject_named_streams(&root.join(Path::new(&payload.path)))?;
        aggregate = aggregate
            .checked_add(payload.size_bytes)
            .ok_or_else(|| anyhow!("signed payload aggregate overflowed"))?;
        if aggregate > MAX_AGGREGATE_BYTES
            || file.metadata()?.len() != payload.size_bytes
            || hash_file(&file, payload.size_bytes)? != payload.sha256
        {
            bail!("signed payload does not match manifest");
        }
    }
    Ok(())
}

fn create_receipt(
    request: &PromotionRequest,
    signed_packs: &[SignedPackObservation],
    request_sha256: &str,
    key_pair: &Ed25519KeyPair,
) -> Result<SignedReceipt> {
    let statement = expected_receipt_statement(request, signed_packs, request_sha256)?;
    let statement_bytes = serde_json::to_vec(&statement)?;
    let mut signed = Vec::with_capacity(RECEIPT_DOMAIN.len() + statement_bytes.len());
    signed.extend_from_slice(RECEIPT_DOMAIN);
    signed.extend_from_slice(&statement_bytes);
    Ok(SignedReceipt {
        schema_version: 1,
        statement,
        key_id: FIXTURE_KEY_ID.to_owned(),
        signature_hex: encode_hex(key_pair.sign(&signed).as_ref()),
    })
}

fn expected_receipt_statement(
    request: &PromotionRequest,
    signed_packs: &[SignedPackObservation],
    request_sha256: &str,
) -> Result<ReceiptStatement> {
    if signed_packs.len() != 2
        || signed_packs[0].manifest.backend != Backend::Cuda
        || signed_packs[1].manifest.backend != Backend::Vulkan
    {
        bail!("signed pack observations are not exactly CUDA then Vulkan");
    }
    for pack in signed_packs {
        validate_manifest(&pack.manifest, request)?;
    }
    Ok(ReceiptStatement {
        schema_version: 1,
        authority: "fixture-only".to_owned(),
        request_sha256: request_sha256.to_owned(),
        source_repository: request.source_repository.clone(),
        source_ref: request.source_ref.clone(),
        source_revision: request.source_revision.clone(),
        workflow_ref: request.workflow_ref.clone(),
        workflow_source_sha: request.workflow_source_sha.clone(),
        run_id: request.run_id.clone(),
        run_attempt: request.run_attempt.clone(),
        artifact_id: request.artifact_id.clone(),
        artifact_digest: request.artifact_digest.clone(),
        handoff_sha256: request.handoff_sha256.clone(),
        release_set_digest: request.release_set_digest.clone(),
        toolchain_manifest_sha256: request.toolchain_manifest_sha256.clone(),
        pack_version: request.pack_version.clone(),
        packs: signed_packs
            .iter()
            .map(|pack| PackReceipt {
                schema_version: pack.manifest.schema_version,
                backend: pack.manifest.backend.clone(),
                pack_id: pack.manifest.pack_id.clone(),
                pack_version: pack.manifest.pack_version.clone(),
                pack_digest: pack.manifest.pack_digest.clone(),
                security_epoch: pack.manifest.security_epoch,
                manifest_sha256: pack.manifest_sha256.clone(),
                signature_key_id: pack.signature_key_id.clone(),
                signature_envelope_sha256: pack.signature_envelope_sha256.clone(),
            })
            .collect(),
    })
}

fn verify_receipt_signature(receipt: &SignedReceipt, public_key: &[u8]) -> Result<()> {
    if receipt.schema_version != 1 || receipt.key_id != FIXTURE_KEY_ID {
        bail!("receipt envelope is incompatible");
    }
    let statement = serde_json::to_vec(&receipt.statement)?;
    let mut signed = Vec::with_capacity(RECEIPT_DOMAIN.len() + statement.len());
    signed.extend_from_slice(RECEIPT_DOMAIN);
    signed.extend_from_slice(&statement);
    UnparsedPublicKey::new(&ED25519, public_key)
        .verify(&signed, &decode_hex_exact(&receipt.signature_hex, 64)?)
        .map_err(|_| anyhow!("receipt signature is invalid"))?;
    Ok(())
}

fn verify_published_set(root: &Path, request: &PromotionRequest, public_key: &[u8]) -> Result<()> {
    validate_exact_public_inventory(root)?;
    let cuda = inspect_signed_pack(&root.join("cuda"), public_key)?;
    let vulkan = inspect_signed_pack(&root.join("vulkan"), public_key)?;
    if cuda.manifest.backend != Backend::Cuda || vulkan.manifest.backend != Backend::Vulkan {
        bail!("published pack order or backend is invalid");
    }
    let receipt_file = open_regular_no_follow(&root.join(RECEIPT_NAME))?;
    reject_hardlink(&receipt_file)?;
    reject_named_streams(&root.join(RECEIPT_NAME))?;
    let receipt: SignedReceipt = parse_canonical(
        &read_exact_bounded(&receipt_file, MAX_HANDOFF_BYTES)?,
        "promotion receipt",
    )?;
    verify_receipt_signature(&receipt, public_key)?;
    let request_sha256 = hash_domain(REQUEST_DOMAIN, &request.canonical_json()?);
    let expected = expected_receipt_statement(request, &[cuda, vulkan], &request_sha256)?;
    if receipt.statement != expected {
        bail!("receipt statement does not exactly bind request and signed pack observations");
    }
    Ok(())
}

fn validate_exact_public_inventory(root: &Path) -> Result<()> {
    let _root = open_directory_no_follow(root)?;
    let names = bounded_directory_names(root, 3)?;
    let expected = [
        OsString::from("cuda"),
        OsString::from(RECEIPT_NAME),
        OsString::from("vulkan"),
    ];
    if names != expected {
        bail!("published set inventory is not exact");
    }
    for backend in ["cuda", "vulkan"] {
        let metadata = fs::symlink_metadata(root.join(backend))?;
        if !metadata.is_dir() || is_link_or_reparse(&metadata) {
            bail!("published backend root is not a physical directory");
        }
    }
    Ok(())
}

fn bounded_directory_names(root: &Path, maximum: usize) -> Result<Vec<OsString>> {
    let mut names = Vec::with_capacity(maximum);
    for entry in fs::read_dir(root)? {
        if names.len() == maximum {
            bail!("directory entry count exceeds the accepted bound");
        }
        names.push(entry?.file_name());
    }
    names.sort();
    Ok(names)
}

fn validate_epoch_policy(
    snapshot: &LedgerSnapshot,
    packs: &[HandoffPack],
    minimum: u64,
) -> Result<()> {
    for pack in packs {
        let high_water = snapshot
            .epochs
            .get(&(pack.backend.as_str().to_owned(), pack.pack_id.clone()))
            .copied()
            .unwrap_or(minimum);
        if pack.security_epoch < minimum || pack.security_epoch < high_water {
            bail!("security epoch is below the durable high-water mark");
        }
    }
    Ok(())
}

fn new_ledger_record(
    sequence: u64,
    previous: &str,
    transition: LedgerTransition,
) -> Result<LedgerRecord> {
    let material = LedgerMaterial {
        schema_version: 1,
        sequence,
        previous_record_sha256: previous,
        kind: &transition.kind,
        release_set_digest: &transition.release_set_digest,
        request_sha256: &transition.request_sha256,
        stage_name: &transition.stage_name,
        output_name: &transition.output_name,
        epochs: &transition.epochs,
    };
    let record_sha256 = hash_domain(LEDGER_DOMAIN, &serde_json::to_vec(&material)?);
    Ok(LedgerRecord {
        schema_version: 1,
        sequence,
        previous_record_sha256: previous.to_owned(),
        kind: transition.kind,
        release_set_digest: transition.release_set_digest,
        request_sha256: transition.request_sha256,
        stage_name: transition.stage_name,
        output_name: transition.output_name,
        epochs: transition.epochs,
        record_sha256,
    })
}

fn append_transition(
    ledger: &mut File,
    snapshot: &LedgerSnapshot,
    transition: LedgerTransition,
) -> Result<()> {
    let record = new_ledger_record(snapshot.next_sequence, &snapshot.last_hash, transition)?;
    ledger.seek(SeekFrom::End(0))?;
    write_ledger_record(ledger, &record)
}

fn write_ledger_record(ledger: &mut File, record: &LedgerRecord) -> Result<()> {
    let bytes = serde_json::to_vec(record)?;
    ledger.write_all(&bytes)?;
    ledger.write_all(b"\n")?;
    ledger.sync_all()?;
    Ok(())
}

fn open_existing_ledger(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).append(true);
    configure_exclusive_ledger(&mut options);
    let file = options
        .open(path)
        .context("fixture ledger is missing or inaccessible; refusing to recreate it")?;
    reject_hardlink(&file)?;
    reject_named_streams(path)?;
    if file.metadata()?.len() == 0 || file.metadata()?.len() > MAX_LEDGER_BYTES {
        bail!("fixture ledger size is outside the accepted bound");
    }
    Ok(file)
}

fn load_ledger(ledger: &mut File) -> Result<LedgerSnapshot> {
    ledger.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    ledger.take(MAX_LEDGER_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_LEDGER_BYTES || !bytes.ends_with(b"\n") {
        bail!("fixture ledger is oversized or torn");
    }
    let mut snapshot = LedgerSnapshot::default();
    let mut releases = BTreeMap::<String, ReleaseState>::new();
    for (index, line) in bytes[..bytes.len() - 1]
        .split(|byte| *byte == b'\n')
        .enumerate()
    {
        if line.is_empty() {
            bail!("fixture ledger contains an empty record");
        }
        let record: LedgerRecord = parse_canonical(line, "ledger record")?;
        let material = LedgerMaterial {
            schema_version: record.schema_version,
            sequence: record.sequence,
            previous_record_sha256: &record.previous_record_sha256,
            kind: &record.kind,
            release_set_digest: &record.release_set_digest,
            request_sha256: &record.request_sha256,
            stage_name: &record.stage_name,
            output_name: &record.output_name,
            epochs: &record.epochs,
        };
        let expected_hash = hash_domain(LEDGER_DOMAIN, &serde_json::to_vec(&material)?);
        if record.schema_version != 1
            || record.sequence != index as u64
            || record.previous_record_sha256 != snapshot.last_hash
            || record.record_sha256 != expected_hash
        {
            bail!("fixture ledger hash chain is invalid");
        }
        if index == 0 {
            if record.kind != LedgerKind::Genesis
                || record.previous_record_sha256 != "0".repeat(64)
                || record.release_set_digest.is_some()
                || !record.epochs.is_empty()
            {
                bail!("fixture ledger genesis is invalid");
            }
        } else {
            apply_ledger_transition(&mut snapshot, &mut releases, &record)?;
        }
        snapshot.last_hash = record.record_sha256;
        snapshot.next_sequence = record.sequence + 1;
    }
    if snapshot.next_sequence == 0 {
        bail!("fixture ledger has no genesis record");
    }
    snapshot.releases = releases;
    Ok(snapshot)
}

fn apply_ledger_transition(
    snapshot: &mut LedgerSnapshot,
    releases: &mut BTreeMap<String, ReleaseState>,
    record: &LedgerRecord,
) -> Result<()> {
    let release = record
        .release_set_digest
        .as_ref()
        .ok_or_else(|| anyhow!("ledger transition has no release digest"))?;
    let request = record
        .request_sha256
        .as_ref()
        .ok_or_else(|| anyhow!("ledger transition has no request digest"))?;
    let stage = record
        .stage_name
        .as_ref()
        .ok_or_else(|| anyhow!("ledger transition has no stage name"))?;
    let output = record
        .output_name
        .as_ref()
        .ok_or_else(|| anyhow!("ledger transition has no output name"))?;
    validate_sha256(release)?;
    validate_sha256(request)?;
    validate_store_component(output)?;
    if stage != &format!(".staging-{release}") {
        bail!("ledger stage name is not canonical");
    }
    match record.kind {
        LedgerKind::Reserved => {
            if !snapshot.used.insert(release.clone())
                || releases.contains_key(release)
                || record.epochs.len() != 2
            {
                bail!("ledger replay or invalid reservation");
            }
            if record.epochs[0].backend != Backend::Cuda
                || record.epochs[1].backend != Backend::Vulkan
            {
                bail!("ledger epochs are not ordered CUDA then Vulkan");
            }
            for epoch in &record.epochs {
                let key = (epoch.backend.as_str().to_owned(), epoch.pack_id.clone());
                let prior = snapshot.epochs.get(&key).copied().unwrap_or(0);
                if epoch.security_epoch < prior {
                    bail!("ledger security epoch regressed");
                }
                snapshot.epochs.insert(key, epoch.security_epoch);
            }
            releases.insert(
                release.clone(),
                ReleaseState {
                    kind: LedgerKind::Reserved,
                    request_sha256: request.clone(),
                    stage_name: stage.clone(),
                    output_name: output.clone(),
                },
            );
        }
        LedgerKind::Ready | LedgerKind::Published => {
            if !record.epochs.is_empty() {
                bail!("non-reservation ledger transition contains epochs");
            }
            let current = releases
                .get_mut(release)
                .ok_or_else(|| anyhow!("ledger transition has no reservation"))?;
            let expected = match record.kind {
                LedgerKind::Ready => LedgerKind::Reserved,
                LedgerKind::Published => LedgerKind::Ready,
                _ => unreachable!(),
            };
            if current.kind != expected
                || current.request_sha256 != *request
                || current.stage_name != *stage
                || current.output_name != *output
            {
                bail!("ledger state transition is invalid");
            }
            current.kind = record.kind.clone();
        }
        LedgerKind::Genesis => bail!("ledger contains a second genesis"),
    }
    Ok(())
}

fn verify_fixture_output(
    root: &Path,
    expected_release_set_digest: &str,
    expected_request_sha256: &str,
) -> Result<()> {
    validate_exact_public_inventory(root)?;
    let public_key = fixture_key_pair()?.public_key().as_ref().to_vec();
    let cuda = inspect_signed_pack(&root.join("cuda"), &public_key)?;
    let vulkan = inspect_signed_pack(&root.join("vulkan"), &public_key)?;
    if cuda.manifest.backend != Backend::Cuda || vulkan.manifest.backend != Backend::Vulkan {
        bail!("fixture output does not contain the exact backend pair");
    }
    let receipt_file = open_regular_no_follow(&root.join(RECEIPT_NAME))?;
    reject_hardlink(&receipt_file)?;
    reject_named_streams(&root.join(RECEIPT_NAME))?;
    let receipt: SignedReceipt = parse_canonical(
        &read_exact_bounded(&receipt_file, MAX_HANDOFF_BYTES)?,
        "promotion receipt",
    )?;
    verify_receipt_signature(&receipt, &public_key)?;
    let actual_packs = [cuda, vulkan]
        .iter()
        .map(|pack| PackReceipt {
            schema_version: pack.manifest.schema_version,
            backend: pack.manifest.backend.clone(),
            pack_id: pack.manifest.pack_id.clone(),
            pack_version: pack.manifest.pack_version.clone(),
            pack_digest: pack.manifest.pack_digest.clone(),
            security_epoch: pack.manifest.security_epoch,
            manifest_sha256: pack.manifest_sha256.clone(),
            signature_key_id: pack.signature_key_id.clone(),
            signature_envelope_sha256: pack.signature_envelope_sha256.clone(),
        })
        .collect::<Vec<_>>();
    if receipt.statement.schema_version != 1
        || receipt.statement.authority != "fixture-only"
        || receipt.statement.release_set_digest != expected_release_set_digest
        || receipt.statement.request_sha256 != expected_request_sha256
        || receipt.statement.packs != actual_packs
    {
        bail!("fixture output receipt does not bind ledger and exact signed packs");
    }
    Ok(())
}

fn parse_canonical<T>(bytes: &[u8], label: &str) -> Result<T>
where
    T: for<'de> Deserialize<'de> + Serialize,
{
    let parsed =
        serde_json::from_slice::<T>(bytes).with_context(|| format!("{label} JSON is invalid"))?;
    if serde_json::to_vec(&parsed)? != bytes {
        bail!("{label} JSON is not canonical");
    }
    Ok(parsed)
}

fn hash_domain(domain: &[u8], bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain);
    hasher.update(bytes);
    encode_hex(&hasher.finalize())
}

fn hash_file(file: &File, expected_size: u64) -> Result<String> {
    let mut file = file.try_clone()?;
    file.seek(SeekFrom::Start(0))?;
    let mut hasher = Sha256::new();
    let mut observed = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        observed = observed
            .checked_add(read as u64)
            .ok_or_else(|| anyhow!("file size overflow"))?;
        if observed > expected_size {
            bail!("file grew while hashed");
        }
        hasher.update(&buffer[..read]);
    }
    if observed != expected_size {
        bail!("file changed while hashed");
    }
    Ok(encode_hex(&hasher.finalize()))
}

fn read_exact_bounded(file: &File, maximum: u64) -> Result<Vec<u8>> {
    let size = file.metadata()?.len();
    if size == 0 || size > maximum {
        bail!("file size is outside accepted bound");
    }
    let mut file = file.try_clone()?;
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::with_capacity(size as usize);
    file.take(maximum + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 != size {
        bail!("file changed while read");
    }
    Ok(bytes)
}

fn write_new_synced(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn fixture_key_pair() -> Result<Ed25519KeyPair> {
    Ed25519KeyPair::from_seed_unchecked(&FIXTURE_SEED)
        .map_err(|_| anyhow!("fixture signing authority is invalid"))
}

fn validate_relative_path(path: &str, manifest_allowed: bool) -> Result<()> {
    if path.is_empty()
        || !path.is_ascii()
        || path.starts_with('/')
        || path.contains('\\')
        || path.contains(':')
    {
        bail!("path is not a canonical relative pack path");
    }
    let components = Path::new(path).components().collect::<Vec<_>>();
    if components.is_empty() || components.len() > MAX_DEPTH {
        bail!("path depth is outside the accepted bound");
    }
    for component in components {
        let Component::Normal(name) = component else {
            bail!("path contains a traversal component");
        };
        let name = name
            .to_str()
            .ok_or_else(|| anyhow!("path component is not UTF-8"))?;
        if name.is_empty()
            || name.len() > 128
            || name.ends_with('.')
            || name.ends_with(' ')
            || name.bytes().any(|byte| byte < 0x20)
            || is_reserved_windows_name(name)
            || (!manifest_allowed && matches!(name, MANIFEST_NAME | SIGNATURE_NAME))
        {
            bail!("path contains an unsafe component");
        }
    }
    Ok(())
}

fn is_reserved_windows_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
}

fn open_regular_no_follow(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    configure_no_follow_file(&mut options);
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || is_link_or_reparse(&metadata) {
        bail!("entry is not a regular non-reparse file");
    }
    Ok(file)
}

fn open_directory_no_follow(path: &Path) -> Result<File> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || is_link_or_reparse(&metadata) {
        bail!("entry is not a physical directory");
    }
    let mut options = OpenOptions::new();
    options.read(true);
    configure_no_follow_directory(&mut options);
    let file = options.open(path)?;
    let opened = file.metadata()?;
    if !opened.is_dir() || is_link_or_reparse(&opened) {
        bail!("opened entry is not a directory");
    }
    reject_named_streams(path)?;
    Ok(file)
}

#[cfg(windows)]
fn configure_no_follow_file(options: &mut OpenOptions) {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ};
    options
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
}

#[cfg(not(windows))]
fn configure_no_follow_file(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.custom_flags(libc_o_nofollow());
}

#[cfg(windows)]
fn configure_no_follow_directory(options: &mut OpenOptions) {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
    };
    options
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
}

#[cfg(not(windows))]
fn configure_no_follow_directory(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.custom_flags(libc_o_nofollow());
}

#[cfg(not(windows))]
fn libc_o_nofollow() -> i32 {
    // The fixture contract is qualified on Windows. This portable fallback is
    // used only so source and request tests remain buildable elsewhere.
    0
}

#[cfg(windows)]
fn configure_exclusive_ledger(options: &mut OpenOptions) {
    use std::os::windows::fs::OpenOptionsExt;
    options.share_mode(0);
}

#[cfg(not(windows))]
fn configure_exclusive_ledger(_options: &mut OpenOptions) {}

#[cfg(windows)]
fn is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_type().is_symlink()
        || metadata.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0
}

#[cfg(not(windows))]
fn is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(windows)]
fn reject_hardlink(file: &File) -> Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };
    let mut information = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) } == 0 {
        return Err(io::Error::last_os_error().into());
    }
    if information.nNumberOfLinks != 1 {
        bail!("hardlinked files are forbidden");
    }
    Ok(())
}

#[cfg(not(windows))]
fn reject_hardlink(file: &File) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    if file.metadata()?.nlink() != 1 {
        bail!("hardlinked files are forbidden");
    }
    Ok(())
}

#[cfg(windows)]
fn reject_named_streams(path: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::Storage::FileSystem::{
        FindClose, FindFirstStreamW, FindNextStreamW, FindStreamInfoStandard,
        WIN32_FIND_STREAM_DATA,
    };
    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut data = unsafe { std::mem::zeroed::<WIN32_FIND_STREAM_DATA>() };
    let handle = unsafe {
        FindFirstStreamW(
            wide.as_ptr(),
            FindStreamInfoStandard,
            &mut data as *mut _ as *mut _,
            0,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(38) {
            Ok(())
        } else {
            Err(error.into())
        };
    }
    let named = |entry: &WIN32_FIND_STREAM_DATA| {
        let length = entry
            .cStreamName
            .iter()
            .position(|item| *item == 0)
            .unwrap_or(entry.cStreamName.len());
        String::from_utf16_lossy(&entry.cStreamName[..length]) != "::$DATA"
    };
    let mut found = named(&data);
    while !found && unsafe { FindNextStreamW(handle, &mut data as *mut _ as *mut _) } != 0 {
        found = named(&data);
    }
    let final_error = io::Error::last_os_error();
    unsafe { FindClose(handle) };
    if found {
        bail!("alternate data streams are forbidden");
    }
    if final_error.raw_os_error() != Some(38) {
        return Err(final_error.into());
    }
    Ok(())
}

#[cfg(not(windows))]
fn reject_named_streams(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(windows)]
fn atomic_publish(source: &Path, destination: &Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};
    let source = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    if unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        return Err(io::Error::last_os_error()).context("atomic write-through publication failed");
    }
    Ok(())
}

#[cfg(not(windows))]
fn atomic_publish(source: &Path, destination: &Path) -> Result<()> {
    fs::rename(source, destination)?;
    File::open(
        destination
            .parent()
            .ok_or_else(|| anyhow!("publication has no parent"))?,
    )?
    .sync_all()?;
    Ok(())
}

fn remove_fixture_stage(parent: &Path, stage: &Path) -> Result<()> {
    if !stage.exists() {
        return Ok(());
    }
    if stage.parent() != Some(parent)
        || !stage
            .file_name()
            .and_then(OsStr::to_str)
            .is_some_and(|name| name.starts_with(".staging-") && name.len() == 73)
    {
        bail!("refusing to remove a non-fixture staging directory");
    }
    fs::remove_dir_all(stage)?;
    Ok(())
}

fn decode_hex_exact(value: &str, bytes: usize) -> Result<Vec<u8>> {
    if value.len() != bytes * 2
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("hex value is noncanonical");
    }
    (0..bytes)
        .map(|index| u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).map_err(Into::into))
        .collect()
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::thread;
    use tempfile::TempDir;

    struct Fixture {
        _temp: TempDir,
        broker: FixtureBroker,
        request: PromotionRequest,
        handoff_parent: PathBuf,
        publication_parent: PathBuf,
    }

    impl Fixture {
        fn new(epoch: u64) -> Self {
            let temp = tempfile::tempdir().unwrap();
            let handoff_parent = temp.path().join("intake");
            let publication_parent = temp.path().join("published");
            let state_root = temp.path().join("state");
            let broker = FixtureBroker::initialize(
                handoff_parent.clone(),
                publication_parent.clone(),
                state_root.clone(),
            )
            .unwrap();
            let request = create_request(
                &handoff_parent,
                &publication_parent,
                "release-a",
                epoch,
                "one",
            );
            Self {
                _temp: temp,
                broker,
                request,
                handoff_parent,
                publication_parent,
            }
        }

        fn output(&self) -> PathBuf {
            PathBuf::from(&self.request.output_root)
        }
    }

    fn create_request(
        handoff_parent: &Path,
        publication_parent: &Path,
        output_name: &str,
        epoch: u64,
        payload_marker: &str,
    ) -> PromotionRequest {
        fs::create_dir_all(handoff_parent).unwrap();
        fs::create_dir_all(publication_parent).unwrap();
        let handoff_root = handoff_parent.join(format!("handoff-{output_name}"));
        fs::create_dir(&handoff_root).unwrap();
        let revision = "a".repeat(40);
        let cuda = create_prepared_pack(
            &handoff_root,
            Backend::Cuda,
            epoch,
            &revision,
            payload_marker,
        );
        let vulkan = create_prepared_pack(
            &handoff_root,
            Backend::Vulkan,
            epoch,
            &revision,
            payload_marker,
        );
        let mut handoff = Handoff {
            schema_version: 1,
            source_repository: "owner/repo".to_owned(),
            source_ref: "refs/heads/main".to_owned(),
            source_revision: revision.clone(),
            workflow_ref:
                "owner/repo/.github/workflows/windows-gpu-pack-promotion.yml@refs/heads/main"
                    .to_owned(),
            run_id: "123".to_owned(),
            run_attempt: "1".to_owned(),
            pack_version: "0.1.0".to_owned(),
            toolchain_manifest_sha256: "e".repeat(64),
            packs: vec![cuda, vulkan],
            release_set_digest: String::new(),
        };
        handoff.release_set_digest = hash_domain(
            RELEASE_SET_DOMAIN,
            &serde_json::to_vec(&ReleaseMaterial {
                schema_version: handoff.schema_version,
                source_repository: &handoff.source_repository,
                source_ref: &handoff.source_ref,
                source_revision: &handoff.source_revision,
                workflow_ref: &handoff.workflow_ref,
                run_id: &handoff.run_id,
                run_attempt: &handoff.run_attempt,
                pack_version: &handoff.pack_version,
                toolchain_manifest_sha256: &handoff.toolchain_manifest_sha256,
                packs: &handoff.packs,
            })
            .unwrap(),
        );
        let handoff_bytes = serde_json::to_vec(&handoff).unwrap();
        write_new_synced(&handoff_root.join(HANDOFF_NAME), &handoff_bytes).unwrap();
        PromotionRequest {
            schema_version: 1,
            handoff_root: handoff_root.to_string_lossy().into_owned(),
            output_root: publication_parent
                .join(output_name)
                .to_string_lossy()
                .into_owned(),
            source_repository: handoff.source_repository,
            source_ref: handoff.source_ref,
            source_revision: revision.clone(),
            workflow_ref: handoff.workflow_ref,
            workflow_source_sha: revision,
            run_id: handoff.run_id,
            run_attempt: handoff.run_attempt,
            artifact_id: "456".to_owned(),
            artifact_digest: "b".repeat(64),
            handoff_sha256: encode_hex(&Sha256::digest(&handoff_bytes)),
            release_set_digest: handoff.release_set_digest,
            toolchain_manifest_sha256: handoff.toolchain_manifest_sha256,
            pack_version: handoff.pack_version,
            minimum_security_epoch: 1,
            require_unused_release_set: true,
        }
    }

    fn create_prepared_pack(
        handoff_root: &Path,
        backend: Backend,
        epoch: u64,
        revision: &str,
        marker: &str,
    ) -> HandoffPack {
        let backend_name = backend.as_str();
        let root = handoff_root.join(backend_name);
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("bin")).unwrap();
        let worker = format!("fixture-{backend_name}-{marker}").into_bytes();
        write_new_synced(&root.join("bin/scribe-inference-worker.exe"), &worker).unwrap();
        let provider = match backend {
            Backend::Cuda => "transcribe-cpp-ggml-cuda",
            Backend::Vulkan => "transcribe-cpp-ggml-vulkan",
        };
        let mut manifest = PackManifest {
            schema_version: 1,
            pack_id: format!("scribe-{backend_name}-windows-x64"),
            pack_version: "0.1.0".to_owned(),
            pack_digest: "0".repeat(64),
            security_epoch: epoch,
            app_protocol_version: 5,
            worker_protocol_version: 5,
            runtime_abi_version: 1,
            app_build: format!("local-transcriber@0.1.0#{revision}"),
            worker_build: format!("scribe-inference-worker@0.1.0#{revision}"),
            backend: backend.clone(),
            provider: provider.to_owned(),
            target_os: "windows".to_owned(),
            target_arch: "x86_64".to_owned(),
            worker_path: "bin/scribe-inference-worker.exe".to_owned(),
            payload: vec![PayloadEntry {
                path: "bin/scribe-inference-worker.exe".to_owned(),
                size_bytes: worker.len() as u64,
                sha256: encode_hex(&Sha256::digest(&worker)),
            }],
        };
        manifest.pack_digest = compute_pack_digest(&manifest).unwrap();
        let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
        write_new_synced(&root.join(MANIFEST_NAME), &manifest_bytes).unwrap();
        HandoffPack {
            backend,
            pack_root: backend_name.to_owned(),
            pack_id: manifest.pack_id,
            pack_version: manifest.pack_version,
            pack_digest: manifest.pack_digest,
            security_epoch: manifest.security_epoch,
            provider: manifest.provider,
            manifest_sha256: encode_hex(&Sha256::digest(&manifest_bytes)),
        }
    }

    fn copy_tree_for_substitution_test(source: &Path, destination: &Path) {
        fs::create_dir(destination).unwrap();
        for entry in fs::read_dir(source).unwrap() {
            let entry = entry.unwrap();
            let target = destination.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_tree_for_substitution_test(&entry.path(), &target);
            } else {
                fs::copy(entry.path(), target).unwrap();
            }
        }
    }

    #[test]
    fn promotes_exact_pair_with_domain_separated_receipt_and_chained_ledger() {
        let fixture = Fixture::new(1);
        fixture
            .broker
            .promote(&fixture.request, FaultPoint::None)
            .unwrap();
        verify_fixture_output(
            &fixture.output(),
            &fixture.request.release_set_digest,
            &hash_domain(REQUEST_DOMAIN, &fixture.request.canonical_json().unwrap()),
        )
        .unwrap();
        let mut ledger = open_existing_ledger(&fixture.broker.ledger_path()).unwrap();
        let snapshot = load_ledger(&mut ledger).unwrap();
        assert!(snapshot.used.contains(&fixture.request.release_set_digest));
        assert_eq!(snapshot.next_sequence, 4);
        assert_eq!(
            snapshot.releases[&fixture.request.release_set_digest].kind,
            LedgerKind::Published
        );
        let receipt_bytes = fs::read(fixture.output().join(RECEIPT_NAME)).unwrap();
        assert!(
            !receipt_bytes
                .windows(RECEIPT_DOMAIN.len())
                .any(|window| window == RECEIPT_DOMAIN)
        );
        let receipt: SignedReceipt = parse_canonical(&receipt_bytes, "receipt").unwrap();
        assert_eq!(receipt.statement.authority, "fixture-only");
    }

    #[test]
    fn replay_is_rejected_even_after_success() {
        let fixture = Fixture::new(1);
        fixture
            .broker
            .promote(&fixture.request, FaultPoint::None)
            .unwrap();
        let error = fixture
            .broker
            .promote(&fixture.request, FaultPoint::None)
            .unwrap_err();
        assert!(error.to_string().contains("already consumed"));
        let mut changed_authorization = fixture.request.clone();
        changed_authorization.artifact_id = "999".to_owned();
        let error = fixture
            .broker
            .promote(&changed_authorization, FaultPoint::None)
            .unwrap_err();
        assert!(error.to_string().contains("already consumed"));
    }

    #[test]
    fn receipt_rejects_a_valid_signature_over_inaccurate_post_sign_metadata() {
        let fixture = Fixture::new(1);
        fixture
            .broker
            .promote(&fixture.request, FaultPoint::None)
            .unwrap();
        let receipt_path = fixture.output().join(RECEIPT_NAME);
        let mut receipt: SignedReceipt =
            serde_json::from_slice(&fs::read(&receipt_path).unwrap()).unwrap();
        receipt.statement.packs[0].signature_envelope_sha256 = "0".repeat(64);
        let statement = serde_json::to_vec(&receipt.statement).unwrap();
        let mut material = RECEIPT_DOMAIN.to_vec();
        material.extend_from_slice(&statement);
        receipt.signature_hex = encode_hex(fixture_key_pair().unwrap().sign(&material).as_ref());
        fs::write(&receipt_path, serde_json::to_vec(&receipt).unwrap()).unwrap();
        assert!(
            verify_published_set(
                &fixture.output(),
                &fixture.request,
                fixture_key_pair().unwrap().public_key().as_ref(),
            )
            .is_err()
        );
    }

    #[test]
    fn receipt_statement_revalidates_post_sign_manifest_version() {
        let fixture = Fixture::new(1);
        fixture
            .broker
            .promote(&fixture.request, FaultPoint::None)
            .unwrap();
        let public_key = fixture_key_pair().unwrap().public_key().as_ref().to_vec();
        let mut cuda = inspect_signed_pack(&fixture.output().join("cuda"), &public_key).unwrap();
        let vulkan = inspect_signed_pack(&fixture.output().join("vulkan"), &public_key).unwrap();
        cuda.manifest.pack_version = "0.1.0-cross-release".to_owned();
        assert!(
            expected_receipt_statement(
                &fixture.request,
                &[cuda, vulkan],
                &hash_domain(REQUEST_DOMAIN, &fixture.request.canonical_json().unwrap()),
            )
            .is_err()
        );
    }

    #[test]
    fn missing_and_corrupt_ledgers_fail_closed_without_output() {
        let missing = Fixture::new(1);
        fs::remove_file(missing.broker.ledger_path()).unwrap();
        assert!(
            missing
                .broker
                .promote(&missing.request, FaultPoint::None)
                .is_err()
        );
        assert!(!missing.output().exists());

        let corrupt = Fixture::new(1);
        fs::write(corrupt.broker.ledger_path(), b"not-a-ledger\n").unwrap();
        assert!(
            corrupt
                .broker
                .promote(&corrupt.request, FaultPoint::None)
                .is_err()
        );
        assert!(!corrupt.output().exists());
    }

    #[test]
    fn security_epoch_high_water_cannot_regress() {
        let fixture = Fixture::new(2);
        fixture
            .broker
            .promote(&fixture.request, FaultPoint::None)
            .unwrap();
        let rollback = create_request(
            &fixture.handoff_parent,
            &fixture.publication_parent,
            "release-b",
            1,
            "two",
        );
        let error = fixture
            .broker
            .promote(&rollback, FaultPoint::None)
            .unwrap_err();
        assert!(error.to_string().contains("high-water"));
        assert!(!Path::new(&rollback.output_root).exists());
    }

    #[test]
    fn fault_after_first_pack_never_publishes_a_partial_pair_and_burns_replay() {
        let fixture = Fixture::new(1);
        assert!(
            fixture
                .broker
                .promote(&fixture.request, FaultPoint::AfterCudaCopy)
                .is_err()
        );
        assert!(!fixture.output().exists());
        fixture.broker.recover().unwrap();
        assert!(!fixture.output().exists());
        assert!(
            !fixture
                .publication_parent
                .join(format!(".staging-{}", fixture.request.release_set_digest))
                .exists()
        );
        assert!(
            fixture
                .broker
                .promote(&fixture.request, FaultPoint::None)
                .is_err()
        );
    }

    #[test]
    fn ready_and_post_publish_faults_recover_only_complete_pairs() {
        for fault in [FaultPoint::AfterReady, FaultPoint::AfterPublish] {
            let fixture = Fixture::new(1);
            assert!(fixture.broker.promote(&fixture.request, fault).is_err());
            fixture.broker.recover().unwrap();
            verify_fixture_output(
                &fixture.output(),
                &fixture.request.release_set_digest,
                &hash_domain(REQUEST_DOMAIN, &fixture.request.canonical_json().unwrap()),
            )
            .unwrap();
            let mut ledger = open_existing_ledger(&fixture.broker.ledger_path()).unwrap();
            let snapshot = load_ledger(&mut ledger).unwrap();
            assert_eq!(
                snapshot.releases[&fixture.request.release_set_digest].kind,
                LedgerKind::Published
            );
        }
    }

    #[test]
    fn recovery_rejects_a_valid_but_cross_release_output_substitution() {
        let fixture = Fixture::new(1);
        fixture
            .broker
            .promote(&fixture.request, FaultPoint::None)
            .unwrap();
        let second = create_request(
            &fixture.handoff_parent,
            &fixture.publication_parent,
            "release-b",
            1,
            "two",
        );
        assert!(
            fixture
                .broker
                .promote(&second, FaultPoint::AfterReady)
                .is_err()
        );
        let second_stage = fixture
            .publication_parent
            .join(format!(".staging-{}", second.release_set_digest));
        fs::remove_dir_all(&second_stage).unwrap();
        copy_tree_for_substitution_test(&fixture.output(), &second_stage);
        let error = fixture.broker.recover().unwrap_err();
        assert!(error.to_string().contains("does not bind ledger"));
        assert!(!Path::new(&second.output_root).exists());
    }

    #[test]
    fn reservation_fault_is_replay_safe_and_has_no_publication() {
        let fixture = Fixture::new(1);
        assert!(
            fixture
                .broker
                .promote(&fixture.request, FaultPoint::AfterReserve)
                .is_err()
        );
        fixture.broker.recover().unwrap();
        assert!(!fixture.output().exists());
        assert!(
            fixture
                .broker
                .promote(&fixture.request, FaultPoint::None)
                .is_err()
        );
    }

    #[test]
    fn concurrent_duplicate_requests_have_one_winner() {
        let fixture = Fixture::new(1);
        let left_broker = fixture.broker.clone();
        let right_broker = fixture.broker.clone();
        let left_request = fixture.request.clone();
        let right_request = fixture.request.clone();
        let left = thread::spawn(move || left_broker.promote(&left_request, FaultPoint::None));
        let right = thread::spawn(move || right_broker.promote(&right_request, FaultPoint::None));
        let outcomes = [left.join().unwrap(), right.join().unwrap()];
        assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
        verify_fixture_output(
            &fixture.output(),
            &fixture.request.release_set_digest,
            &hash_domain(REQUEST_DOMAIN, &fixture.request.canonical_json().unwrap()),
        )
        .unwrap();
    }

    #[test]
    fn handoff_unknown_fields_and_noncanonical_json_are_rejected_before_reservation() {
        for mode in ["unknown", "whitespace"] {
            let fixture = Fixture::new(1);
            let handoff_path = Path::new(&fixture.request.handoff_root).join(HANDOFF_NAME);
            let bytes = fs::read(&handoff_path).unwrap();
            let rewritten = if mode == "unknown" {
                let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                value
                    .as_object_mut()
                    .unwrap()
                    .insert("private_key".into(), "forbidden".into());
                serde_json::to_vec(&value).unwrap()
            } else {
                let mut value = bytes;
                value.push(b'\n');
                value
            };
            fs::write(&handoff_path, &rewritten).unwrap();
            let mut request = fixture.request.clone();
            request.handoff_sha256 = encode_hex(&Sha256::digest(&rewritten));
            assert!(fixture.broker.promote(&request, FaultPoint::None).is_err());
            assert!(!fixture.output().exists());
            let mut ledger = open_existing_ledger(&fixture.broker.ledger_path()).unwrap();
            assert_eq!(load_ledger(&mut ledger).unwrap().next_sequence, 1);
        }
    }

    #[test]
    fn malformed_or_tampered_pack_is_never_signed_or_published() {
        let fixture = Fixture::new(1);
        let worker =
            Path::new(&fixture.request.handoff_root).join("cuda/bin/scribe-inference-worker.exe");
        fs::write(worker, b"tampered").unwrap();
        assert!(
            fixture
                .broker
                .promote(&fixture.request, FaultPoint::None)
                .is_err()
        );
        assert!(!fixture.output().exists());
        let mut ledger = open_existing_ledger(&fixture.broker.ledger_path()).unwrap();
        assert_eq!(load_ledger(&mut ledger).unwrap().next_sequence, 1);
    }

    #[test]
    fn hardlinked_payload_is_rejected_before_signing() {
        let fixture = Fixture::new(1);
        let worker =
            Path::new(&fixture.request.handoff_root).join("cuda/bin/scribe-inference-worker.exe");
        let external = fixture.handoff_parent.join("external-worker.exe");
        fs::rename(&worker, &external).unwrap();
        fs::hard_link(&external, &worker).unwrap();
        assert!(
            fixture
                .broker
                .promote(&fixture.request, FaultPoint::None)
                .is_err()
        );
        assert!(!fixture.output().exists());
    }

    #[cfg(windows)]
    #[test]
    fn alternate_data_stream_is_rejected_before_signing() {
        let fixture = Fixture::new(1);
        let worker =
            Path::new(&fixture.request.handoff_root).join("cuda/bin/scribe-inference-worker.exe");
        fs::write(format!("{}:hostile", worker.display()), b"hidden").unwrap();
        assert!(
            fixture
                .broker
                .promote(&fixture.request, FaultPoint::None)
                .is_err()
        );
        assert!(!fixture.output().exists());
    }

    #[cfg(windows)]
    #[test]
    fn retained_read_handle_denies_source_replacement_during_copy() {
        let fixture = Fixture::new(1);
        let worker =
            Path::new(&fixture.request.handoff_root).join("cuda/bin/scribe-inference-worker.exe");
        let retained = open_regular_no_follow(&worker).unwrap();
        let rewrite = OpenOptions::new().write(true).truncate(true).open(&worker);
        assert!(rewrite.is_err());
        let replacement = worker.with_extension("replacement");
        fs::write(&replacement, b"replacement").unwrap();
        assert!(fs::rename(&replacement, &worker).is_err());
        drop(retained);
    }

    #[test]
    fn unexpected_inventory_and_case_colliding_manifest_are_rejected() {
        let fixture = Fixture::new(1);
        fs::write(
            Path::new(&fixture.request.handoff_root).join("cuda/unexpected.dll"),
            b"unexpected",
        )
        .unwrap();
        assert!(
            fixture
                .broker
                .promote(&fixture.request, FaultPoint::None)
                .is_err()
        );

        let second = Fixture::new(1);
        let manifest_path = Path::new(&second.request.handoff_root).join("cuda/pack-manifest.json");
        let mut manifest: PackManifest =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest.payload.push(PayloadEntry {
            path: "BIN/scribe-inference-worker.exe".to_owned(),
            size_bytes: manifest.payload[0].size_bytes,
            sha256: manifest.payload[0].sha256.clone(),
        });
        fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let bytes = fs::read(Path::new(&second.request.handoff_root).join(HANDOFF_NAME)).unwrap();
        let mut request = second.request.clone();
        request.handoff_sha256 = encode_hex(&Sha256::digest(bytes));
        assert!(second.broker.promote(&request, FaultPoint::None).is_err());
    }

    #[test]
    fn unexpected_entry_flood_is_rejected_at_the_bounded_enumerator() {
        let fixture = Fixture::new(1);
        for index in 0..64 {
            fs::write(
                Path::new(&fixture.request.handoff_root).join(format!("unexpected-{index:02}")),
                b"unexpected",
            )
            .unwrap();
        }
        assert!(bounded_directory_names(Path::new(&fixture.request.handoff_root), 3).is_err());
        assert!(
            fixture
                .broker
                .promote(&fixture.request, FaultPoint::None)
                .is_err()
        );
        assert!(!fixture.output().exists());
        let mut ledger = open_existing_ledger(&fixture.broker.ledger_path()).unwrap();
        assert_eq!(load_ledger(&mut ledger).unwrap().next_sequence, 1);
    }

    #[test]
    fn signed_pack_and_public_entry_floods_are_bounded() {
        let pack_flood = Fixture::new(1);
        pack_flood
            .broker
            .promote(&pack_flood.request, FaultPoint::None)
            .unwrap();
        for index in 0..64 {
            fs::write(
                pack_flood
                    .output()
                    .join(format!("cuda/unexpected-{index:02}.dll")),
                b"unexpected",
            )
            .unwrap();
        }
        assert!(
            verify_fixture_output(
                &pack_flood.output(),
                &pack_flood.request.release_set_digest,
                &hash_domain(
                    REQUEST_DOMAIN,
                    &pack_flood.request.canonical_json().unwrap(),
                ),
            )
            .is_err()
        );

        let public_flood = Fixture::new(1);
        public_flood
            .broker
            .promote(&public_flood.request, FaultPoint::None)
            .unwrap();
        for index in 0..64 {
            fs::write(
                public_flood
                    .output()
                    .join(format!("unexpected-{index:02}.txt")),
                b"unexpected",
            )
            .unwrap();
        }
        assert!(bounded_directory_names(&public_flood.output(), 3).is_err());
    }

    #[test]
    fn traversal_and_oversized_inventory_are_rejected_by_manifest_policy() {
        let fixture = Fixture::new(1);
        let manifest_path =
            Path::new(&fixture.request.handoff_root).join("cuda/pack-manifest.json");
        let manifest: PackManifest =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        let mut traversal = manifest.clone();
        traversal.payload[0].path = "../escape.exe".to_owned();
        assert!(validate_manifest(&traversal, &fixture.request).is_err());

        let mut oversized = manifest;
        oversized.payload = (0..=MAX_FILES)
            .map(|index| PayloadEntry {
                path: format!("bin/file-{index:03}.dll"),
                size_bytes: 1,
                sha256: "f".repeat(64),
            })
            .collect();
        assert!(validate_manifest(&oversized, &fixture.request).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn symlink_or_reparse_payload_is_rejected_when_platform_allows_fixture_creation() {
        use std::os::windows::fs::symlink_file;

        let fixture = Fixture::new(1);
        let worker =
            Path::new(&fixture.request.handoff_root).join("cuda/bin/scribe-inference-worker.exe");
        let external = fixture.handoff_parent.join("external-symlink-target.exe");
        fs::write(&external, fs::read(&worker).unwrap()).unwrap();
        fs::remove_file(&worker).unwrap();
        if let Err(error) = symlink_file(&external, &worker) {
            if error.raw_os_error() == Some(1314) {
                return;
            }
            panic!("could not create reparse fixture: {error}");
        }
        assert!(
            fixture
                .broker
                .promote(&fixture.request, FaultPoint::None)
                .is_err()
        );
        assert!(!fixture.output().exists());
    }

    #[test]
    fn corrupted_hash_chain_and_torn_append_fail_closed() {
        for suffix in [b"{}\n".as_slice(), b"torn".as_slice()] {
            let fixture = Fixture::new(1);
            let mut ledger = OpenOptions::new()
                .append(true)
                .open(fixture.broker.ledger_path())
                .unwrap();
            ledger.write_all(suffix).unwrap();
            ledger.sync_all().unwrap();
            drop(ledger);
            assert!(
                fixture
                    .broker
                    .promote(&fixture.request, FaultPoint::None)
                    .is_err()
            );
            assert!(!fixture.output().exists());
        }
    }

    #[cfg(windows)]
    #[test]
    fn equivalent_windows_publication_parent_spelling_resolves_to_canonical_root() {
        let mut fixture = Fixture::new(1);
        let canonical_parent = fs::canonicalize(&fixture.publication_parent).unwrap();
        assert_ne!(canonical_parent, fixture.publication_parent);
        let output_name = Path::new(&fixture.request.output_root)
            .file_name()
            .unwrap()
            .to_owned();
        fixture.request.output_root = canonical_parent
            .join(&output_name)
            .to_string_lossy()
            .into_owned();

        let (_, resolved_output, resolved_name) =
            fixture.broker.resolve_roots(&fixture.request).unwrap();
        assert_eq!(resolved_output, canonical_parent.join(&output_name));
        assert_eq!(resolved_name, output_name.to_string_lossy());
        fixture
            .broker
            .promote(&fixture.request, FaultPoint::None)
            .unwrap();
        assert!(resolved_output.exists());
    }

    #[cfg(windows)]
    #[test]
    fn consumes_canonical_handoff_generated_by_powershell_and_worker_pack_author() {
        let temp = tempfile::tempdir().unwrap();
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .unwrap();
        let intake = temp.path().join("interop-intake");
        let publication = temp.path().join("interop-publication");
        let result = Command::new("pwsh")
            .args([
                "-NoProfile",
                "-File",
                repository
                    .join("scripts/test-windows-gpu-pack-promotion.ps1")
                    .to_str()
                    .unwrap(),
                "-InteropFixtureDirectory",
                intake.to_str().unwrap(),
                "-InteropPublicationDirectory",
                publication.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "PowerShell producer failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        let request_bytes = fs::read(intake.join("promotion-request.json")).unwrap();
        let request: PromotionRequest = serde_json::from_slice(&request_bytes).unwrap();
        assert_eq!(request.canonical_json().unwrap(), request_bytes);
        let broker = FixtureBroker::initialize(
            intake.clone(),
            publication.clone(),
            temp.path().join("interop-state"),
        )
        .unwrap();
        broker.promote(&request, FaultPoint::None).unwrap();
        verify_fixture_output(
            Path::new(&request.output_root),
            &request.release_set_digest,
            &hash_domain(REQUEST_DOMAIN, &request.canonical_json().unwrap()),
        )
        .unwrap();
    }
}
