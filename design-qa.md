# Design QA - Models simple cards

Final result: passed

## Scope

- Search, language filtering, explicit refresh, and local import controls.
- Default-open Installed and Available disclosures with search-forced expansion.
- One fixed-height card surface for installed and available models.
- Friendly descriptions plus language, capability, size, speed, accuracy, and state.
- Whole-card primary actions with separate Details, Remove, and Cancel controls.
- Fixed-height culling, accessible paging, focus restoration, and modal isolation.
- Floating comparison dock with an exact 24px viewport gap and 24px final-card clearance.

## Native visual evidence

Fresh debug-harness processes were captured from the actual Windows framebuffer using DPI-aware client coordinates (125% display scale).

- `1476x1018` Models installed: search/filter/actions, both disclosures, all visible cards, and the collapsed comparison dock remain inside the content column; the dock floats 24px above the viewport edge.
- `1180x815` Models comparison: the expanded dock is wholly contained, its compact result groups and Start action remain visible, and the fixed header stays above the internally scrollable body.
- `960x680` Settings recording: no Models work regresses the compact route shell or Settings scrolling.
- `1280x1024` same-state captures were combined side-by-side with the repository references for installed Models, expanded comparison, and Settings recording before this verdict.

Capture directory:
`C:\Users\huang\AppData\Local\Temp\scribe-ui-acceptance\models-simple-cards-20260811`

Combined comparisons:

- `comparison-models-installed.png`
- `comparison-models-expanded.png`
- `comparison-settings-recording.png`

## Automated evidence

- Search temporarily expands both result groups without changing saved disclosure state; forced disclosures are disabled and explain how to restore the saved state.
- Main card names and child controls hide technical variants; multiple remote artifacts use visible size metadata to remain distinguishable.
- Fixed card height and total content height stay invariant as the culled window changes.
- AccessKit paging reaches every item in both directions without gaps while offscreen cards remain absent.
- Pointer, Enter, Space, Page Up/Down, Details/Remove restoration, import cancellation, and modal background rejection are covered.
- Remote ratings remain `Not rated` unless the catalog provides an honest rating.
- Expanded comparison focus scrolls only its internal body; the route scroll remains stationary.
- Final card clearance is 24px in collapsed and expanded dock states.

Final gate: formatting, all-target/all-feature check, warnings-as-errors Clippy, diff hygiene, and 733 passing tests; 9 environment-gated runtime/network/benchmark smokes remain intentionally ignored.

## Findings

- P0: none.
- P1: none.
- P2: none.
- Evidence limitation: the preferred Windows Sky automation runtime was unavailable, so native verification used fresh PID-bound DPI-aware framebuffer captures. Real model downloads and speech-runtime execution remain outside this UI branch.
