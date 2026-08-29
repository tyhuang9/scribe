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

#[test]
fn static_gguf_and_native_onnx_are_the_only_inference_architectures() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let artifacts = fs::read_to_string(root.join("src/runtime_artifact.rs"))
        .expect("runtime artifact source must be readable");
    assert!(artifacts.contains("enum RuntimeArtifact"));
    assert!(artifacts.contains("Gguf(RuntimeModel)"));
    assert!(artifacts.contains("OnnxBundle(OnnxModelSpec)"));

    let catalog = fs::read_to_string(root.join("src/model_catalog.rs"))
        .expect("model catalog source must be readable");
    assert!(catalog.contains("enum ArtifactFormat"));
    assert!(catalog.contains("Gguf,"));

    let worker = fs::read_to_string(root.join("src/onnx_worker.rs"))
        .expect("inference worker source must be readable");
    assert!(worker.contains("enum WireRuntimeArtifact"));
    assert!(worker.contains("Gguf(WireRuntimeModel)"));
    assert!(worker.contains("OnnxBundle(OnnxModelSpec)"));
    assert!(worker.contains("ASR recognizers are unavailable in the desktop executable"));
    assert!(worker.contains("resolve_adjacent_inference_worker"));
    assert!(worker.contains("scribe-inference-worker{}"));
    assert!(worker.contains("INFERENCE_WORKER_FLAG"));

    let inference_server = fs::read_to_string(root.join("src/inference_server.rs"))
        .expect("worker-only inference server source must be readable");
    assert!(inference_server.contains("OfflineRecognizer::create("));
    assert!(inference_server.contains("OnlineRecognizer::create("));
    assert!(inference_server.contains("use sherpa_onnx"));
    assert!(!worker.contains("OfflineRecognizer::create("));
    assert!(!worker.contains("OnlineRecognizer::create("));

    let router = fs::read_to_string(root.join("src/runtime_router.rs"))
        .expect("runtime router source must be readable");
    assert!(router.contains("enum RuntimeKind"));
    assert!(router.contains("TranscribeCpp,"));
    assert!(router.contains("EmbeddedRuntime::new("));
    assert!(!router.contains("Command::new"));

    let manifest =
        fs::read_to_string(root.join("Cargo.toml")).expect("Cargo manifest must be readable");
    assert!(manifest.contains("inference-worker = [\"dep:transcribe-cpp\"]"));
    assert!(manifest.contains(
        "transcribe-cpp = { version = \"=0.1.3\", default-features = false, optional = true }"
    ));
    assert!(manifest.contains("name = \"scribe-inference-worker\""));
    assert!(manifest.contains("required-features = [\"inference-worker\"]"));
}

#[test]
fn dynamic_whisper_and_standalone_runtime_paths_stay_removed() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let obsolete_paths = [
        "src/compatibility_bridge.rs",
        "src/runtime_catalog.rs",
        "src/stt/whisper_cpp.rs",
        "native/whisper_shim.c",
    ];
    for relative in obsolete_paths {
        assert!(
            !root.join(relative).exists(),
            "retired runtime path was restored: {relative}"
        );
    }

    let forbidden = [
        "LegacyGgml",
        "LegacyCompatibility",
        "LegacyBatchAdapter",
        "transcribe_legacy",
        "whisper_cli",
        "whisper-cli",
        "whisper.dll",
        "ggml.dll",
        "SCRIBE_WHISPER_",
        "SCRIBE_RUNTIME_DEST",
        "RepairModelRuntime",
        "MaintainModelRuntime",
        "libloading::",
        "scribe_whisper_",
    ];
    for (path, source) in rust_sources() {
        if path == Path::new("architecture_guard.rs") {
            continue;
        }
        let production = production_source_for(&path, &source);
        for retired in forbidden {
            assert!(
                !production.contains(retired),
                "retired runtime token {retired:?} remains in production source {}",
                path.display()
            );
        }
    }

    let build = fs::read_to_string(root.join("build.rs")).expect("build script must be readable");
    for retired in [
        "whisper_shim",
        "scribe_whisper",
        "LoadLibrary",
        "GetProcAddress",
    ] {
        assert!(
            !build.contains(retired),
            "dynamic Whisper build token {retired:?} was restored"
        );
    }

    let manifest =
        fs::read_to_string(root.join("Cargo.toml")).expect("Cargo manifest must be readable");
    assert!(
        !manifest
            .lines()
            .any(|line| line.trim_start().starts_with("libloading =")),
        "libloading must not be a direct production dependency"
    );

    let tray = fs::read_to_string(root.join("src/tray.rs")).expect("tray source must be readable");
    for required in [
        "CString::new(name)",
        "libc::dlopen",
        "libc::RTLD_LAZY | libc::RTLD_LOCAL",
        "libc::dlclose(handle)",
    ] {
        assert!(
            tray.contains(required),
            "Linux tray availability probe must retain {required:?}"
        );
    }
}

const WORKER_RUNTIME_MARKER: &str = "worker-only native runtime";
const NATIVE_RUNTIME_OWNER_PATHS: [&str; 4] = [
    "embedded_runtime.rs",
    "inference_server.rs",
    "onnx_worker.rs",
    "runtime_router.rs",
];

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
        .find(|(path, _)| path == Path::new("onnx_worker.rs"))
        .map(|(_, source)| source.as_str())
        .expect("worker entrypoint exists");

    for required in [
        "INFERENCE_WORKER_FLAG",
        "VAD_WORKER_FLAG",
        "--scribe-inference-worker",
        "--scribe-vad-worker",
        "WorkerRole::Inference",
        "WorkerRole::Vad",
        "pub(crate) fn maybe_run_vad_worker()",
        "pub(crate) fn run_inference_worker_with_factory",
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

    let main = sources
        .iter()
        .find(|(path, _)| path == Path::new("main.rs"))
        .map(|(_, source)| production_source(source))
        .expect("desktop entrypoint exists");
    assert!(main.contains("onnx_worker::maybe_run_vad_worker()"));
    assert!(!main.contains("inference_server"));
    assert!(!main.contains("run_inference_worker_with_factory"));

    let inference_server = sources
        .iter()
        .find(|(path, _)| path == Path::new("inference_server.rs"))
        .map(|(_, source)| production_source(source))
        .expect("worker-only inference server exists");
    for required in [
        "OfflineRecognizer::create(",
        "OnlineRecognizer::create(",
        "run_inference_worker_with_factory(&NativeRecognizerFactory)",
    ] {
        assert!(
            inference_server.contains(required),
            "worker-only inference server must retain {required:?}"
        );
    }
    for forbidden in [
        "OfflineRecognizer",
        "OnlineRecognizer",
        "OfflineRecognizer::create(",
        "OnlineRecognizer::create(",
        "NativeRecognizerFactory",
        "offline_recognizer_config",
        "online_recognizer_config",
    ] {
        assert!(
            !worker.contains(forbidden),
            "desktop/VAD worker substrate must not compile ASR server token {forbidden:?}"
        );
    }

    let dedicated = sources
        .iter()
        .find(|(path, _)| path == Path::new("bin/scribe-inference-worker.rs"))
        .map(|(_, source)| production_source(source))
        .expect("dedicated inference entrypoint exists");
    assert!(dedicated.contains("mod inference_server;"));
    assert!(dedicated.contains("inference_server::run()"));
    assert!(!dedicated.contains("maybe_run_vad_worker"));
}

#[test]
fn verified_worker_pack_stage_remains_fail_closed_and_provider_inert() {
    let module = include_str!("gpu_worker_pack/mod.rs");
    let manifest = include_str!("gpu_worker_pack/manifest.rs");
    let production_manifest = manifest
        .split("#[cfg(test)]")
        .next()
        .expect("manifest has a production section");
    assert!(module.contains("pub(crate) fn production_registry() -> Vec<VerifiedPack>"));
    assert!(module.contains("Vec::new()"));
    assert!(production_manifest.contains("struct ProductionTrustRoot"));
    assert!(production_manifest.contains("fn public_key(&self, _key_id: &str) -> Option<&[u8]>"));
    assert!(production_manifest.contains("None"));
    for forbidden in ["Ed25519KeyPair", "private_key", "signing_seed"] {
        assert!(
            !production_manifest.contains(forbidden),
            "production pack verifier contains signing material/API marker {forbidden}"
        );
    }
    assert!(module.contains("--scribe-verify-worker-pack"));
    assert!(module.contains("PackBackend::Cuda"));
    assert!(module.contains("PackBackend::Vulkan"));
    assert!(module.contains("PackBackend::Metal"));
}

#[test]
fn worker_pack_health_persistence_stays_bounded_and_content_free() {
    let source = include_str!("gpu_worker_pack/health.rs");
    let production = source
        .split("#[cfg(test)]")
        .next()
        .expect("health cache has a production section");
    for required in [
        "pack_digest",
        "runtime_abi",
        "os_arch",
        "driver_version",
        "stable_device_identity",
        "model_digest",
        "app_build",
        "device_set_digest",
        "FIRST_QUARANTINE_SECONDS: u64 = 15 * 60",
        "SECOND_QUARANTINE_SECONDS: u64 = 6 * 60 * 60",
        "THIRD_QUARANTINE_SECONDS: u64 = 7 * 24 * 60 * 60",
    ] {
        assert!(
            production.contains(required),
            "health contract lost {required}"
        );
    }
    for forbidden in [
        "audio_path",
        "transcript",
        "raw_error",
        "diagnostic_text",
        "error_message",
    ] {
        assert!(
            !production.contains(forbidden),
            "health cache production schema regained forbidden content: {forbidden}"
        );
    }
}

#[test]
fn release_packaging_accepts_only_compiled_verified_declared_pack_roots() {
    let build = include_str!("../scripts/build-windows-release.ps1");
    let stage = include_str!("../scripts/stage-verified-worker-packs.ps1");
    let installer = include_str!("../installer/scribe.iss");
    let workflow = include_str!("../.github/workflows/release.yml");
    for required in [
        "WorkerPackRoot",
        "stage-verified-worker-packs.ps1",
        "worker-pack-catalog.json",
        "PackFiles",
    ] {
        assert!(
            build.contains(required),
            "release pack integration lost {required}"
        );
    }
    assert!(stage.matches("Invoke-PackVerifier $verifier").count() >= 2);
    assert!(stage.contains("workers/packs/"));
    assert!(stage.contains("PackRoot.Count -gt 8"));
    assert!(stage.contains("allPackFiles.Count -gt 1024"));
    assert!(!stage.contains("Ed25519KeyPair"));
    assert!(!stage.contains("SIGNING_KEY"));
    assert!(installer.contains("#include WorkerPackAllowlist"));
    assert!(installer.contains("IsGeneratedWorkerPackFile(RelativePath)"));
    assert!(workflow.contains("/DWorkerPackAllowlist=..\\dist\\worker-pack-allowlist.iss"));
    assert!(!build.contains("--features vulkan-acceleration"));
}

#[test]
fn worker_roles_use_private_pipes_and_protocol_only_stdout() {
    let sources = rust_sources();
    let worker = sources
        .iter()
        .find(|(path, _)| path == Path::new("onnx_worker.rs"))
        .map(|(_, source)| production_source(source))
        .expect("worker entrypoint exists");

    assert!(worker.contains("PROTOCOL_MAGIC: [u8; 4] = *b\"SCIF\""));
    assert!(worker.contains("PROTOCOL_VERSION: u8 = 5"));
    for bound_capability in [
        "challenge",
        "app_build",
        "worker_build",
        "bundled_worker_sha256",
        "abi",
        "role",
        "provider",
        "artifacts",
    ] {
        assert!(
            worker.contains(bound_capability),
            "SCIF v5 capability must bind {bound_capability}"
        );
    }
    assert!(worker.contains("Stdio::piped()"));
    assert!(worker.contains("std::io::stdout().lock()"));
    assert!(worker.contains("stderr(Stdio::inherit())"));
    for launch_hardening in [
        "configure_worker_environment(&mut command)",
        "command.env_clear()",
        "harden_windows_dll_search",
        "SetDefaultDllDirectories",
        "FILE_FLAG_OPEN_REPARSE_POINT",
        "worker executable must not be a hardlink",
        "executable.revalidate()",
    ] {
        assert!(
            worker.contains(launch_hardening),
            "worker launch hardening must retain {launch_hardening}"
        );
    }
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
    for removed_runtime_maintenance_path in [
        "scripts/runtime-dependencies.env",
        "scripts/check-runtime-dependency-updates.py",
    ] {
        assert!(
            !root.join(removed_runtime_maintenance_path).exists(),
            "retired dynamic runtime maintenance tool was restored: {removed_runtime_maintenance_path}"
        );
    }

    let stt = fs::read_to_string(root.join("src/stt/mod.rs"))
        .expect("STT cancellation module must be readable");
    let direct_dispatch = production_source(&stt);
    assert!(
        !direct_dispatch.contains("provider_for_backend"),
        "STT cancellation boundary must not perform provider lookup"
    );
    assert!(!direct_dispatch.contains("Command::new"));

    for retained_path in [
        "vendor/sherpa-onnx-sys/LICENSE",
        "native/sherpa-onnx-v1.13.5/PROVENANCE.md",
        "native/transcribe-cpp-v0.1.3/LICENSE",
        "native/transcribe-cpp-v0.1.3/PROVENANCE.md",
        "native/whisper-f049fff/LICENSE",
        "native/whisper-f049fff/PROVENANCE.md",
        "resources/licenses/Moonshine-MIT.txt",
    ] {
        assert!(
            root.join(retained_path).is_file(),
            "native transcription/Sherpa/Moonshine evidence was removed: {retained_path}"
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
        "config.rs",
        "installations.rs",
        "managed_downloads.rs",
        "model_catalog.rs",
        "models.rs",
        "onnx_model_bundles.rs",
        "runtime_artifact.rs",
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

    for protected in [Path::new("model_catalog.rs"), Path::new("models.rs")] {
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
    assert!(
        !bundler.contains("\"-\","),
        "bundled model smoke must use the current self-contained helper protocol"
    );

    let release = fs::read_to_string(repository.join("scripts").join("build-windows-release.ps1"))
        .expect("Windows release script must be readable");
    for required in [
        "cargo build --locked --offline --release --bin local-transcriber --features ui-harness --target $targetTriple",
        "cargo build --locked --offline --release --bin scribe-inference-worker --features inference-worker --target $targetTriple",
        "Get-FileHash -Algorithm SHA256 -LiteralPath $sourceInferenceWorker",
        "SCRIBE_BUNDLED_WORKER_SHA256",
        "SCRIBE_BUILDING_WORKER",
        "scribe-inference-worker.exe",
        "x86_64-pc-windows-msvc",
        "CARGO_TARGET_DIR",
        "[System.IO.Path]::IsPathFullyQualified($env:CARGO_TARGET_DIR)",
        "Join-Path $repositoryRoot $env:CARGO_TARGET_DIR",
        r#"$cargoTargetRoot "$targetTriple\release""#,
        "Assert-Amd64Pe",
        "0x8664",
        "Assert-SafeStagingPath",
        "Remove-ValidatedStaging",
        "Assert-ExactAllowlist",
        "Assert-AllowedPayloadFile",
        "bundle-inventory.json",
        "README.txt",
        "Assert-WindowsGuiSubsystem",
        "Assert-ReviewedWindowsPe",
        "Windows GUI (2)",
        "Windows console PE",
        "Invoke-NativeProcess",
        "RedirectStandardOutput",
        "WaitForExit",
        "Move-Item -LiteralPath $stagingBundle -Destination $finalBundle",
        r#"artifacts\Scribe-windows-x64"#,
        "Final release bundle already exists",
        "A stale release staging sibling exists",
        "licenses/THIRD-PARTY-NOTICES.txt",
        "licenses/transcribe.cpp-MIT.txt",
        "licenses/transcribe.cpp-PROVENANCE.md",
        "licenses/whisper.cpp-MIT.txt",
        "licenses/whisper.cpp-PROVENANCE.md",
        "licenses/sherpa-onnx-PROVENANCE.md",
        "licenses/Silero-VAD-MIT.txt",
        "licenses/Silero-VAD-PROVENANCE.md",
        "This release workflow does not claim Authenticode signing",
        "setup refuses safely and does not delete or change that content",
        "Do not delete per-user app data or external/imported models as part of rollback",
        "Assert-ReleaseSmokeDiagnostics",
        r#"detected architecture 'whisper'"#,
    ] {
        assert!(
            release.contains(required),
            "Windows release packaging must retain {required}"
        );
    }
    for forbidden in ["--all-features", "vulkan-acceleration"] {
        assert!(
            !release.contains(forbidden),
            "Windows release packaging must not enable {forbidden}"
        );
    }

    let vulkan_developer_build = fs::read_to_string(
        repository
            .join("scripts")
            .join("build-vulkan-worker-dev.ps1"),
    )
    .expect("Vulkan developer worker build script must be readable");
    for required in [
        "--bin scribe-inference-worker --features vulkan-acceleration",
        "--bin local-transcriber --features ui-harness,vulkan-acceleration",
        "$env:SCRIBE_BUILDING_WORKER = '1'",
        "$env:SCRIBE_BUNDLED_WORKER_SHA256 = $null",
        "vulkan-dev-bundle-",
        "Copy-Item -LiteralPath $cargoDesktop -Destination $desktop",
        "Copy-Item -LiteralPath $cargoWorker -Destination $worker",
    ] {
        assert!(
            vulkan_developer_build.contains(required),
            "Vulkan developer build must retain {required:?}"
        );
    }
    assert!(
        !vulkan_developer_build.contains("--release"),
        "the opt-in Vulkan helper must not claim or create a release build"
    );
    assert!(
        !release.contains(r#"target\release"#),
        "the unqualified Cargo release directory must not be used as a bundle"
    );

    let worker_build = release
        .find("$env:SCRIBE_BUILDING_WORKER = '1'")
        .expect("release build marks the worker-only compilation");
    let worker_cargo = release
        .find("cargo build --locked --offline --release --bin scribe-inference-worker")
        .expect("release build compiles the worker first");
    let worker_marker_clear = release[worker_cargo..]
        .find("$env:SCRIBE_BUILDING_WORKER = $null")
        .map(|offset| worker_cargo + offset)
        .expect("release build clears the worker marker before desktop compilation");
    let worker_hash = release
        .find("Get-FileHash -Algorithm SHA256 -LiteralPath $sourceInferenceWorker")
        .expect("release build hashes the completed worker image");
    let desktop_cargo = release
        .find("cargo build --locked --offline --release --bin local-transcriber")
        .expect("release build compiles the anchored desktop second");
    assert!(
        worker_build < worker_cargo
            && worker_cargo < worker_marker_clear
            && worker_marker_clear < worker_hash
            && worker_hash < desktop_cargo,
        "release builds must clear the digest, mark/build the worker, clear the marker, hash the image, then build the desktop"
    );

    let build_script = fs::read_to_string(repository.join("build.rs"))
        .expect("Cargo build script must be readable");
    for required in [
        "SCRIBE_BUILDING_WORKER",
        "release desktop build requires SCRIBE_BUNDLED_WORKER_SHA256",
        "release worker build must clear SCRIBE_BUNDLED_WORKER_SHA256",
        "cargo:rustc-env=SCRIBE_BUNDLED_WORKER_SHA256={digest}",
    ] {
        assert!(
            build_script.contains(required),
            "release trust-anchor build script must retain {required:?}"
        );
    }

    let release_inputs = fs::read_to_string(
        repository
            .join("scripts")
            .join("prepare-windows-release-inputs.ps1"),
    )
    .expect("release input preparation script must be readable");
    for required in [
        "whisper-base-en-q8_0-windows-x64.json",
        "Get-FileHash",
        "huggingface.co/$modelRepository/resolve/$modelRevision/$modelFilename",
        "Release input SHA-256 mismatch",
    ] {
        assert!(
            release_inputs.contains(required),
            "release input preparation must retain {required}"
        );
    }
    for forbidden in ["RuntimeSource", "runtime-manifest.json", "Expand-Archive"] {
        assert!(
            !release_inputs.contains(forbidden),
            "release input preparation must not retain dynamic runtime contract {forbidden}"
        );
    }

    for removed_runtime_path in [
        "runtime-manifests/whisper-cpp-v1.9.1-windows-x64.json",
        "scripts/build-release-bundle.sh",
        "scripts/build-whisper-cuda.sh",
        "scripts/build-whisper-ollama-cuda-backend.sh",
        "scripts/bundle-whisper-runtime.ps1",
        "scripts/bundle-whisper-runtime.sh",
        "scripts/check-runtime-dependency-updates.py",
        "scripts/runtime-dependencies.env",
    ] {
        assert!(
            !repository.join(removed_runtime_path).exists(),
            "obsolete dynamic runtime release artifact must stay removed: {removed_runtime_path}"
        );
    }
    for forbidden in [
        "RuntimeSource",
        "runtimes/whisper_cpp",
        "runtime-manifest.json",
        "\"-\",",
    ] {
        assert!(
            !release.contains(forbidden),
            "self-contained release build must not stage dynamic runtime artifact {forbidden}"
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
        "-PortableZipPath dist\\Scribe-windows-x64.zip",
        "actions/checkout@fbc6f3992d24b796d5a048ff273f7fcc4a7b6c09",
        "actions/upload-artifact@b7c566a772e6b6bfb58ed0dc250532a479d7789f",
        "dtolnay/rust-toolchain@01ba1edad32c6f80dbcce879d3e0fa5a00b2a84e",
        "INNO_NUPKG_SHA256: a0dad33db33099d9cd2b89ac2d08b5d70c589b15118ced3b95f469f044f99950",
        "INNO_INSTALLER_SHA256: 4d11e8050b6185e0d49bd9e8cc661a7a59f44959a621d31d11033124c4e8a7b0",
        "-ExerciseStableUpgrade",
        "-EvidenceDirectory dist\\installer-verification-logs",
        "name: windows-installer-verification-logs",
    ] {
        assert!(
            workflow.contains(required),
            "Windows release workflow must retain {required}"
        );
    }
    for uses_line in workflow
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("uses:"))
    {
        let reference = uses_line
            .strip_prefix("uses:")
            .expect("uses line prefix checked")
            .split('#')
            .next()
            .expect("action reference must exist")
            .trim();
        let (_, revision) = reference
            .rsplit_once('@')
            .expect("GitHub Action reference must contain @");
        assert!(
            revision.len() == 40
                && revision
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "GitHub Action reference must use a lowercase immutable full SHA: {uses_line}"
        );
    }
    assert!(
        !workflow.contains("choco install innosetup"),
        "Inno Setup acquisition must not trust a mutable network-only Chocolatey install"
    );
    assert!(
        !workflow.contains("Copy-Item target\\release\\local-transcriber.exe"),
        "Windows release workflow must not publish a bare executable"
    );

    let inno_provenance = fs::read_to_string(
        repository
            .join("installer")
            .join("inno-setup-6.7.1-provenance.json"),
    )
    .expect("Inno Setup provenance must be readable");
    for required in [
        "\"product_version\": \"6.7.1\"",
        "https://community.chocolatey.org/api/v2/package/InnoSetup/6.7.1",
        "\"package_size_bytes\": 10017031",
        "a0dad33db33099d9cd2b89ac2d08b5d70c589b15118ced3b95f469f044f99950",
        "\"embedded_installer_path\": \"tools/innosetup-6.7.1.exe\"",
        "\"embedded_installer_size_bytes\": 10619024",
        "4d11e8050b6185e0d49bd9e8cc661a7a59f44959a621d31d11033124c4e8a7b0",
        "https://files.jrsoftware.org/is/6/innosetup-6.7.1.exe",
        "do not independently prove publisher identity",
    ] {
        assert!(
            inno_provenance.contains(required),
            "Inno Setup provenance must retain {required}"
        );
    }

    let installer = fs::read_to_string(repository.join("installer").join("scribe.iss"))
        .expect("Windows installer script must be readable");
    assert!(
        installer.contains("Source: \"..\\dist\\portable\\*\"")
            && installer.contains("recursesubdirs")
            && installer.contains("createallsubdirs")
            && installer.contains("BeforeInstall: ReleasePayloadHandleForCurrentFile")
            && installer.contains("StableAppIdGuid \"8E0F1935-8E3D-4B1D-9A42-7C7D7C3D5E7A\"")
            && installer.contains("DefaultDirName={code:ResolveDefaultDir}")
            && installer.contains("{localappdata}\\Programs\\Scribe")
            && installer.contains("AppId={code:ResolveAppId}")
            && installer.contains("ReadBoundedToken('SCRIBEVERIFY')")
            && installer.contains("function PrepareToInstall")
            && installer.contains("function ValidateAndBindInstallTree")
            && installer.contains("function QueryExistingAttributes")
            && installer.contains("function BindDirectory")
            && installer.contains("function BindFileForUpdate")
            && installer.contains("function IsInnoUninstallerArtifact")
            && installer.contains("FindFirstFileW")
            && installer.contains("FindNextFileW")
            && installer.contains("FindFirstStreamW")
            && installer.contains("FindNextStreamW")
            && installer.contains("FileShareRead or FileShareWrite")
            && installer.contains("GenericRead or GenericWrite")
            && installer.contains("FileFlagBackupSemantics or FileFlagOpenReparsePoint")
            && installer.contains("DLLGetLastError")
            && installer.contains("ErrorFileNotFound")
            && installer.contains("ErrorPathNotFound")
            && installer.contains("ErrorNoMoreFiles")
            && installer.contains("ErrorHandleEof")
            && installer.contains("FILE_ATTRIBUTE_REPARSE_POINT")
            && installer.contains("case-insensitive path collision")
            && installer.contains("alternate NTFS data stream")
            && installer.contains("SizeOf(FindDataLayoutProbe) <> 592")
            && installer.contains("SizeOf(StreamDataLayoutProbe) <> 600")
            && installer.contains("CreateUninstallRegKey=IsNormalInstall")
            && installer.contains("Check: IsNormalInstall")
            && installer.contains("UsePreviousAppDir=yes")
            && installer.contains("UsePreviousTasks=yes")
            && installer.contains("UsePreviousLanguage=no")
            && installer.contains("Setup did not delete or change any existing content")
            && installer.contains("VerificationInstallDir(Token)")
            && installer.contains("WizardDirValue"),
        "Windows installer must preflight and recursively copy only the validated portable payload"
    );
    assert_eq!(
        installer.matches("GetFileAttributesW(").count(),
        2,
        "every installer attribute query must use the fail-closed error-classifying helper"
    );
    let directory_probe_start = installer
        .find("function BindDirectory")
        .expect("installer directory identity binding must exist");
    let directory_probe_end = installer[directory_probe_start..]
        .find("function BindFileForUpdate")
        .map(|offset| directory_probe_start + offset)
        .expect("installer file identity binding must follow directory binding");
    assert!(
        !installer[directory_probe_start..directory_probe_end].contains("FileShareDelete"),
        "installer must keep enumerated directories from being renamed after preflight"
    );
    let file_probe_end = installer[directory_probe_end..]
        .find("function ValidateNoReparseAncestors")
        .map(|offset| directory_probe_end + offset)
        .expect("installer ancestor validator must follow file identity binding");
    let file_probe_source = &installer[directory_probe_end..file_probe_end];
    let uninstaller_artifact_start = installer
        .find("function IsInnoUninstallerArtifact")
        .expect("installer uninstaller artifact predicate must exist");
    let uninstaller_artifact_end = installer[uninstaller_artifact_start..]
        .find("function QueryExistingAttributes")
        .map(|offset| uninstaller_artifact_start + offset)
        .expect("installer attribute helper must follow uninstaller artifact predicate");
    let uninstaller_artifact_source =
        &installer[uninstaller_artifact_start..uninstaller_artifact_end];
    assert!(
        !installer.contains("FileShareDelete")
            && uninstaller_artifact_source.contains("SameStr(RelativePath, 'unins000.exe')")
            && uninstaller_artifact_source.contains("SameStr(RelativePath, 'unins000.dat')")
            && uninstaller_artifact_source
                .matches("SameStr(RelativePath,")
                .count()
                == 2
            && file_probe_source.contains("ReleaseBeforeInnoReplacement: Boolean")
            && file_probe_source.contains("IdentityAccess := 0")
            && file_probe_source.contains("if not ReleaseBeforeInnoReplacement then")
            && file_probe_source.contains("IdentityAccess := GenericRead")
            && file_probe_source
                .contains("IdentityHandle, Path, ReleaseBeforeInnoReplacement, ErrorText",)
            && file_probe_source
                .contains("Path, IdentityAccess, FileShareRead or FileShareWrite, 0, OpenExisting")
            && file_probe_source
                .contains("Path, GenericRead or GenericWrite, FileShareRead or FileShareWrite"),
        "installer must keep normal file bindings delete-denying and tag only the exact Inno uninstaller pair for release"
    );
    let inspect_start = installer
        .find("function InspectExistingTree")
        .expect("installer tree inspector must exist");
    let inspect_end = installer[inspect_start..]
        .find("function ValidateAndBindInstallTree")
        .map(|offset| inspect_start + offset)
        .expect("stable tree validator must follow tree inspector");
    let inspect_source = &installer[inspect_start..inspect_end];
    let bind_file_call_start = inspect_source
        .find("BindFileForUpdate(")
        .expect("installer tree inspection must bind existing files");
    let bind_file_call_end = inspect_source[bind_file_call_start..]
        .find(") then")
        .map(|offset| bind_file_call_start + offset)
        .expect("installer file binding call must close before its failure branch");
    let bind_file_call_source = &inspect_source[bind_file_call_start..bind_file_call_end];
    let child_path_argument = bind_file_call_source
        .find("ChildPath")
        .expect("installer file binding must use the enumerated child path");
    let uninstaller_argument = bind_file_call_source
        .find("IsInnoUninstallerArtifact(RelativePath)")
        .expect("installer file binding must classify the validated relative path");
    let error_argument = bind_file_call_source
        .find("ErrorText")
        .expect("installer file binding must preserve its fail-closed error result");
    let uninstaller_release_start = installer
        .find("procedure ReleaseInnoUninstallerHandles")
        .expect("installer must release Inno uninstaller handles before replacement");
    let uninstaller_release_end = installer[uninstaller_release_start..]
        .find("function RetainBoundHandle")
        .map(|offset| uninstaller_release_start + offset)
        .expect("installer retained-handle helper must follow uninstaller release helper");
    let uninstaller_release_source = &installer[uninstaller_release_start..uninstaller_release_end];
    let payload_release_start = installer
        .find("procedure ReleasePayloadHandleForCurrentFile")
        .expect("installer must release each payload handle at its BeforeInstall boundary");
    let payload_release_end = installer[payload_release_start..]
        .find("function RetainBoundHandle")
        .map(|offset| payload_release_start + offset)
        .expect("installer retained-handle helper must follow payload release helper");
    let payload_release_source = &installer[payload_release_start..payload_release_end];
    let lifecycle_start = installer
        .find("function PrepareToInstall")
        .expect("installer preflight lifecycle must exist");
    let lifecycle_source = &installer[lifecycle_start..];
    assert!(
        child_path_argument < uninstaller_argument
            && uninstaller_argument < error_argument
            && file_probe_source.contains("RetainBoundHandle(")
            && file_probe_source.contains("IdentityHandle, Path, ReleaseBeforeInnoReplacement")
            && file_probe_source.contains("RejectAlternateStreams(Path, False")
            && file_probe_source.contains("GenericRead or GenericWrite")
            && uninstaller_release_source
                .contains("if BoundHandleReleaseBeforeInnoReplacement[I] then")
            && uninstaller_release_source.contains("CloseHandle(BoundHandles[I])")
            && payload_release_source.contains(
                "CurrentPath := RemoveBackslashUnlessRoot(ExpandFileName(ExpandConstant(CurrentFilename)))"
            )
            && payload_release_source.contains("SameStr(BoundHandlePaths[I], CurrentPath)")
            && payload_release_source
                .contains("if BoundHandleReleaseBeforeInnoReplacement[I] then")
            && payload_release_source.contains("if MatchingHandleIndex <> -1 then")
            && payload_release_source.contains("if FileExists(CurrentPath) then")
            && payload_release_source.contains("if MatchingHandleIndex = -1 then")
            && payload_release_source
                .contains("if not CloseHandle(BoundHandles[MatchingHandleIndex]) then")
            && payload_release_source
                .contains("BoundHandles[MatchingHandleIndex] := InvalidHandleValue")
            && payload_release_source.contains("else if MatchingHandleIndex <> -1 then")
            && lifecycle_source.contains("ReleaseInnoUninstallerHandles();")
            && matches!(
                (
                    lifecycle_source.find("ReleaseInnoUninstallerHandles();"),
                    lifecycle_source.find("WaitAtTestBoundary();")
                ),
                (Some(release), Some(pause)) if release < pause
            ),
        "installer must retain delete-denying payload identity handles while releasing only the validated Inno metadata handles before file replacement"
    );
    let first_probe = inspect_source
        .find("BindDirectory(")
        .expect("installer enumeration must first bind directory identity");
    let enumeration_start = inspect_source
        .find("FindFirstFileW(")
        .expect("installer tree inspection must use native enumeration");
    let enumeration_end = inspect_source
        .rfind("FindNextFileW(")
        .expect("installer tree inspection must continue native enumeration");
    assert!(
        first_probe < enumeration_start && enumeration_start < enumeration_end,
        "installer enumeration must retain a reparse-aware directory handle while reading entries"
    );
    assert!(
        inspect_source.contains("if ErrorCode <> ErrorNoMoreFiles")
            && inspect_source.contains("if ErrorCode = ErrorFileNotFound")
            && inspect_source
                .matches("ErrorCode := DLLGetLastError;")
                .count()
                == 2,
        "installer enumeration must fail closed on start and continuation errors"
    );
    assert!(
        installer.contains("BoundHandles: array[0..2047] of THandle")
            && installer.contains("procedure ReleaseBoundHandles()")
            && lifecycle_source.contains("if CurStep = ssPostInstall then")
            && lifecycle_source.contains("procedure DeinitializeSetup();")
            && lifecycle_source.matches("ReleaseBoundHandles();").count() >= 4,
        "installer must retain identity handles through installation and release them on every exit"
    );
    assert!(
        !installer.contains("[InstallDelete]")
            && !installer.contains("[UninstallDelete]")
            && !installer.contains("[Registry]")
            && !installer.contains("[INI]"),
        "Windows installer must not broadly delete an existing program directory"
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
        "repository-relative Cargo target",
        "expected detected architecture 'whisper'",
        "Existing final bundle",
        "Stale staging refusal",
        "exact executable name",
        "canonical executable parent",
        "PE subsystem mismatch",
        "Windows release workflow",
        "Windows installer",
        "duplicate case-insensitive",
        "Assert-SafePortableZip",
        "Assert-PayloadParity",
        "RUNTIMES/whisper/whisper.dll",
        "nested/model.ONNX",
        "python/runner.py",
        "unreviewed normal import DLL: whisper.dll",
        "unreviewed delay import DLL: onnxruntime.dll",
        "setCaseSensitiveInfo",
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
        "Assert-SafePortableZip",
        "Assert-PayloadParity $bundle $zipRoot \"Portable ZIP\"",
        "Assert-PayloadParity $bundle $installedRoot \"Installed\"",
        "AllowedAdditionalFiles $InnoSetupUninstallerArtifacts",
        "/SCRIBEVERIFY=$verificationToken",
        "Assert-IsolatedInstallerLog",
        "Assert-ExactTreeSnapshot",
        "Invoke-ProtectedRenameRace",
        "Invoke-ReparseRefusalFixture",
        "Assert-Amd64GuiPe",
        "Assert-ReviewedWindowsPe",
        "[switch]$ExerciseStableUpgrade",
        "[string]$EvidenceDirectory",
        "accepted an override outside its derived temporary destination",
        "Stable case-insensitive path collision",
        "Stable unexpected legacy runtime tree",
        "Stable payload file with alternate data stream",
        "Stable payload directory with alternate data stream",
        "Stable root rename race",
        "Stable child-directory rename race",
        "Stable file rename race",
        "Bundle inventory paths differ from the canonical self-contained payload allowlist",
    ] {
        assert!(
            package_verifier.contains(required),
            "release payload verifier must retain {required}"
        );
    }

    let pe_imports = fs::read_to_string(repository.join("scripts").join("windows-pe-imports.ps1"))
        .expect("self-contained Windows PE import parser must be readable");
    for required in [
        "Read-PeImportDirectory",
        r#"[ValidateSet("normal", "delay")]"#,
        "normalDirectoryOffset",
        "delayDirectoryOffset",
        "Convert-PeRvaToFileOffset",
        "Assert-ReviewedWindowsPe",
        "unreviewed normal import DLL",
        "unreviewed delay import DLL",
        "api-ms-win-core-path-l1-1-0.dll",
        "kernel32.dll",
        "user32.dll",
    ] {
        assert!(
            pe_imports.contains(required),
            "self-contained Windows PE import parser must retain {required}"
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
