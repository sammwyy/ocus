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
  atomic_file.rs    shared same-directory atomic writes for project and preset storage
  settings.rs       editor appearance preferences in the platform config directory
  project/
    mod.rs          saved project and video settings
    export_settings.rs  version-compatible per-project export settings
    edit.rs         project commands and undo/redo history
    format.rs       versioned JSON string format
    recovery.rs     ordered background autosave in the app data directory, with startup restore
  media.rs          source references, probing and thumbnail decoding
  media/preview.rs  background video decoding and frame-accurate seek
  media/cache.rs    bounded persistent thumbnail and storyboard RGBA cache
  media/composite.rs  shared preview/export visual composition
  media/audio.rs    background audio decoding, mixing and the playback clock
  playback.rs       default output device and monitor sound sessions
  export.rs         timeline video, GIF and WAV encoding with cancellation
  export/presets.rs  built-in profiles and global named preset storage
  export/queue.rs    session-local FIFO and per-output status/cancellation
  timeline.rs       tracks, clips, and title data
  ui/
    window.rs       integrated titlebar, editor layout, and clip controls
    controls.rs     palette, vector glyphs, buttons, scrubber, thumbnails
    timeline_view.rs  painted ruler, track lanes, clips, playhead and gestures
    title_markup.rs   opt-in HTML/Markdown title styling
    interaction.rs  drag and drop, shortcuts and the actions they trigger
    edits.rs        multi-clip move, snap, split, delete and paste planning
    state.rs        transient selection, monitors, file dialogs, background jobs
```

`main` calls `app::run_app`; the app creates the initial project and provides
it to `ui`. The UI reads saved state but does not edit it directly. Changes
to the project should be commands handled beside the project model so they
can be undone and redone. Selection, playhead, focus, scroll position, and
panel sizes are transient editor state, not project-file data.
Editor appearance preferences are separate from saved projects: `settings.rs`
loads and validates `settings.toml` from the OS config directory, and the
settings modal applies CreamUI system themes or a manual Light, Dark or
Midnight theme with one of CreamUI's built-in accent presets.
File pickers and confirmation dialogs run from the UI event handler; once a
file is selected, loading, saving, and media probing stay on background jobs.

`project::format` owns schema version 1 and converts between JSON strings
and the in-memory project. It rejects missing or unsupported versions and
round-trips media references and title clips. Project load validates settings,
IDs, clip ranges and reference existence. `project::store` loads and saves on
background jobs, with same-directory temporary files, sync, and rename for
atomic writes. Imported media paths are absolute; loaded relative paths resolve
against the project file's directory, not the working directory. The JSON
format stays path-agnostic and clears stale linked-audio flags before validation.
On the existing open/recovery worker, the store probes present sources and
adopts matching-kind metadata only if the repaired project still validates.
Failed probes and incompatible metadata retain the stored values. Repairs are
not undo steps and normal opens remain clean; the status line reports unlinked
clips alongside missing files. Relink commands repair linked flags atomically
with the source replacement, so undo restores both. Optional probe metadata and
title style/parse flags keep version 1 files readable. Track names, locked and
hidden flags are optional version 1 fields and are omitted when empty/false.
Names are trimmed, limited to 32 Unicode characters and cannot contain control
characters. Track metadata and same-kind reordering use undoable commands.
Locked tracks reject clip edits and removal, including through `ReplaceTracks`;
ripple delete leaves them untouched. Hidden tracks are excluded from both
shared visual composition and audio plans (including linked video audio), so
preview and export agree. Media
probing and bounded first-frame thumbnails use the `ffmpeg-next` library on
workers. `media/preview.rs` decodes and scales frames off the UI thread,
maintaining a small frame buffer during playback and seeking to source frames
after an edit. It also generates three representative thumbnails per video
clip's current source range on workers; `ui::state` keeps a bounded, transient
cache and drops obsolete entries when clips change. Thumbnails and complete
storyboards also persist in the platform `Ocus` cache directory, under
`thumbnails/` and `frames/`, with a shared 512 MiB LRU limit. Source metadata
invalidates stale entries; corrupt or unavailable entries are decoded again.
`media/audio.rs` also
extracts bounded waveform peaks on a worker. The timeline and Source monitor
share the project's transient, per-source peak cache. The UI transfers decoded
frames to textures on CreamUI's own GPU device and presents them through its
viewport. `media/composite.rs` decodes active video and still-image clips with
bounded frame storage and alpha-composites video tracks in timeline order:
V1 is the base and lower rows appear above it. It draws styled titles in their
own track's Z order with the same markup parser and font shaping in preview
and export; gaps show black. `export.rs` encodes MP4, Matroska or WebM using
the configured compatible H.264, HEVC or VP9 codec; it mixes sound from audio
tracks and linked video sound through the bounded stereo queue used by preview
playback, and encodes AAC or Opus. GIF uses the same compositor with
per-frame palettes and skips the sound mix; WAV only renders the sound mix.
Audio-only timelines use black video when exported to a video format.
Output resolution, bitrate or constant-quality CRF, encoder speed and frame rate
are independent of the edit
timeline; the visual composition is letterboxed when output aspect differs
from the project, while audio keeps the source timeline clock. `project`
keeps the last export settings and destination as an optional version 1 field,
changed through a command, and auto-saves an already-saved project after
successful export. `export::presets` validates and atomically writes named
presets to the OS config directory independently of project files.
The UI queues project snapshots with per-job settings and destinations.
Only one export worker runs at a time, and the next job starts after that
worker and any resulting project save have finished.
The UI has independent source and program monitors, each with its own
decode stream, position and GPU frame slot; only one plays at a time. It
supports video play/pause, frame stepping, monitor and timeline scrubbing,
timeline zoom, source filtering, filmstrip clips and title text display.
Clip selection may hold several clips; dragging, pasting, nudging, splitting
and deleting a selection is planned in `ui/edits.rs` against the current
project and applied as one `ReplaceTracks` command, so it undoes in one step
and the project's own validation still rejects overlaps. Drag previews and
the rubber band are transient UI state. The Program monitor shows the composed
frame; the selected title has a draggable
frame and resize handle, with a transient draft committed as one `SetTitle`
command on release. `ui/title_markup.rs` parses only the supported HTML and
Markdown syntax when their respective switches are enabled. The preview
composites simultaneous video layers; title edits, split/move/trim/delete,
audio clip settings, track creation/removal and ripple delete use project
commands. Ripple delete merges the original selected time ranges and closes
them across every unlocked track; clips spanning a removed range split with distinct IDs,
preserving both source ranges in one undoable edit. A video with sound is
inserted as one clip with `linked_audio`, an optional field that keeps version 1
files readable (absent means false), so any edit of the clip also edits its
sound; validation only allows it on video
tracks for video sources with audio. Detaching turns it into an aligned clip on
an audio track. Context menus and the drag label are UI overlays drawn as
absolutely positioned layers above the workspace. During playback
`media/audio.rs` decodes each audible clip (linked audio on video tracks and
any clip on audio tracks, honoring volume and mute), resamples and mixes it to
48 kHz stereo on a worker, and `playback.rs` streams it through rodio to the
default output device; video pacing follows the consumed-sample clock so
picture and sound stay aligned, skipping late video frames instead of delaying
the timeline when composition is slower than playback. Source preview decodes
one clip; the Program monitor and export share the layered composition rules.
Video clips may store an optional fade, dissolve, or slide into their start
(`transition`) and out of their end (`transition_out`).
The shared compositor clamps transition length to the clip duration and uses
the adjacent previous clip's continued picture (holding at source EOF), or
the lower tracks, as the outgoing picture. Fade dips through black only at an
adjacent cut; otherwise the incoming clip fades in over the lower tracks.
Transitions are edited through
`SetTransition`; splitting leaves the in-transition only on the left piece.
`SetTransitionOut` edits the out-transition, which stays on the right piece
when split. Out-transitions reveal the lower tracks (or black): fade and
dissolve ramp down the clip's contribution, and slide moves it toward its
configured edge. Slides enter from or exit toward left, right, up, or down;
the default remains right for old projects. Each transition is clamped to
the clip length; if their combined lengths exceed it, both are capped at
half the clip length, rounded down, leaving any middle frame unaffected.
Stored durations are not changed by this render-time clamp.
The optional fields remain compatible with version 1 projects. The Inspector
offers independent In and Out choices, quarter-second duration steps, and
slide edges; the timeline marks their effective ranges with mirrored ramps
at the clip's start and end.

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
