# UI harness

The deterministic UI harness is development-only. It uses the shared shell and egui primitives, never starts audio capture, model downloads, or settings writes, and is unavailable from release builds.

Run a fixture in a debug build:

```powershell
$env:SCRIBE_UI_HARNESS = 'transcribe/no-model'
cargo run --features ui-harness
```

Valid state names are:

- `transcribe/no-model`
- `transcribe/ready`
- `transcribe/listening`
- `transcribe/finalizing`
- `transcribe/no-speech`
- `transcribe/microphone-error`
- `models/installed` — collapsed installed and available cards. Summary rows show up to four priority feature glyphs; each glyph has a hover tooltip while the combined Features group remains non-interactive for keyboard navigation.
- `models/lifecycle` — active, installed, uninstalled, partial, downloading, and failed row states
- `models/card-expanded` — inline local-model details expanded; inspect the single card surface, 44 px controls, and 375 px containment
- `models/compare-expanded`
- `history`
- `settings/recording`

Any other value is ignored, so normal application startup remains fail-closed. The harness freezes timers, relative time, and sample metadata for repeatable screenshots. The visual references are stored in `docs/ui-reference/` and are documentation-only; they are not runtime assets.

`models/card-expanded` exposes the full known feature list plus size and requirements details. Its 44 px controls include the explicit Delete action.

For native manual checks, use `models/installed` and `models/card-expanded` at 1180 × 815 and 960 × 680. Verify installed and available cards, summary-glyph hover tooltips, keyboard focus on the title, lifecycle control, and chevron, then expand a card and verify the detailed feature list and Delete flow. The 375 px card layout is automated-only: deterministic UI tests cover containment, stacked controls, and long-name behavior rather than a native reference capture.
