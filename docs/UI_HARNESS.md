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
- `models/download-downloading` — isolated known-total download with Pause and
  Discard partial controls.
- `models/download-retained` — cancelled download retaining resumable bytes.
- `models/download-failed-partial` — failed download retaining resumable bytes.
- `models/download-failed-alert` — failed-without-partial state; click the
  named warning control to capture its error alert open and press Escape or
  click outside to verify dismissal.
- `models/card-idle` and `models/card-focus` — stable installed-card surfaces
  for idle, hover, and keyboard-focus captures.
- `models/card-expanded` — the inline `tiny.en` card expanded with a long
  description, compact English/Spanish/Japanese metadata, all eight internal
  capability flags for verifying the four-feature UI filter, Requirements,
  and a fixture-only Repair maintenance control.
- `models/compare-expanded`
- `history`
- `settings/recording`

## Model-card contract

- Cards use the full usable Models-route width. At widths at least 620 px, the
  summary uses deterministic nested zones: 50% identity, 24% Speed/Accuracy,
  and 26% lifecycle. The fixed 44 x 44 disclosure target lives inside the
  lifecycle zone. Below 620 px, identity content stacks above metrics,
  features, lifecycle, and disclosure controls. The native shell
  has a 960 × 680 minimum; 375 px component behavior is deterministic-test
  coverage only.
- Expanded details grow inside the same card surface and preserve the collapsed
  card's left and right edges. A one-line description retains the summary's
  18 px preview geometry when expanded; only wrapped copy grows downward from
  that original identity position. Language codes remain compact in the
  existing identity metadata row, and there are no
  duplicate `DESCRIPTION` or `LANGUAGES` headings. Full language names remain
  available through the metadata tooltip and accessibility description.
- Model cards expose only Native streaming, Translation, Word timestamps, and
  Batch transcription, in that order. Cancellation, language detection,
  confidence scores, and custom vocabulary remain internal metadata. The
  collapsed feature group uses at most two 28 x 32 px glyph columns with 8 px
  column and row gaps: one or two features occupy one row, while three or four
  use a second row. Every visible glyph has a hover tooltip and the cluster is
  one accessibility group. The expanded surface renders the same four
  capabilities as a 32 px icon-and-label grid.
- Speed and Accuracy occupy equal cells in the 24% metrics zone. Their visible
  labels sit above continuous 7 px meters, use the same five-bin color mapping,
  and expose AccessKit Meter names and values. Unknown ratings keep an empty
  track without inventing a numeric value. Expanded sections use 6 px divider
  and heading-to-content gaps plus 12 px between Features, Requirements, and
  Maintenance; requirement cells retain their natural compact height.
- A ready inactive installed card is selectable across the full card with
  pointer click, Enter, Space, and AccessKit Default activation. Lifecycle,
  disclosure, maintenance, and partial-cleanup child controls are excluded
  from the card activation hit area and keep their independent behavior.
- Available Install actions use the inverse-neutral Download glyph treatment.
  Stable Delete keeps its destructive outline treatment without changing the
  underlying action or accessible name. Active downloads replace the lifecycle
  action body with a truthful progress module: known totals show a stable-width
  downloaded/total byte label and a 6 px track plus current fill;
  unknown totals show `downloaded / Total unknown` without a fabricated fill.
  Named 44 x 44 Pause or Play and discard-partial controls remain beside the
  progress body: the byte label stays above the track, while Play/Pause and
  discard controls share the track row whenever it fits (and only wrap below
  the track when it cannot). Failed downloads expose their complete error
  through a separate
  accessible Warning popover. Remote cards show progress only after validated
  live or retained byte metadata is available.
- Requirements use responsive RAM, storage, and GPU cells. Optional
  Maintenance appears after Requirements; supported removal, partial-resume,
  discard, and runtime-safety constraints remain unchanged by expansion.

## Native manual states

Run the debug harness from a Visual Studio Developer PowerShell. Bootstrap it
directly from PowerShell, then run the fixture command above:

```powershell
$vsRoot = 'C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools'
$vsDevShell = "$vsRoot\Common7\Tools\Launch-VsDevShell.ps1"
if (-not (Test-Path -LiteralPath $vsDevShell)) { throw "Visual Studio Build Tools Developer PowerShell was not found." }
& $vsDevShell -Arch amd64 -HostArch amd64 -SkipAutomaticLocation
$vsCmake = "$vsRoot\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin"
$linker = (Get-ChildItem "$vsRoot\VC\Tools\MSVC\*\bin\Hostx64\x64\link.exe" |
  Sort-Object FullName -Descending | Select-Object -First 1).FullName
if (-not $linker) { throw "The x64 MSVC linker was not found." }
$env:PATH = "$vsCmake;$([IO.Path]::GetDirectoryName($linker));$env:PATH"
$env:CMAKE_GENERATOR = 'Visual Studio 17 2022'
$env:CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER = $linker
```

Capture and inspect original-resolution client-area screenshots at exactly
1180 × 815 and 960 × 680:

1. `models/installed`: idle, hover a selectable inactive-card coordinate, and
   keyboard-focus `Use whisper.cpp tiny.en for future transcriptions`.
2. `models/download-downloading`, `models/download-retained`,
   `models/download-failed-partial`, and `models/download-failed-alert`:
   inspect the isolated lifecycle states. For the failed-alert fixture, capture
   both closed and warning-alert-open states.
3. `models/lifecycle`: inspect inverse Install and the icon-only Play/Pause
   treatments; verify the byte detail is above the full 6 px progress track and
   fill, named 44 x 44 controls share that track row, and contained error
   controls. No visible
   percentage or lifecycle status copy should replace the model description.
4. `models/card-expanded`: inspect expanded top content, then scroll the
   Models route to inspect the complete feature grid, Requirements, optional
   Maintenance order, and shared card edges.

Reject captures that have a black frame, stale process content, partial window,
or a client rectangle other than the requested viewport. Manual screenshots do
not replace deterministic tests for AccessKit action dispatch, child-control
isolation, 375 px containment, or removal/partial safety.
