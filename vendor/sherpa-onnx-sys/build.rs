use std::env;
use std::error::Error;
use std::ffi::OsStr;
use std::fs;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::{collections::HashSet, ffi::OsString};

use bzip2::read::BzDecoder;
use sha2::{Digest, Sha256};
use tar::Archive;

const RELEASE_BASE_URL: &str = "https://github.com/k2-fsa/sherpa-onnx/releases/download";
const SHERPA_ONNX_STATIC_LIBS: &[&str] = &[
    "sherpa-onnx-c-api",
    "sherpa-onnx-core",
    "kaldi-decoder-core",
    "sherpa-onnx-kaldifst-core",
    "sherpa-onnx-fstfar",
    "sherpa-onnx-fst",
    "kaldi-native-fbank-core",
    "kissfft-float",
    "piper_phonemize",
    "espeak-ng",
    "ucd",
    "onnxruntime",
    "ssentencepiece_core",
];

type DynError = Box<dyn Error>;

/// Immutable release metadata reviewed by Scribe. Do not accept an archive
/// merely because its name resembles a sherpa-onnx release asset.
#[derive(Clone, Copy)]
struct ArchiveIntegrity {
    name: &'static str,
    size: u64,
    sha256: &'static str,
}

const STATIC_ARCHIVES: &[ArchiveIntegrity] = &[
    ArchiveIntegrity {
        name: "sherpa-onnx-v1.13.5-win-x64-static-MT-Release-lib.tar.bz2",
        size: 120_217_991,
        sha256: "b7080b6f470bac96ef0afe56b25ae9b2f9f0ca82d10dad19bf3a2fc5ffd6cffc",
    },
    ArchiveIntegrity {
        name: "sherpa-onnx-v1.13.5-linux-x64-static-lib.tar.bz2",
        size: 22_394_054,
        sha256: "2ade8b7c62de66b9cf2e32bd7dbe077addaa4b18f422b49dc1bf3a1a0b1f762e",
    },
    ArchiveIntegrity {
        name: "sherpa-onnx-v1.13.5-linux-aarch64-static-lib.tar.bz2",
        size: 20_775_501,
        sha256: "f78af8260892f3060c8c0aba9ae93e4e4c1b16fe509238b88e3688889235e1b2",
    },
    ArchiveIntegrity {
        name: "sherpa-onnx-v1.13.5-osx-x64-static-lib.tar.bz2",
        size: 19_623_101,
        sha256: "689f8167a52dc4dbaf05369705e26c8f203c748a8c342750fdfdcd8ca6bb8699",
    },
    ArchiveIntegrity {
        name: "sherpa-onnx-v1.13.5-osx-arm64-static-lib.tar.bz2",
        size: 19_862_746,
        sha256: "339c8fc19bb4b26e118c80792bbc4546eb263040fac36ef0cc027ec29c756b44",
    },
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LinkMode {
    Static,
    Shared,
}

fn main() {
    if let Err(err) = try_main() {
        panic!("{err}");
    }
}

fn try_main() -> Result<(), DynError> {
    println!("cargo:rerun-if-env-changed=SHERPA_ONNX_LIB_DIR");
    println!("cargo:rerun-if-env-changed=SHERPA_ONNX_ARCHIVE_DIR");
    println!("cargo:rerun-if-env-changed=DOCS_RS");

    if env::var_os("DOCS_RS").is_some() {
        // docs.rs sets DOCS_RS=1; skip downloading/linking native libraries
        // so that `cargo doc` can succeed without the real C artifacts.
        return Ok(());
    }

    let target_os = env::var("CARGO_CFG_TARGET_OS")?;
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH")?;
    let link_mode = resolve_link_mode()?;
    let lib_dir = resolve_lib_dir(link_mode, &target_os, &target_arch)?;

    println!("cargo:rustc-link-search=native={}", lib_dir.display());

    if link_mode == LinkMode::Shared
        && matches!(target_os.as_str(), "linux" | "macos" | "android")
    {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib_dir.display());
        emit_relative_rpath(&target_os);
        copy_unix_runtime_libs(&lib_dir, &target_os)?;
    }

    if link_mode == LinkMode::Shared && target_os == "windows" {
        copy_windows_runtime_dlls(&lib_dir)?;
    }

    match link_mode {
        LinkMode::Static => emit_static_link_directives(&target_os),
        LinkMode::Shared => emit_shared_link_directives(),
    }

    Ok(())
}

fn resolve_link_mode() -> Result<LinkMode, DynError> {
    let static_enabled = env::var_os("CARGO_FEATURE_STATIC").is_some();
    let shared_enabled = env::var_os("CARGO_FEATURE_SHARED").is_some();

    if static_enabled && shared_enabled {
        return Err("Features `static` and `shared` cannot be enabled at the same time".into());
    }

    if shared_enabled {
        Ok(LinkMode::Shared)
    } else {
        Ok(LinkMode::Static)
    }
}

fn resolve_lib_dir(
    link_mode: LinkMode,
    target_os: &str,
    target_arch: &str,
) -> Result<PathBuf, DynError> {
    if let Some(path) = env::var_os("SHERPA_ONNX_LIB_DIR") {
        let path = PathBuf::from(path);
        if !path.is_dir() {
            return Err(format!(
                "SHERPA_ONNX_LIB_DIR does not exist or is not a directory: {}",
                path.display()
            )
            .into());
        }
        return Ok(path);
    }

    download_prebuilt_libs(link_mode, target_os, target_arch)
}

fn download_prebuilt_libs(
    link_mode: LinkMode,
    target_os: &str,
    target_arch: &str,
) -> Result<PathBuf, DynError> {
    let archive_name = archive_name(link_mode, target_os, target_arch)?;
    let integrity = static_archive_integrity(link_mode, &archive_name)?;
    let archive_stem = archive_name.trim_end_matches(".tar.bz2");

    let out_dir = PathBuf::from(env::var("OUT_DIR")?);
    let cache_root = target_dir_from_out_dir(&out_dir)?.join("sherpa-onnx-prebuilt");
    let extracted_dir = cache_root.join(archive_stem);
    let lib_dir = extracted_dir.join("lib");

    if lib_dir.is_dir() {
        return Ok(lib_dir);
    }

    // Android archives use jniLibs/{abi}/ instead of lib/. Check both.
    let android_lib_dir = extracted_dir.join("jniLibs").join(android_abi(target_arch));
    if android_lib_dir.is_dir() {
        return Ok(android_lib_dir);
    }

    fs::create_dir_all(&cache_root)?;

    let archive_path = cache_root.join(&archive_name);
    if !archive_path.is_file() {
        if let Some(local_archive_dir) = env::var_os("SHERPA_ONNX_ARCHIVE_DIR") {
            let local_archive_path = PathBuf::from(local_archive_dir).join(&archive_name);
            if !local_archive_path.is_file() {
                return Err(format!(
                    "SHERPA_ONNX_ARCHIVE_DIR does not contain expected archive: {}",
                    local_archive_path.display()
                )
                .into());
            }

            verify_archive(&local_archive_path, integrity)?;
            copy_file_atomically(&local_archive_path, &archive_path)?;
        } else {
            let version = env!("CARGO_PKG_VERSION");
            let url = format!("{RELEASE_BASE_URL}/v{version}/{archive_name}");
            eprintln!("Downloading sherpa-onnx libs from {url}");

            let response = ureq::builder()
                .try_proxy_from_env(true)
                .build()
                .get(&url)
                .call()
                .map_err(|e| format!("Failed to download sherpa-onnx archive from {url}: {e}"))?;
            let mut reader = response.into_reader();
            write_reader_atomically(&mut reader, &archive_path)?;
        }
    }

    if let Err(err) = verify_archive(&archive_path, integrity) {
        // A cache entry is never trusted across invocations. Remove only the
        // invalid local cache; an offline source stays untouched for diagnosis.
        let _ = fs::remove_file(&archive_path);
        return Err(err);
    }

    if extracted_dir.exists() {
        fs::remove_dir_all(&extracted_dir)?;
    }

    let unpack_result = unpack_archive_safely(&archive_path, &cache_root, archive_stem);
    if let Err(err) = unpack_result {
        let _ = fs::remove_file(&archive_path);
        let _ = fs::remove_dir_all(&extracted_dir);
        return Err(format!(
            "Failed to unpack cached archive {}: {err}",
            archive_path.display()
        )
        .into());
    }

    if !lib_dir.is_dir() {
        // Android archives use jniLibs/{abi}/ instead of lib/.
        let android_lib_dir = extracted_dir
            .join("jniLibs")
            .join(android_abi(target_arch));
        if android_lib_dir.is_dir() {
            eprintln!("Downloaded sherpa-onnx Android libs to {}", android_lib_dir.display());
            return Ok(android_lib_dir);
        }
        return Err(format!(
            "Downloaded archive did not contain a lib directory: {}",
            lib_dir.display()
        )
        .into());
    }

    eprintln!("Downloaded sherpa-onnx libs to {}", extracted_dir.display());

    Ok(lib_dir)
}

fn static_archive_integrity(
    link_mode: LinkMode,
    archive_name: &str,
) -> Result<&'static ArchiveIntegrity, DynError> {
    if link_mode != LinkMode::Static {
        return Err("Scribe requires sherpa-onnx static linking; shared archives are not admitted".into());
    }
    STATIC_ARCHIVES
        .iter()
        .find(|candidate| candidate.name == archive_name)
        .ok_or_else(|| format!("No reviewed SHA-256 metadata for sherpa-onnx archive {archive_name}").into())
}

fn verify_archive(path: &Path, integrity: &ArchiveIntegrity) -> Result<(), DynError> {
    let metadata = fs::metadata(path)?;
    if metadata.len() != integrity.size {
        return Err(format!(
            "sherpa-onnx archive size mismatch for {}: expected {}, got {}",
            path.display(), integrity.size, metadata.len()
        ).into());
    }

    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = io::Read::read(&mut file, &mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let actual = format!("{:x}", hasher.finalize());
    if actual != integrity.sha256 {
        return Err(format!(
            "sherpa-onnx archive SHA-256 mismatch for {}: expected {}, got {}",
            path.display(), integrity.sha256, actual
        ).into());
    }
    Ok(())
}

fn unpack_archive_safely(
    archive_path: &Path,
    destination: &Path,
    expected_root: &str,
) -> Result<(), DynError> {
    let tar_file = File::open(archive_path)?;
    let decoder = BzDecoder::new(tar_file);
    let mut archive = Archive::new(decoder);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let entry_path = entry.path()?.into_owned();
        let mut components = entry_path.components();
        let Some(first) = components.next() else {
            return Err("sherpa-onnx archive contains an empty path".into());
        };
        if first.as_os_str() != expected_root {
            return Err(format!(
                "sherpa-onnx archive entry is outside expected root {expected_root}: {}",
                entry_path.display()
            ).into());
        }
        if components.clone().next().is_none() && !entry.header().entry_type().is_dir() {
            return Err(format!("invalid sherpa-onnx archive root entry: {}", entry_path.display()).into());
        }
        if entry_path.is_absolute()
            || entry_path.components().any(|component| !matches!(component, std::path::Component::Normal(_)))
            || entry.header().entry_type().is_symlink()
            || entry.header().entry_type().is_hard_link()
        {
            return Err(format!("unsafe sherpa-onnx archive entry: {}", entry_path.display()).into());
        }
        if !entry.unpack_in(destination)? {
            return Err(format!("sherpa-onnx archive entry escaped destination: {}", entry_path.display()).into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod verifier_tests {
    use super::*;

    #[test]
    fn every_supported_static_target_has_reviewed_metadata() {
        for (os, arch) in [
            ("windows", "x86_64"),
            ("linux", "x86_64"),
            ("linux", "aarch64"),
            ("macos", "x86_64"),
            ("macos", "aarch64"),
        ] {
            let name = archive_name(LinkMode::Static, os, arch).unwrap();
            let integrity = static_archive_integrity(LinkMode::Static, &name).unwrap();
            assert_eq!(integrity.name, name);
            assert_eq!(integrity.sha256.len(), 64);
            assert!(integrity.size > 1_000_000);
        }
    }

    #[test]
    fn verifier_rejects_wrong_size_and_hash() {
        let path = std::env::temp_dir().join(format!("scribe-sherpa-archive-{}", std::process::id()));
        fs::write(&path, b"abc").unwrap();
        let wrong_size = ArchiveIntegrity { name: "test", size: 4, sha256: "00" };
        assert!(verify_archive(&path, &wrong_size).unwrap_err().to_string().contains("size mismatch"));
        let wrong_hash = ArchiveIntegrity { name: "test", size: 3, sha256: "00" };
        assert!(verify_archive(&path, &wrong_hash).unwrap_err().to_string().contains("SHA-256 mismatch"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn source_rejects_traversal_and_link_entries_before_unpacking() {
        let source = include_str!("build.rs");
        assert!(source.contains("Component::Normal"));
        assert!(source.contains("is_symlink"));
        assert!(source.contains("is_hard_link"));
        assert!(source.contains("unpack_in(destination)"));
    }
}

/// Map a Rust target architecture to the Android ABI directory name used
/// in the prebuilt jniLibs/ layout.
fn android_abi(target_arch: &str) -> &str {
    match target_arch {
        "aarch64" => "arm64-v8a",
        "arm" => "armeabi-v7a",
        "x86" => "x86",
        "x86_64" => "x86_64",
        _ => "arm64-v8a",
    }
}

fn archive_name(
    link_mode: LinkMode,
    target_os: &str,
    target_arch: &str,
) -> Result<String, DynError> {
    let version = env!("CARGO_PKG_VERSION");
    let name = match (link_mode, target_os, target_arch) {
        (LinkMode::Static, "linux", "x86_64") => {
            format!("sherpa-onnx-v{version}-linux-x64-static-lib.tar.bz2")
        }
        (LinkMode::Static, "linux", "aarch64") => {
            format!("sherpa-onnx-v{version}-linux-aarch64-static-lib.tar.bz2")
        }
        (LinkMode::Static, "macos", "x86_64") => {
            format!("sherpa-onnx-v{version}-osx-x64-static-lib.tar.bz2")
        }
        (LinkMode::Static, "macos", "aarch64") => {
            format!("sherpa-onnx-v{version}-osx-arm64-static-lib.tar.bz2")
        }
        (LinkMode::Static, "windows", "x86_64") => {
            format!("sherpa-onnx-v{version}-win-x64-static-MT-Release-lib.tar.bz2")
        }
        (LinkMode::Shared, "linux", "x86_64") => {
            format!("sherpa-onnx-v{version}-linux-x64-shared-lib.tar.bz2")
        }
        (LinkMode::Shared, "linux", "aarch64") => {
            format!("sherpa-onnx-v{version}-linux-aarch64-shared-cpu-lib.tar.bz2")
        }
        (LinkMode::Shared, "macos", "x86_64") => {
            format!("sherpa-onnx-v{version}-osx-x64-shared-lib.tar.bz2")
        }
        (LinkMode::Shared, "macos", "aarch64") => {
            format!("sherpa-onnx-v{version}-osx-arm64-shared-lib.tar.bz2")
        }
        (LinkMode::Shared, "windows", "x86_64") => {
            format!("sherpa-onnx-v{version}-win-x64-shared-MT-Release-lib.tar.bz2")
        }
        // Android: one archive with all ABIs under jniLibs/{abi}/.
        (LinkMode::Shared, "android", "aarch64" | "arm" | "x86" | "x86_64") => {
            format!("sherpa-onnx-v{version}-android.tar.bz2")
        }
        _ => return Err(format!(
            "Unsupported target for sherpa-onnx prebuilt libs: os={target_os}, arch={target_arch}"
        )
        .into()),
    };

    Ok(name)
}

fn emit_shared_link_directives() {
    println!("cargo:rustc-link-lib=dylib=sherpa-onnx-c-api");
    println!("cargo:rustc-link-lib=dylib=onnxruntime");
}

fn emit_static_link_directives(target_os: &str) {
    for lib in SHERPA_ONNX_STATIC_LIBS {
        println!("cargo:rustc-link-lib=static={lib}");
    }

    match target_os {
        "linux" => {
            println!("cargo:rustc-link-lib=dylib=stdc++");
            println!("cargo:rustc-link-lib=dylib=m");
            println!("cargo:rustc-link-lib=dylib=pthread");
            println!("cargo:rustc-link-lib=dylib=dl");
        }
        "macos" => {
            println!("cargo:rustc-link-lib=dylib=c++");
            println!("cargo:rustc-link-lib=framework=Foundation");
        }
        _ => {}
    }
}

fn target_dir_from_out_dir(out_dir: &Path) -> Result<PathBuf, DynError> {
    if let Ok(explicit_target_dir) = env::var("CARGO_TARGET_DIR") {
        return Ok(PathBuf::from(explicit_target_dir));
    }

    if let Some(target_dir) = out_dir
        .ancestors()
        .find(|path| path.file_name() == Some(OsStr::new("target")))
    {
        return Ok(target_dir.to_path_buf());
    }

    Ok(out_dir.to_path_buf())
}

fn emit_relative_rpath(target_os: &str) {
    match target_os {
        "linux" | "android" => println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN"),
        "macos" => println!("cargo:rustc-link-arg=-Wl,-rpath,@loader_path"),
        _ => {}
    }
}

fn profile_output_dirs() -> Result<[PathBuf; 2], DynError> {
    let out_dir = PathBuf::from(env::var("OUT_DIR")?);
    let profile = env::var("PROFILE")?;
    let profile_dir = out_dir
        .ancestors()
        .find(|path| path.file_name() == Some(OsStr::new(&profile)))
        .ok_or_else(|| {
            format!(
                "Could not locate Cargo profile directory from {}",
                out_dir.display()
            )
        })?
        .to_path_buf();

    Ok([profile_dir.clone(), profile_dir.join("examples")])
}

fn copy_unix_runtime_libs(lib_dir: &Path, target_os: &str) -> Result<(), DynError> {
    let runtime_libs: Vec<PathBuf> = fs::read_dir(lib_dir)?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| {
            path.file_name()
                .and_then(OsStr::to_str)
                 .map(|name| match target_os {
                     "linux" | "android" => name.contains(".so"),
                     "macos" => name.ends_with(".dylib"),
                    _ => false,
                })
                .unwrap_or(false)
        })
        .collect();

    if runtime_libs.is_empty() {
        return Err(format!(
            "No shared runtime libraries found in {}",
            lib_dir.display()
        )
        .into());
    }

    let mut copy_plan = Vec::<(PathBuf, OsString)>::new();
    let mut planned_names = HashSet::<OsString>::new();

    for lib in runtime_libs {
        if !lib.exists() {
            continue;
        }

        let lib_name = lib
            .file_name()
            .ok_or_else(|| format!("Invalid runtime library path: {}", lib.display()))?
            .to_os_string();

        let source = fs::canonicalize(&lib).unwrap_or(lib.clone());
        if planned_names.insert(lib_name.clone()) {
            copy_plan.push((source.clone(), lib_name));
        }

        if let Some(source_name) = source.file_name() {
            let source_name = source_name.to_os_string();
            if planned_names.insert(source_name.clone()) {
                copy_plan.push((source.clone(), source_name));
            }
        }
    }

    if copy_plan.is_empty() {
        return Err(format!(
            "No usable shared runtime libraries found in {}",
            lib_dir.display()
        )
        .into());
    }

    for dest_dir in profile_output_dirs()? {
        fs::create_dir_all(&dest_dir)?;
        for (source, dest_name) in &copy_plan {
            let dest = dest_dir.join(dest_name);
            fs::copy(source, &dest)?;
        }
    }

    Ok(())
}

fn temp_path_for(path: &Path) -> PathBuf {
    let mut temp_name = path
        .file_name()
        .map(OsStr::to_os_string)
        .unwrap_or_else(|| OsString::from("tmp"));
    temp_name.push(".part");
    path.with_file_name(temp_name)
}

fn copy_file_atomically(src: &Path, dst: &Path) -> Result<(), DynError> {
    let temp_path = temp_path_for(dst);
    if temp_path.exists() {
        let _ = fs::remove_file(&temp_path);
    }
    fs::copy(src, &temp_path)?;
    fs::rename(&temp_path, dst)?;
    Ok(())
}

fn write_reader_atomically(reader: &mut dyn io::Read, dst: &Path) -> Result<(), DynError> {
    let temp_path = temp_path_for(dst);
    if temp_path.exists() {
        let _ = fs::remove_file(&temp_path);
    }

    {
        let mut file = File::create(&temp_path)?;
        io::copy(reader, &mut file)?;
        file.sync_all()?;
    }

    fs::rename(&temp_path, dst)?;
    Ok(())
}

fn copy_windows_runtime_dlls(lib_dir: &Path) -> Result<(), DynError> {
    let dlls: Vec<PathBuf> = fs::read_dir(lib_dir)?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension() == Some(OsStr::new("dll")))
        .collect();

    if dlls.is_empty() {
        println!(
            "cargo:warning=No runtime DLLs found in {}",
            lib_dir.display()
        );
        return Ok(());
    }

    let [profile_dir, examples_dir] = profile_output_dirs()?;
    for dest_dir in [profile_dir.clone(), examples_dir] {
        fs::create_dir_all(&dest_dir)?;
        for dll in &dlls {
            let dest = dest_dir.join(
                dll.file_name()
                    .ok_or_else(|| format!("Invalid DLL path: {}", dll.display()))?,
            );
            fs::copy(dll, &dest)?;
        }
    }

    println!(
        "cargo:warning=Copied Windows runtime DLLs to {} and {}/examples",
        profile_dir.display(),
        profile_dir.display()
    );

    Ok(())
}
