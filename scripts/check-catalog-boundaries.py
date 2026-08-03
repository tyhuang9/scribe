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
        if path.name == "runtime_router.rs":
            continue
        for concrete in ("RuntimeKind", "TranscribeCppRuntime", "OnnxSpeechRuntime"):
            if concrete in source:
                fail(f"{concrete} escaped the private router into {path.relative_to(ROOT)}")

    if router.count("struct TranscribeCppRuntime") != 1:
        fail("expected exactly one TranscribeCppRuntime declaration")
    if "struct OnnxSpeechRuntime" in router:
        fail("OnnxSpeechRuntime exists even though the Zipformer evidence gate is NO-GO")
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
        "backend_label",
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
        "catalog boundary PASS: one handler, manifest routing, neutral UI, "
        "legacy provider selection confined to its private bridge"
    )


if __name__ == "__main__":
    main()
