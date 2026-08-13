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
