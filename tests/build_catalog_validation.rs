#[allow(dead_code)]
#[path = "../build.rs"]
mod catalog_build;

#[test]
fn approved_voice_models_validate_without_the_release_presence_flag() {
    catalog_build::validate_catalog(include_str!("../runtime-artifacts.default.json")).unwrap();
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
