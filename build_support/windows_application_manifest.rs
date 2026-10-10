use std::path::Path;

pub const MANIFEST_PATH: &str = "resources/windows/application.manifest";
pub const MANIFEST_SHA256: &str =
    "26396c66401927b27fad83513f99c3a34ba93b5e27631c85369eb48d72efff48";

pub fn cargo_directives(
    target_os: &str,
    target_env: &str,
    manifest_path: &Path,
) -> Result<Vec<String>, &'static str> {
    let mut directives = vec![
        format!("cargo:rerun-if-changed={MANIFEST_PATH}"),
        "cargo:rerun-if-changed=build_support/windows_application_manifest.rs".to_owned(),
    ];
    if target_os != "windows" || target_env != "msvc" {
        return Ok(directives);
    }
    let path = manifest_path
        .to_str()
        .ok_or("Windows application manifest path must be Unicode")?;
    if !manifest_path.is_absolute()
        || path.starts_with(r"\\?\")
        || path.chars().any(|ch| ch.is_control() || ch == '"')
    {
        return Err(
            "Windows application manifest requires a plain absolute path without controls or quotes",
        );
    }
    // /MANIFESTINPUT retains MAX_PATH even for a long-path-aware executable.
    // Reserve one UTF-16 code unit for the terminating NUL; do not add aliases.
    if path.encode_utf16().count() >= 260 {
        return Err(
            "Windows /MANIFESTINPUT path must be shorter than 260 UTF-16 code units; use a shorter checkout path",
        );
    }
    for binary in ["local-transcriber", "scribe-inference-worker"] {
        directives.push(format!(
            "cargo:rustc-link-arg-bin={binary}=/MANIFEST:EMBED,ID=1"
        ));
        // Cargo passes this as one linker argument, including spaces/Unicode.
        directives.push(format!(
            "cargo:rustc-link-arg-bin={binary}=/MANIFESTINPUT:{path}"
        ));
    }
    Ok(directives)
}
