use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

fn valid_arguments(handoff: &Path, output: &Path) -> Vec<OsString> {
    let pairs = [
        ("--handoff-root", handoff.to_string_lossy().into_owned()),
        ("--output-root", output.to_string_lossy().into_owned()),
        ("--source-repository", "owner/repo".to_owned()),
        ("--source-ref", "refs/heads/main".to_owned()),
        ("--source-revision", "a".repeat(40)),
        (
            "--workflow-ref",
            "owner/repo/.github/workflows/windows-gpu-pack-promotion.yml@refs/heads/main"
                .to_owned(),
        ),
        ("--workflow-source-sha", "a".repeat(40)),
        ("--run-id", "123".to_owned()),
        ("--run-attempt", "1".to_owned()),
        ("--artifact-id", "456".to_owned()),
        ("--artifact-digest", "b".repeat(64)),
        ("--handoff-sha256", "c".repeat(64)),
        ("--release-set-digest", "d".repeat(64)),
        ("--toolchain-manifest-sha256", "e".repeat(64)),
        ("--pack-version", "0.1.0".to_owned()),
        ("--minimum-security-epoch", "1".to_owned()),
    ];
    let mut args = vec![OsString::from("promote-windows-pack-set")];
    for (name, value) in pairs {
        args.push(OsString::from(name));
        args.push(OsString::from(value));
    }
    args.push(OsString::from("--require-unused-release-set"));
    args
}

#[test]
fn release_client_fails_before_touching_untrusted_paths_or_authority() {
    let temp = tempfile::tempdir().unwrap();
    let handoff = temp.path().join("does-not-exist");
    let output = temp.path().join("must-not-exist");
    let result = Command::new(env!("CARGO_BIN_EXE_scribe-windows-gpu-promotion-client"))
        .args(valid_arguments(&handoff, &output))
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(78));
    assert!(result.stdout.is_empty());
    let diagnostic = String::from_utf8(result.stderr).unwrap();
    assert!(!diagnostic.contains(handoff.to_string_lossy().as_ref()));
    assert!(!diagnostic.contains(output.to_string_lossy().as_ref()));
    assert!(!handoff.exists());
    assert!(!output.exists());
    assert!(!temp.path().join("fixture-promotion-ledger.jsonl").exists());
}

#[test]
fn release_client_has_no_key_ledger_endpoint_or_fixture_flags() {
    let temp = tempfile::tempdir().unwrap();
    for forbidden in [
        "--private-key",
        "--ledger-root",
        "--broker-endpoint",
        "--fixture-signing",
        "--policy-namespace",
    ] {
        let mut arguments =
            valid_arguments(&temp.path().join("missing"), &temp.path().join("output"));
        arguments.extend([OsString::from(forbidden), OsString::from("forbidden")]);
        let status = Command::new(env!("CARGO_BIN_EXE_scribe-windows-gpu-promotion-client"))
            .args(arguments)
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(64), "accepted {forbidden}");
    }
}

#[test]
fn release_client_artifact_contains_no_fixture_authority_identity() {
    let binary = std::fs::read(env!("CARGO_BIN_EXE_scribe-windows-gpu-promotion-client")).unwrap();
    for forbidden in [
        b"fixture-ed25519-v1".as_slice(),
        b"fixture-promotion-ledger.jsonl".as_slice(),
        b"fixture signing authority".as_slice(),
        b"handoff_root".as_slice(),
        b"output_root".as_slice(),
        b"stage_name".as_slice(),
        b"output_name".as_slice(),
    ] {
        assert!(
            !binary
                .windows(forbidden.len())
                .any(|window| window == forbidden),
            "release client contains fixture authority material"
        );
    }
}
