# Ocus

Ocus is an early-stage, lightweight video editor written in Rust. The
[roadmap](ROADMAP.md) targets importing media, editing a timeline, previewing,
and exporting an MP4 in v0.1. **The end-to-end video workflow is not implemented
yet.** Today the repository builds a CreamUI desktop shell with an integrated
titlebar, four timeline tracks, a versioned JSON project representation, and
undo/redo commands for renaming projects and inserting/removing clips. The
**Add title** control appends a three-second text clip to the first video track.
The compact workspace supports selecting and renaming titles, stepping through
frames to inspect title text, splitting, moving, trimming, deleting and ripple
deleting clips, and undo/redo. **Open project** and **Save as** use native
file dialogs instead of requiring typed paths; **Save** reuses the chosen
path. **New** asks before discarding unsaved changes. Saves are atomic and
file operations run in the background. **Import media** accepts multiple
files from the native picker and probes video, audio, and images with FFmpeg
on a worker thread. The media bin can insert a clip on a video or audio track
using the probed duration (still images default to three seconds).
Videos with audio place aligned video and audio clips in a single undoable
action. Audio clips have editable volume and mute values, but those values do
not affect playback or export yet. Previously saved references without probe
metadata can be re-imported.

The workspace shows a time-scaled, paged timeline, a text-only monitor, and
an inspector that scrolls through clip controls. On narrower windows the
media library and inspector share a tabbed sidebar.

This is not a completed v0.1 video editor. There is no frame compositor, audio
playback, thumbnail generation, or MP4 export yet. The pinned CreamUI 0.3.0
does not publish the GPU viewport API. Building now requires FFmpeg development
libraries (available on the Fedora development machine). The UI intentionally
does not offer fake play or export buttons.

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
- `src/media.rs`: source references and FFmpeg probing (decoding is not implemented).
- `src/ui/`: compact CreamUI workspace and background file workflow.
- `src/main.rs`: executable entry point.

See [architecture](ARCHITECTURE.md) for dependencies and planned media
boundaries, [contributing](CONTRIBUTING.md) for the workflow, and
[code style](CODESTYLE.md) for source conventions. Concrete release blockers
are listed in [TODO.md](TODO.md).
