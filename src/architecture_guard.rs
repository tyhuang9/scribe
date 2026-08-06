//! Test-only architectural boundary guard.
//!
//! The executable is a binary crate, so integration tests cannot rely on a
//! public library surface. These tests inspect the checked-out Rust sources
//! directly and fail closed when a concrete runtime leaks above the private
//! router or when UI code starts selecting model families.

use std::fs;
use std::path::{Path, PathBuf};

fn rust_sources() -> Vec<(PathBuf, String)> {
    fn visit(root: &Path, files: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(root).expect("source directory must be readable") {
            let path = entry.expect("source entry must be readable").path();
            if path.is_dir() {
                visit(&path, files);
            } else if path.extension().and_then(|value| value.to_str()) == Some("rs") {
                files.push(path);
            }
        }
    }

    let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut paths = Vec::new();
    visit(&source_root, &mut paths);
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let relative = path
                .strip_prefix(&source_root)
                .expect("source path stays beneath src")
                .to_path_buf();
            let source = fs::read_to_string(&path).expect("Rust source must be UTF-8");
            (relative, source)
        })
        .collect()
}

fn production_prefix(source: &str) -> &str {
    source.split("\n#[cfg(test)]").next().unwrap_or(source)
}

#[test]
fn concrete_runtime_selection_is_private_and_single_handler() {
    let sources = rust_sources();
    let router = sources
        .iter()
        .find(|(path, _)| path == Path::new("runtime_router.rs"))
        .map(|(_, source)| source)
        .expect("runtime router exists");

    assert_eq!(
        router.matches("struct TranscribeCppRuntime").count(),
        1,
        "the application must ship exactly one primary runtime declaration"
    );
    assert!(
        !router.contains("struct OnnxSpeechRuntime"),
        "the evidence-gated ONNX handler must not exist without passing evidence"
    );
    assert_eq!(
        router
            .matches("impl SpeechEngine for TranscribeCppRuntime")
            .count(),
        1,
        "the primary handler must implement the common engine contract once"
    );
    assert!(
        router.contains("enum RuntimeKind") && !router.contains("pub enum RuntimeKind"),
        "RuntimeKind must remain private to the router"
    );

    for (path, source) in &sources {
        if path == Path::new("runtime_router.rs") || path == Path::new("architecture_guard.rs") {
            continue;
        }
        for concrete in ["RuntimeKind", "TranscribeCppRuntime", "OnnxSpeechRuntime"] {
            assert!(
                !source.contains(concrete),
                "{concrete} escaped the private router into {}",
                path.display()
            );
        }
    }
}

#[test]
fn application_and_ui_sources_are_runtime_neutral() {
    let sources = rust_sources();
    let family_terms = [
        "whisper.cpp",
        "faster-whisper",
        "vosk",
        "sherpa",
        "zipformer",
        "moonshine",
        "parakeet",
        "qwen",
        "voxtral",
        "nemotron",
        "sensevoice",
        "canary",
    ];
    let semantic_escapes = [
        "use crate::stt",
        "runtime_catalog::",
        "provider_for_backend",
        ".backend",
        "RuntimeRouter",
        "transcribe_with_config",
        "stt::whisper_cpp",
        "stt::faster_whisper",
        "stt::vosk",
        "stt::sherpa_onnx",
    ];

    for (path, source) in &sources {
        let is_application = path == Path::new("app.rs");
        let is_ui = path.starts_with("ui");
        if !is_application && !is_ui {
            continue;
        }
        let production = production_prefix(source);
        let lowered = production.to_ascii_lowercase();
        for term in family_terms {
            assert!(
                !lowered.contains(term),
                "application/UI source {} contains model-family term {term:?}",
                path.display()
            );
        }
        for escape in semantic_escapes {
            assert!(
                !production.contains(escape),
                "application/UI source {} bypasses TranscriptionService via {escape:?}",
                path.display()
            );
        }
    }
}

#[test]
fn model_family_logic_is_confined_to_private_adapters_and_catalog_validation() {
    let sources = rust_sources();
    let allowed_files = [
        "compatibility_bridge.rs",
        "config.rs",
        "installations.rs",
        "managed_downloads.rs",
        "model_catalog.rs",
        "models.rs",
        "runtime_catalog.rs",
        "runtime_router.rs",
        "settings/schema.rs",
        "transcription.rs",
    ];
    let family_terms = [
        "whisper.cpp",
        "faster-whisper",
        "vosk",
        "sherpa",
        "zipformer",
        "moonshine",
        "parakeet",
        "qwen",
        "voxtral",
        "nemotron",
        "sensevoice",
        "canary",
    ];

    for (path, source) in &sources {
        if path == Path::new("architecture_guard.rs")
            || path.starts_with("stt")
            || allowed_files
                .iter()
                .any(|allowed| path == Path::new(allowed))
        {
            continue;
        }
        let production = production_prefix(source).to_ascii_lowercase();
        for term in family_terms {
            assert!(
                !production.contains(term),
                "model-family logic {term:?} escaped private adapters/catalog validation into {}",
                path.display()
            );
        }
    }
}

#[test]
fn tentative_transcripts_have_no_output_module_path() {
    let output = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("text_output.rs"),
    )
    .expect("text output source must be readable");
    let production = production_prefix(&output).to_ascii_lowercase();

    assert!(
        !production.contains("tentative"),
        "text output must only receive finalized text; tentative text belongs in the overlay"
    );
}

#[test]
fn no_web_runtime_or_ui_pcm_transport_is_present() {
    let sources = rust_sources();
    for (path, source) in &sources {
        if path == Path::new("architecture_guard.rs") {
            continue;
        }
        let production = production_prefix(source).to_ascii_lowercase();
        for forbidden in ["tauri::", "webview", "ipc::", "javascript"] {
            assert!(
                !production.contains(forbidden),
                "{} introduces forbidden web/UI transport {forbidden:?}",
                path.display()
            );
        }
    }

    for (path, source) in &sources {
        if !path.starts_with("ui") {
            continue;
        }
        let production = production_prefix(source);
        for pcm_shape in ["Vec<f32>", "&[f32]", "PreparedAudio"] {
            assert!(
                !production.contains(pcm_shape),
                "UI module {} receives native PCM via {pcm_shape}",
                path.display()
            );
        }
    }
}

#[test]
fn production_native_path_does_not_force_harness_light_visuals() {
    let sources = rust_sources();
    let main = sources
        .iter()
        .find(|(path, _)| path == Path::new("main.rs"))
        .map(|(_, source)| source)
        .expect("main source exists");
    let app = sources
        .iter()
        .find(|(path, _)| path == Path::new("app.rs"))
        .map(|(_, source)| source)
        .expect("app source exists");

    assert!(
        main.contains("follow_system_theme: true"),
        "native options must continue following the system theme"
    );
    assert!(
        main.contains("Box::new(app::LocalTranscriberApp::new(cc))"),
        "normal startup must retain the production app path"
    );
    assert!(
        !main.contains("set_visuals(egui::Visuals::light())"),
        "native options/startup must not force light visuals"
    );
    assert!(
        app.contains("cc.egui_ctx.set_visuals(stitch_visuals(resolve_theme_mode("),
        "production app startup must keep its theme selection path"
    );
    assert!(
        app.contains("frame.info().system_theme"),
        "production app updates must retain system-theme resolution"
    );
    assert!(
        !app.contains("configure_harness_style"),
        "harness-only theme initialization must not leak into production"
    );
}
