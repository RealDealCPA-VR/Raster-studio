# Golden-Image Tests

Reference images for deterministic compositor validation. For each blend mode,
filter, mask, transform, and adjustment:

1. Load a deterministic fixture (see `../project-fixtures`).
2. Render at a known scale + color setup.
3. Compare against the checked-in expected image with a small, defined
   tolerance (per-pixel RMS or max-channel delta).
4. Where feasible, run **both** the GPU path and a software/reference path
   (the CPU reference functions in `layer-model::BlendMode::blend_channel` and
   the `adjustments` crate) and assert they agree.

## Conventions

- Expected images: `<category>/<name>.expected.png` (committed).
- Actual/diff output on failure: `_actual/` and `_diff/` (git-ignored).
- Keep fixtures tiny (e.g. 64×64) so tests are fast and diffs are reviewable.

## Regenerating goldens

Regeneration must be an explicit, reviewed action — never automatic — so a
rendering regression can't silently rewrite its own baseline. Provide a
`--bless` flag on the test binary (Phase 1) that writes new expected images
only when passed.
