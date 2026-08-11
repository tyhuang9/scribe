# Scribe Design Notes

Visual source of truth: Google Stitch project `projects/13126365166628126458`, `Scribe Design System`.

## Direction

Scribe should feel like a native utility, not a dashboard. The default Transcribe page stays sparse and task-focused: selected model, active hotkey, a centered Start/Stop Listening control, and a large transcript panel.

## Layout

- Canonical shell: 214px labeled sidebar at widths of 1000px and above; 66px icon rail below that breakpoint. The same shell wraps Transcribe, Models, and Settings, with Settings pinned to the bottom.
- The primary navigation is exactly, in order: Transcribe, Models, History, Settings. About and Advanced live as Settings tabs; the developer Playground is reached from Advanced and is never a sidebar destination.
- Light cool-gray app canvas with white bordered panels.
- Compact page headers and dense rows on Models, Playground, and Settings.
- 28px outer content margins, 12-16px panel spacing, and 4-8px radii.
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
- Package status green `#12B76A` is an indicator/fill color. Use the accessible success-text token for text on light surfaces.

## Product Rules

- Visible product name is `Scribe`.
- Local-first contract stays explicit: no cloud STT, no account/sync service, no always-on listener or model process, no Python server, and no plugin system. Any future cleanup/reasoning pass must be local, optional, and off by default; it must never use a cloud service.
- Six local backends are runnable in the current build: `whisper.cpp`, `faster-whisper`, Vosk, sherpa-onnx, Moonshine, and Parakeet.
- The sherpa-onnx family is experimental and uses managed, short-lived Python sidecars for batch transcription only. Streaming requires a future `SttBackend` streaming API.
- Errors should use plain language and keep the transcript visible when insertion fails.
- Models use one-click setup: when a model needs a backend runtime, the app prepares the packaged/staged shared runtime asynchronously before downloading the model. Runtime maintenance controls are available in a collapsed disclosure for explicit update and removal; model rows do not repeat runtime metadata or expose unavailable custom-model actions.
- Playground selection is independent from installation. Its centered `Choose models to test` dialog lists installed models with backend and readiness labels; changes are drafted until Apply, and selection cannot change while Playground work is active. Empty selection is valid, but cannot start a test run. Cards retain persisted drag order with 4px inter-card spacing.
- The selector uses a foreground interaction shield, initial focus, explicit Cancel, and opener-focus restoration. egui 0.27 does not provide a native modal/focus-trap primitive, so strict Tab-cycle containment remains a toolkit limitation; Escape and `Cancel model selection` are the reliable keyboard close paths.
