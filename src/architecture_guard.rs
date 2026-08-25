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
    assert_eq!(
        router.matches("struct OnnxSpeechRuntime").count(),
        1,
        "the private ONNX handler must have exactly one router-owned declaration"
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
        "OnnxBundle",
        "OnnxSpeech",
        "native-onnx",
    ];

    for (path, source) in &sources {
        if !protected.iter().any(|protected| path == protected) && !path.starts_with("ui") {
            continue;
        }
        let production = production_prefix(source);
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
        "onnx_model_bundles.rs",
        "onnx_worker.rs",
        "runtime_catalog.rs",
        "runtime_router.rs",
        "settings/schema.rs",
        "silero_vad_native.rs",
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
fn onnx_bundle_http_and_typed_receipts_stay_below_the_service_boundary() {
    let sources = rust_sources();
    let bundles = sources
        .iter()
        .find(|(path, _)| path == Path::new("onnx_model_bundles.rs"))
        .map(|(_, source)| production_prefix(source))
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
        Path::new("app.rs"),
        Path::new("config.rs"),
        Path::new("model_catalog.rs"),
        Path::new("models.rs"),
        Path::new("runtime_catalog.rs"),
    ] {
        let production = sources
            .iter()
            .find(|(path, _)| path == protected)
            .map(|(_, source)| production_prefix(source))
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
        let production = production_prefix(&source).to_ascii_lowercase();
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
