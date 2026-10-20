"""Non-installing CPU development-component authoring and inspection.

This inventory is not installer/root authority. Inspection NEVER executes a
component executable. Only assembly probes its freshly built writer, unprivileged.
"""
from __future__ import annotations

import argparse
import ctypes
from contextlib import contextmanager
import errno
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import selectors
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import time

REPO = Path(__file__).resolve().parent.parent
WORKER = "payload/scribe-inference-worker"
WRITER = "tools/scribe-macos-cpu-worker-installer"
FILES = ["component.json", WORKER, WRITER]
DIRECTORIES = ["payload", "tools"]
CPU_ROOT = "/Library/Application Support/Scribe/CPU"
MAX_WORKER = 512 * 1024 * 1024
MAX_WRITER = 64 * 1024 * 1024
MAX_JSON = 8192
TARGETS = ("aarch64-apple-darwin", "x86_64-apple-darwin")
NATIVE_DARWIN = sys.platform == "darwin"


class ComponentError(Exception):
    pass


class CleanupUncertain(ComponentError):
    """Do not publish or delete a workspace with unconfirmed child cleanup."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ComponentError(message)


def validate_identity(digest: str, revision: str) -> None:
    require(re.fullmatch(r"[0-9a-f]{64}", digest) is not None, "invalid frozen worker SHA-256")
    require(re.fullmatch(r"[\x21-\x7e]{12,96}", revision) is not None, "invalid frozen build revision")


def strict_json(data: bytes) -> dict:
    require(0 < len(data) <= MAX_JSON, "metadata size is outside its bound")

    def pairs(entries):
        result = {}
        for key, value in entries:
            require(key not in result, "duplicate metadata key")
            result[key] = value
        return result

    def invalid_constant(_value):
        raise ComponentError("non-finite metadata number")

    try:
        value = json.loads(data.decode("utf-8"), object_pairs_hook=pairs,
                           parse_constant=invalid_constant)
    except (UnicodeError, ValueError, RecursionError) as error:
        raise ComponentError("malformed metadata") from error
    require(type(value) is dict, "metadata must be an object")
    return value


def typed_equal(actual, expected) -> bool:
    if type(actual) is not type(expected):
        return False
    if type(expected) is dict:
        return actual.keys() == expected.keys() and all(
            typed_equal(actual[key], value) for key, value in expected.items())
    if type(expected) is list:
        return len(actual) == len(expected) and all(
            typed_equal(a, b) for a, b in zip(actual, expected))
    return actual == expected


def fingerprint(info):
    # Python 3.12 on Windows exposes creation time via lstat but change time via
    # fstat. Native authoring is macOS-only and retains its strict ctime check.
    return (info.st_dev, info.st_ino, info.st_mode, info.st_nlink,
            info.st_size, info.st_mtime_ns, info.st_ctime_ns if os.name == "posix" else None)


def check_node(info, directory: bool, maximum: int = MAX_WORKER) -> None:
    require(not (getattr(info, "st_file_attributes", 0) &
                 getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400)), "reparse-point node")
    require(stat.S_ISDIR(info.st_mode) if directory else stat.S_ISREG(info.st_mode),
            "unexpected node type")
    if not directory:
        require(info.st_nlink == 1, "hard-linked component file")
        require(0 < info.st_size <= maximum, "component file size is outside its bound")


def open_regular(path: Path, maximum: int):
    before = path.lstat()
    check_node(before, False, maximum)
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_CLOEXEC", 0)
    flags |= getattr(os, "O_NONBLOCK", 0)
    descriptor = os.open(path, flags)
    try:
        opened = os.fstat(descriptor)
        check_node(opened, False, maximum)
        require(fingerprint(before) == fingerprint(opened), "file changed during descriptor open")
        return os.fdopen(descriptor, "rb", buffering=0), opened
    except BaseException:
        os.close(descriptor)
        raise


def hash_file(path: Path, maximum: int) -> tuple[str, int]:
    source, before = open_regular(path, maximum)
    digest = hashlib.sha256()
    remaining = before.st_size
    with source:
        while remaining:
            block = source.read(min(65536, remaining))
            require(bool(block), "file was truncated while hashing")
            digest.update(block)
            remaining -= len(block)
        require(not source.read(1), "file grew while hashing")
        require(fingerprint(os.fstat(source.fileno())) == fingerprint(before),
                "file changed while hashing")
    require(fingerprint(path.lstat()) == fingerprint(before), "named file changed while hashing")
    return digest.hexdigest(), before.st_size


def read_bounded(path: Path, maximum: int) -> bytes:
    handle, before = open_regular(path, maximum)
    with handle:
        data = handle.read(maximum + 1)
        require(len(data) == before.st_size and len(data) <= maximum,
                "metadata changed during read")
        require(fingerprint(os.fstat(handle.fileno())) == fingerprint(before),
                "metadata changed during read")
    return data


def copy_new(source: Path, destination: Path, maximum: int) -> tuple[str, int]:
    reader, before = open_regular(source, maximum)
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
    with reader, os.fdopen(os.open(destination, flags, 0o600), "wb", buffering=0) as output:
        remaining = before.st_size
        while remaining:
            block = reader.read(min(65536, remaining))
            require(bool(block), "source was truncated while copying")
            position = 0
            while position < len(block):
                written = output.write(block[position:])
                require(written is not None and written > 0, "short component write")
                position += written
            remaining -= len(block)
        require(not reader.read(1), "source grew while copying")
        require(fingerprint(os.fstat(reader.fileno())) == fingerprint(before),
                "source changed while copying")
        if hasattr(os, "fchmod"):
            os.fchmod(output.fileno(), 0o755)
        os.fsync(output.fileno())
    # Authenticate the separately reopened COPIED bytes, not the source read.
    return hash_file(destination, maximum)


def check_inventory(root: Path) -> None:
    check_node(root.lstat(), True)
    if hasattr(os, "listxattr"):
        require(not {"com.apple.ResourceFork", "com.apple.FinderInfo"}.intersection(
            os.listxattr(root, follow_symlinks=False)), "component root contains resource-fork metadata")
    actual_files, actual_directories = [], []
    pending = [root]
    while pending:
        directory = pending.pop()
        relative_root = directory.relative_to(root)
        seen = set()
        with os.scandir(directory) as entries:
            for entry in entries:
                require(len(seen) < len(FILES) + len(DIRECTORIES), "oversized component inventory")
                name = entry.name
                require(name.casefold() not in seen, "case-colliding component names")
                seen.add(name.casefold())
                path = directory / name
                relative = (relative_root / name).as_posix()
                info = path.lstat()
                if relative in DIRECTORIES:
                    check_node(info, True)
                    actual_directories.append(relative)
                    pending.append(path)
                else:
                    require(relative in FILES, "unexpected component entry")
                    check_node(info, False, MAX_JSON if relative == "component.json"
                               else MAX_WRITER if relative == WRITER else MAX_WORKER)
                    actual_files.append(relative)
                if hasattr(os, "listxattr"):
                    attributes = os.listxattr(path, follow_symlinks=False)
                    require(not {"com.apple.ResourceFork", "com.apple.FinderInfo"}.intersection(attributes),
                            "component contains resource-fork metadata")
    require(sorted(actual_files) == FILES and sorted(actual_directories) == DIRECTORIES,
            "component inventory is not exact")


def record_for(digest: str, revision: str, worker_size: int,
               writer_digest: str, writer_size: int) -> dict:
    return {"schema_version": 1, "component": "scribe-protected-cpu-development",
            "build_revision": revision, "worker_sha256": digest, "worker_size_bytes": worker_size,
            "writer_sha256": writer_digest, "writer_size_bytes": writer_size,
            "architectures": ["aarch64", "x86_64"], "files": FILES,
            "installs_application": False, "trusted_installer": False}


def verify_files(root: Path, digest: str, revision: str) -> dict:
    validate_identity(digest, revision)
    check_inventory(root)
    record = strict_json(read_bounded(root / "component.json", MAX_JSON))
    worker_digest, worker_size = hash_file(root / WORKER, MAX_WORKER)
    writer_digest, writer_size = hash_file(root / WRITER, MAX_WRITER)
    require(worker_digest == digest, "copied worker differs from frozen authority")
    require(typed_equal(record, record_for(digest, revision, worker_size, writer_digest, writer_size)),
            "component inventory record mismatch")
    return record


def probe_environment() -> dict[str, str]:
    # No loader/provider variables, user configuration, signing material or stdin.
    return {"PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "LC_ALL": "C", "LANG": "C"}


def build_environment(digest: str, revision: str) -> dict[str, str]:
    # Source authoring trusts the reviewed toolchain and Cargo cache/config,
    # not ambient wrappers, compiler flags, providers or signing overrides.
    environment = {key: os.environ[key] for key in
                   ("PATH", "HOME", "CARGO_HOME", "RUSTUP_HOME", "RUSTUP_TOOLCHAIN", "TMPDIR")
                   if key in os.environ}
    environment.update(LC_ALL="C", LANG="C", SCRIBE_BUNDLED_WORKER_SHA256=digest,
                       SCRIBE_BUILD_REVISION=revision, MACOSX_DEPLOYMENT_TARGET="13.0",
                       CARGO_NET_OFFLINE="true", RUSTC_WRAPPER="", RUSTC_WORKSPACE_WRAPPER="",
                       CARGO_ENCODED_RUSTFLAGS="")
    return environment


def check_directory_acl(descriptor: int) -> None:
    """Same Darwin ACL constants/policy as src/macos_worker_fs.rs, read-only."""
    library = ctypes.CDLL(None, use_errno=True)
    pointer, integer = ctypes.c_void_p, ctypes.c_int
    library.acl_get_fd_np.argtypes, library.acl_get_fd_np.restype = [integer, integer], pointer
    library.acl_valid.argtypes, library.acl_valid.restype = [pointer], integer
    library.acl_get_entry.argtypes = [pointer, integer, ctypes.POINTER(pointer)]
    library.acl_get_entry.restype = integer
    library.acl_get_tag_type.argtypes = [pointer, ctypes.POINTER(integer)]
    library.acl_get_tag_type.restype = integer
    library.acl_get_permset_mask_np.argtypes = [pointer, ctypes.POINTER(ctypes.c_uint64)]
    library.acl_get_permset_mask_np.restype = integer
    library.acl_free.argtypes, library.acl_free.restype = [pointer], integer
    acl = library.acl_get_fd_np(descriptor, 0x100)
    if not acl:
        require(ctypes.get_errno() == errno.ENOENT, "could not read workspace ACL")
        return
    try:
        require(library.acl_valid(acl) == 0, "invalid workspace ACL")
        for index in range(129):
            entry = pointer()
            status = library.acl_get_entry(acl, 0 if index == 0 else -1, ctypes.byref(entry))
            if status == -1:
                require(ctypes.get_errno() == errno.EINVAL, "could not enumerate workspace ACL")
                return
            require(status == 0 and entry.value and index < 128, "oversized or malformed workspace ACL")
            tag, rights = integer(), ctypes.c_uint64()
            require(library.acl_get_tag_type(entry, ctypes.byref(tag)) == 0 and
                    library.acl_get_permset_mask_np(entry, ctypes.byref(rights)) == 0,
                    "could not inspect workspace ACL rights")
            known = sum(1 << bit for bit in (*range(1, 14), 20))
            mutation = sum(1 << bit for bit in (2, 4, 5, 6, 8, 10, 12, 13))
            require(tag.value in (1, 2) and not rights.value & ~known,
                    "unknown workspace ACL rights")
            require(tag.value != 1 or not rights.value & mutation, "workspace ACL grants mutation")
    finally:
        library.acl_free(acl)


def check_directory_authority(descriptor: int) -> None:
    info = os.fstat(descriptor)
    require(info.st_uid in (0, os.geteuid()), "workspace ancestor has an unexpected owner")
    require(not info.st_mode & 0o022 or (info.st_uid == 0 and info.st_mode & stat.S_ISVTX),
            "workspace ancestor is writable by other users")
    if NATIVE_DARWIN:
        check_directory_acl(descriptor)


@contextmanager
def private_workspace(parent: Path, prefix: str):
    """Only remove our exact directory through its retained, suitable parent."""
    parent = parent.resolve(strict=True)
    flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW
    if NATIVE_DARWIN:
        # Every absolute-path reopen (including describe) must stay beneath
        # ancestors another non-root user cannot rename or redirect.
        for ancestor in reversed((parent, *parent.parents)):
            descriptor = os.open(ancestor, flags)
            try:
                check_directory_authority(descriptor)
            finally:
                os.close(descriptor)
    parent_fd = os.open(parent, flags)
    try:
        info = os.fstat(parent_fd)
        check_directory_authority(parent_fd)
        require(shutil.rmtree.avoids_symlink_attacks, "descriptor-safe cleanup is required")
        workspace = Path(tempfile.mkdtemp(prefix=prefix, dir=parent))
        identity = workspace.lstat()
        workspace_fd = os.open(workspace, flags)
        try:
            check_directory_authority(workspace_fd)
        finally:
            os.close(workspace_fd)
        named_parent = parent.stat()
        require((info.st_dev, info.st_ino) == (named_parent.st_dev, named_parent.st_ino),
                "workspace parent changed")
        cleanup_confirmed = True
        try:
            yield workspace
        except CleanupUncertain:
            cleanup_confirmed = False
            raise
        finally:
            if cleanup_confirmed:
                current = os.stat(workspace.name, dir_fd=parent_fd, follow_symlinks=False)
                require((current.st_dev, current.st_ino) == (identity.st_dev, identity.st_ino)
                        and stat.S_ISDIR(current.st_mode), "workspace identity changed; retaining it")
                shutil.rmtree(workspace.name, dir_fd=parent_fd)
    finally:
        os.close(parent_fd)


def require_outside_app(parent: Path, app: Path) -> None:
    app_identity = app.stat()
    for ancestor in (parent, *parent.parents):
        info = ancestor.stat()
        require((info.st_dev, info.st_ino) != (app_identity.st_dev, app_identity.st_ino),
                "component output must not mutate the application")


def capture(arguments: list[str], *, timeout: float = 10, limit: int = 262144,
            error_limit: int = 65536, environment: dict | None = None, cwd: Path | None = None) -> bytes:
    """Bound output and time while reserving PID ownership until final reap.

    macOS needs Python >=3.13 for waitid/WNOWAIT:
    https://docs.python.org/3/library/os.html#os.waitid
    Never call poll()/communicate() before a failure's process-group signal.
    """
    require(all(hasattr(os, name) for name in ("waitid", "WNOWAIT", "WEXITED", "P_PID")),
            "metadata process checks require waitid/WNOWAIT (Python 3.13+ on macOS)")
    require(signal.getsignal(signal.SIGCHLD) == signal.SIG_DFL,
            "unexpected child reaper policy")
    require(Path(arguments[0]).is_absolute(), "tool executable must be absolute")
    require(timeout > 0 and limit > 0 and error_limit > 0, "invalid process bounds")
    deadline = time.monotonic() + timeout
    process = subprocess.Popen(arguments, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                               stderr=subprocess.PIPE,
                               env=probe_environment() if environment is None else environment,
                               cwd=cwd,
                               start_new_session=True, close_fds=True, bufsize=0)
    output = [bytearray(), bytearray()]
    reserved = True
    reaped = False
    try:
        with selectors.DefaultSelector() as selector:
            for index, pipe in enumerate((process.stdout, process.stderr)):
                os.set_blocking(pipe.fileno(), False)
                selector.register(pipe, selectors.EVENT_READ, index)
            while True:
                try:
                    terminal = os.waitid(os.P_PID, process.pid,
                                         os.WEXITED | os.WNOHANG | os.WNOWAIT)
                except ChildProcessError as error:
                    reserved = False
                    raise CleanupUncertain("child ownership was lost; refusing group signals") from error
                remaining = deadline - time.monotonic()
                require(remaining > 0, "metadata process exceeded its deadline")
                if terminal is not None and not selector.get_map():
                    status = process.wait(timeout=remaining)
                    reaped = True
                    # EOF and leader exit alone do not prove descendants stopped.
                    # Once reaped, never signal: PID/PGID ownership is no longer held.
                    try:
                        os.killpg(process.pid, 0)
                    except ProcessLookupError:
                        pass
                    except OSError as error:
                        raise CleanupUncertain("cannot confirm process-group exit") from error
                    else:
                        raise CleanupUncertain("process group remains after leader exit")
                    require(status == 0, "bounded tool exited unsuccessfully")
                    return bytes(output[0])
                for key, _ in selector.select(min(remaining, 0.05)):
                    index = key.data
                    maximum = limit if index == 0 else error_limit
                    try:
                        chunk = os.read(key.fd, min(65536, maximum - len(output[index]) + 1))
                    except BlockingIOError:
                        continue
                    if not chunk:
                        selector.unregister(key.fileobj)
                    else:
                        require(len(output[index]) + len(chunk) <= maximum,
                                "metadata process exceeded its output bound")
                        output[index].extend(chunk)
    finally:
        cleanup_error = None
        if not reaped and reserved:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            except OSError as error:
                cleanup_error = error
            try:
                process.wait(timeout=2)
                reaped = True
            except (OSError, subprocess.TimeoutExpired) as error:
                cleanup_error = error
            if reaped:
                try:
                    os.killpg(process.pid, 0)
                except ProcessLookupError:
                    pass
                except OSError as error:
                    cleanup_error = error
                else:
                    cleanup_error = ComponentError("process group cleanup remains uncertain")
        for pipe in (process.stdout, process.stderr):
            pipe.close()
        if cleanup_error is not None or not reaped:
            raise CleanupUncertain("metadata child cleanup could not be confirmed") from cleanup_error


def expected_description(digest: str, revision: str) -> dict:
    return {"schemaVersion": 1, "component": "protected-cpu-writer", "cpuRoot": CPU_ROOT,
            "workerName": "scribe-inference-worker", "workerSha256": digest,
            "buildRevision": revision, "installsApplication": False}


def check_description(data: bytes, digest: str, revision: str) -> None:
    require(typed_equal(strict_json(data), expected_description(digest, revision)),
            "fresh writer description does not match frozen inputs")


def has_markers(path: Path, markers: list[bytes], maximum: int) -> None:
    handle, before = open_regular(path, maximum)
    found = [False] * len(markers)
    overlap = max(map(len, markers), default=1) - 1
    with handle:
        tail = b""
        remaining = before.st_size
        while remaining:
            block = handle.read(min(65536, remaining))
            require(bool(block), "binary was truncated during marker inspection")
            remaining -= len(block)
            combined = tail + block
            found = [yes or marker in combined for yes, marker in zip(found, markers)]
            tail = combined[-overlap:] if overlap else b""
        require(not handle.read(1), "binary grew during marker inspection")
        require(fingerprint(os.fstat(handle.fileno())) == fingerprint(before),
                "binary changed during marker inspection")
    require(all(found), "binary slice does not contain the frozen identities")


def inspect_universal(path: Path, markers: list[bytes], workspace: Path, maximum: int) -> None:
    hash_file(path, maximum)
    architectures = capture(["/usr/bin/lipo", str(path), "-archs"]).decode("ascii").split()
    require(sorted(architectures) == ["arm64", "x86_64"], "binary must have exactly ARM64 and x64 slices")
    capture(["/usr/bin/codesign", "--verify", "--strict", str(path)])
    for architecture in ("arm64", "x86_64"):
        thin = workspace / (path.name + "-" + architecture)
        require(not os.path.lexists(thin), "slice inspection output already exists")
        capture(["/usr/bin/lipo", str(path), "-thin", architecture, "-output", str(thin)])
        try:
            loads = capture(["/usr/bin/otool", "-l", str(thin)])
            require(re.search(rb"\bminos\s+13\.0(?:\.0)?\s*(?:\n|$)", loads) is not None,
                    "binary slice must target macOS 13.0")
            require(b"/Metal.framework/" not in loads, "CPU component must not load Metal")
            dependencies = capture(["/usr/bin/otool", "-L", str(thin)]).decode("utf-8").splitlines()[1:]
            for line in dependencies:
                library = line.strip().split(" (compatibility version", 1)[0]
                require(library.startswith(("/usr/lib/", "/System/Library/Frameworks/")),
                        "component depends on a non-system native library")
                require(not any(name in library.lower() for name in ("metal.framework", "cuda", "vulkan")),
                        "component contains a GPU dependency")
            has_markers(thin, markers, maximum)
        finally:
            if thin.is_file() and not thin.is_symlink():
                thin.unlink()


def inspect_component(root: Path, digest: str, revision: str) -> dict:
    """Non-executing component inspector; the record never grants root trust."""
    require(sys.platform == "darwin", "native component inspection requires macOS")
    root = root.absolute()
    verify_files(root, digest, revision)
    with private_workspace(Path(tempfile.gettempdir()), "scribe-component-inspect-") as workspace:
        snapshot = workspace / "snapshot"
        (snapshot / "payload").mkdir(parents=True, mode=0o700)
        (snapshot / "tools").mkdir(mode=0o700)
        for name in FILES:
            maximum = MAX_JSON if name == "component.json" else MAX_WRITER if name == WRITER else MAX_WORKER
            copy_new(root / name, snapshot / name, maximum)
        # Tools see only our private bounded snapshot, not mutable external paths.
        record = verify_files(snapshot, digest, revision)
        inspect_universal(snapshot / WORKER, [], workspace, MAX_WORKER)
        inspect_universal(snapshot / WRITER, [digest.encode(), revision.encode(), CPU_ROOT.encode()],
                          workspace, MAX_WRITER)
    return record


def publish_no_replace(stage: Path, output: Path) -> None:
    require(sys.platform == "darwin", "exclusive component publication requires macOS")
    source_parent = os.open(stage.parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        target_parent = os.open(output.parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        try:
            rename = ctypes.CDLL(None, use_errno=True).renameatx_np
            rename.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint]
            rename.restype = ctypes.c_int
            # Darwin sys/stdio.h RENAME_EXCL == 0x4. No overwrite or final cleanup.
            if rename(source_parent, os.fsencode(stage.name), target_parent, os.fsencode(output.name), 4) != 0:
                raise OSError(ctypes.get_errno(), "exclusive component publication failed")
            os.fsync(target_parent)
        finally:
            os.close(target_parent)
    finally:
        os.close(source_parent)


def assemble(app: Path, output: Path, digest: str, revision: str) -> None:
    require(sys.platform == "darwin", "component assembly requires macOS")
    require(os.geteuid() != 0, "development components must be built unprivileged")
    validate_identity(digest, revision)
    require(hasattr(os, "waitid"), "component assembly requires Python 3.13+ on macOS")
    require(app.is_dir() and not app.is_symlink(), "unsafe application input")
    app = app.resolve(strict=True)
    output = output.absolute()
    require(output.name not in ("", ".", "..") and not any(ord(c) < 32 for c in str(output)),
            "unsafe component output name")
    require(output.parent.is_dir() and not output.parent.is_symlink(), "unsafe component output parent")
    output = output.parent.resolve(strict=True) / output.name
    require_outside_app(output.parent, app)
    require(not os.path.lexists(output), "component output already exists")
    capture(["/bin/bash", str(REPO / "scripts/verify-macos-release-package.sh"), "--app", str(app)],
            timeout=60, environment={**probe_environment(), "PATH": os.environ.get("PATH", "")})
    resources = app / "Contents/Resources"
    catalog = strict_json(read_bounded(resources / "worker-pack-catalog.json", MAX_JSON))
    authority = strict_json(read_bounded(resources / "gpu-pack-release-authority.json", MAX_JSON))
    require(typed_equal(catalog, {"schema_version": 1, "packs": []}), "component requires a CPU-only catalog")
    expected_authority = strict_json(read_bounded(
        REPO / "runtime-manifests/gpu-pack-release-authority-macos-empty.json", MAX_JSON))
    require(typed_equal(authority, expected_authority), "component requires default-deny epoch-zero authority")
    info = plistlib.loads(read_bounded(app / "Contents/Info.plist", MAX_JSON))
    version = info.get("CFBundleShortVersionString", "")
    require(re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version) is not None, "invalid application version")
    with private_workspace(output.parent, ".scribe-cpu-component-build-") as workspace:
        tree = workspace / "component"
        (tree / "payload").mkdir(parents=True, mode=0o700)
        (tree / "tools").mkdir(mode=0o700)
        native = app / "Contents/MacOS"
        copied_digest, worker_size = copy_new(native / "scribe-inference-worker", tree / WORKER, MAX_WORKER)
        require(copied_digest == digest, "copied final worker differs from frozen build input")
        desktop_snapshot = workspace / "Scribe"
        copy_new(native / "Scribe", desktop_snapshot, MAX_WORKER)
        inspect_universal(tree / WORKER,
                          [f"scribe-inference-worker@{version}#{revision}".encode()], workspace, MAX_WORKER)
        inspect_universal(desktop_snapshot,
                          [f"local-transcriber@{version}#{revision}".encode(), digest.encode()], workspace, MAX_WORKER)
        environment = build_environment(digest, revision)
        cargo = shutil.which("cargo")
        require(cargo is not None, "reviewed Rust toolchain is required")
        slices = []
        for target in TARGETS:
            target_directory = workspace / ("build-" + target)
            environment["CARGO_TARGET_DIR"] = str(target_directory)
            capture([cargo, "build", "--locked", "--offline", "--release", "--target", target,
                     "--manifest-path", str(REPO / "tools/macos-cpu-worker-installer/Cargo.toml")],
                    environment=environment, cwd=REPO, timeout=1200,
                    limit=4 * 1024 * 1024, error_limit=4 * 1024 * 1024)
            binary = target_directory / target / "release/scribe-macos-cpu-worker-installer"
            hash_file(binary, MAX_WRITER)
            slices.append(str(binary))
        writer = tree / WRITER
        capture(["/usr/bin/lipo", "-create", "-output", str(writer), *slices])
        capture(["/usr/bin/codesign", "--force", "--sign", "-", "--options", "runtime", str(writer)])
        # This is the ONLY component executable launched, freshly built above.
        check_description(capture([str(writer), "describe"], timeout=5, limit=4096, error_limit=4096),
                          digest, revision)
        writer_digest, writer_size = hash_file(writer, MAX_WRITER)
        record = record_for(digest, revision, worker_size, writer_digest, writer_size)
        with (tree / "component.json").open("xb") as handle:
            handle.write(json.dumps(record, sort_keys=True, separators=(",", ":")).encode())
            handle.flush()
            os.fsync(handle.fileno())
        inspect_component(tree, digest, revision)
        publish_no_replace(tree, output)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    for name in ("assemble", "inspect"):
        command = sub.add_parser(name)
        command.add_argument("--worker-sha256", required=True)
        command.add_argument("--build-revision", required=True)
        if name == "assemble":
            command.add_argument("--app", type=Path, required=True)
            command.add_argument("--output-directory", type=Path, required=True)
        else:
            command.add_argument("--component", type=Path, required=True)
    arguments = parser.parse_args()
    try:
        if arguments.command == "assemble":
            assemble(arguments.app, arguments.output_directory, arguments.worker_sha256, arguments.build_revision)
        else:
            inspect_component(arguments.component, arguments.worker_sha256, arguments.build_revision)
    except (ComponentError, OSError, subprocess.SubprocessError, ValueError) as error:
        parser.exit(1, f"CPU development component rejected: {error}\n")


if __name__ == "__main__":
    main()
