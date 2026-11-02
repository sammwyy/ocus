# Ocus

Ocus is an early-stage, lightweight video editor written in Rust. The
[roadmap](ROADMAP.md) targets importing media, editing a timeline, previewing,
and exporting an MP4 in v0.1. **Those editing workflows are not implemented
yet.** Today the repository builds a CreamUI desktop shell with an integrated
titlebar, four timeline tracks, a versioned JSON project representation, and
undo/redo commands for renaming projects and inserting/removing clips. The
**+ Title** control appends a default, three-second text clip to the first
video track; **Undo** and **Redo** operate on that edit. Titles are not
editable or previewed yet, and changes exist only in memory until project
save/open is implemented.

## Run and check

Install a Rust toolchain and run from the repository root:

```sh
cargo run --locked
cargo test --all-targets --locked
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
```

The app needs a graphical desktop session; tests run headlessly. Dependencies
come from crates.io, including the pinned `creamui` 0.3.0 release. No local
CreamUI checkout is needed.

## Code map

- `src/app.rs`: window startup and wiring.
- `src/project/`: saved project, JSON format, and edit history/undo-redo.
- `src/timeline.rs`: tracks, clips, and titles.
- `src/media.rs`: referenced source files (decoding is not implemented).
- `src/ui/`: CreamUI workspace, integrated titlebar, and title/undo controls.
- `src/main.rs`: executable entry point.

See [architecture](ARCHITECTURE.md) for dependencies and planned media
boundaries, [contributing](CONTRIBUTING.md) for the workflow, and
[code style](CODESTYLE.md) for source conventions. Concrete release blockers
are listed in [TODO.md](TODO.md).
