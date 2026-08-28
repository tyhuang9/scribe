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

fn rust_code_mask(source: &str) -> Vec<bool> {
    let bytes = source.as_bytes();
    let mut code = vec![true; bytes.len()];
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'/') {
            let start = index;
            index += 2;
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            code[start..index].fill(false);
        } else if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'*') {
            let start = index;
            let mut depth = 1_u32;
            index += 2;
            while index < bytes.len() && depth > 0 {
                if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'*') {
                    depth += 1;
                    index += 2;
                } else if bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'/') {
                    depth -= 1;
                    index += 2;
                } else {
                    index += 1;
                }
            }
            code[start..index].fill(false);
        } else if bytes[index] == b'"'
            || (bytes[index] == b'\''
                && (bytes.get(index + 2) == Some(&b'\'')
                    || (bytes.get(index + 1) == Some(&b'\\')
                        && bytes[index + 2..bytes.len().min(index + 13)].contains(&b'\''))))
        {
            let quote = bytes[index];
            let start = index;
            index += 1;
            while index < bytes.len() {
                if bytes[index] == b'\\' {
                    index = (index + 2).min(bytes.len());
                } else if bytes[index] == quote {
                    index += 1;
                    break;
                } else {
                    index += 1;
                }
            }
            code[start..index].fill(false);
        } else if bytes[index] == b'r' {
            let mut cursor = index + 1;
            while bytes.get(cursor) == Some(&b'#') {
                cursor += 1;
            }
            if bytes.get(cursor) == Some(&b'"') {
                let hashes = cursor - index - 1;
                let start = index;
                cursor += 1;
                while cursor < bytes.len() {
                    if bytes[cursor] == b'"'
                        && bytes.get(cursor + 1..cursor + 1 + hashes) == Some(&vec![b'#'; hashes])
                    {
                        cursor += 1 + hashes;
                        break;
                    }
                    cursor += 1;
                }
                index = cursor;
                code[start..index].fill(false);
            } else {
                index += 1;
            }
        } else {
            index += 1;
        }
    }
    code
}

fn production_source(source: &str) -> String {
    const TEST_ATTR: &str = "#[cfg(test)]";
    let mut retained = source.to_owned();
    loop {
        let mask = rust_code_mask(&retained);
        let Some(start) = retained
            .match_indices(TEST_ATTR)
            .find_map(|(index, _)| mask[index].then_some(index))
        else {
            return retained;
        };
        let bytes = retained.as_bytes();
        let mut cursor = start + TEST_ATTR.len();
        while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
            cursor += 1;
        }
        while retained[cursor..].starts_with("#[") {
            let attribute_end = retained[cursor..]
                .find(']')
                .map(|offset| cursor + offset + 1)
                .expect("test-gated companion attribute must close");
            cursor = attribute_end;
            while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
                cursor += 1;
            }
        }
        let mask = rust_code_mask(&retained);
        let mut parens = 0_i32;
        let mut brackets = 0_i32;
        let mut end = None;
        let mut index = cursor;
        while index < bytes.len() {
            if !mask[index] {
                index += 1;
                continue;
            }
            match bytes[index] {
                b'(' => parens += 1,
                b')' => parens -= 1,
                b'[' => brackets += 1,
                b']' => brackets -= 1,
                b'{' if parens == 0 && brackets == 0 => {
                    let mut depth = 1_i32;
                    index += 1;
                    while index < bytes.len() && depth > 0 {
                        if mask[index] {
                            match bytes[index] {
                                b'{' => depth += 1,
                                b'}' => depth -= 1,
                                _ => {}
                            }
                        }
                        index += 1;
                    }
                    while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
                        index += 1;
                    }
                    if matches!(bytes.get(index), Some(b';' | b',')) {
                        index += 1;
                    }
                    end = Some(index);
                    break;
                }
                b';' | b',' if parens == 0 && brackets == 0 => {
                    end = Some(index + 1);
                    break;
                }
                _ => {}
            }
            index += 1;
        }
        let end = end.expect("cfg(test)-gated Rust item must have a terminator");
        retained.replace_range(start..end, "");
    }
}

fn production_source_for(_path: &Path, source: &str) -> String {
    production_source(source)
}

#[test]
fn cfg_test_stripping_preserves_later_production_for_lf_and_crlf() {
    let fixture = r###"fn before() { let _ = "}"; }
// } comment
#[cfg(test)]
#[allow(dead_code)]
fn hidden() { let _ = r#"{ nested }"#; /* } */ if true { let _ = '{'; } }
struct Boundary {
    before: u8,
    #[cfg(test)]
    hidden_field: String,
    after: u8,
}
fn after() { let _ = "production-after"; }
"###;
    for source in [fixture.to_owned(), fixture.replace('\n', "\r\n")] {
        let production = production_source(&source);
        assert!(production.contains("fn before()"));
        assert!(production.contains("fn after()"));
        assert!(production.contains("after: u8"));
        assert!(!production.contains("fn hidden()"));
        assert!(!production.contains("hidden_field"));
    }
}

#[test]
fn runtime_artifact_module_is_a_leaf_value_boundary() {
    let source = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src")
            .join("runtime_artifact.rs"),
    )
    .expect("runtime artifact source must be readable");
    for forbidden in ["crate::onnx_worker", "crate::runtime_router"] {
        assert!(
            !source.contains(forbidden),
            "artifact leaf imported execution module {forbidden:?}"
        );
    }
}

const WORKER_RUNTIME_MARKER: &str = "worker-only native runtime";
const NATIVE_RUNTIME_OWNER_PATHS: [&str; 3] =
    ["embedded_runtime.rs", "onnx_worker.rs", "runtime_router.rs"];

fn is_native_runtime_owner(path: &Path) -> bool {
    NATIVE_RUNTIME_OWNER_PATHS
        .iter()
        .any(|allowed| path == Path::new(allowed))
}

#[test]
fn native_runtime_marker_set_matches_exact_owner_allowlist() {
    let sources = rust_sources();
    let mut documented = sources
        .iter()
        .filter(|(path, source)| {
            path != Path::new("architecture_guard.rs") && source.contains(WORKER_RUNTIME_MARKER)
        })
        .map(|(path, _)| path.as_path())
        .collect::<Vec<_>>();
    documented.sort();
    let mut expected = NATIVE_RUNTIME_OWNER_PATHS
        .iter()
        .map(Path::new)
        .collect::<Vec<_>>();
    expected.sort();
    assert_eq!(documented, expected);

    let copied_marker = "//! worker-only native runtime\nfn escaped() {}";
    assert!(copied_marker.contains(WORKER_RUNTIME_MARKER));
    assert!(!is_native_runtime_owner(Path::new("copied_marker.rs")));
}

#[test]
fn generic_process_worker_transport_has_neutral_diagnostics_and_thread_names() {
    let worker = rust_sources()
        .into_iter()
        .find(|(path, _)| path == Path::new("onnx_worker.rs"))
        .map(|(_, source)| production_source(&source))
        .expect("process worker source exists");
    let transport = worker
        .split("pub(crate) struct ProcessWorkerSupervisor")
        .nth(1)
        .and_then(|tail| {
            tail.split("pub(crate) struct InferenceWorkerSupervisor")
                .next()
        })
        .expect("generic process worker transport remains delimited");
    for stale in [
        "ONNX pending map",
        "ONNX transcription request",
        "ONNX request",
        "ONNX spawn lock",
        "ONNX writer lock",
        "an ONNX stream",
        "no ONNX stream",
        "another ONNX request",
    ] {
        assert!(
            !transport.contains(stale),
            "generic process worker transport restored ONNX-specific diagnostic {stale:?}"
        );
    }
    for stale in [
        "scribe-onnx-launch",
        "scribe-onnx-reader-",
        "scribe-onnx-reaper-",
    ] {
        assert!(
            !worker.contains(stale),
            "generic process worker thread restored ONNX-specific label {stale:?}"
        );
    }
    for required in [
        "scribe-process-worker-launch",
        "scribe-process-worker-reader-",
        "scribe-process-worker-reaper-",
    ] {
        assert!(
            worker.contains(required),
            "generic process worker thread label {required:?} is missing"
        );
    }
}

#[test]
fn native_runtime_ownership_is_confined_to_exact_owner_paths() {
    let sources = rust_sources();
    let worker = sources
        .iter()
        .find(|(path, source)| {
            path != Path::new("architecture_guard.rs")
                && source.contains("pub(crate) fn maybe_run_worker()")
        })
        .map(|(_, source)| source.as_str())
        .expect("worker entrypoint exists");

    for required in [
        "INFERENCE_WORKER_FLAG",
        "VAD_WORKER_FLAG",
        "--scribe-inference-worker",
        "--scribe-vad-worker",
        "WorkerRole::Inference",
        "WorkerRole::Vad",
        "fn worker_loop_for_role",
        "RuntimeRouter::new()",
        "fn load_worker_runtime",
        "fn execute_worker_batch",
        "WireRuntimeArtifact::OnnxBundle",
    ] {
        assert!(
            worker.contains(required),
            "unified child runtime must retain {required:?}"
        );
    }
    for obsolete in [
        "LEGACY_ONNX_WORKER_FLAG",
        "OnnxSpeechRuntime",
        "OnnxWorkerSupervisor",
        "Control::Load {",
        "Control::Transcribe",
    ] {
        assert!(
            !worker.contains(obsolete),
            "obsolete nested ONNX topology {obsolete:?} must stay removed"
        );
    }
    let role_parser = worker
        .split("fn worker_role_from_args")
        .nth(1)
        .and_then(|tail| tail.split("/// Generic parent-side facade").next())
        .expect("worker role parser remains delimited before the parent facade");
    for required_rejection in [
        "value == \"--onnx-worker\"",
        "value.starts_with(\"--scribe-\")",
        "value.ends_with(\"-worker\")",
        "bail!(\"unknown private Scribe worker role\")",
    ] {
        assert!(
            role_parser.contains(required_rejection),
            "private worker-shaped arguments must fail closed via {required_rejection:?}"
        );
    }
    let router = sources
        .iter()
        .find(|(path, _)| path == Path::new("runtime_router.rs"))
        .map(|(path, source)| production_source_for(path, source))
        .expect("runtime router source exists");
    for obsolete in [
        "OnnxSupervisorControl",
        "OnnxSupervisorFactory",
        "production_onnx_supervisor",
        "ProcessWorkerSupervisor",
        "HeavyRuntimeOwner::OnnxSpeech",
    ] {
        assert!(
            !router.contains(obsolete),
            "RuntimeRouter restored obsolete ONNX machinery {obsolete:?}"
        );
    }

    let native_owner_sources = sources
        .iter()
        .filter(|(path, _)| is_native_runtime_owner(path))
        // An owner module may contain focused `#[cfg(test)]` adapters between
        // its production sections. The exact path allowlist is authoritative,
        // so inspect the complete owner module here.
        .map(|(_, source)| source.as_str())
        .collect::<Vec<_>>();
    assert!(
        native_owner_sources.iter().any(|source| {
            source.contains("OfflineRecognizer::create(")
                && source.contains("OnlineRecognizer::create(")
        }),
        "the marked child runtime must directly own the sherpa recognizers"
    );
    assert!(
        native_owner_sources
            .iter()
            .any(|source| source.contains("SileroVadModel::load_bundled(")),
        "the marked child runtime must directly own the VAD recognizer"
    );
    assert!(
        native_owner_sources
            .iter()
            .any(|source| source.contains("Model::load_with(")),
        "the marked child runtime must directly own the embedded GGUF model"
    );

    let native_constructors = [
        "Model::load_with(",
        "EmbeddedRuntime::new(",
        "TranscribeCppRuntime::new(",
        "OfflineRecognizer::create(",
        "OnlineRecognizer::create(",
        "SileroVadModel::load_bundled(",
        "NativeRuntimeOpaque",
        "NativeWhisperHandle",
    ];
    for (path, source) in &sources {
        if path == Path::new("architecture_guard.rs") {
            continue;
        }
        let production = production_source_for(path, source);
        if native_constructors
            .iter()
            .any(|constructor| production.contains(constructor))
        {
            assert!(
                is_native_runtime_owner(path),
                "native model/session/recognizer/FFI construction escaped the exact owner allowlist: {}",
                path.display()
            );
        }
    }

    let service = sources
        .iter()
        .find(|(path, _)| path == Path::new("transcription.rs"))
        .map(|(_, source)| production_source(source))
        .expect("transcription service source exists");
    assert!(
        service.contains("let worker = RuntimeWorker::new_process();"),
        "production TranscriptionService must dispatch through the process supervisor"
    );
}

#[test]
fn worker_roles_use_private_pipes_and_protocol_only_stdout() {
    let sources = rust_sources();
    let worker = sources
        .iter()
        .find(|(path, source)| {
            path != Path::new("architecture_guard.rs")
                && source.contains("pub(crate) fn maybe_run_worker()")
        })
        .map(|(_, source)| production_source(source))
        .expect("worker entrypoint exists");

    assert!(worker.contains("PROTOCOL_MAGIC: [u8; 4] = *b\"SCIF\""));
    assert!(worker.contains("PROTOCOL_VERSION: u8 = 4"));
    assert!(worker.contains("Stdio::piped()"));
    assert!(worker.contains("std::io::stdout().lock()"));
    assert!(worker.contains("stderr(Stdio::inherit())"));
    assert!(
        !worker
            .lines()
            .any(|line| line.trim_start().starts_with("print!("))
    );
    assert!(
        !worker
            .lines()
            .any(|line| line.trim_start().starts_with("println!("))
    );

    for forbidden in [
        "TcpListener",
        "TcpStream",
        "UdpSocket",
        "localhost",
        "127.0.0.1",
        "http://",
        "https://",
        "reqwest",
        "ureq",
    ] {
        assert!(
            !worker.contains(forbidden),
            "worker transport must remain private pipe-based; found {forbidden:?}"
        );
    }

    assert!(
        worker.contains("WorkerRole::Inference => INFERENCE_WORKER_FLAG")
            && worker.contains("WorkerRole::Vad => VAD_WORKER_FLAG"),
        "STT and VAD must launch as distinct worker roles"
    );
}

#[test]
fn retired_download_helpers_and_private_descriptor_fields_stay_removed() {
    let sources = rust_sources();
    let downloads = sources
        .iter()
        .find(|(path, _)| path == Path::new("managed_downloads.rs"))
        .map(|(_, source)| production_source(source))
        .expect("managed download source exists");
    for retired in [
        "download_faster_whisper_model",
        "download_vosk_model",
        "download_sherpa_model",
        "download_runner_model",
    ] {
        assert!(
            !downloads.contains(retired),
            "retired download helper {retired:?} was restored"
        );
    }

    let catalog = sources
        .iter()
        .find(|(path, _)| path == Path::new("model_catalog.rs"))
        .map(|(_, source)| production_source(source))
        .expect("model catalog source exists");
    let declaration = catalog
        .find("pub struct ModelDescriptor")
        .expect("ModelDescriptor declaration exists");
    let open = catalog[declaration..]
        .find('{')
        .map(|offset| declaration + offset)
        .expect("ModelDescriptor body opens");
    let mask = rust_code_mask(&catalog);
    let mut depth = 1_i32;
    let mut close = open + 1;
    while close < catalog.len() && depth > 0 {
        if mask[close] {
            match catalog.as_bytes()[close] {
                b'{' => depth += 1,
                b'}' => depth -= 1,
                _ => {}
            }
        }
        close += 1;
    }
    assert_eq!(depth, 0, "ModelDescriptor body closes");
    let body = &catalog[open + 1..close - 1];
    for private in [
        "backend",
        "runtime",
        "architecture",
        "artifact",
        "revision",
        "sha256",
        "filename",
    ] {
        assert!(
            !body.lines().any(|line| {
                line.trim_start()
                    .strip_prefix("pub ")
                    .is_some_and(|field| field.starts_with(&format!("{private}:")))
            }),
            "ModelDescriptor leaks private field {private:?}"
        );
    }
}

#[test]
fn retired_python_provider_stack_stays_absent_without_crossing_native_boundaries() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for retired_path in [
        "src/stt/faster_whisper.rs",
        "src/stt/vosk.rs",
        "src/stt/sherpa_onnx.rs",
        "scripts/faster_whisper_runner.py",
        "scripts/vosk_runner.py",
        "scripts/sherpa_onnx_runner.py",
        "scripts/bundle-faster-whisper-runtime.sh",
        "scripts/bundle-vosk-runtime.sh",
        "scripts/bundle-sherpa-onnx-runtime.sh",
        "scripts/bundle-moonshine-runtime.sh",
        "scripts/bundle-parakeet-runtime.sh",
    ] {
        assert!(
            !root.join(retired_path).exists(),
            "retired provider file was restored: {retired_path}"
        );
    }

    let retired_ids_and_aliases = [
        "vosk_small_en",
        "faster_whisper_tiny_en",
        "faster_whisper_base_en",
        "faster_whisper_small_en_gpu",
        "faster_whisper_medium_en_gpu",
        "faster_whisper_large_v3",
        "faster_whisper_turbo",
        "faster_whisper_distil_large_v3",
        "sherpa_onnx_zipformer_small",
        "parakeet_0_6b",
        "faster_whisper",
        "faster_whisper_small_en",
        "faster_whisper_medium_en",
        "sherpa_onnx_streaming",
    ];
    let retired_runner_invocations = [
        "faster_whisper_runner.py",
        "vosk_runner.py",
        "sherpa_onnx_runner.py",
        "scribe-faster-whisper",
        "scribe-vosk",
        "scribe-sherpa-onnx",
        "SCRIBE_FAST_WHISPER_RUNTIME_DEST",
        "SCRIBE_VOSK_RUNTIME_DEST",
        "SCRIBE_SHERPA_ONNX_RUNTIME_DEST",
    ];
    for (path, source) in rust_sources() {
        if path == Path::new("architecture_guard.rs") {
            continue;
        }
        let production = production_source_for(&path, &source);
        for retired in retired_ids_and_aliases {
            assert!(
                !production.contains(&format!("\"{retired}\"")),
                "retired provider ID/alias {retired:?} remains recognized by {}",
                path.display()
            );
        }
        for invocation in retired_runner_invocations {
            assert!(
                !production.contains(invocation),
                "retired runner invocation {invocation:?} remains in {}",
                path.display()
            );
        }
        if ![
            Path::new("model_catalog.rs"),
            Path::new("onnx_model_bundles.rs"),
            Path::new("onnx_worker.rs"),
            Path::new("runtime_artifact.rs"),
            Path::new("runtime_router.rs"),
        ]
        .contains(&path.as_path())
        {
            assert!(
                !production.contains("\"moonshine\""),
                "retired bare Moonshine provider ID escaped the native allowlist into {}",
                path.display()
            );
        }
    }

    let scripts = fs::read_dir(root.join("scripts")).expect("scripts directory must be readable");
    for entry in scripts {
        let path = entry.expect("script entry must be readable").path();
        if !path.is_file() {
            continue;
        }
        let source = fs::read_to_string(&path).expect("maintainer scripts must be UTF-8");
        for invocation in retired_runner_invocations {
            assert!(
                !source.contains(invocation),
                "retired runner invocation {invocation:?} remains in {}",
                path.display()
            );
        }
    }
    let dependency_defaults = fs::read_to_string(root.join("scripts/runtime-dependencies.env"))
        .expect("runtime dependency defaults must be readable");
    assert!(
        dependency_defaults
            .lines()
            .all(|line| line.trim().is_empty() || line.trim_start().starts_with('#')),
        "retired Python dependency pins were restored"
    );
    let dependency_checker =
        fs::read_to_string(root.join("scripts/check-runtime-dependency-updates.py"))
            .expect("dependency checker must be readable");
    assert_eq!(
        dependency_checker
            .lines()
            .find(|line| line.trim_start().starts_with("PINNED_PACKAGES:"))
            .map(str::trim),
        Some("PINNED_PACKAGES: dict[str, str] = {}")
    );

    let stt = fs::read_to_string(root.join("src/stt/mod.rs"))
        .expect("STT compatibility module must be readable");
    let direct_dispatch = production_source(&stt)
        .split("pub fn transcribe_with_config")
        .nth(1)
        .expect("direct compatibility dispatch exists")
        .to_owned();
    assert!(
        !direct_dispatch.contains("provider_for_backend"),
        "direct compatibility dispatch must not perform provider lookup"
    );

    for retained_path in [
        "vendor/sherpa-onnx-sys/LICENSE",
        "native/sherpa-onnx-v1.13.5/PROVENANCE.md",
        "resources/licenses/Moonshine-MIT.txt",
    ] {
        assert!(
            root.join(retained_path).is_file(),
            "native Sherpa/Moonshine evidence was removed: {retained_path}"
        );
    }
}

#[test]
fn private_onnx_runtime_contract_does_not_leak_into_product_surfaces() {
    let sources = rust_sources();
    let protected = [
        Path::new("app.rs"),
        Path::new("config.rs"),
        Path::new("model_catalog.rs"),
        Path::new("runtime_catalog.rs"),
    ];
    let forbidden = [
        "OnnxModelSpec",
        "OnnxModelFamily",
        "OnnxFileRole",
        "OnnxSpeech",
        "native-onnx",
    ];

    for (path, source) in &sources {
        if !protected.iter().any(|protected| path == protected) && !path.starts_with("ui") {
            continue;
        }
        let production = production_source(source).replace("moonshine-tiny-en-int8-onnx", "");
        for identifier in forbidden {
            assert!(
                !production.contains(identifier),
                "private ONNX runtime identifier {identifier:?} leaked into {}",
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
        let is_ui = path.starts_with("ui") && path != Path::new("ui/harness.rs");
        if !is_application && !is_ui {
            continue;
        }
        let production = production_source(source).replace("moonshine-tiny-en-int8-onnx", "");
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
        "onnx_model_bundles.rs",
        "runtime_artifact.rs",
        "runtime_catalog.rs",
        "runtime_router.rs",
        "settings/schema.rs",
        "silero_vad_native.rs",
        "transcription.rs",
        "ui/harness.rs",
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
            || is_native_runtime_owner(path)
            || allowed_files
                .iter()
                .any(|allowed| path == Path::new(allowed))
        {
            continue;
        }
        let production = production_source(source)
            .replace("moonshine-tiny-en-int8-onnx", "")
            .to_ascii_lowercase();
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
fn onnx_bundle_http_and_typed_receipts_stay_below_the_service_boundary() {
    let sources = rust_sources();
    let bundles = sources
        .iter()
        .find(|(path, _)| path == Path::new("onnx_model_bundles.rs"))
        .map(|(_, source)| production_source(source))
        .expect("private ONNX bundle module exists");
    assert_eq!(
        bundles
            .matches("download_pinned_artifact_for_target(")
            .count(),
        1,
        "only the explicit bundle installation path may invoke the HTTP downloader"
    );
    assert!(bundles.contains("fn stage_onnx_bundle_install("));
    assert!(bundles.contains("fn verified_receipt_at("));
    assert!(bundles.contains("OnnxModelSpec"));

    for protected in [
        Path::new("model_catalog.rs"),
        Path::new("models.rs"),
        Path::new("runtime_catalog.rs"),
    ] {
        let production = sources
            .iter()
            .find(|(path, _)| path == protected)
            .map(|(_, source)| production_source(source))
            .expect("protected source exists");
        for forbidden in [
            "onnx_model_bundles",
            "OnnxBundleReceipt",
            "OnnxBundleManifest",
            "stage_onnx_bundle_install",
        ] {
            assert!(
                !production.contains(forbidden),
                "private ONNX bundle contract {forbidden:?} leaked into {}",
                protected.display()
            );
        }
    }
}

#[test]
fn tentative_transcripts_have_no_output_or_history_module_path() {
    let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let protected = [
        source_root.join("text_output.rs"),
        source_root.join("history").join("mod.rs"),
        source_root.join("history").join("database.rs"),
    ];

    for path in protected {
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{} must be readable: {error}", path.display()));
        let production = production_source(&source).to_ascii_lowercase();
        assert!(
            !production.contains("tentative"),
            "{} must only receive finalized text; tentative text belongs in the overlay",
            path.display()
        );
    }
}

#[test]
fn route_shell_has_no_synthetic_models_scroll_surface() {
    let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let screens = fs::read_to_string(source_root.join("ui").join("screens.rs"))
        .expect("shared screens source must be readable");
    let app = fs::read_to_string(source_root.join("app.rs"))
        .expect("application source must be readable");
    let harness = fs::read_to_string(source_root.join("ui").join("harness.rs"))
        .expect("harness source must be readable");

    for forbidden in [
        "models_footer_spacer",
        "MODEL_COMPARISON_BODY_BLEED",
        "comparison_top + 10_000.0",
        "comparison_top + 10000.0",
    ] {
        assert!(
            !screens.contains(forbidden),
            "shared models UI must not recreate synthetic scroll artifact {forbidden}"
        );
    }
    assert!(screens.contains("fn show_route_scroll"));
    assert!(screens.contains("const ROUTE_TOP_INSET: f32 = 28.0"));
    assert!(screens.contains("const ROUTE_HORIZONTAL_INSET: f32 = 28.0"));
    assert!(screens.contains("let route_width = ui.available_width()"));
    assert!(screens.contains("ui.set_width(route_width)"));
    assert!(
        screens.contains("ui.set_width((route_width - ROUTE_HORIZONTAL_INSET * 2.0).max(0.0))")
    );
    assert!(screens.contains("comparison_viewport.width() - ROUTE_HORIZONTAL_INSET * 2.0"));
    assert!(app.contains("show_route_scroll(ui, UiRoute::Models"));
    assert!(app.contains("show_route_scroll(ui, UiRoute::History"));
    assert!(app.contains("SettingsTab::About"));
    assert!(app.contains("passive_microphone_monitor_needed"));
    assert!(
        !app.contains("page-scroll"),
        "legacy pages must not reintroduce an inner route scroll area"
    );
    assert!(harness.contains("show_route_scroll(ui, view.route"));
}

#[test]
fn no_web_runtime_or_ui_pcm_transport_is_present() {
    let sources = rust_sources();
    for (path, source) in &sources {
        if path == Path::new("architecture_guard.rs") {
            continue;
        }
        let production = production_source(source).to_ascii_lowercase();
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
        let production = production_source(source);
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

#[test]
fn windows_release_bundles_the_exact_offline_base_model_with_attribution() {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest_path = repository
        .join("runtime-manifests")
        .join("whisper-base-en-q8_0-windows-x64.json");
    let manifest: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(&manifest_path).expect("bundled model manifest must be readable"),
    )
    .expect("bundled model manifest must be valid JSON");
    let catalog = crate::model_catalog::runtime_model_manifest(
        &crate::transcription::ModelId::new(crate::model_catalog::BUNDLED_BASE_MODEL_ID),
    )
    .expect("bundled base model remains in the normalized catalog");

    assert_eq!(manifest["schema_version"], 1);
    assert_eq!(manifest["model_id"], catalog.id);
    assert_eq!(manifest["repository"], catalog.artifact_repository);
    assert_eq!(manifest["revision"], catalog.artifact_revision);
    assert_eq!(manifest["artifact_filename"], catalog.artifact_filename);
    assert_eq!(manifest["size_bytes"], catalog.artifact_size_bytes);
    assert_eq!(manifest["sha256"], catalog.artifact_sha256);
    assert_eq!(manifest["platform_triple"], "x86_64-pc-windows-msvc");
    assert_eq!(
        manifest["attribution_files"],
        serde_json::json!([
            "resources/licenses/Apache-2.0.txt",
            "resources/licenses/OpenAI-Whisper-MIT.txt",
            "resources/licenses/Whisper-Base-En-NOTICE.txt"
        ])
    );

    let bundler = fs::read_to_string(repository.join("scripts").join("bundle-base-model.ps1"))
        .expect("bundled model packaging script must be readable");
    for required in [
        "Get-FileHash",
        "Invoke-NativeProcess",
        "RedirectStandardOutput",
        "WaitForExit",
        "--scribe-install-smoke-parent",
        "HF_HUB_OFFLINE",
        "TRANSFORMERS_OFFLINE",
        "cancellation_verified",
        "capabilities.cancellation",
        "exact executable name local-transcriber.exe",
        "canonical executable parent must equal",
        "Assert-NoReparseAncestors",
        "Assert-TreeHasNoReparsePoints",
    ] {
        assert!(
            bundler.contains(required),
            "bundled model packaging must retain {required}"
        );
    }
    for forbidden in ["Invoke-WebRequest", "Start-BitsTransfer", "curl.exe"] {
        assert!(
            !bundler.contains(forbidden),
            "release packaging must not download the bundled model via {forbidden}"
        );
    }

    let release = fs::read_to_string(repository.join("scripts").join("build-windows-release.ps1"))
        .expect("Windows release script must be readable");
    for required in [
        "cargo build --locked --offline --release --all-features --target $targetTriple",
        "x86_64-pc-windows-msvc",
        r#"target\$targetTriple\release"#,
        "Assert-Amd64Pe",
        "0x8664",
        "Assert-SafeStagingPath",
        "Remove-ValidatedStaging",
        "Assert-ExactAllowlist",
        "bundle-inventory.json",
        "README.txt",
        "Assert-WindowsGuiSubsystem",
        "Windows GUI (2)",
        "Invoke-NativeProcess",
        "RedirectStandardOutput",
        "WaitForExit",
        "Move-Item -LiteralPath $stagingBundle -Destination $finalBundle",
        r#"artifacts\Scribe-windows-x64"#,
        "Final release bundle already exists",
        "A stale release staging sibling exists",
    ] {
        assert!(
            release.contains(required),
            "Windows release packaging must retain {required}"
        );
    }
    assert!(
        !release.contains(r#"target\release"#),
        "the unqualified Cargo release directory must not be used as a bundle"
    );

    let release_inputs = fs::read_to_string(
        repository
            .join("scripts")
            .join("prepare-windows-release-inputs.ps1"),
    )
    .expect("release input preparation script must be readable");
    for required in [
        "whisper-cpp-v1.9.1-windows-x64.json",
        "whisper-base-en-q8_0-windows-x64.json",
        "Get-FileHash",
        "Expand-Archive",
        "huggingface.co/$modelRepository/resolve/$modelRevision/$modelFilename",
        "Release input SHA-256 mismatch",
    ] {
        assert!(
            release_inputs.contains(required),
            "release input preparation must retain {required}"
        );
    }

    let workflow = fs::read_to_string(
        repository
            .join(".github")
            .join("workflows")
            .join("release.yml"),
    )
    .expect("Windows release workflow must be readable");
    for required in [
        "prepare-windows-release-inputs.ps1",
        "build-windows-release.ps1",
        "verify-windows-release-package.ps1",
        "-BundlePath dist\\portable",
    ] {
        assert!(
            workflow.contains(required),
            "Windows release workflow must retain {required}"
        );
    }
    assert!(
        !workflow.contains("Copy-Item target\\release\\local-transcriber.exe"),
        "Windows release workflow must not publish a bare executable"
    );

    let installer = fs::read_to_string(repository.join("installer").join("scribe.iss"))
        .expect("Windows installer script must be readable");
    assert!(
        installer.contains("Source: \"..\\dist\\portable\\*\"")
            && installer.contains("recursesubdirs")
            && installer.contains("createallsubdirs"),
        "Windows installer must recursively copy the validated portable payload"
    );

    let main = fs::read_to_string(repository.join("src").join("main.rs"))
        .expect("application main source must be readable");
    assert!(
        main.contains("windows_subsystem = \"windows\"")
            && main.contains("report_startup_failure")
            && main.contains("MessageBoxW"),
        "non-debug Windows startup failures must stay visible without a console"
    );

    let packaging_tests = fs::read_to_string(
        repository
            .join("scripts")
            .join("test-windows-release-packaging.ps1"),
    )
    .expect("Windows release fail-closed tests must be readable");
    for required in [
        "PE Machine mismatch",
        "Validated staging cleanup",
        "Out-of-bounds cleanup",
        "outside the explicit allowlist",
        "Cargo-target bundle path",
        "Existing final bundle",
        "Stale staging refusal",
        "exact executable name",
        "canonical executable parent",
        "PE subsystem mismatch",
        "Windows release workflow",
        "Windows installer",
    ] {
        assert!(
            packaging_tests.contains(required),
            "Windows release fail-closed tests must cover {required}"
        );
    }

    let package_verifier = fs::read_to_string(
        repository
            .join("scripts")
            .join("verify-windows-release-package.ps1"),
    )
    .expect("release payload verifier must be readable");
    for required in [
        "Release payload differs from its explicit inventory",
        "Invoke-NativeProcess",
        "RedirectStandardOutput",
        "WaitForExit",
        "/VERYSILENT",
        "Assert-Bundle -Root $installedRoot -AllowedAdditionalFiles $InnoSetupUninstallerArtifacts",
    ] {
        assert!(
            package_verifier.contains(required),
            "release payload verifier must retain {required}"
        );
    }

    let notice = fs::read_to_string(
        repository
            .join("resources")
            .join("licenses")
            .join("Whisper-Base-En-NOTICE.txt"),
    )
    .expect("bundled model notice must be readable");
    assert!(notice.contains(catalog.artifact_repository));
    assert!(notice.contains(catalog.artifact_revision));
    assert!(notice.contains(catalog.artifact_sha256));
    assert!(notice.contains("Apache-2.0.txt"));
    assert!(notice.contains("OpenAI-Whisper-MIT.txt"));
    assert!(notice.contains("official OpenAI Whisper model distribution metadata"));
    assert!(notice.contains("artifacts as Apache-2.0"));
    assert!(notice.contains("source-code repository includes an MIT License"));
    assert!(notice.contains("not presented here as the license for the"));
    assert!(notice.contains("does not state a legal conclusion"));
    let upstream_mit = fs::read_to_string(
        repository
            .join("resources")
            .join("licenses")
            .join("OpenAI-Whisper-MIT.txt"),
    )
    .expect("OpenAI Whisper MIT notice must be readable");
    assert!(upstream_mit.contains("Copyright (c) 2022 OpenAI"));
}
