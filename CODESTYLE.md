# Code style

## Rust code

- Prefer small modules named for the responsibility they implement. No empty
  adapter crates, unused abstractions, unreachable branches, or stub functions.
- Use clear types for frame counts, timestamps, source references, and
  track/clip identities when those operations are introduced. Avoid mixing
  source time, timeline time, audio samples, and UI coordinates.
- Keep saved project and timeline data independent of CreamUI, decoders,
  `wgpu`, and audio devices. Put media engines, preview, and export near
  their respective features; do not swallow their errors or log sensitive
  media contents.
- Keep UI state (selection, playhead, scroll, focus) distinct from the saved
  project. Don't clone whole projects or decoded frames just to repaint.
- Format Rust with `cargo fmt`. Add focused tests for changes and run the
  checks in [CONTRIBUTING.md](CONTRIBUTING.md).

## Comments and deferred work

- Name code so the normal flow needs no narration. Comment only a
  non-obvious invariant or constraint that cannot be expressed clearly in
  code. Do not add task, PR, or changelog comments.
- No commented-out code, inline `TODO`/`FIXME`, or pretend implementations.
  Record concrete unfinished work in [TODO.md](TODO.md), not in source.
- Treat [ROADMAP.md](ROADMAP.md) as intended scope, not proof that an item
  has shipped. Keep the README and architecture guide aligned with reality.
