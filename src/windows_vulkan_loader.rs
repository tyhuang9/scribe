//! Windows-only admission for Scribe's pack-local Vulkan policy loader.
//!
//! A load-time import is mapped before Rust `main`, so this module cannot make
//! a claim about code that may already have run from `DllMain`. It instead
//! enforces the narrower worker invariant: the signed, retained pack sibling is
//! the only mapped `vulkan-1.dll`, and its exact identity is admitted before any
//! Vulkan/provider API is called.

use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{Context, Result, anyhow, bail};
use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_NO_MORE_FILES, HANDLE, HMODULE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT,
    FILE_SHARE_READ, GetFileInformationByHandle,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, MODULEENTRY32W, Module32FirstW, Module32NextW, TH32CS_SNAPMODULE,
    TH32CS_SNAPMODULE32,
};
use windows_sys::Win32::System::LibraryLoader::{
    GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_PIN, GetModuleFileNameW,
    GetModuleHandleExW,
};

use crate::gpu_worker_pack::manifest::{
    PackBackend, VerifiedCopyEntry, VerifiedPackLease, hash_exact_length,
};

include!(concat!(
    env!("OUT_DIR"),
    "/windows_vulkan_policy_loader_identity.rs"
));

const MAX_MAPPED_MODULES: usize = 4096;
const MAX_MODULE_PATH_WCHARS: usize = 32_768;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WindowsFileIdentity {
    volume_serial: u32,
    file_index: u64,
}

#[derive(Debug, Eq, PartialEq)]
enum WindowsAbsolutePrefix {
    Disk(u16),
    Unc { server: Vec<u16>, share: Vec<u16> },
}

#[derive(Debug, Eq, PartialEq)]
struct WindowsAbsolutePath {
    prefix: WindowsAbsolutePrefix,
    components: Vec<Vec<u16>>,
}

struct VerifiedVulkanLoader {
    _file: File,
    module: usize,
}

static VERIFIED_VULKAN_LOADER: OnceLock<VerifiedVulkanLoader> = OnceLock::new();

struct Snapshot(HANDLE);

impl Drop for Snapshot {
    fn drop(&mut self) {
        if self.0 != INVALID_HANDLE_VALUE {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

/// Bind the desktop's launch decision to the exact policy-loader entry in the
/// signed inventory. `VerifiedPackLease` keeps the already-verified payload
/// handles alive after this immediate pre-launch recheck.
pub(crate) fn validate_verified_pack_loader(lease: &VerifiedPackLease) -> Result<()> {
    if lease.verified_pack().backend != PackBackend::Vulkan {
        return Ok(());
    }
    let entry = validated_signed_loader_entry(
        &lease.verified_pack().worker_relative_path,
        lease.copy_entries(),
    )?;
    lease
        .recheck()
        .context("Windows Vulkan pack authority changed before loader admission")?;
    let mut file = lease
        .open_copy_file(entry)
        .context("could not retain the signed Windows Vulkan policy loader")?;
    let digest = hash_exact_length(&mut file, entry.size_bytes, &entry.path)
        .context("could not hash the signed Windows Vulkan policy loader")?;
    if digest != PINNED_VULKAN_LOADER_SHA256 {
        bail!("signed Windows Vulkan policy-loader bytes do not match the reviewed build")
    }
    lease
        .recheck()
        .context("Windows Vulkan pack authority changed during loader admission")
}

fn validated_signed_loader_entry<'a>(
    worker_relative_path: &str,
    entries: &'a [VerifiedCopyEntry],
) -> Result<&'a VerifiedCopyEntry> {
    let expected_path = loader_relative_path(worker_relative_path)?;
    let mut matching = entries.iter().filter(|entry| entry.path == expected_path);
    let entry = matching.next().ok_or_else(|| {
        anyhow!("Windows Vulkan pack inventory omitted its adjacent policy loader")
    })?;
    if matching.next().is_some() {
        bail!("Windows Vulkan pack inventory repeated its adjacent policy loader")
    }
    if entry.size_bytes != PINNED_VULKAN_LOADER_SIZE_BYTES
        || entry.sha256 != PINNED_VULKAN_LOADER_SHA256
    {
        bail!("Windows Vulkan pack policy-loader identity differs from the reviewed build")
    }
    Ok(entry)
}

fn loader_relative_path(worker_relative_path: &str) -> Result<String> {
    let worker = Path::new(worker_relative_path);
    let parent = worker.parent().unwrap_or_else(|| Path::new(""));
    let loader = parent.join(PINNED_VULKAN_LOADER_FILENAME);
    let mut names = Vec::new();
    for component in loader.components() {
        let Component::Normal(name) = component else {
            bail!("Windows Vulkan worker path cannot locate an adjacent policy loader")
        };
        let name = name
            .to_str()
            .ok_or_else(|| anyhow!("Windows Vulkan policy-loader path is not Unicode"))?;
        names.push(name);
    }
    if names.is_empty() {
        bail!("Windows Vulkan policy-loader path is empty")
    }
    Ok(names.join("/"))
}

/// Admit and retain the already-mapped loader. This deliberately never calls
/// `LoadLibrary`: a missing import is a build/pack failure, not a search cue.
pub(crate) fn bootstrap_mapped_policy_loader() -> Result<()> {
    if VERIFIED_VULKAN_LOADER.get().is_some() {
        return Ok(());
    }

    let module = unique_mapped_policy_loader()?;
    let module = pin_module(module)?;
    let mapped_path = module_path(module)?;
    let executable =
        std::env::current_exe().context("could not locate the Windows Vulkan worker executable")?;
    let expected_path = executable
        .parent()
        .ok_or_else(|| anyhow!("Windows Vulkan worker executable has no parent directory"))?
        .join(PINNED_VULKAN_LOADER_FILENAME);
    if !strict_windows_absolute_path_eq(&mapped_path, &expected_path) {
        bail!("mapped Vulkan loader is not the exact pack-adjacent absolute path")
    }

    let mapped_file = open_regular_no_follow(&mapped_path).with_context(|| {
        format!(
            "could not open mapped Vulkan loader {}",
            mapped_path.display()
        )
    })?;
    let mapped_identity = regular_file_identity(&mapped_file, &mapped_path)?;
    let mut expected_file = open_regular_no_follow(&expected_path).with_context(|| {
        format!(
            "could not open adjacent Vulkan policy loader {}",
            expected_path.display()
        )
    })?;
    let expected_identity = regular_file_identity(&expected_file, &expected_path)?;
    if mapped_identity != expected_identity {
        bail!("mapped Vulkan loader is not the exact pack-adjacent policy-loader file")
    }
    let metadata = expected_file
        .metadata()
        .context("could not inspect adjacent Vulkan policy-loader size")?;
    if metadata.len() != PINNED_VULKAN_LOADER_SIZE_BYTES {
        bail!("adjacent Vulkan policy-loader size differs from the reviewed build")
    }
    let digest = hash_exact_length(
        &mut expected_file,
        PINNED_VULKAN_LOADER_SIZE_BYTES,
        PINNED_VULKAN_LOADER_FILENAME,
    )
    .context("could not hash the mapped Windows Vulkan policy loader")?;
    if digest != PINNED_VULKAN_LOADER_SHA256 {
        bail!("mapped Vulkan policy-loader bytes differ from the reviewed build")
    }

    let verified = VerifiedVulkanLoader {
        _file: expected_file,
        module: module as usize,
    };
    if VERIFIED_VULKAN_LOADER.set(verified).is_err() && VERIFIED_VULKAN_LOADER.get().is_none() {
        bail!("could not retain the verified Windows Vulkan policy loader")
    }
    Ok(())
}

pub(crate) fn require_policy_loader() -> Result<()> {
    if VERIFIED_VULKAN_LOADER.get().is_none() {
        bail!("Windows Vulkan provider initialization preceded policy-loader admission")
    }
    Ok(())
}

pub(crate) fn validate_fixed_layer_disable(value: Option<&OsStr>) -> Result<()> {
    if value != Some(OsStr::new("~all~")) {
        bail!("Windows Vulkan pack requires the fixed layer-disable policy")
    }
    Ok(())
}

#[cfg(feature = "vulkan-acceleration")]
pub(crate) fn ash_entry(require_policy_loader: bool) -> Result<ash::Entry> {
    use windows_sys::Win32::System::LibraryLoader::GetProcAddress;

    if let Some(verified) = VERIFIED_VULKAN_LOADER.get() {
        let symbol = unsafe {
            GetProcAddress(
                verified.module as HMODULE,
                c"vkGetInstanceProcAddr".as_ptr().cast(),
            )
        }
        .ok_or_else(|| anyhow!("verified Vulkan policy loader omitted vkGetInstanceProcAddr"))?;
        let get_instance_proc_addr = unsafe {
            std::mem::transmute::<
                unsafe extern "system" fn() -> isize,
                ash::vk::PFN_vkGetInstanceProcAddr,
            >(symbol)
        };
        let static_fn = ash::vk::StaticFn {
            get_instance_proc_addr,
        };
        // SAFETY: the function comes from the exact verified module, which was
        // pinned and whose file handle remains live for the process lifetime.
        return Ok(unsafe { ash::Entry::from_static_fn(static_fn) });
    }
    if require_policy_loader {
        bail!("Windows Vulkan identity discovery preceded policy-loader admission")
    }
    // Pack-less developer Vulkan builds retain their existing SDK/system-loader
    // behavior; production pack discovery always takes the verified branch.
    unsafe { ash::Entry::load() }.context("could not load the Windows Vulkan loader")
}

fn unique_mapped_policy_loader() -> Result<HMODULE> {
    let snapshot = module_snapshot()?;
    let mut entry = unsafe { std::mem::zeroed::<MODULEENTRY32W>() };
    entry.dwSize =
        u32::try_from(size_of::<MODULEENTRY32W>()).expect("Windows module entry size fits in u32");
    if unsafe { Module32FirstW(snapshot.0, &mut entry) } == 0 {
        return Err(std::io::Error::last_os_error())
            .context("could not enumerate mapped Windows worker modules");
    }

    let mut observed = 0_usize;
    let mut matches = Vec::new();
    loop {
        observed += 1;
        if observed > MAX_MAPPED_MODULES {
            bail!("Windows worker mapped-module list exceeded its safety bound")
        }
        if wide_module_name_matches(&entry.szModule, PINNED_VULKAN_LOADER_FILENAME) {
            matches.push(entry.hModule);
        }
        if unsafe { Module32NextW(snapshot.0, &mut entry) } == 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                break;
            }
            return Err(error).context("could not continue mapped Windows module enumeration");
        }
    }
    if matches.len() != 1 {
        bail!(
            "Windows Vulkan worker requires exactly one already-mapped {}, found {}",
            PINNED_VULKAN_LOADER_FILENAME,
            matches.len()
        )
    }
    Ok(matches[0])
}

fn module_snapshot() -> Result<Snapshot> {
    let mut last_error = None;
    for _ in 0..3 {
        let handle =
            unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, 0) };
        if handle != INVALID_HANDLE_VALUE {
            return Ok(Snapshot(handle));
        }
        last_error = Some(std::io::Error::last_os_error());
    }
    Err(last_error.unwrap_or_else(std::io::Error::last_os_error))
        .context("could not snapshot mapped Windows worker modules")
}

fn wide_module_name_matches(value: &[u16], expected: &str) -> bool {
    let end = value
        .iter()
        .position(|character| *character == 0)
        .unwrap_or(value.len());
    OsString::from_wide(&value[..end])
        .to_string_lossy()
        .eq_ignore_ascii_case(expected)
}

fn strict_windows_absolute_path_eq(left: &Path, right: &Path) -> bool {
    let Some(left) = parse_strict_windows_absolute_path(left.as_os_str()) else {
        return false;
    };
    let Some(right) = parse_strict_windows_absolute_path(right.as_os_str()) else {
        return false;
    };
    // NTFS can enable case sensitivity per directory. Folding a component
    // could admit a different, retargetable ancestor outside the retained path.
    windows_prefix_eq(&left.prefix, &right.prefix) && left.components == right.components
}

fn parse_strict_windows_absolute_path(path: &OsStr) -> Option<WindowsAbsolutePath> {
    const BACKSLASH: u16 = b'\\' as u16;
    const COLON: u16 = b':' as u16;
    const QUESTION: u16 = b'?' as u16;

    let value = path.encode_wide().collect::<Vec<_>>();
    if value.is_empty() || value.contains(&0) {
        return None;
    }

    let (prefix, rest) = if value.len() >= 7
        && value[..4] == [BACKSLASH, BACKSLASH, QUESTION, BACKSLASH]
        && wide_eq_ascii_case_insensitive(&value[4..7], &[b'U' as u16, b'N' as u16, b'C' as u16])
        && value.get(7) == Some(&BACKSLASH)
    {
        parse_unc_prefix(&value[8..])?
    } else if value.len() >= 7
        && value[..4] == [BACKSLASH, BACKSLASH, QUESTION, BACKSLASH]
        && wide_is_ascii_alphabetic(value[4])
        && value[5] == COLON
        && value[6] == BACKSLASH
    {
        (WindowsAbsolutePrefix::Disk(value[4]), &value[7..])
    } else if value.len() >= 3
        && wide_is_ascii_alphabetic(value[0])
        && value[1] == COLON
        && value[2] == BACKSLASH
    {
        (WindowsAbsolutePrefix::Disk(value[0]), &value[3..])
    } else if value.len() >= 2
        && value[..2] == [BACKSLASH, BACKSLASH]
        && value.get(2) != Some(&QUESTION)
    {
        parse_unc_prefix(&value[2..])?
    } else {
        return None;
    };

    let components = parse_strict_windows_components(rest)?;
    Some(WindowsAbsolutePath { prefix, components })
}

fn parse_unc_prefix(value: &[u16]) -> Option<(WindowsAbsolutePrefix, &[u16])> {
    const BACKSLASH: u16 = b'\\' as u16;

    let server_end = value.iter().position(|value| *value == BACKSLASH)?;
    let server = &value[..server_end];
    let after_server = &value[server_end + 1..];
    let share_end = after_server.iter().position(|value| *value == BACKSLASH)?;
    let share = &after_server[..share_end];
    if !strict_windows_component(server) || !strict_windows_component(share) {
        return None;
    }
    Some((
        WindowsAbsolutePrefix::Unc {
            server: server.to_vec(),
            share: share.to_vec(),
        },
        &after_server[share_end + 1..],
    ))
}

fn parse_strict_windows_components(value: &[u16]) -> Option<Vec<Vec<u16>>> {
    const BACKSLASH: u16 = b'\\' as u16;

    if value.is_empty() {
        return None;
    }
    let mut components = Vec::new();
    for component in value.split(|value| *value == BACKSLASH) {
        if !strict_windows_component(component) {
            return None;
        }
        components.push(component.to_vec());
    }
    Some(components)
}

fn strict_windows_component(value: &[u16]) -> bool {
    const DOT: u16 = b'.' as u16;
    const SPACE: u16 = b' ' as u16;

    if value.is_empty()
        || value == [DOT]
        || value == [DOT, DOT]
        || value
            .last()
            .is_some_and(|value| *value == DOT || *value == SPACE)
        || value.iter().any(|value| {
            *value < 0x20
                || matches!(
                    *value,
                    0x22 | 0x2a | 0x2f | 0x3a | 0x3c | 0x3e | 0x3f | 0x7c
                )
        })
    {
        return false;
    }
    !is_dos_device_name(value)
}

fn is_dos_device_name(value: &[u16]) -> bool {
    const DOT: u16 = b'.' as u16;

    let stem_end = value
        .iter()
        .position(|value| *value == DOT)
        .unwrap_or(value.len());
    let stem = &value[..stem_end];
    if ["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$", "CLOCK$"]
        .iter()
        .any(|reserved| wide_eq_ascii_case_insensitive_str(stem, reserved))
    {
        return true;
    }
    if stem.len() != 4
        || (!wide_eq_ascii_case_insensitive_str(&stem[..3], "COM")
            && !wide_eq_ascii_case_insensitive_str(&stem[..3], "LPT"))
    {
        return false;
    }
    (u16::from(b'1')..=u16::from(b'9')).contains(&stem[3])
        || matches!(stem[3], 0x00b9 | 0x00b2 | 0x00b3)
}

fn windows_prefix_eq(left: &WindowsAbsolutePrefix, right: &WindowsAbsolutePrefix) -> bool {
    match (left, right) {
        (WindowsAbsolutePrefix::Disk(left), WindowsAbsolutePrefix::Disk(right)) => {
            wide_eq_ascii_case_insensitive(std::slice::from_ref(left), std::slice::from_ref(right))
        }
        (
            WindowsAbsolutePrefix::Unc {
                server: left_server,
                share: left_share,
            },
            WindowsAbsolutePrefix::Unc {
                server: right_server,
                share: right_share,
            },
        ) => left_server == right_server && left_share == right_share,
        _ => false,
    }
}

fn wide_eq_ascii_case_insensitive(left: &[u16], right: &[u16]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| wide_ascii_lower(*left) == wide_ascii_lower(*right))
}

fn wide_eq_ascii_case_insensitive_str(left: &[u16], right: &str) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right.bytes())
            .all(|(left, right)| wide_ascii_lower(*left) == u16::from(right.to_ascii_lowercase()))
}

fn wide_is_ascii_alphabetic(value: u16) -> bool {
    (u16::from(b'A')..=u16::from(b'Z')).contains(&value)
        || (u16::from(b'a')..=u16::from(b'z')).contains(&value)
}

fn wide_ascii_lower(value: u16) -> u16 {
    if (u16::from(b'A')..=u16::from(b'Z')).contains(&value) {
        value + u16::from(b'a' - b'A')
    } else {
        value
    }
}

fn pin_module(module: HMODULE) -> Result<HMODULE> {
    let mut pinned = std::ptr::null_mut();
    if unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_PIN,
            module.cast(),
            &mut pinned,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error())
            .context("could not pin the mapped Windows Vulkan policy loader");
    }
    if pinned != module {
        bail!("Windows pinned a different module than the enumerated Vulkan loader")
    }
    Ok(pinned)
}

fn module_path(module: HMODULE) -> Result<PathBuf> {
    let mut buffer = vec![0_u16; MAX_MODULE_PATH_WCHARS];
    let length = unsafe {
        GetModuleFileNameW(
            module,
            buffer.as_mut_ptr(),
            u32::try_from(buffer.len()).expect("module path bound fits in u32"),
        )
    };
    if length == 0 {
        return Err(std::io::Error::last_os_error())
            .context("could not obtain the mapped Vulkan loader path");
    }
    if length as usize >= buffer.len() {
        bail!("mapped Vulkan loader path exceeded its safety bound")
    }
    buffer.truncate(length as usize);
    Ok(PathBuf::from(OsString::from_wide(&buffer)))
}

fn open_regular_no_follow(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        bail!("Windows Vulkan policy-loader path is not a regular non-reparse file")
    }
    Ok(file)
}

fn regular_file_identity(file: &File, path: &Path) -> Result<WindowsFileIdentity> {
    let mut information = unsafe { std::mem::zeroed::<BY_HANDLE_FILE_INFORMATION>() };
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) } == 0 {
        return Err(std::io::Error::last_os_error()).with_context(|| {
            format!(
                "could not read Windows file identity for {}",
                path.display()
            )
        });
    }
    if information.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        bail!("Windows Vulkan policy loader is a reparse point")
    }
    if information.nNumberOfLinks != 1 {
        bail!("Windows Vulkan policy loader must not be a hardlink")
    }
    Ok(WindowsFileIdentity {
        volume_serial: information.dwVolumeSerialNumber,
        file_index: (u64::from(information.nFileIndexHigh) << 32)
            | u64::from(information.nFileIndexLow),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, size_bytes: u64, sha256: &str) -> VerifiedCopyEntry {
        VerifiedCopyEntry {
            path: path.to_owned(),
            size_bytes,
            sha256: sha256.to_owned(),
        }
    }

    #[test]
    fn generated_identity_matches_the_reviewed_source_manifest() {
        let manifest: serde_json::Value = serde_json::from_str(include_str!(
            "../native/vulkan-policy-loader/source-manifest.json"
        ))
        .unwrap();
        let artifact = manifest.get("artifact").unwrap();
        assert_eq!(
            artifact.get("filename").and_then(serde_json::Value::as_str),
            Some(PINNED_VULKAN_LOADER_FILENAME)
        );
        assert_eq!(
            artifact
                .get("size_bytes")
                .and_then(serde_json::Value::as_u64),
            Some(PINNED_VULKAN_LOADER_SIZE_BYTES)
        );
        assert_eq!(
            artifact.get("sha256").and_then(serde_json::Value::as_str),
            Some(PINNED_VULKAN_LOADER_SHA256)
        );
    }

    #[test]
    fn signed_loader_entry_must_be_the_exact_reviewed_worker_sibling() {
        let root_entry = entry(
            PINNED_VULKAN_LOADER_FILENAME,
            PINNED_VULKAN_LOADER_SIZE_BYTES,
            PINNED_VULKAN_LOADER_SHA256,
        );
        assert_eq!(
            validated_signed_loader_entry(
                "scribe-inference-worker.exe",
                std::slice::from_ref(&root_entry),
            )
            .unwrap(),
            &root_entry
        );

        let nested_path = format!("bin/{PINNED_VULKAN_LOADER_FILENAME}");
        let nested_entry = entry(
            &nested_path,
            PINNED_VULKAN_LOADER_SIZE_BYTES,
            PINNED_VULKAN_LOADER_SHA256,
        );
        assert_eq!(
            validated_signed_loader_entry("bin/scribe-inference-worker.exe", &[nested_entry])
                .unwrap()
                .path,
            nested_path
        );
    }

    #[test]
    fn signed_loader_entry_rejects_missing_or_changed_identity() {
        assert!(
            validated_signed_loader_entry("scribe-inference-worker.exe", &[])
                .unwrap_err()
                .to_string()
                .contains("omitted")
        );
        for changed in [
            entry(
                PINNED_VULKAN_LOADER_FILENAME,
                PINNED_VULKAN_LOADER_SIZE_BYTES + 1,
                PINNED_VULKAN_LOADER_SHA256,
            ),
            entry(
                PINNED_VULKAN_LOADER_FILENAME,
                PINNED_VULKAN_LOADER_SIZE_BYTES,
                &"0".repeat(64),
            ),
            entry(
                &format!("other/{PINNED_VULKAN_LOADER_FILENAME}"),
                PINNED_VULKAN_LOADER_SIZE_BYTES,
                PINNED_VULKAN_LOADER_SHA256,
            ),
        ] {
            assert!(
                validated_signed_loader_entry("scribe-inference-worker.exe", &[changed]).is_err()
            );
        }

        let duplicate = entry(
            PINNED_VULKAN_LOADER_FILENAME,
            PINNED_VULKAN_LOADER_SIZE_BYTES,
            PINNED_VULKAN_LOADER_SHA256,
        );
        assert!(
            validated_signed_loader_entry(
                "scribe-inference-worker.exe",
                &[duplicate.clone(), duplicate]
            )
            .unwrap_err()
            .to_string()
            .contains("repeated")
        );
    }

    #[test]
    fn module_name_matching_is_ascii_case_insensitive_and_nul_bounded() {
        let mut exact = [0_u16; 32];
        for (slot, value) in exact.iter_mut().zip("VULKAN-1.DLL".encode_utf16()) {
            *slot = value;
        }
        assert!(wide_module_name_matches(
            &exact,
            PINNED_VULKAN_LOADER_FILENAME
        ));
        exact[0] = 'x' as u16;
        assert!(!wide_module_name_matches(
            &exact,
            PINNED_VULKAN_LOADER_FILENAME
        ));
    }

    #[test]
    fn malformed_worker_relative_paths_cannot_select_a_loader() {
        for worker in ["../worker.exe", "/worker.exe", "C:/worker.exe"] {
            assert!(loader_relative_path(worker).is_err(), "accepted {worker}");
        }
    }

    #[test]
    fn strict_absolute_path_comparison_accepts_only_drive_case_and_supported_prefix_spelling() {
        assert!(strict_windows_absolute_path_eq(
            Path::new(r"C:\Scribe\Pack\vulkan-1.dll"),
            Path::new(r"\\?\c:\Scribe\Pack\vulkan-1.dll")
        ));
        assert!(strict_windows_absolute_path_eq(
            Path::new(r"\\server\share\Scribe\vulkan-1.dll"),
            Path::new(r"\\?\UNC\server\share\Scribe\vulkan-1.dll")
        ));
        assert!(strict_windows_absolute_path_eq(
            Path::new(r"C:\Users\黄 Name\Pack\vulkan-1.dll"),
            Path::new(r"\\?\C:\Users\黄 Name\Pack\vulkan-1.dll")
        ));
    }

    #[test]
    fn strict_absolute_path_comparison_rejects_case_distinct_ancestry() {
        assert!(!strict_windows_absolute_path_eq(
            Path::new(r"C:\Scribe\Pack\vulkan-1.dll"),
            Path::new(r"C:\Scribe\pack\vulkan-1.dll")
        ));
        assert!(!strict_windows_absolute_path_eq(
            Path::new(r"\\server\Share\Pack\vulkan-1.dll"),
            Path::new(r"\\?\UNC\server\share\Pack\vulkan-1.dll")
        ));
        assert!(!strict_windows_absolute_path_eq(
            Path::new(r"\\Server\share\Pack\vulkan-1.dll"),
            Path::new(r"\\?\UNC\server\share\Pack\vulkan-1.dll")
        ));
    }

    #[test]
    fn strict_absolute_path_comparison_rejects_alias_dot_and_relative_paths() {
        let expected = Path::new(r"C:\Scribe\Pack\vulkan-1.dll");
        for changed in [
            r"C:\Scribe\PACKAG~1\vulkan-1.dll",
            r"C:\Scribe\Pack\.\vulkan-1.dll",
            r"C:\Scribe\Pack\sub\..\vulkan-1.dll",
            r"C:\Scribe\\Pack\vulkan-1.dll",
            r"Scribe\Pack\vulkan-1.dll",
            r"\\.\C:\Scribe\Pack\vulkan-1.dll",
            r"\\?\Volume{00000000-0000-0000-0000-000000000000}\vulkan-1.dll",
        ] {
            assert!(
                !strict_windows_absolute_path_eq(Path::new(changed), expected),
                "accepted {changed}"
            );
        }
        for malformed in [
            r"C:\Scribe\Pack\.\vulkan-1.dll",
            r"C:\Scribe\Pack\sub\..\vulkan-1.dll",
            r"C:\Scribe\\Pack\vulkan-1.dll",
            r"Scribe\Pack\vulkan-1.dll",
            r"\\.\C:\Scribe\Pack\vulkan-1.dll",
            r"\\?\Volume{00000000-0000-0000-0000-000000000000}\vulkan-1.dll",
        ] {
            assert!(
                !strict_windows_absolute_path_eq(Path::new(malformed), Path::new(malformed)),
                "accepted malformed namespace {malformed}"
            );
        }
    }

    #[test]
    fn supported_prefix_equivalence_rejects_win32_normalization_sensitive_components() {
        for component in [
            "Pack.",
            "Pack ",
            "Pa<ck",
            "Pa>ck",
            "Pa\"ck",
            "Pa/ck",
            "Pa|ck",
            "Pa?ck",
            "Pa*ck",
            "Pa:ck",
            "Pa\u{001f}ck",
            "CON",
            "prn.txt",
            "AuX.log",
            "NUL.tar.gz",
            "COM1.bin",
            "lpt9.txt",
            "COM¹.log",
            "LPT².log",
            "com³.log",
            "CONIN$",
            "CONOUT$.txt",
            "CLOCK$",
        ] {
            let normal = PathBuf::from(format!(
                r"C:\Scribe\{component}\{PINNED_VULKAN_LOADER_FILENAME}"
            ));
            let verbatim = PathBuf::from(format!(
                r"\\?\C:\Scribe\{component}\{PINNED_VULKAN_LOADER_FILENAME}"
            ));
            assert!(
                !strict_windows_absolute_path_eq(&normal, &verbatim),
                "accepted normalization-sensitive component {component:?}"
            );
        }
    }

    #[test]
    fn fixed_layer_disable_rejects_missing_or_override_values() {
        assert!(validate_fixed_layer_disable(Some(OsStr::new("~all~"))).is_ok());
        assert!(validate_fixed_layer_disable(None).is_err());
        assert!(validate_fixed_layer_disable(Some(OsStr::new("all"))).is_err());
        assert!(validate_fixed_layer_disable(Some(OsStr::new("~all~,*"))).is_err());
    }

    #[test]
    fn pinned_filename_is_a_plain_windows_dll_name() {
        assert_eq!(
            Path::new(PINNED_VULKAN_LOADER_FILENAME).file_name(),
            Some(OsStr::new(PINNED_VULKAN_LOADER_FILENAME))
        );
        assert_eq!(
            Path::new(PINNED_VULKAN_LOADER_FILENAME)
                .components()
                .count(),
            1
        );
    }
}
