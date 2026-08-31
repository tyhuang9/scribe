#!/usr/bin/env python3
"""Fixture-only tests for the Linux GPU qualification evidence gate."""

from __future__ import annotations

import copy
import hashlib
import importlib.util
import json
import pathlib
import subprocess
import sys
import tempfile
import unittest
from typing import Any


SCRIPT_ROOT = pathlib.Path(__file__).resolve().parent
REPOSITORY_ROOT = SCRIPT_ROOT.parent
TOOL_PATH = SCRIPT_ROOT / "qualify-linux-gpu-evidence.py"
AUTO_MANIFEST = REPOSITORY_ROOT / "runtime-manifests/gpu-auto-qualification-linux-x86_64.json"
EXPECTED_AUTO_BYTES = b'{"schema_version":1,"mode":"default_deny","target_os":"linux","target_arch":"x86_64","entries":[]}\n'
PRODUCTION_AUTHORITY = REPOSITORY_ROOT / "runtime-manifests/linux-gpu-qualification-production-authority.json"
EXPECTED_AUTHORITY_BYTES = b'{"approved_plan_sha256":[],"kind":"linux_gpu_qualification_production_authority","schema_version":1}\n'

spec = importlib.util.spec_from_file_location("linux_gpu_qualification", TOOL_PATH)
assert spec is not None and spec.loader is not None
qualification = importlib.util.module_from_spec(spec)
spec.loader.exec_module(qualification)


def digest(label: str) -> str:
    return hashlib.sha256(label.encode("ascii")).hexdigest()


def envelope_bytes(kind: str, record: dict[str, Any]) -> bytes:
    return qualification.canonical_bytes({"kind": kind, "record": record, "schema_version": 1})


def metric_run(
    mode: str,
    target: str,
    sequence: int,
    gpu_warm_ms: int,
    identity: dict[str, Any],
) -> dict[str, Any]:
    if mode == "cold":
        end_to_end_ms = 200 + sequence if target == "cpu" else 180 + sequence
    else:
        end_to_end_ms = 100 if target == "cpu" else gpu_warm_ms
    acquisition = identity["acquisition"]
    worker = identity["cpu_baseline"] if target == "cpu" else identity["gpu_worker"]
    record = {
        "acquisition_batch_id": acquisition["batch_id"],
        "backend_ms": end_to_end_ms - 10,
        "end_to_end_ms": end_to_end_ms,
        "execution": {
            "backend": "cpu" if target == "cpu" else identity["backend"],
            "device_memory_kind": "none" if target == "cpu" else identity["device"]["memory_model"],
            "hello_sha256": digest(f"hello-{mode}-{target}-{sequence}"),
            "protocol_version": worker["protocol_version"],
            "provider_id": worker["provider_id"],
            "runtime_abi": worker["runtime_abi"],
            "stable_device_id": "cpu:host" if target == "cpu" else identity["device"]["stable_device_id"],
            "worker_build_id": worker["worker_build_id"],
            "worker_generation": (
                f"{acquisition['batch_id']}:{mode}:{target}:{sequence:02}"
                if mode == "cold"
                else f"{acquisition['batch_id']}:warm:{target}"
            ),
            "worker_sha256": worker["worker_sha256"],
        },
        "failure_category": "none",
        "machine_id_sha256": acquisition["machine_id_sha256"],
        "outcome": "success",
        "pair_id": f"{acquisition['batch_id']}:{mode}:{sequence:02}",
        "pair_order": "cpu_then_gpu" if sequence % 2 else "gpu_then_cpu",
        "peak_process_memory_bytes": 600_000_000 + sequence * 1024,
        "peak_shared_device_memory_bytes": 0,
        "peak_vram_bytes": 0 if target == "cpu" else 800_000_000 + sequence * 2048,
        "priming_runs": 0 if mode == "cold" else 1,
        "reset_state": "fresh_process_fresh_model" if mode == "cold" else "same_process_primed_model",
        "sequence": sequence,
        "session_id": (
            f"{acquisition['batch_id']}:{mode}:{sequence:02}:session"
            if mode == "cold"
            else f"{acquisition['batch_id']}:warm:session"
        ),
        "transcript_sha256": digest("expected-transcript"),
    }
    path = f"{identity['lane_id']}/runs/{mode}/{target}/{sequence:02}.evidence"
    return {
        "artifact_path": path,
        "artifact_sha256": hashlib.sha256(
            envelope_bytes("linux_gpu_qualification_run_artifact", record)
        ).hexdigest(),
        **record,
    }


def fixture_lane(gpu_warm_ms: int = 110) -> dict[str, Any]:
    driver = "linux:nvidia:570.86.15"
    stable_id = "native:pci:0000:01:00.0"
    identity = {
        "backend": "cuda",
        "acquisition": {
            "batch_id": "fixture-batch-001",
            "controls": {
                "background_load_policy": "isolated",
                "cpu_governor": "performance",
                "gpu_power_profile": "fixed_maximum_performance",
                "power_source": "ac",
                "thermal_policy": "no_throttling_observed",
            },
            "host": {
                "cpu_arch": "x86_64",
                "cpu_model_sha256": digest("cpu-model"),
                "logical_cpus": 16,
                "numa_nodes": 1,
                "physical_cores": 8,
                "total_memory_bytes": 32_000_000_000,
            },
            "machine_id_sha256": digest("machine"),
            "ordering": {
                "scheme": "paired_alternating_cpu_first_v1",
                "warm_priming_runs": 1,
            },
            "protocol": {
                "harness_sha256": digest("qualification-harness"),
                "protocol_id": "scribe-linux-gpu-qualification",
                "protocol_version": 1,
            },
            "threading": {
                "cpu_affinity_sha256": digest("cpu-affinity"),
                "cpu_worker_threads": 8,
                "gpu_affinity_sha256": digest("gpu-affinity"),
                "gpu_worker_threads": 4,
            },
        },
        "cpu_baseline": {
            "backend": "cpu",
            "protocol_version": 5,
            "provider_id": "scribe-inference-worker-cpu",
            "runtime_abi": 1,
            "worker_build_id": "scribe-inference-worker@0.1.0#fixture-cpu",
            "worker_sha256": digest("cpu-worker"),
        },
        "device": {
            "device_class": "discrete_gpu",
            "memory_model": "dedicated_vram",
            "qualified_minimum_total_memory_bytes": 8_000_000_000,
            "stable_device_id": stable_id,
            "total_memory_bytes": 12_884_901_888,
            "vendor": "nvidia",
        },
        "driver": {"kind": "exact", "value": driver},
        "glibc_version": "2.35",
        "gpu_worker": {
            "backend": "cuda",
            "protocol_version": 5,
            "provider_id": "transcribe-cpp-ggml-cuda",
            "runtime_abi": 1,
            "worker_build_id": "scribe-inference-worker@0.1.0#fixture-cuda",
            "worker_sha256": digest("gpu-worker"),
        },
        "kernel_version": "5.15.0-213-generic",
        "lane_id": "fixture-ubuntu-22.04-nvidia-cuda",
        "model": {"model_digest": digest("model"), "model_id": "whisper-base-en-q8_0"},
        "pack": {
            "pack_digest": digest("pack"),
            "pack_id": "scribe-cuda-linux-x64",
            "pack_version": "0.1.0-fixture",
            "runtime_abi": 1,
            "security_epoch": 1,
        },
        "provider_id": "transcribe-cpp-ggml-cuda",
        "target_arch": "x86_64",
        "ubuntu_version": "22.04",
        "workload": {
            "audio_sha256": digest("audio"),
            "expected_transcript_sha256": digest("expected-transcript"),
            "workload_id": "fixture-english-30s",
        },
    }
    run_sets = {
        mode: {
            target: [
                metric_run(mode, target, sequence, gpu_warm_ms, identity)
                for sequence in range(1, (5 if mode == "cold" else 20) + 1)
            ]
            for target in ("cpu", "gpu")
        }
        for mode in ("cold", "warm")
    }
    lifecycle_records = [
        {
            "active_request_migrated": False,
            "artifact_path": "events/device-loss.evidence",
            "driver_after": driver,
            "driver_before": driver,
            "event": "device_loss",
            "observed_failure_category": "device_loss",
            "partial_output_replayed": False,
            "recovered_next_request": True,
            "result": "pass",
            "selection_reevaluated": True,
            "stable_device_id_after": stable_id,
        },
        {
            "active_request_migrated": False,
            "artifact_path": "events/driver-change.evidence",
            "driver_after": driver,
            "driver_before": "linux:nvidia:570.26.00",
            "event": "driver_change",
            "observed_failure_category": "none",
            "partial_output_replayed": False,
            "recovered_next_request": True,
            "result": "pass",
            "selection_reevaluated": True,
            "stable_device_id_after": stable_id,
        },
        {
            "active_request_migrated": False,
            "artifact_path": "events/suspend-resume.evidence",
            "driver_after": driver,
            "driver_before": driver,
            "event": "suspend_resume",
            "observed_failure_category": "none",
            "partial_output_replayed": False,
            "recovered_next_request": True,
            "result": "pass",
            "selection_reevaluated": True,
            "stable_device_id_after": stable_id,
        },
    ]
    lifecycle = []
    for value in lifecycle_records:
        value["artifact_path"] = f"{identity['lane_id']}/{value['artifact_path']}"
        record = {key: item for key, item in value.items() if key != "artifact_path"}
        lifecycle.append(
            {
                "artifact_path": value["artifact_path"],
                "artifact_sha256": hashlib.sha256(
                    envelope_bytes("linux_gpu_qualification_lifecycle_artifact", record)
                ).hexdigest(),
                **record,
            }
        )
    acquisition_path = f"{identity['lane_id']}/acquisition.evidence"
    acquisition_sha256 = hashlib.sha256(
        envelope_bytes("linux_gpu_qualification_acquisition_artifact", identity["acquisition"])
    ).hexdigest()
    return {
        "acquisition_artifact_path": acquisition_path,
        "acquisition_artifact_sha256": acquisition_sha256,
        "identity": identity,
        "lifecycle": lifecycle,
        "run_sets": run_sets,
    }


def refresh_lane_artifact_digests(lane: dict[str, Any]) -> None:
    lane["acquisition_artifact_sha256"] = hashlib.sha256(
        envelope_bytes("linux_gpu_qualification_acquisition_artifact", lane["identity"]["acquisition"])
    ).hexdigest()
    for target_sets in lane["run_sets"].values():
        for runs in target_sets.values():
            for run in runs:
                record = {
                    key: value for key, value in run.items() if key not in {"artifact_path", "artifact_sha256"}
                }
                run["artifact_sha256"] = hashlib.sha256(
                    envelope_bytes("linux_gpu_qualification_run_artifact", record)
                ).hexdigest()
    for event in lane["lifecycle"]:
        record = {
            key: value for key, value in event.items() if key not in {"artifact_path", "artifact_sha256"}
        }
        event["artifact_sha256"] = hashlib.sha256(
            envelope_bytes("linux_gpu_qualification_lifecycle_artifact", record)
        ).hexdigest()


def fixture_documents_for_lanes(
    lanes: list[dict[str, Any]],
) -> tuple[dict[str, Any], dict[str, Any]]:
    lanes.sort(key=lambda value: value["identity"]["lane_id"])
    for value in lanes:
        refresh_lane_artifact_digests(value)
    bindings = {
        field: qualification.file_sha256(REPOSITORY_ROOT / relative)
        for field, relative in qualification.CONTRACT_PATHS.items()
    }
    plan = {
        "cold_runs": 5,
        "contract_bindings": bindings,
        "fixture_only": True,
        "kind": "linux_gpu_release_qualification_plan",
        "maximum_gpu_p95_cpu_percent": 110,
        "required_events": ["device_loss", "driver_change", "suspend_resume"],
        "required_lanes": [
            {"evidence_sha256": qualification.canonical_digest(value), "identity": value["identity"]}
            for value in lanes
        ],
        "schema_version": 1,
        "target_arch": "x86_64",
        "target_os": "linux",
        "warm_runs": 20,
    }
    evidence = {
        "fixture_only": True,
        "kind": "linux_gpu_release_qualification_evidence",
        "lanes": lanes,
        "plan_sha256": qualification.canonical_digest(plan),
        "schema_version": 1,
    }
    return plan, evidence


def fixture_documents(lane: dict[str, Any] | None = None) -> tuple[dict[str, Any], dict[str, Any]]:
    return fixture_documents_for_lanes([] if lane is None else [lane])


def bind_documents(plan: dict[str, Any], evidence: dict[str, Any]) -> None:
    for lane in evidence["lanes"]:
        refresh_lane_artifact_digests(lane)
    plan["required_lanes"] = [
        {"evidence_sha256": qualification.canonical_digest(lane), "identity": lane["identity"]}
        for lane in evidence["lanes"]
    ]
    evidence["plan_sha256"] = qualification.canonical_digest(plan)


class QualificationFixtureTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="scribe-linux-gpu-qualification-")
        self.root = pathlib.Path(self.temporary.name)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def write(self, name: str, document: dict[str, Any], *, canonical: bool = True) -> pathlib.Path:
        path = self.root / name
        payload = (
            qualification.canonical_bytes(document)
            if canonical
            else json.dumps(document, indent=2).encode("utf-8")
        )
        path.write_bytes(payload)
        return path

    def run_tool(
        self,
        plan: dict[str, Any],
        evidence: dict[str, Any],
        *,
        allow_fixture: bool = True,
        require_eligible: bool = False,
        canonical: bool = True,
        prepare_artifacts: bool = True,
    ) -> subprocess.CompletedProcess[str]:
        plan_path = self.write("plan.json", plan, canonical=canonical)
        evidence_path = self.write("evidence.json", evidence, canonical=canonical)
        artifact_root = self.root / "artifacts"
        if prepare_artifacts:
            for lane in evidence["lanes"]:
                acquisition_path = artifact_root / pathlib.PurePosixPath(
                    lane["acquisition_artifact_path"]
                )
                acquisition_path.parent.mkdir(parents=True, exist_ok=True)
                acquisition_path.write_bytes(
                    envelope_bytes(
                        "linux_gpu_qualification_acquisition_artifact",
                        lane["identity"]["acquisition"],
                    )
                )
                for mode, target_sets in lane["run_sets"].items():
                    for target, runs in target_sets.items():
                        for run in runs:
                            path = artifact_root / pathlib.PurePosixPath(run["artifact_path"])
                            path.parent.mkdir(parents=True, exist_ok=True)
                            record = {
                                key: value
                                for key, value in run.items()
                                if key not in {"artifact_path", "artifact_sha256"}
                            }
                            path.write_bytes(
                                envelope_bytes("linux_gpu_qualification_run_artifact", record)
                            )
                for event in lane["lifecycle"]:
                    path = artifact_root / pathlib.PurePosixPath(event["artifact_path"])
                    path.parent.mkdir(parents=True, exist_ok=True)
                    record = {
                        key: value
                        for key, value in event.items()
                        if key not in {"artifact_path", "artifact_sha256"}
                    }
                    path.write_bytes(
                        envelope_bytes("linux_gpu_qualification_lifecycle_artifact", record)
                    )
        command = [
            sys.executable,
            str(TOOL_PATH),
            "--plan",
            str(plan_path),
            "--evidence",
            str(evidence_path),
            "--artifact-root",
            str(artifact_root),
        ]
        if allow_fixture:
            command.append("--allow-fixture")
        if require_eligible:
            command.append("--require-eligible")
        return subprocess.run(command, text=True, capture_output=True, check=False)

    def parse_success(self, result: subprocess.CompletedProcess[str]) -> dict[str, Any]:
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)

    def test_checked_in_state_is_canonical_default_deny(self) -> None:
        self.assertEqual(AUTO_MANIFEST.read_bytes(), EXPECTED_AUTO_BYTES)
        self.assertEqual(PRODUCTION_AUTHORITY.read_bytes(), EXPECTED_AUTHORITY_BYTES)
        authority = json.loads(PRODUCTION_AUTHORITY.read_bytes())
        self.assertEqual(authority["approved_plan_sha256"], [])

    def test_fixture_cannot_be_promoted_by_relabeling_and_rehashing(self) -> None:
        plan, evidence = fixture_documents(fixture_lane())
        plan["fixture_only"] = False
        evidence["fixture_only"] = False
        evidence["plan_sha256"] = qualification.canonical_digest(plan)
        result = self.run_tool(plan, evidence, allow_fixture=False)
        self.assertEqual(result.returncode, 1)
        self.assertIn("protected production authority", result.stderr)

    def test_complete_boundary_fixture_is_deterministic_but_never_eligible(self) -> None:
        plan, evidence = fixture_documents(fixture_lane(110))
        first = self.run_tool(plan, evidence)
        first_decision = self.parse_success(first)
        second = self.run_tool(plan, evidence)
        second_decision = self.parse_success(second)
        self.assertEqual(first.stdout, second.stdout)
        self.assertEqual(first_decision, second_decision)
        self.assertTrue(first_decision["evidence_complete"])
        self.assertTrue(first_decision["qualification_passed"])
        self.assertFalse(first_decision["auto_eligible"])
        self.assertEqual(first_decision["decision_reason"], "fixture_only_never_auto_eligible")
        lane = first_decision["lanes"][0]
        self.assertTrue(lane["checks"]["performance_passed"])
        self.assertEqual(lane["metrics"]["warm"]["cpu"]["end_to_end_ms"], {"p50": 100, "p95": 100})
        self.assertEqual(lane["metrics"]["warm"]["gpu"]["end_to_end_ms"], {"p50": 110, "p95": 110})
        self.assertEqual(lane["metrics"]["cold"]["cpu"]["end_to_end_ms"], {"p50": 203, "p95": 205})
        self.assertEqual(lane["metrics"]["cold"]["gpu"]["end_to_end_ms"], {"p50": 183, "p95": 185})
        self.assertEqual(
            lane["metrics"]["warm"]["cpu"]["peak_process_memory_bytes"],
            {"p50": 600_010_240, "p95": 600_019_456},
        )
        self.assertEqual(lane["metrics"]["warm"]["cpu"]["peak_vram_bytes"], {"p50": 0, "p95": 0})
        self.assertGreater(lane["metrics"]["warm"]["gpu"]["peak_vram_bytes"]["p95"], 0)

    def test_fixture_requires_explicit_test_mode_and_cannot_satisfy_release_gate(self) -> None:
        plan, evidence = fixture_documents(fixture_lane())
        rejected = self.run_tool(plan, evidence, allow_fixture=False)
        self.assertEqual(rejected.returncode, 1)
        self.assertIn("requires --allow-fixture", rejected.stderr)
        gated = self.run_tool(plan, evidence, require_eligible=True)
        self.assertEqual(gated.returncode, 2)
        self.assertFalse(json.loads(gated.stdout)["auto_eligible"])

    def test_gpu_p95_above_boundary_fails_closed(self) -> None:
        plan, evidence = fixture_documents(fixture_lane(111))
        decision = self.parse_success(self.run_tool(plan, evidence))
        lane = decision["lanes"][0]
        self.assertFalse(decision["qualification_passed"])
        self.assertFalse(decision["auto_eligible"])
        self.assertEqual(lane["reasons"], ["gpu_p95_exceeds_cpu_boundary"])
        cold_lane = fixture_lane()
        for run in cold_lane["run_sets"]["cold"]["gpu"]:
            run["end_to_end_ms"] = 500
            run["backend_ms"] = 490
        cold_plan, cold_evidence = fixture_documents(cold_lane)
        cold_decision = self.parse_success(self.run_tool(cold_plan, cold_evidence))
        self.assertFalse(cold_decision["qualification_passed"])
        self.assertEqual(cold_decision["lanes"][0]["reasons"], ["gpu_p95_exceeds_cpu_boundary"])

    def test_vulkan_vendor_and_second_ubuntu_lane_identities_are_supported(self) -> None:
        drivers = {
            "amd": ("linux:amdgpu:6.8.0", "linux:amdgpu:6.7.0"),
            "intel": ("linux:i915:6.8.0", "linux:i915:6.7.0"),
            "nvidia": ("linux:nvidia:570.86.15:vulkan", "linux:nvidia:570.26.00:vulkan"),
        }
        for vendor, (current_driver, prior_driver) in drivers.items():
            with self.subTest(vendor=vendor):
                lane = fixture_lane()
                lane["identity"]["backend"] = "vulkan"
                lane["identity"]["provider_id"] = "transcribe-cpp-ggml-vulkan"
                lane["identity"]["gpu_worker"]["backend"] = "vulkan"
                lane["identity"]["gpu_worker"]["provider_id"] = "transcribe-cpp-ggml-vulkan"
                lane["identity"]["gpu_worker"]["worker_build_id"] = (
                    "scribe-inference-worker@0.1.0#fixture-vulkan"
                )
                lane["identity"]["ubuntu_version"] = "24.04"
                lane["identity"]["kernel_version"] = "6.8.0-85-generic"
                lane["identity"]["glibc_version"] = "2.39"
                lane["identity"]["lane_id"] = f"fixture-ubuntu-24.04-{vendor}-vulkan"
                lane["identity"]["device"]["vendor"] = vendor
                lane["identity"]["driver"]["value"] = current_driver
                lane["identity"]["pack"]["pack_id"] = "scribe-vulkan-linux-x64"
                for mode in lane["run_sets"].values():
                    for run in mode["gpu"]:
                        run["execution"]["backend"] = "vulkan"
                        run["execution"]["provider_id"] = "transcribe-cpp-ggml-vulkan"
                        run["execution"]["worker_build_id"] = (
                            "scribe-inference-worker@0.1.0#fixture-vulkan"
                        )
                for event in lane["lifecycle"]:
                    event["driver_after"] = current_driver
                    event["driver_before"] = prior_driver if event["event"] == "driver_change" else current_driver
                plan, evidence = fixture_documents(lane)
                decision = self.parse_success(self.run_tool(plan, evidence))
                self.assertTrue(decision["qualification_passed"])
                self.assertFalse(decision["auto_eligible"])

    def test_integrated_and_unified_memory_are_explicitly_represented(self) -> None:
        for device_class in ("integrated_gpu", "unified_gpu"):
            with self.subTest(device_class=device_class):
                lane = fixture_lane()
                lane["identity"]["device"]["device_class"] = device_class
                lane["identity"]["device"]["memory_model"] = "shared_host_memory"
                for mode in lane["run_sets"].values():
                    for run in mode["gpu"]:
                        run["execution"]["device_memory_kind"] = "shared_host_memory"
                        run["peak_shared_device_memory_bytes"] = run["peak_vram_bytes"]
                        run["peak_vram_bytes"] = 0
                plan, evidence = fixture_documents(lane)
                decision = self.parse_success(self.run_tool(plan, evidence))
                self.assertTrue(decision["qualification_passed"])

    def test_fake_gpu_and_mislabeled_execution_attestations_are_rejected(self) -> None:
        cases: list[tuple[str, dict[str, Any]]] = []
        zero_vram = fixture_lane()
        zero_vram["run_sets"]["warm"]["gpu"][0]["peak_vram_bytes"] = 0
        cases.append(("zero-vram", zero_vram))
        minimal_gpu = fixture_lane()
        minimal_gpu["identity"]["device"]["total_memory_bytes"] = 1
        minimal_gpu["identity"]["device"]["qualified_minimum_total_memory_bytes"] = 1
        cases.append(("minimal-gpu", minimal_gpu))
        mislabeled = fixture_lane()
        execution = mislabeled["run_sets"]["warm"]["gpu"][0]["execution"]
        execution["backend"] = "cpu"
        execution["provider_id"] = mislabeled["identity"]["cpu_baseline"]["provider_id"]
        execution["worker_build_id"] = mislabeled["identity"]["cpu_baseline"]["worker_build_id"]
        execution["worker_sha256"] = mislabeled["identity"]["cpu_baseline"]["worker_sha256"]
        execution["stable_device_id"] = "cpu:host"
        execution["device_memory_kind"] = "none"
        cases.append(("mislabeled-cpu", mislabeled))
        for label, lane in cases:
            with self.subTest(label=label):
                plan, evidence = fixture_documents(lane)
                result = self.run_tool(plan, evidence)
                self.assertEqual(result.returncode, 1, result.stdout)

    def test_cross_machine_batch_and_protocol_violations_are_rejected(self) -> None:
        cases: list[tuple[str, dict[str, Any]]] = []
        wrong_machine = fixture_lane()
        wrong_machine["run_sets"]["cold"]["gpu"][0]["machine_id_sha256"] = digest("other-machine")
        cases.append(("cross-machine", wrong_machine))
        wrong_batch = fixture_lane()
        wrong_batch["run_sets"]["warm"]["cpu"][0]["acquisition_batch_id"] = "other-batch"
        cases.append(("cross-batch", wrong_batch))
        wrong_priming = fixture_lane()
        wrong_priming["run_sets"]["warm"]["gpu"][0]["priming_runs"] = 0
        cases.append(("warm-priming", wrong_priming))
        wrong_generation = fixture_lane()
        wrong_generation["run_sets"]["warm"]["gpu"][1]["execution"]["worker_generation"] += ":new"
        cases.append(("warm-generation", wrong_generation))
        wrong_reset = fixture_lane()
        wrong_reset["run_sets"]["cold"]["cpu"][0]["reset_state"] = "same_process_primed_model"
        cases.append(("cold-reset", wrong_reset))
        wrong_order = fixture_lane()
        wrong_order["run_sets"]["cold"]["cpu"][0]["pair_order"] = "gpu_then_cpu"
        cases.append(("pair-order", wrong_order))
        for label, lane in cases:
            with self.subTest(label=label):
                plan, evidence = fixture_documents(lane)
                result = self.run_tool(plan, evidence)
                self.assertEqual(result.returncode, 1, result.stdout)

    def test_cross_lane_artifact_and_attestation_reuse_is_rejected(self) -> None:
        first = fixture_lane()
        second = fixture_lane()
        second["identity"]["lane_id"] = "fixture-ubuntu-22.04-nvidia-cuda-second"
        plan, evidence = fixture_documents_for_lanes([first, second])
        result = self.run_tool(plan, evidence)
        self.assertEqual(result.returncode, 1)
        self.assertTrue(
            "artifact path" in result.stderr
            or "artifact digest" in result.stderr
            or "Hello attestation" in result.stderr
        )

    def test_correctness_reliability_and_lifecycle_failures_are_reported(self) -> None:
        cases: list[tuple[str, Any, set[str]]] = []
        mismatch = fixture_lane()
        mismatch["run_sets"]["warm"]["gpu"][0]["transcript_sha256"] = digest("wrong-transcript")
        cases.append(("correctness", mismatch, {"correctness_not_equivalent"}))
        failed_run = fixture_lane()
        run = failed_run["run_sets"]["warm"]["gpu"][0]
        run.update(
            {
                "backend_ms": 0,
                "end_to_end_ms": 0,
                "failure_category": "worker_crash",
                "outcome": "failure",
                "peak_process_memory_bytes": 0,
                "peak_vram_bytes": 0,
                "transcript_sha256": qualification.ZERO_SHA256,
            }
        )
        cases.append(
            (
                "reliability",
                failed_run,
                {"correctness_not_equivalent", "reliability_not_equivalent", "gpu_p95_exceeds_cpu_boundary"},
            )
        )
        lifecycle = fixture_lane()
        lifecycle["lifecycle"][0]["result"] = "fail"
        cases.append(("lifecycle", lifecycle, {"lifecycle_evidence_failed"}))
        for label, lane, expected_reasons in cases:
            with self.subTest(label=label):
                plan, evidence = fixture_documents(lane)
                decision = self.parse_success(self.run_tool(plan, evidence))
                self.assertFalse(decision["qualification_passed"])
                self.assertTrue(expected_reasons.issubset(set(decision["lanes"][0]["reasons"])))

    def test_missing_and_hostile_evidence_is_rejected(self) -> None:
        cases: list[tuple[str, dict[str, Any], dict[str, Any]]] = []
        plan, evidence = fixture_documents(fixture_lane())
        missing_run_plan, missing_run = copy.deepcopy(plan), copy.deepcopy(evidence)
        missing_run["lanes"][0]["run_sets"]["cold"]["gpu"].pop()
        bind_documents(missing_run_plan, missing_run)
        cases.append(("missing-run", missing_run_plan, missing_run))
        missing_event_plan, missing_event = copy.deepcopy(plan), copy.deepcopy(evidence)
        missing_event["lanes"][0]["lifecycle"].pop()
        bind_documents(missing_event_plan, missing_event)
        cases.append(("missing-event", missing_event_plan, missing_event))
        missing_lane = copy.deepcopy(evidence)
        missing_lane["lanes"] = []
        cases.append(("missing-lane", plan, missing_lane))
        boolean_metric_plan, boolean_metric = copy.deepcopy(plan), copy.deepcopy(evidence)
        boolean_metric["lanes"][0]["run_sets"]["warm"]["gpu"][0]["end_to_end_ms"] = True
        bind_documents(boolean_metric_plan, boolean_metric)
        cases.append(("boolean-metric", boolean_metric_plan, boolean_metric))
        reused_plan, reused = copy.deepcopy(plan), copy.deepcopy(evidence)
        reused["lanes"][0]["run_sets"]["warm"]["gpu"][1]["artifact_path"] = reused["lanes"][0]["run_sets"]["warm"]["gpu"][0]["artifact_path"]
        bind_documents(reused_plan, reused)
        cases.append(("reused-artifact", reused_plan, reused))
        extra_plan, extra = copy.deepcopy(plan), copy.deepcopy(evidence)
        extra["lanes"][0]["unexpected"] = "field"
        bind_documents(extra_plan, extra)
        cases.append(("unknown-field", extra_plan, extra))
        for label, identity_field, value in (
            ("old-kernel", "kernel_version", "5.14.0"),
            ("old-glibc", "glibc_version", "2.34"),
        ):
            invalid_lane = fixture_lane()
            invalid_lane["identity"][identity_field] = value
            invalid_plan, invalid_evidence = fixture_documents(invalid_lane)
            cases.append((label, invalid_plan, invalid_evidence))
        old_driver_lane = fixture_lane()
        old_driver_lane["identity"]["driver"]["value"] = "linux:nvidia:570.25"
        for event in old_driver_lane["lifecycle"]:
            event["driver_after"] = "linux:nvidia:570.25"
            event["driver_before"] = (
                "linux:nvidia:570.24" if event["event"] == "driver_change" else "linux:nvidia:570.25"
            )
        old_driver_plan, old_driver_evidence = fixture_documents(old_driver_lane)
        cases.append(("old-cuda-driver", old_driver_plan, old_driver_evidence))
        for label, case_plan, case_evidence in cases:
            with self.subTest(label=label):
                result = self.run_tool(case_plan, case_evidence)
                self.assertEqual(result.returncode, 1, result.stdout)
                self.assertIn("rejected", result.stderr)

    def test_mutation_after_review_is_rejected_as_forged_evidence(self) -> None:
        plan, evidence = fixture_documents(fixture_lane())
        evidence["lanes"][0]["run_sets"]["warm"]["gpu"][0]["end_to_end_ms"] = 1
        result = self.run_tool(plan, evidence)
        self.assertEqual(result.returncode, 1)
        self.assertIn("reviewed evidence digest", result.stderr)

    def test_missing_and_tampered_source_artifacts_are_rejected(self) -> None:
        plan, evidence = fixture_documents(fixture_lane())
        missing = self.run_tool(plan, evidence, prepare_artifacts=False)
        self.assertEqual(missing.returncode, 1)
        self.assertIn("could not inspect", missing.stderr)
        artifact_root = self.root / "artifacts"
        complete = self.run_tool(plan, evidence)
        self.assertEqual(complete.returncode, 0, complete.stderr)
        lane_path = evidence["lanes"][0]["identity"]["lane_id"]
        (artifact_root / lane_path / "runs/warm/gpu/01.evidence").write_bytes(b"tampered")
        plan_path = self.write("plan.json", plan)
        evidence_path = self.write("evidence.json", evidence)
        tampered = subprocess.run(
            [
                sys.executable,
                str(TOOL_PATH),
                "--plan",
                str(plan_path),
                "--evidence",
                str(evidence_path),
                "--artifact-root",
                str(artifact_root),
                "--allow-fixture",
            ],
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(tampered.returncode, 1)
        self.assertIn("digest does not match", tampered.stderr)

    def test_plan_and_identity_binding_mutations_are_rejected(self) -> None:
        plan, evidence = fixture_documents(fixture_lane())
        wrong_contract = copy.deepcopy(plan)
        wrong_contract["contract_bindings"]["runtime_contract_sha256"] = digest("wrong-contract")
        evidence_for_wrong_contract = copy.deepcopy(evidence)
        evidence_for_wrong_contract["plan_sha256"] = qualification.canonical_digest(wrong_contract)
        contract_result = self.run_tool(wrong_contract, evidence_for_wrong_contract)
        self.assertEqual(contract_result.returncode, 1)
        self.assertIn("checked-in contract", contract_result.stderr)
        wrong_plan_hash = copy.deepcopy(evidence)
        wrong_plan_hash["plan_sha256"] = digest("wrong-plan")
        hash_result = self.run_tool(plan, wrong_plan_hash)
        self.assertEqual(hash_result.returncode, 1)
        self.assertIn("exact reviewed plan", hash_result.stderr)
        identity_mutation = copy.deepcopy(evidence)
        identity_mutation["lanes"][0]["identity"]["driver"]["value"] = "linux:nvidia:999.0"
        identity_result = self.run_tool(plan, identity_mutation)
        self.assertEqual(identity_result.returncode, 1)
        self.assertIn("identity does not match", identity_result.stderr)

    def test_noncanonical_and_duplicate_json_are_rejected(self) -> None:
        plan, evidence = fixture_documents(fixture_lane())
        noncanonical = self.run_tool(plan, evidence, canonical=False)
        self.assertEqual(noncanonical.returncode, 1)
        self.assertIn("not canonical JSON", noncanonical.stderr)
        plan_path = self.write("duplicate-plan.json", plan)
        evidence_path = self.root / "duplicate-evidence.json"
        evidence_path.write_text(
            '{"fixture_only":true,"fixture_only":true,"kind":"linux_gpu_release_qualification_evidence","lanes":[],"plan_sha256":"' + digest("x") + '","schema_version":1}\n',
            encoding="utf-8",
        )
        result = subprocess.run(
            [sys.executable, str(TOOL_PATH), "--plan", str(plan_path), "--evidence", str(evidence_path), "--allow-fixture"],
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(result.returncode, 1)
        self.assertIn("duplicate field", result.stderr)


if __name__ == "__main__":
    unittest.main(verbosity=2)
