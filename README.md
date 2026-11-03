# Ocus

Ocus is an early-stage, lightweight video editor written in Rust. The
[roadmap](ROADMAP.md) targets importing media, editing a timeline, previewing,
and exporting an MP4 in v0.1. **The end-to-end video workflow is not implemented
yet.** Today the repository builds a CreamUI desktop shell with an integrated
titlebar, four timeline tracks, a versioned JSON project representation, and
undo/redo commands for renaming projects and inserting/removing clips. The
timeline toolbar's **Title** tool appends a three-second text clip to the first
video track. The workspace supports selecting and renaming titles, stepping through
frames to inspect title text, splitting, moving, trimming, deleting and ripple
deleting clips, and undo/redo. **Open** and **Save as…** use native
file dialogs instead of requiring typed paths; **Save** reuses the chosen
path. **New** asks before discarding unsaved changes. Saves are atomic and
file operations run in the background. **Import** accepts multiple
files from the native picker and probes video, audio, and images with FFmpeg
on a worker thread. Video and still-image sources get small, regenerable
first-frame thumbnails. The media bin offers search, video/audio/still
filters, grid and list views, and files can also be dropped onto it to import
them. Each source has a one-click insert action that adds a clip on a video or
audio track using the probed duration (still images default to three seconds).
Videos with audio place aligned video and audio clips in a single undoable
action. Audio clips have editable volume and mute values, but those values do
not affect playback or export yet. Previously saved references without probe
metadata can be re-imported.

The workspace has two GPU monitors: **Source** shows the selected timeline
clip's trimmed range, or the whole file selected in the media bin, and
**Program** shows the sequence at the playhead. Each has its own scrub bar,
Play/Pause, frame stepping and jump-to-start/end; starting one pauses the
other. **Match frame** moves the program playhead to the source monitor's
frame, and **Insert** adds the selected bin file. The timeline is one painted
surface with track headers, a zoomable ruler (2 s to 3 min per view), clip
filmstrips sampled from their actual trimmed source range, audio volume
levels and a playhead across every track. Pressing a clip selects it, pressing
empty track space clears the selection, and dragging scrubs; the mouse wheel
scrolls through time and Ctrl+wheel zooms. Filmstrips regenerate in the
background and do not change the project file. The timeline can expand to give
tracks more room. Video decoding happens on workers and frames are presented
on the same GPU device as CreamUI. On a CPU-only window the monitors instead
explain why video cannot be displayed. On narrower windows the media bin and
inspector share a tabbed sidebar.

**Export silent MP4** encodes H.264 from a continuous sequence of video-only
clips and still images on one video track. Cuts and trims are honored; export
runs in the background, shows progress, can be cancelled, and publishes a
completed file atomically. The button reports unsupported sequences rather
than silently omitting content. Audio, titles, gaps and multiple active tracks
are **not** exported, and output has no AAC audio stream.

This is not a completed v0.1 video editor. Preview renders a single active
video layer or still image without audio; it does not yet composite overlapping
tracks or titles onto video. Audio playback and audio-synchronized timing are
not implemented. Building requires FFmpeg development libraries (available
on the Fedora development machine) and a GPU-capable CreamUI window.

## Run and check

See [DEVELOPMENT.md](DEVELOPMENT.md) for per-OS installation and building
instructions. With the prerequisites installed, run from the repository root:

```sh
cargo run --locked
cargo test --all-targets --locked
cargo fmt --all --check
cargo clippy --all-targets --locked -- -D warnings
```

The app needs a graphical desktop session; tests run headlessly. CreamUI is
pinned to a commit on its GitHub repository, while other dependencies come
from crates.io. No local CreamUI checkout is needed.

## Code map

- `src/app.rs`: window startup and wiring.
- `src/project/`: saved project, JSON format, and edit history/undo-redo.
- `src/timeline.rs`: tracks, clips, and titles.
- `src/media.rs` and `src/media/preview.rs`: source references, FFmpeg probing,
  thumbnails and background video decoding.
- `src/export.rs`: limited H.264 MP4 encoding and cancellation.
- `src/ui/`: compact CreamUI workspace and background file workflow.
- `src/main.rs`: executable entry point.

See [architecture](ARCHITECTURE.md) for dependencies and planned media
boundaries, [contributing](CONTRIBUTING.md) for the workflow, and
[code style](CODESTYLE.md) for source conventions. Concrete release blockers
are listed in [TODO.md](TODO.md).
