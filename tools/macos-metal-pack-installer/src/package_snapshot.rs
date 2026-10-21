//! Build-only frozen bytes/shape, NOT signature, release or execution admission.
//! Reviewed source/toolchain/Cargo output and non-hostile same-UID/root are trusted.
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub(crate) const MANIFEST_MAX: u64 = 256 * 1024;
pub(crate) const SIGNATURE_MAX: u64 = 4 * 1024;
pub(crate) const WORKER_MAX: u64 = 2 * 1024 * 1024 * 1024;
pub(crate) const OUTPUTS: [&str; 3] = [
    "package-manifest.json",
    "package-signature.json",
    "package-worker.bin",
];

fn sha_valid(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub(crate) fn validate_path(path: &Path) -> Result<()> {
    let name = path.to_str().context("build path must be Unicode")?;
    // Both Windows separators are native spellings; normalizing separators
    // here is lexical only and never resolves a link or a parent component.
    #[cfg(windows)]
    let spelling = name.replace('/', "\\");
    #[cfg(not(windows))]
    let spelling = name.to_owned();
    let spelling_path = Path::new(&spelling);
    if !path.is_absolute()
        || name.len() > 32767
        || name.bytes().any(|b| b.is_ascii_control())
        || spelling_path.components().any(|part| {
            matches!(
                part,
                std::path::Component::CurDir | std::path::Component::ParentDir
            )
        })
        || spelling_path.components().collect::<PathBuf>().as_os_str() != spelling_path.as_os_str()
    {
        bail!("Metal package build path must be absolute and directive-safe");
    }
    #[cfg(windows)]
    for part in spelling_path.components() {
        if let std::path::Component::Normal(name) = part
            && (name.to_string_lossy().contains(':')
                || name.to_string_lossy().ends_with(['.', ' ']))
        {
            bail!("alternate-stream build input");
        }
    }
    Ok(())
}
fn check_node(metadata: &Metadata, directory: bool) -> Result<()> {
    if metadata.file_type().is_symlink()
        || (directory && !metadata.is_dir())
        || (!directory && !metadata.is_file())
    {
        bail!("Metal package build input is not an unlinked regular node");
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            bail!("reparse-point build input");
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if !directory && metadata.nlink() != 1 {
            bail!("hard-linked build input");
        }
    }
    Ok(())
}
fn ancestors(path: &Path) -> Result<()> {
    validate_path(path)?;
    for parent in path.ancestors().skip(1) {
        check_node(&fs::symlink_metadata(parent)?, true)?;
    }
    Ok(())
}
fn length(length: u64, maximum: u64) -> Result<()> {
    if length == 0 || length > maximum {
        bail!("Metal package build input exceeds its byte bound");
    }
    Ok(())
}
fn open_regular(path: &Path, maximum: u64) -> Result<(File, u64)> {
    ancestors(path)?;
    let before = fs::symlink_metadata(path)?;
    check_node(&before, false)?;
    length(before.len(), maximum)?;
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000);
    }
    let file = options.open(path)?;
    let opened = file.metadata()?;
    check_node(&opened, false)?;
    if opened.len() != before.len() {
        bail!("build input changed while opening");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if (opened.dev(), opened.ino()) != (before.dev(), before.ino()) {
            bail!("build input identity changed");
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        if info.nNumberOfLinks != 1 {
            bail!("hard-linked build input");
        }
    }
    Ok((file, before.len()))
}
fn hash_reader(file: &mut File, expected_length: u64) -> Result<String> {
    let mut digest = Sha256::new();
    let mut remaining = expected_length;
    let mut buffer = [0u8; 64 * 1024];
    while remaining > 0 {
        let count = file.read(&mut buffer[..remaining.min(65536) as usize])?;
        if count == 0 {
            bail!("truncated Metal package build input");
        }
        digest.update(&buffer[..count]);
        remaining -= count as u64;
    }
    if file.read(&mut buffer[..1])? != 0 {
        bail!("growing Metal package build input");
    }
    Ok(format!("{:x}", digest.finalize()))
}
pub(crate) fn read_input(path: &Path, maximum: u64) -> Result<Vec<u8>> {
    let (file, size) = open_regular(path, maximum)?;
    let mut bytes = Vec::new();
    file.take(size + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 != size {
        bail!("release input changed length");
    }
    Ok(bytes)
}
fn copy_snapshot(
    source: &Path,
    destination: &Path,
    maximum: u64,
    expected: Option<(&str, Option<u64>)>,
) -> Result<()> {
    let (mut input, size) = open_regular(source, maximum)?;
    let digest = hash_reader(&mut input, size)?;
    if let Some((expected, expected_size)) = expected
        && (digest != expected || expected_size.is_some_and(|length| length != size))
    {
        bail!("Metal package frozen input mismatch");
    }
    input.seek(SeekFrom::Start(0))?;
    #[cfg(test)]
    AFTER_HASH.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() {
            hook(source);
        }
    });
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    if std::io::copy(
        &mut std::io::Read::by_ref(&mut input).take(size + 1),
        &mut output,
    )? != size
    {
        bail!("Metal build input changed while copying");
    }
    output.flush()?;
    output.sync_all()?;
    drop(output);
    let (mut reopened, copied_size) = open_regular(destination, maximum)?;
    if copied_size != size || hash_reader(&mut reopened, copied_size)? != digest {
        bail!("Metal package snapshot readback mismatch");
    }
    Ok(())
}
fn names(path: &Path, expected: &[&str]) -> Result<()> {
    ancestors(path)?;
    check_node(&fs::symlink_metadata(path)?, true)?;
    let mut names = fs::read_dir(path)?
        .take(expected.len() + 1)
        .map(|entry| Ok(entry?.file_name()))
        .collect::<std::io::Result<Vec<_>>>()?;
    names.sort();
    if names
        != expected
            .iter()
            .map(std::ffi::OsString::from)
            .collect::<Vec<_>>()
    {
        bail!("unexpected Metal package build inventory");
    }
    Ok(())
}
pub(crate) fn require_release_inputs(catalog: &[u8], authority: &[u8]) -> Result<()> {
    // Shape rejection only. Full canonical release binding is mandatory at
    // runtime through admit_embedded, not duplicated in this build helper.
    let catalog: serde_json::Value = serde_json::from_slice(catalog)?;
    let authority: serde_json::Value = serde_json::from_slice(authority)?;
    if catalog["packs"]
        .as_array()
        .is_none_or(|packs| packs.is_empty())
        || authority["entries"]
            .as_array()
            .is_none_or(|entries| entries.is_empty())
        || !authority["release_security_epoch"]
            .as_u64()
            .is_some_and(|epoch| epoch > 0 && epoch <= 9_007_199_254_740_991)
    {
        bail!("Metal package entry requires nonempty explicit release inputs");
    }
    Ok(())
}
pub(crate) fn snapshot(
    source: &Path,
    destination: &Path,
    manifest_sha: &str,
    target_arch: &str,
) -> Result<()> {
    if !sha_valid(manifest_sha) {
        bail!("Metal package requires the frozen manifest SHA-256");
    }
    if !matches!(target_arch, "aarch64" | "x86_64") {
        bail!("unsupported Metal package target architecture");
    }
    names(
        source,
        &["pack-manifest.json", "pack-manifest.sig", "worker"],
    )?;
    names(&source.join("worker"), &["scribe-inference-worker"])?;
    validate_path(destination)?;
    fs::create_dir(destination)?; // Fresh caller-owned output, never adopt/reuse.
    copy_snapshot(
        &source.join("pack-manifest.json"),
        &destination.join(OUTPUTS[0]),
        MANIFEST_MAX,
        Some((manifest_sha, None)),
    )?;
    let manifest: serde_json::Value =
        serde_json::from_slice(&read_input(&destination.join(OUTPUTS[0]), MANIFEST_MAX)?)?;
    let payload = manifest["payload"]
        .as_array()
        .context("missing Metal worker inventory")?;
    if manifest["target_arch"] != target_arch
        || manifest["target_os"] != "macos"
        || manifest["backend"] != "metal"
        || manifest["worker_path"] != "worker/scribe-inference-worker"
        || payload.len() != 1
        || payload[0]["path"] != "worker/scribe-inference-worker"
    {
        bail!("Metal package snapshot has incompatible target or inventory");
    }
    let worker_size = payload[0]["size_bytes"]
        .as_u64()
        .context("missing Metal worker size")?;
    length(worker_size, WORKER_MAX)?;
    let worker_sha = payload[0]["sha256"]
        .as_str()
        .filter(|value| sha_valid(value))
        .context("invalid Metal worker digest")?;
    copy_snapshot(
        &source.join("pack-manifest.sig"),
        &destination.join(OUTPUTS[1]),
        SIGNATURE_MAX,
        None,
    )?;
    copy_snapshot(
        &source.join("worker/scribe-inference-worker"),
        &destination.join(OUTPUTS[2]),
        WORKER_MAX,
        Some((worker_sha, Some(worker_size))),
    )?;
    names(
        source,
        &["pack-manifest.json", "pack-manifest.sig", "worker"],
    )?;
    names(&source.join("worker"), &["scribe-inference-worker"])?;
    Ok(())
}

#[cfg(test)]
type HashHook = Box<dyn FnMut(&Path)>;
#[cfg(test)]
thread_local! { static AFTER_HASH: std::cell::RefCell<Option<HashHook>> = const { std::cell::RefCell::new(None) }; }
#[cfg(test)]
#[path = "package_snapshot_tests.rs"]
mod tests;
