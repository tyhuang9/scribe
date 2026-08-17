# Settings navigation redesign — Branch 3 evidence

Final result: passed.

## Scope verified

- The primary navigation exposes exactly Transcribe, Models, History, and Settings.
- Settings exposes exactly General, Recording, Advanced, and About.
- Legacy Output routes normalize to General. About and the developer Playground remain inside Settings, and Playground provides a Back to Advanced action.
- Dense Settings sections and rows use responsive desktop/compact layouts, at least 44 px interaction targets, and separators only between rendered rows.
- The passive microphone monitor exists only while the window is visible on Settings > Recording with no active capture owner; it tears down on route exit, hide, quit, or capture ownership and requests an approximately 20 Hz repaint only while the monitor session exists.

## Native visual evidence

Evidence directory:

`C:\Users\huang\AppData\Local\Temp\scribe-ui-acceptance\settings-nav-redesign-20260811`

- `settings-recording-1476x1018.png` — clean per-monitor-DPI-aware client crop from a fresh process; no taskbar or non-client chrome is included.
- `settings-recording-1180x815.png` — standard-width Recording view.
- `settings-recording-960x680.png` — compact view with the responsive icon rail and contained scrolling.
- `C:\tmp\scribe-settings-native-captures-1786496775646\general.png` — fresh post-regrouping General sections with app/model, appearance/overlay, and output ownership.
- `C:\tmp\scribe-settings-native-1786496721200\advanced-survival.png` — fresh Recording-to-Advanced native switch; PID remained alive at 120 DPI and stderr contained no AccessKit panic.
- `C:\tmp\scribe-settings-native-tabs-1786496882489\advanced.png` — selected Advanced tab with Voice detection visible; the same tab owns history/privacy, developer/diagnostics, and Playground lower in its scroll area.
- `C:\tmp\scribe-settings-native-tabs-1786496882489\about.png` — selected About tab with flattened Application, Scribe/version, local-first privacy, and local paths content.
- `settings-recording-1280x1024.png` — implementation capture at the exact Product Design reference size.
- `comparison-settings-recording.png` — combined same-state reference and implementation comparison; the Handy image is treated as directional, while Scribe's approved navigation and information architecture remain authoritative.

Direct inspection found no clipping, taskbar contamination, overlapping controls, blank conditional-row gaps, or separators without adjacent rows. The four primary navigation items and four Settings tabs remain visible and ordered correctly. The 960x680 view transitions to the compact rail and preserves scrolling. The Playground open/back interaction is additionally exercised through rendered pointer and focus tests.

## Automated verification

Frozen implementation HEAD: `7cce3fc Stabilize settings AccessKit tab updates`.

- `cargo fmt --all --check`: passed.
- `git diff --check`: passed.
- `cargo check --all-targets --all-features`: passed with zero warnings.
- `cargo clippy --all-targets --all-features -- -D warnings`: passed.
- `cargo test --all-targets --all-features -- --test-threads=1`: 753 passed, 0 failed, 9 ignored, 0 measured, 0 filtered; 762 total.
- Architecture guards: 7 passed within the full test run.
- The 9 ignored tests are pre-existing local GGUF/audio, live catalog, whisper.cpp/JFK, faster-whisper, and CUDA/GPU runtime smoke tests; none was enabled for this branch.

Focused regression coverage includes the exact navigation/tab inventory and ownership, legacy Output and Debug redirects, real Settings interaction bounds at desktop and compact widths, conditional-row separator transitions, Playground open/back/route-exit/focus behavior, passive-monitor predicate/idempotence/teardown/repaint timing, About hierarchy, and focused-tab keyboard navigation. Incremental AccessKit regressions additionally prove that Recording-to-Advanced updates contain no updated-and-orphaned nodes, the active content is parented by its `TabPanel`, and all four tabs stay inside deterministic disjoint automatic-ID ranges with more than 90% reserved headroom.

## Specialist verdicts

- Code review: approved after all requested changes were resolved.
- Security and privacy: passed with no Critical, High, or Medium findings; diagnostics remain redacted and local, and the passive monitor retains no PCM data.
- Accessibility: passed at `7cce3fc` with no Critical, Major, or Minor findings; actual Settings controls meet the 44 px target floor, disabled diagnostics export is programmatically described, Playground focus enters and returns correctly, and native tab updates preserve valid AccessKit ancestry.
- Performance: passed after Settings model projection was reduced to one selected-model resolution per frame; passive repainting remains scoped to the live meter session.
- UI/UX: passed after Playground route-exit cleanup and flattened About hierarchy.
- QA: passed on the frozen all-target/all-feature gate above. A fresh native Recording-to-Advanced switch at 120 DPI left the process alive with empty stderr, and fresh native General, Recording, Advanced, and About captures showed the intended selected tab and content.

## Remaining limitation and deliberate non-goals

The deterministic harness and state tests do not open a real microphone, so operating-system-level device release during native monitor teardown was not directly observed. Model downloads, live catalog access, transcription runtime smoke, audio capture, and GPU/CUDA smoke were deliberately excluded by branch scope; the corresponding ignored tests were not run.

---

# Revised STT model manager v2 — implementation evidence

Status: **automated design evidence passed; native screenshot review pending**.

## Current refinement — `e01bdd4`

Final result: **blocked** for the revised-model-manager native visual gate.

The current implementation replaces the row flow layout with one physical
header/row cell splitter, clips long Model identity content to its own cell,
uses compact 32 px visual buttons with 44 px interaction targets, and gives
Recording Mode a contrast-checked purple pill. The Details drawer now has a
stable two-column stat grid, keyboard containment, Escape/outside dismissal,
and tested open/close behavior.

Automated evidence on this commit: `cargo fmt --all -- --check`, `cargo check
--all-targets --all-features`, `cargo clippy --all-targets --all-features --
-D warnings`, `cargo test --all-targets --all-features -- --test-threads=1`
(776 passed, 11 ignored), debug/release builds, and `git diff --check` all
passed. Focused coverage proves header/row grid tracks, clipped long identity
content, drawer Tab containment, Escape dismissal, and light/dark segmented
control contrast.

Native capture was attempted with the deterministic `models/details-drawer`
fixture at 1180×815 in an isolated local-data directory. The Scribe process
created a responsive native window and remained alive while the drawer was
open, but the available desktop screenshot path captured the host browser
rather than the GPU-composited Scribe window. `PrintWindow` returned only a
black frame, and the bundled Windows Computer Use runtime is unavailable in
this session. Direct visual comparison against the supplied model-list and
drawer references is therefore still required before this visual gate can be
called passed.

## Implemented reference requirements

- The Models route uses compact 76 px desktop rows (124 px at very narrow
  widths), a single Active badge beside only the selected model name, separate
  Installed and Available sections, five-segment Speed and Accuracy meters,
  and exactly one lifecycle-dependent row action.
- The toolbar is a full-width search field with compact Refresh, Import local
  GGUF, and language-filter controls. Download state uses the installer’s
  actual byte counts for both the circular cancel control and a 2 px row line.
- Local and trusted-remote Details use an accessible right drawer with a
  fixed header/footer, responsive full-width presentation, keyboard trap,
  Escape/outside close when safe, and focus restoration. Only a ready inactive
  local model exposes **Use this model** in the drawer footer.
- Settings use leading labels/trailing values, responsive stacking, compact
  shared button/keycap visuals with 44 px interaction targets, vertically
  centered hotkeys, a 30 px History title, and a 16 px route bottom inset.

## Automated evidence at `9145447`

- `cargo test --all-targets --all-features ui::screens::tests -- --test-threads=1`: 43 passed.
- `cargo test --all-targets --all-features app::layout_tests:: -- --test-threads=1`: 191 passed.
- Focused harness regression for 375 px model rows: passed.
- Focused active-removal replacement/blocking regressions: passed.
- `cargo fmt --all -- --check`, `cargo check --all-targets --all-features`,
  `cargo clippy --all-targets --all-features -- -D warnings`, and `git diff
  --check`: passed.

## Native visual gate limitation

The required light/dark native captures at 1476×1018, 1180×815, and 960×680
could not be captured in this session because no native desktop-control or
screenshot interface was available. The running debug executable also locks
the normal debug output path. As a result, direct pixel comparison against
`02-final-list-states.png` and `03-details-drawer.png` remains pending; this
is an evidence gap, not a claim that the visual gate passed. The deterministic
harness provides `models/lifecycle`, `models/details-drawer`, `history`, and
`settings/recording` fixtures for the follow-up capture.

---

# Model card redesign: design QA

## Scope

- Branch: `agent/model-card-accordions`
- Reviewed head: `51824ac` (`Left align model description previews`)
- Route/state: `models/card-expanded`
- Supported native viewports: 1180 x 815 and the application minimum of 960 x 680, at Windows 100% display scaling.
- A 375 px full-shell viewport is intentionally excluded because Scribe enforces a 960 x 680 native minimum. Card-only behavior at 375 px is covered by deterministic component tests.

## Source references

- `C:\Users\huang\.codex\attachments\1364743b-4158-4cb2-b03d-82a39e511f2d\image-1.png` — 769 x 119 px; collapsed installed-card structure.
- `C:\Users\huang\.codex\attachments\1364743b-4158-4cb2-b03d-82a39e511f2d\image-2.png` — 748 x 92 px; Install/Uninstall action treatment.
- `C:\Users\huang\.codex\attachments\1364743b-4158-4cb2-b03d-82a39e511f2d\image-3.png` — 756 x 179 px; expanded metadata and description treatment.

The source files are component crops, not full-screen mocks. They were compared against the corresponding model-card regions rather than scaled to the full native window.

## Implementation evidence

- `design-qa-evidence/implementation-1180x815-expanded-v6.png` — 1194 x 853 px outer window; verified client area 1180 x 815.
- `design-qa-evidence/implementation-960x680-expanded-v6.png` — 974 x 718 px outer window; verified client area 960 x 680.
- `design-qa-evidence/implementation-1180x815-expanded-v6-scrolled.png` — complete expanded-card body at the standard viewport.
- `design-qa-evidence/implementation-960x680-expanded-v6-scrolled.png` — complete expanded-card body at the supported minimum viewport.
- `design-qa-evidence/comparison-source-vs-1180-v6.png` — combined reference/implementation comparison.
- `design-qa-evidence/comparison-source-vs-960-v6.png` — combined minimum-width expanded-details comparison.

The application was rebuilt from `51824ac` with `--features ui-harness` before the v6 captures. Windows `PrintWindow` succeeded for all top and scrolled captures. Native geometry was measured directly: 1180 x 815 and 960 x 680 client rectangles.

## States reviewed

- Installed inactive card with Uninstall action.
- Installed active card with the sole Active badge.
- Expanded installed card with GPU acceleration, RAM usage, file size, full description, full languages, and runtime metadata.
- Collapsed name/description/language stack with language glyph and abbreviated language.
- Bar-only speed and accuracy indicators.
- Trailing collapsed and expanded disclosure controls.
- Supported wide and minimum-width native layouts.

## Findings and resolution history

1. Resolved: collapsed metadata leaked model size and allowed the globe to wrap away from the language code. The collapsed row now contains one atomic globe-and-language group and no size.
2. Resolved: expanded statistics ran together. GPU acceleration, RAM usage, and file size now use distinct spaced columns with label/value hierarchy.
3. Resolved: expanded content repeated Uninstall. The single lifecycle control remains in the card summary.
4. Resolved: the installing label contained mojibake. It now renders as `Installing...`.
5. Resolved: the expanded surface previously used redundant framing/separators. Summary and details now share one neutral outer card and one divider, with no colored left rail.
6. Passed: all lifecycle and disclosure controls are visible and contained at both supported native widths. At 960 px the right outline is visually tight against the client edge, but neither Uninstall nor the trailing chevron is clipped. Automated component bounds tests independently cover the boundary.
7. Intentional deviation from the references: the meters are unlabeled segmented bars because the user explicitly requested speed and accuracy bars with no visible text. Accessible names and tooltips retain their meaning.
8. Resolved after final code review: the collapsed description and language now stay within the fixed identity track. The v6 glyphs visibly share the model-name text axis, while meters, lifecycle action, and disclosure remain in their own tracks.
9. Resolved after final code review: recognized full language names and arbitrary valid two- or three-letter language codes remain truthful in the collapsed summary; more than three unique languages render as `Multilingual`.

## Remaining enhancements

- P3: if future shell geometry allows it, a little more visual breathing room at the 960 px right edge would be pleasant. This is not a usability or containment defect.
- Native screen-reader and non-100%-DPI application testing were not performed in this visual pass. Deterministic AccessKit role, name, expanded-state, focus, target-size, and containment tests cover the implemented semantics.

## Verdict

No P0, P1, or P2 visual issue remains in the reviewed supported-width evidence. The model cards match the requested information hierarchy and lifecycle behavior while preserving Scribe's existing design system.

final result: passed

---

# Foreground-aware recording overlay QA - 2026-08-17

## Source and implementation scope

- Visual source of truth: `C:\Users\huang\.codex\attachments\1262dc67-8414-4589-960b-16e952d23970\image-1.png` (1142 x 137 source pixels).
- Reviewed implementation source head: `69372d1` (`Remove obsolete native overlay seam helpers`), including native renderer commits `b208f1b`, `6507543`, `62b6d50`, and `bbc3994`.
- Live target: a 600 x 62 logical-point bottom/top-center capsule with waveform, elapsed time, one divider, a one-line Unicode-grapheme-safe transcript/status tail, and a separately hardened 44 x 44 cancel viewport.
- Compact target: a 320 x 52 logical-point status capsule without transcript preview.
- Windows paints two UI-thread-owned top-level layered HWNDs from a premultiplied BGRA DIB through `UpdateLayeredWindow(ULW_ALPHA)`. The display is pass-through and nonactivating; only the separate cancel HWND accepts pointer input, without activation.
- The implementation reuses the checked-in Phosphor waveform/X glyphs, a shared contrast-checked Scribe-purple recording token, serialized `Minimal`/`Live`/`Off` values, and the existing overlay controller/action contract. No settings migration was introduced.

## Verified design and behavior evidence

- Deterministic geometry tests cover Live and Compact capsule/control containment at 100%, 125%, 150%, and 200% DPI.
- The visual hierarchy matches the selected source order: waveform, timer, divider, one-line rolling text, cancel.
- Light and dark theme tokens include a translucent painted surface, border, top highlight, contained shadow, and contrast checks against conservative black/white composited backgrounds. The dark recording mark matches the measured reference purple `#8B7CF6`; light mode uses the deeper `#765EEB` companion required for contrast over translucent content.
- The experimental DWM system backdrop was removed. The production path uses deterministic painted translucency without native backdrop blur.
- The waveform base glyph remains contrast-safe while its decorative halo reflects actual microphone level and freezes when reduced motion is requested.
- Unicode tests cover combining marks, CJK, flags, skin-tone modifiers, and ZWJ emoji at the production one-line limit.
- Accessibility tests verify exact cancel naming, a contained 44 x 44 target, non-live tentative text, single-owner Compact/root announcements, and Live precedence of notice/error, committed transcript delta, then phase.
- Foreground policy, stale-session rejection, pending/active abandonment, nonblocking worker drain, target retirement, and absence of final transcription/history/output are covered by automated tests.
- Native tests cover ordered fail-closed presentation, pass-through/control profile separation, DIB sizing, premultiplication, real Phosphor glyph output, tooltip/control capability gating, AccessKit tree integrity, and session binding. Focused gates passed: native 22/22, overlay view 25/25, and theme 8/8.
- Final branch gates passed: formatting, `git diff --check`, all-target/all-feature check, strict Clippy, and the serialized all-target/all-feature suite. The suite discovered 954 tests, passed 943, failed 0, and ignored 11 explicit local runtime/fixture tests.

## Native Windows capture and interaction evidence

All captures below were produced with `Windows.Graphics.Capture` monitor capture and a hardware D3D device at feature level `0xB100`. They show the actual compositor result, not an application framebuffer. Every manifest records both overlay windows visible and uncloaked, the display style `0x80800A8`, the control style `0x8080088`, and an unchanged foreground HWND before, during, and after capture.

- Live light: `design-qa-evidence/overlay-native/live-light-20260817-161758.png` and `.json` (750 x 78 at 120 DPI; 55 x 55 control).
- Live dark: `design-qa-evidence/overlay-native/live-dark-20260817-161801.png` and `.json` (750 x 78 at 120 DPI; 55 x 55 control).
- Compact light: `design-qa-evidence/overlay-native/compact-light-20260817-161703.png` and `.json` (320 x 52 at 96 DPI; 44 x 44 control).
- Compact dark: `design-qa-evidence/overlay-native/compact-dark-20260817-161805.png` and `.json` (400 x 65 at 120 DPI; 55 x 55 control).
- Same-state comparison: `design-qa-evidence/overlay-native/comparison-reference-live-dark.png`.
- Win32/UIA probe: `design-qa-evidence/overlay-native/native-probes-20260817-162324.json`.

The combined comparison was inspected at native resolution. Capsule silhouette/height, content order, purple Phosphor waveform, elapsed time, divider, transcript, X, transparency, and the join between the display/control HWNDs match closely; no black control tile or seam is present. Intentional differences are the theme-aware navy/light painted tint instead of a reference-only neutral dark surface, italic tentative text required by the preview contract, and deterministic translucency without native backdrop blur. Timer/divider alignment is approximately 5-13 logical points left of the reference cluster but remains within the approved 600 x 62 layout and does not impair hierarchy or readability.

The probe verified `HTTRANSPARENT` (-1) for the display and `HTCLIENT` (1) for the control, `MA_NOACTIVATE` (3) for both, server-side UIA providers on both HWNDs, the exact control/button name `Cancel recording and discard it`, Invoke-pattern availability, `IsKeyboardFocusable=false`, and no foreground-HWND change.

## Remaining physical release gates

The deterministic fixture does not open a microphone, issue a real abandon action, or prove taskbar/Alt+Tab enumeration, Narrator/NVDA speech, tooltip dwell, physical pointer routing into an underlying third-party app, every top/bottom monitor edge, or a full mixed-DPI monitor matrix. Those checks remain required through UI-06, UI-08, UI-10, UI-11, REC-04, and the applicable output-target rows in `docs/MANUAL_TEST_MATRIX.md`. They do not block the native visual-fidelity verdict, but they remain release gates for the complete feature.

final result: passed

---

## Accordion width follow-up — 2026-08-13

- Source truth: the user-directed requirement that expanding a model must not change the card's horizontal bounds, applied to the three model-card references listed above.
- Implementation evidence: `design-qa-evidence/implementation-1180x815-expanded-width-fix.png` (1194 x 853 outer window; requested 1180 x 815 client viewport; Windows `PrintWindow` succeeded).
- State: `models/card-expanded`, light theme, installed active card collapsed above an installed inactive card with inline details expanded.
- Full-view and focused-region result: the collapsed and expanded card outlines share the same left and right edges. The divider, three metadata columns, description, language list, runtime information, and any partial-cleanup action remain inside the single outer card frame. No separate crop was needed because both complete outlines and the expanded content are legible together in the full-resolution capture.
- Automated responsive evidence: `expanding_model_details_preserves_the_card_width` compares both card edges in collapsed and expanded states at 1180 x 815, 960 x 680, and the 375 x 680 component viewport.
- Interaction evidence: focused tests retain the correct Install/Retry/Resume/Cancel/Upgrade/Uninstall routing and local/remote partial-cleanup actions after the latest-main integration.
- Console/runtime errors: the native harness remained alive through capture and emitted no visible runtime error; full UI/default test suites and strict Clippy passed on the same staged source.
- Findings: no actionable P0, P1, or P2 issue remains. The targeted width regression is resolved.

final result: passed

---

# Settings design QA

## Evidence

- Source visual truth:
  - `C:\Users\huang\.codex\attachments\f475cd0d-5607-489a-9100-f25a528b6f10\image-1.png`
  - `C:\Users\huang\.codex\attachments\f475cd0d-5607-489a-9100-f25a528b6f10\image-2.png`
  - The follow-up Advanced screenshot was also reviewed from the conversation, but it did not have a workspace file path.
- Implementation screenshots:
  - `C:\Users\huang\Documents\Projects\scribe-gguf-q8-catalog\.codex-artifacts\settings-all-tabs\recording-light.png`
  - `C:\Users\huang\Documents\Projects\scribe-gguf-q8-catalog\.codex-artifacts\settings-all-tabs\recording-help-light.png`
  - `C:\Users\huang\Documents\Projects\scribe-gguf-q8-catalog\.codex-artifacts\settings-all-tabs\advanced-light.png`
  - `C:\Users\huang\Documents\Projects\scribe-gguf-q8-catalog\.codex-artifacts\settings-all-tabs\general-light.png`
- Combined comparison: `C:\Users\huang\Documents\Projects\scribe-gguf-q8-catalog\.codex-artifacts\settings-all-tabs\comparison-recording.png`
- Source pixels: 1082 x 696 and 870 x 174; source density was not declared.
- Implementation viewport request: 1180 x 1200 CSS pixels. Native Windows capture: 1493 x 1487 pixels, including window chrome and display scaling.
- Combined comparison pixels: 2200 x 1500. The implementation was proportionally scaled for the comparison; source pixels were not resampled.
- State: Settings / Recording, General, and Advanced; light theme; default fixture data. The source Recording image is dark while the mode-selector reference is light, so exact palette matching across the two source images was not treated as a requirement.

## Full-view comparison evidence

The file-backed source and final Recording capture were placed together in `comparison-recording.png` and reviewed as one comparison input. The final screen preserves the existing Scribe structure and typography while applying the requested changes: every desktop settings label begins at one fixed left edge, controls begin in one fixed control column, boolean settings use switches, and the mode selector uses the product's blue semantic accent rather than the unrelated purple reference color.

General and Advanced native captures show the same 270-point left-aligned label track. No overlap, clipping, broken card rhythm, or inconsistent control start position was found at the captured desktop viewport. Compact layout behavior is covered by the automated 480-pixel layout test.

## Focused-region comparison evidence

Focused review was necessary because alignment, switch state, and help behavior are too small to judge reliably from the full-screen comparison alone.

- The Recording transcription region replaces the source checkbox with a switch and adjacent 44 x 44 help button.
- `recording-help-light.png` confirms that activating the help button opens readable persistent content below the trigger without obscuring the setting name or switch.
- The Advanced capture confirms that “Stop after speech ends” and all timing-row labels share the same left edge and that timing inputs share the control column.
- The General capture confirms the same label track across switch, button, and combo-box rows.

## Required fidelity surfaces

- Fonts and typography: existing Scribe font family, hierarchy, weight, and wrapping are preserved. The clearer setting names fit without truncation at the desktop reference viewport.
- Spacing and layout rhythm: settings cards, separators, row heights, and section gaps remain consistent. Desktop labels use a fixed 270-point column and left alignment; compact rows stack below the breakpoint.
- Colors and visual tokens: switches and the recording-mode selector use semantic theme tokens. Automated contrast tests cover light and dark selector and inactive-switch states.
- Image quality and asset fidelity: no source imagery or decorative assets are involved. Existing product icons remain unchanged and sharp in the native captures.
- Copy and content: checkbox-era phrases were replaced with concise setting names; supporting detail moved into accessible help disclosures. Locked voice-detection state includes a visible reason.
- Interaction and accessibility: switches expose stable Switch roles, names, descriptions, checked states, and 44-point targets. Help controls expose a keyboard-focusable Toggle pattern, persistent expanded state, and Escape dismissal. Locked switches remain disabled while their help remains available.

## Findings

No actionable P0, P1, or P2 visual or interaction differences remain.

The dark source and light implementation are different theme states, and the implementation capture includes the full application shell while the source is cropped to settings cards. Those are expected evidence differences, not product defects. Dark-theme visual behavior is additionally protected by semantic-token contrast tests; a native dark harness capture is not available because the current harness fixes light visuals.

## Comparison history

1. Initial reference issue: settings labels were centered and boolean settings used checkboxes. Fix: added a fixed 270-point desktop label track, left-aligned label painting, shared switches, clearer labels, and help controls. Post-fix evidence: `recording-light.png`, `advanced-light.png`, and `general-light.png`.
2. Accessibility review issue: the first help affordance was hover-only and locked help could not be opened. Fix: replaced transient tooltips with persistent 44 x 44 disclosure buttons, kept help available beside disabled switches, and added an explicit voice-detection lock reason. Post-fix evidence: `recording-help-light.png` and AccessKit interaction tests.
3. Interaction review issue: a switch could report its pre-click state during the activation frame. Fix: the shared switch now paints and exposes its effective post-click state immediately. Post-fix evidence: bidirectional action and AccessKit checked-state tests.

## Residual manual checks

- Narrator or NVDA announcement wording for the disclosure's expanded state.
- High-contrast mode and display scaling above the captured configuration.
- Touch input on a physical Windows touch device.

final result: passed

---

# Label-adjacent Settings help design QA

## Evidence

- Source visual truth: `C:\Users\huang\.codex\attachments\f475cd0d-5607-489a-9100-f25a528b6f10\image-1.png`
- Rendered implementation: `C:\Users\huang\Documents\Projects\scribe-gguf-q8-catalog\.codex-artifacts\settings-label-help\recording-label-help.png`
- Pinned-disclosure implementation: `C:\Users\huang\Documents\Projects\scribe-gguf-q8-catalog\.codex-artifacts\settings-label-help\recording-help-pinned.png`
- Combined comparison input: `C:\Users\huang\Documents\Projects\scribe-gguf-q8-catalog\.codex-artifacts\settings-label-help\comparison-label-help.png`
- Source pixels: 1080 x 696. Implementation pixels: 1194 x 1190. Combined comparison pixels: 1100 x 780.
- Requested harness viewport: 1180 x 1200 CSS pixels at the host display density. The Windows work area constrained the native outer capture to 1194 x 1190 pixels, including non-client chrome; no density resampling was used for the focused implementation crop.
- State: Settings / Recording / Transcription. The source is the prior dark-theme checkbox layout and the implementation is the deterministic light-theme harness, so this pass compares structure, alignment, control placement, and copy rather than exact cross-theme color values.

## Full-view comparison evidence

The final Recording capture and pinned-disclosure capture were opened and inspected at original resolution. The setting name begins at the same left edge as the other row labels, the `?` immediately follows the name inside the label track, and the toggle remains in the fixed control column. The label-side disclosure does not shift the toggle or alter the card width, separator rhythm, or surrounding controls. The pinned popover stays anchored beneath its `?`, remains readable, and leaves the toggle unobscured.

## Focused-region comparison evidence

The prior and final Transcription regions were cropped without altering their content and placed together in `comparison-label-help.png`. This focused view was required because the label/help relationship is too small to judge reliably from a full-screen capture. It shows the intended change from a centered checkbox-era row to `Live transcription preview (?)` in the left label column with the switch in the trailing control column. No overlap, truncation, or control-column drift is visible.

## Required fidelity surfaces

- Fonts and typography: the existing Scribe family, size, weight, line height, and hierarchy are preserved; the revised label is readable without wrapping or truncation.
- Spacing and layout rhythm: the 270-point desktop label track remains fixed and left-aligned; the 44-point help target fits beside the label without moving the switch. The automated 480-pixel regression verifies compact stacking.
- Colors and visual tokens: the new control reuses the existing panel, border, muted-text, focus-ring, and semantic switch tokens. Theme contrast remains covered by the existing light/dark token tests; exact source-to-implementation palette comparison is intentionally excluded because the evidence uses different themes.
- Image quality and asset fidelity: no product imagery is involved. The existing Scribe glyphs and control rendering remain sharp in the native capture.
- Copy and content: the checkbox-era `Provisional transcription` wording is replaced by the clearer `Live transcription preview`; the explanation is retained in the disclosure description instead of occupying the field column.
- Interaction and accessibility: focused regression tests cover the 300 ms hover delay, icon-to-popover pointer transfer, leave-close behavior, click pin/toggle, outside-click and Escape dismissal, focus exposure, Enter/Space activation, one active disclosure, disabled-setting help, and desktop/compact geometry. Native smoke checks displayed the UI Automation focus ring, exposed the help name and description, opened the pinned popover through a pointer click, kept the process alive, and produced empty stderr.

## Findings

No actionable P0, P1, or P2 visual differences remain for the requested label-adjacent help pattern.

The different source and implementation themes and the source's cropped card versus the implementation's full application shell are expected evidence differences. They do not change the evaluated alignment or information hierarchy.

## Comparison history

1. Earlier implementation: the help button was rendered beside the setting field. Fix: moved shared help rendering into the Settings row's label cell for both desktop and compact layouts. Post-fix evidence: `recording-label-help.png` and `comparison-label-help.png`.
2. Review finding: independent row state could compete, the disclosure exposed an invalid checked state, and Escape/same-button dismissal could reopen it. Fix: introduced one shared active-help state, retained only the valid expanded state, and added dismissal suppression until leave/re-entry. Post-fix evidence: six focused interaction test groups and the final accessibility/code review pass.

## Residual manual checks

- Narrator or NVDA announcement wording on a physical Windows desktop.
- Pointer timing and popup placement at unusual display scales and near viewport edges.
- Touch input on a Windows touch device.
final result: passed

---

# Two-column model cards current implementation QA — 2026-08-13

## Evidence

The following native Windows captures were produced from the current staged implementation with the UI harness and inspected at original resolution. The requested client rectangles were verified before every capture.

- 1180 x 815 installed idle: `design-qa-evidence/two-column-1180x815-installed-idle.png`
- 1180 x 815 installed pointer hover: `design-qa-evidence/two-column-1180x815-installed-hover.png`
- 1180 x 815 installed keyboard focus: `design-qa-evidence/two-column-1180x815-installed-focus.png`
- 1180 x 815 expanded top: `design-qa-evidence/two-column-1180x815-expanded-top.png`
- 1180 x 815 expanded scrolled: `design-qa-evidence/two-column-1180x815-expanded-scrolled.png`
- 960 x 680 installed idle: `design-qa-evidence/two-column-960x680-installed-idle.png`
- 960 x 680 installed pointer hover: `design-qa-evidence/two-column-960x680-installed-hover.png`
- 960 x 680 installed keyboard focus: `design-qa-evidence/two-column-960x680-installed-focus.png`
- 960 x 680 expanded top: `design-qa-evidence/two-column-960x680-expanded-top.png`
- 960 x 680 expanded scrolled: `design-qa-evidence/two-column-960x680-expanded-scrolled.png`

## Implementation findings

Implementation-only visual inspection passed with no P0, P1, or P2 finding at either supported native width. Collapsed cards retain the intended identity/metrics hierarchy, keep the Active badge adjacent to the short model name, place SPEED and ACCURACY labels above continuous meters, expose contained red-outline Delete actions and trailing chevrons, and preserve their right edges without horizontal clipping. Pointer-hover captures show the accent border, tint, and elevated shadow. Keyboard-focus captures show both the focused descendant control and the containing card's focus-within surface.

Expanded cards retain the same left and right edges as collapsed neighbors and hide the collapsed preview. Their readable section order is DESCRIPTION, LANGUAGES, FEATURES, REQUIREMENTS, followed by conditional MAINTENANCE when applicable. REQUIREMENTS uses three distinct responsive cells with muted uppercase labels above normal values; the installed fixture renders RAM `75MB`, ON DISK `75MB`, and GPU `Unknown`, which is truthful for its unknown capability metadata. No technical repository, revision, SHA, or architecture metadata and no nested decorative rail are present.

## Source-reference blocker

Source-to-implementation comparison is not verified because the exact newer selected two-column source reference has no local attachment path in this workspace. The older `image-1`, `image-2`, and `image-3` references were not substituted, and no combined reference comparison was created. Reattach or provide the exact path to the newer selected source reference to complete source fidelity review.

final result: blocked

# Full-width interactive model cards QA - 2026-08-14

## Verified implementation evidence

- Full UI-harness suite: 823 passed, 0 failed, 11 ignored.
- Full default suite: 763 passed, 0 failed, 10 ignored.
- Strict all-targets UI-harness Clippy, UI-harness check, formatting, and diff checks passed.
- Deterministic geometry covers the 60/40 desktop split at 1180 x 815 and
  960 x 680, stacked 375 px component layout, equal collapsed/expanded card
  edges, in-place description/language expansion, and the responsive feature
  grid.
- Deterministic interaction coverage verifies pointer, Enter, Space, and
  AccessKit Default activation for ready inactive installed cards, plus child
  lifecycle/disclosure/feature/maintenance exclusion and action precedence.
- Windows UI Automation exposed the complete current Models route while the
  process remained responsive: the full-card
  `Use whisper.cpp tiny.en for future transcriptions`
  target, lifecycle controls, feature group, and disclosure controls were all
  present with their expected names and roles.

## Native presentation blocker

Fresh implementation screenshots could not be accepted because the current
Windows console session presents the Scribe OpenGL client as solid white. The
same behavior occurs for the normal application, the current UI-harness build,
and a clean detached build of the previously visually validated commit
`e72ab68`. Both foreground screen capture and PrintWindow reproduce the blank
client while the process remains responsive, emits no panic or stderr, and
continues to expose a populated accessibility tree. This A/B result rules out
the full-width model-card changes as the cause and identifies an external
NVIDIA/OpenGL session presentation failure. Invalid blank PNGs were removed
and are not evidence.

Recovery requires a user-visible graphics-driver/session reset, sign-out, or
reboot. No such system-wide action was performed implicitly. Native idle,
hover, keyboard-focus, expanded, and scrolled captures at 1180 x 815 and
960 x 680 must be repeated after recovery.

## Source-reference blocker

The two newer selected reference images are visible in the conversation but
have no local attachment paths in this workspace. Older model-card references
were not substituted. Source-to-implementation comparison therefore remains
blocked until the exact newer references are reattached or their local paths
are provided.

final result: blocked

---

# Model card feature and spacing refinement QA - 2026-08-14

## Verified deterministic evidence

- Model-card presentation exposes exactly Native streaming, Translation, Word
  timestamps, and Batch transcription while retaining the complete internal
  capability projection.
- The all-capabilities fixture renders four 28 x 32 px summary slots with 8 px
  gaps; the current curated feature pair renders a compact 64 x 32 px group.
  Individual hover tooltips remain non-activating and the cluster remains one
  accessibility group.
- At 1180 x 815 and 960 x 680, Speed and Accuracy have equal top-row regions,
  features and lifecycle have equal bottom-row regions, and the 44 x 44
  disclosure stays in the trailing rail.
- The language row's actual left edge aligns with both the title and
  description axes within one pixel. Compact codes remain unchanged when the
  card expands; full names remain available through tooltip and accessibility
  description.
- Rendered layout diagnostics verify 6 px around the divider, 6 px from each
  section heading to its content, 12 px between Features, Requirements, and
  Maintenance, 32 px expanded feature rows with 4 px row gaps, and natural
  Requirements height.
- Full UI-harness suite: 828 passed, 0 failed, 11 ignored.
- Full default suite: 764 passed, 0 failed, 10 ignored.
- Strict all-targets UI-harness Clippy, all-targets/all-features check,
  formatting, and diff checks passed.

## Native presentation blocker

Fresh native visual evidence was not accepted because the current Windows
graphics session has not been reset since the independently diagnosed
NVIDIA/OpenGL white-client failure. The same external failure previously
reproduced on both this branch and a known-good historical commit while the
application remained responsive and exposed a populated accessibility tree.
No blank capture is retained as evidence. After a graphics-session reset,
repeat the 1180 x 815 and 960 x 680 idle, hover, keyboard-focus, expanded-top,
and expanded-scrolled captures.

## Source-reference blocker

The latest selected reference images are visible in the conversation but do
not have resolvable local attachment paths in this workspace. Older references
were not substituted. A same-state, same-viewport source comparison therefore
remains blocked until those exact images are reattached or their local paths
are provided.

final result: blocked

---

# Three-zone model cards and download progress QA - 2026-08-14

## Visual source and review result

The newest user-provided sketch is the visual source of truth for this pass,
but it has no resolvable local file path in the workspace. Older references and
captures were not substituted. A same-state, same-viewport source comparison
therefore cannot be performed, and this section does not claim visual parity or
a visual pass.

## Verified deterministic evidence

- The focused model-card matrix passed 22 tests with 0 failures. It covers the
  exact contiguous 50/24/26 desktop zones at 1180 x 815 and 960 x 680, equal
  Speed/Accuracy cells, the fixed 44 x 44 chevron inside lifecycle, the 619/620
  stack boundary, 375 px containment, equal collapsed/expanded edges, and the
  shrink-width invariant.
- One or two core features occupy one 32 px row; three or four use at most two
  columns and a second 32 px row with 8 px column and row gaps. Only Native
  streaming, Translation, Word timestamps, and Batch transcription are visible,
  tooltip-enabled, and grouped for accessibility.
- Rating coverage exercises every 1..5 proportional fill, the shared five-bin
  light/dark color mapping with at least 3:1 non-text contrast, equal metric
  cells, visible labels, AccessKit Meter names and numeric values, and an empty
  non-numeric unknown state.
- Lifecycle coverage verifies inverse Install, Retry, and Resume actions,
  destructive Delete, named 44 x 44 Cancel, child-control isolation from full
  card selection, clamped determinate percentages, full-height 6 px tracks and
  fills, concise byte labels, truthful unknown totals, remote progress only
  after live byte projection, and removal of progress after download state ends.
- Existing pointer, Enter, Space, and AccessKit Default selection coverage,
  accordion semantics, removal focus restoration, active-removal revalidation,
  partial cleanup, legacy cleanup, and safe removal routing remain green.
- Full `ui-harness` suite: 831 passed, 0 failed, 11 ignored. Full default suite:
  765 passed, 0 failed, 10 ignored. Strict all-target/all-feature Clippy with
  warnings denied, all-target/all-feature check, formatting, and diff checks
  passed under the Visual Studio x64 environment with bundled CMake and MSVC.

## Fresh native implementation evidence

Four current-branch client captures survived original-resolution inspection:

- `design-qa-evidence/three-zone-1180x815-installed-idle.png` - installed idle,
  1475 x 1019 physical pixels at 120 DPI, corresponding to an 1180 x 815 logical
  egui client.
- `design-qa-evidence/three-zone-960x680-installed-idle.png` - installed idle,
  1200 x 850 physical pixels at 120 DPI, corresponding to a 960 x 680 logical
  egui client.
- `design-qa-evidence/three-zone-1180x815-expanded.png` - expanded-card top,
  1475 x 1019 physical pixels at 120 DPI. The lower expanded details are outside
  the visible area or covered by the comparison dock.
- `design-qa-evidence/three-zone-960x680-expanded.png` - expanded-card top,
  1200 x 850 physical pixels at 120 DPI. The lower expanded details are outside
  the visible area or covered by the comparison dock.

The idle captures show the current Models client and the three-zone installed
cards. The expanded captures show the expanded `tiny.en` identity and top detail
state. They are implementation-only evidence; without the source artifact they
cannot establish source fidelity.

## Exact blockers and comparison gaps

- Source artifact: the newest sketch has no local path, so no source crop,
  overlay, side-by-side, or measured source comparison is available.
- Full-view comparison: no accepted current capture shows the complete expanded
  Features, Requirements, and conditional Maintenance surface at either
  viewport.
- Focused comparison: no accepted current capture proves pointer hover,
  keyboard focus/focus-within, or a visible downloading progress module at
  either viewport.
- Native state confirmation: one attempted downloading capture contained the
  Windows task switcher and another did not expose a progress card. Both invalid
  generated files were deleted and are not evidence. Further capture stopped
  because the available helper could not simultaneously prove the exact titled
  Scribe HWND, client rectangle, and UI Automation state target.

Required fidelity surfaces still needing direct review are the 50/24/26 visual
balance, metric alignment and color progression, one-row/two-row feature
geometry, inverse and destructive lifecycle treatments, hover elevation,
focus-within ring, determinate and unknown-total progress modules, 44 x 44
Cancel/disclosure targets, and complete expanded section spacing at both
viewports.

## Recovery steps

1. Reattach the newest sketch or provide its exact local path.
2. Use a title-aware capture flow that selects the exact Scribe HWND, verifies
   the logical client size, and asserts the target card/control state through
   UI Automation before each capture.
3. Capture idle, hover, keyboard focus, visible downloading, expanded top, and
   expanded scrolled states at logical 1180 x 815 and 960 x 680.
4. Inspect every PNG at original resolution, reject contaminated or stale
   frames, then compare each accepted state directly with the source artifact.

final result: blocked

---

# Stable download controls and progress QA - 2026-08-14

## Source and result

The two newest user-provided progress crops are the selected visual source for
this pass. They are visible in the conversation but have no resolvable local
attachment path in the workspace, so they cannot be opened beside the native
captures as a same-input comparison. Older references were not substituted.
Source-fidelity QA is therefore blocked even though current implementation
evidence is available.

## Deterministic verification

- The exact job-bound discard flow retains partial bytes on Pause, queues X
  cleanup only for the matching active job, rejects stale completion events,
  waits for that worker to exit before deletion, and lets a successful verified
  activation win without deleting the installed artifact.
- Local and trusted remote retained-byte metadata is revalidated and projected
  after restart without persistence or catalog-format changes. Existing regular
  file, reparse-point, pinned-artifact, mutation-barrier, and remote-snapshot
  safeguards remain in the deletion path.
- Model descriptions remain the catalog description in downloading, paused,
  verifying, failed, and legacy states. No visible lifecycle status or
  percentage text replaces the description.
- Known totals render a measured, stable-width `downloaded / total` label.
  Unknown totals render `downloaded / Total unknown` and an indeterminate Meter
  without a fabricated fill. The full 6 px track is below the header controls.
- Active downloads expose named 44 x 44 Pause and X controls. Retained and
  failed-with-partial states expose Play and X. Failed-without-partial exposes
  inverse Install plus a separate 44 x 44 Warning popover control. Hover,
  click, Enter, Space, Escape, outside dismissal, full error content, and
  full-card activation exclusion are covered deterministically.
- Desktop 50/24/26 geometry, centered 100 px lifecycle body, fixed chevron rail,
  responsive second control row, 375 px containment, full-card selection,
  removal safety/focus restoration, legacy cleanup, and accordion edges remain
  covered.
- Final full UI suite: 849 passed, 0 failed, 11 ignored. Final default suite:
  781 passed, 0 failed, 10 ignored. Strict all-target/all-feature Clippy with
  warnings denied, all-target/all-feature check, formatting, and diff checks
  passed under Visual Studio x64, bundled CMake, and the Hostx64 MSVC linker.

## Native implementation evidence

Sixteen current-build client captures were decoded and inspected at original
resolution. Logical 1180 x 815 captures are 1475 x 1019 physical pixels and
logical 960 x 680 captures are 1200 x 850 at the current 120-DPI scale. Fourteen
captures are usable implementation evidence; both paused captures were rejected
under the harness capture-rejection rules.

- `stable-download-1180x815-downloading-scrolled.png` and
  `stable-download-960x680-downloading.png` show the retained byte label,
  Pause/X header, complete track, and proportional fill without visible percent
  or status copy.
- `stable-download-1180x815-paused.png` and
  `stable-download-960x680-paused.png` contain visibly partial, black-framed
  client content. They are rejected and do not prove the paused state.
- `stable-download-1180x815-failed-partial.png` and
  `stable-download-960x680-failed-partial.png` show stable Play/X placement and
  retained progress for failed-with-partial state.
- `stable-download-1180x815-failed-alert.png` and
  `stable-download-960x680-failed-alert.png` show the separate Warning and
  inverse Install controls without a progress module in the closed state. No
  accepted native capture shows the warning popover open; its interaction is
  covered only by deterministic tests in this evidence set.
- The 1180 and 960 idle, focus, expanded-top, and expanded-scrolled captures
  confirm unchanged three-zone card geometry and the expanded Features,
  Requirements, and Maintenance surfaces. The scrolled expanded captures also
  confirm the duplicate Discard-partial maintenance action is absent.

The captures are implementation evidence only. They do not establish fidelity
to the selected progress crops because the exact source files cannot be opened
in the comparison workflow.

## Remaining blocker

Reattach the two selected progress crops or provide their exact local paths,
produce clean paused captures at both logical viewports and an accepted native
warning-open capture, then perform the same-state comparison before changing
this result to passed. Draft PR #38 must remain unchanged while this Product
Design gate is blocked.

final result: blocked

---

# Inline progress layout QA - 2026-08-14

## Source and comparison set

This pass uses the three newest local reference attachments directly:

- `C:\Users\huang\.codex\attachments\764e8dcf-4492-46da-b4c5-a76f2267faf2\image-1.png`
- `C:\Users\huang\.codex\attachments\764e8dcf-4492-46da-b4c5-a76f2267faf2\image-2.png`
- `C:\Users\huang\.codex\attachments\764e8dcf-4492-46da-b4c5-a76f2267faf2\image-3.png`

The references and current-build captures were opened together at original
resolution. The newest instruction intentionally supersedes the reference-1
vertical control stack: the byte label remains above, while the complete 6 px
track, Pause/Play, and X now share the following row.

## Accepted current-build evidence

- `design-qa-evidence/inline-progress-1180x815-downloading-v3.png`
- `design-qa-evidence/inline-progress-960x680-downloading.png`
- `design-qa-evidence/inline-progress-1180x815-same-fixture-collapsed.png`
- `design-qa-evidence/inline-progress-960x680-same-fixture-collapsed.png`
- `design-qa-evidence/inline-progress-1180x815-expanded.png`
- `design-qa-evidence/inline-progress-960x680-expanded.png`

Logical 1180 x 815 captures are 1475 x 1019 physical pixels and logical
960 x 680 captures are 1200 x 850 at the current 120-DPI scale. A valid but
off-state 1180 capture that did not show the downloading card was rejected and
deleted before this report.

## Comparison result

- At both viewports the track, Pause, and X are ordered left-to-right on one
  line, remain contained, and preserve their full control targets.
- The byte label has its own stable measured slot above the track/control row.
  Deterministic early/near-complete fixtures with the same total prove that the
  label slot, track, Pause, and X bounds do not move as the byte count grows.
- The full progress module is vertically centered in the fixed lifecycle body.
  Install and Delete share the same vertical-center contract.
- Collapsed and expanded cards retain the same title, description, language,
  metric, lifecycle, and chevron origins for a one-line description. A truly
  overflowing expanded description keeps the preview baseline and grows only
  downward; it does not compress or pull the summary upward.
- The neutral card surface, 50/24/26 zones, feature treatment, meter colors,
  shadows, and focus treatment remain consistent with the supplied card
  references. No P0, P1, or P2 visual defect was found.

## Verification

- Focused screen tests: 70 passed, 0 failed.
- Focused harness tests: 69 passed, 0 failed, 1 pre-existing ignored.
- Full all-feature suite: 853 passed, 0 failed, 11 ignored.
- Full default suite: 783 passed, 0 failed, 10 ignored.
- Strict all-target/all-feature Clippy with warnings denied, all-target/all-
  feature check, formatting, and diff checks pass in direct Visual Studio
  Developer PowerShell. No `cmd.exe` wrapper is used by the documented launch
  or verification workflow.

final result: passed

---

# Transcribe selector hotkey wrap - Product Design QA - 2026-08-15

## Review scope and source

- Reviewed head: `82b708e`.
- Source visual truth: `C:\Users\huang\.codex\attachments\e11279a4-7377-4a61-b924-44310d219c48\image-1.png` (1207 x 226 source pixels).
- Targeted states: `transcribe/ready` at 1180 x 815 and 960 x 680, plus the 375 px component state.
- Implementation screenshot path is unavailable because the Windows Computer Use Sky runtime was unavailable. No same-viewport/density screenshot or combined comparison exists.

## Verification and findings

- Automated evidence passed for the exact threshold, long-model/chord containment, the 72 x 44 Change target, AccessKit groups, and focused selector coverage (10/10).
- Full suite: 882 passed, 11 ignored. `fmt`, `check`, and `clippy` passed.
- Accessibility and UI/UX code/test reviews passed.
- Native screenshot, full-view comparison, and focused-region comparison are blocked and must not be claimed as verified.

## Comparison history

1. Initial fixed 220-280 hotkey overflow.
2. Intrinsic hotkey narrow fallback.
3. Long-model 375 px reflow.
4. Duplicate 8 px gap/repaint regression.

## Required fidelity surfaces

- Typography and full name: unchanged.
- Spacing and layout: unchanged outside the requested wrap behavior.
- Colors: unchanged.
- Imagery: not applicable.
- Copy: unchanged.

final result: blocked
