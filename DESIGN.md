# Scribe Design Notes

Visual source of truth: Google Stitch project `projects/13126365166628126458`, `Scribe Design System`.

## Direction

Scribe should feel like a native utility, not a dashboard. The default Transcribe page stays sparse and task-focused: selected model, active hotkey, a centered Start/Stop Listening control, and a large transcript panel.

## Layout

- 200px left sidebar with the Scribe name, primary navigation, and quiet local-first cues.
- Light cool-gray app canvas with white bordered panels.
- Compact page headers and dense rows on Models, Playground, and Settings.
- 24px outer content margins, 12-16px panel spacing, and 4-8px radii.
- No decorative shadows, gradients, marketing hero sections, or nested card layouts.

## Color

- Canvas: `#F7F9FB`
- Surface: `#FFFFFF`
- Primary text and CTA: `#1D212A` / `#060A12`
- Secondary text: `#555F6D`
- Borders: `#E2E8F0`, stronger `#CBD5E1`
- Accent: `#2563EB`
- Success: `#16A34A`
- Warning: `#CA8A04`
- Error: `#DC2626`

## Product Rules

- Visible product name is `Scribe`.
- Local-first contract stays explicit: no cloud STT, no account/sync, no always-on listener, no Python server, no reasoning cleanup pipeline, and no plugin system.
- `whisper.cpp` is the only runnable backend in this phase.
- Other catalog backends remain planned/experimental until adapters are implemented.
- Errors should use plain language and keep the transcript visible when insertion fails.
