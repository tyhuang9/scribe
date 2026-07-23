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
- Local-first contract stays explicit: no cloud STT, no account/sync service, no always-on listener or model process, no Python server, and no plugin system. Any future cleanup/reasoning pass must be local, optional, and off by default; it must never use a cloud service.
- Six local backends are runnable in the current build: `whisper.cpp`, `faster-whisper`, Vosk, sherpa-onnx, Moonshine, and Parakeet.
- The sherpa-onnx family is experimental and uses managed, short-lived Python sidecars for batch transcription only. Streaming requires a future `SttBackend` streaming API.
- Errors should use plain language and keep the transcript visible when insertion fails.
- Models use one-click setup: when a model needs a backend runtime, the app prepares the packaged/staged shared runtime asynchronously before downloading the model. Runtime maintenance controls are available in a collapsed disclosure for explicit update and removal; model rows do not repeat runtime metadata or expose unavailable custom-model actions.
