"""Observe one configured inert app package; NOT package admission or installation.

Only this invocation's fresh fixed fixture is read. The BOM listing is partial
semantic evidence, not binary BOM validation: flags, ACLs, hard-link tables and
Installer effects remain unqualified. No payload is extracted or executed.
The system pkgbuild, lsbom and cksum tools and non-hostile local writers are
assumptions. Vendor-authored manuals describe the requested fields; native CI
establishes their actual representation, not a portable invented schema.
"""
from __future__ import annotations

import json
import os
from pathlib import Path
import plistlib
import re
import stat
import sys
import tempfile

import macos_cpu_component as component
import macos_cpu_pkg_profile as cpu
import macos_product_pkg_profile as product

APP = "ScribeSourceFixture.app"
IDENTIFIER = "com.scribe.source-test.app-payload-profile"
VERSION = "1.0.0"
LONG_PATH = APP + "/Contents/Resources/" + "/".join(("a" * 90, "b" * 90, "c" * 70)) + "/data.txt"
FILES = {
    APP + "/Contents/Info.plist": plistlib.dumps({
        "CFBundleIdentifier": IDENTIFIER, "CFBundleName": "Scribe Source Fixture",
        "CFBundlePackageType": "APPL", "CFBundleExecutable": "fixture",
        "CFBundleVersion": VERSION, "CFBundleShortVersionString": VERSION}),
    APP + "/Contents/MacOS/fixture": b"# Inert app; no interpreter.\n",
    LONG_PATH: b"Fixed long-path resource.\n",
}
CONTROLS = [{"RootRelativeBundlePath": APP, "BundleIsRelocatable": False,
             "BundleIsVersionChecked": False, "BundleHasStrictIdentifier": True,
             "BundleOverwriteAction": "upgrade"}]
MAX_BYTES = 64 * 1024
MAX_RECORDS = 32
MAX_NAME = 1024
MAX_REPORT = 32 * 1024


def require(condition: bool, message: str) -> None:
    component.require(condition, "app profile: " + message)


def _inventory() -> dict[str, tuple[int, bytes]]:
    result = {".": (0o40755, b"")}
    for name, data in FILES.items():
        result["./" + name] = (0o100755 if name.endswith("/fixture") else 0o100644, data)
        for parent in Path(name).parents:
            if str(parent) != ".":
                result["./" + parent.as_posix()] = (0o40755, b"")
    return result


def _label(name: str) -> str:
    if name == ".":
        return "payload-root"
    if name == "./" + LONG_PATH:
        return "long-resource"
    if name in {"./" + value for value in FILES}:
        return "executable" if name.endswith("/fixture") else "info-plist"
    return "app-directory" if name in _inventory() else "unrecognized"


def _mode(value: int) -> str:
    return {0o40755: "directory-0755", 0o100755: "regular-0755", 0o100644: "regular-0644",
            0: "zero"}.get(value, "other")


def _odc(data: bytes) -> dict:
    """Bounded generated-fixture decoder; independent of production CPU limits."""
    require(type(data) is bytes and 0 < len(data) <= MAX_BYTES, "payload bound")
    expected, rows, seen, offset = _inventory(), [], set(), 0
    while True:
        require(offset + 76 <= len(data) and data[offset:offset + 6] == b"070707", "ODC header")
        fields, cursor = [], offset + 6
        for width in (6, 6, 6, 6, 6, 6, 6, 11, 6, 11):
            field = data[cursor:cursor + width]
            require(all(48 <= char <= 55 for char in field), "ODC numeric field")
            fields.append(int(field, 8))
            cursor += width
        device, _inode, mode, uid, gid, links, rdev, _mtime, namesize, size = fields
        require(2 <= namesize <= MAX_NAME and size <= MAX_BYTES, "ODC record bound")
        endname, end = cursor + namesize, cursor + namesize + size
        require(end <= len(data), "ODC truncated record")
        raw = data[cursor:endname]
        require(raw[-1:] == b"\0" and b"\0" not in raw[:-1], "ODC name terminator")
        try:
            name = raw[:-1].decode("ascii")
        except UnicodeError:
            raise component.ComponentError("app profile: ODC non-ASCII fixture name") from None
        body = data[endname:end]
        if name == "TRAILER!!!":
            require(not body and len(data) - end <= 4096 and not any(data[end:]), "ODC trailer")
            return {"records": rows, "inventory_matches": seen == set(expected) and len(rows) == len(expected),
                    "trailer_mode_class": _mode(mode), "trailer_links_one": links == 1,
                    "trailer_uid_zero": uid == 0, "trailer_gid_zero": gid == 0,
                    "trailer_device_zero": device == 0, "trailer_rdev_zero": rdev == 0,
                    "zero_padding_bytes": len(data) - end}
        require(len(rows) < MAX_RECORDS, "ODC record count")
        wanted = expected.get(name)
        rows.append({"path_class": _label(name), "duplicate": name in seen,
                     "long_name": namesize > 256, "mode_class": _mode(mode),
                     "expected_mode_matches": wanted is not None and mode == wanted[0],
                     "uid_class": "root" if uid == 0 else "nonzero",
                     "gid_class": "wheel" if gid == 0 else "nonzero",
                     "links_one": links == 1, "links_count": links if links <= MAX_RECORDS else None,
                     "device_zero": device == 0, "rdev_zero": rdev == 0,
                     "size": size, "bytes_match": wanted is not None and body == wanted[1]})
        seen.add(name)
        offset = end


def _decimal(value: str, maximum: int) -> int | None:
    if re.fullmatch(r"0|[1-9][0-9]{0,11}", value) is None:
        return None
    number = int(value)
    return number if number <= maximum else None


def _bom_listing(data: bytes, checksums: dict[str, int]) -> dict:
    """Observe requested fmugsc columns, without guessing absent/unknown fields."""
    require(type(data) is bytes and len(data) <= MAX_BYTES and data.endswith(b"\n"), "BOM listing bound or tail")
    try:
        text = data.decode("ascii")
    except UnicodeError:
        raise component.ComponentError("app profile: BOM listing encoding") from None
    lines = text.splitlines()
    require(0 < len(lines) <= MAX_RECORDS, "BOM listing count")
    expected, seen, rows = _inventory(), set(), []
    for line in lines:
        fields = line.split("\t")
        name = fields[0]
        wanted = expected.get(name)
        row = {"path_class": _label(name), "field_count": len(fields), "duplicate": name in seen,
               "mode_class": "unobserved", "expected_mode_matches": False,
               "uid_class": "unobserved", "gid_class": "unobserved",
               "size_matches": None, "checksum_matches": None}
        row["value_classes"] = ["empty" if not value else "bounded-decimal" if
            _decimal(value, 0xffffffffffff) is not None else "other" for value in fields[1:7]]
        if 4 <= len(fields) <= 6:
            mode = int(fields[1], 8) if re.fullmatch(r"[0-7]{1,6}", fields[1]) else -1
            row.update(mode_class=_mode(mode), expected_mode_matches=wanted is not None and mode == wanted[0],
                       uid_class="root" if fields[2] == "0" else "other",
                       gid_class="wheel" if fields[3] == "0" else "other")
            if len(fields) == 6 and wanted is not None and stat.S_ISREG(wanted[0]):
                row["size_matches"] = _decimal(fields[4], MAX_BYTES) == len(wanted[1])
                row["checksum_matches"] = (name in checksums and
                    _decimal(fields[5], 0xffffffff) == checksums[name])
        rows.append(row)
        seen.add(name)
    return {"records": rows, "inventory_matches": seen == set(expected) and len(rows) == len(expected),
            "flags_acl_xattrs_hardlink_tables": "unobserved", "large_files": "unobserved"}


def _package_info(data: bytes) -> dict:
    root = product._xml(data)
    payload = root.findall("payload")
    controls = []
    for tag in ("bundle", "bundle-version", "upgrade-bundle", "update-bundle",
                "atomic-update-bundle", "strict-identifier", "relocate"):
        nodes = root.findall(tag)
        bundles = [item for node in nodes for item in ([node] if tag == "bundle" else node.findall("bundle"))]
        controls.append({"tag": tag, "container_count": len(nodes),
                         "bundle_count": len(bundles),
                         "identifiers_match": all(item.get("id") == IDENTIFIER for item in bundles) if bundles else None,
                         "paths_match": all(node.get("path") == APP for node in nodes) if tag == "bundle" and nodes else None})
    return {"shapes": product._xml_shapes(data), "root_fields": cpu._package_info_attribute_shapes(root),
            "root_identity_matches": root.tag == "pkg-info" and root.get("identifier") == IDENTIFIER
                and root.get("version") == VERSION and root.get("install-location") == "/Applications",
            "controls": controls, "payload_count": len(payload),
            "number_of_files": [_decimal(node.get("numberOfFiles", ""), MAX_RECORDS) for node in payload],
            "install_kbytes": [_decimal(node.get("installKBytes", ""), MAX_BYTES) for node in payload]}


def _identity(path: Path) -> tuple:
    info = path.lstat()
    return (*product._identity(path), info.st_uid, info.st_gid) if hasattr(info, "st_uid") else product._identity(path)


def _owned_probe() -> dict:
    with component.private_workspace(Path(tempfile.gettempdir()), "scribe-app-profile-") as workspace:
        root, temporary = workspace / "payload", workspace / "tmp"
        root.mkdir(mode=0o755)
        root.chmod(0o755)
        temporary.mkdir(mode=0o700)
        for name, (mode, data) in sorted(_inventory().items(), key=lambda item: (item[0].count("/"), item[0])):
            if name == ".":
                continue
            path = root / name.removeprefix("./")
            if stat.S_ISDIR(mode):
                path.mkdir(mode=0o755)
                path.chmod(0o755)
            else:
                product._write(path, data, stat.S_IMODE(mode))
        controls = workspace / "components.plist"
        product._write(controls, plistlib.dumps(CONTROLS), 0o600)
        files = {path: _identity(path) for path in (controls, *(root / name for name in FILES))}
        directories = {root / name.removeprefix("./"): product._directory_identity(root / name.removeprefix("./"))
                       for name, (mode, _) in _inventory().items() if stat.S_ISDIR(mode)}

        def unchanged():
            require(all(_identity(path) == value for path, value in files.items())
                    and all(product._directory_identity(path) == value for path, value in directories.items()),
                    "owned input changed")

        environment = {**component.probe_environment(), "TMPDIR": str(temporary)}
        def run(args):
            unchanged()
            result = component.capture(args, timeout=30, limit=MAX_BYTES, error_limit=8192,
                                       environment=environment, cwd=workspace)
            unchanged()
            return result

        target = workspace / "app.pkg"
        run(["/usr/bin/pkgbuild", "--root", str(root), "--component-plist", str(controls),
             "--ownership", "recommended", "--compression", "legacy", "--identifier", IDENTIFIER,
             "--version", VERSION, "--install-location", "/Applications", str(target)])
        files[target] = _identity(target)
        tree = product._xar(component.read_bounded(target, product.MAX_PACKAGE))
        members = tree["members"]
        require(set(members) == {"PackageInfo", "Payload", "Bom"}
                and all(item["decoded"] is not None for item in members.values()), "fixture member format")
        payload = members["Payload"]["decoded"]
        compression = "gzip" if payload.startswith(b"\x1f\x8b") else "none-odc"
        if payload.startswith(b"\x1f\x8b"):
            payload = product._inflate(payload, MAX_BYTES)
        payload_report = _odc(payload)
        bom = workspace / "owned.bom"
        require(0 < len(members["Bom"]["decoded"]) <= MAX_BYTES, "BOM member bound")
        product._write(bom, members["Bom"]["decoded"], 0o600)
        files[bom] = _identity(bom)
        checksums = {}
        for name, data in FILES.items():
            path = root / name
            result = run(["/usr/bin/cksum", str(path)])
            # Compare the exact known path privately; never emit raw tool output.
            match = re.fullmatch(rb"([0-9]{1,10}) ([0-9]{1,12}) " + re.escape(os.fsencode(path)) + rb"\n", result)
            require(match is not None and int(match[1]) <= 0xffffffff and int(match[2]) == len(data), "checksum output")
            checksums["./" + name] = int(match[1])
        listing = run(["/usr/bin/lsbom", "-p", "fmugsc", str(bom)])
        report = {"observation_only": True, "package_admission": False, "signature_verification": False,
                  "installer_effects": "unobserved", "owned_inputs_unchanged": True,
                  "source_entry_count": len(_inventory()), "source_file_bytes": sum(map(len, FILES.values())),
                  "source_long_path_over_256": len(LONG_PATH) > 256,
                  "members": product._tree_shapes(tree), "payload_compression": compression,
                  "package_info": _package_info(members["PackageInfo"]["decoded"]),
                  "payload": payload_report, "bom_listing": _bom_listing(listing, checksums)}
        unchanged()
        require(len(json.dumps(report)) <= MAX_REPORT, "report bound")
        return report


def probe() -> dict:
    require(sys.platform == "darwin" and os.geteuid() != 0, "requires unprivileged Mac")
    try:
        return _owned_probe()
    except component.CleanupUncertain as error:
        raise error from None
    except OSError:
        raise component.ComponentError("app profile: owned filesystem operation failed") from None


def main() -> None:
    require(len(sys.argv) == 1, "fixed fixture accepts no arguments")
    print(json.dumps(probe(), sort_keys=True, separators=(",", ":")))


if __name__ == "__main__":
    main()
