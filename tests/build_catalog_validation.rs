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
