//! Wire contract for a separately privileged Windows GPU pack promotion broker.
//!
//! Release builds contain request validation only. They cannot access a key,
//! ledger, broker endpoint, or output. The filesystem, ledger, and signing
//! state machine below is compiled exclusively into unit tests as a hostile
//! input contract proof; it is not production authority.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};

pub const COMMAND: &str = "promote-windows-pack-set";
const SHA256_LEN: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromotionRequest {
    pub schema_version: u16,
    pub handoff_root: String,
    pub output_root: String,
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

impl PromotionRequest {
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
        let minimum_security_epoch = text("--minimum-security-epoch")?
            .parse::<u64>()
            .map_err(|_| anyhow!("minimum security epoch is noncanonical"))?;
        let request = Self {
            schema_version: 1,
            handoff_root: text("--handoff-root")?,
            output_root: text("--output-root")?,
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
        };
        request.validate()?;
        Ok(request)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != 1 || !self.require_unused_release_set {
            bail!("unsupported request policy");
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
        if self.handoff_root.is_empty()
            || self.output_root.is_empty()
            || self.handoff_root.len() > 32_767
            || self.output_root.len() > 32_767
            || self.minimum_security_epoch == 0
        {
            bail!("path or security epoch is outside the contract");
        }
        Ok(())
    }

    pub fn canonical_json(&self) -> Result<Vec<u8>> {
        self.validate()?;
        Ok(serde_json::to_vec(self)?)
    }
}

fn validate_sha256(value: &str) -> Result<()> {
    if value.len() != SHA256_LEN
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        bail!("digest is noncanonical");
    }
    Ok(())
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

fn validate_store_component(value: &str) -> Result<()> {
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
mod request_tests {
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
        let request = PromotionRequest::parse_cli(valid_args()).unwrap();
        assert_eq!(request.schema_version, 1);
        assert!(request.require_unused_release_set);
    }

    #[test]
    fn cli_rejects_unknown_duplicate_missing_and_key_or_state_options() {
        for extra in [
            "--private-key",
            "--ledger-root",
            "--broker-endpoint",
            "--fixture-signing",
        ] {
            let mut arguments = valid_args();
            arguments.extend([OsString::from(extra), OsString::from("forbidden")]);
            assert!(
                PromotionRequest::parse_cli(arguments).is_err(),
                "accepted {extra}"
            );
        }
        let mut duplicate = valid_args();
        duplicate.extend([OsString::from("--run-id"), OsString::from("999")]);
        assert!(PromotionRequest::parse_cli(duplicate).is_err());
        let mut missing = valid_args();
        missing.truncate(missing.len() - 1);
        assert!(PromotionRequest::parse_cli(missing).is_err());
    }

    #[test]
    fn request_json_is_canonical_and_denies_unknown_fields() {
        let request = PromotionRequest::parse_cli(valid_args()).unwrap();
        let bytes = request.canonical_json().unwrap();
        let round_trip: PromotionRequest = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(round_trip, request);
        let mut value = serde_json::to_value(&request).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("key_path".into(), "forbidden".into());
        assert!(serde_json::from_value::<PromotionRequest>(value).is_err());
    }
}
