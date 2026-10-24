"""Private readback of this owner's fresh components and unsigned product.

Not arbitrary package admission, a transferable receipt or Installer authority.
Component constructors remain responsible for full payload/entry verification.
This layer streams exact decoded member equivalence and fixed distribution
semantics. Reviewed tools and non-hostile same-user/root writers are trusted.
"""
from __future__ import annotations

import hashlib
import os
from pathlib import Path
import re
import xml.etree.ElementTree as ET

import macos_app_pkg_codec as app
import macos_cpu_component as component
import macos_cpu_pkg as streams

NAMES = ("cpu-worker.pkg", "metal-worker.pkg", "app.pkg")
IDENTIFIERS = (streams.IDENTIFIER, streams._METAL_IDENTIFIER, app.IDENTIFIER)
MEMBERS = (("PackageInfo", "Scripts"), ("PackageInfo", "Scripts"), ("PackageInfo", "Payload", "Bom"))
MAX_METADATA = 65536
MAX_BOM = MAX_METADATA + 2048 * 256
STAT_FIELDS = {"mode", "uid", "gid", "user", "group", "mtime", "atime", "ctime", "inode", "deviceno", "FinderCreateTime"}


def require(condition: bool, message: str) -> None:
    component.require(condition, "product package: " + message)


def _product_bound(sizes: tuple) -> int:
    require(type(sizes) is tuple and len(sizes) == 3 and all(type(size) is int and size > 0 for size in sizes),
            "invalid owned component sizes")
    total = sum(sizes)
    return total + total // 100 + 4 * MAX_METADATA


def _file_layout(name: str, maximum: int) -> dict:
    index = NAMES.index(name)
    return {member: ("application/octet-stream" if member in ("Scripts", "Payload") else "application/x-gzip",
                     MAX_METADATA if member == "PackageInfo" else MAX_BOM if member == "Bom" else maximum)
            for member in MEMBERS[index]}


def _index(path: Path, maximum: int, layout: dict, directories: tuple = ()) -> tuple[dict, dict]:
    """Closed own-output XAR index; bounded reads, no member extraction.

    Standalone components have one name property. The observed product member
    tree has two equal leaf names ONLY directly beneath its three fixed parents.
    IDs/stat values are inert, never links, source identity or extraction paths.
    """
    handle, before = component.open_regular(path, maximum)
    with handle:
        magic, header_size, version, packed, expanded, algorithm = streams.HEADER.unpack(streams._exact(handle, 28))
        require((magic, header_size, version, algorithm) == (b"xar!", 28, 1, 1)
                and 0 < packed <= MAX_METADATA and 0 < expanded <= MAX_METADATA, "unsupported XAR header")
        compressed = streams._exact(handle, packed)
        xml = b"".join(streams._inflate([compressed], MAX_METADATA, 15))
        require(len(xml) == expanded, "TOC length differs")
        root = app.xml_tree(xml)
        require(root.tag == "xar" and not root.attrib and len(root) == 1 and root[0].tag == "toc"
                and not root[0].attrib, "unexpected XAR root")
        toc, heap = root[0], 28 + packed
        require(sorted(node.tag for node in toc) == ["checksum", "creation-time"] + ["file"] * (4 if directories else len(layout)),
                "unexpected TOC members or signature")
        checksum = toc.find("checksum")
        require(checksum.attrib == {"style": "sha1"} and sorted(node.tag for node in checksum) == ["offset", "size"]
                and app._number(checksum, "size", 20) == 20, "unsupported TOC checksum")
        offset = app._number(checksum, "offset", maximum)
        require(heap + offset + 20 <= before.st_size, "TOC checksum outside package")
        handle.seek(heap + offset)
        require(streams._exact(handle, 20) == hashlib.sha1(compressed, usedforsecurity=False).digest(), "TOC checksum differs")
        app._leaf(toc, "creation-time")
        ranges, files, seen_directories = [(offset, offset + 20)], {}, set()
        pending = [(node, "") for node in reversed(toc.findall("file"))]
        while pending:
            node, parent = pending.pop()
            names = node.findall("name")
            wanted_names = 2 if parent else 1
            require(len(names) == wanted_names and all(not value.attrib and not len(value) for value in names)
                    and len({value.text for value in names}) == 1, "ambiguous member names")
            name = names[0].text
            require(type(name) is str and re.fullmatch(r"[A-Za-z][A-Za-z0-9._-]{0,63}", name) is not None,
                    "unsupported member name")
            full_name = parent + name
            kind = app._leaf(node, "type")
            tags = [child.tag for child in node if child.tag != "name"]
            scalars = [tag for tag in tags if tag != "file"]
            require(set(node.attrib) <= {"id"} and len(scalars) == len(set(scalars)), "ambiguous member metadata")
            if node.attrib:
                app._decimal(node.get("id"), 0xffffffff)
            allowed = {"type", "file" if kind == "directory" else "data"} | STAT_FIELDS
            require(set(tags) <= allowed, "unexpected member metadata")
            for tag in set(tags) - {"data", "file", "inode", "deviceno", "FinderCreateTime"}:
                app._leaf(node, tag)
            app._source_metadata(node)
            if kind == "directory":
                require(not parent and name in directories and name not in seen_directories
                        and not node.findall("data"), "unexpected component directory")
                seen_directories.add(name)
                pending.extend((child, name + "/") for child in reversed(node.findall("file")))
                continue
            require(kind == "file" and full_name in layout and full_name not in files
                    and not node.findall("file"), "unexpected, duplicate or aliased member")
            records = node.findall("data")
            require(len(records) == 1 and not records[0].attrib and sorted(child.tag for child in records[0]) ==
                    ["archived-checksum", "encoding", "extracted-checksum", "length", "offset", "size"], "invalid member data")
            data = records[0]
            style, bound = layout[full_name]
            begin, length, size = app._number(data, "offset", maximum), app._number(data, "length", maximum), app._number(data, "size", bound)
            encoding = data.findall("encoding")
            require(len(encoding) == 1 and not len(encoding[0]) and encoding[0].attrib == {"style": style}
                    and length > 0 and size > 0 and heap + begin + length <= before.st_size, "invalid member encoding or range")
            ranges.append((begin, begin + length))
            files[full_name] = (begin, length, size, bound, style,
                               app._checksum(data, "archived-checksum"), app._checksum(data, "extracted-checksum"))
        require(set(files) == set(layout) and seen_directories == set(directories), "component member inventory differs")
        cursor = 0
        for begin, end in sorted(ranges):
            require(begin == cursor, "overlap, gap or unreferenced heap bytes")
            cursor = end
        require(heap + cursor == before.st_size, "trailing product bytes")
        identities, metadata = {}, {}
        for name, (begin, length, size, bound, style, stored_sha, decoded_sha) in files.items():
            stored, decoded, sha256, count = hashlib.sha1(usedforsecurity=False), hashlib.sha1(usedforsecurity=False), hashlib.sha256(), 0
            def chunks():
                for block in streams._region(handle, heap + begin, length):
                    stored.update(block)
                    yield block
            data = chunks() if style == "application/octet-stream" else streams._inflate(chunks(), bound, 47)
            retained = [] if name.rsplit("/", 1)[-1] in ("Distribution", "PackageInfo") else None
            for block in data:
                count += len(block)
                require(count <= size, "decoded member exceeds declared size")
                decoded.update(block)
                sha256.update(block)
                if retained is not None:
                    require(count <= MAX_METADATA, "metadata exceeds bound")
                    retained.append(block)
            require(count == size and stored.hexdigest() == stored_sha and decoded.hexdigest() == decoded_sha,
                    "member checksum or length differs")
            identities[name] = (sha256.hexdigest(), count)
            if retained is not None:
                metadata[name] = b"".join(retained)
        require(component.fingerprint(os.fstat(handle.fileno())) == component.fingerprint(before)
                and component.fingerprint(path.lstat()) == component.fingerprint(before), "package changed during readback")
    return identities, metadata


def _components(records: dict, version: str) -> tuple[dict, dict]:
    require(set(records) == set(NAMES), "owned component set differs")
    members, infos = {}, {}
    for name, identifier in zip(NAMES, IDENTIFIERS):
        path, maximum, _sha, _size, _node = records[name]
        identities, metadata = _index(path, maximum, _file_layout(name, maximum))
        info = app.xml_tree(metadata["PackageInfo"])
        require(info.tag == "pkg-info" and info.get("identifier") == identifier and info.get("version") == version,
                "component identity differs from full release")
        payload = info.findall("payload")
        if name == "app.pkg":
            require(len(payload) == 1 and len(info.findall("bundle")) == 1, "app component metadata missing")
            space = app._decimal(payload[0].get("installKBytes"), 2**63 - 1)
            require(space > 0, "invalid app space hint")
        else:
            require(not payload or len(payload) == 1 and payload[0].attrib == {"installKBytes": "0", "numberOfFiles": "0"},
                    "worker component unexpectedly has payload")
            space = 0
        members.update({name + "/" + leaf: identity for leaf, identity in identities.items()})
        infos[name] = (identifier, space, tuple(ET.tostring(bundle) for bundle in info.findall("bundle")))
    return members, infos


def _distribution_source(version: str, infos: dict) -> bytes:
    """Fixed mandatory local-system product; choice order is not install proof."""
    require(re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", version) is not None and set(infos) == set(NAMES), "invalid distribution inputs")
    root = ET.Element("installer-gui-script", minSpecVersion="2")
    ET.SubElement(root, "title").text = "Scribe"
    ET.SubElement(root, "options", customize="never", **{"require-scripts": "false", "allow-external-scripts": "false"})
    ET.SubElement(root, "domains", enable_localSystem="true", enable_currentUserHome="false", enable_anywhere="false")
    outline = ET.SubElement(root, "choices-outline")
    for name in ("app.pkg", "cpu-worker.pkg", "metal-worker.pkg"):
        identifier, space, _bundles = infos[name]
        ET.SubElement(outline, "line", choice=identifier)
        choice = ET.SubElement(root, "choice", id=identifier, title="Scribe", description="Required Scribe component",
                               visible="false", enabled="false", selected="true")
        ET.SubElement(choice, "pkg-ref", id=identifier)
        ET.SubElement(root, "pkg-ref", id=identifier, version=version, installKBytes=str(space)).text = name
    return ET.tostring(root, encoding="utf-8", xml_declaration=True)


def _semantic(node) -> tuple:
    require(not (node.tail or "").strip(), "unexpected distribution tail")
    return node.tag, tuple(sorted(node.attrib.items())), (node.text or "").strip(), tuple(_semantic(child) for child in node)


def _distribution(data: bytes, source: bytes, infos: dict) -> None:
    """Permit ONLY documented and independently checked producer additions.

    Unknown output attributes remain rejected, including unqualified attributes
    seen only as redacted counts in the inert observer. No opaque XML is removed.
    """
    root, expected = app.xml_tree(data), app.xml_tree(source)
    by_id = {value[0]: (name, value) for name, value in infos.items()}
    seen, references = set(), set()
    for node in list(root):
        if node.tag != "pkg-ref":
            continue
        identifier = node.get("id")
        require(identifier in by_id, "unknown distribution package reference")
        name, (_identity, space, bundles) = by_id[identifier]
        if len(node):
            require(node.attrib == {"id": identifier} and identifier not in seen
                    and not (node.text or "").strip() and len(node) == 1, "ambiguous generated bundle metadata")
            container = node[0]
            require(container.tag == "bundle-version" and not container.attrib and not (container.text or "").strip()
                    and tuple(_semantic(bundle) for bundle in container) == tuple(_semantic(app.xml_tree(bundle)) for bundle in bundles),
                    "generated bundle versions differ from own component")
            _semantic(node)  # Also check all tails before discarding checked metadata.
            seen.add(identifier)
            root.remove(node)
        else:
            wanted = expected.findall("pkg-ref")
            original = next(value for value in wanted if value.get("id") == identifier)
            require(identifier not in references and node.attrib == original.attrib
                    and node.get("installKBytes") == str(space) and node.text == "#" + name,
                    "distribution package version, size, controls or URL differ")
            node.text = name  # Exactly the documented local product URL rewrite.
            references.add(identifier)
    require(references == set(by_id) and _semantic(root) == _semantic(expected), "distribution semantics differ")


def _inspect_product(path: Path, records: dict, members: dict, infos: dict, distribution: bytes) -> None:
    layout = {"Distribution": ("application/x-gzip", MAX_METADATA)}
    for name in NAMES:
        layout.update({name + "/" + member: value for member, value in _file_layout(name, records[name][1]).items()})
    identities, metadata = _index(path, _product_bound(tuple(records[name][3] for name in NAMES)), layout, NAMES)
    require({name: value for name, value in identities.items() if name != "Distribution"} == members,
            "embedded component member bytes differ")
    _distribution(metadata["Distribution"], distribution, infos)
