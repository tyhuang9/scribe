#[allow(dead_code)]
#[path = "../build.rs"]
mod catalog_build;

#[test]
fn approved_voice_models_validate_without_the_release_presence_flag() {
    catalog_build::validate_catalog(include_str!("../runtime-artifacts.default.json")).unwrap();
}

#[test]
fn approved_voice_runtime_and_official_urls_are_exact() {
    let approved = include_str!("../runtime-artifacts.default.json");
    for (original, replacement) in [
        (
            "https://github.com/ggml-org/llama.cpp/releases/download/b9637/llama-b9637-bin-win-cpu-x64.zip",
            "https://release-assets.githubusercontent.com/untrusted.zip",
        ),
        ("upstream_llama_cpp_flat_zip_v1", "scribe_portable_zip_v1"),
        ("43983896", "43983895"),
        (
            "https://huggingface.co/Qwen/Qwen3-0.6B-GGUF/resolve/ef4088322893040952513f532f736ddeab518403/Qwen3-0.6B-Q8_0.gguf",
            "https://huggingface.co/Qwen/Qwen3-0.6B-GGUF/resolve/main/Qwen3-0.6B-Q8_0.gguf",
        ),
    ] {
        let mutated = approved.replacen(original, replacement, 1);
        assert!(
            catalog_build::validate_catalog(&mutated).is_err(),
            "mutation must fail: {original}"
        );
    }
}

#[test]
fn every_official_runtime_and_model_fingerprint_is_build_enforced() {
    let approved: serde_json::Value =
        serde_json::from_str(include_str!("../runtime-artifacts.default.json")).unwrap();
    for (field, replacement) in [
        ("version", serde_json::json!("b9638")),
        ("os", serde_json::json!("linux")),
        ("arch", serde_json::json!("aarch64")),
        ("device", serde_json::json!("gpu")),
        (
            "url",
            serde_json::json!("https://release-assets.githubusercontent.com/untrusted.zip"),
        ),
        ("sha256", serde_json::json!("0".repeat(64))),
        ("size_bytes", serde_json::json!(16_906_750_u64)),
        ("unpacked_size_bytes", serde_json::json!(43_983_895_u64)),
        ("entrypoint", serde_json::json!("bin/llama-cli.exe")),
        (
            "archive_layout",
            serde_json::json!("scribe_portable_zip_v1"),
        ),
        (
            "upstream_repository",
            serde_json::json!("attacker/llama.cpp"),
        ),
        ("upstream_revision", serde_json::json!("0".repeat(40))),
        ("upstream_asset", serde_json::json!("renamed.zip")),
        ("upstream_sha256", serde_json::json!("0".repeat(64))),
        ("upstream_size_bytes", serde_json::json!(16_906_750_u64)),
        ("license", serde_json::json!("Apache-2.0")),
        ("license_sha256", serde_json::json!("0".repeat(64))),
    ] {
        let mut catalog = approved.clone();
        catalog["artifacts"][0][field] = replacement;
        assert!(
            catalog_build::validate_catalog(&catalog.to_string()).is_err(),
            "runtime mutation must fail: {field}"
        );
    }

    for model_index in 0..2 {
        for (field, replacement) in [
            ("upstream_revision", serde_json::json!("0".repeat(40))),
            (
                "url",
                serde_json::json!("https://huggingface.co/Qwen/changed/resolve/main/model.gguf"),
            ),
            ("sha256", serde_json::json!("0".repeat(64))),
            ("size_bytes", serde_json::json!(1_u64)),
            (
                "managed_relative_path",
                serde_json::json!("voice-intent/changed.gguf"),
            ),
        ] {
            let mut catalog = approved.clone();
            catalog["intent_models"][model_index][field] = replacement;
            assert!(
                catalog_build::validate_catalog(&catalog.to_string()).is_err(),
                "model {model_index} mutation must fail: {field}"
            );
        }
    }
}

#[test]
fn arbitrary_voice_model_identity_is_rejected_without_the_release_presence_flag() {
    let approved: serde_json::Value =
        serde_json::from_str(include_str!("../runtime-artifacts.default.json")).unwrap();
    for (field, replacement) in [
        ("model_id", serde_json::json!("qwen3_arbitrary")),
        ("version", serde_json::json!("Qwen3-arbitrary")),
        (
            "upstream_repository",
            serde_json::json!("attacker/arbitrary-GGUF"),
        ),
        (
            "upstream_revision",
            serde_json::json!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        ),
        ("upstream_filename", serde_json::json!("arbitrary.gguf")),
        ("size_bytes", serde_json::json!(4)),
        ("sha256", serde_json::json!("a".repeat(64))),
        (
            "managed_relative_path",
            serde_json::json!("voice-intent/arbitrary.gguf"),
        ),
        ("license", serde_json::json!("MIT")),
        ("license_sha256", serde_json::json!("b".repeat(64))),
    ] {
        let mut catalog = approved.clone();
        catalog["intent_models"] = serde_json::json!([catalog["intent_models"][0].clone()]);
        catalog["intent_models"][0][field] = replacement;

        let error = catalog_build::validate_catalog(&catalog.to_string()).unwrap_err();

        assert!(error.contains("approved Qwen artifact"), "{field}: {error}");
    }
}
