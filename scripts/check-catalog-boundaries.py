#!/usr/bin/env python3
"""Fail-closed Phase 3 source-boundary checks for the normalized catalog."""

from __future__ import annotations

import pathlib
import re
import sys


ROOT = pathlib.Path(__file__).resolve().parents[1]
SRC = ROOT / "src"
NATIVE_PCM_SHAPES = ("Vec<f32>", "&[f32]", "PreparedAudio")


def fail(message: str) -> None:
    print(f"catalog boundary FAILED: {message}", file=sys.stderr)
    raise SystemExit(1)


def rust_item(source: str, signature: str) -> str:
    """Return one brace-delimited Rust item, ignoring braces inside strings."""
    start = source.find(signature)
    if start < 0:
        fail(f"expected Rust item {signature!r}")
    brace = source.find("{", start)
    if brace < 0:
        fail(f"expected body for Rust item {signature!r}")
    depth = 0
    in_string = False
    escaped = False
    for index in range(brace, len(source)):
        character = source[index]
        if in_string:
            if escaped:
                escaped = False
            elif character == "\\":
                escaped = True
            elif character == '"':
                in_string = False
            continue
        if character == '"':
            in_string = True
        elif character == "{":
            depth += 1
        elif character == "}":
            depth -= 1
            if depth == 0:
                return source[start : index + 1]
    fail(f"unterminated Rust item {signature!r}")


def production_source(source: str) -> str:
    return source.split("\n#[cfg(test)]", maxsplit=1)[0]


def without_trailing_test_module(source: str) -> str:
    matches = list(re.finditer(r"(?m)^#\[cfg\(test\)\]\s*\nmod\s+tests\s*\{", source))
    return source[: matches[-1].start()] if matches else source


def native_pcm_ui_violations(sources: dict[str, str]) -> list[tuple[str, str]]:
    violations = []
    for relative, source in sources.items():
        if not relative.startswith("ui/"):
            continue
        production = production_source(source)
        for pcm_shape in NATIVE_PCM_SHAPES:
            if pcm_shape in production:
                violations.append((relative, pcm_shape))
    return violations


def main() -> None:
    sources = {path: path.read_text(encoding="utf-8") for path in SRC.rglob("*.rs")}
    router = without_trailing_test_module(sources[SRC / "runtime_router.rs"])

    pcm_self_test = native_pcm_ui_violations(
        {
            "ui/first.rs": "fn neutral() {}",
            "ui/second.rs": "fn forbidden(samples: Vec<f32>) {}",
            "service.rs": "fn allowed(samples: &[f32]) {}",
        }
    )
    if pcm_self_test != [("ui/second.rs", "Vec<f32>")]:
        fail("native PCM UI scan self-test did not inspect every UI source")

    for path, source in sources.items():
        if path.name in {"runtime_router.rs", "architecture_guard.rs"}:
            continue
        for concrete in ("RuntimeKind", "TranscribeCppRuntime"):
            if concrete in source:
                fail(f"{concrete} escaped the private router into {path.relative_to(ROOT)}")

    if router.count("struct TranscribeCppRuntime") != 1:
        fail("expected exactly one TranscribeCppRuntime declaration")
    for obsolete in (
        "OnnxSpeechRuntime",
        "OnnxSupervisorControl",
        "OnnxSupervisorFactory",
        "production_onnx_supervisor",
        "HeavyRuntimeOwner::OnnxSpeech",
    ):
        if obsolete in router:
            fail(f"obsolete nested ONNX router machinery {obsolete!r} was restored")
    if "OnnxWorkerSupervisor" in router:
        fail("RuntimeRouter may not own or spawn an ONNX worker")
    if ".starts_with(\"whisper_cpp_\")" in router:
        fail("runtime routing still depends on a model-ID prefix")

    app = sources[SRC / "app.rs"]
    # app.rs contains cfg(test)-gated helpers near the top and production UI
    # far below them. Stop only at the actual trailing layout-test module so
    # later production paths are never silently omitted.
    layout_tests = re.search(
        r"(?m)^#\[cfg\(test\)\]\s*\nmod\s+layout_tests\s*\{", app
    )
    if layout_tests is None:
        fail("app.rs trailing layout test module is missing")
    app_production = app[: layout_tests.start()]
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
        "silero_vad_native.rs",
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
        production = production_source(source).lower()
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
        production = production_source(source).lower()
        for web_transport in ("tauri::", "webview", "ipc::", "javascript"):
            if web_transport in production:
                fail(
                    f"forbidden web/UI transport {web_transport!r} exists in "
                    f"{path.relative_to(ROOT)}"
                )

    bundle_source = production_source(sources[SRC / "onnx_model_bundles.rs"])
    if bundle_source.count("download_pinned_artifact_for_target(") != 1:
        fail("only the explicit ONNX bundle install path may invoke HTTP")
    for protected_name in (
        "app.rs",
        "config.rs",
        "model_catalog.rs",
        "models.rs",
        "runtime_catalog.rs",
    ):
        protected = production_source(sources[SRC / protected_name])
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

    relative_sources = {
        path.relative_to(SRC).as_posix(): source for path, source in sources.items()
    }
    for relative, pcm_shape in native_pcm_ui_violations(relative_sources):
        fail(f"native PCM shape {pcm_shape!r} escaped into src/{relative}")

    text_output = sources[SRC / "text_output.rs"]
    if "tentative" in production_source(text_output).lower():
        fail("tentative transcript text has a path into the output module")

    worker = sources[SRC / "onnx_worker.rs"]
    for required in (
        'INFERENCE_WORKER_FLAG: &str = "--scribe-inference-worker"',
        'VAD_WORKER_FLAG: &str = "--scribe-vad-worker"',
        "fn load_worker_runtime",
        "fn execute_worker_batch",
        "WireRuntimeArtifact::OnnxBundle",
        "OfflineRecognizer::create(",
        "OnlineRecognizer::create(",
    ):
        if required not in worker:
            fail(f"direct child-owned ONNX topology lost {required!r}")
    worker_role = rust_item(worker, "fn worker_role_from_args")
    if "LEGACY_ONNX_WORKER_FLAG" in worker or "--onnx-worker" in worker_role:
        fail("legacy --onnx-worker role was restored")
    for signature in ("fn load_worker_runtime", "fn execute_worker_batch"):
        item = rust_item(worker, signature)
        for nested_spawn in ("Command::new", "OnnxWorkerSupervisor", "OsWorkerLauncher"):
            if nested_spawn in item:
                fail(
                    f"normalized ONNX path {signature} can spawn nested worker via "
                    f"{nested_spawn!r}"
                )

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
