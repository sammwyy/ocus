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
  media.rs          referenced source paths and kinds
  timeline.rs       tracks, clips, and title data
  ui/
    window.rs       integrated titlebar, editor layout, and clip controls
    state.rs        transient selection, file paths, background jobs
```

`main` calls `app::run_app`; the app creates the initial project and provides
it to `ui`. The UI reads saved state but does not edit it directly. Changes
to the project should be commands handled beside the project model so they
can be undone and redone. Selection, playhead, focus, scroll position, and
panel sizes are transient editor state, not project-file data.

`project::format` owns schema version 1 and converts between JSON strings
and the in-memory project. It rejects missing or unsupported versions and
round-trips media references and title clips. Project load validates settings,
IDs, clip ranges and reference existence. `project::store` loads and saves on
background jobs, with same-directory temporary files, sync, and rename for
atomic writes. Imported media paths are absolute. Format migration and codec
probing do not exist. `media.rs` defines source references, **not** a decoder.
The UI supports frame stepping and title text display, not rendered video;
title edits, split/move/trim/delete, and ripple delete use project commands.
Audio, frame compositor, thumbnails and MP4 export remain unimplemented.

## Where new work belongs

| Work | Place it when implemented | Boundary |
| --- | --- | --- |
| File open/save and format migrations | `project/` | Keep writes atomic and source files untouched |
| Clip edits, ripple delete, undo/redo | `project/` and `timeline/` | UI sends edits; it never mutates saved fields directly |
| Media probing, decoding, thumbnails, caches | `media/` | Background work; caches are bounded and regenerable |
| Audio clock, play/pause/seek | `playback/` | Audio timing drives video, not UI redraws |
| `wgpu` preview compositor | `preview/` | Present a same-device texture through CreamUI |
| MP4 encoding and export jobs | `export/` | Read the same timeline semantics as preview; support cancel |
| Panels, controls, timeline gestures | `ui/` | No decoding or encoding on the UI thread |
| Window lifecycle and subsystem wiring | `app.rs` (split only if needed) | No clip-edit or codec logic in startup |

The directories in this table are **placement guidance**, not empty modules to
create in advance. Keep expensive work and failure reporting outside the UI
event loop. Use frame counts for timeline positions and durations; audio samples
and UI pixels are different units. Imported media is referenced, never copied
or modified; thumbnails, waveforms, and proxies can be regenerated.

CreamUI is a pinned crates.io dependency, not a sibling path checkout. Its
published 0.3.0 release supports the shell but does not yet include the GPU
viewport and split-pane APIs in the neighboring repository; see
[TODO.md](TODO.md). Do not build a preview around avoidable per-frame CPU
uploads just to use an unpublished widget API.

Test project edits and format round trips without a window; test UI layout
with a headless CreamUI painter. Add codec/device integration tests and measure
startup, memory, and playback when those subsystems exist. A compiling desktop
shell is not proof of platform support or the roadmap's performance targets.
