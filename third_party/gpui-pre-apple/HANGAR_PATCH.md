# gpui-pre-apple 0.3.7, patched for Hangar

Source: crates.io `gpui-pre-apple` 0.3.7 (Apache-2.0, see `LICENSE-APACHE`), the Metal renderer behind
`gpui-pre` 0.3.7 that our gpui-component pin uses. Wired in by `[patch.crates-io]` in the root
`Cargo.toml`; excluded from the workspace (no fmt, clippy or lints). The vendored `Cargo.lock` was dropped
and `deprecated` is allowed in its `Cargo.toml` (upstream cocoa usage).

## Why
`Window::paint_surface(CVPixelBuffer)` is GPUI's only zero-copy path for GPU surfaces, but the stock
renderer asserts the buffer is NV12 (`420YpCbCr8BiPlanarFullRange`). The iOS simulator framebuffer is
a BGRA IOSurface; without this patch every frame is copied on the CPU and re-uploaded to the atlas.

## Change (`src/metal_renderer.rs`, `src/shaders.metal`)
Port of zed-industries/zed PR #61291 (by michaelsieminski, open as of 2026-10-08):
- `draw_surfaces` dispatches on the pixel format: `32BGRA` binds one `BGRA8Unorm` texture through the
  existing `CVMetalTextureCache` and draws with a new `surfaces_bgra` pipeline; NV12 is unchanged;
  any other format is logged and skipped instead of panicking.
- New `surface_fragment_bgra` shader.

Difference from the PR: the BGRA shader samples with linear filtering (the PR uses nearest), because
Hangar draws device screens scaled down to fit the panel.

## Removal
Delete this directory and the `[patch.crates-io]` entry once the gpui-component pin moves to a GPUI
whose renderer accepts BGRA surfaces.
