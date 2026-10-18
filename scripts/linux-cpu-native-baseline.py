#!/usr/bin/env python3
"""Validate the native build evidence for Scribe's portable Linux CPU worker."""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import posixpath
import re
import shlex
import stat
import sys
from collections.abc import Mapping, Sequence
from typing import Any


MAX_CACHE_BYTES = 4 * 1024 * 1024
MAX_COMPILE_DATABASE_BYTES = 64 * 1024 * 1024
MAX_COMPILE_RECORDS = 20_000
MAX_ARGUMENTS = 4_096
MAX_ARGUMENT_BYTES = 32 * 1024

EXPECTED_CACHE_VALUES = {
    "BUILD_SHARED_LIBS": "OFF",
    "CMAKE_BUILD_TYPE": "Release",
    "CMAKE_EXPORT_COMPILE_COMMANDS": "ON",
    "GGML_AMX_BF16": "OFF",
    "GGML_AMX_INT8": "OFF",
    "GGML_AMX_TILE": "OFF",
    "GGML_AVX": "OFF",
    "GGML_AVX2": "OFF",
    "GGML_AVX512": "OFF",
    "GGML_AVX512_BF16": "OFF",
    "GGML_AVX512_VBMI": "OFF",
    "GGML_AVX512_VNNI": "OFF",
    "GGML_AVX_VNNI": "OFF",
    "GGML_BACKEND_DL": "OFF",
    "GGML_BLAS": "OFF",
    "GGML_BMI2": "OFF",
    "GGML_CCACHE": "OFF",
    "GGML_CPU": "ON",
    "GGML_CPU_ALL_VARIANTS": "OFF",
    "GGML_CUDA": "OFF",
    "GGML_F16C": "OFF",
    "GGML_FMA": "OFF",
    "GGML_METAL": "OFF",
    "GGML_NATIVE": "OFF",
    "GGML_OPENMP": "OFF",
    "GGML_SSE42": "OFF",
    "GGML_VULKAN": "OFF",
    "TRANSCRIBE_BUILD_SHARED": "OFF",
    "TRANSCRIBE_CUDA": "OFF",
    "TRANSCRIBE_GGML_BACKEND_DL": "OFF",
    "TRANSCRIBE_METAL": "OFF",
    "TRANSCRIBE_USE_OPENMP": "OFF",
    "TRANSCRIBE_USE_SYSTEM_BLAS": "OFF",
    "TRANSCRIBE_VULKAN": "OFF",
    "TRANSCRIBE_X86_CONSERVATIVE": "ON",
}

EXPECTED_CACHE_KINDS = {
    name: "STRING" if name == "CMAKE_BUILD_TYPE" else "BOOL"
    for name in EXPECTED_CACHE_VALUES
}

REQUIRED_COMPILE_RECORDS = {
    ("transcribe", "src/transcribe.cpp"),
    ("ggml-base", "ggml/src/ggml.c"),
    ("ggml", "ggml/src/ggml-backend-reg.cpp"),
    ("ggml-cpu", "ggml/src/ggml-cpu/ggml-cpu.c"),
    ("ggml-cpu", "ggml/src/ggml-cpu/arch/x86/quants.c"),
    ("ggml-cpu", "ggml/src/ggml-cpu/arch/x86/repack.cpp"),
}

_CACHE_ENTRY = re.compile(r"^([A-Za-z0-9_.+-]+):([A-Za-z]+)=(.*)$")
_TARGET_RUSTFLAGS = re.compile(r"^CARGO_TARGET_[A-Z0-9_]+_RUSTFLAGS$")
_TARGET_BUILD_OVERRIDE = re.compile(r"^CARGO_TARGET_[A-Z0-9_]+_(?:LINKER|RUNNER)$")
_COMPILER_OVERRIDE = re.compile(
    r"^(?:(?:HOST|TARGET)_)?(?:CC|CXX|CPP|AR|RANLIB|NM|STRIP|LD)(?:_.+)?$"
)
_COMPILER_FLAGS = re.compile(
    r"^(?:(?:HOST|TARGET)_)?(?:CFLAGS|CXXFLAGS|CPPFLAGS|LDFLAGS)(?:_.+)?$"
)
_PREFIXED_COMPILER_OVERRIDE = re.compile(
    r"^(?:[A-Z0-9]+_){2,}(?:CC|CXX|CPP|AR|RANLIB|NM|STRIP|LD)$"
)
_FORBIDDEN_MACRO = re.compile(
    r"^(?:"
    r"GGML_(?:NATIVE|SSE42|AVX(?:_VNNI)?|AVX2|BMI2|FMA|F16C|"
    r"AVX512(?:_VBMI|_VNNI|_BF16)?|AMX_(?:TILE|INT8|BF16))|"
    r"__[A-Z0-9_]+__"
    r")$",
    re.IGNORECASE,
)


class BaselineError(ValueError):
    """The supplied environment or native build evidence is not admissible."""


def fail(message: str) -> None:
    raise BaselineError(message)


def validate_environment(environment: Mapping[str, str]) -> None:
    """Reject ambient inputs that can retarget or wrap the native CPU build."""

    exact = {
        "BINDGEN_EXTRA_CLANG_ARGS",
        "CARGO_BUILD_RUSTFLAGS",
        "CARGO_BUILD_RUSTC",
        "CARGO_BUILD_RUSTC_WRAPPER",
        "CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER",
        "CARGO_BUILD_TARGET",
        "CARGO_ENCODED_RUSTFLAGS",
        "CARGO_TARGET_DIR",
        "CMAKE_ARGS",
        "CC_KNOWN_WRAPPER_CUSTOM",
        "CC_SHELL_ESCAPED_FLAGS",
        "CPATH",
        "COMPILER_PATH",
        "CRATE_CC_NO_DEFAULTS",
        "CROSS_COMPILE",
        "CPLUS_INCLUDE_PATH",
        "C_INCLUDE_PATH",
        "GCC_EXEC_PREFIX",
        "LIBRARY_PATH",
        "RUSTC",
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
        "RUSTFLAGS",
        "TRANSCRIBE_CMAKE_ARGS",
    }
    for original_name in environment:
        name = original_name.upper().replace("-", "_")
        if (
            name in exact
            or name == "CMAKE"
            or name.startswith(("CMAKE_", "HOST_CMAKE", "TARGET_CMAKE"))
            or _TARGET_RUSTFLAGS.fullmatch(name)
            or _TARGET_BUILD_OVERRIDE.fullmatch(name)
            or _COMPILER_OVERRIDE.fullmatch(name)
            or _PREFIXED_COMPILER_OVERRIDE.fullmatch(name)
            or _COMPILER_FLAGS.fullmatch(name)
        ):
            fail(f"Linux CPU baseline does not accept ambient build override: {original_name}")


def _read_regular_file(path: pathlib.Path, label: str, maximum: int) -> bytes:
    flags = (
        os.O_RDONLY
        | getattr(os, "O_CLOEXEC", 0)
        | getattr(os, "O_BINARY", 0)
        | getattr(os, "O_NONBLOCK", 0)
    )
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    elif path.is_symlink():
        fail(f"{label} must not be a symbolic link")
    descriptor = -1
    try:
        descriptor = os.open(path, flags)
        before = os.fstat(descriptor)
        if not stat.S_ISREG(before.st_mode):
            fail(f"{label} must be a regular file")
        if before.st_nlink != 1:
            fail(f"{label} must have exactly one hard link")
        if before.st_size <= 0 or before.st_size > maximum:
            fail(f"{label} is empty or exceeds its size bound")
        chunks: list[bytes] = []
        remaining = before.st_size
        while remaining:
            chunk = os.read(descriptor, min(remaining, 1024 * 1024))
            if not chunk:
                fail(f"{label} ended before its recorded length")
            chunks.append(chunk)
            remaining -= len(chunk)
        if os.read(descriptor, 1):
            fail(f"{label} grew while it was read")
        after = os.fstat(descriptor)
        if (
            before.st_dev,
            before.st_ino,
            before.st_size,
            before.st_mtime_ns,
        ) != (
            after.st_dev,
            after.st_ino,
            after.st_size,
            after.st_mtime_ns,
        ):
            fail(f"{label} changed while it was read")
        return b"".join(chunks)
    except OSError as error:
        fail(f"could not descriptor-read {label}: {error}")
    finally:
        if descriptor >= 0:
            os.close(descriptor)


def _decode_utf8(data: bytes, label: str) -> str:
    try:
        text = data.decode("utf-8", errors="strict")
    except UnicodeDecodeError as error:
        fail(f"{label} is not strict UTF-8: {error}")
    if text.startswith("\ufeff") or "\x00" in text:
        fail(f"{label} contains a forbidden byte-order mark or NUL")
    return text


def parse_cmake_cache(data: bytes) -> dict[str, tuple[str, str]]:
    cache: dict[str, tuple[str, str]] = {}
    for line_number, line in enumerate(_decode_utf8(data, "CMake cache").splitlines(), 1):
        if not line or line.startswith("#") or line.startswith("//"):
            continue
        matched = _CACHE_ENTRY.fullmatch(line)
        if matched is None:
            continue
        name, kind, value = matched.groups()
        if name in cache:
            fail(f"CMake cache contains duplicate entry {name!r} on line {line_number}")
        cache[name] = (kind, value)
    return cache


def _cache_value(cache: Mapping[str, tuple[str, str]], name: str) -> str:
    entry = cache.get(name)
    if entry is None:
        fail(f"CMake cache is missing required entry {name}")
    return entry[1]


def _unsafe_option(arguments: Sequence[str]) -> str | None:
    index = 1
    while index < len(arguments):
        argument = arguments[index]
        if argument.startswith("@"):
            return argument
        if argument in {
            "--config",
            "--gcc-toolchain",
            "--specs",
            "--sysroot",
            "--target",
            "-B",
            "-Xclang",
            "-Xassembler",
            "-fplugin",
            "-imacros",
            "-include",
            "-isysroot",
            "-mllvm",
            "-specs",
            "-target",
            "-wrapper",
            "-x",
        }:
            return argument
        if argument.startswith(
            (
                "--config=",
                "--gcc-toolchain=",
                "--specs=",
                "--sysroot=",
                "-B",
                "-Wa,",
                "-Wp,",
                "-Xassembler=",
                "-fplugin",
                "-imacros",
                "-include",
                "-isysroot",
                "-specs=",
                "-target=",
                "-wrapper=",
            )
        ):
            return argument
        if argument.startswith(("--target=", "-target-cpu", "-target-feature", "-x")):
            return argument
        # The pinned GNU x64 recipe needs only -m64. An ISA blacklist misses
        # extensions (or -m32); require review before allowing another -m flag.
        if argument.startswith("-m") and argument != "-m64":
            return argument
        macro: str | None = None
        if argument in {"-D", "-U"}:
            if index + 1 >= len(arguments):
                return argument
            macro = arguments[index + 1]
            index += 1
        elif argument.startswith(("-D", "-U")) and len(argument) > 2:
            macro = argument[2:]
        if macro is not None:
            macro_name = macro.split("=", 1)[0]
            # This recipe never manually defines compiler-reserved macros;
            # those can retarget conditional source without an ISA option.
            if _FORBIDDEN_MACRO.fullmatch(macro_name):
                return argument
        index += 1
    return None


def _validate_cache(cache: Mapping[str, tuple[str, str]]) -> tuple[str, str]:
    for name, expected in EXPECTED_CACHE_VALUES.items():
        kind, actual = cache.get(name, ("", ""))
        if not kind:
            fail(f"CMake cache is missing required entry {name}")
        expected_kind = EXPECTED_CACHE_KINDS[name]
        if kind != expected_kind:
            fail(f"CMake cache requires {name}:{expected_kind}; found type {kind!r}")
        if actual != expected:
            fail(f"CMake cache requires {name}={expected}; found {actual!r}")
    for name in ("CMAKE_C_FLAGS", "CMAKE_CXX_FLAGS", "CMAKE_C_FLAGS_RELEASE", "CMAKE_CXX_FLAGS_RELEASE"):
        value = _cache_value(cache, name)
        try:
            arguments = ["compiler", *shlex.split(value, posix=True)]
        except ValueError as error:
            fail(f"CMake cache {name} is not a parseable compiler flag list: {error}")
        unsafe = _unsafe_option(arguments)
        if unsafe is not None:
            fail(f"CMake cache {name} contains forbidden ISA or indirection option {unsafe!r}")
    for name in (
        "CMAKE_C_COMPILER_LAUNCHER",
        "CMAKE_CXX_COMPILER_LAUNCHER",
        "CMAKE_RULE_LAUNCH_COMPILE",
        "CMAKE_TOOLCHAIN_FILE",
    ):
        if name in cache and cache[name][1].strip():
            fail(f"CMake cache contains forbidden build indirection {name}")
    c_compiler = _cache_value(cache, "CMAKE_C_COMPILER")
    cxx_compiler = _cache_value(cache, "CMAKE_CXX_COMPILER")
    if not c_compiler or not cxx_compiler or any(character.isspace() for character in c_compiler + cxx_compiler):
        fail("CMake cache compiler identities must be nonempty paths without whitespace")
    return c_compiler, cxx_compiler


def _reject_duplicate_json_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            fail(f"compile database object contains duplicate field {key!r}")
        result[key] = value
    return result


def _parse_compile_database(data: bytes) -> list[dict[str, Any]]:
    try:
        value = json.loads(
            _decode_utf8(data, "compile database"), object_pairs_hook=_reject_duplicate_json_keys
        )
    except (json.JSONDecodeError, TypeError) as error:
        fail(f"compile database is not valid JSON: {error}")
    if type(value) is not list or not value or len(value) > MAX_COMPILE_RECORDS:
        fail("compile database must be a nonempty bounded array")
    if any(type(record) is not dict for record in value):
        fail("compile database entries must be objects")
    return value


def _path_within(path: pathlib.Path, root: pathlib.Path, label: str) -> None:
    try:
        path.relative_to(root)
    except ValueError:
        fail(f"{label} resolves outside its required root")


def _resolve_existing(raw: str, base: pathlib.Path, label: str) -> pathlib.Path:
    if not raw or "\x00" in raw:
        fail(f"{label} must be a nonempty path")
    path = pathlib.Path(raw)
    if not path.is_absolute():
        path = base / path
    try:
        return path.resolve(strict=True)
    except (OSError, RuntimeError) as error:
        fail(f"{label} does not resolve to an existing path: {error}")


def _resolve_output(raw: str, directory: pathlib.Path, build_root: pathlib.Path) -> pathlib.Path:
    if not raw or "\x00" in raw:
        fail("compile output must be a nonempty path")
    path = pathlib.Path(raw)
    if not path.is_absolute():
        path = directory / path
    resolved = path.resolve(strict=False)
    _path_within(resolved, build_root, "compile output")
    return resolved


def _compiler_lexical(path: str) -> str:
    normalized = posixpath.normpath(path.replace("\\", "/"))
    if re.match(r"^[A-Za-z]:/", normalized):
        normalized = normalized.casefold()
    return normalized


def _compiler_identity(path: str) -> tuple[int, int]:
    try:
        metadata = os.stat(path)
    except OSError as error:
        fail(f"cached compiler path is unavailable: {path}: {error}")
    if not stat.S_ISREG(metadata.st_mode) or metadata.st_mode & 0o111 == 0:
        fail(f"cached compiler is not a regular executable: {path}")
    return metadata.st_dev, metadata.st_ino


def _validate_compiler(actual: str, cached: str, label: str) -> None:
    if _compiler_lexical(actual) == _compiler_lexical(cached):
        if sys.platform == "linux":
            _compiler_identity(cached)
        return
    if sys.platform != "linux" or _compiler_identity(actual) != _compiler_identity(cached):
        fail(f"{label} command does not use the compiler retained by CMake")


def _arguments(record: Mapping[str, Any], index: int) -> list[str]:
    has_arguments = "arguments" in record
    has_command = "command" in record
    if has_arguments == has_command:
        fail(f"compile database entry {index} must contain exactly one of arguments or command")
    if has_arguments:
        raw = record["arguments"]
        if type(raw) is not list or not raw or len(raw) > MAX_ARGUMENTS:
            fail(f"compile database entry {index} has an invalid arguments array")
        if any(
            type(argument) is not str
            or not argument
            or len(argument) > MAX_ARGUMENT_BYTES
            or any(ord(character) < 32 or ord(character) == 127 for character in argument)
            for argument in raw
        ):
            fail(f"compile database entry {index} has an invalid bounded argument")
        return list(raw)
    command = record["command"]
    if type(command) is not str or not command or len(command) > MAX_ARGUMENT_BYTES * 8:
        fail(f"compile database entry {index} has an invalid command string")
    if any(ord(character) < 32 or ord(character) == 127 for character in command) or any(
        token in command for token in ("`", "$", "|", "&", ";", "<", ">")
    ):
        fail(f"compile database entry {index} command contains shell indirection")
    try:
        parsed = shlex.split(command, posix=True)
    except ValueError as error:
        fail(f"compile database entry {index} command is not parseable: {error}")
    if not parsed or len(parsed) > MAX_ARGUMENTS or any(len(argument) > MAX_ARGUMENT_BYTES for argument in parsed):
        fail(f"compile database entry {index} command is not bounded")
    return parsed


def _single_operand(arguments: Sequence[str], option: str, label: str) -> str:
    operands: list[str] = []
    for index, argument in enumerate(arguments):
        if argument == option:
            if index + 1 >= len(arguments):
                fail(f"{label} is missing its operand")
            operands.append(arguments[index + 1])
    if len(operands) != 1:
        fail(f"compile command must contain exactly one {label}")
    return operands[0]


def _target_from_output(output: pathlib.Path, build_root: pathlib.Path) -> str:
    relative = output.relative_to(build_root)
    parts = relative.parts
    targets = [
        parts[index + 1][:-4]
        for index, part in enumerate(parts[:-1])
        if part == "CMakeFiles" and parts[index + 1].endswith(".dir")
    ]
    if len(targets) != 1 or not targets[0]:
        fail("compile output does not identify exactly one CMake target")
    return targets[0]


def _validate_compile_database(
    records: Sequence[Mapping[str, Any]],
    build_root: pathlib.Path,
    source_root: pathlib.Path,
    c_compiler: str,
    cxx_compiler: str,
) -> None:
    observed: set[tuple[str, str]] = set()
    observed_outputs: set[pathlib.Path] = set()
    source_languages = {".c": (c_compiler, "C"), ".cc": (cxx_compiler, "C++"), ".cpp": (cxx_compiler, "C++"), ".cxx": (cxx_compiler, "C++")}
    for index, record in enumerate(records):
        allowed = {"directory", "file", "arguments", "command", "output"}
        if not set(record).issubset(allowed):
            fail(f"compile database entry {index} contains an unexpected field")
        directory_raw = record.get("directory")
        source_raw = record.get("file")
        if type(directory_raw) is not str or type(source_raw) is not str:
            fail(f"compile database entry {index} requires string directory and file fields")
        directory = _resolve_existing(directory_raw, build_root, f"compile entry {index} directory")
        if not directory.is_dir():
            fail(f"compile entry {index} directory is not a directory")
        _path_within(directory, build_root, f"compile entry {index} directory")
        source = _resolve_existing(source_raw, directory, f"compile entry {index} source")
        if not source.is_file():
            fail(f"compile entry {index} source is not a file")
        _path_within(source, source_root, f"compile entry {index} source")
        language = source_languages.get(source.suffix.lower())
        if language is None:
            fail(f"compile entry {index} has an unexpected source language")
        arguments = _arguments(record, index)
        _validate_compiler(arguments[0], language[0], language[1])
        unsafe = _unsafe_option(arguments)
        if unsafe is not None:
            fail(f"compile entry {index} contains forbidden ISA or indirection option {unsafe!r}")
        compiled_source = _resolve_existing(
            _single_operand(arguments, "-c", "-c source operand"),
            directory,
            f"compile entry {index} -c source",
        )
        if compiled_source != source:
            fail(f"compile entry {index} file and -c source differ")
        output = _resolve_output(
            _single_operand(arguments, "-o", "-o output operand"), directory, build_root
        )
        if "output" in record:
            output_field = record["output"]
            if type(output_field) is not str:
                fail(f"compile database entry {index} output must be a string")
            if _resolve_output(output_field, directory, build_root) != output:
                fail(f"compile database entry {index} output disagrees with -o")
        target = _target_from_output(output, build_root)
        relative_source = source.relative_to(source_root).as_posix()
        target_source = (target, relative_source)
        if target_source in observed:
            fail(
                "compile database contains duplicate target/source evidence: "
                f"{target}:{relative_source}"
            )
        if output in observed_outputs:
            fail(f"compile database reuses output path {output}")
        observed.add(target_source)
        observed_outputs.add(output)
    missing = sorted(REQUIRED_COMPILE_RECORDS - observed)
    if missing:
        rendered = ", ".join(f"{target}:{source}" for target, source in missing)
        fail(f"compile database is missing required native target/source coverage: {rendered}")


def verify_build_root(build_root_input: os.PathLike[str] | str) -> None:
    lexical_build_root = pathlib.Path(build_root_input)
    if lexical_build_root.is_symlink():
        fail("native build root must not be a symbolic link")
    try:
        build_root = lexical_build_root.resolve(strict=True)
    except (OSError, RuntimeError) as error:
        fail(f"native build root does not exist: {error}")
    if not build_root.is_dir():
        fail("native build root must be a real directory")
    cache = parse_cmake_cache(
        _read_regular_file(build_root / "CMakeCache.txt", "CMake cache", MAX_CACHE_BYTES)
    )
    c_compiler, cxx_compiler = _validate_cache(cache)
    source_root_raw = _cache_value(cache, "CMAKE_HOME_DIRECTORY")
    source_root = _resolve_existing(source_root_raw, build_root, "CMake source root")
    if not source_root.is_dir():
        fail("CMake source root is not a directory")
    records = _parse_compile_database(
        _read_regular_file(
            build_root / "compile_commands.json",
            "compile database",
            MAX_COMPILE_DATABASE_BYTES,
        )
    )
    _validate_compile_database(records, build_root, source_root, c_compiler, cxx_compiler)


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    subparsers.add_parser("check-environment", help="reject ambient build and ISA overrides")
    verify = subparsers.add_parser("verify", help="verify an actual native CMake build tree")
    verify.add_argument("--build-root", required=True, type=pathlib.Path)
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    arguments = _parser().parse_args(argv)
    try:
        if arguments.command == "check-environment":
            validate_environment(os.environ)
        else:
            verify_build_root(arguments.build_root)
    except BaselineError as error:
        print(f"Linux CPU native baseline rejected: {error}", file=sys.stderr)
        return 1
    print("Linux CPU native baseline verified.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
