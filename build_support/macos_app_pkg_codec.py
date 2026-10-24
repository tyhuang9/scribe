"""Private streaming contract for this owner's fresh system-pkgbuild output.

Not an arbitrary archive inspector, root authentication or installation path.
The reviewed producer/BOM interpreter and non-hostile local writers are trusted.
No observation-module parser supplies an acceptance predicate here.
"""
from __future__ import annotations

import hashlib
import os
from pathlib import Path
import re
import stat

import macos_cpu_component as component
import macos_cpu_pkg as streams
from macos_cpu_pkg_profile import xml_tree

APP_NAME = "Scribe.app"
IDENTIFIER = "com.scribe.local-transcriber.application"
CODE_IDENTIFIER = "com.scribe.local-transcriber"
CHUNK = 65536
MAX_METADATA = 65536
# Native Intel/ARM inert fixtures establish the dot-relative bundle shape.
# This fixed real-app expectation still needs full-app native qualification;
# it is not a normalization rule or installation qualification.
BUNDLE_PATH = "./Scribe.app"


def require(condition: bool, message: str) -> None:
    component.require(condition, "app package: " + message)


def _payload_bound(expected: dict) -> int:
    require(type(expected) is dict and 1 <= len(expected) <= 256, "invalid payload inventory")
    require(expected.get(".") == (0o40755, "", 0) and expected.get("./" + APP_NAME) == (0o40755, "", 0),
            "missing payload roots")
    total = 76 + len(b"TRAILER!!!\0") + MAX_METADATA
    for name, value in expected.items():
        require(type(name) is str and name.isascii() and (name in (".", "./" + APP_NAME) or name.startswith("./" + APP_NAME + "/"))
                and len(name) < 4096 and "\\" not in name and ":" not in name
                and all(part not in ("", ".", "..") for part in name.split("/")[1:]), "invalid expected path")
        mode, digest, size = value
        require(mode in (0o40755, 0o100644, 0o100755) and type(size) is int and 0 <= size <= 2 * 1024**3,
                "invalid expected metadata")
        require((stat.S_ISDIR(mode) and digest == "" and size == 0) or
                (stat.S_ISREG(mode) and size > 0 and type(digest) is str and re.fullmatch(r"[0-9a-f]{64}", digest) is not None),
                "invalid expected bytes")
        if name != ".":
            parent = name.rsplit("/", 1)[0]
            require(parent in expected and stat.S_ISDIR(expected[parent][0]), "missing payload ancestor")
        total += 76 + len(name.encode("ascii")) + 1 + size
    return total


def _bom_bound(expected: dict) -> int:
    _payload_bound(expected)
    return MAX_METADATA + 2048 * len(expected)


def _listing_bound(expected: dict) -> int:
    _payload_bound(expected)
    return sum(len(name) + 128 for name in expected)


def _source_checksums(data: bytes, root: Path, expected: dict) -> dict:
    wanted = {os.fsencode(root / name.removeprefix("./")): name
              for name, (mode, _, _) in expected.items() if stat.S_ISREG(mode)}
    maximum = sum(len(name) + 40 for name in wanted)
    require(type(data) is bytes and len(data) <= maximum and data.endswith(b"\n"), "source checksum output bound or tail")
    rows = data[:-1].split(b"\n")
    require(len(rows) == len(wanted), "source checksum inventory differs")
    checksums = {}
    for row in rows:
        fields = row.split(b" ", 2)
        require(len(fields) == 3 and fields[2] in wanted, "unexpected source checksum path")
        name = wanted[fields[2]]
        require(name not in checksums and re.fullmatch(rb"0|[1-9][0-9]{0,9}", fields[0]) is not None
                and re.fullmatch(rb"[1-9][0-9]{0,11}", fields[1]) is not None
                and int(fields[0]) <= 0xffffffff and int(fields[1]) == expected[name][2], "source checksum metadata differs")
        checksums[name] = int(fields[0])
    return checksums


def _package_bound(expected: dict) -> int:
    payload = _payload_bound(expected)
    return payload + payload // 100 + 4 * MAX_METADATA + _bom_bound(expected)


def _odc(chunks, expected: dict) -> None:
    _payload_bound(expected)
    stream, seen = streams._Stream(chunks), set()
    maximum_name = max(len(name.encode()) + 1 for name in (*expected, "TRAILER!!!"))
    for _ in range(len(expected) + 1):
        header = stream.exact(76)
        require(header[:6] == b"070707", "unsupported payload format")
        fields, offset = [], 6
        for width in (6, 6, 6, 6, 6, 6, 6, 11, 6, 11):
            value = header[offset:offset + width]
            require(all(48 <= char <= 55 for char in value), "invalid ODC numeric field")
            fields.append(int(value, 8))
            offset += width
        _device, _inode, mode, uid, gid, links, rdev, _mtime, namesize, size = fields
        require(2 <= namesize <= maximum_name, "payload name exceeds derived bound")
        raw = stream.exact(namesize)
        require(raw[-1:] == b"\0" and b"\0" not in raw[:-1], "invalid payload name terminator")
        try:
            name = raw[:-1].decode("ascii")
        except UnicodeError:
            raise component.ComponentError("app package: invalid payload name encoding") from None
        if name == "TRAILER!!!":
            require(seen == set(expected) and mode == 0 and size == 0 and links == 1, "invalid payload trailer or inventory")
            padding = 0
            while block := stream.read(CHUNK):
                padding += len(block)
                require(padding <= MAX_METADATA and not any(block), "invalid payload padding or trailing stream")
            return
        require(name in expected and name not in seen, "unexpected, duplicate or aliased payload node")
        wanted_mode, digest, wanted_size = expected[name]
        require(mode == wanted_mode and uid == 0 and gid == 0 and rdev == 0 and size == wanted_size,
                "payload type, ownership, mode, device or size differs")
        require(links == 1 if stat.S_ISREG(mode) else 1 <= links <= len(expected) + 2,
                "unsupported payload link metadata")
        seen.add(name)
        if stat.S_ISREG(mode):
            measured, remaining = hashlib.sha256(), size
            while remaining:
                block = stream.exact(min(CHUNK, remaining))
                measured.update(block)
                remaining -= len(block)
            require(measured.hexdigest() == digest, "payload bytes differ from retained staging")
    raise component.ComponentError("app package: missing payload trailer")


def _decimal(value: str, maximum: int) -> int:
    require(re.fullmatch(r"0|[1-9][0-9]{0,19}", value) is not None, "invalid decimal metadata")
    result = int(value)
    require(result <= maximum, "decimal metadata exceeds bound")
    return result


def _leaf(node, name: str) -> str:
    matches = node.findall(name)
    require(len(matches) == 1 and not matches[0].attrib and not len(matches[0]), "ambiguous metadata scalar")
    value = matches[0].text or ""
    require(0 < len(value) <= 128 and all(32 <= ord(char) < 127 for char in value), "invalid metadata scalar")
    return value


def _number(node, name: str, maximum: int) -> int:
    return _decimal(_leaf(node, name), maximum)


def _checksum(node, name: str) -> str:
    matches = node.findall(name)
    require(len(matches) == 1 and not len(matches[0]) and matches[0].attrib == {"style": "sha1"},
            "unsupported member checksum")
    value = matches[0].text or ""
    require(re.fullmatch(r"[0-9a-f]{40}", value) is not None, "invalid member checksum")
    return value


def _source_metadata(member) -> None:
    for name, bits in (("inode", 64), ("deviceno", 32)):
        if member.find(name) is not None:
            value = _leaf(member, name)
            require(re.fullmatch(r"0|-?[1-9][0-9]{0,18}", value) is not None
                    and -(2**(bits - 1)) <= int(value) < 2**(bits - 1), "invalid inert source stat metadata")
    created = member.find("FinderCreateTime")
    if created is not None:
        require(not created.attrib and not (created.text or "").strip()
                and sorted(child.tag for child in created) == ["nanoseconds", "time"], "invalid creation-time metadata")
        _leaf(created, "time")
        _number(created, "nanoseconds", 999999999)


def _package_info(data: bytes, version: str, expected: dict) -> None:
    root = xml_tree(data)
    required = {"identifier": IDENTIFIER, "version": version, "install-location": "/Applications",
                "auth": "root", "format-version": "2", "overwrite-permissions": "true",
                "postinstall-action": "none", "relocatable": "false"}
    require(root.tag == "pkg-info" and all(root.get(key) == value for key, value in required.items())
            and set(root.attrib) <= set(required) | {"generator-version"}, "app package identity or control differs")
    generator = root.get("generator-version")
    require(generator is None or 0 < len(generator) <= 128 and all(32 <= ord(char) < 127 for char in generator),
            "unsupported generator metadata")
    require([node.tag for node in root] == ["payload", "bundle", "bundle-version", "upgrade-bundle",
                "update-bundle", "atomic-update-bundle", "strict-identifier", "relocate"], "unexpected app package controls")
    for node in root.iter():
        require(not (node.text or "").strip() and not (node.tail or "").strip(), "unexpected package metadata text")
    payload, bundle = root[:2]
    require(not len(payload) and set(payload.attrib) == {"numberOfFiles", "installKBytes"}
            and _decimal(payload.get("numberOfFiles"), len(expected)) == len(expected), "payload inventory count differs")
    # This is the producer's bounded space estimate, not a sum of file bytes or
    # an independently qualified Installer disk-space calculation.
    require(0 < _decimal(payload.get("installKBytes"), (_payload_bound(expected) + len(expected) * 4096) // 1024 + 1),
            "invalid payload space estimate")
    require(not len(bundle) and bundle.attrib == {"id": CODE_IDENTIFIER, "path": BUNDLE_PATH,
            "CFBundleVersion": version, "CFBundleShortVersionString": version}, "bundle identity or path differs")
    for node in root[2:]:
        require(not node.attrib, "unexpected bundle control attribute")
        if node.tag in ("upgrade-bundle", "strict-identifier"):
            require(len(node) == 1 and node[0].tag == "bundle" and not len(node[0])
                    and node[0].attrib == {"id": CODE_IDENTIFIER}, "bundle reference differs")
        else:
            require(not len(node), "unexpected bundle control reference")


def _inspect_fresh(package: Path, expected: dict, version: str, private: Path) -> tuple:
    maximum = _package_bound(expected)
    handle, before = component.open_regular(package, maximum)
    bom = private / "inspected.bom"
    bom_identity = None
    with handle:
        magic, size, format_version, compressed, expanded, algorithm = streams.HEADER.unpack(streams._exact(handle, 28))
        require((magic, size, format_version, algorithm) == (b"xar!", 28, 1, 1)
                and 0 < compressed <= MAX_METADATA and 0 < expanded <= MAX_METADATA, "unsupported XAR header")
        packed_toc = streams._exact(handle, compressed)
        toc_bytes = b"".join(streams._inflate([packed_toc], MAX_METADATA, 15))
        require(len(toc_bytes) == expanded, "TOC size differs")
        root = xml_tree(toc_bytes)
        require(root.tag == "xar" and not root.attrib and len(root) == 1 and root[0].tag == "toc"
                and not root[0].attrib, "unexpected XAR root")
        toc, heap = root[0], 28 + compressed
        require(sorted(node.tag for node in toc) == ["checksum", "creation-time", "file", "file", "file"],
                "unexpected XAR members, signature or script")
        checksum = toc.find("checksum")
        require(checksum.attrib == {"style": "sha1"} and sorted(node.tag for node in checksum) == ["offset", "size"]
                and _number(checksum, "size", 20) == 20, "invalid TOC checksum metadata")
        offset = _number(checksum, "offset", maximum)
        require(heap + offset + 20 <= before.st_size, "TOC checksum outside package")
        handle.seek(heap + offset)
        require(streams._exact(handle, 20) == hashlib.sha1(packed_toc, usedforsecurity=False).digest(), "TOC checksum differs")
        _leaf(toc, "creation-time")
        ranges, members, identifiers = [(offset, offset + 20)], {}, set()
        for node in toc.findall("file"):
            require(set(node.attrib) <= {"id"}, "unexpected member attribute")
            if node.attrib:
                identity = _decimal(node.get("id"), 0xffffffff)
                require(identity not in identifiers, "duplicate member identity")
                identifiers.add(identity)
            name = _leaf(node, "name")
            allowed = {"name", "type", "mode", "data", "uid", "gid", "user", "group", "mtime", "atime", "ctime",
                       "inode", "deviceno", "FinderCreateTime"}
            tags = [child.tag for child in node]
            require(len(tags) == len(set(tags)) and set(tags) <= allowed and name not in members
                    and name in ("PackageInfo", "Payload", "Bom") and _leaf(node, "type") == "file",
                    "unexpected member, type, alias or metadata")
            for tag in set(tags) - {"data", "FinderCreateTime", "inode", "deviceno"}:
                _leaf(node, tag)
            _source_metadata(node)
            require(node.find("mode") is None or _leaf(node, "mode") == "0644", "unexpected outer member mode")
            data = node.findall("data")
            require(len(data) == 1 and not data[0].attrib and sorted(child.tag for child in data[0]) ==
                    ["archived-checksum", "encoding", "extracted-checksum", "length", "offset", "size"], "invalid member data")
            data = data[0]
            bound = MAX_METADATA if name == "PackageInfo" else _bom_bound(expected) if name == "Bom" else maximum
            begin, length, declared = _number(data, "offset", maximum), _number(data, "length", bound), _number(data, "size", bound)
            encoding = data.findall("encoding")
            style = "application/octet-stream" if name == "Payload" else "application/x-gzip"
            require(len(encoding) == 1 and not len(encoding[0]) and encoding[0].attrib == {"style": style}
                    and length > 0 and declared > 0 and heap + begin + length <= before.st_size, "invalid encoding or member range")
            ranges.append((begin, begin + length))
            members[name] = (begin, length, declared, bound, _checksum(data, "archived-checksum"), _checksum(data, "extracted-checksum"))
        require(set(members) == {"PackageInfo", "Payload", "Bom"}, "missing app component member")
        cursor = 0
        for begin, end in sorted(ranges):
            require(begin == cursor, "overlap, gap or unreferenced heap bytes")
            cursor = end
        require(heap + cursor == before.st_size, "trailing package bytes")
        for name, (begin, length, declared, bound, archived_sha, extracted_sha) in members.items():
            archived, extracted, count = hashlib.sha1(usedforsecurity=False), hashlib.sha1(usedforsecurity=False), 0
            def stored():
                for block in streams._region(handle, heap + begin, length):
                    archived.update(block)
                    yield block
            decoded = stored() if name == "Payload" else streams._inflate(stored(), bound, 47)
            def contents():
                nonlocal count
                for block in decoded:
                    count += len(block)
                    require(count <= declared, "member exceeds declared size")
                    extracted.update(block)
                    yield block
            if name == "Payload":
                _odc(streams._inflate(contents(), _payload_bound(expected), 31), expected)
            elif name == "PackageInfo":
                _package_info(b"".join(contents()), version, expected)
            else:
                digest = hashlib.sha256()
                with bom.open("xb") as output:
                    for block in contents():
                        require(output.write(block) == len(block), "short owned Bom write")
                        digest.update(block)
                    output.flush()
                    os.fsync(output.fileno())
                    bom.chmod(0o600)
                    # Bind the actual write descriptor, not a later pathname
                    # baseline after unrelated potentially large package reads.
                    bom_identity = (digest.hexdigest(), count, component.fingerprint(os.fstat(output.fileno())))
            require(count == declared and archived.hexdigest() == archived_sha and extracted.hexdigest() == extracted_sha,
                    "member checksum or size differs")
            if name == "Bom":
                require((*component.hash_file(bom, _bom_bound(expected)), component.fingerprint(bom.lstat())) == bom_identity,
                        "owned Bom readback differs from decoded package bytes")
        require(component.fingerprint(os.fstat(handle.fileno())) == component.fingerprint(before)
                and component.fingerprint(package.lstat()) == component.fingerprint(before), "package changed during inspection")
    require(bom_identity is not None, "missing owned Bom binding")
    return bom, bom_identity


def _bom_listing(data: bytes, expected: dict, checksums: dict) -> None:
    """Documented trusted-tool projection, not complete binary BOM validation.

    Only own fresh generated Bom is interpreted. Internal flags/link tables and
    Installer behavior remain delegated-tool assumptions. Independent source
    cksum results must agree; no alternative algorithm/normalization is tried.
    """
    require(type(data) is bytes and 0 < len(data) <= _listing_bound(expected) and data.endswith(b"\n"),
            "BOM listing bound or tail")
    try:
        lines = data[:-1].decode("ascii").split("\n")
    except UnicodeError:
        raise component.ComponentError("app package: invalid BOM listing encoding") from None
    require(len(lines) == len(expected) and set(checksums) == {name for name, (mode, _, _) in expected.items()
                if stat.S_ISREG(mode)}, "BOM or source checksum inventory differs")
    seen = set()
    for row in lines:
        fields = row.split("\t")
        require(len(fields) == 6 and fields[0] in expected and fields[0] not in seen, "BOM inventory or fields differ")
        name = fields[0]
        mode, _, size = expected[name]
        require(fields[1:4] == [format(mode, "o"), "0", "0"], "BOM type, mode or ownership differs")
        if stat.S_ISREG(mode):
            require(fields[4] == str(size) and _decimal(fields[5], 0xffffffff) == checksums[name],
                    "BOM source size or checksum differs")
        else:
            require(fields[4:] == ["", ""], "unexpected BOM directory metadata")
        seen.add(name)
    require(seen == set(expected), "missing BOM node")
