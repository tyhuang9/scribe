# UI harness

The deterministic UI harness is development-only. It uses the shared shell and
egui primitives, never starts audio capture, model downloads, or settings
writes, and is unavailable from release builds.

Run a fixture in a debug build:

```powershell
$env:SCRIBE_UI_HARNESS = 'models/card-expanded'
$env:SCRIBE_UI_HARNESS_VIEWPORT = '1180x815'
cargo run --features ui-harness
```

Unknown fixture names fail closed to normal application startup. The harness
freezes timers, relative time, and sample metadata for repeatable screenshots.
The visual references in `docs/ui-reference/` are documentation-only and are
not runtime assets.

## Fixture routes

- `transcribe/no-model`
- `transcribe/ready`
- `transcribe/listening`
- `transcribe/finalizing`
- `transcribe/no-speech`
- `transcribe/microphone-error`
- `models/installed` — installed cards, including a ready inactive local card
  for whole-card selection and keyboard-focus checks.
- `models/lifecycle` — active, installed, available/Install, partial,
  downloading, and failed lifecycle rows.
- `models/card-expanded` — the inline `tiny.en` card expanded with a long
  description, English/Spanish/Japanese metadata, all eight known feature
  capabilities, Requirements, and a fixture-only Repair maintenance control.
- `models/compare-expanded`
- `history`
- `settings/recording`

## Model-card contract

- Cards use the full usable Models-route width. At widths at least 620 px, the
  summary uses deterministic 60/40 identity and metric/control tracks. Below
  620 px, identity content stacks above trailing controls. The native shell
  has a 960 × 680 minimum; 375 px component behavior is deterministic-test
  coverage only.
- Expanded details grow inside the same card surface and preserve the collapsed
  card's left and right edges. The description wraps in place, language codes
  switch to full language names in the existing identity metadata row, and
  there are no duplicate `DESCRIPTION` or `LANGUAGES` headings.
- Known capabilities render all eight feature glyphs: Batch transcription,
  Native streaming, Cancellation, Word timestamps, Translation, Automatic
  language detection, Confidence scores, and Custom vocabulary. The collapsed
  feature group wraps glyphs and exposes hover tooltips; the expanded surface
  renders the same capabilities as a wrapping icon-and-label grid.
- A ready inactive installed card is selectable across the full card with
  pointer click, Enter, Space, and AccessKit Default activation. Lifecycle,
  disclosure, maintenance, and partial-cleanup child controls are excluded
  from the card activation hit area and keep their independent behavior.
- Available Install actions use the inverse-neutral Download glyph treatment
  with the compact artifact size. Stable Delete keeps its destructive outline
  treatment without changing the underlying action or accessible name.
- Requirements use responsive RAM, storage, and GPU cells. Optional
  Maintenance appears after Requirements; supported removal, partial-resume,
  discard, and runtime-safety constraints remain unchanged by expansion.

## Native manual states

Capture and inspect original-resolution client-area screenshots at exactly
1180 × 815 and 960 × 680:

1. `models/installed`: idle, hover a selectable inactive-card coordinate, and
   keyboard-focus `Select whisper.cpp tiny.en`.
2. `models/lifecycle`: inspect the inverse Install glyph and compact size,
   then confirm partial/download/error controls remain truthful and contained.
3. `models/card-expanded`: inspect expanded top content, then scroll the
   Models route to inspect the complete feature grid, Requirements, optional
   Maintenance order, and shared card edges.

Reject captures that have a black frame, stale process content, partial window,
or a client rectangle other than the requested viewport. Manual screenshots do
not replace deterministic tests for AccessKit action dispatch, child-control
isolation, 375 px containment, or removal/partial safety.
