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
MAX_LANES = 64
MAX_ARTIFACTS = 4096
MAX_CUMULATIVE_ARTIFACT_BYTES = 512 * 1024 * 1024
MIN_GPU_MEMORY_BYTES = 256 * 1024 * 1024
MIN_GPU_PEAK_MEMORY_BYTES = 16 * 1024 * 1024
ZERO_SHA256 = "0" * 64
SHA256_PATTERN = re.compile(r"^[0-9a-f]{64}$")
IDENTIFIER_PATTERN = re.compile(r"^[a-z0-9][a-z0-9._:-]{0,159}$")
PACK_COMPONENT_PATTERN = re.compile(r"^[a-z0-9](?:[a-z0-9._-]{0,94}[a-z0-9])?$")
PCI_ID_PATTERN = re.compile(r"^native:pci:[0-9a-f]{4}:[0-9a-f]{2}:[0-9a-f]{2}\.[0-7]$")
ARTIFACT_COMPONENT_PATTERN = re.compile(r"^[a-z0-9](?:[a-z0-9._-]{0,94}[a-z0-9])?$")
VERSION_PATTERN = re.compile(r"^([0-9]+)\.([0-9]+)(?:\.[0-9]+)*(?:[-+._][a-z0-9.-]+)?$")
DRIVER_IDENTITY_PATTERN = re.compile(r"^linux:[a-z0-9._-]+:[a-z0-9:._-]+$")
CUDA_DRIVER_PATTERN = re.compile(r"^linux:nvidia:([0-9]+)\.([0-9]+)(?:\.[0-9]+)*$")
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
PRODUCTION_AUTHORITY_PATH = "runtime-manifests/linux-gpu-qualification-production-authority.json"


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
    if file_stat.st_nlink != 1:
        fail(f"{label} must have exactly one link")
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


def artifact_path(value: Any, label: str) -> pathlib.PurePosixPath:
    raw = json_string(value, label, 240)
    if "\\" in raw or raw.startswith("/"):
        fail(f"{label} must be a canonical relative POSIX path")
    path = pathlib.PurePosixPath(raw)
    if not path.parts or any(
        part in {"", ".", ".."} or ARTIFACT_COMPONENT_PATTERN.fullmatch(part) is None
        for part in path.parts
    ):
        fail(f"{label} must be a canonical relative POSIX path")
    return path


def validate_artifact_file(
    artifact_root: pathlib.Path,
    relative_value: Any,
    expected_digest: str,
    label: str,
    artifact_paths: set[str],
    artifact_budget: dict[str, int],
    descriptor_bound: bool,
) -> bytes:
    relative = artifact_path(relative_value, f"{label}.artifact_path")
    folded = relative.as_posix().casefold()
    if folded in artifact_paths:
        fail("qualification evidence reuses or case-collides an artifact path")
    artifact_paths.add(folded)
    if descriptor_bound:
        raw, current_stat = read_descriptor_bound_artifact(artifact_root, relative, label)
    else:
        current = artifact_root
        for index, component in enumerate(relative.parts):
            current = current / component
            try:
                current_stat = current.lstat()
            except OSError as error:
                fail(f"could not inspect {label} artifact: {error}")
            if stat.S_ISLNK(current_stat.st_mode):
                fail(f"{label} artifact path contains a symbolic link")
            if index + 1 < len(relative.parts):
                if not stat.S_ISDIR(current_stat.st_mode):
                    fail(f"{label} artifact ancestor is not a directory")
            elif not stat.S_ISREG(current_stat.st_mode):
                fail(f"{label} artifact must be a regular file")
        raw = current.read_bytes()
    if current_stat.st_nlink != 1:
        fail(f"{label} artifact must have exactly one link")
    if current_stat.st_size == 0 or current_stat.st_size > MAX_INPUT_BYTES:
        fail(f"{label} artifact is empty or oversized")
    artifact_budget["count"] += 1
    artifact_budget["bytes"] += current_stat.st_size
    if artifact_budget["count"] > MAX_ARTIFACTS:
        fail("qualification evidence exceeds the global artifact-count bound")
    if artifact_budget["bytes"] > MAX_CUMULATIVE_ARTIFACT_BYTES:
        fail("qualification evidence exceeds the cumulative artifact-byte bound")
    if hashlib.sha256(raw).hexdigest() != expected_digest:
        fail(f"{label} artifact digest does not match the supplied file")
    return raw


def read_descriptor_bound_artifact(
    artifact_root: pathlib.Path,
    relative: pathlib.PurePosixPath,
    label: str,
) -> tuple[bytes, os.stat_result]:
    if sys.platform != "linux":
        fail("production artifact evaluation is supported only on Linux")
    directory_flags = os.O_RDONLY | os.O_CLOEXEC | os.O_DIRECTORY | os.O_NOFOLLOW
    file_flags = os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW
    descriptors: list[int] = []
    try:
        descriptors.append(os.open(artifact_root, directory_flags))
        for component in relative.parts[:-1]:
            descriptors.append(os.open(component, directory_flags, dir_fd=descriptors[-1]))
        file_descriptor = os.open(relative.parts[-1], file_flags, dir_fd=descriptors[-1])
        descriptors.append(file_descriptor)
        file_stat = os.fstat(file_descriptor)
        if not stat.S_ISREG(file_stat.st_mode):
            fail(f"{label} artifact must be a regular file")
        chunks: list[bytes] = []
        total = 0
        while True:
            chunk = os.read(file_descriptor, 1024 * 1024)
            if not chunk:
                break
            total += len(chunk)
            if total > MAX_INPUT_BYTES:
                fail(f"{label} artifact is oversized")
            chunks.append(chunk)
        after = os.fstat(file_descriptor)
        if (
            after.st_dev,
            after.st_ino,
            after.st_size,
            after.st_mtime_ns,
            after.st_ctime_ns,
        ) != (
            file_stat.st_dev,
            file_stat.st_ino,
            file_stat.st_size,
            file_stat.st_mtime_ns,
            file_stat.st_ctime_ns,
        ):
            fail(f"{label} artifact changed during descriptor-bound acquisition")
        return b"".join(chunks), after
    except OSError as error:
        fail(f"could not descriptor-open {label} artifact: {error}")
    finally:
        for descriptor in reversed(descriptors):
            try:
                os.close(descriptor)
            except OSError:
                pass


def validate_artifact_envelope(
    artifact_root: pathlib.Path,
    relative_value: Any,
    expected_digest: str,
    expected_kind: str,
    expected_record: dict[str, Any],
    label: str,
    artifact_paths: set[str],
    artifact_budget: dict[str, int],
    descriptor_bound: bool,
) -> None:
    raw = validate_artifact_file(
        artifact_root,
        relative_value,
        expected_digest,
        label,
        artifact_paths,
        artifact_budget,
        descriptor_bound,
    )
    try:
        envelope = json.loads(raw.decode("utf-8"), object_pairs_hook=reject_duplicate_keys)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        fail(f"{label} artifact is not a canonical evidence envelope: {error}")
    exact_keys(envelope, {"schema_version", "kind", "record"}, f"{label} artifact envelope")
    if raw != canonical_bytes(envelope):
        fail(f"{label} artifact envelope is not canonical JSON")
    if json_integer(envelope["schema_version"], f"{label} artifact schema_version", 1, 1) != 1:
        fail(f"{label} artifact schema is unsupported")
    if json_string(envelope["kind"], f"{label} artifact kind") != expected_kind:
        fail(f"{label} artifact kind is unsupported")
    if envelope["record"] != expected_record:
        fail(f"{label} artifact record does not match the reviewed evidence")


def validate_execution(
    value: Any,
    label: str,
    target: str,
    identity: dict[str, Any],
    mode: str,
    sequence: int,
    attestation_digests: set[str],
) -> dict[str, Any]:
    execution = exact_keys(
        value,
        {
            "backend",
            "provider_id",
            "worker_build_id",
            "worker_sha256",
            "protocol_version",
            "runtime_abi",
            "worker_generation",
            "hello_sha256",
            "stable_device_id",
            "device_memory_kind",
        },
        label,
    )
    worker = identity["cpu_baseline"] if target == "cpu" else identity["gpu_worker"]
    expected_backend = "cpu" if target == "cpu" else identity["backend"]
    expected_stable_id = "cpu:host" if target == "cpu" else identity["device"]["stable_device_id"]
    expected_memory_kind = "none" if target == "cpu" else identity["device"]["memory_model"]
    expected_generation = (
        f"{identity['acquisition']['batch_id']}:{mode}:{target}:{sequence:02}"
        if mode == "cold"
        else f"{identity['acquisition']['batch_id']}:warm:{target}"
    )
    expected_values = {
        "backend": expected_backend,
        "provider_id": worker["provider_id"],
        "worker_build_id": worker["worker_build_id"],
        "worker_sha256": worker["worker_sha256"],
        "protocol_version": worker["protocol_version"],
        "runtime_abi": worker["runtime_abi"],
        "worker_generation": expected_generation,
        "stable_device_id": expected_stable_id,
        "device_memory_kind": expected_memory_kind,
    }
    for field, expected in expected_values.items():
        observed = execution[field]
        if type(expected) is int:
            json_integer(observed, f"{label}.{field}", 1)
        else:
            json_string(observed, f"{label}.{field}", 160)
        if observed != expected:
            fail(f"{label}.{field} does not match the admitted execution target")
    hello = sha256_value(execution["hello_sha256"], f"{label}.hello_sha256")
    if hello in attestation_digests:
        fail("qualification evidence reuses a worker Hello attestation")
    attestation_digests.add(hello)
    return execution


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


def validate_worker_identity(value: Any, label: str, *, cpu: bool) -> dict[str, Any]:
    worker = exact_keys(
        value,
        {
            "backend",
            "provider_id",
            "worker_build_id",
            "worker_sha256",
            "protocol_version",
            "runtime_abi",
        },
        label,
    )
    expected_backend = "cpu" if cpu else None
    backend = json_string(worker["backend"], f"{label}.backend", 16)
    if expected_backend is not None and backend != expected_backend:
        fail(f"{label}.backend must be CPU")
    provider = identifier(worker["provider_id"], f"{label}.provider_id")
    if cpu and provider != "scribe-inference-worker-cpu":
        fail(f"{label}.provider_id must identify the reviewed CPU baseline")
    json_string(worker["worker_build_id"], f"{label}.worker_build_id", 160)
    sha256_value(worker["worker_sha256"], f"{label}.worker_sha256")
    if json_integer(worker["protocol_version"], f"{label}.protocol_version", 1, 255) != 5:
        fail(f"{label}.protocol_version must match the reviewed worker protocol")
    json_integer(worker["runtime_abi"], f"{label}.runtime_abi", 1, (1 << 16) - 1)
    return worker


def validate_acquisition(value: Any, label: str) -> dict[str, Any]:
    acquisition = exact_keys(
        value,
        {"protocol", "batch_id", "machine_id_sha256", "host", "threading", "controls", "ordering"},
        label,
    )
    protocol = exact_keys(
        acquisition["protocol"],
        {"protocol_id", "protocol_version", "harness_sha256"},
        f"{label}.protocol",
    )
    if identifier(protocol["protocol_id"], f"{label}.protocol.protocol_id") != "scribe-linux-gpu-qualification":
        fail(f"{label}.protocol.protocol_id is unsupported")
    if json_integer(protocol["protocol_version"], f"{label}.protocol.protocol_version", 1, 1) != 1:
        fail(f"{label}.protocol.protocol_version is unsupported")
    sha256_value(protocol["harness_sha256"], f"{label}.protocol.harness_sha256")
    identifier(acquisition["batch_id"], f"{label}.batch_id")
    sha256_value(acquisition["machine_id_sha256"], f"{label}.machine_id_sha256")
    host = exact_keys(
        acquisition["host"],
        {
            "cpu_arch",
            "cpu_model_sha256",
            "physical_cores",
            "logical_cpus",
            "numa_nodes",
            "total_memory_bytes",
        },
        f"{label}.host",
    )
    if json_string(host["cpu_arch"], f"{label}.host.cpu_arch", 16) != "x86_64":
        fail(f"{label}.host.cpu_arch must be x86_64")
    sha256_value(host["cpu_model_sha256"], f"{label}.host.cpu_model_sha256")
    physical = json_integer(host["physical_cores"], f"{label}.host.physical_cores", 1, 4096)
    logical = json_integer(host["logical_cpus"], f"{label}.host.logical_cpus", 1, 8192)
    if physical > logical:
        fail(f"{label}.host core topology is inconsistent")
    json_integer(host["numa_nodes"], f"{label}.host.numa_nodes", 1, 256)
    json_integer(host["total_memory_bytes"], f"{label}.host.total_memory_bytes", 1024 * 1024 * 1024)
    threading = exact_keys(
        acquisition["threading"],
        {"cpu_worker_threads", "gpu_worker_threads", "cpu_affinity_sha256", "gpu_affinity_sha256"},
        f"{label}.threading",
    )
    cpu_threads = json_integer(
        threading["cpu_worker_threads"], f"{label}.threading.cpu_worker_threads", 1, logical
    )
    gpu_threads = json_integer(
        threading["gpu_worker_threads"], f"{label}.threading.gpu_worker_threads", 1, logical
    )
    if cpu_threads > logical or gpu_threads > logical:
        fail(f"{label}.threading exceeds the host topology")
    sha256_value(threading["cpu_affinity_sha256"], f"{label}.threading.cpu_affinity_sha256")
    sha256_value(threading["gpu_affinity_sha256"], f"{label}.threading.gpu_affinity_sha256")
    controls = exact_keys(
        acquisition["controls"],
        {
            "power_source",
            "cpu_governor",
            "gpu_power_profile",
            "thermal_policy",
            "background_load_policy",
        },
        f"{label}.controls",
    )
    expected_controls = {
        "power_source": "ac",
        "cpu_governor": "performance",
        "gpu_power_profile": "fixed_maximum_performance",
        "thermal_policy": "no_throttling_observed",
        "background_load_policy": "isolated",
    }
    for field, expected in expected_controls.items():
        if json_string(controls[field], f"{label}.controls.{field}", 64) != expected:
            fail(f"{label}.controls.{field} violates acquisition protocol v1")
    ordering = exact_keys(
        acquisition["ordering"],
        {"scheme", "warm_priming_runs"},
        f"{label}.ordering",
    )
    if json_string(ordering["scheme"], f"{label}.ordering.scheme", 64) != "paired_alternating_cpu_first_v1":
        fail(f"{label}.ordering.scheme violates acquisition protocol v1")
    if json_integer(ordering["warm_priming_runs"], f"{label}.ordering.warm_priming_runs", 0, 16) != 1:
        fail(f"{label}.ordering.warm_priming_runs violates acquisition protocol v1")
    return acquisition


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
            "cpu_baseline",
            "gpu_worker",
            "acquisition",
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
    kernel_version = json_string(identity["kernel_version"], f"{label}.kernel_version", 128)
    kernel_match = VERSION_PATTERN.fullmatch(kernel_version)
    if kernel_match is None or (int(kernel_match.group(1)), int(kernel_match.group(2))) < (5, 15):
        fail(f"{label}.kernel_version is outside the reviewed Linux runtime contract")
    glibc_version = json_string(identity["glibc_version"], f"{label}.glibc_version", 64)
    glibc_match = VERSION_PATTERN.fullmatch(glibc_version)
    if glibc_match is None or (int(glibc_match.group(1)), int(glibc_match.group(2))) < (2, 35):
        fail(f"{label}.glibc_version is outside the reviewed Linux runtime contract")
    backend = json_string(identity["backend"], f"{label}.backend", 16)
    provider = identifier(identity["provider_id"], f"{label}.provider_id")
    cpu_baseline = validate_worker_identity(identity["cpu_baseline"], f"{label}.cpu_baseline", cpu=True)
    gpu_worker = validate_worker_identity(identity["gpu_worker"], f"{label}.gpu_worker", cpu=False)
    acquisition = validate_acquisition(identity["acquisition"], f"{label}.acquisition")
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
        {
            "stable_device_id",
            "vendor",
            "device_class",
            "memory_model",
            "total_memory_bytes",
            "qualified_minimum_total_memory_bytes",
        },
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
    total_memory = json_integer(
        device["total_memory_bytes"],
        f"{label}.device.total_memory_bytes",
        MIN_GPU_MEMORY_BYTES,
    )
    qualified_minimum = json_integer(
        device["qualified_minimum_total_memory_bytes"],
        f"{label}.device.qualified_minimum_total_memory_bytes",
        MIN_GPU_MEMORY_BYTES,
        total_memory,
    )
    if qualified_minimum > total_memory:
        fail(f"{label}.device qualified memory exceeds the observed total")
    memory_model = json_string(device["memory_model"], f"{label}.device.memory_model", 32)
    expected_memory_model = "dedicated_vram" if device_class == "discrete_gpu" else "shared_host_memory"
    if memory_model != expected_memory_model:
        fail(f"{label}.device.memory_model does not match the device class")
    driver = exact_keys(identity["driver"], {"kind", "value"}, f"{label}.driver")
    if json_string(driver["kind"], f"{label}.driver.kind", 16) != "exact":
        fail(f"{label}.driver.kind must be exact")
    driver_value = json_string(driver["value"], f"{label}.driver.value", 128)
    if DRIVER_IDENTITY_PATTERN.fullmatch(driver_value) is None:
        fail(f"{label}.driver.value is not a canonical Linux runtime driver identity")
    valid_binding = (
        backend == "cuda" and provider == "transcribe-cpp-ggml-cuda" and vendor == "nvidia"
    ) or (
        backend == "vulkan"
        and provider == "transcribe-cpp-ggml-vulkan"
        and vendor in {"nvidia", "amd", "intel"}
    )
    if not valid_binding:
        fail(f"{label} has an invalid backend, provider, and vendor binding")
    if gpu_worker["backend"] != backend or gpu_worker["provider_id"] != provider:
        fail(f"{label}.gpu_worker does not match the candidate backend and provider")
    if gpu_worker["runtime_abi"] != identity["pack"]["runtime_abi"]:
        fail(f"{label}.gpu_worker runtime ABI does not match the pack")
    expected_pack_id = f"scribe-{backend}-linux-x64"
    if identity["pack"]["pack_id"] != expected_pack_id:
        fail(f"{label}.pack.pack_id does not match the candidate backend")
    driver_prefixes = {
        "nvidia": ("linux:nvidia:",),
        "amd": ("linux:amdgpu:",),
        "intel": ("linux:i915:", "linux:xe:"),
    }
    if not driver_value.startswith(driver_prefixes[vendor]):
        fail(f"{label}.driver.value does not match the GPU vendor")
    if cpu_baseline["worker_sha256"] == gpu_worker["worker_sha256"]:
        fail(f"{label} CPU and GPU workers must be distinct admitted artifacts")
    if acquisition["host"]["total_memory_bytes"] < total_memory and memory_model == "shared_host_memory":
        fail(f"{label} shared GPU memory exceeds host memory")
    if backend == "cuda":
        cuda_driver = CUDA_DRIVER_PATTERN.fullmatch(driver_value)
        if cuda_driver is None or (int(cuda_driver.group(1)), int(cuda_driver.group(2))) < (570, 26):
            fail(f"{label}.driver.value is below the reviewed CUDA driver minimum")
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
    if len(plan["required_lanes"]) > MAX_LANES:
        fail("qualification plan exceeds the representative-lane bound")
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


def load_production_authority(repository_root: pathlib.Path) -> tuple[set[str], str]:
    authority, raw = load_canonical_json(
        repository_root / PRODUCTION_AUTHORITY_PATH,
        "Linux GPU qualification production authority",
    )
    exact_keys(
        authority,
        {"schema_version", "kind", "approved_plan_sha256"},
        "Linux GPU qualification production authority",
    )
    if json_integer(
        authority["schema_version"],
        "Linux GPU qualification production authority.schema_version",
        1,
        1,
    ) != 1:
        fail("Linux GPU qualification production authority schema is unsupported")
    if json_string(
        authority["kind"], "Linux GPU qualification production authority.kind"
    ) != "linux_gpu_qualification_production_authority":
        fail("Linux GPU qualification production authority kind is unsupported")
    values = authority["approved_plan_sha256"]
    if type(values) is not list:
        fail("Linux GPU qualification production authority approvals must be an array")
    approved: set[str] = set()
    previous = ""
    for index, value in enumerate(values):
        digest = sha256_value(
            value,
            f"Linux GPU qualification production authority approval {index}",
        )
        if digest <= previous:
            fail("Linux GPU qualification production approvals must be strictly sorted and unique")
        previous = digest
        approved.add(digest)
    return approved, hashlib.sha256(raw).hexdigest()


def load_auto_manifest_entries(repository_root: pathlib.Path) -> list[dict[str, Any]]:
    path = repository_root / CONTRACT_PATHS["auto_manifest_sha256"]
    try:
        raw = path.read_bytes()
        document = json.loads(raw.decode("utf-8"), object_pairs_hook=reject_duplicate_keys)
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as error:
        fail(f"could not parse the Linux Auto manifest: {error}")
    manifest = exact_keys(
        document,
        {"schema_version", "mode", "target_os", "target_arch", "entries"},
        "Linux Auto manifest",
    )
    if (
        json_integer(manifest["schema_version"], "Linux Auto manifest.schema_version", 1, 1) != 1
        or json_string(manifest["mode"], "Linux Auto manifest.mode") != "default_deny"
        or json_string(manifest["target_os"], "Linux Auto manifest.target_os") != "linux"
        or json_string(manifest["target_arch"], "Linux Auto manifest.target_arch") != "x86_64"
    ):
        fail("Linux Auto manifest platform or policy is unsupported")
    if type(manifest["entries"]) is not list:
        fail("Linux Auto manifest.entries must be an array")
    for index, entry in enumerate(manifest["entries"]):
        exact_keys(
            entry,
            {
                "pack",
                "model_digest",
                "backend",
                "provider_id",
                "vendor",
                "device_class",
                "minimum_total_memory_bytes",
                "driver",
                "evidence",
            },
            f"Linux Auto manifest entry {index}",
        )
    return manifest["entries"]


def validate_run(
    value: Any,
    label: str,
    expected_sequence: int,
    target: str,
    mode: str,
    identity: dict[str, Any],
    artifact_digests: set[str],
    artifact_paths: set[str],
    artifact_root: pathlib.Path,
    artifact_budget: dict[str, int],
    attestation_digests: set[str],
    descriptor_bound: bool,
) -> dict[str, Any]:
    run = exact_keys(
        value,
        {
            "sequence",
            "artifact_path",
            "artifact_sha256",
            "acquisition_batch_id",
            "machine_id_sha256",
            "session_id",
            "pair_id",
            "pair_order",
            "reset_state",
            "priming_runs",
            "execution",
            "outcome",
            "failure_category",
            "end_to_end_ms",
            "backend_ms",
            "peak_process_memory_bytes",
            "peak_vram_bytes",
            "peak_shared_device_memory_bytes",
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
    acquisition = identity["acquisition"]
    if identifier(run["acquisition_batch_id"], f"{label}.acquisition_batch_id") != acquisition["batch_id"]:
        fail(f"{label} is from a different acquisition batch")
    if sha256_value(run["machine_id_sha256"], f"{label}.machine_id_sha256") != acquisition["machine_id_sha256"]:
        fail(f"{label} is from a different machine")
    expected_session = (
        f"{acquisition['batch_id']}:{mode}:{expected_sequence:02}:session"
        if mode == "cold"
        else f"{acquisition['batch_id']}:warm:session"
    )
    expected_pair = f"{acquisition['batch_id']}:{mode}:{expected_sequence:02}"
    expected_order = "cpu_then_gpu" if expected_sequence % 2 else "gpu_then_cpu"
    if identifier(run["session_id"], f"{label}.session_id") != expected_session:
        fail(f"{label}.session_id violates acquisition protocol v1")
    if identifier(run["pair_id"], f"{label}.pair_id") != expected_pair:
        fail(f"{label}.pair_id violates acquisition protocol v1")
    if json_string(run["pair_order"], f"{label}.pair_order", 32) != expected_order:
        fail(f"{label}.pair_order violates acquisition protocol v1")
    expected_reset = "fresh_process_fresh_model" if mode == "cold" else "same_process_primed_model"
    if json_string(run["reset_state"], f"{label}.reset_state", 40) != expected_reset:
        fail(f"{label}.reset_state violates acquisition protocol v1")
    expected_priming = 0 if mode == "cold" else acquisition["ordering"]["warm_priming_runs"]
    if json_integer(run["priming_runs"], f"{label}.priming_runs", 0, 16) != expected_priming:
        fail(f"{label}.priming_runs violates acquisition protocol v1")
    validate_execution(
        run["execution"],
        f"{label}.execution",
        target,
        identity,
        mode,
        expected_sequence,
        attestation_digests,
    )
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
    shared_memory = json_integer(
        run["peak_shared_device_memory_bytes"],
        f"{label}.peak_shared_device_memory_bytes",
    )
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
    if outcome == "failure":
        pass
    elif target == "cpu":
        if vram != 0 or shared_memory != 0:
            fail(f"{label} CPU run must report zero GPU device memory")
    elif identity["device"]["memory_model"] == "dedicated_vram":
        if vram < MIN_GPU_PEAK_MEMORY_BYTES or vram > identity["device"]["total_memory_bytes"] or shared_memory != 0:
            fail(f"{label} discrete GPU run has implausible device-memory evidence")
    elif shared_memory < MIN_GPU_PEAK_MEMORY_BYTES or shared_memory > identity["device"]["total_memory_bytes"] or vram != 0:
        fail(f"{label} shared-memory GPU run has implausible device-memory evidence")
    record = {key: value for key, value in run.items() if key not in {"artifact_path", "artifact_sha256"}}
    validate_artifact_envelope(
        artifact_root,
        run["artifact_path"],
        artifact,
        "linux_gpu_qualification_run_artifact",
        record,
        label,
        artifact_paths,
        artifact_budget,
        descriptor_bound,
    )
    return run


def validate_event(
    value: Any,
    label: str,
    expected_event: str,
    identity: dict[str, Any],
    artifact_digests: set[str],
    artifact_paths: set[str],
    artifact_root: pathlib.Path,
    artifact_budget: dict[str, int],
    descriptor_bound: bool,
) -> dict[str, Any]:
    event = exact_keys(
        value,
        {
            "event",
            "artifact_path",
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
    record = {key: value for key, value in event.items() if key not in {"artifact_path", "artifact_sha256"}}
    validate_artifact_envelope(
        artifact_root,
        event["artifact_path"],
        artifact,
        "linux_gpu_qualification_lifecycle_artifact",
        record,
        label,
        artifact_paths,
        artifact_budget,
        descriptor_bound,
    )
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
        "peak_shared_device_memory_bytes",
    ):
        values = [run[field] for run in successful]
        result[field] = (
            {"p50": nearest_rank(values, 50), "p95": nearest_rank(values, 95)} if values else None
        )
    return result


def auto_entry_projection(
    identity: dict[str, Any],
    run_sets: dict[str, Any],
    parsed: dict[str, dict[str, list[dict[str, Any]]]],
) -> dict[str, Any]:
    cold_transcripts = {
        target: [run["transcript_sha256"] for run in parsed["cold"][target]]
        for target in ("cpu", "gpu")
    }
    warm_transcripts = {
        target: [run["transcript_sha256"] for run in parsed["warm"][target]]
        for target in ("cpu", "gpu")
    }
    return {
        "pack": identity["pack"],
        "model_digest": identity["model"]["model_digest"],
        "backend": identity["backend"],
        "provider_id": identity["provider_id"],
        "vendor": identity["device"]["vendor"],
        "device_class": identity["device"]["device_class"],
        "minimum_total_memory_bytes": identity["device"]["qualified_minimum_total_memory_bytes"],
        "driver": identity["driver"],
        "evidence": {
            "id": identity["lane_id"],
            "cold_runs": len(parsed["cold"]["cpu"]),
            "warm_runs": len(parsed["warm"]["cpu"]),
            "gpu_p95_ms": nearest_rank(
                [run["end_to_end_ms"] for run in parsed["warm"]["gpu"]], 95
            ),
            "cpu_p95_ms": nearest_rank(
                [run["end_to_end_ms"] for run in parsed["warm"]["cpu"]], 95
            ),
            "correctness_verified": True,
            "reliability_verified": True,
            "cold_evidence_sha256": canonical_digest(run_sets["cold"]),
            "warm_evidence_sha256": canonical_digest(run_sets["warm"]),
            "transcript_parity_evidence_sha256": canonical_digest(
                {
                    "expected": identity["workload"]["expected_transcript_sha256"],
                    "cold": cold_transcripts,
                    "warm": warm_transcripts,
                }
            ),
        },
    }


def validate_lane_evidence(
    value: Any,
    expected: dict[str, Any],
    plan: dict[str, Any],
    index: int,
    artifact_root: pathlib.Path,
    artifact_digests: set[str],
    artifact_paths: set[str],
    artifact_budget: dict[str, int],
    attestation_digests: set[str],
    descriptor_bound: bool,
) -> tuple[dict[str, Any], dict[str, Any]]:
    lane = exact_keys(
        value,
        {
            "identity",
            "acquisition_artifact_path",
            "acquisition_artifact_sha256",
            "run_sets",
            "lifecycle",
        },
        f"evidence lane {index}",
    )
    identity = validate_identity(lane["identity"], f"evidence lane {index}.identity")
    if identity != expected["identity"]:
        fail(f"evidence lane {index} identity does not match its reviewed plan")
    if canonical_digest(lane) != expected["evidence_sha256"]:
        fail(f"evidence lane {index} does not match its reviewed evidence digest")
    run_sets = exact_keys(lane["run_sets"], {"cold", "warm"}, f"evidence lane {index}.run_sets")
    acquisition_digest = sha256_value(
        lane["acquisition_artifact_sha256"],
        f"evidence lane {index}.acquisition_artifact_sha256",
    )
    if acquisition_digest in artifact_digests:
        fail("qualification evidence reuses an artifact digest")
    artifact_digests.add(acquisition_digest)
    validate_artifact_envelope(
        artifact_root,
        lane["acquisition_artifact_path"],
        acquisition_digest,
        "linux_gpu_qualification_acquisition_artifact",
        identity["acquisition"],
        f"evidence lane {index}.acquisition",
        artifact_paths,
        artifact_budget,
        descriptor_bound,
    )
    parsed: dict[str, dict[str, list[dict[str, Any]]]] = {}
    for mode, expected_count in (("cold", plan["cold_runs"]), ("warm", plan["warm_runs"])):
        target_sets = exact_keys(run_sets[mode], {"cpu", "gpu"}, f"evidence lane {index}.{mode}")
        parsed[mode] = {}
        for target in ("cpu", "gpu"):
            raw_runs = target_sets[target]
            if type(raw_runs) is not list or len(raw_runs) != expected_count:
                fail(f"evidence lane {index}.{mode}.{target} has the wrong run count")
            parsed[mode][target] = [
                validate_run(
                    run,
                    f"evidence lane {index}.{mode}.{target}[{offset}]",
                    offset + 1,
                    target,
                    mode,
                    identity,
                    artifact_digests,
                    artifact_paths,
                    artifact_root,
                    artifact_budget,
                    attestation_digests,
                    descriptor_bound,
                )
                for offset, run in enumerate(raw_runs)
            ]
    if type(lane["lifecycle"]) is not list or len(lane["lifecycle"]) != len(REQUIRED_EVENTS):
        fail(f"evidence lane {index}.lifecycle is incomplete")
    events = [
        validate_event(
            event,
            f"evidence lane {index}.lifecycle[{offset}]",
            expected_event,
            identity,
            artifact_digests,
            artifact_paths,
            artifact_root,
            artifact_budget,
            descriptor_bound,
        )
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
    performance_passed = all_successful and all(
        nearest_rank([run["end_to_end_ms"] for run in parsed[mode]["gpu"]], 95) * 100
        <= nearest_rank([run["end_to_end_ms"] for run in parsed[mode]["cpu"]], 95)
        * plan["maximum_gpu_p95_cpu_percent"]
        for mode in ("cold", "warm")
    )
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
        "auto_entry_projection": auto_entry_projection(identity, run_sets, parsed) if passed else None,
    }
    return lane, summary


def decide(
    plan: dict[str, Any],
    plan_raw: bytes,
    evidence: dict[str, Any],
    repository_root: pathlib.Path,
    allow_fixture: bool,
    artifact_root: pathlib.Path | None,
) -> dict[str, Any]:
    required_lanes = validate_plan(plan, repository_root)
    approved_plans, production_authority_digest = load_production_authority(repository_root)
    auto_manifest_entries = load_auto_manifest_entries(repository_root)
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
    if not fixture_only and plan_digest not in approved_plans:
        fail("qualification plan is not approved by the protected production authority")
    if not fixture_only and sys.platform != "linux":
        fail("production qualification evaluation is supported only on Linux")
    if type(evidence["lanes"]) is not list:
        fail("qualification evidence.lanes must be an array")
    if len(evidence["lanes"]) > MAX_LANES:
        fail("qualification evidence exceeds the representative-lane bound")
    if len(evidence["lanes"]) != len(required_lanes):
        fail("qualification evidence does not cover every representative lane")
    if required_lanes:
        if artifact_root is None:
            fail("nonempty qualification evidence requires --artifact-root")
        try:
            root_stat = artifact_root.lstat()
        except OSError as error:
            fail(f"could not inspect artifact root: {error}")
        if stat.S_ISLNK(root_stat.st_mode) or not stat.S_ISDIR(root_stat.st_mode):
            fail("artifact root must be a regular non-symlink directory")
        artifact_root = artifact_root.resolve()
    artifact_digests: set[str] = set()
    artifact_paths: set[str] = set()
    attestation_digests: set[str] = set()
    artifact_budget = {"count": 0, "bytes": 0}
    descriptor_bound = not fixture_only
    summaries = [
        validate_lane_evidence(
            lane,
            expected,
            plan,
            index,
            artifact_root,
            artifact_digests,
            artifact_paths,
            artifact_budget,
            attestation_digests,
            descriptor_bound,
        )[1]
        for index, (lane, expected) in enumerate(zip(evidence["lanes"], required_lanes))
    ]
    evidence_complete = bool(required_lanes) and len(summaries) == len(required_lanes)
    qualification_passed = evidence_complete and all(summary["qualification_passed"] for summary in summaries)
    projections = [
        summary["auto_entry_projection"]
        for summary in summaries
        if summary["auto_entry_projection"] is not None
    ]
    projection_keys = [json.dumps(value, sort_keys=True, separators=(",", ":")) for value in projections]
    manifest_keys = [
        json.dumps(value, sort_keys=True, separators=(",", ":")) for value in auto_manifest_entries
    ]
    activation_manifest_complete = (
        qualification_passed
        and bool(projection_keys)
        and len(set(projection_keys)) == len(projection_keys)
        and sorted(projection_keys) == sorted(manifest_keys)
    )
    auto_eligible = qualification_passed and activation_manifest_complete and not fixture_only
    if fixture_only:
        reason = "fixture_only_never_auto_eligible"
    elif not evidence_complete:
        reason = "no_complete_representative_evidence"
    elif not qualification_passed:
        reason = "one_or_more_representative_lanes_failed"
    elif not activation_manifest_complete:
        reason = "exact_one_to_one_auto_projection_missing"
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
        "artifact_count": artifact_budget["count"],
        "artifact_bytes": artifact_budget["bytes"],
        "activation_manifest_complete": activation_manifest_complete,
        "lanes": summaries,
        "plan_sha256": plan_digest,
        "production_authority_sha256": production_authority_digest,
        "qualification_passed": qualification_passed,
    }


def parse_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--plan", required=True, type=pathlib.Path)
    parser.add_argument("--evidence", required=True, type=pathlib.Path)
    parser.add_argument("--artifact-root", type=pathlib.Path)
    parser.add_argument("--output", type=pathlib.Path)
    parser.add_argument("--allow-fixture", action="store_true")
    parser.add_argument("--require-eligible", action="store_true")
    return parser.parse_args()


def write_new_file(path: pathlib.Path, payload: bytes) -> None:
    if not path.name or path.name in {".", ".."}:
        fail("output path is invalid")
    try:
        parent = path.parent.resolve(strict=True)
        parent_stat = parent.lstat()
    except OSError as error:
        fail(f"output parent must already exist: {error}")
    if stat.S_ISLNK(parent_stat.st_mode) or not stat.S_ISDIR(parent_stat.st_mode):
        fail("output parent must be a regular non-symlink directory")
    destination = parent / path.name
    temporary = parent / f".{path.name}.{os.getpid()}.tmp"
    try:
        with temporary.open("xb") as output:
            output.write(payload)
            output.flush()
            os.fsync(output.fileno())
        try:
            os.link(temporary, destination)
        except FileExistsError:
            fail("output path already exists")
        if sys.platform == "linux":
            directory_descriptor = os.open(parent, os.O_RDONLY | os.O_CLOEXEC | os.O_DIRECTORY)
            try:
                os.fsync(directory_descriptor)
            finally:
                os.close(directory_descriptor)
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
        decision = decide(
            plan,
            plan_raw,
            evidence,
            repository_root,
            arguments.allow_fixture,
            arguments.artifact_root,
        )
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
