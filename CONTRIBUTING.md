# Contributing to Ocus

These guidelines apply to human contributors and coding agents. Start with the
[README](README.md), [architecture guide](ARCHITECTURE.md), and
[code style](CODESTYLE.md). For native dependencies and OS-specific build
instructions, see [DEVELOPMENT.md](DEVELOPMENT.md). The [roadmap](ROADMAP.md)
describes the intended releases; the README describes what runs today.

## Development workflow

1. Pick one user workflow or a narrow foundation it requires. Locate the
   relevant `project`, `timeline`, `media`, `ui`, or app startup code first.
2. Keep changes to saved project state behind editor commands so undo/redo
   can account for them. Add tests for the command and its affected data or
   format. For bugs, reproduce the failure before fixing it.
3. Keep native dependencies inside the feature that uses them. Do not add
   FFmpeg, audio, GPU compositor, or background-worker dependencies until
   the code using them works and can be tested. Never mutate imported files.
4. Update relevant documentation when observable behavior, project format,
   commands, or architecture changes. Put concrete deferred work in
   [TODO.md](TODO.md); keep milestones in [ROADMAP.md](ROADMAP.md).

Use Rust and Cargo. The desktop shell depends on a pinned CreamUI GitHub
revision with GPU viewport support, not a local path dependency. Media probing uses
`ffmpeg-next`; building requires FFmpeg development libraries and `libclang`:

```sh
cargo run --locked
cargo fmt --all --check
cargo test --all-targets --locked
cargo clippy --all-targets --locked -- -D warnings
```

`cargo run` needs a graphical desktop session. The model and UI rendering tests
run headlessly but still link FFmpeg. Add dependencies with `cargo add`, commit
`Cargo.lock`, and enable only the features an implemented workflow uses.
Use the committed GitHub revision of CreamUI, not a sibling checkout.

## Review boundaries

- A UI control must perform its stated action; do not expose an inactive
  import, playback, save, or export button as if it works.
- Do not decode media or run encoding on the UI thread. Background work should
  report bounded progress and allow cancellation where the workflow needs it.
- Changes to serialized project fields need versioning/migration decisions,
  round-trip tests, and behavior for unsupported versions before release.
- Check empty/error states, small windows, keyboard access, clipping, and CPU
  fallback when touching the interface or preview.
- Measure startup, idle memory, playback, and scrubbing before claiming the
  [performance targets](ROADMAP.md#performance-targets) are met.

## Commits and pull requests

Use Conventional Commits with an English, imperative description:
`feat(timeline): add clip trim commands`, `fix(project): reject missing media`,
or `docs: explain the preview pipeline`. Use `feat`, `fix`, `refactor`, `perf`,
`test`, `docs`, `build`, `ci`, or `chore` as appropriate; the type is not the
literal word “description”. Keep each commit coherent, include its tests, and
review `git diff --cached --check` before committing. Do not add generated-by
or co-author trailers for tools; credit a co-author only when someone actually
co-authored the change.

Describe the behavior, verification, and any untested platform or remaining
limitation in a pull request. Do not push or rewrite shared history unless
explicitly requested.
