use std::env;
use std::ffi::OsStr;
use std::fs;
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::{collections::HashSet, ffi::OsString};

#[path = "src/archive_verifier.rs"]
mod archive_verifier;
use archive_verifier::{
    activate_verified_archive, debug_escape_allowed, reviewed_static_archive_integrity,
    static_archive_name, validate_static_library_layout, verify_archive, DynError,
};

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
    println!("cargo:rerun-if-env-changed=SHERPA_ONNX_ALLOW_DEBUG_DOWNLOAD");
    println!("cargo:rerun-if-env-changed=DOCS_RS");

    if env::var_os("DOCS_RS").is_some() {
        // docs.rs sets DOCS_RS=1; skip downloading/linking native libraries
        // so that `cargo doc` can succeed without the real C artifacts.
        return Ok(());
    }

    let target_os = env::var("CARGO_CFG_TARGET_OS")?;
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH")?;
    let link_mode = resolve_link_mode()?;
    if link_mode != LinkMode::Static {
        return Err("Scribe's sherpa-onnx patch supports statically linked native archives only; the `shared` feature is rejected".into());
    }
    let lib_dir = resolve_lib_dir(link_mode, &target_os, &target_arch)?;

    println!("cargo:rustc-link-search=native={}", lib_dir.display());

    if link_mode == LinkMode::Shared && matches!(target_os.as_str(), "linux" | "macos" | "android")
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
        Err("Scribe's sherpa-onnx patch rejects the `shared` feature; enable `static` only".into())
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
        if !debug_profile_escape_allowed(true) {
            return Err("SHERPA_ONNX_LIB_DIR is an unverified developer override and is rejected in release builds".into());
        }
        if !path.is_dir() {
            return Err(format!(
                "SHERPA_ONNX_LIB_DIR does not exist or is not a directory: {}",
                path.display()
            )
            .into());
        }
        validate_static_library_layout(&path, target_os, SHERPA_ONNX_STATIC_LIBS)?;
        println!(
            "cargo:warning=Using unverified debug-only SHERPA_ONNX_LIB_DIR override: {}",
            path.display()
        );
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
    let integrity = reviewed_static_archive_integrity(&archive_name)?;
    let archive_stem = archive_name.trim_end_matches(".tar.bz2");

    let out_dir = PathBuf::from(env::var("OUT_DIR")?);
    let cache_root = target_dir_from_out_dir(&out_dir)?.join("sherpa-onnx-prebuilt");
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
        } else if allow_debug_network_download() {
            let version = env!("CARGO_PKG_VERSION");
            let url = format!("{RELEASE_BASE_URL}/v{version}/{archive_name}");
            eprintln!("Downloading sherpa-onnx libs from {url}");

            let response = get_with_allowlisted_redirects(&url)?;
            if let Some(length) = response.header("Content-Length") {
                let length = length.parse::<u64>().map_err(|_| {
                    format!("Invalid Content-Length for sherpa-onnx archive from {url}")
                })?;
                if length > integrity.size {
                    return Err(format!(
                        "sherpa-onnx download is larger than reviewed size: expected at most {}, got {length}",
                        integrity.size
                    )
                    .into());
                }
            }
            let mut reader = response.into_reader();
            write_reader_atomically_limited(&mut reader, &archive_path, integrity.size)?;
        } else {
            return Err(format!(
                "sherpa-onnx archive {archive_name} is not cached; set SHERPA_ONNX_ARCHIVE_DIR to a reviewed local archive. Release builds never download native archives (debug-only downloads require SHERPA_ONNX_ALLOW_DEBUG_DOWNLOAD=1)."
            )
            .into());
        }
    }

    if let Err(err) = verify_archive(&archive_path, integrity) {
        // A cache entry is never trusted across invocations. Remove only the
        // invalid local cache; an offline source stays untouched for diagnosis.
        let _ = fs::remove_file(&archive_path);
        return Err(err);
    }

    activate_verified_archive(
        &archive_path,
        integrity,
        &cache_root,
        archive_stem,
        target_os,
        SHERPA_ONNX_STATIC_LIBS,
    )
}

fn allow_debug_network_download() -> bool {
    debug_profile_escape_allowed(
        env::var("SHERPA_ONNX_ALLOW_DEBUG_DOWNLOAD")
            .ok()
            .as_deref()
            == Some("1"),
    )
}

fn debug_profile_escape_allowed(explicitly_enabled: bool) -> bool {
    debug_escape_allowed(
        &env::var("PROFILE").unwrap_or_default(),
        cfg!(debug_assertions),
        explicitly_enabled,
    )
}

fn get_with_allowlisted_redirects(url: &str) -> Result<ureq::Response, DynError> {
    let agent = ureq::builder()
        .try_proxy_from_env(true)
        .redirects(0)
        .build();
    let mut current = url.to_owned();
    for _ in 0..=3 {
        let response = agent
            .get(&current)
            .call()
            .map_err(|error| format!("Failed to download sherpa-onnx archive from {current}: {error}"))?;
        if !(300..400).contains(&response.status()) {
            return Ok(response);
        }
        let location = response
            .header("Location")
            .ok_or("sherpa-onnx download redirect omitted Location")?;
        if !is_allowlisted_release_url(location) {
            return Err(format!("Rejected sherpa-onnx download redirect to {location}").into());
        }
        current = location.to_owned();
    }
    Err("sherpa-onnx download exceeded three allowlisted redirects".into())
}

fn is_allowlisted_release_url(url: &str) -> bool {
    [
        "https://github.com/",
        "https://objects.githubusercontent.com/",
        "https://release-assets.githubusercontent.com/",
    ]
    .iter()
    .any(|prefix| url.starts_with(prefix))
}

fn archive_name(
    link_mode: LinkMode,
    target_os: &str,
    target_arch: &str,
) -> Result<String, DynError> {
    let version = env!("CARGO_PKG_VERSION");
    let name = match (link_mode, target_os, target_arch) {
        (LinkMode::Static, os, arch) => static_archive_name(version, os, arch)?,
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
        _ => {
            return Err(format!(
            "Unsupported target for sherpa-onnx prebuilt libs: os={target_os}, arch={target_arch}"
        )
            .into())
        }
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
        return Err(format!("No shared runtime libraries found in {}", lib_dir.display()).into());
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
    temp_name.push(format!(".part-{}", std::process::id()));
    path.with_file_name(temp_name)
}

fn copy_file_atomically(src: &Path, dst: &Path) -> Result<(), DynError> {
    let temp_path = temp_path_for(dst);
    if temp_path.exists() {
        let _ = fs::remove_file(&temp_path);
    }
    fs::copy(src, &temp_path)?;
    if let Err(error) = fs::rename(&temp_path, dst) {
        let _ = fs::remove_file(&temp_path);
        if dst.is_file() {
            return Ok(());
        }
        return Err(error.into());
    }
    Ok(())
}

fn write_reader_atomically_limited(
    reader: &mut dyn io::Read,
    dst: &Path,
    expected_size: u64,
) -> Result<(), DynError> {
    let temp_path = temp_path_for(dst);
    if temp_path.exists() {
        let _ = fs::remove_file(&temp_path);
    }

    {
        let mut file = File::create(&temp_path)?;
        let mut copied = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            copied = copied.saturating_add(read as u64);
            if copied > expected_size {
                drop(file);
                let _ = fs::remove_file(&temp_path);
                return Err(
                    format!("sherpa-onnx download exceeded reviewed size {expected_size}").into(),
                );
            }
            file.write_all(&buffer[..read])?;
        }
        file.sync_all()?;
    }

    if let Err(error) = fs::rename(&temp_path, dst) {
        let _ = fs::remove_file(&temp_path);
        if dst.is_file() {
            return Ok(());
        }
        return Err(error.into());
    }
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
