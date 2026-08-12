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
- `settings-general-1180x815.png` — General sections, dense rows, conditional separators, and scroll containment.
- `settings-advanced-1180x815.png` and `settings-advanced-bottom-1180x815.png` — Advanced at the top and scrolled to the Developer section.
- `settings-advanced-playground-control-1180x815.png` — enabled developer tooling with the Settings-contained Open model Playground action.
- `settings-about-1180x815.png` — flattened About content inside the single Application Settings section.
- `settings-recording-1280x1024.png` — implementation capture at the exact Product Design reference size.
- `comparison-settings-recording.png` — combined same-state reference and implementation comparison; the Handy image is treated as directional, while Scribe's approved navigation and information architecture remain authoritative.

Direct inspection found no clipping, taskbar contamination, overlapping controls, blank conditional-row gaps, or separators without adjacent rows. The four primary navigation items and four Settings tabs remain visible and ordered correctly. The 960x680 view transitions to the compact rail and preserves scrolling. The Playground open/back interaction is additionally exercised through rendered pointer and focus tests.

## Automated verification

Frozen HEAD before this documentation-only update: `6b26448a0e6baceaf0b0bee24656602352fc76c4`.

- `cargo fmt --all --check`: passed.
- `git diff --check`: passed.
- `cargo check --all-targets --all-features`: passed with zero warnings.
- `cargo clippy --all-targets --all-features -- -D warnings`: passed.
- `cargo test --all-targets --all-features -- --test-threads=1`: 749 passed, 0 failed, 9 ignored, 0 measured, 0 filtered; 758 total.
- Architecture guards: 7 passed within the full test run.
- The 9 ignored tests are pre-existing local GGUF/audio, live catalog, whisper.cpp/JFK, faster-whisper, and CUDA/GPU runtime smoke tests; none was enabled for this branch.

Focused regression coverage includes the exact navigation/tab inventory, legacy Output and Debug redirects, real Settings interaction bounds at desktop and compact widths, conditional-row separator transitions, Playground open/back/route-exit/focus behavior, passive-monitor predicate/idempotence/teardown/repaint timing, About hierarchy, and focused-tab keyboard navigation.

## Specialist verdicts

- Code review: approved after all requested changes were resolved.
- Security and privacy: passed with no Critical, High, or Medium findings; diagnostics remain redacted and local, and the passive monitor retains no PCM data.
- Accessibility: passed; actual Settings controls meet the 44 px target floor, disabled diagnostics export is programmatically described, and Playground focus enters and returns correctly.
- Performance: passed after Settings model projection was reduced to one selected-model resolution per frame; passive repainting remains scoped to the live meter session.
- UI/UX: passed after Playground route-exit cleanup and flattened About hierarchy.
- QA: passed on the frozen all-target/all-feature gate above.

## Remaining limitation and deliberate non-goals

The deterministic harness and state tests do not open a real microphone, so operating-system-level device release during native monitor teardown was not directly observed. Model downloads, live catalog access, transcription runtime smoke, audio capture, and GPU/CUDA smoke were deliberately excluded by branch scope; the corresponding ignored tests were not run.
