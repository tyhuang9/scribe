use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    const CATALOG_ENV: &str = "SCRIBE_RUNTIME_ARTIFACT_CATALOG";
    let source = env::var_os(CATALOG_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("runtime-artifacts.default.json"));
    let destination = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set"))
        .join("runtime-artifacts.json");

    println!("cargo:rerun-if-env-changed={CATALOG_ENV}");
    println!("cargo:rerun-if-changed={}", source.display());
    fs::copy(&source, &destination).unwrap_or_else(|err| {
        panic!(
            "failed to embed runtime artifact catalog {}: {err}",
            source.display()
        )
    });
}
