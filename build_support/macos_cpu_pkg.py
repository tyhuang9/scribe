"""Internal unsigned CPU component authoring, never installation or root trust.

Inspect ONLY the fresh output of this invocation's trusted system pkgbuild.
This is not an external archive/admission CLI. The finalized CPU-only app,
reviewed source/toolchain and non-hostile same-user/root writers remain trusted.
Production entry signing, final package authentication, notarization and product
composition are separate work. No operation or worker executable is run here.
"""
from __future__ import annotations

import hashlib
import os
from pathlib import Path
import re
import struct
import sys
import zlib

import macos_cpu_component as component
import macos_cpu_pkg_entry as entry
from macos_cpu_pkg_profile import xml_tree

IDENTIFIER = "com.scribe.local-transcriber.cpu-worker"
CHUNK = 65536
MAX_METADATA = 65536
HEADER = struct.Struct(">4sHHQQI")


def require(condition: bool, message: str) -> None:
    component.require(condition, "CPU component package: " + message)


def _package_bound(entry_size: int) -> int:
    require(type(entry_size) is int and 0 < entry_size <= entry.MAX_ENTRY, "invalid entry size")
    return entry_size + entry_size // 100 + 4 * 1024 * 1024


def _exact(handle, count: int) -> bytes:
    require(0 <= count <= CHUNK, "metadata read exceeds its bound")
    data = handle.read(count)
    require(len(data) == count, "truncated package metadata")
    return data


def _region(handle, begin: int, length: int):
    handle.seek(begin)
    remaining = length
    while remaining:
        block = handle.read(min(CHUNK, remaining))
        require(bool(block), "truncated package member")
        remaining -= len(block)
        yield block


def _inflate(chunks, maximum: int, window: int):
    """One complete bounded stream, with bounded output per decompressor call."""
    decoder, total = zlib.decompressobj(window), 0
    for pending in chunks:
        require(not decoder.eof, "trailing or concatenated compressed stream")
        while True:
            try:
                block = decoder.decompress(pending, CHUNK)
            except zlib.error as error:
                raise component.ComponentError("CPU component package: malformed compression") from error
            pending = decoder.unconsumed_tail
            total += len(block)
            require(total <= maximum and not decoder.unused_data, "decoded member exceeds bound or has trailing data")
            if block:
                yield block
            if not pending and len(block) < CHUNK:
                break
    require(decoder.eof and not decoder.unused_data and not decoder.unconsumed_tail,
            "incomplete compressed stream")


def _leaf(node, name: str) -> str:
    matches = node.findall(name)
    require(len(matches) == 1 and not len(matches[0]) and not matches[0].attrib, "ambiguous metadata scalar")
    value = matches[0].text or ""
    require(0 < len(value) <= 128 and all(32 <= ord(c) < 127 for c in value), "unsafe metadata scalar")
    return value


def _number(node, name: str, maximum: int) -> int:
    value = _leaf(node, name)
    require(re.fullmatch(r"0|[1-9][0-9]{0,10}", value) is not None, "invalid numeric metadata")
    result = int(value)
    require(result <= maximum, "numeric metadata exceeds bound")
    return result


def _source_stat_metadata(member) -> None:
    """Validate inert Scripts stat text, never identity or extraction authority.

    Apple's xar/configure.ac selects PRId64/PRId32 for the macOS inode/device
    serialization, regardless of ino_t's logical unsigned type. No value is
    reinterpreted, compared to this host, retained as identity, or reported.
    """
    for tag, bits in (("inode", 64), ("deviceno", 32)):
        if member.find(tag) is None:
            continue
        value = _leaf(member, tag)
        require(not (member.find(tag).tail or "").strip(), "unexpected stat scalar tail")
        require(re.fullmatch(r"0|-?[1-9][0-9]{0,18}", value) is not None,
                "invalid serialized stat integer")
        require(-(2 ** (bits - 1)) <= int(value) < 2 ** (bits - 1),
                "serialized stat integer exceeds source bound")
    created = member.find("FinderCreateTime")
    if created is None:
        return
    # xar/lib/darwinattr.c writes exactly these leaves. This does not admit
    # arbitrary same-shape containers, attributes, aliases, or extra metadata.
    require(not created.attrib and not (created.text or "").strip()
            and not (created.tail or "").strip()
            and sorted(child.tag for child in created) == ["nanoseconds", "time"],
            "unsupported source creation-time structure")
    timestamp = _leaf(created, "time")
    require(re.fullmatch(r"[0-9]{4}-(?:0[1-9]|1[0-2])-(?:0[1-9]|[12][0-9]|3[01])"
                         r"T(?:[01][0-9]|2[0-3]):[0-5][0-9]:(?:[0-5][0-9]|60)", timestamp) is not None,
            "invalid source creation-time spelling")
    year, month, day = int(timestamp[:4]), int(timestamp[5:7]), int(timestamp[8:10])
    leap = year % 4 == 0 and (year % 100 != 0 or year % 400 == 0)
    days = (31, 29 if leap else 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31)
    require(year > 0 and day <= days[month - 1], "invalid source creation-time calendar")
    _number(created, "nanoseconds", 999999999)
    require(all(not (child.tail or "").strip() for child in created), "unexpected creation-time scalar tail")


def _checksum(node, name: str) -> str:
    matches = node.findall(name)
    require(len(matches) == 1 and not len(matches[0]) and matches[0].attrib == {"style": "sha1"},
            "unsupported or ambiguous member checksum")
    value = matches[0].text or ""
    require(re.fullmatch(r"[0-9a-f]{40}", value) is not None, "invalid checksum spelling")
    return value


def _package_info(data: bytes, version: str) -> None:
    root = xml_tree(data)
    require(root.tag == "pkg-info" and root.get("identifier") == IDENTIFIER
            and root.get("version") == version and root.get("install-location") == "/", "package identity differs")
    # Deliberately narrow construction policy, NOT inferred native defaults.
    # Unqualified metadata fails closed; future native acceptance must establish
    # any additional inert fields before extending this profile.
    require(set(root.attrib) <= {"identifier", "version", "install-location", "format-version", "auth"},
            "unsupported PackageInfo attribute")
    require(root.get("format-version") in (None, "2") and root.get("auth") in (None, "root"),
            "unsupported PackageInfo format or authorization metadata")
    scripts = root.findall("scripts")
    require(len(scripts) == 1 and len(list(root.iter("scripts"))) == 1 and not scripts[0].attrib,
            "ambiguous operation container")
    operations = list(scripts[0])
    require(len(operations) == 1 and operations[0].tag == "postinstall" and not len(operations[0])
            and operations[0].attrib == {"file": "./postinstall"}
            and not (operations[0].text or "").strip()
            and len(list(root.iter("postinstall"))) == 1, "unexpected package operation")
    for child in root:
        if child is scripts[0]:
            continue
        # No payload exists in a nopayload component. Do not admit bundles,
        # references, relocation rules or other unobserved executable constructs.
        require(child.tag == "payload" and not len(child) and not (child.text or "").strip()
                and child.attrib == {"installKBytes": "0", "numberOfFiles": "0"},
                "unsupported PackageInfo construct")
    require(len(root.findall("payload")) <= 1, "duplicate payload metadata")


class _Stream:
    """Small look-ahead for the private ODC reader; never stores the entry."""
    def __init__(self, chunks):
        self.chunks, self.pending = iter(chunks), b""

    def read(self, count: int) -> bytes:
        require(0 <= count <= CHUNK, "ODC read exceeds its bound")
        parts, remaining = [], count
        while remaining:
            if not self.pending:
                self.pending = next(self.chunks, b"")
                if not self.pending:
                    break
            used = min(remaining, len(self.pending))
            parts.append(self.pending[:used])
            self.pending = self.pending[used:]
            remaining -= used
        return b"".join(parts)

    def exact(self, count: int) -> bytes:
        data = self.read(count)
        require(len(data) == count, "truncated ODC record")
        return data


def _odc(chunks, entry_sha: str, entry_size: int) -> None:
    stream = _Stream(chunks)
    seen, root_seen = False, False
    for _ in range(3):  # at most optional root, sole postinstall, trailer
        header = stream.exact(76)
        require(header[:6] == b"070707", "unsupported ODC format")
        widths = (6, 6, 6, 6, 6, 6, 6, 11, 6, 11)
        fields, offset = [], 6
        for width in widths:
            field = header[offset:offset + width]
            require(all(48 <= b <= 55 for b in field), "invalid ODC numeric field")
            fields.append(int(field, 8))
            offset += width
        mode, links, name_size, size = fields[2], fields[5], fields[8], fields[9]
        require(2 <= name_size <= 256 and size <= entry_size, "ODC name or body exceeds bound")
        name = stream.exact(name_size)
        require(name[-1:] == b"\0" and b"\0" not in name[:-1], "invalid ODC name")
        name = name[:-1]
        if name == b"TRAILER!!!":
            require(seen and mode == 0 and links == 1 and size == 0, "invalid ODC trailer")
            padding = 0
            while block := stream.read(CHUNK):
                padding += len(block)
                require(padding <= MAX_METADATA and not any(block), "nonzero or excessive ODC tail")
            return
        if name == b".":
            require(not root_seen and not seen and mode == 0o040700 and links == 3 and size == 0,
                    "unsupported ODC root")
            root_seen = True
            continue
        require(name == b"./postinstall" and not seen and mode == 0o100755 and links == 1
                and size == entry_size, "unexpected ODC file, alias, link or operation")
        seen, digest, remaining = True, hashlib.sha256(), size
        while remaining:
            block = stream.exact(min(CHUNK, remaining))
            digest.update(block)
            remaining -= len(block)
        require(digest.hexdigest() == entry_sha, "packaged entry differs from retained entry")
    raise component.ComponentError("CPU component package: missing ODC trailer")


def _inspect_fresh(package: Path, entry_sha: str, entry_size: int, version: str) -> None:
    """Private fresh-output content binding, never signed-PKG/root admission."""
    maximum = _package_bound(entry_size)
    handle, before = component.open_regular(package, maximum)
    with handle:
        magic, size, version_id, compressed, expanded, algorithm = HEADER.unpack(_exact(handle, HEADER.size))
        require((magic, size, version_id, algorithm) == (b"xar!", 28, 1, 1)
                and 0 < compressed <= MAX_METADATA and 0 < expanded <= MAX_METADATA, "unsupported XAR header")
        compressed_toc = _exact(handle, compressed)
        toc_bytes = b"".join(_inflate([compressed_toc], MAX_METADATA, 15))
        require(len(toc_bytes) == expanded, "TOC expanded size differs")
        root = xml_tree(toc_bytes)
        require(root.tag == "xar" and not root.attrib and len(root) == 1
                and root[0].tag == "toc" and not root[0].attrib, "unexpected XAR root")
        toc, heap = root[0], HEADER.size + compressed
        require(sorted(child.tag for child in toc) == ["checksum", "creation-time", "file", "file"],
                "unexpected XAR member or signature")
        checks = toc.findall("checksum")
        require(len(checks) == 1 and checks[0].attrib == {"style": "sha1"}
                and sorted(child.tag for child in checks[0]) == ["offset", "size"], "unsupported TOC checksum")
        checksum_offset = _number(checks[0], "offset", maximum)
        require(_number(checks[0], "size", 20) == 20, "TOC checksum length differs")
        # Apple's xar/lib/archive.c hashes the COMPRESSED TOC, then stores that
        # digest in the heap. SHA1 is format-integrity only, never project trust.
        require(heap + checksum_offset + 20 <= before.st_size, "TOC checksum outside heap")
        handle.seek(heap + checksum_offset)
        require(_exact(handle, 20) == hashlib.sha1(compressed_toc, usedforsecurity=False).digest(), "TOC checksum differs")
        _leaf(toc, "creation-time")
        ranges = [(checksum_offset, checksum_offset + 20)]
        members, identifiers = {}, set()
        for member_index, member in enumerate(toc.findall("file")):
            require(set(member.attrib) <= {"id"}, "unsupported member attribute")
            if member.attrib:
                identifier = member.get("id")
                require(re.fullmatch(r"0|[1-9][0-9]{0,9}", identifier) is not None and identifier not in identifiers,
                        "ambiguous member identity")
                identifiers.add(identifier)
            tags = [node.tag for node in member]
            name = _leaf(member, "name")
            allowed = {"name", "type", "mode", "data", "uid", "gid", "user", "group", "mtime", "atime", "ctime"}
            if name == "Scripts":
                allowed.update({"inode", "deviceno", "FinderCreateTime"})
            duplicate_count = len(tags) - len(set(tags))
            unsupported_count = sum(tag not in allowed for tag in tags)
            require(duplicate_count == 0 and unsupported_count == 0,
                    "unsupported member metadata "
                    f"(member-index={member_index}; duplicate-fields={duplicate_count}; "
                    f"unsupported-fields={unsupported_count})")
            require(name in ("PackageInfo", "Scripts") and name not in members and _leaf(member, "type") == "file",
                    "unexpected member, alias or file type")
            for tag in set(tags) - {"data", "inode", "deviceno", "FinderCreateTime"}:
                _leaf(member, tag)
            if name == "Scripts":
                _source_stat_metadata(member)
            mode = _leaf(member, "mode") if "mode" in tags else None
            require(mode == "0644" if name == "Scripts" else mode in (None, "0644"), "unsupported outer member mode")
            nodes = member.findall("data")
            require(len(nodes) == 1 and not nodes[0].attrib, "ambiguous member data")
            node = nodes[0]
            require(sorted(child.tag for child in node) ==
                    ["archived-checksum", "encoding", "extracted-checksum", "length", "offset", "size"],
                    "unsupported member data metadata")
            begin = _number(node, "offset", maximum)
            length = _number(node, "length", MAX_METADATA if name == "PackageInfo" else maximum)
            declared = _number(node, "size", MAX_METADATA if name == "PackageInfo" else maximum)
            require(length > 0 and declared > 0 and heap + begin + length <= before.st_size, "invalid member range")
            ranges.append((begin, begin + length))
            encodings = node.findall("encoding")
            expected = "application/x-gzip" if name == "PackageInfo" else "application/octet-stream"
            require(len(encodings) == 1 and not len(encodings[0]) and encodings[0].attrib == {"style": expected},
                    "unsupported member encoding")
            members[name] = (begin, length, declared, _checksum(node, "archived-checksum"), _checksum(node, "extracted-checksum"))
        require(set(members) == {"PackageInfo", "Scripts"}, "missing component member")
        cursor = 0
        for begin, end in sorted(ranges):
            require(begin == cursor, "overlap, gap or unreferenced heap bytes")
            cursor = end
        require(heap + cursor == before.st_size, "trailing package bytes")
        for name in ("PackageInfo", "Scripts"):
            begin, length, declared, archived_sha, extracted_sha = members[name]
            stored_digest, extracted_digest = hashlib.sha1(usedforsecurity=False), hashlib.sha1(usedforsecurity=False)

            def stored():
                for block in _region(handle, heap + begin, length):
                    stored_digest.update(block)
                    yield block

            decoded = _inflate(stored(), MAX_METADATA, 47) if name == "PackageInfo" else stored()
            count = 0

            def contents():
                nonlocal count
                for block in decoded:
                    count += len(block)
                    require(count <= declared, "member expanded beyond declared size")
                    extracted_digest.update(block)
                    yield block

            if name == "PackageInfo":
                _package_info(b"".join(contents()), version)
            else:
                # TOC's Scripts member is opaque gzip, followed by bounded ODC.
                # Three fixed-name ODC headers/names need at most 255 bytes,
                # independently of the allowed zero padding and entry length.
                _odc(_inflate(contents(), entry_size + MAX_METADATA + 255, 31), entry_sha, entry_size)
            require(count == declared and stored_digest.hexdigest() == archived_sha
                    and extracted_digest.hexdigest() == extracted_sha, "member size or checksum differs")
        require(component.fingerprint(os.fstat(handle.fileno())) == component.fingerprint(before)
                and component.fingerprint(package.lstat()) == component.fingerprint(before), "package changed during inspection")


def _package_constructed_entry(app: Path, private: Path, before: dict, version: str,
                               constructed: tuple[Path, str, int]) -> tuple[Path, str, int]:
    """Private tail for the caller's OWN freshly constructed CPU entry only.

    Both source callers own this private tree, have admitted the CPU-only app,
    and have just built the entry there. No arbitrary prebuilt-entry interface,
    callbacks, trust flags or publication authority are provided.
    """
    scripts = private / "scripts"
    operation, sha, size = constructed
    require(operation == scripts / "postinstall", "entry constructor returned wrong path")
    with os.scandir(scripts) as inventory:
        first, second = next(inventory, None), next(inventory, None)
        require(first is not None and first.name == "postinstall" and second is None,
                "unexpected operation inventory")
    require(component.hash_file(operation, entry.MAX_ENTRY) == (sha, size) and os.access(operation, os.X_OK),
            "entry constructor identity or mode differs")
    identity = component.fingerprint(operation.lstat())
    candidate = private / "cpu-worker.pkg"
    temporary = private / "temp"
    temporary.mkdir(mode=0o700)
    component.capture(["/usr/bin/pkgbuild", "--nopayload", "--scripts", str(scripts), "--identifier", IDENTIFIER,
                       "--version", version, "--install-location", "/", str(candidate)], timeout=120,
                      environment={**component.probe_environment(), "TMPDIR": str(temporary)},
                      limit=8192, error_limit=8192, operation="cpu-component-pkg-build")
    _inspect_fresh(candidate, sha, size, version)
    require(component.cpu_app_input_snapshot(app) == before
            and component.hash_file(operation, entry.MAX_ENTRY) == (sha, size)
            and component.fingerprint(operation.lstat()) == identity, "app or entry changed during packaging")
    package_sha, package_size = component.hash_file(candidate, _package_bound(size))
    return candidate, package_sha, package_size


def build_component(app: Path, workspace: Path, digest: str, revision: str) -> tuple[Path, str, int]:
    """Construct cpu-worker.pkg; return unsigned/PRE-SIGN path/hash/size only."""
    require(sys.platform == "darwin", "construction requires macOS")
    require(os.geteuid() != 0, "construction must be unprivileged")
    component.validate_identity(digest, revision)
    require(app.is_absolute() and app.is_dir() and not app.is_symlink(), "unsafe application input")
    app = app.resolve(strict=True)
    require(workspace.is_absolute() and workspace.is_dir() and not workspace.is_symlink()
            and not any(ord(c) < 32 for c in str(workspace)), "unsafe author workspace")
    workspace = workspace.resolve(strict=True)
    component.require_outside_app(workspace, app)
    output = workspace / "cpu-worker.pkg"
    require(not os.path.lexists(output), "output already exists")
    before = component.cpu_app_input_snapshot(app)
    require(before[component.CPU_APP_INPUTS[0]][0] == digest, "worker differs from frozen input")
    version = component.verify_cpu_app(app)  # CPU-only/default-deny policy unchanged
    with component.private_workspace(workspace, ".scribe-cpu-pkg-") as private:
        scripts = private / "scripts"
        scripts.mkdir(mode=0o700)
        constructed = entry.build_entry(app, scripts, digest, revision)
        candidate, package_sha, package_size = _package_constructed_entry(app, private, before, version, constructed)
        component.publish_no_replace(candidate, output)
    return output, package_sha, package_size
