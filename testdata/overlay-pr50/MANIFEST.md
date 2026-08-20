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
| `live-empty` | 600 × 62 | Live, Listening, empty transcript | light, dark | 96 |
| `compact-finalizing` | 320 × 52 | Minimal, Finalizing | light, dark | 96 |
| `live-error` | 600 × 62 | Live, Error, retryable `Microphone unavailable` | light, dark | 96 |

The suffix is the DPI (`96`, `120`, `144`, or `192`). Each `.bgra` file has
exactly `physical_width × physical_height × 4` bytes. The regression test
compares every byte against the renderer output; fixtures are not regenerated
by candidate code.

## Provenance

The source checkout was created with:

```powershell
$source = Join-Path $env:TEMP 'scribe-pr50-golden-source'
git worktree add --detach $source 1d50d02
git -C $source diff --exit-code
```

A temporary test-only dump routine was inserted beside the existing native
raster tests in that detached checkout, run with the command below, and then
removed. `git diff --exit-code` and the raster blob ID
`730a81ee3a466eced80e8400b799a5079d12bb0a` confirmed the source checkout was
back to the immutable PR #50 content after generation.

```powershell
cargo test --bin local-transcriber overlay::native_windows::raster::tests::dump_pr50_overlay_golden_frames -- --exact --nocapture
```

The six empty, non-listening, and error-state fixtures were generated the same
way from the immutable checkout with the temporary test named
`dump_pr50_edge_golden_frames`. Afterward, both `git diff --exit-code` and the
blob-ID check above passed again.

## Production-source lock

The regression suite normalizes CRLF to LF, hashes everything before the
`#[cfg(test)] mod tests` boundary, and requires SHA-256
`b55312b40a692f1edb70def84e7f374c2577fedcd0e8e1ad83d9b9bc9f9bf079`.
That boundary deliberately excludes tests so fixture/test maintenance can grow
without weakening the guarantee that the production raster implementation is
identical to PR #50.

## Opt-in candidate generator

The ignored `generate_pr50_overlay_fixture_candidate` test can render the full
corpus and its `SHA256SUMS` into an explicitly selected directory. It rejects
relative paths and every path inside this repository, so normal tests and even
an accidental opt-in cannot overwrite the committed fixtures. For example:

```powershell
$env:SCRIBE_PR50_GOLDEN_OUTPUT_DIR = Join-Path $env:TEMP 'scribe-pr50-overlay-candidate'
cargo test --bin local-transcriber overlay::native_windows::raster::tests::generate_pr50_overlay_fixture_candidate -- --ignored --exact --nocapture
```

Candidate output is diagnostic only. Updating committed goldens still requires
generation from the immutable PR #50 checkout and an explicit review of the
resulting hashes.

`SHA256SUMS` records a digest for every generated frame. The golden comparison
test does not compare only those digests: it reads the committed PR #50 frame
bytes and requires the new renderer output to match every byte. The separate
ignored candidate generator only writes diagnostic output outside the
repository.

## Tracked compositor references

The existing PR #50 Windows.Graphics.Capture evidence remains unchanged. Its
SHA-256 fingerprints in this branch are:

| Capture | SHA-256 |
| --- | --- |
| `live-light-6d5492c-39988.png` | `4499549427b02dc0bbe0f89edd8d14f952d2272384a9180994c2fa55c312030f` |
| `live-dark-6d5492c-39464.png` | `2bff0e555dbb3be761781de27413503136d683352be729acb1cbdf1d30b96bf6` |
| `compact-light-6d5492c-29856.png` | `d2b87f5d19987474db8076ac12a8ea27230495e68a09d2610a24cbefefff7bdb` |
| `compact-dark-6d5492c-31940.png` | `2ac4bbcbcaef69bae0751ed0cf19cdd01fab2258e7b49352ed6f145e0cfa4ed6` |
