fn main() {
    require_windows_static_crt();

    println!("cargo:rerun-if-changed=native/whisper_shim.c");
    println!("cargo:rerun-if-changed=native/whisper-f049fff/whisper.h");
    println!("cargo:rerun-if-changed=native/whisper-f049fff/ggml.h");
    println!("cargo:rerun-if-changed=native/whisper-f049fff/ggml-cpu.h");
    println!("cargo:rerun-if-changed=native/whisper-f049fff/ggml-backend.h");
    println!("cargo:rerun-if-changed=native/whisper-f049fff/ggml-alloc.h");

    cc::Build::new()
        .file("native/whisper_shim.c")
        .include("native")
        .warnings(true)
        .compile("scribe_whisper_shim");

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if matches!(target_os.as_str(), "linux" | "android") {
        println!("cargo:rustc-link-lib=dl");
    }
}

fn require_windows_static_crt() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os != "windows" || target_env != "msvc" {
        return;
    }

    let target_features = std::env::var("CARGO_CFG_TARGET_FEATURE").unwrap_or_default();
    if !target_features
        .split(',')
        .any(|feature| feature == "crt-static")
    {
        panic!(
            "Windows native dependencies require the static MSVC CRT; preserve `-C target-feature=+crt-static` from .cargo/config.toml when overriding target rustflags"
        );
    }
}
