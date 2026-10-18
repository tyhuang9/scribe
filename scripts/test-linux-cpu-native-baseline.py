#!/usr/bin/env python3
"""Portable fixture tests for the Linux native CPU build-evidence gate."""

from __future__ import annotations

import copy
import importlib.util
import json
import os
import pathlib
import shlex
import sys
import tempfile
import unittest
from typing import Any


SCRIPT_ROOT = pathlib.Path(__file__).resolve().parent
TOOL_PATH = SCRIPT_ROOT / "linux-cpu-native-baseline.py"
EXPECTED_TEST_COUNT = 22

spec = importlib.util.spec_from_file_location("linux_cpu_native_baseline", TOOL_PATH)
assert spec is not None and spec.loader is not None
baseline = importlib.util.module_from_spec(spec)
spec.loader.exec_module(baseline)


class BaselineFixture:
    def __init__(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="scribe-linux-cpu-baseline-")
        self.root = pathlib.Path(self.temporary.name)
        self.source_root = self.root / "source"
        self.build_root = self.root / "build"
        self.source_root.mkdir()
        self.build_root.mkdir()
        self.c_compiler = "/usr/bin/cc"
        self.cxx_compiler = "/usr/bin/c++"
        self.cache: dict[str, tuple[str, str]] = {
            name: (baseline.EXPECTED_CACHE_KINDS[name], value)
            for name, value in baseline.EXPECTED_CACHE_VALUES.items()
        }
        self.cache.update(
            {
                "CMAKE_C_COMPILER": ("FILEPATH", self.c_compiler),
                "CMAKE_CXX_COMPILER": ("FILEPATH", self.cxx_compiler),
                "CMAKE_C_FLAGS": ("STRING", ""),
                "CMAKE_CXX_FLAGS": ("STRING", ""),
                "CMAKE_C_FLAGS_RELEASE": ("STRING", "-O3 -DNDEBUG"),
                "CMAKE_CXX_FLAGS_RELEASE": ("STRING", "-O3 -DNDEBUG"),
                "CMAKE_HOME_DIRECTORY": ("INTERNAL", str(self.source_root)),
            }
        )
        self.records: list[dict[str, Any]] = []
        self.records_by_key: dict[tuple[str, str], dict[str, Any]] = {}
        for sequence, (target, relative_source) in enumerate(
            sorted(baseline.REQUIRED_COMPILE_RECORDS)
        ):
            source = self.source_root / pathlib.PurePosixPath(relative_source)
            source.parent.mkdir(parents=True, exist_ok=True)
            source.write_text("/* portable fixture */\n", encoding="utf-8")
            output = (
                self.build_root
                / "CMakeFiles"
                / f"{target}.dir"
                / f"fixture-{sequence}.o"
            )
            output.parent.mkdir(parents=True, exist_ok=True)
            compiler = self.c_compiler if source.suffix == ".c" else self.cxx_compiler
            arguments = [
                compiler,
                "-O3",
                "-DNDEBUG",
                "-m64",
                "-fPIC",
                "-ffunction-sections",
                "-fdata-sections",
                "-c",
                str(source),
                "-o",
                str(output),
            ]
            record: dict[str, Any] = {
                "directory": str(self.build_root),
                "file": str(source),
            }
            if sequence == 0:
                record["command"] = shlex.join(arguments)
            else:
                record["arguments"] = arguments
                record["output"] = str(output)
            self.records.append(record)
            self.records_by_key[(target, relative_source)] = record
        self.write()

    def close(self) -> None:
        self.temporary.cleanup()

    def write_cache(self) -> None:
        lines = [
            f"{name}:{kind}={value}"
            for name, (kind, value) in sorted(self.cache.items())
        ]
        (self.build_root / "CMakeCache.txt").write_text(
            "\n".join(lines) + "\n", encoding="utf-8"
        )

    def write_database(self) -> None:
        (self.build_root / "compile_commands.json").write_text(
            json.dumps(self.records, separators=(",", ":")) + "\n",
            encoding="utf-8",
        )

    def write(self) -> None:
        self.write_cache()
        self.write_database()

    def verify(self) -> None:
        self.write()
        baseline.verify_build_root(self.build_root)


class LinuxCpuNativeBaselineTests(unittest.TestCase):
    def fixture(self) -> BaselineFixture:
        fixture = BaselineFixture()
        self.addCleanup(fixture.close)
        return fixture

    def assert_rejected(self, fixture: BaselineFixture, pattern: str) -> None:
        fixture.write()
        with self.assertRaisesRegex(baseline.BaselineError, pattern):
            baseline.verify_build_root(fixture.build_root)

    def test_safe_profile_accepts_arguments_command_and_optional_output(self) -> None:
        fixture = self.fixture()

        fixture.verify()

    def test_environment_rejects_present_empty_override_aliases(self) -> None:
        baseline.validate_environment({"PATH": "/usr/bin", "HOME": "/tmp/home"})
        overrides = (
            "RUSTFLAGS",
            "CARGO_ENCODED_RUSTFLAGS",
            "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS",
            "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER",
            "RUSTC_WRAPPER",
            "CARGO_BUILD_RUSTC",
            "GCC_EXEC_PREFIX",
            "COMPILER_PATH",
            "CC",
            "HOST_CXX",
            "TARGET_X86_64_UNKNOWN_LINUX_GNU_CC",
            "CC_KNOWN_WRAPPER_CUSTOM",
            "CRATE_CC_NO_DEFAULTS",
            "CC_SHELL_ESCAPED_FLAGS",
            "CROSS_COMPILE",
            "CMAKE",
            "CMAKE_TOOLCHAIN_FILE",
            "CMAKE_X86_64_UNKNOWN_LINUX_GNU_TOOLCHAIN_FILE",
            "HOST_CMAKE_GENERATOR",
            "TARGET_CMAKE_PREFIX_PATH",
        )
        for name in overrides:
            with self.subTest(name=name), self.assertRaisesRegex(
                baseline.BaselineError, "ambient build override"
            ):
                baseline.validate_environment({name: ""})

    def test_cache_requires_exact_profile_values_and_types(self) -> None:
        mutations = (
            ("TRANSCRIBE_X86_CONSERVATIVE", "BOOL", "OFF"),
            ("GGML_NATIVE", "BOOL", "ON"),
            ("BUILD_SHARED_LIBS", "BOOL", "ON"),
            ("GGML_BLAS", "BOOL", "ON"),
            ("GGML_CCACHE", "BOOL", "ON"),
            ("CMAKE_EXPORT_COMPILE_COMMANDS", "BOOL", "OFF"),
            ("GGML_AVX2", "BOOL", "ON"),
            ("CMAKE_BUILD_TYPE", "BOOL", "Release"),
        )
        for name, kind, value in mutations:
            with self.subTest(name=name, kind=kind, value=value):
                fixture = BaselineFixture()
                self.addCleanup(fixture.close)
                fixture.cache[name] = (kind, value)
                self.assert_rejected(fixture, name)

    def test_cache_rejects_missing_and_duplicate_entries(self) -> None:
        missing = self.fixture()
        del missing.cache["GGML_NATIVE"]
        self.assert_rejected(missing, "missing required entry GGML_NATIVE")

        duplicate = self.fixture()
        duplicate.write()
        cache_path = duplicate.build_root / "CMakeCache.txt"
        with cache_path.open("a", encoding="utf-8") as stream:
            stream.write("GGML_NATIVE:BOOL=OFF\n")
        with self.assertRaisesRegex(baseline.BaselineError, "duplicate entry 'GGML_NATIVE'"):
            baseline.verify_build_root(duplicate.build_root)

    def test_cache_rejects_unsafe_flag_indirection(self) -> None:
        for option in (
            "-march=native",
            "-B/tmp/toolchain",
            "@flags.rsp",
            "--target=x86_64-linux-gnu",
            "-mfpmath=sse",
            "-fplugin=/tmp/plugin.so",
        ):
            with self.subTest(option=option):
                fixture = BaselineFixture()
                self.addCleanup(fixture.close)
                fixture.cache["CMAKE_C_FLAGS"] = ("STRING", option)
                self.assert_rejected(fixture, "forbidden ISA or indirection")

    def test_compile_database_requires_all_pinned_target_sources(self) -> None:
        for required in sorted(baseline.REQUIRED_COMPILE_RECORDS):
            with self.subTest(required=required):
                fixture = BaselineFixture()
                self.addCleanup(fixture.close)
                fixture.records.remove(fixture.records_by_key[required])
                self.assert_rejected(
                    fixture, "missing required native target/source coverage"
                )

    def test_compile_database_rejects_target_source_misbinding(self) -> None:
        fixture = self.fixture()
        record = fixture.records_by_key[("ggml-base", "ggml/src/ggml.c")]
        output_index = record["arguments"].index("-o") + 1
        record["arguments"][output_index] = record["arguments"][output_index].replace(
            "ggml-base.dir", "ggml-cpu.dir"
        )
        record["output"] = record["arguments"][output_index]

        self.assert_rejected(fixture, "missing required native target/source coverage")

    def test_compile_database_rejects_isa_and_tuning_options(self) -> None:
        for option in (
            "-march=native",
            "-mtune=host",
            "-mcpu=native",
            "-mavx2",
            "-msse4.2",
            "-mfpmath=sse",
            "-msse2avx",
            "-msse4a",
            "-mrdrnd",
            "-mrdseed",
            "-madx",
            "-mxsave",
            "-mfsgsbase",
            "-mcx16",
            "-m32",
            "-D__AVX2__=0",
            "-D__SSE4A__=0",
            "-D__RDRND__=0",
            "-DGGML_AVX2=OFF",
        ):
            with self.subTest(option=option):
                fixture = BaselineFixture()
                self.addCleanup(fixture.close)
                record = next(item for item in fixture.records if "arguments" in item)
                record["arguments"].insert(1, option)
                self.assert_rejected(fixture, "forbidden ISA or indirection")

    def test_compile_database_rejects_hidden_option_sources(self) -> None:
        for option in (
            "@flags.rsp",
            "-include",
            "-imacros",
            "--config=compiler.cfg",
            "-specs=compiler.specs",
            "-fplugin=plugin.so",
            "-Xclang",
            "-Btoolchain",
            "-x",
            "--target=x86_64-linux-gnu",
        ):
            with self.subTest(option=option):
                fixture = BaselineFixture()
                self.addCleanup(fixture.close)
                record = next(item for item in fixture.records if "arguments" in item)
                record["arguments"].insert(1, option)
                self.assert_rejected(fixture, "forbidden ISA or indirection")

    def test_compile_database_rejects_compiler_wrapper_or_mismatch(self) -> None:
        fixture = self.fixture()
        record = next(item for item in fixture.records if "arguments" in item)
        record["arguments"][0] = "ccache"

        self.assert_rejected(fixture, "compiler")

    @unittest.skipUnless(sys.platform == "linux", "requires Linux compiler inode identity")
    def test_alternative_compiler_symlink_identity_is_accepted(self) -> None:
        fixture = self.fixture()
        alias = fixture.root / "cc-alias"
        alias.symlink_to(fixture.c_compiler)
        for record in fixture.records:
            if "arguments" in record and record["file"].endswith(".c"):
                record["arguments"][0] = str(alias)

        fixture.verify()

    def test_compile_database_rejects_file_and_compiled_source_mismatch(self) -> None:
        fixture = self.fixture()
        record = next(item for item in fixture.records if "arguments" in item)
        replacement = next(
            item["file"]
            for item in fixture.records
            if item["file"] != record["file"]
            and pathlib.Path(item["file"]).suffix == pathlib.Path(record["file"]).suffix
        )
        source_index = record["arguments"].index("-c") + 1
        record["arguments"][source_index] = replacement

        self.assert_rejected(fixture, "file and -c source differ")

    def test_compile_database_binds_directory_and_output_to_build_root(self) -> None:
        directory_fixture = self.fixture()
        directory_fixture.records[0]["directory"] = str(directory_fixture.root)
        self.assert_rejected(directory_fixture, "directory resolves outside")

        output_fixture = self.fixture()
        record = next(item for item in output_fixture.records if "arguments" in item)
        output_index = record["arguments"].index("-o") + 1
        record["arguments"][output_index] = str(output_fixture.root / "outside.o")
        record["output"] = record["arguments"][output_index]
        self.assert_rejected(output_fixture, "compile output resolves outside")

    def test_optional_output_must_match_derived_output(self) -> None:
        fixture = self.fixture()
        record = next(item for item in fixture.records if "output" in item)
        record["output"] = str(fixture.build_root / "CMakeFiles" / "other.dir" / "other.o")

        self.assert_rejected(fixture, "output disagrees with -o")

    def test_compile_database_rejects_malformed_shapes_and_duplicate_json_keys(self) -> None:
        malformed = self.fixture()
        malformed.records[0]["arguments"] = [malformed.cxx_compiler]
        self.assert_rejected(malformed, "exactly one of arguments or command")

        for arguments in ("/usr/bin/cc -c source.c", [], ["/usr/bin/cc", 7]):
            with self.subTest(arguments=arguments):
                shaped = BaselineFixture()
                self.addCleanup(shaped.close)
                record = next(item for item in shaped.records if "arguments" in item)
                record["arguments"] = arguments
                self.assert_rejected(shaped, "invalid arguments array|invalid bounded argument")

        duplicate = self.fixture()
        duplicate.write_cache()
        (duplicate.build_root / "compile_commands.json").write_text(
            '[{"directory":"one","directory":"two"}]\n', encoding="utf-8"
        )
        with self.assertRaisesRegex(baseline.BaselineError, "duplicate field 'directory'"):
            baseline.verify_build_root(duplicate.build_root)

        evidence_cases = (
            ("missing", None),
            ("empty", b""),
            ("malformed", b"not-json\n"),
        )
        for label, contents in evidence_cases:
            with self.subTest(evidence=label):
                evidence = BaselineFixture()
                self.addCleanup(evidence.close)
                evidence.write()
                database = evidence.build_root / "compile_commands.json"
                if contents is None:
                    database.unlink()
                else:
                    database.write_bytes(contents)
                with self.assertRaises(baseline.BaselineError):
                    baseline.verify_build_root(evidence.build_root)

    def test_compile_database_rejects_duplicate_evidence_and_shared_output(self) -> None:
        duplicate = self.fixture()
        duplicate.records.append(copy.deepcopy(duplicate.records[0]))
        self.assert_rejected(duplicate, "duplicate target/source evidence")

        shared = self.fixture()
        records = [item for item in shared.records if "arguments" in item]
        first_output = records[0]["arguments"][records[0]["arguments"].index("-o") + 1]
        second_output_index = records[1]["arguments"].index("-o") + 1
        records[1]["arguments"][second_output_index] = first_output
        records[1]["output"] = first_output
        self.assert_rejected(shared, "reuses output path")

    def test_compile_database_rejects_shell_syntax_and_control_characters(self) -> None:
        for suffix in (" $UNRESOLVED", " | tee evidence", " > redirected"):
            with self.subTest(suffix=suffix):
                fixture = BaselineFixture()
                self.addCleanup(fixture.close)
                fixture.records[0]["command"] += suffix
                self.assert_rejected(fixture, "shell indirection")

        controlled = self.fixture()
        record = next(item for item in controlled.records if "arguments" in item)
        record["arguments"].insert(1, "-DNAME=value\nnext")
        self.assert_rejected(controlled, "invalid bounded argument")

    def test_evidence_files_reject_hardlinks(self) -> None:
        hardlinked = self.fixture()
        hardlinked.write()
        database = hardlinked.build_root / "compile_commands.json"
        backing = hardlinked.build_root / "compile_commands.backing.json"
        database.replace(backing)
        os.link(backing, database)
        with self.assertRaisesRegex(baseline.BaselineError, "exactly one hard link"):
            baseline.verify_build_root(hardlinked.build_root)

    def test_evidence_files_reject_symlinks(self) -> None:
        linked = self.fixture()
        linked.write()
        cache = linked.build_root / "CMakeCache.txt"
        cache_backing = linked.build_root / "CMakeCache.backing.txt"
        cache.replace(cache_backing)
        try:
            cache.symlink_to(cache_backing)
        except (NotImplementedError, OSError) as error:
            self.skipTest(f"symbolic links are unavailable: {error}")
        with self.assertRaises(baseline.BaselineError):
            baseline.verify_build_root(linked.build_root)

    @unittest.skipUnless(hasattr(os, "mkfifo"), "requires POSIX FIFO support")
    def test_evidence_files_reject_fifo_without_blocking(self) -> None:
        fixture = self.fixture()
        fixture.write()
        database = fixture.build_root / "compile_commands.json"
        database.unlink()
        os.mkfifo(database)

        with self.assertRaisesRegex(baseline.BaselineError, "must be a regular file"):
            baseline.verify_build_root(fixture.build_root)

    def test_evidence_files_reject_oversize_and_invalid_utf8(self) -> None:
        oversized = self.fixture()
        oversized.write()
        (oversized.build_root / "CMakeCache.txt").write_bytes(
            b"x" * (baseline.MAX_CACHE_BYTES + 1)
        )
        with self.assertRaisesRegex(baseline.BaselineError, "size bound"):
            baseline.verify_build_root(oversized.build_root)

        invalid = self.fixture()
        invalid.write()
        (invalid.build_root / "CMakeCache.txt").write_bytes(b"\xff\xfe")
        with self.assertRaisesRegex(baseline.BaselineError, "not strict UTF-8"):
            baseline.verify_build_root(invalid.build_root)

    def test_build_root_symbolic_link_is_rejected(self) -> None:
        fixture = self.fixture()
        fixture.write()
        alias = fixture.root / "build-link"
        try:
            alias.symlink_to(fixture.build_root, target_is_directory=True)
        except (NotImplementedError, OSError) as error:
            self.skipTest(f"directory symbolic links are unavailable: {error}")
        with self.assertRaisesRegex(baseline.BaselineError, "must not be a symbolic link"):
            baseline.verify_build_root(alias)


def main() -> int:
    suite = unittest.defaultTestLoader.loadTestsFromTestCase(
        LinuxCpuNativeBaselineTests
    )
    discovered = suite.countTestCases()
    if discovered != EXPECTED_TEST_COUNT:
        print(
            f"expected {EXPECTED_TEST_COUNT} Linux CPU baseline tests; discovered {discovered}",
            file=sys.stderr,
        )
        return 1
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    return 0 if result.wasSuccessful() else 1


if __name__ == "__main__":
    raise SystemExit(main())
