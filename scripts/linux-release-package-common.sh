#!/usr/bin/env bash

linux_regular_file_bytes() {
  local root="$1"
  find "$root" -type f -printf '%s\n' | awk '{ total += $1 } END { printf "%.0f\n", total + 0 }'
}

linux_require_x86_64_elf() {
  local path="$1" label="$2"
  python3 - "$path" "$label" <<'PY'
import pathlib, struct, sys
label = sys.argv[2]
with pathlib.Path(sys.argv[1]).open("rb") as source:
    header = source.read(24)
valid = (
    len(header) == 24
    and header[:4] == b"\x7fELF"
    and header[4:7] == b"\x02\x01\x01"
    and header[7] == 0
    and struct.unpack_from("<H", header, 16)[0] in (2, 3)
    and struct.unpack_from("<H", header, 18)[0] == 62
    and struct.unpack_from("<I", header, 20)[0] == 1
)
if not valid:
    raise SystemExit(f"{label} must be ELF64 little-endian x86_64 ET_EXEC or ET_DYN")
PY
}
