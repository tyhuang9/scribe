use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

const WORKER_BINARY: &str = "scribe-inference-worker";
const CUDA_LIBRARIES: [&str; 4] = [
    "cudart_static.lib",
    "cublas.lib",
    "cublasLt.lib",
    "cuda.lib",
];

pub(crate) struct WindowsCudaLinkInputs<'a> {
    pub(crate) target_arch: &'a str,
    pub(crate) target_env: &'a str,
    pub(crate) building_worker: Option<&'a str>,
    pub(crate) cuda_path: Option<&'a OsStr>,
}

pub(crate) fn is_windows_cuda_build(cuda_enabled: bool, target_os: &str) -> bool {
    cuda_enabled && target_os == "windows"
}

pub(crate) fn resolve_windows_cuda_link_args(
    inputs: WindowsCudaLinkInputs<'_>,
) -> Result<Vec<String>, String> {
    if inputs.target_arch != "x86_64" || inputs.target_env != "msvc" {
        return Err(
            "Windows cuda-acceleration requires the x86_64-pc-windows-msvc target".to_owned(),
        );
    }
    if inputs.building_worker != Some("1") {
        return Err(
            "Windows cuda-acceleration may be linked only when SCRIBE_BUILDING_WORKER is exactly 1"
                .to_owned(),
        );
    }

    let cuda_path = inputs
        .cuda_path
        .ok_or_else(|| "Windows cuda-acceleration requires an explicit CUDA_PATH".to_owned())?;
    let cuda_text = cuda_path.to_str().ok_or_else(|| {
        "Windows cuda-acceleration requires CUDA_PATH to be valid Unicode".to_owned()
    })?;
    if cuda_text.is_empty() {
        return Err("Windows cuda-acceleration requires a nonempty CUDA_PATH".to_owned());
    }
    if cuda_text.chars().any(char::is_control) {
        return Err("Windows cuda-acceleration rejects control characters in CUDA_PATH".to_owned());
    }

    let cuda_root = PathBuf::from(cuda_path);
    if !cuda_root.is_absolute() {
        return Err("Windows cuda-acceleration requires CUDA_PATH to be absolute".to_owned());
    }
    validate_physical_directory_with_ancestors(&cuda_root, "CUDA_PATH")?;

    let library_directory = cuda_root.join("lib").join("x64");
    validate_physical_directory_with_ancestors(&library_directory, "CUDA_PATH\\lib\\x64")?;

    let mut libraries = Vec::with_capacity(CUDA_LIBRARIES.len());
    for name in CUDA_LIBRARIES {
        let library = library_directory.join(name);
        validate_regular_non_reparse_file(&library, name)?;
        libraries.push(library);
    }

    Ok(libraries
        .into_iter()
        .map(|library| {
            format!(
                "cargo:rustc-link-arg-bin={WORKER_BINARY}={}",
                library.display()
            )
        })
        .collect())
}

fn validate_physical_directory_with_ancestors(path: &Path, label: &str) -> Result<(), String> {
    for ancestor in path
        .ancestors()
        .filter(|ancestor| !ancestor.as_os_str().is_empty())
    {
        let metadata = fs::symlink_metadata(ancestor).map_err(|error| {
            format!(
                "Windows cuda-acceleration requires {label} and every ancestor to exist as physical directories ({}: {error})",
                ancestor.display()
            )
        })?;
        if !metadata.is_dir() || is_link_or_reparse(&metadata) {
            return Err(format!(
                "Windows cuda-acceleration requires {label} and every ancestor to be physical non-reparse directories ({})",
                ancestor.display()
            ));
        }
    }
    Ok(())
}

fn validate_regular_non_reparse_file(path: &Path, label: &str) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        format!(
            "Windows cuda-acceleration requires {label} at {}: {error}",
            path.display()
        )
    })?;
    if !metadata.is_file() || is_link_or_reparse(&metadata) {
        return Err(format!(
            "Windows cuda-acceleration requires {label} to be a regular non-reparse file at {}",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::{OsStr, OsString};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        fn new(label: &str) -> Self {
            Self::new_under(label, &std::env::temp_dir())
        }

        fn new_under(label: &str, temp_root: &Path) -> Self {
            // macOS exposes its system temporary directory through /var. Resolve
            // only this test-owned anchor; SDK paths still reject every link.
            #[cfg(unix)]
            let temp_root = fs::canonicalize(temp_root).expect("physical fixture temp root");
            let id = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
            let root = temp_root.join(format!(
                "scribe-windows-cuda-link-{label}-{}-{id}",
                std::process::id()
            ));
            fs::create_dir(&root).expect("CUDA link fixture root unexpectedly existed");
            fs::create_dir(root.join("lib")).unwrap();
            fs::create_dir(root.join("lib").join("x64")).unwrap();
            for library in CUDA_LIBRARIES {
                fs::write(root.join("lib").join("x64").join(library), library).unwrap();
            }
            Self { root }
        }

        fn resolve(&self) -> Result<Vec<String>, String> {
            resolve_with_path(Some(self.root.as_os_str()))
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn resolve_with_path(cuda_path: Option<&OsStr>) -> Result<Vec<String>, String> {
        resolve_windows_cuda_link_args(WindowsCudaLinkInputs {
            target_arch: "x86_64",
            target_env: "msvc",
            building_worker: Some("1"),
            cuda_path,
        })
    }

    #[test]
    fn emits_exact_binary_specific_args_in_required_order_for_space_path() {
        let fixture = Fixture::new("space path");
        let library_directory = fixture.root.join("lib").join("x64");
        let expected = CUDA_LIBRARIES
            .map(|library| {
                format!(
                    "cargo:rustc-link-arg-bin={WORKER_BINARY}={}",
                    library_directory.join(library).display()
                )
            })
            .to_vec();

        assert_eq!(fixture.resolve().unwrap(), expected);
        assert!(expected.iter().all(|line| !line.contains("rustc-link-lib")));
        assert!(
            expected
                .iter()
                .all(|line| !line.contains("rustc-link-search"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn fixture_temp_alias_resolves_without_weakening_sdk_link_rejection() {
        let owner = Fixture::new("temp-alias-owner");
        let physical = owner.root.join("physical-temp");
        fs::create_dir(&physical).unwrap();
        let alias = owner.root.join("temp-alias");
        create_directory_link(&physical, &alias);

        let fixture = Fixture::new_under("aliased-temp", &alias);
        assert_eq!(fixture.root.parent(), Some(physical.as_path()));
        assert_eq!(fixture.resolve().unwrap().len(), CUDA_LIBRARIES.len());

        let linked_sdk = alias.join(fixture.root.file_name().unwrap());
        assert!(
            resolve_with_path(Some(linked_sdk.as_os_str()))
                .unwrap_err()
                .contains("physical non-reparse directories")
        );
        drop(fixture);
        remove_directory_link(&alias);
    }

    #[test]
    fn cpu_build_is_isolated_from_windows_cuda_linking() {
        assert!(!is_windows_cuda_build(false, "windows"));
    }

    #[test]
    fn vulkan_build_is_isolated_from_windows_cuda_linking() {
        assert!(!is_windows_cuda_build(false, "windows"));
    }

    #[test]
    fn non_windows_cuda_build_is_isolated_from_windows_cuda_linking() {
        for target_os in ["linux", "macos"] {
            assert!(!is_windows_cuda_build(true, target_os));
        }
        assert!(is_windows_cuda_build(true, "windows"));
    }

    #[test]
    fn rejects_non_x86_64_msvc_targets_and_invalid_worker_markers() {
        let fixture = Fixture::new("gate");
        for (arch, env) in [("aarch64", "msvc"), ("x86_64", "gnu")] {
            let result = resolve_windows_cuda_link_args(WindowsCudaLinkInputs {
                target_arch: arch,
                target_env: env,
                building_worker: Some("1"),
                cuda_path: Some(fixture.root.as_os_str()),
            });
            assert!(result.unwrap_err().contains("x86_64-pc-windows-msvc"));
        }
        for marker in [None, Some(""), Some("0"), Some("true"), Some("01")] {
            let result = resolve_windows_cuda_link_args(WindowsCudaLinkInputs {
                target_arch: "x86_64",
                target_env: "msvc",
                building_worker: marker,
                cuda_path: Some(fixture.root.as_os_str()),
            });
            assert!(result.unwrap_err().contains("exactly 1"));
        }
    }

    #[test]
    fn rejects_missing_empty_relative_nonexistent_and_file_valued_cuda_path() {
        assert!(
            resolve_with_path(None)
                .unwrap_err()
                .contains("explicit CUDA_PATH")
        );
        assert!(
            resolve_with_path(Some(OsStr::new("")))
                .unwrap_err()
                .contains("nonempty CUDA_PATH")
        );
        assert!(
            resolve_with_path(Some(OsStr::new("relative-cuda")))
                .unwrap_err()
                .contains("absolute")
        );

        let fixture = Fixture::new("file-root");
        let nonexistent = fixture.root.join("missing-sdk");
        assert!(
            resolve_with_path(Some(nonexistent.as_os_str()))
                .unwrap_err()
                .contains("exist as physical directories")
        );
        let file = fixture.root.join("not-a-directory");
        fs::write(&file, b"file").unwrap();
        assert!(
            resolve_with_path(Some(file.as_os_str()))
                .unwrap_err()
                .contains("physical non-reparse directories")
        );
    }

    #[test]
    fn rejects_each_missing_library_without_returning_partial_args() {
        for missing in CUDA_LIBRARIES {
            let fixture = Fixture::new(missing);
            fs::remove_file(fixture.root.join("lib").join("x64").join(missing)).unwrap();
            let result = fixture.resolve();
            assert!(result.is_err(), "missing {missing} was accepted");
        }
    }

    #[test]
    fn rejects_directory_in_place_of_library() {
        let fixture = Fixture::new("directory-library");
        let library = fixture.root.join("lib").join("x64").join("cublas.lib");
        fs::remove_file(&library).unwrap();
        fs::create_dir(&library).unwrap();
        assert!(
            fixture
                .resolve()
                .unwrap_err()
                .contains("regular non-reparse file")
        );
    }

    #[test]
    fn validates_all_libraries_before_returning_any_directives() {
        let fixture = Fixture::new("all-or-nothing");
        fs::remove_file(fixture.root.join("lib").join("x64").join("cublasLt.lib")).unwrap();
        assert!(fixture.resolve().is_err());
    }

    #[test]
    fn rejects_control_characters_that_could_inject_cargo_directives() {
        for control in ['\n', '\r', '\t', '\u{1b}', '\u{7f}'] {
            let path = OsString::from(format!(
                "{}{}injected",
                std::env::temp_dir().display(),
                control
            ));
            let error = resolve_with_path(Some(path.as_os_str())).unwrap_err();
            assert!(error.contains("control characters"));
        }
    }

    #[test]
    fn manifest_authenticates_every_statically_linked_cuda_library() {
        let manifest_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("runtime-manifests")
            .join("gpu-worker-toolchain-windows-x64.json");
        let manifest = fs::read_to_string(manifest_path).unwrap();
        let cuda = manifest.split_once("\"cuda\": {").unwrap().1;
        let required = cuda
            .split_once("\"required_files\": [")
            .unwrap()
            .1
            .split_once(']')
            .unwrap()
            .0
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(|line| line.trim_end_matches(',').trim_matches('"'))
            .collect::<Vec<_>>();
        assert_eq!(
            required,
            [
                "bin/nvcc.exe",
                "include/cuda.h",
                "lib/x64/cudart_static.lib",
                "lib/x64/cublas.lib",
                "lib/x64/cublasLt.lib",
                "lib/x64/cuda.lib",
            ]
        );
    }

    #[test]
    fn rejects_reparse_cuda_root() {
        let fixture = Fixture::new("root-target");
        let link = fixture.root.with_extension("root-link");
        create_directory_link(&fixture.root, &link);
        let result = resolve_with_path(Some(link.as_os_str()));
        remove_directory_link(&link);
        assert!(
            result
                .unwrap_err()
                .contains("physical non-reparse directories")
        );
    }

    #[test]
    fn rejects_reparse_cuda_ancestor() {
        let fixture = Fixture::new("ancestor-target");
        let parent = fixture.root.parent().unwrap();
        let link = parent.join(format!(
            "scribe-windows-cuda-link-ancestor-link-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        create_directory_link(parent, &link);
        let through_link = link.join(fixture.root.file_name().unwrap());
        let result = resolve_with_path(Some(through_link.as_os_str()));
        remove_directory_link(&link);
        assert!(
            result
                .unwrap_err()
                .contains("physical non-reparse directories")
        );
    }

    #[test]
    fn rejects_reparse_cuda_library_directory() {
        let fixture = Fixture::new("lib-directory-target");
        let lib = fixture.root.join("lib");
        let x64 = lib.join("x64");
        let physical = lib.join("physical-x64");
        fs::rename(&x64, &physical).unwrap();
        create_directory_link(&physical, &x64);
        let result = fixture.resolve();
        remove_directory_link(&x64);
        assert!(
            result
                .unwrap_err()
                .contains("physical non-reparse directories")
        );
    }

    #[test]
    fn rejects_reparse_cuda_library_entry() {
        let fixture = Fixture::new("library-entry-target");
        let library = fixture.root.join("lib").join("x64").join("cublas.lib");
        let physical = fixture.root.join("physical-library-entry");
        fs::remove_file(&library).unwrap();
        fs::create_dir(&physical).unwrap();
        create_directory_link(&physical, &library);
        let result = fixture.resolve();
        remove_directory_link(&library);
        assert!(result.unwrap_err().contains("regular non-reparse file"));
    }

    #[test]
    fn rejects_file_symlink_cuda_library_when_platform_permits_fixture_creation() {
        let fixture = Fixture::new("library-file-symlink-target");
        let library = fixture.root.join("lib").join("x64").join("cublas.lib");
        let physical = library.with_extension("physical");
        fs::rename(&library, &physical).unwrap();
        if let Err(error) = create_file_link(&physical, &library) {
            #[cfg(windows)]
            if error.raw_os_error() == Some(1314) {
                eprintln!(
                    "file-symlink CUDA library fixture unavailable: Windows symlink privilege is not enabled"
                );
                return;
            }
            panic!("could not create file-symlink CUDA library fixture: {error}");
        }
        let result = fixture.resolve();
        fs::remove_file(&library).unwrap();
        assert!(result.unwrap_err().contains("regular non-reparse file"));
    }

    #[cfg(windows)]
    fn create_directory_link(target: &Path, link: &Path) {
        let status = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .status()
            .unwrap();
        assert!(
            status.success(),
            "could not create directory junction fixture"
        );
    }

    #[cfg(not(windows))]
    fn create_directory_link(target: &Path, link: &Path) {
        std::os::unix::fs::symlink(target, link).unwrap();
    }

    #[cfg(windows)]
    fn create_file_link(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::windows::fs::symlink_file(target, link)
    }

    #[cfg(not(windows))]
    fn create_file_link(target: &Path, link: &Path) -> std::io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(windows)]
    fn remove_directory_link(link: &Path) {
        fs::remove_dir(link).unwrap();
    }

    #[cfg(not(windows))]
    fn remove_directory_link(link: &Path) {
        fs::remove_file(link).unwrap();
    }
}
