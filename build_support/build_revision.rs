use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const BUILD_REVISION_ENV: &str = "SCRIBE_BUILD_REVISION";
pub const SOURCE_IDENTITY_FILES: [&str; 4] = [
    "Cargo.lock",
    "build.rs",
    "src/onnx_worker.rs",
    "src/worker_contracts.rs",
];

pub fn emit_build_revision<F>(repository_root: &Path, sha256_hex: F)
where
    F: Fn(&[u8]) -> String,
{
    let override_revision = env::var_os(BUILD_REVISION_ENV).and_then(|value| {
        let value = value
            .to_str()
            .unwrap_or_else(|| panic!("{BUILD_REVISION_ENV} must be valid Unicode when set"));
        (!value.trim().is_empty()).then(|| value.to_owned())
    });
    if let Some(revision) = &override_revision {
        assert!(
            is_valid_build_revision(revision),
            "{BUILD_REVISION_ENV} must be a 12-96 character ASCII build identity"
        );
    }
    println!("cargo:rerun-if-env-changed={BUILD_REVISION_ENV}");
    for path in SOURCE_IDENTITY_FILES {
        println!("cargo:rerun-if-changed={path}");
    }
    if let Some(revision) = override_revision {
        println!("cargo:rustc-env={BUILD_REVISION_ENV}={revision}");
        return;
    }
    let git_dir = watch_git_identity(repository_root)
        .unwrap_or_else(|error| panic!("could not inspect local Git build identity: {error}"));
    let revision = match git_dir {
        Some(_) => git_revision(repository_root)
            .unwrap_or_else(|| panic!("could not resolve local Git build identity")),
        None => {
            let digest = source_digest(repository_root, &sha256_hex)
                .unwrap_or_else(|error| panic!("could not derive source build identity: {error}"));
            format!("source-{digest}")
        }
    };
    assert!(
        is_valid_build_revision(&revision),
        "{BUILD_REVISION_ENV} must be a 12-96 character ASCII build identity"
    );
    println!("cargo:rustc-env={BUILD_REVISION_ENV}={revision}");
}

pub fn source_digest<F>(repository_root: &Path, sha256_hex: F) -> io::Result<String>
where
    F: Fn(&[u8]) -> String,
{
    let mut bytes = Vec::new();
    for relative in SOURCE_IDENTITY_FILES {
        bytes.extend(fs::read(repository_root.join(relative))?);
    }
    Ok(sha256_hex(&bytes))
}

pub fn is_valid_build_revision(value: &str) -> bool {
    (12..=96).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii() && !byte.is_ascii_control())
}

fn git_revision(repository_root: &Path) -> Option<String> {
    sanitized_git_command(repository_root)
        .args(["rev-parse", "--verify", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_owned())
        .filter(|value| is_git_object_id(value))
}

fn sanitized_git_command(repository_root: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(repository_root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .env_remove("GIT_NAMESPACE");
    command
}

fn watch_git_identity(repository_root: &Path) -> io::Result<Option<PathBuf>> {
    let marker = repository_root.join(".git");
    let Some(git_dir) = git_dir_from_marker(&marker)? else {
        return Ok(None);
    };
    if marker.is_file() {
        println!("cargo:rerun-if-changed={}", marker.display());
    }
    if !git_dir.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Git metadata path is not a directory",
        ));
    }
    let git_resolved_dir = git_resolved_dir(repository_root)?;
    if fs::canonicalize(&git_dir)? != fs::canonicalize(git_resolved_dir)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Git resolved metadata does not match the local .git marker",
        ));
    }
    if git_ref_format(repository_root)? != "files" {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Git reference storage must be files",
        ));
    }
    let common = git_dir.join("commondir");
    if common.is_file() {
        println!("cargo:rerun-if-changed={}", common.display());
    }
    let head_path = git_path(repository_root, "HEAD")?;
    println!("cargo:rerun-if-changed={}", head_path.display());
    let head = read_git_head(&head_path)?;
    let Some(reference) = head.reference() else {
        return Ok(Some(git_dir));
    };
    let terminal_reference = git_symbolic_ref(repository_root)?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "Git HEAD reference is unresolved",
        )
    })?;
    if terminal_reference != reference {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Git symbolic reference chains are unsupported",
        ));
    }
    let reference_path = git_path(
        repository_root,
        reference.to_str().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "Git reference is not Unicode")
        })?,
    )?;
    let packed = git_path(repository_root, "packed-refs")?;
    let refs_root = git_path(repository_root, "refs")?;
    if !watch_git_reference(&reference_path, &packed, &refs_root, reference)? {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Git HEAD reference is absent from loose and packed refs",
        ));
    }
    Ok(Some(git_dir))
}

fn git_resolved_dir(repository_root: &Path) -> io::Result<PathBuf> {
    let output = sanitized_git_command(repository_root)
        .args(["rev-parse", "--path-format=absolute", "--absolute-git-dir"])
        .output()?;
    if !output.status.success() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Git could not resolve local metadata",
        ));
    }
    let value = String::from_utf8(output.stdout).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidData, "Git metadata path is not UTF-8")
    })?;
    let path = PathBuf::from(value.trim());
    if path.as_os_str().is_empty() || !path.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Git resolved metadata is not a directory",
        ));
    }
    Ok(path)
}

fn git_ref_format(repository_root: &Path) -> io::Result<String> {
    let output = sanitized_git_command(repository_root)
        .args(["config", "--local", "--get", "extensions.refStorage"])
        .output()?;
    if output.status.code() == Some(1) {
        return Ok("files".to_owned());
    }
    if !output.status.success() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Git could not resolve reference storage",
        ));
    }
    String::from_utf8(output.stdout)
        .map(|value| value.trim().to_owned())
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "Git reference storage is not UTF-8",
            )
        })
}

fn git_path(repository_root: &Path, path: &str) -> io::Result<PathBuf> {
    let output = sanitized_git_command(repository_root)
        .args(["rev-parse", "--path-format=absolute", "--git-path", path])
        .output()?;
    if !output.status.success() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Git could not resolve metadata path",
        ));
    }
    let value = String::from_utf8(output.stdout).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidData, "Git metadata path is not UTF-8")
    })?;
    let path = PathBuf::from(value.trim());
    if path.as_os_str().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Git metadata path is empty",
        ));
    }
    Ok(path)
}

fn git_symbolic_ref(repository_root: &Path) -> io::Result<Option<PathBuf>> {
    let output = sanitized_git_command(repository_root)
        .args(["symbolic-ref", "-q", "HEAD"])
        .output()?;
    if !output.status.success() {
        return Ok(None);
    }
    let reference = PathBuf::from(
        String::from_utf8(output.stdout)
            .map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "Git symbolic ref is not UTF-8")
            })?
            .trim(),
    );
    if reference.is_absolute()
        || !reference.starts_with("refs")
        || reference.components().any(|part| part.as_os_str() == "..")
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Git symbolic ref is malformed",
        ));
    }
    Ok(Some(reference))
}

fn git_dir_from_marker(marker: &Path) -> io::Result<Option<PathBuf>> {
    let metadata = match fs::symlink_metadata(marker) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            ".git marker must not be a symlink",
        ));
    }
    if metadata.is_dir() {
        return Ok(Some(marker.to_owned()));
    }
    let marker_contents = fs::read_to_string(marker)?;
    let value = marker_contents
        .strip_prefix("gitdir: ")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, ".git file is malformed"))?;
    let candidate = PathBuf::from(value);
    Ok(Some(if candidate.is_absolute() {
        candidate
    } else {
        marker
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, ".git marker has no parent"))?
            .join(candidate)
    }))
}

enum GitHead {
    Detached,
    Reference(PathBuf),
}

impl GitHead {
    fn reference(&self) -> Option<&Path> {
        match self {
            Self::Detached => None,
            Self::Reference(reference) => Some(reference),
        }
    }
}

fn read_git_head(head_path: &Path) -> io::Result<GitHead> {
    let head = fs::read_to_string(head_path)?;
    let value = head.trim();
    let Some(reference) = value.strip_prefix("ref: ") else {
        if is_git_object_id(value) {
            return Ok(GitHead::Detached);
        }
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Git HEAD is malformed",
        ));
    };
    let reference = Path::new(reference);
    if reference.is_absolute()
        || !reference.starts_with("refs")
        || reference.components().any(|part| part.as_os_str() == "..")
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Git HEAD reference escapes metadata",
        ));
    }
    Ok(GitHead::Reference(reference.to_owned()))
}

fn watch_git_reference(
    reference_path: &Path,
    packed: &Path,
    refs_root: &Path,
    reference: &Path,
) -> io::Result<bool> {
    let mut found = false;
    if reference_path.is_file() {
        let value = fs::read_to_string(reference_path)?;
        if !is_git_object_id(value.trim()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Git loose ref is malformed",
            ));
        }
        println!("cargo:rerun-if-changed={}", reference_path.display());
        found = true;
    }
    watch_nearest_existing_ref_parent_path(reference_path, refs_root)?;
    if packed.is_file() {
        println!("cargo:rerun-if-changed={}", packed.display());
        found |= packed_ref_contains(packed, reference)?;
    }
    Ok(found)
}

fn watch_nearest_existing_ref_parent_path(
    reference_path: &Path,
    refs_root: &Path,
) -> io::Result<()> {
    if !reference_path.starts_with(refs_root) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Git active reference escapes the resolved refs directory",
        ));
    }
    let mut parent = reference_path.parent();
    while let Some(candidate) = parent {
        if !candidate.starts_with(refs_root) {
            break;
        }
        if candidate.is_dir() {
            println!("cargo:rerun-if-changed={}", candidate.display());
            return Ok(());
        }
        parent = candidate.parent();
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "Git active reference has no existing refs parent",
    ))
}

fn packed_ref_contains(packed: &Path, reference: &Path) -> io::Result<bool> {
    let reference = reference.to_string_lossy();
    for line in fs::read_to_string(packed)?.lines() {
        if line.starts_with('#') || line.starts_with('^') || line.is_empty() {
            continue;
        }
        let Some((object_id, name)) = line.split_once(' ') else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Git packed-refs is malformed",
            ));
        };
        if name == reference {
            if !is_git_object_id(object_id) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Git packed ref is malformed",
                ));
            }
            return Ok(true);
        }
    }
    Ok(false)
}

fn is_git_object_id(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}
