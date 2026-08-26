# Scribe Design Notes

Visual source of truth: Google Stitch project `projects/13126365166628126458`, `Scribe Design System`.

## Brand identity

- The primary identity is the waveform-S mark paired with the lowercase `scribe` wordmark.
- The product tagline is exactly: **Lightning-fast local transcription that stays out of your way.**
- Use the light lockup on white, Ice Mist, or other light surfaces. Use the dark lockup on Deep Navy or Navy Surface.
- Keep clear space around the mark equal to at least half the mark's width. Do not recolor individual waveform bars outside the approved theme variant, stretch the lockup, add effects, or place it on a visually busy background.
- The canonical vector geometry lives in `assets/branding/scribe-mark.svg`; the native renderer in `src/branding.rs` mirrors its seven symmetric bars, Soft Aqua outer-adjacent bars, and S curve. Public-facing lockups in `docs/assets/branding/` add only the theme surface and wordmark. The documentation site copies those lockups into `website/src/assets/` and `website/public/brand/` so Astro can select theme variants and public links remain base-path safe.
- Compact `scribe-header-light.svg` and `scribe-header-dark.svg` variants omit the surface tile and tagline so the mark and wordmark stay legible in documentation chrome and the README header. The tagline remains adjacent visible text, not baked into these compact assets.
- Run `pwsh -File docs/assets/branding/verify-svg-parity.ps1` after editing any brand SVG. It verifies canonical mark copies, bar geometry and color roles, the S path, and lockup-copy hashes.

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

### Canonical palette

| Role | Light theme | Dark theme |
| --- | --- | --- |
| Primary background | Ice Mist `#EAF5F5` | Deep Navy `#061C2E` |
| Elevated surface | White `#FFFFFF` | Navy Surface `#08233A` |
| Primary text | Deep Ink `#08233A` | Ice Mist `#EAF5F5` |
| Brand accent | Scribe Teal `#2D979C` | Teal Accent `#7CCBC9` |
| Secondary accent | Soft Aqua `#ACDBD9` | Soft Aqua `#ACDBD9` |
| Warning / caution | Warm Sand `#E9D1B1` | Warm Sand `#E9D1B1` |
| Recording / error | Live Coral `#FD816F` | Live Coral `#FD816F` |

### Semantic and accessibility mappings

- Deep Ink is the normal text color on white and Ice Mist. Ice Mist is the normal text color on Deep Navy and Navy Surface.
- `#176D70` is the derived accessible teal text/link token for light surfaces. It preserves the teal identity while meeting WCAG AA; links remain underlined or otherwise identifiable without color alone.
- Scribe Teal, Soft Aqua, Warm Sand, and Live Coral are fills, borders, focus indicators, large marks, and status backgrounds on light surfaces. They are not normal-text colors on white.
- Deep Ink text may be placed on Warm Sand. Deep Navy text may be placed on Scribe Teal, Teal Accent, or Live Coral. Do not place white normal text on Scribe Teal.
- Focus indicators use Scribe Teal on light surfaces and Teal Accent on dark surfaces, with a visible offset so the indicator is not lost against adjacent controls.
- Recording and error states pair Live Coral with an icon, label, or state copy; color is never the only signal.
- Package-status colors outside this brand palette remain functional indicators only and require a separately verified text color.

Representative contrast ratios (sRGB, WCAG 2.x): Deep Ink/white `16.01:1`, Deep Ink/Ice Mist `14.39:1`, accessible teal/white `6.07:1`, accessible teal/Ice Mist `5.46:1`, muted gray `#526F7C`/Ice Mist `4.81:1`, Ice Mist/Deep Navy `15.56:1`, Teal Accent/Deep Navy `9.27:1`, Deep Ink/Warm Sand `10.84:1`, and Deep Navy/Live Coral `7.04:1`. For comparison, Scribe Teal/white is only `3.49:1` and Live Coral/white is only `2.46:1`, so neither is approved for normal text on white.

## Product Rules

- Visible product name is `Scribe`.
- Local-first contract stays explicit: no cloud STT, no account/sync service, no always-on listener or model process, no Python server, and no plugin system. Any future cleanup/reasoning pass must be local, optional, and off by default; it must never use a cloud service.
- Six local backends are runnable in the current build: `whisper.cpp`, `faster-whisper`, Vosk, sherpa-onnx, Moonshine, and Parakeet.
- The sherpa-onnx family is experimental and uses managed, short-lived Python sidecars for batch transcription only. Streaming requires a future `SttBackend` streaming API.
- Errors should use plain language and keep the transcript visible when insertion fails.
- Models use one-click setup: when a model needs a backend runtime, the app prepares the packaged/staged shared runtime asynchronously before downloading the model. Runtime maintenance controls are available in a collapsed disclosure for explicit update and removal; model rows do not repeat runtime metadata or expose unavailable custom-model actions.
- Playground selection is independent from installation. Its centered `Choose models to test` dialog lists installed models with backend and readiness labels; changes are drafted until Apply, and selection cannot change while Playground work is active. Empty selection is valid, but cannot start a test run. Cards retain persisted drag order with 4px inter-card spacing.
- The selector uses a foreground interaction shield, initial focus, explicit Cancel, and opener-focus restoration. egui 0.27 does not provide a native modal/focus-trap primitive, so strict Tab-cycle containment remains a toolkit limitation; Escape and `Cancel model selection` are the reliable keyboard close paths.
