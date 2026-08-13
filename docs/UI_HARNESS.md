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
- `models/installed`
- `models/lifecycle` — active, installed, uninstalled, partial, downloading, and failed row states
- `models/card-expanded` â€” inline local-model details expanded; inspect the single card surface, 44 px controls, and 375 px containment
- `models/compare-expanded`
- `history`
- `settings/recording`

Any other value is ignored, so normal application startup remains fail-closed. The harness freezes timers, relative time, and sample metadata for repeatable screenshots. The visual references are stored in `docs/ui-reference/` and are documentation-only; they are not runtime assets.
