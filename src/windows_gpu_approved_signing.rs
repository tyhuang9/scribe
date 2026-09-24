//! GPU-free, policy-bound signing of one approved Windows CUDA/Vulkan pair.
//!
//! The candidate handoff is treated only as data. No worker or candidate code
//! is loaded while the production key is available.

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::manifest::{
    EMBEDDED_MINIMUM_SECURITY_EPOCH, MANIFEST_NAME, ProductionTrustRoot, SIGNATURE_NAME, TrustRoot,
    is_link_or_reparse, open_regular_no_follow, reject_hardlink, reject_named_streams,
    validate_build_identity, validate_identifier, validate_relative_path, validate_root,
};
use crate::worker_pack_authoring::{
    ApprovedPackTarget, AuthoringBackend, MAX_PRIVATE_KEY_BYTES, PreparedPack,
    inspect_approved_prepared_pack, sign_approved_prepared_pack,
};

pub(crate) const HANDOFF_NAME: &str = "windows-gpu-pack-handoff.json";
pub(crate) const RECEIPT_NAME: &str = "windows-gpu-pack-signing-receipt.json";
const CONTROL_SCHEMA_VERSION: u16 = 1;
const MAX_CONTROL_BYTES: u64 = 256 * 1024;
const RELEASE_SET_DOMAIN: &[u8] = b"scribe-windows-gpu-release-set-v1\0";
const WINDOWS_TARGET_OS: &str = "windows";
const WINDOWS_TARGET_ARCH: &str = "x86_64";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ApprovedWindowsPackSet {
    pub(crate) schema_version: u16,
    pub(crate) policy_sha256: String,
    pub(crate) signer_source_revision: String,
    pub(crate) signer_sha256: String,
    pub(crate) source_repository: String,
    pub(crate) source_ref: String,
    pub(crate) source_revision: String,
    pub(crate) workflow_ref: String,
    pub(crate) run_id: String,
    pub(crate) run_attempt: String,
    pub(crate) artifact_id: String,
    pub(crate) artifact_digest: String,
    pub(crate) handoff_sha256: String,
    pub(crate) release_set_digest: String,
    pub(crate) toolchain_manifest_sha256: String,
    pub(crate) pack_version: String,
    pub(crate) packs: Vec<ApprovedPack>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ApprovedPack {
    pub(crate) backend: String,
    pub(crate) pack_root: String,
    pub(crate) pack_id: String,
    pub(crate) pack_version: String,
    pub(crate) pack_digest: String,
    pub(crate) security_epoch: u64,
    pub(crate) provider: String,
    pub(crate) manifest_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WindowsSigningPolicy {
    pub(crate) schema_version: u16,
    pub(crate) policy_id: String,
    pub(crate) key_id: String,
    pub(crate) public_key_sha256: String,
    pub(crate) source_repository: String,
    pub(crate) source_ref: String,
    pub(crate) app_version: String,
    pub(crate) protocol_version: u16,
    pub(crate) worker_abi_version: u16,
    pub(crate) minimum_security_epoch: u64,
    pub(crate) packs: Vec<PolicyPack>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PolicyPack {
    pub(crate) backend: String,
    pub(crate) pack_id: String,
    pub(crate) provider: String,
    pub(crate) worker_path: String,
    pub(crate) security_epoch: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WindowsPackHandoff {
    schema_version: u16,
    source_repository: String,
    source_ref: String,
    source_revision: String,
    workflow_ref: String,
    run_id: String,
    run_attempt: String,
    pack_version: String,
    toolchain_manifest_sha256: String,
    packs: Vec<ApprovedPack>,
    release_set_digest: String,
}

#[derive(Serialize)]
struct ReleaseSetMaterial<'a> {
    schema_version: u16,
    source_repository: &'a str,
    source_ref: &'a str,
    source_revision: &'a str,
    workflow_ref: &'a str,
    run_id: &'a str,
    run_attempt: &'a str,
    pack_version: &'a str,
    toolchain_manifest_sha256: &'a str,
    packs: &'a [ApprovedPack],
}

#[derive(Clone, Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InspectedWindowsPackSet {
    pub(crate) schema_version: u16,
    pub(crate) policy_id: String,
    pub(crate) policy_sha256: String,
    pub(crate) key_id: String,
    pub(crate) source_revision: String,
    pub(crate) release_set_digest: String,
    pub(crate) pack_version: String,
    pub(crate) signer_source_revision: String,
    pub(crate) signer_sha256: String,
    pub(crate) packs: Vec<InspectedPack>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct InspectedPack {
    pub(crate) backend: String,
    pub(crate) pack_root: String,
    pub(crate) pack_id: String,
    pub(crate) pack_version: String,
    pub(crate) pack_digest: String,
    pub(crate) manifest_sha256: String,
    pub(crate) security_epoch: u64,
    pub(crate) provider: String,
    pub(crate) payload_files: usize,
    pub(crate) installed_payload_bytes: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct WindowsPackSigningReceipt {
    pub(crate) schema_version: u16,
    pub(crate) policy_id: String,
    pub(crate) policy_sha256: String,
    pub(crate) key_id: String,
    pub(crate) signer_source_revision: String,
    pub(crate) signer_sha256: String,
    pub(crate) source_repository: String,
    pub(crate) source_ref: String,
    pub(crate) source_revision: String,
    pub(crate) workflow_ref: String,
    pub(crate) run_id: String,
    pub(crate) run_attempt: String,
    pub(crate) artifact_id: String,
    pub(crate) artifact_digest: String,
    pub(crate) handoff_sha256: String,
    pub(crate) release_set_digest: String,
    pub(crate) toolchain_manifest_sha256: String,
    pub(crate) pack_version: String,
    pub(crate) packs: Vec<InspectedPack>,
}

struct ValidatedSet {
    approval: ApprovedWindowsPackSet,
    policy: WindowsSigningPolicy,
    policy_sha256: String,
    app_build: String,
    worker_build: String,
    inspected: Vec<InspectedPack>,
}

pub(crate) fn inspect_approved_windows_set(
    handoff_root: &Path,
    approval_path: &Path,
    policy_path: &Path,
) -> Result<InspectedWindowsPackSet> {
    inspect_with_trust(
        handoff_root,
        approval_path,
        policy_path,
        &ProductionTrustRoot,
    )
}

pub(crate) fn sign_approved_windows_set(
    handoff_root: &Path,
    approval_path: &Path,
    policy_path: &Path,
    output_root: &Path,
    private_key_input: &mut dyn Read,
) -> Result<WindowsPackSigningReceipt> {
    sign_with_trust(
        handoff_root,
        approval_path,
        policy_path,
        output_root,
        private_key_input,
        &ProductionTrustRoot,
    )
}

fn inspect_with_trust(
    handoff_root: &Path,
    approval_path: &Path,
    policy_path: &Path,
    trust: &dyn TrustRoot,
) -> Result<InspectedWindowsPackSet> {
    let validated = validate_set(handoff_root, approval_path, policy_path, trust)?;
    Ok(inspected_descriptor(&validated))
}

fn sign_with_trust(
    handoff_root: &Path,
    approval_path: &Path,
    policy_path: &Path,
    output_root: &Path,
    private_key_input: &mut dyn Read,
    trust: &dyn TrustRoot,
) -> Result<WindowsPackSigningReceipt> {
    if output_root.exists() {
        bail!("signed output root must be fresh");
    }
    let output_parent = output_root
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or_else(|| anyhow!("signed output root must have an existing parent"))?;
    validate_root(output_parent).context("signed output parent is unsafe")?;

    // This complete validation occurs before the secret input is read.
    let validated = validate_set(handoff_root, approval_path, policy_path, trust)?;
    let private_key = read_bounded_private_key(private_key_input)?;

    let staging_path = fresh_staging_path(output_parent)?;
    fs::create_dir(&staging_path).context("could not create signed-set staging directory")?;
    let mut staging = StagingDirectory(Some(staging_path));

    for (approved, rule) in validated.approval.packs.iter().zip(&validated.policy.packs) {
        let source = handoff_root.join(&approved.pack_root);
        let destination = staging.path().join(&approved.pack_root);
        copy_prepared_tree(&source, &destination)?;
        let target = approved_target(
            rule,
            &validated.policy,
            &validated.app_build,
            &validated.worker_build,
        )?;
        let copied = inspect_approved_prepared_pack(&destination, &target)?;
        if copied.manifest_sha256 != approved.manifest_sha256
            || copied.pack_digest != approved.pack_digest
        {
            bail!("copied prepared pack changed before signing");
        }
        sign_approved_prepared_pack(
            &destination,
            &target,
            trust,
            &validated.policy.key_id,
            &private_key,
            &approved.manifest_sha256,
            &approved.pack_digest,
        )?;
    }

    let receipt = receipt_from_validated(&validated);
    write_new_file(
        &staging.path().join(RECEIPT_NAME),
        &serde_json::to_vec(&receipt)?,
    )?;
    validate_signed_staging(staging.path(), &validated, trust)?;
    if output_root.exists() {
        bail!("signed output root appeared before atomic publication");
    }
    fs::rename(staging.path(), output_root)
        .context("could not atomically publish signed pack set")?;
    staging.0 = None;
    Ok(receipt)
}

fn validate_set(
    handoff_root: &Path,
    approval_path: &Path,
    policy_path: &Path,
    trust: &dyn TrustRoot,
) -> Result<ValidatedSet> {
    validate_handoff_root(handoff_root)?;
    let approval_bytes = read_bounded_control(approval_path, "approval")?;
    let policy_bytes = read_bounded_control(policy_path, "signing policy")?;
    let handoff_path = handoff_root.join(HANDOFF_NAME);
    let handoff_bytes = read_bounded_control(&handoff_path, "handoff")?;
    let approval: ApprovedWindowsPackSet =
        serde_json::from_slice(&approval_bytes).context("approved pack-set JSON is invalid")?;
    let policy: WindowsSigningPolicy =
        serde_json::from_slice(&policy_bytes).context("Windows signing policy JSON is invalid")?;
    let handoff: WindowsPackHandoff =
        serde_json::from_slice(&handoff_bytes).context("Windows pack handoff JSON is invalid")?;
    if serde_json::to_vec(&handoff)? != handoff_bytes {
        bail!("Windows pack handoff is not canonical JSON");
    }
    let policy_sha256 = sha256_hex(&policy_bytes);
    validate_policy(&policy, trust)?;
    validate_approval(&approval, &policy, &policy_sha256)?;
    validate_handoff(&handoff, &approval, &handoff_bytes)?;

    let app_build = format!(
        "local-transcriber@{}#{}",
        policy.app_version, approval.source_revision
    );
    let worker_build = format!(
        "scribe-inference-worker@{}#{}",
        policy.app_version, approval.source_revision
    );
    validate_build_identity(&app_build, "approved app build")?;
    validate_build_identity(&worker_build, "approved worker build")?;

    let mut inspected = Vec::with_capacity(2);
    for (approved, rule) in approval.packs.iter().zip(&policy.packs) {
        let target = approved_target(rule, &policy, &app_build, &worker_build)?;
        let prepared =
            inspect_approved_prepared_pack(&handoff_root.join(&approved.pack_root), &target)?;
        compare_prepared_to_approval(&prepared, approved)?;
        inspected.push(inspected_pack(approved, &prepared));
    }
    Ok(ValidatedSet {
        approval,
        policy,
        policy_sha256,
        app_build,
        worker_build,
        inspected,
    })
}

fn validate_policy(policy: &WindowsSigningPolicy, trust: &dyn TrustRoot) -> Result<()> {
    if policy.schema_version != CONTROL_SCHEMA_VERSION {
        bail!("unsupported Windows signing policy schema");
    }
    validate_identifier(&policy.policy_id, "signing policy ID")?;
    validate_identifier(&policy.key_id, "signing key ID")?;
    validate_sha256(&policy.public_key_sha256, "policy public-key SHA-256")?;
    validate_ascii_text(&policy.source_repository, "policy source repository", 192)?;
    validate_ascii_text(&policy.source_ref, "policy source ref", 192)?;
    validate_store_text(&policy.app_version, "policy app version")?;
    if policy.protocol_version == 0 || policy.worker_abi_version == 0 {
        bail!("policy protocol and worker ABI versions must be positive");
    }
    if policy.minimum_security_epoch < EMBEDDED_MINIMUM_SECURITY_EPOCH {
        bail!("policy minimum security epoch is below the embedded floor");
    }
    let public_key = TrustRoot::public_key(trust, &policy.key_id).ok_or_else(|| {
        anyhow!("signing policy key has no separately reviewed production trust entry")
    })?;
    if sha256_hex(public_key) != policy.public_key_sha256 {
        bail!("signing policy public-key digest does not match production trust");
    }
    validate_exact_pack_order(
        &policy
            .packs
            .iter()
            .map(|pack| (pack.backend.as_str(), pack.pack_id.as_str()))
            .collect::<Vec<_>>(),
    )?;
    for rule in &policy.packs {
        validate_identifier(&rule.pack_id, "policy pack ID")?;
        validate_identifier(&rule.provider, "policy provider")?;
        validate_relative_path(&rule.worker_path)?;
        if rule.worker_path != "bin/scribe-inference-worker.exe" {
            bail!("policy worker path is not the reviewed Windows worker path");
        }
        if rule.security_epoch < policy.minimum_security_epoch {
            bail!("policy pack security epoch is below the policy floor");
        }
    }
    Ok(())
}

fn validate_approval(
    approval: &ApprovedWindowsPackSet,
    policy: &WindowsSigningPolicy,
    policy_sha256: &str,
) -> Result<()> {
    if approval.schema_version != CONTROL_SCHEMA_VERSION {
        bail!("unsupported approved pack-set schema");
    }
    if approval.policy_sha256 != policy_sha256 {
        bail!("approval does not bind the exact reviewed signing policy bytes");
    }
    validate_revision(&approval.signer_source_revision, "signer source revision")?;
    validate_sha256(&approval.signer_sha256, "signer executable SHA-256")?;
    validate_revision(&approval.source_revision, "candidate source revision")?;
    validate_ascii_text(&approval.source_repository, "source repository", 192)?;
    validate_ascii_text(&approval.source_ref, "source ref", 192)?;
    validate_ascii_text(&approval.workflow_ref, "workflow ref", 384)?;
    validate_positive_decimal(&approval.run_id, "run ID")?;
    validate_positive_decimal(&approval.run_attempt, "run attempt")?;
    validate_positive_decimal(&approval.artifact_id, "artifact ID")?;
    validate_sha256(&approval.artifact_digest, "artifact digest")?;
    validate_sha256(&approval.handoff_sha256, "handoff SHA-256")?;
    validate_sha256(&approval.release_set_digest, "release-set digest")?;
    validate_sha256(
        &approval.toolchain_manifest_sha256,
        "toolchain manifest SHA-256",
    )?;
    validate_store_text(&approval.pack_version, "pack version")?;
    if approval.source_repository != policy.source_repository
        || approval.source_ref != policy.source_ref
    {
        bail!("approval source does not match reviewed signing policy");
    }
    validate_exact_pack_order(
        &approval
            .packs
            .iter()
            .map(|pack| (pack.backend.as_str(), pack.pack_id.as_str()))
            .collect::<Vec<_>>(),
    )?;
    for ((approved, rule), expected_root) in approval
        .packs
        .iter()
        .zip(&policy.packs)
        .zip(["cuda", "vulkan"])
    {
        if approved.pack_root != expected_root
            || approved.backend != rule.backend
            || approved.pack_id != rule.pack_id
            || approved.provider != rule.provider
            || approved.security_epoch != rule.security_epoch
            || approved.pack_version != approval.pack_version
        {
            bail!("approval pack identity does not match reviewed signing policy");
        }
        validate_store_text(&approved.pack_version, "approved pack version")?;
        validate_sha256(&approved.pack_digest, "approved pack digest")?;
        validate_sha256(&approved.manifest_sha256, "approved manifest SHA-256")?;
    }
    Ok(())
}

fn validate_handoff(
    handoff: &WindowsPackHandoff,
    approval: &ApprovedWindowsPackSet,
    handoff_bytes: &[u8],
) -> Result<()> {
    if handoff.schema_version != CONTROL_SCHEMA_VERSION
        || handoff.source_repository != approval.source_repository
        || handoff.source_ref != approval.source_ref
        || handoff.source_revision != approval.source_revision
        || handoff.workflow_ref != approval.workflow_ref
        || handoff.run_id != approval.run_id
        || handoff.run_attempt != approval.run_attempt
        || handoff.pack_version != approval.pack_version
        || handoff.toolchain_manifest_sha256 != approval.toolchain_manifest_sha256
        || handoff.packs != approval.packs
        || handoff.release_set_digest != approval.release_set_digest
    {
        bail!("canonical handoff does not match the approved release inputs");
    }
    if sha256_hex(handoff_bytes) != approval.handoff_sha256 {
        bail!("canonical handoff bytes do not match the approved digest");
    }
    let material = ReleaseSetMaterial {
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
    let mut hasher = Sha256::new();
    hasher.update(RELEASE_SET_DOMAIN);
    hasher.update(serde_json::to_vec(&material)?);
    if encode_hex(&hasher.finalize()) != handoff.release_set_digest {
        bail!("canonical handoff release-set digest is invalid");
    }
    Ok(())
}

fn approved_target<'a>(
    rule: &'a PolicyPack,
    policy: &'a WindowsSigningPolicy,
    app_build: &'a str,
    worker_build: &'a str,
) -> Result<ApprovedPackTarget<'a>> {
    let backend = AuthoringBackend::parse(&rule.backend)
        .ok_or_else(|| anyhow!("policy backend is unsupported"))?;
    Ok(ApprovedPackTarget {
        pack_id: &rule.pack_id,
        security_epoch: rule.security_epoch,
        backend,
        provider: &rule.provider,
        target_os: WINDOWS_TARGET_OS,
        target_arch: WINDOWS_TARGET_ARCH,
        worker_path: &rule.worker_path,
        app_protocol_version: policy.protocol_version,
        runtime_abi_version: policy.worker_abi_version,
        app_build,
        worker_build,
    })
}

fn compare_prepared_to_approval(prepared: &PreparedPack, approved: &ApprovedPack) -> Result<()> {
    if prepared.pack_id != approved.pack_id
        || prepared.pack_version != approved.pack_version
        || prepared.pack_digest != approved.pack_digest
        || prepared.security_epoch != approved.security_epoch
        || prepared.backend != approved.backend
        || prepared.provider != approved.provider
        || prepared.target_os != WINDOWS_TARGET_OS
        || prepared.target_arch != WINDOWS_TARGET_ARCH
        || prepared.manifest_sha256 != approved.manifest_sha256
    {
        bail!("prepared pack does not match the approved pack descriptor");
    }
    Ok(())
}

fn validate_handoff_root(root: &Path) -> Result<()> {
    validate_root(root)?;
    let mut names = BTreeSet::new();
    for entry in fs::read_dir(root).context("could not enumerate handoff root")? {
        let entry = entry?;
        let name = entry
            .file_name()
            .to_str()
            .ok_or_else(|| anyhow!("handoff root entry is not UTF-8"))?
            .to_owned();
        let metadata = fs::symlink_metadata(entry.path())?;
        if is_link_or_reparse(&metadata) {
            bail!("handoff root contains a link or reparse point");
        }
        match name.as_str() {
            HANDOFF_NAME if metadata.is_file() => {}
            "cuda" | "vulkan" if metadata.is_dir() => {}
            _ => bail!("handoff root contains an unexpected entry"),
        }
        if !names.insert(name.to_ascii_lowercase()) {
            bail!("handoff root contains a case-colliding entry");
        }
    }
    let expected = [HANDOFF_NAME, "cuda", "vulkan"]
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    if names != expected {
        bail!("handoff root is incomplete");
    }
    Ok(())
}

fn validate_exact_pack_order(packs: &[(&str, &str)]) -> Result<()> {
    let expected = [
        ("cuda", "scribe-cuda-windows-x64"),
        ("vulkan", "scribe-vulkan-windows-x64"),
    ];
    if packs != expected {
        bail!("Windows signing policy requires the exact ordered CUDA/Vulkan pair");
    }
    Ok(())
}

fn validate_signed_staging(
    root: &Path,
    validated: &ValidatedSet,
    trust: &dyn TrustRoot,
) -> Result<()> {
    let mut names = fs::read_dir(root)?
        .map(|entry| {
            entry?
                .file_name()
                .into_string()
                .map_err(|_| std::io::Error::other("non-UTF-8 signed-set entry"))
        })
        .collect::<std::io::Result<Vec<_>>>()?;
    names.sort();
    if names != ["cuda", "vulkan", RECEIPT_NAME] {
        bail!("signed staging set has an unexpected inventory");
    }
    for (approved, rule) in validated.approval.packs.iter().zip(&validated.policy.packs) {
        let target = approved_target(
            rule,
            &validated.policy,
            &validated.app_build,
            &validated.worker_build,
        )?;
        let allowed = [target.backend.manifest_backend()];
        let verifier = crate::manifest::PackVerifier::new(
            trust,
            crate::manifest::Compatibility {
                app_build: target.app_build,
                worker_build: target.worker_build,
                target_os: WINDOWS_TARGET_OS,
                target_arch: WINDOWS_TARGET_ARCH,
                allowed_backends: &allowed,
            },
        );
        let verified = verifier.verify(&root.join(&approved.pack_root))?;
        if verified.pack_digest != approved.pack_digest
            || verified.security_epoch != approved.security_epoch
        {
            bail!("signed pack failed final identity verification");
        }
    }
    let receipt = read_bounded_control(&root.join(RECEIPT_NAME), "signing receipt")?;
    if receipt != serde_json::to_vec(&receipt_from_validated(validated))? {
        bail!("signed-set receipt changed before publication");
    }
    Ok(())
}

fn inspected_descriptor(validated: &ValidatedSet) -> InspectedWindowsPackSet {
    InspectedWindowsPackSet {
        schema_version: CONTROL_SCHEMA_VERSION,
        policy_id: validated.policy.policy_id.clone(),
        policy_sha256: validated.policy_sha256.clone(),
        key_id: validated.policy.key_id.clone(),
        source_revision: validated.approval.source_revision.clone(),
        release_set_digest: validated.approval.release_set_digest.clone(),
        pack_version: validated.approval.pack_version.clone(),
        signer_source_revision: validated.approval.signer_source_revision.clone(),
        signer_sha256: validated.approval.signer_sha256.clone(),
        packs: validated.inspected.clone(),
    }
}

fn receipt_from_validated(validated: &ValidatedSet) -> WindowsPackSigningReceipt {
    let approval = &validated.approval;
    WindowsPackSigningReceipt {
        schema_version: CONTROL_SCHEMA_VERSION,
        policy_id: validated.policy.policy_id.clone(),
        policy_sha256: validated.policy_sha256.clone(),
        key_id: validated.policy.key_id.clone(),
        signer_source_revision: approval.signer_source_revision.clone(),
        signer_sha256: approval.signer_sha256.clone(),
        source_repository: approval.source_repository.clone(),
        source_ref: approval.source_ref.clone(),
        source_revision: approval.source_revision.clone(),
        workflow_ref: approval.workflow_ref.clone(),
        run_id: approval.run_id.clone(),
        run_attempt: approval.run_attempt.clone(),
        artifact_id: approval.artifact_id.clone(),
        artifact_digest: approval.artifact_digest.clone(),
        handoff_sha256: approval.handoff_sha256.clone(),
        release_set_digest: approval.release_set_digest.clone(),
        toolchain_manifest_sha256: approval.toolchain_manifest_sha256.clone(),
        pack_version: approval.pack_version.clone(),
        packs: validated.inspected.clone(),
    }
}

fn inspected_pack(approved: &ApprovedPack, prepared: &PreparedPack) -> InspectedPack {
    InspectedPack {
        backend: approved.backend.clone(),
        pack_root: approved.pack_root.clone(),
        pack_id: approved.pack_id.clone(),
        pack_version: approved.pack_version.clone(),
        pack_digest: approved.pack_digest.clone(),
        manifest_sha256: approved.manifest_sha256.clone(),
        security_epoch: approved.security_epoch,
        provider: approved.provider.clone(),
        payload_files: prepared.payload_files,
        installed_payload_bytes: prepared.installed_payload_bytes,
    }
}

fn read_bounded_control(path: &Path, label: &str) -> Result<Vec<u8>> {
    let mut file =
        open_regular_no_follow(path).with_context(|| format!("could not open {label}"))?;
    let metadata = file
        .metadata()
        .with_context(|| format!("could not inspect {label}"))?;
    reject_hardlink(&file, &metadata, path)?;
    reject_named_streams(path)?;
    if metadata.len() == 0 || metadata.len() > MAX_CONTROL_BYTES {
        bail!("{label} size is outside the accepted bound");
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    Read::by_ref(&mut file)
        .take(MAX_CONTROL_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("could not read {label}"))?;
    if bytes.len() as u64 != metadata.len() {
        bail!("{label} changed while it was read");
    }
    Ok(bytes)
}

fn read_bounded_private_key(input: &mut dyn Read) -> Result<Vec<u8>> {
    let mut private_key = Vec::new();
    input
        .take(MAX_PRIVATE_KEY_BYTES + 1)
        .read_to_end(&mut private_key)
        .context("could not read bounded signing key input")?;
    if private_key.is_empty() || private_key.len() as u64 > MAX_PRIVATE_KEY_BYTES {
        bail!("production signing key size is outside the accepted bound");
    }
    Ok(private_key)
}

fn copy_prepared_tree(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir(destination).context("could not create staged pack root")?;
    copy_directory(source, destination, source, 0)
}

fn copy_directory(
    source_root: &Path,
    destination_root: &Path,
    source: &Path,
    depth: usize,
) -> Result<()> {
    if depth > 12 {
        bail!("prepared pack exceeds the maximum directory depth while copying");
    }
    for entry in fs::read_dir(source).context("could not enumerate prepared pack for copying")? {
        let entry = entry?;
        let source_path = entry.path();
        let metadata = fs::symlink_metadata(&source_path)?;
        if is_link_or_reparse(&metadata) {
            bail!("prepared pack changed to a link or reparse point while copying");
        }
        let relative = source_path
            .strip_prefix(source_root)
            .map_err(|_| anyhow!("prepared pack path escaped while copying"))?;
        let relative_text = relative
            .to_str()
            .ok_or_else(|| anyhow!("prepared pack path is not UTF-8"))?
            .replace('\\', "/");
        if metadata.is_dir() {
            validate_relative_path(&format!("{relative_text}/placeholder"))?;
            let destination = destination_root.join(relative);
            fs::create_dir(&destination)?;
            copy_directory(source_root, destination_root, &source_path, depth + 1)?;
        } else if metadata.is_file() {
            if !matches!(relative_text.as_str(), MANIFEST_NAME) {
                validate_relative_path(&relative_text)?;
            }
            if relative_text == SIGNATURE_NAME {
                bail!("prepared pack unexpectedly contains a signature");
            }
            let mut input = open_regular_no_follow(&source_path)?;
            let opened = input.metadata()?;
            reject_hardlink(&input, &opened, &source_path)?;
            reject_named_streams(&source_path)?;
            if opened.len() != metadata.len() {
                bail!("prepared pack changed while copying");
            }
            let destination = destination_root.join(relative);
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&destination)?;
            let copied = std::io::copy(&mut input, &mut output)?;
            if copied != opened.len() {
                bail!("prepared pack changed length while copying");
            }
            output.sync_all()?;
        } else {
            bail!("prepared pack contains a nonregular entry while copying");
        }
    }
    Ok(())
}

fn fresh_staging_path(parent: &Path) -> Result<PathBuf> {
    for _ in 0..8 {
        let mut nonce = [0_u8; 16];
        getrandom::fill(&mut nonce).map_err(|_| anyhow!("could not generate staging nonce"))?;
        let path = parent.join(format!(
            ".scribe-windows-gpu-signing-{}",
            encode_hex(&nonce)
        ));
        if !path.exists() {
            return Ok(path);
        }
    }
    bail!("could not allocate a fresh signed-set staging path")
}

fn write_new_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn validate_positive_decimal(value: &str, label: &str) -> Result<()> {
    let number = value
        .parse::<u64>()
        .with_context(|| format!("{label} is not a canonical positive decimal string"))?;
    if number == 0 || number.to_string() != value {
        bail!("{label} is not a canonical positive decimal string");
    }
    Ok(())
}

fn validate_revision(value: &str, label: &str) -> Result<()> {
    if value.len() != 40
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("{label} is not a canonical full Git revision");
    }
    Ok(())
}

fn validate_sha256(value: &str, label: &str) -> Result<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("{label} is not a canonical SHA-256 value");
    }
    Ok(())
}

fn validate_ascii_text(value: &str, label: &str, maximum: usize) -> Result<()> {
    if value.is_empty()
        || value.len() > maximum
        || !value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
    {
        bail!("{label} is outside the accepted ASCII bound");
    }
    Ok(())
}

fn validate_store_text(value: &str, label: &str) -> Result<()> {
    if crate::manifest::StoreComponent::new(value.to_owned()).is_none() {
        bail!("{label} is not a canonical store component");
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    encode_hex(&Sha256::digest(bytes))
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

struct StagingDirectory(Option<PathBuf>);

impl StagingDirectory {
    fn path(&self) -> &Path {
        self.0.as_deref().expect("staging path remains owned")
    }
}

impl Drop for StagingDirectory {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = fs::remove_dir_all(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::time::{SystemTime, UNIX_EPOCH};

    use ring::rand::SystemRandom;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    use crate::manifest::{PackManifest, PayloadEntry, StoreComponent, compute_pack_digest};

    const CANDIDATE_REVISION: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const SIGNER_REVISION: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const KEY_ID: &str = "scribe-test-production-v1";

    struct TestTrust {
        public_key: Vec<u8>,
    }

    impl TrustRoot for TestTrust {
        fn public_key(&self, key_id: &str) -> Option<&[u8]> {
            (key_id == KEY_ID).then_some(self.public_key.as_slice())
        }
    }

    struct Fixture {
        owner: PathBuf,
        handoff: PathBuf,
        approval: PathBuf,
        policy: PathBuf,
        output: PathBuf,
        private_key: Vec<u8>,
        trust: TestTrust,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.owner);
        }
    }

    fn fixture(label: &str) -> Fixture {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let owner = std::env::temp_dir().join(format!(
            "scribe-approved-signing-{label}-{}-{nonce}",
            std::process::id()
        ));
        let handoff = owner.join("handoff");
        fs::create_dir_all(&handoff).unwrap();

        let key_document = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
        let private_key = key_document.as_ref().to_vec();
        let key_pair = Ed25519KeyPair::from_pkcs8(&private_key).unwrap();
        let trust = TestTrust {
            public_key: key_pair.public_key().as_ref().to_vec(),
        };
        let policy = WindowsSigningPolicy {
            schema_version: CONTROL_SCHEMA_VERSION,
            policy_id: "windows-gpu-production-v1".to_owned(),
            key_id: KEY_ID.to_owned(),
            public_key_sha256: sha256_hex(&trust.public_key),
            source_repository: "tyhuang9/scribe".to_owned(),
            source_ref: "refs/heads/main".to_owned(),
            app_version: "9.8.7".to_owned(),
            protocol_version: 5,
            worker_abi_version: 1,
            minimum_security_epoch: 1,
            packs: vec![
                PolicyPack {
                    backend: "cuda".to_owned(),
                    pack_id: "scribe-cuda-windows-x64".to_owned(),
                    provider: "transcribe-cpp-ggml-cuda".to_owned(),
                    worker_path: "bin/scribe-inference-worker.exe".to_owned(),
                    security_epoch: 1,
                },
                PolicyPack {
                    backend: "vulkan".to_owned(),
                    pack_id: "scribe-vulkan-windows-x64".to_owned(),
                    provider: "transcribe-cpp-ggml-vulkan".to_owned(),
                    worker_path: "bin/scribe-inference-worker.exe".to_owned(),
                    security_epoch: 1,
                },
            ],
        };
        let policy_path = owner.join("policy.json");
        let policy_bytes = serde_json::to_vec_pretty(&policy).unwrap();
        fs::write(&policy_path, &policy_bytes).unwrap();

        let app_build = format!("local-transcriber@9.8.7#{CANDIDATE_REVISION}");
        let worker_build = format!("scribe-inference-worker@9.8.7#{CANDIDATE_REVISION}");
        let packs = policy
            .packs
            .iter()
            .zip(["cuda", "vulkan"])
            .map(|(rule, root_name)| {
                create_prepared_pack(
                    &handoff.join(root_name),
                    root_name,
                    rule,
                    &app_build,
                    &worker_build,
                )
            })
            .collect::<Vec<_>>();
        let mut handoff_document = WindowsPackHandoff {
            schema_version: CONTROL_SCHEMA_VERSION,
            source_repository: policy.source_repository.clone(),
            source_ref: policy.source_ref.clone(),
            source_revision: CANDIDATE_REVISION.to_owned(),
            workflow_ref:
                "tyhuang9/scribe/.github/workflows/windows-gpu-pack-promotion.yml@refs/heads/main"
                    .to_owned(),
            run_id: "12345".to_owned(),
            run_attempt: "1".to_owned(),
            pack_version: "candidate-1".to_owned(),
            toolchain_manifest_sha256: "c".repeat(64),
            packs,
            release_set_digest: String::new(),
        };
        handoff_document.release_set_digest = release_set_digest(&handoff_document);
        let handoff_bytes = serde_json::to_vec(&handoff_document).unwrap();
        fs::write(handoff.join(HANDOFF_NAME), &handoff_bytes).unwrap();

        let approval_document = ApprovedWindowsPackSet {
            schema_version: CONTROL_SCHEMA_VERSION,
            policy_sha256: sha256_hex(&policy_bytes),
            signer_source_revision: SIGNER_REVISION.to_owned(),
            signer_sha256: "d".repeat(64),
            source_repository: handoff_document.source_repository.clone(),
            source_ref: handoff_document.source_ref.clone(),
            source_revision: handoff_document.source_revision.clone(),
            workflow_ref: handoff_document.workflow_ref.clone(),
            run_id: handoff_document.run_id.clone(),
            run_attempt: handoff_document.run_attempt.clone(),
            artifact_id: "9876".to_owned(),
            artifact_digest: "e".repeat(64),
            handoff_sha256: sha256_hex(&handoff_bytes),
            release_set_digest: handoff_document.release_set_digest.clone(),
            toolchain_manifest_sha256: handoff_document.toolchain_manifest_sha256.clone(),
            pack_version: handoff_document.pack_version.clone(),
            packs: handoff_document.packs.clone(),
        };
        let approval = owner.join("approval.json");
        fs::write(
            &approval,
            serde_json::to_vec_pretty(&approval_document).unwrap(),
        )
        .unwrap();
        Fixture {
            output: owner.join("signed"),
            owner,
            handoff,
            approval,
            policy: policy_path,
            private_key,
            trust,
        }
    }

    fn create_prepared_pack(
        root: &Path,
        root_name: &str,
        rule: &PolicyPack,
        app_build: &str,
        worker_build: &str,
    ) -> ApprovedPack {
        fs::create_dir_all(root.join("bin")).unwrap();
        let worker = format!("worker-{root_name}").into_bytes();
        fs::write(root.join(&rule.worker_path), &worker).unwrap();
        let payload = vec![PayloadEntry {
            path: rule.worker_path.clone(),
            size_bytes: worker.len() as u64,
            sha256: sha256_hex(&worker),
        }];
        let backend = AuthoringBackend::parse(&rule.backend).unwrap();
        let mut manifest = PackManifest {
            schema_version: 1,
            pack_id: StoreComponent::new(rule.pack_id.clone()).unwrap(),
            pack_version: StoreComponent::new("candidate-1").unwrap(),
            pack_digest: "0".repeat(64),
            security_epoch: rule.security_epoch,
            app_protocol_version: 5,
            worker_protocol_version: 5,
            runtime_abi_version: 1,
            app_build: app_build.to_owned(),
            worker_build: worker_build.to_owned(),
            backend: backend.manifest_backend(),
            provider: rule.provider.clone(),
            target_os: WINDOWS_TARGET_OS.to_owned(),
            target_arch: WINDOWS_TARGET_ARCH.to_owned(),
            worker_path: rule.worker_path.clone(),
            payload,
        };
        manifest.pack_digest = compute_pack_digest(&manifest).unwrap();
        let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
        fs::write(root.join(MANIFEST_NAME), &manifest_bytes).unwrap();
        ApprovedPack {
            backend: rule.backend.clone(),
            pack_root: root_name.to_owned(),
            pack_id: rule.pack_id.clone(),
            pack_version: "candidate-1".to_owned(),
            pack_digest: manifest.pack_digest,
            security_epoch: rule.security_epoch,
            provider: rule.provider.clone(),
            manifest_sha256: sha256_hex(&manifest_bytes),
        }
    }

    fn release_set_digest(handoff: &WindowsPackHandoff) -> String {
        let material = ReleaseSetMaterial {
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
        let mut hasher = Sha256::new();
        hasher.update(RELEASE_SET_DOMAIN);
        hasher.update(serde_json::to_vec(&material).unwrap());
        encode_hex(&hasher.finalize())
    }

    #[test]
    fn approved_pair_supports_a_candidate_revision_different_from_the_signer() {
        let fixture = fixture("different-revision");
        assert_ne!(
            CANDIDATE_REVISION,
            crate::worker_identity::DESKTOP_BUILD_ID
                .rsplit('#')
                .next()
                .unwrap()
        );
        let inspected = inspect_with_trust(
            &fixture.handoff,
            &fixture.approval,
            &fixture.policy,
            &fixture.trust,
        )
        .unwrap();
        assert_eq!(inspected.source_revision, CANDIDATE_REVISION);
        assert_eq!(inspected.packs.len(), 2);

        let mut key = Cursor::new(&fixture.private_key);
        let receipt = sign_with_trust(
            &fixture.handoff,
            &fixture.approval,
            &fixture.policy,
            &fixture.output,
            &mut key,
            &fixture.trust,
        )
        .unwrap();
        assert_eq!(receipt.source_revision, CANDIDATE_REVISION);
        assert!(fixture.output.join("cuda").join(SIGNATURE_NAME).is_file());
        assert!(fixture.output.join("vulkan").join(SIGNATURE_NAME).is_file());
        assert!(fixture.output.join(RECEIPT_NAME).is_file());
    }

    struct CountingReader {
        reads: usize,
    }

    impl Read for CountingReader {
        fn read(&mut self, _buffer: &mut [u8]) -> std::io::Result<usize> {
            self.reads += 1;
            Ok(0)
        }
    }

    #[test]
    fn invalid_control_data_is_rejected_before_secret_input_is_read() {
        let fixture = fixture("pre-key-validation");
        fs::write(fixture.handoff.join("unexpected.txt"), b"unexpected").unwrap();
        let mut key = CountingReader { reads: 0 };
        let error = sign_with_trust(
            &fixture.handoff,
            &fixture.approval,
            &fixture.policy,
            &fixture.output,
            &mut key,
            &fixture.trust,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("unexpected entry"));
        assert_eq!(key.reads, 0);
        assert!(!fixture.output.exists());
    }

    #[test]
    fn unexpected_empty_pack_directory_is_rejected_before_secret_input_is_read() {
        let fixture = fixture("unexpected-pack-directory");
        fs::create_dir(fixture.handoff.join("cuda/unexpected-empty")).unwrap();
        let mut key = CountingReader { reads: 0 };
        let error = sign_with_trust(
            &fixture.handoff,
            &fixture.approval,
            &fixture.policy,
            &fixture.output,
            &mut key,
            &fixture.trust,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("directory outside its canonical inventory"));
        assert_eq!(key.reads, 0);
        assert!(!fixture.output.exists());
    }

    struct MutatingKeyReader {
        key: Cursor<Vec<u8>>,
        target: PathBuf,
        mutated: bool,
    }

    impl Read for MutatingKeyReader {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if !self.mutated {
                fs::write(&self.target, b"changed-after-validation")?;
                self.mutated = true;
            }
            self.key.read(buffer)
        }
    }

    #[test]
    fn pair_corruption_after_validation_leaves_no_partial_publication() {
        let fixture = fixture("atomic-failure");
        let mut key = MutatingKeyReader {
            key: Cursor::new(fixture.private_key.clone()),
            target: fixture
                .handoff
                .join("vulkan/bin/scribe-inference-worker.exe"),
            mutated: false,
        };
        assert!(
            sign_with_trust(
                &fixture.handoff,
                &fixture.approval,
                &fixture.policy,
                &fixture.output,
                &mut key,
                &fixture.trust,
            )
            .is_err()
        );
        assert!(!fixture.output.exists());
        assert_eq!(
            fs::read_dir(&fixture.owner)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".scribe-windows-gpu-signing-"))
                .count(),
            0
        );
    }

    #[test]
    fn exact_epoch_identity_and_policy_hash_are_enforced() {
        let fixture = fixture("policy-boundaries");
        let policy_bytes = fs::read(&fixture.policy).unwrap();
        let mut policy: WindowsSigningPolicy = serde_json::from_slice(&policy_bytes).unwrap();
        policy.packs[0].security_epoch = 2;
        fs::write(&fixture.policy, serde_json::to_vec_pretty(&policy).unwrap()).unwrap();
        let mut approval: ApprovedWindowsPackSet =
            serde_json::from_slice(&fs::read(&fixture.approval).unwrap()).unwrap();
        approval.policy_sha256 = sha256_hex(&fs::read(&fixture.policy).unwrap());
        fs::write(
            &fixture.approval,
            serde_json::to_vec_pretty(&approval).unwrap(),
        )
        .unwrap();
        let error = inspect_with_trust(
            &fixture.handoff,
            &fixture.approval,
            &fixture.policy,
            &fixture.trust,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("approval pack identity"));

        policy.minimum_security_epoch = 3;
        fs::write(&fixture.policy, serde_json::to_vec_pretty(&policy).unwrap()).unwrap();
        approval.policy_sha256 = sha256_hex(&fs::read(&fixture.policy).unwrap());
        fs::write(
            &fixture.approval,
            serde_json::to_vec_pretty(&approval).unwrap(),
        )
        .unwrap();
        let error = inspect_with_trust(
            &fixture.handoff,
            &fixture.approval,
            &fixture.policy,
            &fixture.trust,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("below the policy floor"));
    }

    #[test]
    fn wrong_key_is_not_reported_with_secret_material() {
        let fixture = fixture("wrong-key");
        let sentinel = b"this-secret-must-not-appear";
        let mut input = Cursor::new(sentinel.as_slice());
        let error = sign_with_trust(
            &fixture.handoff,
            &fixture.approval,
            &fixture.policy,
            &fixture.output,
            &mut input,
            &fixture.trust,
        )
        .unwrap_err()
        .to_string();
        assert!(!error.contains(std::str::from_utf8(sentinel).unwrap()));
        assert!(!fixture.output.exists());
    }
}
