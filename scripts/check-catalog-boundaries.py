#!/usr/bin/env python3
"""Fail-closed Phase 3 source-boundary checks for the normalized catalog."""

from __future__ import annotations

import pathlib
import re
import sys


ROOT = pathlib.Path(__file__).resolve().parents[1]
SRC = ROOT / "src"


def fail(message: str) -> None:
    print(f"catalog boundary FAILED: {message}", file=sys.stderr)
    raise SystemExit(1)


def main() -> None:
    sources = {path: path.read_text(encoding="utf-8") for path in SRC.rglob("*.rs")}
    router = sources[SRC / "runtime_router.rs"]

    for path, source in sources.items():
        if path.name in {"runtime_router.rs", "architecture_guard.rs"}:
            continue
        for concrete in ("RuntimeKind", "TranscribeCppRuntime", "OnnxSpeechRuntime"):
            if concrete in source:
                fail(f"{concrete} escaped the private router into {path.relative_to(ROOT)}")

    if router.count("struct TranscribeCppRuntime") != 1:
        fail("expected exactly one TranscribeCppRuntime declaration")
    if router.count("struct OnnxSpeechRuntime") != 1:
        fail("expected exactly one private router-owned OnnxSpeechRuntime declaration")
    if ".starts_with(\"whisper_cpp_\")" in router:
        fail("runtime routing still depends on a model-ID prefix")

    app = sources[SRC / "app.rs"]
    app_production = app.split("\n#[cfg(test)]\nmod layout_tests", maxsplit=1)[0]
    family_terms = (
        "whisper.cpp",
        "faster-whisper",
        "vosk",
        "sherpa",
        "zipformer",
        "moonshine",
        "parakeet",
    )
    lowered_app = app_production.lower()
    for term in family_terms:
        if term in lowered_app:
            fail(f"application UI contains model-family term {term!r}")
    for concrete_path in (
        "stt::whisper_cpp",
        "stt::faster_whisper",
        "stt::vosk",
        "stt::sherpa_onnx",
    ):
        if concrete_path in app_production:
            fail(f"application UI imports concrete adapter {concrete_path}")
    for semantic_escape in (
        "use crate::stt",
        "runtime_catalog::",
        "provider_for_backend",
        ".backend",
        "RuntimeRouter",
        "transcribe_with_config",
        "whisper_cpp_",
    ):
        if semantic_escape in app_production:
            fail(f"production UI bypasses TranscriptionService via {semantic_escape!r}")

    for path, source in sources.items():
        scanned_source = source
        if path.name == "app.rs":
            scanned_source = app_production
        if path.name == "architecture_guard.rs":
            continue
        if "provider_for_backend" not in scanned_source:
            continue
        relative = path.relative_to(SRC).as_posix()
        if relative not in {"stt/mod.rs", "compatibility_bridge.rs", "runtime_router.rs"}:
            fail(f"legacy provider selection escaped its bridge into src/{relative}")

    concrete_adapter_paths = (
        "stt::whisper_cpp",
        "stt::faster_whisper",
        "stt::vosk",
        "stt::sherpa_onnx",
    )
    for path, source in sources.items():
        relative = path.relative_to(SRC).as_posix()
        scanned_source = app_production if path.name == "app.rs" else source
        if relative.startswith("stt/") or relative in {
            "architecture_guard.rs",
            "runtime_router.rs",
            "compatibility_bridge.rs",
        }:
            continue
        for concrete_path in concrete_adapter_paths:
            if concrete_path in scanned_source:
                fail(
                    f"concrete compatibility adapter {concrete_path} escaped "
                    f"its private bridge into src/{relative}"
                )

    # Family-specific compatibility and packaging knowledge is intentionally
    # confined to private adapters, the service/router, or artifact/catalog
    # validation. This fail-closed allowlist prevents a future application or
    # UI branch from selecting a model family directly.
    family_validation_allowlist = {
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
        "transcription.rs",
    }
    expanded_family_terms = family_terms + (
        "whisper-cpp",
        "qwen",
        "voxtral",
        "nemotron",
        "sensevoice",
        "canary",
    )
    for path, source in sources.items():
        relative = path.relative_to(SRC).as_posix()
        if (
            relative == "architecture_guard.rs"
            or relative.startswith("stt/")
            or relative in family_validation_allowlist
        ):
            continue
        production = source.split("\n#[cfg(test)]", maxsplit=1)[0].lower()
        for term in expanded_family_terms:
            if term in production:
                fail(
                    f"model-family term {term!r} escaped private adapters/catalog "
                    f"validation into src/{relative}"
                )

    downloads = sources[SRC / "managed_downloads.rs"]
    for retired_helper in (
        "download_faster_whisper_model",
        "download_vosk_model",
        "download_sherpa_model",
        "download_runner_model",
    ):
        if retired_helper in downloads:
            fail(f"unreachable legacy download helper {retired_helper!r} was restored")

    for path, source in sources.items():
        if path.name == "architecture_guard.rs":
            continue
        production = source.split("\n#[cfg(test)]", maxsplit=1)[0].lower()
        for web_transport in ("tauri::", "webview", "ipc::", "javascript"):
            if web_transport in production:
                fail(
                    f"forbidden web/UI transport {web_transport!r} exists in "
                    f"{path.relative_to(ROOT)}"
                )

    bundle_source = sources[SRC / "onnx_model_bundles.rs"].split(
        "\n#[cfg(test)]", maxsplit=1
    )[0]
    if bundle_source.count("download_pinned_artifact_for_target(") != 1:
        fail("only the explicit ONNX bundle install path may invoke HTTP")
    for protected_name in (
        "app.rs",
        "config.rs",
        "model_catalog.rs",
        "models.rs",
        "runtime_catalog.rs",
    ):
        protected = sources[SRC / protected_name].split("\n#[cfg(test)]", maxsplit=1)[0]
        for forbidden in (
            "onnx_model_bundles",
            "OnnxBundleReceipt",
            "OnnxBundleManifest",
            "stage_onnx_bundle_install",
        ):
            if forbidden in protected:
                fail(
                    f"private ONNX bundle contract {forbidden!r} escaped into "
                    f"src/{protected_name}"
                )
        if path.relative_to(SRC).as_posix().startswith("ui/"):
            for pcm_shape in ("Vec<f32>", "&[f32]", "PreparedAudio"):
                if pcm_shape in source.split("\n#[cfg(test)]", maxsplit=1)[0]:
                    fail(
                        f"native PCM shape {pcm_shape!r} escaped into "
                        f"{path.relative_to(ROOT)}"
                    )

    text_output = sources[SRC / "text_output.rs"]
    if "tentative" in text_output.split("\n#[cfg(test)]", maxsplit=1)[0].lower():
        fail("tentative transcript text has a path into the output module")

    catalog = sources[SRC / "model_catalog.rs"]
    descriptor = re.search(
        r"pub struct ModelDescriptor \{(?P<body>.*?)\n\}", catalog, re.DOTALL
    )
    if descriptor is None:
        fail("ModelDescriptor declaration is missing")
    descriptor_body = descriptor.group("body").lower()
    for forbidden_field in (
        "backend",
        "runtime",
        "architecture",
        "format",
        "revision",
        "sha256",
        "filename",
    ):
        if re.search(rf"pub\s+{re.escape(forbidden_field)}\s*:", descriptor_body):
            fail(f"ModelDescriptor leaks private field {forbidden_field}")

    print(
        "catalog boundary PASS: private handlers, receipt routing, neutral UI, "
        "native-only PCM, final-only output, and legacy selection confined"
    )


if __name__ == "__main__":
    main()
