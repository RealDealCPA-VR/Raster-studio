# Project Fixtures

Small, deterministic `.rstudio` packages and source images used by integration
and golden-image tests. Keep them tiny and committed so tests are hermetic.

Suggested fixtures:

- `flat-rgba/` — a 64×64 solid-color layered project (blend-mode tests).
- `two-layer-mask/` — two raster layers + a raster mask (masking tests).
- `adjustment-stack/` — a raster layer under levels/curves adjustments
  (non-destructive reload tests).

The vertical-slice integration test (`../integration/tests/vertical_slice.rs`)
builds its documents programmatically and uses a temp dir, so it needs no
committed fixture — prefer that style for logic tests and reserve committed
fixtures for pixel-exact golden comparisons.
