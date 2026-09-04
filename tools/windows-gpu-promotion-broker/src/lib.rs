//! Wire contract for a separately privileged Windows GPU pack promotion broker.
//!
//! Release builds contain intent validation and a fixed authenticated Windows
//! transport to a no-authority service. They cannot access a key, ledger,
//! caller-selected endpoint, handoff, or output. The filesystem, ledger, and
//! signing state machine below is compiled exclusively into unit tests as a
//! hostile-input contract proof; it is not production authority.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

mod protocol;

pub use protocol::{
    BrokerAckV1, BrokerOutcomeV1, BrokerRequestV1, BrokerResponseV1, NotProvisionedCode,
    PIPE_ENDPOINT, SERVICE_NAME, SERVICE_SID,
};

#[cfg(windows)]
mod windows_native;

#[cfg(windows)]
pub use windows_native::{
    ClientTransportError, harden_dll_search, request_promotion, run_service_dispatcher,
};

pub const COMMAND: &str = "promote-windows-pack-set";
pub const PROMOTION_POLICY_NAMESPACE: &str = "scribe-windows-gpu-production-v1";
pub const PROMOTION_INTENT_DOMAIN: &[u8] = b"scribe-windows-gpu-promotion-intent-v1\0";
const SHA256_LEN: usize = 64;

/// Canonical authority input for a future privileged broker.
///
/// This is deliberately the only serializable promotion type. Local intake
/// and publication paths belong to [`ClientInvocation`] and cannot cross the
/// future authority boundary as part of the signed or replayed identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromotionIntent {
    pub schema_version: u16,
    pub policy_namespace: String,
    pub source_repository: String,
    pub source_ref: String,
    pub source_revision: String,
    pub workflow_ref: String,
    pub workflow_source_sha: String,
    pub run_id: String,
    pub run_attempt: String,
    pub artifact_id: String,
    pub artifact_digest: String,
    pub handoff_sha256: String,
    pub release_set_digest: String,
    pub toolchain_manifest_sha256: String,
    pub pack_version: String,
    pub minimum_security_epoch: u64,
    pub require_unused_release_set: bool,
}

impl PromotionIntent {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != 1
            || self.policy_namespace != PROMOTION_POLICY_NAMESPACE
            || !self.require_unused_release_set
        {
            bail!("unsupported promotion intent policy");
        }
        for value in [
            &self.artifact_digest,
            &self.handoff_sha256,
            &self.release_set_digest,
            &self.toolchain_manifest_sha256,
        ] {
            validate_sha256(value)?;
        }
        for value in [&self.source_revision, &self.workflow_source_sha] {
            if value.len() != 40
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                bail!("source identity is noncanonical");
            }
        }
        if self.workflow_source_sha != self.source_revision {
            bail!("workflow source does not match the default-branch source revision");
        }
        for (value, maximum) in [
            (&self.run_id, 20),
            (&self.artifact_id, 20),
            (&self.run_attempt, 10),
        ] {
            validate_positive_decimal(value, maximum)?;
        }
        validate_identifier(&self.source_repository, 200, true)?;
        validate_identifier(&self.source_ref, 256, true)?;
        validate_identifier(&self.workflow_ref, 256, true)?;
        validate_store_component(&self.pack_version)?;
        if self.minimum_security_epoch == 0 {
            bail!("security epoch is outside the contract");
        }
        Ok(())
    }

    pub fn canonical_json(&self) -> Result<Vec<u8>> {
        self.validate()?;
        Ok(serde_json::to_vec(self)?)
    }

    pub fn sha256(&self) -> Result<String> {
        let mut hasher = Sha256::new();
        hasher.update(PROMOTION_INTENT_DOMAIN);
        hasher.update(self.canonical_json()?);
        Ok(encode_hex(&hasher.finalize()))
    }
}

/// Process-local CLI inputs paired with the canonical authority intent.
///
/// This type intentionally does not implement `Serialize` or `Deserialize`.
/// Its paths locate the downloaded handoff and a test-local publication parent;
/// they never influence intent bytes, replay identity, or protected output
/// naming.
#[derive(Clone, Eq, PartialEq)]
pub struct ClientInvocation {
    pub handoff_root: PathBuf,
    pub output_root: PathBuf,
    pub intent: PromotionIntent,
}

impl ClientInvocation {
    pub fn new(
        handoff_root: impl Into<PathBuf>,
        output_root: impl Into<PathBuf>,
        intent: PromotionIntent,
    ) -> Result<Self> {
        let invocation = Self {
            handoff_root: handoff_root.into(),
            output_root: output_root.into(),
            intent,
        };
        invocation.validate()?;
        Ok(invocation)
    }

    pub fn parse_cli(arguments: impl IntoIterator<Item = OsString>) -> Result<Self> {
        let mut arguments = arguments.into_iter();
        let command = arguments.next().ok_or_else(|| anyhow!("missing command"))?;
        if command != OsStr::new(COMMAND) {
            bail!("unsupported command");
        }
        let mut values = BTreeMap::<String, OsString>::new();
        let mut unused_required = false;
        while let Some(option) = arguments.next() {
            let option = option
                .to_str()
                .ok_or_else(|| anyhow!("option name is not UTF-8"))?;
            if option == "--require-unused-release-set" {
                if unused_required {
                    bail!("duplicate option");
                }
                unused_required = true;
                continue;
            }
            if !option.starts_with("--") {
                bail!("unexpected positional argument");
            }
            let value = arguments
                .next()
                .ok_or_else(|| anyhow!("option is missing a value"))?;
            if value.to_string_lossy().starts_with("--") {
                bail!("option is missing a value");
            }
            if values.insert(option.to_owned(), value).is_some() {
                bail!("duplicate option");
            }
        }
        const REQUIRED: [&str; 15] = [
            "--artifact-digest",
            "--artifact-id",
            "--handoff-root",
            "--handoff-sha256",
            "--minimum-security-epoch",
            "--output-root",
            "--pack-version",
            "--release-set-digest",
            "--run-attempt",
            "--run-id",
            "--source-ref",
            "--source-repository",
            "--source-revision",
            "--toolchain-manifest-sha256",
            "--workflow-ref",
        ];
        let mut accepted = REQUIRED.into_iter().collect::<BTreeSet<_>>();
        accepted.insert("--workflow-source-sha");
        if values.len() != accepted.len()
            || values.keys().any(|key| !accepted.contains(key.as_str()))
            || !unused_required
        {
            bail!("unknown or missing option");
        }
        let text = |name: &str| -> Result<String> {
            values
                .get(name)
                .and_then(|value| value.to_str())
                .map(str::to_owned)
                .ok_or_else(|| anyhow!("option value is not UTF-8"))
        };
        let path = |name: &str| -> Result<PathBuf> {
            values
                .get(name)
                .cloned()
                .map(PathBuf::from)
                .ok_or_else(|| anyhow!("path option is missing"))
        };
        let minimum_security_epoch_text = text("--minimum-security-epoch")?;
        validate_positive_decimal(&minimum_security_epoch_text, 20)?;
        let minimum_security_epoch = minimum_security_epoch_text
            .parse::<u64>()
            .map_err(|_| anyhow!("minimum security epoch is noncanonical"))?;
        Self::new(
            path("--handoff-root")?,
            path("--output-root")?,
            PromotionIntent {
                schema_version: 1,
                policy_namespace: PROMOTION_POLICY_NAMESPACE.to_owned(),
                source_repository: text("--source-repository")?,
                source_ref: text("--source-ref")?,
                source_revision: text("--source-revision")?,
                workflow_ref: text("--workflow-ref")?,
                workflow_source_sha: text("--workflow-source-sha")?,
                run_id: text("--run-id")?,
                run_attempt: text("--run-attempt")?,
                artifact_id: text("--artifact-id")?,
                artifact_digest: text("--artifact-digest")?,
                handoff_sha256: text("--handoff-sha256")?,
                release_set_digest: text("--release-set-digest")?,
                toolchain_manifest_sha256: text("--toolchain-manifest-sha256")?,
                pack_version: text("--pack-version")?,
                minimum_security_epoch,
                require_unused_release_set: true,
            },
        )
    }

    pub fn validate(&self) -> Result<()> {
        self.intent.validate()?;
        validate_local_path(&self.handoff_root)?;
        validate_local_path(&self.output_root)?;
        Ok(())
    }
}

fn validate_local_path(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty() || path.as_os_str().to_string_lossy().len() > 32_767 {
        bail!("local path is outside the invocation contract");
    }
    Ok(())
}

pub(crate) fn validate_sha256(value: &str) -> Result<()> {
    if value.len() != SHA256_LEN
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("digest is noncanonical");
    }
    Ok(())
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

fn validate_positive_decimal(value: &str, maximum: usize) -> Result<()> {
    if value.is_empty()
        || value.len() > maximum
        || value.starts_with('0')
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        bail!("numeric identity is noncanonical");
    }
    Ok(())
}

fn validate_identifier(value: &str, maximum: usize, slash_allowed: bool) -> Result<()> {
    if value.is_empty()
        || value.len() > maximum
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'.' | b'_' | b'-' | b':' | b'@')
                || (slash_allowed && byte == b'/')
        })
    {
        bail!("identity is noncanonical");
    }
    Ok(())
}

pub(crate) fn validate_store_component(value: &str) -> Result<()> {
    let bytes = value.as_bytes();
    if value.is_empty()
        || value.len() > 96
        || value == "."
        || value == ".."
        || !bytes
            .first()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        || !bytes
            .last()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        || !bytes.iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
    {
        bail!("pack version is noncanonical");
    }
    Ok(())
}

#[cfg(test)]
mod fixture;

#[cfg(test)]
mod intent_tests {
    use super::*;

    fn valid_args() -> Vec<OsString> {
        let pairs = [
            ("--handoff-root", r"C:\handoff"),
            ("--output-root", r"C:\output"),
            ("--source-repository", "owner/repo"),
            ("--source-ref", "refs/heads/main"),
            ("--source-revision", &"a".repeat(40)),
            (
                "--workflow-ref",
                "owner/repo/.github/workflows/promote.yml@refs/heads/main",
            ),
            ("--workflow-source-sha", &"a".repeat(40)),
            ("--run-id", "123"),
            ("--run-attempt", "1"),
            ("--artifact-id", "456"),
            ("--artifact-digest", &"b".repeat(64)),
            ("--handoff-sha256", &"c".repeat(64)),
            ("--release-set-digest", &"d".repeat(64)),
            ("--toolchain-manifest-sha256", &"e".repeat(64)),
            ("--pack-version", "0.1.0"),
            ("--minimum-security-epoch", "1"),
        ];
        let mut arguments = vec![OsString::from(COMMAND)];
        for (name, value) in pairs {
            arguments.push(OsString::from(name));
            arguments.push(OsString::from(value));
        }
        arguments.push(OsString::from("--require-unused-release-set"));
        arguments
    }

    #[test]
    fn cli_accepts_only_the_exact_broker_contract() {
        let invocation = ClientInvocation::parse_cli(valid_args()).unwrap();
        assert_eq!(invocation.intent.schema_version, 1);
        assert!(invocation.intent.require_unused_release_set);
    }

    #[test]
    fn cli_rejects_unknown_duplicate_missing_and_key_or_state_options() {
        for extra in [
            "--private-key",
            "--ledger-root",
            "--broker-endpoint",
            "--fixture-signing",
            "--policy-namespace",
        ] {
            let mut arguments = valid_args();
            arguments.extend([OsString::from(extra), OsString::from("forbidden")]);
            assert!(
                ClientInvocation::parse_cli(arguments).is_err(),
                "accepted {extra}"
            );
        }
        let mut duplicate = valid_args();
        duplicate.extend([OsString::from("--run-id"), OsString::from("999")]);
        assert!(ClientInvocation::parse_cli(duplicate).is_err());
        let mut missing = valid_args();
        missing.truncate(missing.len() - 1);
        assert!(ClientInvocation::parse_cli(missing).is_err());
    }

    #[test]
    fn intent_json_is_canonical_path_free_and_denies_local_or_authority_fields() {
        let invocation = ClientInvocation::parse_cli(valid_args()).unwrap();
        let bytes = invocation.intent.canonical_json().unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        assert!(!text.contains("handoff_root"));
        assert!(!text.contains("output_root"));
        for path in [&invocation.handoff_root, &invocation.output_root] {
            let encoded = serde_json::to_string(&path.to_string_lossy()).unwrap();
            assert!(!text.contains(encoded.trim_matches('"')));
        }
        let round_trip: PromotionIntent = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(round_trip, invocation.intent);
        for forbidden in [
            "handoff_root",
            "output_root",
            "endpoint",
            "broker_endpoint",
            "private_key",
            "key_path",
            "ledger_root",
            "state_root",
        ] {
            let mut value = serde_json::to_value(&invocation.intent).unwrap();
            value
                .as_object_mut()
                .unwrap()
                .insert(forbidden.into(), "forbidden".into());
            assert!(
                serde_json::from_value::<PromotionIntent>(value).is_err(),
                "accepted {forbidden}"
            );
        }
    }

    #[test]
    fn intent_identity_is_path_independent_and_authority_sensitive() {
        let first = ClientInvocation::parse_cli(valid_args()).unwrap();
        let mut different_paths = valid_args();
        for (name, value) in [
            ("--handoff-root", r"D:\different-intake"),
            ("--output-root", r"E:\different-publication"),
        ] {
            let index = different_paths
                .iter()
                .position(|argument| argument == name)
                .unwrap();
            different_paths[index + 1] = OsString::from(value);
        }
        let second = ClientInvocation::parse_cli(different_paths).unwrap();
        assert_eq!(
            first.intent.canonical_json().unwrap(),
            second.intent.canonical_json().unwrap()
        );
        assert_eq!(
            first.intent.sha256().unwrap(),
            second.intent.sha256().unwrap()
        );

        let original_digest = first.intent.sha256().unwrap();
        let mut mutations = Vec::new();
        let mut repository = first.intent.clone();
        repository.source_repository = "other/repo".to_owned();
        mutations.push(repository);
        let mut source_ref = first.intent.clone();
        source_ref.source_ref = "refs/heads/release".to_owned();
        mutations.push(source_ref);
        let mut revision = first.intent.clone();
        revision.source_revision = "9".repeat(40);
        revision.workflow_source_sha = "9".repeat(40);
        mutations.push(revision);
        let mut workflow = first.intent.clone();
        workflow.workflow_ref = "owner/repo/.github/workflows/other.yml@refs/heads/main".to_owned();
        mutations.push(workflow);
        let mut run_id = first.intent.clone();
        run_id.run_id = "124".to_owned();
        mutations.push(run_id);
        let mut run_attempt = first.intent.clone();
        run_attempt.run_attempt = "2".to_owned();
        mutations.push(run_attempt);
        let mut artifact_id = first.intent.clone();
        artifact_id.artifact_id = "457".to_owned();
        mutations.push(artifact_id);
        let mut artifact = first.intent.clone();
        artifact.artifact_digest = "f".repeat(64);
        mutations.push(artifact);
        let mut handoff = first.intent.clone();
        handoff.handoff_sha256 = "1".repeat(64);
        mutations.push(handoff);
        let mut release = first.intent.clone();
        release.release_set_digest = "0".repeat(64);
        mutations.push(release);
        let mut toolchain = first.intent.clone();
        toolchain.toolchain_manifest_sha256 = "2".repeat(64);
        mutations.push(toolchain);
        let mut version = first.intent.clone();
        version.pack_version = "0.1.1".to_owned();
        mutations.push(version);
        let mut epoch = first.intent.clone();
        epoch.minimum_security_epoch = 2;
        mutations.push(epoch);
        for changed in mutations {
            changed.validate().unwrap();
            assert_ne!(changed.sha256().unwrap(), original_digest);
        }

        let mut changed_schema = first.intent.clone();
        changed_schema.schema_version = 2;
        assert!(changed_schema.sha256().is_err());
        let mut changed_namespace = first.intent.clone();
        changed_namespace.policy_namespace = "attacker-policy".to_owned();
        assert!(changed_namespace.sha256().is_err());
        let mut disabled_replay = first.intent.clone();
        disabled_replay.require_unused_release_set = false;
        assert!(disabled_replay.sha256().is_err());
    }

    #[test]
    fn canonical_intent_bytes_and_domain_digest_match_the_powershell_golden_vector() {
        const GOLDEN_JSON: &str = r#"{"schema_version":1,"policy_namespace":"scribe-windows-gpu-production-v1","source_repository":"tyhuang9/scribe","source_ref":"refs/heads/main","source_revision":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","workflow_ref":"tyhuang9/scribe/.github/workflows/windows-gpu-pack-promotion.yml@refs/heads/main","workflow_source_sha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","run_id":"1001","run_attempt":"1","artifact_id":"2002","artifact_digest":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","handoff_sha256":"cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc","release_set_digest":"dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd","toolchain_manifest_sha256":"eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee","pack_version":"0.1.0-promotion-fixture","minimum_security_epoch":1,"require_unused_release_set":true}"#;
        const GOLDEN_SHA256: &str =
            "475757a8bc0a8672fad3864bc14261ae8aa84d1fe91d27093e925ca2138ab508";

        let intent: PromotionIntent = serde_json::from_slice(GOLDEN_JSON.as_bytes()).unwrap();
        assert_eq!(intent.canonical_json().unwrap(), GOLDEN_JSON.as_bytes());
        assert_eq!(intent.sha256().unwrap(), GOLDEN_SHA256);
    }

    #[test]
    fn default_branch_workflow_source_must_equal_the_pack_source_revision() {
        let mut arguments = valid_args();
        let index = arguments
            .iter()
            .position(|value| value == "--workflow-source-sha")
            .unwrap();
        arguments[index + 1] = OsString::from("f".repeat(40));
        assert!(ClientInvocation::parse_cli(arguments).is_err());
    }

    #[test]
    fn minimum_security_epoch_requires_canonical_positive_u64_decimal() {
        for invalid in ["01", "+1", "0", "18446744073709551616"] {
            let mut arguments = valid_args();
            let index = arguments
                .iter()
                .position(|value| value == "--minimum-security-epoch")
                .unwrap();
            arguments[index + 1] = OsString::from(invalid);
            assert!(
                ClientInvocation::parse_cli(arguments).is_err(),
                "accepted noncanonical epoch {invalid}"
            );
        }
    }
}
