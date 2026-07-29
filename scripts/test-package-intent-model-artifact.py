#!/usr/bin/env python3

import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("package-intent-model-artifact.py")
SPEC = importlib.util.spec_from_file_location("package_intent_model_artifact", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(MODULE)


class IntentModelArtifactPackagerTests(unittest.TestCase):
    def ready_catalog(self):
        models = []
        for tier, approved in MODULE.APPROVED_MODELS.items():
            models.append(
                {
                    "runtime_id": MODULE.RUNTIME_ID,
                    "tier": tier,
                    **approved,
                    "url": f"https://downloads.scribe.test.invalid/releases/1/{approved['upstream_filename']}",
                }
            )
        # Replace the deliberately reserved fixture URL before validation.
        for model in models:
            model["url"] = model["url"].replace(
                "downloads.scribe.test.invalid", "downloads.scribe-app.dev"
            )
        return {
            "schema_version": 2,
            "catalog_version": "1.0.0",
            "artifacts": [
                {
                    "runtime_id": MODULE.RUNTIME_ID,
                    "os": "windows",
                    "arch": "x86_64",
                    "device": "cpu",
                }
            ],
            "intent_models": models,
        }

    def test_ready_catalog_requires_runtime_both_exact_models_and_direct_urls(self):
        catalog = self.ready_catalog()
        MODULE.validate_ready(catalog, "windows", "x86_64")

        without_runtime = {**catalog, "artifacts": []}
        with self.assertRaisesRegex(ValueError, "missing voice_intent_llama_cpp"):
            MODULE.validate_ready(without_runtime, "windows", "x86_64")

        without_url = json.loads(json.dumps(catalog))
        del without_url["intent_models"][0]["url"]
        with self.assertRaisesRegex(ValueError, "lacks a direct release URL"):
            MODULE.validate_ready(without_url, "windows", "x86_64")

        wrong_hash = json.loads(json.dumps(catalog))
        wrong_hash["intent_models"][1]["sha256"] = "0" * 64
        with self.assertRaisesRegex(ValueError, "missing or unapproved balanced"):
            MODULE.validate_ready(wrong_hash, "windows", "x86_64")

    def test_release_base_url_rejects_redirect_prone_or_placeholder_shapes(self):
        for url in [
            "http://downloads.scribe-app.dev/releases/1",
            "https://artifacts.example.invalid/releases/1",
            "https://127.0.0.1/releases/1",
            "https://downloads.scribe-app.dev/releases/1?mutable=yes",
        ]:
            with self.assertRaises(ValueError, msg=url):
                MODULE.validate_base_url(url)

    def test_wrong_model_bytes_fail_before_catalog_or_output_mutation(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            model = root / "not-the-model.gguf"
            model.write_bytes(b"not approved")
            catalog = root / "catalog.json"
            catalog.write_text(
                json.dumps(
                    {
                        "schema_version": 1,
                        "catalog_version": "1.0.0",
                        "artifacts": [],
                    }
                ),
                encoding="utf-8",
            )
            output = root / "output"
            result = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "--tier",
                    "compact",
                    "--model-file",
                    str(model),
                    "--release-base-url",
                    "https://downloads.scribe-app.dev/releases/1",
                    "--output-dir",
                    str(output),
                    "--catalog",
                    str(catalog),
                    "--catalog-version",
                    "1.0.0",
                ],
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("does not match the approved upstream bytes", result.stderr)
            self.assertFalse(output.exists())
            self.assertEqual(json.loads(catalog.read_text(encoding="utf-8"))["schema_version"], 1)


if __name__ == "__main__":
    unittest.main()
