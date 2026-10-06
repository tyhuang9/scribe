use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

#[path = "../build_support/build_revision.rs"]
pub mod build_revision;

fn sha256(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}

fn unique_root(label: &str) -> PathBuf {
    env::temp_dir().join(format!(
        "scribe-build-revision-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn run(command: &mut Command, label: &str) -> Output {
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("could not run {label}: {error}"));
    assert!(
        output.status.success(),
        "{label} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn scrub_git_environment<'a>(command: &'a mut Command, root: &Path) -> &'a mut Command {
    command
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env_remove("GIT_NAMESPACE")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("no-global-config"))
        .env("GIT_TEMPLATE_DIR", root.join("no-hooks"));
    command
}

fn git(root: &Path, arguments: &[&str]) -> Output {
    let mut command = Command::new("git");
    command
        .current_dir(root)
        .args(["-c", "core.hooksPath=disabled-hooks"])
        .args(arguments);
    scrub_git_environment(&mut command, root);
    run(&mut command, "isolated git")
}

struct FixtureRun {
    revision: String,
}

fn fixture_cargo(
    root: &Path,
    cargo_home: &Path,
    target: &Path,
    override_revision: Option<&str>,
) -> FixtureRun {
    fixture_cargo_with_git_overrides(root, cargo_home, target, override_revision, &[])
}

fn fixture_cargo_with_git_dir(
    root: &Path,
    cargo_home: &Path,
    target: &Path,
    override_revision: Option<&str>,
    git_dir_override: Option<&Path>,
) -> FixtureRun {
    let overrides = git_dir_override
        .map(|path| vec![("GIT_DIR", path)])
        .unwrap_or_default();
    fixture_cargo_with_git_overrides(root, cargo_home, target, override_revision, &overrides)
}

fn fixture_cargo_with_git_overrides(
    root: &Path,
    cargo_home: &Path,
    target: &Path,
    override_revision: Option<&str>,
    git_overrides: &[(&str, &Path)],
) -> FixtureRun {
    let mut command = Command::new("cargo");
    command
        .current_dir(root)
        .args(["run", "--locked", "--offline", "--quiet"])
        .env("CARGO_HOME", cargo_home)
        .env("CARGO_TARGET_DIR", target)
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_BUILD_RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("SCRIBE_BUILD_REVISION");
    scrub_git_environment(&mut command, root);
    for (name, value) in git_overrides {
        command.env(name, value);
    }
    if let Some(revision) = override_revision {
        command.env("SCRIBE_BUILD_REVISION", revision);
    }
    let output = run(&mut command, "offline fixture cargo");
    let stdout = String::from_utf8(output.stdout).unwrap();
    let revision = stdout
        .lines()
        .last()
        .unwrap_or_else(|| panic!("fixture did not print a compiled build revision: {stdout}"))
        .to_owned();
    let mut output_files = fs::read_dir(target.join("debug/build"))
        .unwrap()
        .flatten()
        .map(|entry| entry.path().join("output"))
        .filter(|path| path.is_file())
        .filter(|path| {
            path.parent()
                .unwrap()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("build-revision-fixture-")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        output_files.len(),
        1,
        "fixture build-script output was ambiguous: {output_files:?}"
    );
    let emitted = fs::read_to_string(output_files.pop().unwrap()).unwrap();
    assert!(
        emitted.contains(&format!("cargo:rustc-env=SCRIBE_BUILD_REVISION={revision}")),
        "fixture did not retain its emitted build-script revision: {emitted}"
    );
    FixtureRun { revision }
}

fn fixture_cargo_failure(
    root: &Path,
    cargo_home: &Path,
    target: &Path,
    override_revision: &OsStr,
) -> Output {
    let mut command = Command::new("cargo");
    command
        .current_dir(root)
        .args(["run", "--locked", "--offline", "--quiet"])
        .env("CARGO_HOME", cargo_home)
        .env("CARGO_TARGET_DIR", target)
        .env("SCRIBE_BUILD_REVISION", override_revision);
    scrub_git_environment(&mut command, root);
    let output = command.output().unwrap();
    assert!(
        !output.status.success(),
        "invalid override unexpectedly succeeded"
    );
    output
}

fn write_fixture(root: &Path) {
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"build-revision-fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
    )
    .unwrap();
    let helper = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap())
        .join("build_support/build_revision.rs");
    fs::write(
        root.join("build.rs"),
        format!(
            "#[path = {:?}] mod build_revision;\nfn identity(bytes: &[u8]) -> String {{ format!(\"{{:064x}}\", bytes.len()) }}\nfn main() {{ build_revision::emit_build_revision(std::path::Path::new(\".\"), identity); }}\n",
            helper
        ),
    )
    .unwrap();
    fs::write(
        root.join("src/main.rs"),
        "fn main() { println!(\"{}\", env!(\"SCRIBE_BUILD_REVISION\")); }\n",
    )
    .unwrap();
    fs::write(root.join("src/onnx_worker.rs"), b"c").unwrap();
    fs::write(root.join("src/worker_contracts.rs"), b"d").unwrap();
}

#[test]
fn source_digest_is_ordered_and_missing_inputs_fail() {
    let root = unique_root("digest");
    assert!(!root.exists());
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("Cargo.lock"), b"a").unwrap();
    fs::write(root.join("build.rs"), b"b").unwrap();
    fs::write(root.join("src/onnx_worker.rs"), b"c").unwrap();
    fs::write(root.join("src/worker_contracts.rs"), b"d").unwrap();
    assert_eq!(
        build_revision::source_digest(&root, sha256).unwrap(),
        "88d4266fd4e6338d13b845fcf289579d209c897823b9217da3e161936f031589"
    );
    fs::remove_file(root.join("src/worker_contracts.rs")).unwrap();
    assert!(build_revision::source_digest(&root, sha256).is_err());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn fixture_rebuilds_identity_for_git_topologies_overrides_and_source_fallback() {
    let fixture_parent = unique_root("fixture");
    assert!(!fixture_parent.exists());
    let root = fixture_parent.join("repository");
    let cargo_home = fixture_parent.join("cargo-home");
    let target = fixture_parent.join("target");
    write_fixture(&root);
    fs::create_dir_all(&cargo_home).unwrap();
    let mut lock = Command::new("cargo");
    lock.current_dir(&root)
        .args(["generate-lockfile", "--offline"])
        .env("CARGO_HOME", &cargo_home)
        .env_remove("SCRIBE_BUILD_REVISION");
    scrub_git_environment(&mut lock, &root);
    run(&mut lock, "offline fixture lockfile");

    git(&root, &["init"]);
    git(&root, &["config", "user.email", "fixture@example.invalid"]);
    git(&root, &["config", "user.name", "fixture"]);
    git(&root, &["add", "."]);
    git(&root, &["commit", "-m", "initial"]);
    let first = fixture_cargo(&root, &cargo_home, &target, None).revision;
    assert_eq!(
        first,
        String::from_utf8(git(&root, &["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
    );
    assert_eq!(
        fixture_cargo(&root, &cargo_home, &target, Some("   ")).revision,
        first
    );
    assert_eq!(
        fixture_cargo(&root, &cargo_home, &target, Some("")).revision,
        first
    );
    assert_eq!(
        fixture_cargo(&root, &cargo_home, &target, Some("build identity!? []"),).revision,
        "build identity!? []"
    );
    for valid in ["v".repeat(12), "v".repeat(96)] {
        assert_eq!(
            fixture_cargo(&root, &cargo_home, &target, Some(&valid)).revision,
            valid
        );
    }
    for invalid in [
        "too-short",
        &"x".repeat(97),
        "control-LF\npayload",
        "control-CR\rpayload",
        "control\u{7f}byte",
    ] {
        let output = fixture_cargo_failure(&root, &cargo_home, &target, OsStr::new(invalid));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("must be a 12-96 character ASCII build identity"));
        assert!(!stderr.contains("cargo:"));
    }

    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;

        let non_unicode = std::ffi::OsString::from_wide(&[0xd800]);
        let output = fixture_cargo_failure(&root, &cargo_home, &target, &non_unicode);
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("SCRIBE_BUILD_REVISION must be valid Unicode when set")
        );
    }

    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;

        let non_unicode = std::ffi::OsString::from_vec(vec![0xff]);
        let output = fixture_cargo_failure(&root, &cargo_home, &target, &non_unicode);
        assert!(
            String::from_utf8_lossy(&output.stderr)
                .contains("SCRIBE_BUILD_REVISION must be valid Unicode when set")
        );
    }

    let external = fixture_parent.join("external");
    fs::create_dir_all(&external).unwrap();
    git(&external, &["init"]);
    git(
        &external,
        &["config", "user.email", "fixture@example.invalid"],
    );
    git(&external, &["config", "user.name", "fixture"]);
    fs::write(external.join("external.txt"), "external\n").unwrap();
    git(&external, &["add", "."]);
    git(&external, &["commit", "-m", "external"]);
    assert_eq!(
        fixture_cargo_with_git_dir(
            &root,
            &cargo_home,
            &target,
            None,
            Some(&external.join(".git")),
        )
        .revision,
        first
    );
    let injected_target = fixture_parent.join("injected-target");
    let external_git = external.join(".git");
    for (label, name, value) in [
        ("git-dir", "GIT_DIR", external_git.as_path()),
        ("git-work-tree", "GIT_WORK_TREE", external.as_path()),
        ("git-common-dir", "GIT_COMMON_DIR", external_git.as_path()),
    ] {
        assert_eq!(
            fixture_cargo_with_git_overrides(
                &root,
                &cargo_home,
                &fixture_parent.join(label),
                None,
                &[(name, value)],
            )
            .revision,
            first
        );
    }
    let overrides = [
        ("GIT_DIR", external_git.as_path()),
        ("GIT_WORK_TREE", external.as_path()),
        ("GIT_COMMON_DIR", external_git.as_path()),
    ];
    assert_eq!(
        fixture_cargo_with_git_overrides(&root, &cargo_home, &injected_target, None, &overrides,)
            .revision,
        first
    );
    git(
        &root,
        &["commit", "--allow-empty", "-m", "ambient-override-change"],
    );
    let first =
        fixture_cargo_with_git_overrides(&root, &cargo_home, &injected_target, None, &overrides)
            .revision;
    assert_eq!(
        first,
        String::from_utf8(git(&root, &["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
    );
    assert_ne!(
        first,
        String::from_utf8(git(&external, &["rev-parse", "HEAD"]).stdout)
            .unwrap()
            .trim()
    );

    git(
        &root,
        &["commit", "--allow-empty", "-m", "loose-branch-change"],
    );
    let loose_branch = fixture_cargo(&root, &cargo_home, &target, None).revision;
    assert_ne!(loose_branch, first);

    git(&root, &["pack-refs", "--all", "--prune"]);
    let branch_reference = root.join(".git").join(
        String::from_utf8(git(&root, &["symbolic-ref", "--quiet", "HEAD"]).stdout)
            .unwrap()
            .trim(),
    );
    assert!(!branch_reference.exists());
    assert_eq!(
        fixture_cargo(&root, &cargo_home, &target, None).revision,
        loose_branch
    );
    git(&root, &["commit", "--allow-empty", "-m", "packed-to-loose"]);
    assert!(branch_reference.is_file());
    let branch = fixture_cargo(&root, &cargo_home, &target, None).revision;
    assert_ne!(branch, loose_branch);

    git(&root, &["checkout", "--detach", "HEAD"]);
    assert_eq!(
        fixture_cargo(&root, &cargo_home, &target, None).revision,
        branch
    );
    git(&root, &["commit", "--allow-empty", "-m", "detached-change"]);
    let detached = fixture_cargo(&root, &cargo_home, &target, None).revision;
    assert_ne!(detached, branch);

    let worktree = fixture_parent.join("worktree");
    let worktree_target = fixture_parent.join("worktree-target");
    git(
        &root,
        &[
            "worktree",
            "add",
            "-b",
            "fixture-linked",
            worktree.to_str().unwrap(),
            "HEAD",
        ],
    );
    let worktree_first = fixture_cargo(&worktree, &cargo_home, &worktree_target, None).revision;
    assert_eq!(worktree_first, detached);
    git(
        &worktree,
        &["commit", "--allow-empty", "-m", "worktree-loose-change"],
    );
    let worktree_loose = fixture_cargo(&worktree, &cargo_home, &worktree_target, None).revision;
    assert_ne!(worktree_loose, worktree_first);
    git(&root, &["pack-refs", "--all", "--prune"]);
    let linked_reference = root.join(".git/refs/heads/fixture-linked");
    assert!(!linked_reference.exists());
    assert_eq!(
        fixture_cargo(&worktree, &cargo_home, &worktree_target, None).revision,
        worktree_loose
    );
    git(
        &worktree,
        &["commit", "--allow-empty", "-m", "worktree-packed-change"],
    );
    let worktree_changed = fixture_cargo(&worktree, &cargo_home, &worktree_target, None).revision;
    assert_ne!(worktree_changed, worktree_loose);
    assert!(linked_reference.is_file());
    git(&root, &["worktree", "remove", worktree.to_str().unwrap()]);

    assert_eq!(
        fixture_cargo(&root, &cargo_home, &target, Some("override-stable")).revision,
        "override-stable"
    );
    git(
        &root,
        &["commit", "--allow-empty", "-m", "override-git-change"],
    );
    assert_eq!(
        fixture_cargo(&root, &cargo_home, &target, Some("override-stable")).revision,
        "override-stable"
    );
    assert_eq!(
        fixture_cargo(&root, &cargo_home, &target, Some("override-changed")).revision,
        "override-changed"
    );

    fs::write(root.join(".git/HEAD"), "not-a-git-head\n").unwrap();
    let mut malformed = Command::new("cargo");
    malformed
        .current_dir(&root)
        .args(["run", "--locked", "--offline", "--quiet"])
        .env("CARGO_HOME", &cargo_home)
        .env("CARGO_TARGET_DIR", &target)
        .env_remove("SCRIBE_BUILD_REVISION");
    scrub_git_environment(&mut malformed, &root);
    let malformed_output = malformed.output().unwrap();
    assert!(!malformed_output.status.success());
    assert!(
        String::from_utf8_lossy(&malformed_output.stderr)
            .contains("could not inspect local Git build identity")
    );

    fs::rename(root.join(".git"), root.join(".git-broken")).unwrap();
    let fallback = fixture_cargo(&root, &cargo_home, &target, None).revision;
    assert!(fallback.starts_with("source-"));
    fs::write(root.join("src/onnx_worker.rs"), b"changed-c").unwrap();
    assert_ne!(
        fixture_cargo(&root, &cargo_home, &target, None).revision,
        fallback
    );
    fs::remove_file(root.join("src/worker_contracts.rs")).unwrap();
    let mut missing = Command::new("cargo");
    missing
        .current_dir(&root)
        .args(["run", "--locked", "--offline", "--quiet"])
        .env("CARGO_HOME", &cargo_home)
        .env("CARGO_TARGET_DIR", &target)
        .env_remove("SCRIBE_BUILD_REVISION");
    scrub_git_environment(&mut missing, &root);
    let missing_output = missing.output().unwrap();
    assert!(!missing_output.status.success());
    assert!(
        String::from_utf8_lossy(&missing_output.stderr)
            .contains("could not derive source build identity")
    );
    fs::remove_dir_all(fixture_parent).unwrap();
}
