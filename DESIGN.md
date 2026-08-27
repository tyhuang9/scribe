# Scribe Design Notes

Visual source of truth: Google Stitch project `projects/13126365166628126458`, `Scribe Design System`.

## Brand identity

- The primary identity is the waveform-S mark paired with the lowercase `scribe` wordmark.
- The product tagline is exactly: **Lightning-fast local transcription that stays out of your way.**
- Use the light lockup on white, Ice Mist, or other light surfaces. Use the approved dark lockup on its intended identity surface; the application dark theme uses Charcoal and Surface for UI surfaces.
- Keep clear space around the mark equal to at least half the mark's width. Do not recolor individual waveform bars outside the approved theme variant, stretch the lockup, add effects, or place it on a visually busy background.
- The canonical vector geometry lives in `assets/branding/scribe-mark.svg`; the native renderer in `src/branding.rs` mirrors its seven symmetric bars, Soft Aqua outer-adjacent bars, and S curve. Full light/dark marketing lockups live beside the mark in `assets/branding/` and include the tagline. Public-facing compact lockups in `docs/assets/branding/` add only the theme surface and wordmark; the documentation site copies them into `website/src/assets/` and `website/public/brand/` so Astro can select theme variants and public links remain base-path safe.
- Compact `scribe-header-light.svg` and `scribe-header-dark.svg` variants omit the surface tile and tagline so the mark and wordmark stay legible in documentation chrome and the README header. The tagline remains adjacent visible text, not baked into these compact assets.
- Approved identity SVG and icon asset bytes remain intentionally stable across application theme changes. The application theme may change surrounding UI surfaces, but it does not recolor, regenerate, or otherwise mutate those approved assets.
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
| Primary background | Ice Mist `#EAF5F5` | Charcoal `#121418` |
| Elevated surface | White `#FFFFFF` | Surface `#1A1D22` |
| Primary text | Deep Ink `#08233A` | Soft Text `#E9F0F0` |
| Brand accent | Scribe Teal `#2D979C` | Scribe Teal `#2D979C` |
| Secondary / supporting text | Soft Aqua `#ACDBD9` | Muted Gray `#8E99A3` |
| Warning / caution | Warm Sand `#E9D1B1` | Accessible Warm Sand `#F2C27B` (derived) |
| Recording / error | Live Coral `#FD816F` | Live Coral `#FD816F` |

The exact application tokens are:

- Light: Deep Ink `#08233A`, Scribe Teal `#2D979C`, Soft Aqua `#ACDBD9`, Ice Mist `#EAF5F5`, Warm Sand `#E9D1B1`, and Live Coral `#FD816F`.
- Dark: Charcoal `#121418`, Surface `#1A1D22`, Soft Text `#E9F0F0`, Muted Gray `#8E99A3`, Scribe Teal `#2D979C`, and Live Coral `#FD816F`.

The dark warning, link, and status-copy values are accessible derived states,
not replacements for the six exact dark application tokens. Approved identity
SVG and icon asset bytes remain intentionally stable and are not recolored by
the application theme.

`#23262C` is the approved derived dark elevation surface. Use it only when a
raised or active panel must remain visibly distinct from Surface `#1A1D22`,
such as the active model-card state; it is not a seventh canonical palette
token.

### Semantic and accessibility mappings

- Deep Ink is the normal text color on white and Ice Mist. Soft Text is the normal text color on Charcoal and Surface, while Muted Gray is reserved for supporting copy and metadata.
- `#176D70` is the derived accessible teal text/link token for light surfaces. It preserves the teal identity while meeting WCAG AA; links remain underlined or otherwise identifiable without color alone.
- Scribe Teal, Soft Aqua, Warm Sand, and Live Coral are fills, borders, focus indicators, large marks, and status backgrounds on light surfaces. They are not normal-text colors on white. In dark mode, raw Scribe Teal remains the exact accent/focus token; derived brighter teal is used for small accent text where needed.
- Deep Ink text may be placed on Warm Sand. Charcoal text may be placed on Scribe Teal, derived teal, or Live Coral. Do not place white normal text on Scribe Teal.
- Focus indicators use Scribe Teal on both light and dark surfaces, with a visible offset so the indicator is not lost against adjacent controls.
- Recording and error states pair Live Coral with an icon, label, or state copy; color is never the only signal.
- Package-status colors outside this brand palette remain functional indicators only and require a separately verified text color.

Representative contrast ratios (sRGB, WCAG 2.x): Deep Ink/white `16.01:1`, Deep Ink/Ice Mist `14.39:1`, accessible teal/white `6.07:1`, accessible teal/Ice Mist `5.46:1`, Muted Gray/Charcoal `6.35:1`, Muted Gray/Surface `5.82:1`, Soft Text/Charcoal `15.97:1`, Soft Text/Surface `14.63:1`, Scribe Teal/Charcoal `5.28:1`, Scribe Teal/Surface `4.84:1`, derived teal `#77D1D3`/Surface `9.53:1`, and Charcoal/Live Coral `7.50:1`. For comparison, Scribe Teal/white is only `3.49:1` and Live Coral/white is only `2.46:1`, so neither is approved for normal text on white.

## Product Rules

- Visible product name is `Scribe`.
- Local-first contract stays explicit: no cloud STT, no account/sync service, no always-on listener or model process, no Python server, and no plugin system. Any future cleanup/reasoning pass must be local, optional, and off by default; it must never use a cloud service.
- Six local backends are runnable in the current build: `whisper.cpp`, `faster-whisper`, Vosk, sherpa-onnx, Moonshine, and Parakeet.
- The sherpa-onnx family is experimental and uses managed, short-lived Python sidecars for batch transcription only. Streaming requires a future `SttBackend` streaming API.
- Errors should use plain language and keep the transcript visible when insertion fails.
- Models use one-click setup: when a model needs a backend runtime, the app prepares the packaged/staged shared runtime asynchronously before downloading the model. Runtime maintenance controls are available in a collapsed disclosure for explicit update and removal; model rows do not repeat runtime metadata or expose unavailable custom-model actions.
- Playground selection is independent from installation. Its centered `Choose models to test` dialog lists installed models with backend and readiness labels; changes are drafted until Apply, and selection cannot change while Playground work is active. Empty selection is valid, but cannot start a test run. Cards retain persisted drag order with 4px inter-card spacing.
- The selector uses a foreground interaction shield, initial focus, explicit Cancel, and opener-focus restoration. egui 0.27 does not provide a native modal/focus-trap primitive, so strict Tab-cycle containment remains a toolkit limitation; Escape and `Cancel model selection` are the reliable keyboard close paths.
