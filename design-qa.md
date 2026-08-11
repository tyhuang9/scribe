# Design QA — scrolling, comparison, naming, and runtime follow-up

Final result: passed

## Scope

- Shared 28px route inset and full-viewport scroll ownership.
- Models comparison dock containment, responsive results, internal scrolling, and modal layering.
- Installed-row bounds, model display names/variants, and runtime-readiness messaging.
- Settings and long production-route keyboard focus reachability.

## Native visual evidence

All captures use a fresh debug harness process and DPI-aware client coordinates.

- `1476x1018`: collapsed Models dock is visible; model rows, metadata, chevrons, Compare, and Add models remain inside the content inset.
- `1180x815`: expanded Models dock stays inside the viewport, uses compact result groups, and keeps Start test recording plus accuracy actions visible.
- `960x680`: Settings remains bounded and coherent; overflowing content is owned by the outer route scroll area.

Capture directory:
`C:\Users\huang\AppData\Local\Temp\scribe-ui-acceptance\final-layer-row-fix-20260811`

## Automated evidence

- Exact heading inset across production routes and the shared harness shell.
- Edge-owned route scroll, final-control focus scrolling, and exact 24px dock clearance.
- Four-result comparison body scrolling at all three viewports with a fixed header and stationary outer route.
- Foreground dock in normal use; demoted, inert dock below Add/Details/Remove modal windows.
- AccessKit table/group hierarchy, dialog isolation, variant labels, runtime warnings, and disabled Compare reason.
- Stable built-in model IDs and artifacts with explicit display and variant labels.

Final gate: formatting, all-target/all-feature check, warnings-as-errors Clippy, diff hygiene, and 724 passing tests; 9 runtime/network/benchmark smokes remain intentionally ignored.

## Findings

- P0: none.
- P1: none.
- P2: none.
- Evidence limitation: the preferred Windows Sky automation runtime was unavailable, so native verification used DPI-aware framebuffer captures. Real speech-runtime execution was intentionally outside this UI change.
