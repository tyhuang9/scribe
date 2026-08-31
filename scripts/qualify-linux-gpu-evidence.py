#!/usr/bin/env python3
"""Validate and summarize release-reviewed Linux GPU qualification evidence."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import re
import stat
import sys
from collections import Counter
from typing import Any


MAX_INPUT_BYTES = 16 * 1024 * 1024
ZERO_SHA256 = "0" * 64
SHA256_PATTERN = re.compile(r"^[0-9a-f]{64}$")
IDENTIFIER_PATTERN = re.compile(r"^[a-z0-9][a-z0-9._:-]{0,159}$")
PACK_COMPONENT_PATTERN = re.compile(r"^[a-z0-9](?:[a-z0-9._-]{0,94}[a-z0-9])?$")
PCI_ID_PATTERN = re.compile(r"^native:pci:[0-9a-f]{4}:[0-9a-f]{2}:[0-9a-f]{2}\.[0-7]$")
SUPPORTED_UBUNTU = {"22.04", "24.04"}
SUPPORTED_FAILURES = {
    "none",
    "unavailable",
    "startup",
    "handshake",
    "timeout",
    "oom",
    "device_loss",
    "provider_error",
    "worker_crash",
    "correctness_mismatch",
    "invalid_input",
    "model_corruption",
    "cancelled",
    "partial_output",
}
REQUIRED_EVENTS = ["device_loss", "driver_change", "suspend_resume"]
CONTRACT_PATHS = {
    "auto_manifest_sha256": "runtime-manifests/gpu-auto-qualification-linux-x86_64.json",
    "runtime_contract_sha256": "runtime-manifests/gpu-runtime-linux-x86_64.json",
    "toolchain_contract_sha256": "runtime-manifests/gpu-worker-toolchain-linux-x86_64.json",
}


class EvidenceError(ValueError):
    pass


def fail(message: str) -> None:
    raise EvidenceError(message)


def exact_keys(value: Any, expected: set[str], label: str) -> dict[str, Any]:
    if type(value) is not dict:
        fail(f"{label} must be an object")
    actual = set(value)
    if actual != expected:
        fail(f"{label} has unexpected or missing fields")
    return value


def json_string(value: Any, label: str, maximum: int = 256) -> str:
    if type(value) is not str or not value or len(value) > maximum:
        fail(f"{label} must be a nonempty bounded JSON string")
    if any(ord(character) < 0x20 or ord(character) > 0x7E for character in value):
        fail(f"{label} must contain printable ASCII only")
    return value


def json_integer(value: Any, label: str, minimum: int = 0, maximum: int = (1 << 63) - 1) -> int:
    if type(value) is not int or value < minimum or value > maximum:
        fail(f"{label} must be a bounded JSON integer")
    return value


def json_boolean(value: Any, label: str) -> bool:
    if type(value) is not bool:
        fail(f"{label} must be a JSON boolean")
    return value


def sha256_value(value: Any, label: str, *, allow_zero: bool = False) -> str:
    digest = json_string(value, label, 64)
    if SHA256_PATTERN.fullmatch(digest) is None or (not allow_zero and digest == ZERO_SHA256):
        fail(f"{label} must be a lowercase nonzero SHA-256 digest")
    return digest


def identifier(value: Any, label: str) -> str:
    result = json_string(value, label, 160)
    if IDENTIFIER_PATTERN.fullmatch(result) is None:
        fail(f"{label} is not a canonical identifier")
    return result


def pack_component(value: Any, label: str) -> str:
    result = json_string(value, label, 96)
    if PACK_COMPONENT_PATTERN.fullmatch(result) is None:
        fail(f"{label} is not a canonical pack component")
    return result


def canonical_bytes(value: Any) -> bytes:
    return (json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True) + "\n").encode("ascii")


def canonical_digest(value: Any) -> str:
    return hashlib.sha256(canonical_bytes(value)).hexdigest()


def reject_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            fail(f"JSON object contains duplicate field {key!r}")
        result[key] = value
    return result


def load_canonical_json(path: pathlib.Path, label: str) -> tuple[dict[str, Any], bytes]:
    try:
        file_stat = path.lstat()
    except OSError as error:
        fail(f"could not inspect {label}: {error}")
    if stat.S_ISLNK(file_stat.st_mode) or not stat.S_ISREG(file_stat.st_mode):
        fail(f"{label} must be a regular non-symlink file")
    if file_stat.st_size == 0 or file_stat.st_size > MAX_INPUT_BYTES:
        fail(f"{label} is empty or oversized")
    try:
        raw = path.read_bytes()
        text = raw.decode("utf-8")
        document = json.loads(text, object_pairs_hook=reject_duplicate_keys)
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        fail(f"could not parse {label}: {error}")
    if type(document) is not dict:
        fail(f"{label} must contain a JSON object")
    if raw != canonical_bytes(document):
        fail(f"{label} is not canonical JSON")
    return document, raw


def file_sha256(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        while chunk := source.read(1024 * 1024):
            digest.update(chunk)
    return digest.hexdigest()


def validate_pack(value: Any, label: str) -> None:
    pack = exact_keys(
        value,
        {"pack_id", "pack_version", "pack_digest", "security_epoch", "runtime_abi"},
        label,
    )
    pack_component(pack["pack_id"], f"{label}.pack_id")
    pack_component(pack["pack_version"], f"{label}.pack_version")
    sha256_value(pack["pack_digest"], f"{label}.pack_digest")
    json_integer(pack["security_epoch"], f"{label}.security_epoch", 1, (1 << 32) - 1)
    json_integer(pack["runtime_abi"], f"{label}.runtime_abi", 1, (1 << 16) - 1)


def validate_identity(value: Any, label: str) -> dict[str, Any]:
    identity = exact_keys(
        value,
        {
            "lane_id",
            "ubuntu_version",
            "target_arch",
            "kernel_version",
            "glibc_version",
            "backend",
            "provider_id",
            "pack",
            "model",
            "workload",
            "device",
            "driver",
        },
        label,
    )
    identifier(identity["lane_id"], f"{label}.lane_id")
    ubuntu = json_string(identity["ubuntu_version"], f"{label}.ubuntu_version", 5)
    if ubuntu not in SUPPORTED_UBUNTU:
        fail(f"{label}.ubuntu_version is outside the reviewed Ubuntu lanes")
    if json_string(identity["target_arch"], f"{label}.target_arch", 16) != "x86_64":
        fail(f"{label}.target_arch must be x86_64")
    json_string(identity["kernel_version"], f"{label}.kernel_version", 128)
    json_string(identity["glibc_version"], f"{label}.glibc_version", 64)
    backend = json_string(identity["backend"], f"{label}.backend", 16)
    provider = identifier(identity["provider_id"], f"{label}.provider_id")
    validate_pack(identity["pack"], f"{label}.pack")
    model = exact_keys(identity["model"], {"model_id", "model_digest"}, f"{label}.model")
    identifier(model["model_id"], f"{label}.model.model_id")
    sha256_value(model["model_digest"], f"{label}.model.model_digest")
    workload = exact_keys(
        identity["workload"],
        {"workload_id", "audio_sha256", "expected_transcript_sha256"},
        f"{label}.workload",
    )
    identifier(workload["workload_id"], f"{label}.workload.workload_id")
    sha256_value(workload["audio_sha256"], f"{label}.workload.audio_sha256")
    sha256_value(
        workload["expected_transcript_sha256"],
        f"{label}.workload.expected_transcript_sha256",
    )
    device = exact_keys(
        identity["device"],
        {"stable_device_id", "vendor", "device_class", "total_memory_bytes"},
        f"{label}.device",
    )
    stable_id = json_string(device["stable_device_id"], f"{label}.device.stable_device_id", 64)
    if PCI_ID_PATTERN.fullmatch(stable_id) is None:
        fail(f"{label}.device.stable_device_id must be a canonical Linux PCI identity")
    vendor = json_string(device["vendor"], f"{label}.device.vendor", 16)
    if vendor not in {"nvidia", "amd", "intel"}:
        fail(f"{label}.device.vendor is unsupported")
    device_class = json_string(device["device_class"], f"{label}.device.device_class", 32)
    if device_class not in {"discrete_gpu", "integrated_gpu", "unified_gpu"}:
        fail(f"{label}.device.device_class is unsupported")
    json_integer(device["total_memory_bytes"], f"{label}.device.total_memory_bytes", 1)
    driver = exact_keys(identity["driver"], {"kind", "value"}, f"{label}.driver")
    if json_string(driver["kind"], f"{label}.driver.kind", 16) != "exact":
        fail(f"{label}.driver.kind must be exact")
    json_string(driver["value"], f"{label}.driver.value", 128)
    valid_binding = (
        backend == "cuda" and provider == "transcribe-cpp-ggml-cuda" and vendor == "nvidia"
    ) or (
        backend == "vulkan"
        and provider == "transcribe-cpp-ggml-vulkan"
        and vendor in {"nvidia", "amd", "intel"}
    )
    if not valid_binding:
        fail(f"{label} has an invalid backend, provider, and vendor binding")
    return identity


def validate_plan(plan: dict[str, Any], repository_root: pathlib.Path) -> list[dict[str, Any]]:
    exact_keys(
        plan,
        {
            "schema_version",
            "kind",
            "fixture_only",
            "target_os",
            "target_arch",
            "cold_runs",
            "warm_runs",
            "maximum_gpu_p95_cpu_percent",
            "required_events",
            "contract_bindings",
            "required_lanes",
        },
        "qualification plan",
    )
    if json_integer(plan["schema_version"], "qualification plan.schema_version", 1, 1) != 1:
        fail("qualification plan schema is unsupported")
    if json_string(plan["kind"], "qualification plan.kind") != "linux_gpu_release_qualification_plan":
        fail("qualification plan kind is unsupported")
    json_boolean(plan["fixture_only"], "qualification plan.fixture_only")
    if json_string(plan["target_os"], "qualification plan.target_os") != "linux":
        fail("qualification plan must target Linux")
    if json_string(plan["target_arch"], "qualification plan.target_arch") != "x86_64":
        fail("qualification plan must target x86_64")
    if json_integer(plan["cold_runs"], "qualification plan.cold_runs") != 5:
        fail("qualification plan must require exactly five cold runs")
    if json_integer(plan["warm_runs"], "qualification plan.warm_runs") != 20:
        fail("qualification plan must require exactly twenty warm runs")
    if json_integer(
        plan["maximum_gpu_p95_cpu_percent"],
        "qualification plan.maximum_gpu_p95_cpu_percent",
    ) != 110:
        fail("qualification plan must use the reviewed 110 percent p95 boundary")
    if plan["required_events"] != REQUIRED_EVENTS:
        fail("qualification plan required events are not canonical and complete")
    bindings = exact_keys(plan["contract_bindings"], set(CONTRACT_PATHS), "qualification plan.contract_bindings")
    for field, relative_path in CONTRACT_PATHS.items():
        expected = sha256_value(bindings[field], f"qualification plan.contract_bindings.{field}")
        actual = file_sha256(repository_root / relative_path)
        if expected != actual:
            fail(f"qualification plan {field} does not bind the checked-in contract")
    if type(plan["required_lanes"]) is not list:
        fail("qualification plan.required_lanes must be an array")
    required_lanes: list[dict[str, Any]] = []
    previous_lane_id = ""
    evidence_digests: set[str] = set()
    for index, entry_value in enumerate(plan["required_lanes"]):
        entry = exact_keys(entry_value, {"identity", "evidence_sha256"}, f"required lane {index}")
        identity = validate_identity(entry["identity"], f"required lane {index}.identity")
        lane_id = identity["lane_id"]
        if lane_id <= previous_lane_id:
            fail("qualification plan required lanes must be strictly sorted and unique")
        previous_lane_id = lane_id
        digest = sha256_value(entry["evidence_sha256"], f"required lane {index}.evidence_sha256")
        if digest in evidence_digests:
            fail("qualification plan reuses one evidence digest for multiple lanes")
        evidence_digests.add(digest)
        required_lanes.append(entry)
    return required_lanes


def validate_run(
    value: Any,
    label: str,
    expected_sequence: int,
    target: str,
    artifact_digests: set[str],
) -> dict[str, Any]:
    run = exact_keys(
        value,
        {
            "sequence",
            "artifact_sha256",
            "outcome",
            "failure_category",
            "end_to_end_ms",
            "backend_ms",
            "peak_process_memory_bytes",
            "peak_vram_bytes",
            "transcript_sha256",
        },
        label,
    )
    if json_integer(run["sequence"], f"{label}.sequence", 1, 20) != expected_sequence:
        fail(f"{label}.sequence is not canonical and contiguous")
    artifact = sha256_value(run["artifact_sha256"], f"{label}.artifact_sha256")
    if artifact in artifact_digests:
        fail("qualification evidence reuses an artifact digest")
    artifact_digests.add(artifact)
    outcome = json_string(run["outcome"], f"{label}.outcome", 16)
    if outcome not in {"success", "failure"}:
        fail(f"{label}.outcome is unsupported")
    failure = json_string(run["failure_category"], f"{label}.failure_category", 32)
    if failure not in SUPPORTED_FAILURES:
        fail(f"{label}.failure_category is unsupported")
    end_to_end = json_integer(run["end_to_end_ms"], f"{label}.end_to_end_ms")
    backend = json_integer(run["backend_ms"], f"{label}.backend_ms")
    memory = json_integer(run["peak_process_memory_bytes"], f"{label}.peak_process_memory_bytes")
    vram = json_integer(run["peak_vram_bytes"], f"{label}.peak_vram_bytes")
    transcript = sha256_value(
        run["transcript_sha256"], f"{label}.transcript_sha256", allow_zero=True
    )
    if outcome == "success":
        if failure != "none" or min(end_to_end, backend, memory) <= 0 or backend > end_to_end:
            fail(f"{label} has inconsistent successful-run metrics")
        if transcript == ZERO_SHA256:
            fail(f"{label} successful run has no transcript digest")
    else:
        if failure == "none" or transcript != ZERO_SHA256:
            fail(f"{label} has inconsistent failure metadata")
    if target == "cpu" and vram != 0:
        fail(f"{label} CPU run must report zero VRAM")
    return run


def validate_event(
    value: Any,
    label: str,
    expected_event: str,
    identity: dict[str, Any],
    artifact_digests: set[str],
) -> dict[str, Any]:
    event = exact_keys(
        value,
        {
            "event",
            "artifact_sha256",
            "result",
            "observed_failure_category",
            "selection_reevaluated",
            "active_request_migrated",
            "partial_output_replayed",
            "recovered_next_request",
            "stable_device_id_after",
            "driver_before",
            "driver_after",
        },
        label,
    )
    if json_string(event["event"], f"{label}.event") != expected_event:
        fail(f"{label}.event is not canonical")
    artifact = sha256_value(event["artifact_sha256"], f"{label}.artifact_sha256")
    if artifact in artifact_digests:
        fail("qualification evidence reuses an artifact digest")
    artifact_digests.add(artifact)
    if json_string(event["result"], f"{label}.result", 16) not in {"pass", "fail"}:
        fail(f"{label}.result is unsupported")
    category = json_string(
        event["observed_failure_category"], f"{label}.observed_failure_category", 32
    )
    if category not in SUPPORTED_FAILURES:
        fail(f"{label}.observed_failure_category is unsupported")
    for field in (
        "selection_reevaluated",
        "active_request_migrated",
        "partial_output_replayed",
        "recovered_next_request",
    ):
        json_boolean(event[field], f"{label}.{field}")
    stable_id = json_string(event["stable_device_id_after"], f"{label}.stable_device_id_after", 64)
    if stable_id != identity["device"]["stable_device_id"]:
        fail(f"{label} is bound to the wrong stable device")
    before = json_string(event["driver_before"], f"{label}.driver_before", 128)
    after = json_string(event["driver_after"], f"{label}.driver_after", 128)
    current_driver = identity["driver"]["value"]
    if expected_event == "device_loss":
        if category != "device_loss" or before != current_driver or after != current_driver:
            fail(f"{label} does not describe the required device-loss observation")
    elif expected_event == "driver_change":
        if category != "none" or before == after or after != current_driver:
            fail(f"{label} does not describe the required driver-change observation")
    elif category != "none" or before != current_driver or after != current_driver:
        fail(f"{label} does not describe the required suspend/resume observation")
    return event


def nearest_rank(values: list[int], percentile: int) -> int:
    ordered = sorted(values)
    rank = (len(ordered) * percentile + 99) // 100
    return ordered[rank - 1]


def metric_summary(runs: list[dict[str, Any]]) -> dict[str, Any]:
    successful = [run for run in runs if run["outcome"] == "success"]
    failures = Counter(run["failure_category"] for run in runs if run["outcome"] == "failure")
    result: dict[str, Any] = {
        "failure_categories": dict(sorted(failures.items())),
        "run_count": len(runs),
        "successful_runs": len(successful),
    }
    for field in (
        "end_to_end_ms",
        "backend_ms",
        "peak_process_memory_bytes",
        "peak_vram_bytes",
    ):
        values = [run[field] for run in successful]
        result[field] = (
            {"p50": nearest_rank(values, 50), "p95": nearest_rank(values, 95)} if values else None
        )
    return result


def validate_lane_evidence(
    value: Any,
    expected: dict[str, Any],
    plan: dict[str, Any],
    index: int,
) -> tuple[dict[str, Any], dict[str, Any]]:
    lane = exact_keys(value, {"identity", "run_sets", "lifecycle"}, f"evidence lane {index}")
    identity = validate_identity(lane["identity"], f"evidence lane {index}.identity")
    if identity != expected["identity"]:
        fail(f"evidence lane {index} identity does not match its reviewed plan")
    if canonical_digest(lane) != expected["evidence_sha256"]:
        fail(f"evidence lane {index} does not match its reviewed evidence digest")
    run_sets = exact_keys(lane["run_sets"], {"cold", "warm"}, f"evidence lane {index}.run_sets")
    artifact_digests: set[str] = set()
    parsed: dict[str, dict[str, list[dict[str, Any]]]] = {}
    for mode, expected_count in (("cold", plan["cold_runs"]), ("warm", plan["warm_runs"])):
        target_sets = exact_keys(run_sets[mode], {"cpu", "gpu"}, f"evidence lane {index}.{mode}")
        parsed[mode] = {}
        for target in ("cpu", "gpu"):
            raw_runs = target_sets[target]
            if type(raw_runs) is not list or len(raw_runs) != expected_count:
                fail(f"evidence lane {index}.{mode}.{target} has the wrong run count")
            parsed[mode][target] = [
                validate_run(run, f"evidence lane {index}.{mode}.{target}[{offset}]", offset + 1, target, artifact_digests)
                for offset, run in enumerate(raw_runs)
            ]
    if type(lane["lifecycle"]) is not list or len(lane["lifecycle"]) != len(REQUIRED_EVENTS):
        fail(f"evidence lane {index}.lifecycle is incomplete")
    events = [
        validate_event(event, f"evidence lane {index}.lifecycle[{offset}]", expected_event, identity, artifact_digests)
        for offset, (event, expected_event) in enumerate(zip(lane["lifecycle"], REQUIRED_EVENTS))
    ]

    expected_transcript = identity["workload"]["expected_transcript_sha256"]
    all_runs = [run for mode in parsed.values() for target in mode.values() for run in target]
    all_successful = all(run["outcome"] == "success" for run in all_runs)
    correctness_equivalent = all_successful and all(
        run["transcript_sha256"] == expected_transcript for run in all_runs
    )
    reliability_equivalent = all_successful
    lifecycle_passed = all(
        event["result"] == "pass"
        and event["selection_reevaluated"]
        and not event["active_request_migrated"]
        and not event["partial_output_replayed"]
        and event["recovered_next_request"]
        for event in events
    )
    gpu_p95 = nearest_rank([run["end_to_end_ms"] for run in parsed["warm"]["gpu"] if run["outcome"] == "success"], 95) if all_successful else 0
    cpu_p95 = nearest_rank([run["end_to_end_ms"] for run in parsed["warm"]["cpu"] if run["outcome"] == "success"], 95) if all_successful else 0
    performance_passed = all_successful and gpu_p95 * 100 <= cpu_p95 * plan["maximum_gpu_p95_cpu_percent"]
    reasons: list[str] = []
    if not correctness_equivalent:
        reasons.append("correctness_not_equivalent")
    if not reliability_equivalent:
        reasons.append("reliability_not_equivalent")
    if not lifecycle_passed:
        reasons.append("lifecycle_evidence_failed")
    if not performance_passed:
        reasons.append("gpu_p95_exceeds_cpu_boundary")
    passed = not reasons
    summary = {
        "backend": identity["backend"],
        "device_stable_id": identity["device"]["stable_device_id"],
        "driver": identity["driver"]["value"],
        "lane_id": identity["lane_id"],
        "metrics": {
            mode: {target: metric_summary(parsed[mode][target]) for target in ("cpu", "gpu")}
            for mode in ("cold", "warm")
        },
        "checks": {
            "correctness_equivalent": correctness_equivalent,
            "lifecycle_passed": lifecycle_passed,
            "performance_passed": performance_passed,
            "reliability_equivalent": reliability_equivalent,
        },
        "qualification_passed": passed,
        "reasons": reasons,
    }
    return lane, summary


def decide(
    plan: dict[str, Any],
    plan_raw: bytes,
    evidence: dict[str, Any],
    repository_root: pathlib.Path,
    allow_fixture: bool,
) -> dict[str, Any]:
    required_lanes = validate_plan(plan, repository_root)
    exact_keys(
        evidence,
        {"schema_version", "kind", "fixture_only", "plan_sha256", "lanes"},
        "qualification evidence",
    )
    if json_integer(evidence["schema_version"], "qualification evidence.schema_version", 1, 1) != 1:
        fail("qualification evidence schema is unsupported")
    if json_string(evidence["kind"], "qualification evidence.kind") != "linux_gpu_release_qualification_evidence":
        fail("qualification evidence kind is unsupported")
    fixture_only = json_boolean(evidence["fixture_only"], "qualification evidence.fixture_only")
    if fixture_only != plan["fixture_only"]:
        fail("qualification plan and evidence fixture modes differ")
    if fixture_only and not allow_fixture:
        fail("fixture-only qualification evidence requires --allow-fixture")
    plan_digest = hashlib.sha256(plan_raw).hexdigest()
    if sha256_value(evidence["plan_sha256"], "qualification evidence.plan_sha256") != plan_digest:
        fail("qualification evidence does not bind the exact reviewed plan")
    if type(evidence["lanes"]) is not list:
        fail("qualification evidence.lanes must be an array")
    if len(evidence["lanes"]) != len(required_lanes):
        fail("qualification evidence does not cover every representative lane")
    summaries = [
        validate_lane_evidence(lane, expected, plan, index)[1]
        for index, (lane, expected) in enumerate(zip(evidence["lanes"], required_lanes))
    ]
    evidence_complete = bool(required_lanes) and len(summaries) == len(required_lanes)
    qualification_passed = evidence_complete and all(summary["qualification_passed"] for summary in summaries)
    auto_eligible = qualification_passed and not fixture_only
    if fixture_only:
        reason = "fixture_only_never_auto_eligible"
    elif not evidence_complete:
        reason = "no_complete_representative_evidence"
    elif not qualification_passed:
        reason = "one_or_more_representative_lanes_failed"
    else:
        reason = "complete_release_evidence_passed"
    return {
        "schema_version": 1,
        "kind": "linux_gpu_release_qualification_decision",
        "auto_eligible": auto_eligible,
        "decision_reason": reason,
        "evidence_complete": evidence_complete,
        "evidence_sha256": canonical_digest(evidence),
        "fixture_only": fixture_only,
        "lanes": summaries,
        "plan_sha256": plan_digest,
        "qualification_passed": qualification_passed,
    }


def parse_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--plan", required=True, type=pathlib.Path)
    parser.add_argument("--evidence", required=True, type=pathlib.Path)
    parser.add_argument("--output", type=pathlib.Path)
    parser.add_argument("--allow-fixture", action="store_true")
    parser.add_argument("--require-eligible", action="store_true")
    return parser.parse_args()


def write_new_file(path: pathlib.Path, payload: bytes) -> None:
    path = path.resolve()
    if path.exists() or path.is_symlink():
        fail("output path already exists")
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    try:
        with temporary.open("xb") as output:
            output.write(payload)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
    finally:
        try:
            temporary.unlink()
        except FileNotFoundError:
            pass


def main() -> int:
    arguments = parse_arguments()
    repository_root = pathlib.Path(__file__).resolve().parent.parent
    try:
        plan, plan_raw = load_canonical_json(arguments.plan, "qualification plan")
        evidence, _ = load_canonical_json(arguments.evidence, "qualification evidence")
        decision = decide(plan, plan_raw, evidence, repository_root, arguments.allow_fixture)
        payload = canonical_bytes(decision)
        if arguments.output is not None:
            write_new_file(arguments.output, payload)
        sys.stdout.buffer.write(payload)
        if arguments.require_eligible and not decision["auto_eligible"]:
            return 2
        return 0
    except EvidenceError as error:
        print(f"Linux GPU qualification rejected: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
