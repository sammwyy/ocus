# Ocus architecture

Ocus is a Rust desktop application, currently one Cargo package. Its source is
grouped by the parts of a video editor rather than by generic service layers.
Keep a feature in this package until real implementations and dependencies
justify a separate crate. The v0.1 scope is in [ROADMAP.md](ROADMAP.md).

## What exists today

```text
src/
  main.rs           executable entry point
  app.rs            starts CreamUI and connects the project to the window
  project/
    mod.rs          saved project and output settings
    edit.rs         project commands and undo/redo history
    format.rs       versioned JSON string format
  media.rs          source references, probing and thumbnail decoding
  media/preview.rs  background video decoding and frame-accurate seek
  export.rs         silent H.264 MP4 encoding with cancellation
  timeline.rs       tracks, clips, and title data
  ui/
    window.rs       integrated titlebar, editor layout, and clip controls
    controls.rs     palette, vector glyphs, buttons, scrubber, thumbnails
    timeline_view.rs  painted ruler, track lanes, clips and playhead
    state.rs        transient selection, monitors, file dialogs, background jobs
```

`main` calls `app::run_app`; the app creates the initial project and provides
it to `ui`. The UI reads saved state but does not edit it directly. Changes
to the project should be commands handled beside the project model so they
can be undone and redone. Selection, playhead, focus, scroll position, and
panel sizes are transient editor state, not project-file data.
File pickers and confirmation dialogs run from the UI event handler; once a
file is selected, loading, saving, and media probing stay on background jobs.

`project::format` owns schema version 1 and converts between JSON strings
and the in-memory project. It rejects missing or unsupported versions and
round-trips media references and title clips. Project load validates settings,
IDs, clip ranges and reference existence. `project::store` loads and saves on
background jobs, with same-directory temporary files, sync, and rename for
atomic writes. Imported media paths are absolute. Format migrations do not
exist yet; optional probe metadata keeps version 1 files readable. Media
probing and bounded first-frame thumbnails use the `ffmpeg-next` library on
workers. `media/preview.rs` decodes and scales frames off the UI thread,
maintaining a small frame buffer during playback and seeking to source frames
after an edit. It also generates three representative thumbnails per video
clip's current source range on workers; `ui::state` keeps a bounded, transient
cache and drops obsolete entries when clips change. The UI transfers decoded
frames to textures on CreamUI's own GPU device and presents them through its
viewport. `export.rs` decodes
video and still-image clips sequentially with bounded frame storage and
encodes a silent H.264 MP4 from one continuous video track. It refuses audio,
titles, gaps, and multiple active tracks rather than dropping them silently.
The UI has independent source and program monitors, each with its own
decode stream, position and GPU frame slot; only one plays at a time. It
supports video play/pause, frame stepping, monitor and timeline scrubbing,
timeline zoom, source filtering, filmstrip clips and title text display. It does
not composite simultaneous video/title layers; title edits, split/move/trim/delete,
audio clip settings, and ripple delete use project commands. Video sources
with audio add aligned clips to both tracks; later edits to those clips are
not linked yet. Audio, the preview compositor, title rendering, and full
A/V export remain unimplemented.

## Where new work belongs

| Work | Place it when implemented | Boundary |
| --- | --- | --- |
| File open/save and format migrations | `project/` | Keep writes atomic and source files untouched |
| Clip edits, ripple delete, undo/redo | `project/` and `timeline/` | UI sends edits; it never mutates saved fields directly |
| Media probing, decoding, thumbnails, caches | `media/` | Background work; caches are bounded and regenerable |
| Audio clock, play/pause/seek | `playback/` | Audio timing drives video, not UI redraws |
| `wgpu` preview compositor | `preview/` | Present a same-device texture through CreamUI |
| MP4 encoding and export jobs | `export.rs` (split as needed) | Read the same timeline semantics as preview; support cancel |
| Panels, controls, timeline gestures | `ui/` | No decoding or encoding on the UI thread |
| Window lifecycle and subsystem wiring | `app.rs` (split only if needed) | No clip-edit or codec logic in startup |

The directories in this table are **placement guidance**, not empty modules to
create in advance. Keep expensive work and failure reporting outside the UI
event loop. Use frame counts for timeline positions and durations; audio samples
and UI pixels are different units. Imported media is referenced, never copied
or modified; thumbnails, waveforms, and proxies can be regenerated.

CreamUI is pinned to an immutable commit from its GitHub repository, not a
sibling path checkout. The GPU viewport and background-stream APIs are
available at that revision. Video decoding and pixel conversion stay on a
worker; the UI uploads frames to the window's existing GPU device.

Test project edits and format round trips without a window; test UI layout
with a headless CreamUI painter. Add codec/device integration tests and measure
startup, memory, and playback when those subsystems exist. A compiling desktop
shell is not proof of platform support or the roadmap's performance targets.
