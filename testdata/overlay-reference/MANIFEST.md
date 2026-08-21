# Native overlay reference-contract fixtures

These raw, top-down premultiplied-BGRA fixtures lock the accepted native
renderer for the selected overlay reference. They intentionally supersede the
earlier PR #50 source-pixel lock: the supplied image is the visual contract,
while these deterministic frames are the automated regression contract after
same-state design QA.

## Selected reference

- Source: `C:\Users\huang\.codex\attachments\8c74ab60-cbfb-4a09-8ee4-cade05f597f2\image-1.png`
- Dimensions: 1175 x 152 pixels
- SHA-256: `0ea2c3df19e1f40346fda7e5499b5815a6c8b601a6cc9b95e23ccbddffb19bf6`
- Required Live order: static Scribe brand mark, elapsed time, divider, live
  transcript, and the existing cancel control at the right.
- When rolling preview did not start, Live retains the same shell with brand,
  elapsed time, and cancel control; divider and transcript are absent. Minimal
  mode remains a distinct user-selected presentation.

The implementation uses the bundled Phosphor `WAVEFORM` glyph as the static
Scribe brand mark. It does not fabricate a logo or animate the mark from audio
levels. Elapsed time and transcript use one regular Segoe UI face and the same
restrained light-gray token. Long transcript display preserves the beginning
and adds a trailing ellipsis. Committed and tentative text remain distinct in
state and accessibility wording even though the selected reference calls for
one continuous visual style.

At 96, 120, 144, and 192 DPI, the brand, elapsed text ink, divider, transcript
ink, and compact status elements are measured from one physical centerline.
The raster and UI Automation trees consume the same physical rectangles. The
separate cancel window remains a 44 logical-pixel target, shares the display's
physical center, and uses the reference-specific 16 logical-pixel right inset
in Live mode.

The dark capsule uses a neutral translucent surface compensated for the native
shadow stack. Over the reference backdrop (approximately RGB 240/240/245), an
unpainted center pixel composites to RGB 87/87/94; comparable source pixels are
approximately RGB 81-85/82-86/86-90. The slightly lighter purple mark is an
accessibility-constrained variant that maintains at least 3:1 non-text
contrast. Native and fallback regressions also verify at least 4.5:1 for normal
muted, error, and warning text over black and white backdrop extremes.

## Corpus

The standard Live fixture records `00:12` and composes committed text
`Clicking the settings icon in the top` with tentative text `right...`.

| Fixture prefix | Logical geometry | Modes and state | Themes | DPI |
| --- | --- | --- | --- | --- |
| `live` | 600 x 62 | Live, Listening, preview available | light, dark | 96, 120, 144, 192 |
| `compact` | 320 x 52 | Minimal, Listening | light, dark | 96, 120, 144, 192 |
| `cancel` | 44 x 44 | independent cancel control | light, dark | 96, 120, 144, 192 |
| `live-empty` | 600 x 62 | Live, Listening, empty started preview | light, dark | 96 |
| `live-no-preview` | 600 x 62 | Live, Listening, preview unavailable | light, dark | 96 |
| `compact-finalizing` | 320 x 52 | Minimal, Finalizing | light, dark | 96 |
| `live-error` | 600 x 62 | Live, retryable preview error | light, dark | 96 |

The suffix is the DPI. Every `.bgra` file contains exactly
`physical_width x physical_height x 4` bytes. `SHA256SUMS` records every frame,
and normal tests both verify the checksums and compare every committed byte to
a fresh render.

## Reproduction and approval

The ignored generator refuses relative paths and paths inside the repository,
so candidate code cannot silently approve its own fixtures. Generate into a
new external directory:

```powershell
$candidatePath = Join-Path $env:TEMP ('scribe-overlay-reference-' + [guid]::NewGuid().ToString('N'))
$env:SCRIBE_OVERLAY_REFERENCE_OUTPUT_DIR = $candidatePath
cargo test --all-features overlay::native_windows::raster::tests::generate_reference_contract_overlay_fixture_candidate -- --ignored --exact --nocapture
```

Copying candidate bytes into this directory requires explicit visual review of
the composited Live state against the source image, review of light/dark and
edge states, and acceptance of the new hashes. The tracked comparison and
review history live in `design-qa-evidence/overlay-native/` and
`design-qa.md`. These fixtures do not claim that a translucent screenshot can
be pixel-identical across arbitrary desktop backgrounds; they lock the
approved renderer that satisfies the reference composition and measured
contract.
