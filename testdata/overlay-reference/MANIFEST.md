# Native overlay reference-contract fixtures

These raw, top-down premultiplied-BGRA fixtures lock the accepted native
renderer for the selected overlay reference. They intentionally supersede the
earlier PR #50 source-pixel lock: the supplied image is the visual contract,
while these deterministic frames are the automated regression contract after
same-state design QA.

## Selected reference

- Tracked source: `design-qa-evidence/overlay-native/reference-source.png`
- Original attachment provenance:
  `C:\Users\huang\.codex\attachments\8c74ab60-cbfb-4a09-8ee4-cade05f597f2\image-1.png`
- Dimensions: 1175 x 152 pixels
- SHA-256: `0ea2c3df19e1f40346fda7e5499b5815a6c8b601a6cc9b95e23ccbddffb19bf6`
- Reported regression source:
  `design-qa-evidence/overlay-native/reported-live-overlay-top-line.png`
- Reported source dimensions: 769 x 89 pixels
- Reported source SHA-256:
  `15a6791e254722290c59e4c7966260cabe80f6e0db3acd7710d7e722bad97949`
- Required Live order: static Scribe brand mark, elapsed time, divider, live
  transcript, and the existing cancel control at the right.
- When rolling preview did not start, Live retains the same shell with brand,
  elapsed time, and cancel control; divider and transcript are absent during
  normal recording phases. A subsequent general capture error remains visible
  and announced. Compact (the serialized `Minimal` mode) uses the same visual
  shell, brand, timer, and cancel control without a divider or transcript
  while listening; it replaces the frozen timer with the current lifecycle
  status after recording ends.

The implementation uses the bundled Phosphor `WAVEFORM` glyph as the static
Scribe brand mark. It does not fabricate a logo or animate the mark from audio
levels. Elapsed time and transcript use one regular Segoe UI face and the same
restrained light-gray token. Transcript display stays left-aligned while the
full line fits. Once it overflows, the visible window follows the newest
Unicode-grapheme-safe suffix, keeps its right edge fixed, and does not add an
ellipsis. The full committed and tentative text remains intact in state and
accessibility wording even though the selected reference calls for one
continuous visual style.

At 96, 120, 144, and 192 DPI, the brand, elapsed text ink, divider, transcript
ink, and Compact brand/timer elements are measured from one physical centerline.
The raster and UI Automation trees consume the same physical rectangles. The
separate cancel window remains a 44 logical-pixel target, shares the display's
physical center, and uses the reference-specific 16 logical-pixel right inset
in both Live and Compact modes.

The dark capsule uses a neutral translucent surface compensated for the native
shadow stack. Over the reference backdrop (approximately RGB 240/240/245), an
unpainted center pixel composites to RGB 87/87/94; comparable source pixels are
approximately RGB 81-85/82-86/86-90. The slightly lighter purple mark is an
accessibility-constrained variant that maintains at least 3:1 non-text
contrast. Native and fallback regressions also verify at least 4.5:1 for normal
muted, error, and warning text over black and white backdrop extremes.

## Corpus

The standard Live fixture records `00:10` and composes committed text
`Alright, What is going on? Why is there a line on` with tentative text
`That's pretty cool. These newest words stay visible.`. The combined line is
long enough to exercise tail-follow behavior.

| Fixture prefix | Logical geometry | Modes and state | Themes | DPI |
| --- | --- | --- | --- | --- |
| `live` | 600 x 62 | Live, Listening, preview available | light, dark | 96, 120, 144, 192 |
| `compact` | 200 x 62 | Compact (`Minimal`), Listening | light, dark | 96, 120, 144, 192 |
| `cancel` | 44 x 44 | independent cancel control | light, dark | 96, 120, 144, 192 |
| `live-empty` | 600 x 62 | Live, Listening, empty started preview | light, dark | 96 |
| `live-no-preview` | 600 x 62 | Live, Listening, preview unavailable | light, dark | 96 |
| `compact-finalizing`, `compact-processing`, `compact-pasting`, `compact-success` | 200 x 62 | Compact (`Minimal`) lifecycle states, static reduced-motion glyph for active work; Success uses a green check-circle with `Done` | light, dark | 96 |
| `live-finalizing`, `live-processing`, `live-pasting`, `live-success` | 600 x 62 | Live lifecycle states with recorded-time context and static reduced-motion glyph for active work; Success uses a green check-circle with `Done` | light, dark | 96 |
| `live-error` | 600 x 62 | Live, retryable preview error with recorded-time context | light, dark | 96 |

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

The tracked approval artifacts make the review reproducible without the
temporary attachment path:

- `reference-source.png` SHA-256:
  `0ea2c3df19e1f40346fda7e5499b5815a6c8b601a6cc9b95e23ccbddffb19bf6`
- `reference-contract-live-dark.png` SHA-256:
  `913cb4d8b587295eeea2bc679d9bf19a0686abb7ac783bd29141c885b2a60b79`
- `reference-contract-comparison.png` SHA-256:
  `f428ec83234fffad9e7b084c6164959af942e8c21d91ed53ceaa5c13109cc223`
- `reference-contract-wgc-live-dark-96.png` SHA-256:
  `3878d61ba5e2b50e3869ab713d0433d8ab8213f8fabcf45ee2fe76043f51ed0d`
- `reference-contract-wgc-live-dark-96.json` SHA-256:
  `a637e23137a6fd8bcfa8ff29b5f90f964731b674f613070953a956f45de280e0`
- `reported-live-overlay-top-line.png` SHA-256:
  `15a6791e254722290c59e4c7966260cabe80f6e0db3acd7710d7e722bad97949`
- `reported-vs-revised-live-light-120.png` SHA-256:
  `0de32080b7e5d22accfcd46b96369b7896d6a0f0dbc1c98bab9eaf716193f250`
- `revised-live-compact-light-120.png` SHA-256:
  `c91961928bd7e3838e7ec0c06d9f0b5a106b0f6f387dc98d68aa54a6efe5dad9`
- `revised-live-compact-dark-120.png` SHA-256:
  `65cff8bd116fefba077a57ef19e38a8a9d53dc8febcf05b5cb7320e7f06b8365`
- `revised-wgc-live-light.png` / `.json` SHA-256:
  `db1064074985a3b830c31a7f250b63eacf64070aa237deba5423d8107c5dda89` /
  `c34345a165e1aafe7a41d2706ff978b9cb3bbc8e350f920d291080fa00683357`
- `revised-wgc-live-dark.png` / `.json` SHA-256:
  `a9b671024e2cb4028f862e3fd3cc942bb986931b86bb45acf4b16d96ba2de6c3` /
  `c9b410550ae31f31e7a69c816f26325f0e4250fdf7381a604511873d6f3e028f`
- `revised-wgc-compact-light.png` / `.json` SHA-256:
  `a03361c48ff158afd7b168628e7cd06e21dbcb89bb15af63ca24ab7c844993d8` /
  `8fb295476ac84e1ffcebb7308a5a7031efe76d03fae4acd79be531ed7612b6b9`
- `revised-wgc-compact-dark.png` / `.json` SHA-256:
  `1414f7dbdb7ee07cd8b964df88810ac467a4e8c6854b05f33df154fd212f3103` /
  `448184396f1b9d8ebb3ef090b9c3ac989c302aeb386b5c44ab2a6463f72fd24e`

The revised WGC set was captured from source head `bf4be29` with the exact
isolated executable SHA-256
`2aad0bc56e36812859035ebec3ac2b6185a262a9520ceb39bac1f5872e8bd55e`.
Each manifest records hardware D3D capture, 120 DPI, visible/uncloaked layered
windows, an unchanged foreground HWND, and the expected 750 x 78 Live or
250 x 78 Compact display plus 55 x 55 cancel control.

The final Product Design and specialist UI/UX acceptance record is the latest
"Native overlay tail-follow and Compact-shell revision" section of
`design-qa.md`.
