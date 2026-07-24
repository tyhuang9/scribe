# Scribe Stitch handoff

This folder contains design evidence for Google Stitch project `projects/13126365166628126458` (**Scribe Design System**) and the native egui implementation contract in `../DESIGN.md`.

## Authority and export policy

The refined official Stitch PNG exports are directional visual evidence for the four product screens:

- `designs/transcribe-glassmorphic.png`
- `designs/models-glassmorphic.png`
- `designs/playground-glassmorphic.png`
- `designs/settings-glassmorphic.png`

The selected exploratory remote mock, `designs/selected-transcribe-mock.png`, is the full official remote export. It is directional evidence for the selected direction, but it is still an exploration rather than the final refined Transcribe screen.

`../DESIGN.md` is authoritative for native egui geometry, product behavior, accessibility, and verification. The refined web HTML uses an 80 px rail and a 72 px pill-style listening control; the native app deliberately adapts those details to the selected compact contract: a 60 px rail and a 56 px tactile recording control.

The matching official Stitch HTML exports are exact remote evidence and contain CDN JavaScript/runtime animation. They are intentionally local-only and ignored through six explicit `.gitignore` entries. Do not sanitize, modify, or commit them. The official PNG exports remain committable.

Stitch provided `designs/technical-spec-glassmorphic.html` but no corresponding official PNG. That HTML stays local-only. `designs/technical-spec.png` is the corrected local 60 px-rail documentation preview and is committable; `designs/technical-spec.html` is its local source.

## Exploratory local references

The three `transcribe-mock-{a,b,c}.html/.png` pairs remain synthetic local design explorations. Local mock B is explicitly a cropped interaction-band preview inspired by the selected direction; it is not the full official remote mock and must not be treated as the final screen. `base.css` supports only these local previews and the local technical-spec preview.

The obsolete `transcribe-final`, `models-final`, `playground-final`, and `settings-final` local HTML/PNG pairs were removed because they showed the legacy wide rail and contradicted the refined official exports.

## Native verification

The implementation must be visually checked at the native 1100x760 primary window and 840x600 minimum window defined in `src/main.rs`. The selected design contract uses a 60 logical px icon rail.
