# Ocus

Ocus is an early-stage, lightweight video editor written in Rust. The
[roadmap](ROADMAP.md) targets importing media, editing a timeline, previewing,
and exporting an MP4 in v0.1. **The end-to-end video workflow is not implemented
yet.** Today the repository builds a CreamUI desktop shell with an integrated
titlebar, four timeline tracks, a versioned JSON project representation, and
undo/redo commands for renaming projects and inserting/removing clips. The
**+ Title** control appends a three-second text clip to the first video track.
The compact workspace supports selecting and renaming titles, stepping through
frames to inspect title text, splitting, moving, trimming, deleting and ripple
deleting clips, and undo/redo. Enter a `.ocus` path and use **Save** or **Open**
to persist/reload the project; saves are atomic and file operations run in the
background. **Add reference** accepts an existing video, audio or image path,
but only records its path and extension: sources are **not** decoded or probed.

This is not a completed v0.1 video editor. There is no frame compositor, audio
playback, thumbnail generation, or MP4 export yet. The pinned CreamUI 0.3.0
does not publish the GPU viewport API, and building FFmpeg bindings requires
FFmpeg development libraries (now available on the Fedora development machine).
The UI intentionally does not offer fake play or export buttons.

## Run and check

See [DEVELOPMENT.md](DEVELOPMENT.md) for per-OS installation and building
instructions. With the prerequisites installed, run from the repository root:

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
- `src/ui/`: compact CreamUI workspace and background file workflow.
- `src/main.rs`: executable entry point.

See [architecture](ARCHITECTURE.md) for dependencies and planned media
boundaries, [contributing](CONTRIBUTING.md) for the workflow, and
[code style](CODESTYLE.md) for source conventions. Concrete release blockers
are listed in [TODO.md](TODO.md).
