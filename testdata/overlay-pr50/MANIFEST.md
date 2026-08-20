# PR50 native overlay raster fixtures

These raw, top-down premultiplied-BGRA fixtures were generated from the clean,
immutable checkout of `tyhuang9/scribe` commit `1d50d02` (PR #50 head), using
the Windows native `NativeRasterizer` and its embedded Phosphor font.

The captured state is the existing raster-test state: Listening, elapsed
`00:12`, RMS `0.65`, peak `0.82`, committed text `The native overlay keeps the
latest committed phrase`, and tentative text ` and this tentative ending`.
The cancel-control fixture is the independent 44 logical-pixel control.

| Fixture prefix | Logical geometry | Modes | Themes | DPI |
| --- | --- | --- | --- | --- |
| `live` | 600 × 62 | Live | light, dark | 96, 120, 144, 192 |
| `compact` | 320 × 52 | Minimal | light, dark | 96, 120, 144, 192 |
| `cancel` | 44 × 44 | cancel control | light, dark | 96, 120, 144, 192 |

The suffix is the DPI (`96`, `120`, `144`, or `192`). Each `.bgra` file has
exactly `physical_width × physical_height × 4` bytes. The regression test
compares every byte against the renderer output; fixtures are not regenerated
by candidate code.

## Provenance

The source checkout was created with:

```powershell
git worktree add --detach C:\Users\huang\Documents\Projects\scribe-pr50-golden-source 1d50d02
git -C C:\Users\huang\Documents\Projects\scribe-pr50-golden-source diff --exit-code
```

A temporary test-only dump routine was inserted beside the existing native
raster tests in that detached checkout, run with the command below, and then
removed. `git diff --exit-code` and the raster blob ID
`730a81ee3a466eced80e8400b799a5079d12bb0a` confirmed the source checkout was
back to the immutable PR #50 content after generation.

```powershell
cargo test --bin local-transcriber overlay::native_windows::raster::tests::dump_pr50_overlay_golden_frames -- --exact --nocapture
```

`SHA256SUMS` records a digest for every generated frame. The candidate test
does not compare only those digests: it reads the PR #50 bytes and requires the
new renderer output to match every byte.

## Tracked compositor references

The existing PR #50 Windows.Graphics.Capture evidence remains unchanged. Its
SHA-256 fingerprints in this branch are:

| Capture | SHA-256 |
| --- | --- |
| `live-light-6d5492c-39988.png` | `4499549427b02dc0bbe0f89edd8d14f952d2272384a9180994c2fa55c312030f` |
| `live-dark-6d5492c-39464.png` | `2bff0e555dbb3be761781de27413503136d683352be729acb1cbdf1d30b96bf6` |
| `compact-light-6d5492c-29856.png` | `d2b87f5d19987474db8076ac12a8ea27230495e68a09d2610a24cbefefff7bdb` |
| `compact-dark-6d5492c-31940.png` | `2ac4bbcbcaef69bae0751ed0cf19cdd01fab2258e7b49352ed6f145e0cfa4ed6` |
