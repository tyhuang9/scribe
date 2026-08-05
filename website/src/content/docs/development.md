---
title: Development
description: Build and validate the native application and its documentation.
---

The desktop application is Rust with egui/eframe. The full contributor gates are:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
cargo build --all-features
```

Runtime changes must also run the architecture-boundary guard, relevant native fixture smoke tests, and release/package builds described in the implementation report. Keep runtime work local and reproducible; do not turn development fallback paths into a support claim.

## Documentation site

The documentation lives in `website/` and uses Astro with Starlight:

```bash
cd website
npm ci
npm run dev
npm run check
npm run build
```

The source of truth for product behavior is the checked-in application code and root README. This site is the curated user and contributor guide; update it when a verified product boundary changes.
