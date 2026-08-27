# Scribe application-icon tile

`scribe-app-icon.png` is the approved application-icon raster supplied for the
Scribe brand refresh. It is deliberately kept as the original 128×127 PNG;
the native consumers deterministically normalize it to square output with an
area-weighted box resample at render time rather than modifying the supplied
pixels.

- SHA-256: `f836d49b93ba3e2027d31e10588fe30f755837f912dc59e0e94c0565ded0aac4`
- Intended consumers: the native window icon, system-tray icon, and sidebar
  brand tile.
- Canvas: opaque Deep Navy is intentional. Do not replace this raster with a
  redrawn mark or a transparent approximation.
