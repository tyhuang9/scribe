# Stitch prompts - Scribe

Use the existing project design system asset. These prompts describe layout, information hierarchy, real content, and state behavior only; do not repeat colors, fonts, or token values.

## Explorations - Transcribe

### Soft Neumorphic Utility

Minimal desktop transcription utility for a dependable local speech-to-text workflow. Desktop-first. Use a compact icon rail with accessible labels/tooltips; header with Transcribe and a local-only status; a compact Current Model/Hotkey context strip; a left-aligned tactile listening plane; then a dominant transcript editor with Copy and Clear. Provide an in-place setup-required state with local model actions only. Do not add metrics, cloud features, a marketing hero, or decorative illustration.

### Transcribe - Glassmorphic Variant (selected)

Refine the existing selected Transcribe exploration into the final desktop utility direction. Preserve the compact 60 px icon rail, lightly translucent/tinted shell over a cool canvas, compact Current Model and Hotkey context, centered 56 px tactile Start Listening / Stop Listening control, and large subtly recessed Transcript well. Keep Copy/Clear subordinate. Include explicit ready, recording, transcribing, setup-required, and transcript-insertion-error states while preserving transcript text. Scribe remains local-only: no cloud speech service, account sync, or invented metrics.

### Balanced Hybrid

Desktop local speech-to-text utility optimized for reviewing/editing transcript. Use the compact icon rail, compact header context, transcript as the largest region, and a compact lower listening tray. Include empty, recording, transcribing, completion, and insertion-error annotations without invented data. Keep all content non-overlapping and local-only.

## Final refinement - Models

Desktop Models page aligned to the selected compact icon-rail direction. Keep compact search/backend filtering and optional download activity. Use a collapsed Runtime maintenance disclosure explaining shared local backend runtimes. Show dense model rows with model name, backend, actual local install/readiness state, selected-default treatment, and one contextual primary action: Install, Installing, Retry, Repair, Select, or Active. Include empty/no-match and failure states. Do not invent model benchmarks, model sizes, accuracy, or cloud features. Use restrained inset depth only for selected/actionable controls.

## Final refinement - Model Playground

Desktop Model Playground aligned to the selected compact icon rail and frosted shell. Explain that measurements are calculated locally and appear only after a run. Include Selected Models with Choose Models. Its dialog lists installed models with backend/readiness, drafts changes until Apply, supports Cancel/Escape, and blocks background interaction. An empty selection is valid but disables test run. Selected model results are draggable vertical cards with preserved order and space for actual result/duration/error state only. Never show fabricated benchmark values.

## Final refinement - Settings

Desktop Settings page aligned to the selected compact icon-rail direction. Use a dense single column with General, Shortcuts, Performance, Audio, Appearance, and Runtime. Include only real controls: close to tray, auto insert into focused app, restore clipboard, paste delay, hotkey capture/mode, device/GPU where supported, microphone refresh, max recording duration, theme, and explicit local-only runtime statement. Settings save immediately with quiet inline feedback. Include plain-language unavailable states.

## Final refinement - Technical documentation

Desktop technical documentation screen for the Scribe design handoff. Use a compact table of contents and readable document column covering design goals, selected Transcribe layout, token roles, component state contracts, local-first content rules, accessibility/focus, motion limits, and egui 0.27 constraints. Use code-styled blocks only for dimensions and tokens; do not fabricate API/runtime data.
