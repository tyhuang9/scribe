# Scribe Design System

Design source of truth: Google Stitch project `projects/13126365166628126458`, **Scribe Design System**. The selected reference is screen `projects/13126365166628126458/screens/365f1f7e48474c8289a76ccd176d841c`, **Transcribe - Glassmorphic Variant**. The local reference package is in [`.stitch/`](.stitch/).

## 1. Visual theme and atmosphere

Scribe is a compact, local-first desktop utility, not a dashboard or marketing experience. The direction is **minimal glass with selective neumorphism**: a cool mineral canvas, a lightly tinted/translucent app shell, a compact icon rail, dense but breathable working content, and shallow inset depth only where it confirms a physical interaction surface. Density is 6/10, variance 2/10, and motion 2/10.

Glass is structural, never decorative. Neumorphism is limited to the active navigation state and centered recording control. Avoid glossy card stacks, oversized shadows, gradients, and anything that makes local transcription feel like cloud software.

## 2. Color palette and roles

- **Fog Canvas** `#EEF2F6` - window background and recessed areas.
- **Frosted App Surface** `rgba(255,255,255,0.72)` - main glass shell; fall back to solid `#F8FAFC` when system blur is unavailable.
- **Quiet Surface** `#FFFFFF` - transcript, dialogs, and elevated content only.
- **Ink** `#18212B` - headings, primary label, and primary text. Never use pure black.
- **Muted Ink** `#5E6B78` - descriptive copy, metadata, and disabled labels.
- **Hairline** `rgba(121,139,157,0.26)` - structural 1 px borders.
- **Recessed Edge** `rgba(255,255,255,0.84)` with `rgba(92,110,128,0.14)` - paired inset highlight/shadow.
- **Scribe Blue** `#3269C7` - the only action accent: primary action, focus, selected navigation, and progress.
- **Ready Green** `#2F7D58` - runnable/complete state only.
- **Caution Ochre** `#9A6A18` - actionable setup attention only.
- **Error Red** `#B44142` - error and destructive confirmation only.

Text must meet WCAG 2.1 AA contrast on its actual surface. Status always combines text and an icon/shape, not color alone.

## 3. Typography

- **Interface / display:** Segoe UI Variable or SF Pro Display where supplied by the OS. Prefer platform UI typography before bundling a web font.
- **Body:** Segoe UI Variable or SF Pro Text, 14 px / 20 px default. Body text is never below 13 px on desktop.
- **Monospace:** Cascadia Mono / SF Mono for paths, shortcuts, timings, and technical documentation.
- **Scale:** 12 metadata, 14 body/control, 16 section title, 22 page title, 28 only for empty-state title. Weight, not oversized type, provides hierarchy.
- **Native font contract:** Do not bundle or require Inter in native egui; use the platform UI font. Official Stitch project assets and exact remote exports may use the typography configured in their Stitch design system and remain valid visual evidence.
- **Banned:** generic serif faces, centered hero typography, all-caps UI labels, and invented metrics.

## 4. Layout and responsive/window rules

- Native verification contract: 1100x760 is the primary window size and 840x600 is the minimum window size, matching `src/main.rs`. Verify every product screen at both sizes.
- **Native authority:** This document is authoritative for native egui geometry, behavior, accessibility, and verification. Official Stitch PNG exports are directional visual evidence. Where a refined web export uses an 80 px rail or a 72 px pill-style listening control, the native implementation deliberately follows the selected compact contract here: a 60 px rail and a 56 px tactile recording control.
- The selected direction uses a 60 px icon rail with tooltips; the product name is represented by a compact Scribe mark. Do not restore a wide text sidebar without product approval.
- Main content uses a 24 px outer gutter, 16 px section rhythm, and a maximum working width of 1120 px. Extra width is breathing room, not stretched controls.
- Transcribe uses a compact model/hotkey header strip, one centered 56 px tactile record control, then the dominant transcript well. At compact width, metadata wraps above the record control.
- Models, Playground, and Settings remain single-column task pages. Do not create dashboard-style equal card grids.
- Keep controls wrapped rather than clipped. No horizontal scrolling except within a path/code field with a visible copy affordance.
- Modal dialogs are centered and constrained to 560 px. A dimming shield blocks background activation.

## 5. Component styling

- **Glass shell:** 1 px Hairline border, 16 px radius, 12-16 px blur only when OS compositing supports it. Fallback is Quiet Surface.
- **Panels:** 10 px radius, quiet/translucent fill, 1 px Hairline. Use only a very low diffuse dialog shadow.
- **Listening control:** centered 56 px circular or softly rounded tactile control plane; shallow two-sided inset. It is the one prominent physical affordance and must not glow.
- **Transcript well:** largest page region; subtly recessed with a visible editing boundary. Copy/Clear stay subordinate at the lower edge. A completed voice-edit result may add compact Undo, Redo, and View original actions on the same wrapped action row.
- **Primary button:** Scribe Blue fill, white label, 40 px min height except the 56 px record control. Active state translates 1 px down; disabled state explains why.
- **Secondary/row actions:** transparent or Quiet Surface, 1 px Hairline, 32-36 px height. Destructive actions other than the current transcript Clear behavior remain visually separated.
- **Navigation:** icon target is 40 x 40 px, with an accessible name and tooltip. Selected state uses a quiet blue-tinted inset, not a floating pill.
- **Inputs:** stable label above/left, no floating labels; 36 px minimum height; 2 px Scribe Blue focus ring with offset.
- **Badges:** backend and readiness only. Do not turn the interface into a chip inventory.
- **Loading:** reserve final geometry. Operational progress animation is allowed when paired with visible state text and must not move the layout; decorative perpetual motion is banned.
- **Empty/error:** preserve transcript and selected models. Explain the next local action in one sentence then present one clear action.

## 6. Interaction, states, and motion

- Listening has explicit ready, recording, stopping, transcribing, editing, completed, and error states. Record/stop is never ambiguous.
- Start is disabled until a runnable local model/runtime exists. Its explanation directs people to Models and never suggests cloud sign-in.
- Copy and Clear act only on the transcript. Clear immediately empties the transcript in the current product; this visual branch does not change that behavior or add a confirmation. Auto-insert errors keep the transcript visible.
- Live preview is visually provisional and read-only. It never exposes edit actions and is replaced by the authoritative final result.
- Voice editing is an opt-in Recording subsection with a binary toggle, exclusive Compact/Balanced radios, readiness, verified download progress, and Install/Remove commands. It must fit the existing Settings card and wrap at the 840 px minimum viewport.
- Successful edits show a quiet applied-count label. Ambiguous or failed edits preserve the original in a single review panel with Retry, Use original, and Copy; review never triggers automatic insertion.
- Undo, Redo, and View original change only Scribe's displayed transcript. They never imply that text already pasted into another application was recalled.
- Model setup is progressive: Install can prepare a local shared runtime then download the model. Runtime maintenance remains collapsed.
- Playground selection is drafted until Apply. It uses a pointer shield, initial focus, Cancel/Escape, and opener-focus restoration. Empty selection is valid but cannot run.
- Settings save immediately with quiet inline "Saved" feedback. Toggle/radio state is communicated by text, icon, and position.
- Decorative transitions are 120-180 ms, opacity/transform only, ease-out. Operational recording/transcription progress may animate when paired with text. Decorative perpetual motion is banned. The native-motion experiment honors `SCRIBE_REDUCED_MOTION=1` (also `true`, `yes`, or `on`) at startup to disable record-control interpolation without persisted configuration; a discoverable in-product setting remains a later follow-up.

## 7. Accessibility and native egui constraints

- Keyboard order follows visual order. Every icon-only control needs an accessible name and tooltip.
- Minimum desktop target is 32 px; navigation and primary record targets are 40 px and 56 px respectively. Preserve 8 px between adjacent row actions.
- Focus is always visible with a 2 px Scribe Blue outline; hover is never the only affordance.
- eframe/egui 0.27 has no CSS `backdrop-filter`. Approximate glass with translucent fills and optional platform viewport transparency, plus a solid fallback.
- Use `available_width`, `horizontal_wrapped`, vertical fallback sections, `ScrollArea`, and clipping-safe labels. Avoid fixed absolute positioning.
- System scaling differs by OS. Treat spacing as logical points and visually verify 100% and 125% scaling.
- egui 0.27 does not provide a strict native modal focus trap. Escape and explicit Cancel are reliable close paths.

## 8. Product content constraints

- Visible product name is `Scribe`; product tabs are **Transcribe**, **Models**, **Model Playground**, and **Settings**.
- The local-first contract stays explicit: no cloud STT, accounts, sync, always-on listener, Python server, or plugin system. Any future cleanup/reasoning stays local, optional, off by default, and never cloud-backed.
- Runnables in the current build: `whisper.cpp`, `faster-whisper`, Vosk, sherpa-onnx, Moonshine, and Parakeet. The sherpa-onnx family is experimental and batch-only through short-lived managed Python sidecars; streaming needs a future `SttBackend` API.
- Do not invent model accuracy, speed, RAM, uptime, usage, or performance numbers. Show run measurements only after a run; otherwise use "Ready", "Not installed", and "No run yet".
- Errors are plain language and remain near the control that needs action. Preserve transcript text if insertion fails.

## 9. Anti-patterns (banned)

- No emoji UI icons; use one native/vector icon language consistently.
- No neon, outer glow, purple/blue cyber aesthetic, excessive blur, glass card stacks, or large gradients.
- No marketing hero, centered slogan, fake charts/statistics, or system-performance metric cards.
- No three-column equal card layout, decorative illustrations, avatars, or generic names.
- No pure black, bundled/required Inter in native egui, generic serif, custom cursor, or text overlap. Exact official Stitch exports may retain their configured Stitch typography.
- No fabricated cloud/privacy claims; all local-only wording must match shipped behavior.
