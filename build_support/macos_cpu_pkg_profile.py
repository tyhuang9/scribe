"""Observe native pkgbuild's fixed, unsigned source-test fixture; never install.

This has no archive/path input and is NOT a package inspector or trust decision.
Only the freshly generated tiny fixture is read. No archive is extracted, no
operation is executed, and no real worker, application or signing key is used.
"""
from __future__ import annotations

import json
import os
from pathlib import Path
import re
import struct
import sys
import tempfile
import xml.etree.ElementTree as ET
import zlib

import macos_cpu_component as component

IDENTIFIER = "com.scribe.source-test.cpu-package-profile"
VERSION = "1.0.0"
FIXTURE = b"# Scribe package-format fixture; intentionally no interpreter.\n"
MAX_PACKAGE = 1024 * 1024
MAX_TOC = 64 * 1024
MAX_MEMBER = 64 * 1024
HEADER = struct.Struct(">4sHHQQI")


def require(condition: bool, message: str) -> None:
    component.require(condition, "package profile: " + message)


def inflate(data: bytes, maximum: int) -> bytes:
    # Accept one complete zlib/gzip stream only. No unbounded flush, recursion,
    # concatenated members or allocating from a claimed expanded length.
    require(0 < len(data) <= MAX_PACKAGE and 0 < maximum <= MAX_TOC,
            "invalid compression bounds")
    decoder = zlib.decompressobj(47)
    try:
        result = decoder.decompress(data, maximum + 1)
    except zlib.error as error:
        raise component.ComponentError("package profile: invalid compressed stream") from error
    require(len(result) <= maximum and decoder.eof and not decoder.unconsumed_tail
            and not decoder.unused_data, "incomplete, trailing or oversized compressed stream")
    return result


def xml_tree(data: bytes) -> ET.Element:
    require(0 < len(data) <= MAX_TOC, "XML exceeds its bound")
    try:
        text = data.decode("utf-8")
        # NUL-interleaved ASCII can otherwise let Expat autodetect UTF-16 after
        # our UTF-8 decode and bypass a literal declaration check.
        require("\x00" not in text and "<!" not in text,
                "NUL, DTD and entity declarations are forbidden")
        root = ET.fromstring(text)
    except (UnicodeError, ET.ParseError) as error:
        raise component.ComponentError("package profile: malformed XML") from error
    pending, count = [(root, 0)], 0
    while pending:
        node, depth = pending.pop()
        count += 1
        require(count <= 256 and depth <= 8 and len(node) <= 64, "XML tree exceeds its bound")
        require(re.fullmatch(r"[A-Za-z][A-Za-z0-9-]{0,63}", node.tag) is not None,
                "unsupported XML tag")
        pending.extend((child, depth + 1) for child in node)
    return root


def scalar(node: ET.Element, name: str) -> str:
    matches = node.findall(name)
    require(len(matches) == 1 and len(matches[0]) == 0, "ambiguous scalar")
    value = matches[0].text or ""
    require(re.fullmatch(r"[A-Za-z0-9./_-]{1,96}", value) is not None, "unsafe scalar")
    return value


def number(node: ET.Element, name: str, maximum: int) -> int:
    text = scalar(node, name)
    require(re.fullmatch(r"0|[1-9][0-9]{0,9}", text) is not None, "invalid numeric scalar")
    value = int(text)
    require(value <= maximum, "numeric scalar exceeds bound")
    return value


def observe_generated(data: bytes) -> dict:
    """Private probe decoding, not reusable admission of arbitrary archives.

    XAR header layout: Apple's xar/include/xar.h.in. Bounded decoding here
    records observed encodings; it does not verify checksums or authenticity.
    """
    require(HEADER.size < len(data) <= MAX_PACKAGE, "package size exceeds bound")
    magic, size, version, compressed, expanded, checksum = HEADER.unpack(data[:HEADER.size])
    require(magic == b"xar!" and size == HEADER.size and version == 1, "unsupported XAR header")
    require(0 < compressed <= MAX_TOC and 0 < expanded <= MAX_TOC
            and size + compressed <= len(data), "invalid TOC bounds")
    toc_bytes = inflate(data[size:size + compressed], MAX_TOC)
    require(len(toc_bytes) == expanded, "TOC size differs from header")
    root = xml_tree(toc_bytes)
    require(root.tag == "xar" and len(root) == 1 and root[0].tag == "toc", "unexpected XAR root")
    toc = root[0]
    files = toc.findall("file")
    require(2 <= len(files) <= 8, "unexpected outer member count")
    heap = size + compressed
    members, ranges, contents = [], [], {}
    for file in files:
        name, kind = scalar(file, "name"), scalar(file, "type")
        require(re.fullmatch(r"[A-Za-z][A-Za-z0-9]{0,31}", name) is not None
                and name.casefold() not in {key.casefold() for key in contents}
                and kind == "file" and not file.findall("file"), "unsafe outer member")
        records = file.findall("data")
        require(len(records) == 1, "ambiguous member data")
        record = records[0]
        offset = number(record, "offset", MAX_PACKAGE)
        length = number(record, "length", MAX_MEMBER)
        declared = number(record, "size", MAX_MEMBER)
        begin, end = heap + offset, heap + offset + length
        require(length > 0 and begin >= heap and end <= len(data)
                and all(end <= a or begin >= b for a, b in ranges), "invalid member range")
        ranges.append((begin, end))
        encodings = record.findall("encoding")
        require(len(encodings) == 1 and set(encodings[0].attrib) == {"style"},
                "ambiguous member encoding")
        encoding = encodings[0].get("style")
        require(encoding in ("application/octet-stream", "application/x-gzip"),
                "unobserved member encoding; review the native tool output")
        raw = data[begin:end]
        decoded = inflate(raw, MAX_MEMBER) if encoding == "application/x-gzip" else raw
        require(len(decoded) == declared, "member size differs from TOC")
        contents[name] = decoded
        styles = {}
        for tag in ("archived-checksum", "extracted-checksum"):
            values = record.findall(tag)
            require(len(values) <= 1, "ambiguous checksum metadata")
            if values:
                style = values[0].get("style", "")
                require(re.fullmatch(r"[a-z0-9-]{1,16}", style) is not None,
                        "unsafe checksum label")
                styles[tag] = style
        members.append({"name": name, "type": kind, "mode": scalar(file, "mode"),
                        "encoding": encoding, "stored_bytes": length,
                        "decoded_bytes": declared, "prefix_hex": decoded[:8].hex(),
                        "checksum_styles": styles})
    require({"PackageInfo", "Scripts"}.issubset(contents), "missing required observation member")
    package_info = xml_tree(contents["PackageInfo"])
    require(package_info.tag == "pkg-info" and package_info.get("identifier") == IDENTIFIER
            and package_info.get("version") == VERSION
            and package_info.get("install-location") == "/", "fixture package identity changed")
    # XAR may already have decoded the archive. Do not presume a second gzip
    # layer or a CPIO dialect. Record each observed layer and the final prefix.
    scripts = contents["Scripts"]
    inner = "none"
    if scripts.startswith(b"\x1f\x8b"):
        inner = "gzip"
    elif len(scripts) >= 2 and scripts[0] & 15 == 8 and scripts[0] >> 4 <= 7 \
            and int.from_bytes(scripts[:2], "big") % 31 == 0:
        inner = "zlib"
    if inner != "none":
        scripts = inflate(scripts, MAX_MEMBER)
    require(scripts.count(FIXTURE) == 1, "fixture is absent or duplicated in Scripts")
    references = [node.get("file", "") for node in package_info.iter("postinstall")]
    require(len(references) == 1 and references[0] in ("postinstall", "./postinstall"),
            "unexpected operation reference")
    return {"schema_version": 1, "observation_only": True, "package_bytes": len(data),
            "xar_header_bytes": size, "xar_version": version, "toc_checksum_algorithm": checksum,
            "toc_compressed_bytes": compressed, "toc_expanded_bytes": expanded,
            "toc_child_tags": sorted(child.tag for child in toc),
            "members": sorted(members, key=lambda member: member["name"]),
            "package_identifier": IDENTIFIER, "package_version": VERSION,
            "install_location": "/", "postinstall_reference": references[0],
            "scripts_inner_compression": inner, "scripts_decoded_bytes": len(scripts),
            "scripts_prefix_hex": scripts[:16].hex(),
            "fixture_occurrences": 1}


def probe() -> dict:
    require(sys.platform == "darwin", "native observation requires macOS")
    require(os.geteuid() != 0, "native observation must be unprivileged")
    with component.private_workspace(Path(tempfile.gettempdir()), "scribe-pkg-profile-") as workspace:
        scripts = workspace / "scripts"
        scripts.mkdir(mode=0o700)
        fixture = scripts / "postinstall"
        with fixture.open("xb") as handle:
            handle.write(FIXTURE)
            handle.flush()
            os.fchmod(handle.fileno(), 0o755)
            os.fsync(handle.fileno())
        package = workspace / "fixture.pkg"
        # No signing, resources, payload root, script execution or Installer.
        component.capture(["/usr/bin/pkgbuild", "--nopayload", "--scripts", str(scripts),
                           "--identifier", IDENTIFIER, "--version", VERSION,
                           "--install-location", "/", str(package)], timeout=30, limit=8192)
        report = observe_generated(component.read_bounded(package, MAX_PACKAGE))
        listing = component.capture(["/usr/bin/xar", "-tf", str(package)], timeout=5, limit=1024)
        require(sorted(listing.decode("ascii").splitlines()) ==
                [member["name"] for member in report["members"]], "tool listing differs from TOC")
        return report


def main() -> None:
    require(len(sys.argv) == 1, "this fixed fixture accepts no archive, path or options")
    print(json.dumps(probe(), sort_keys=True, separators=(",", ":")))


if __name__ == "__main__":
    main()
