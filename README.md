# Ocus

Projects open even when source files have moved. Missing files are marked in the
media bin; choose **Relink…** to search a folder by exact filename, or right-click
a missing item and choose **Relink file…** to locate a renamed file. Locating one
file also reconnects missing files with matching names in the same folder. Each
relinked file can be undone independently. Relink missing timeline media before
exporting, then save the project to keep its updated source paths.

Ocus is an early-stage, lightweight video editor written in Rust. The
[roadmap](ROADMAP.md) tracks its intended milestones. The CreamUI editor
supports importing media, editing and previewing a multitrack timeline,
saving projects and exporting video. A full manual end-to-end workflow check
is still pending. The editor has an integrated titlebar, four initial timeline
tracks, a versioned JSON project format and undo/redo for project edits. The
timeline toolbar's **Title** tool adds a text clip initially three seconds long,
whose duration can be changed in the inspector; over footage
it places the title on a free video track at the playhead. Selecting a title
shows a frame around it in the Program monitor: drag inside to move it or drag
its bottom-right handle to resize it. The inspector edits its multiline text,
font family, size, color (picker or hex), bold, italic and outline. Independent
HTML and Markdown switches parse `<b>` / `<bold>`, `<i>` / `<italic>`,
`<color=red>` / `<color=#ff0000>`, `**bold**` and `_italic_` in the preview;
other HTML remains literal text. These edits can be undone and are saved with
the project. The workspace also supports splitting, moving, trimming, deleting
and ripple-deleting clips. Ripple delete closes the selected clips' time ranges
across all tracks; a clip spanning a removed range keeps its surviving source
frames as two clips, and the whole edit is one undo step. **Open** and
**Save as…** use native file dialogs instead of requiring typed paths;
**Save** reuses the chosen path. **New** asks before discarding unsaved
changes. Saves are atomic and file operations run in the background.
**Import** accepts multiple
files from the native picker and probes video, audio, and images with FFmpeg
on a worker thread. Video and still-image sources get small, regenerable
first-frame thumbnails. The media bin offers search, a segmented
All/video/audio/still filter with per-type counts, grid and list views, and files can also be dropped onto it to import
them. Each source has a one-click insert action that adds a clip on a video or
audio track using the probed duration (still images default to three seconds).
A video with sound becomes one clip whose audio is linked to the picture:
the timeline draws it as an attached audio band under the video, and every
split, move, trim, delete or ripple applies to both, so cutting a clip in three
and ripple-deleting the middle keeps picture and sound 1:1. **Detach audio**
(inspector or right-click) moves that sound to an audio track that is free for
the same range, creating a new one if needed, and **Link audio** folds a
matching pair back together. Clip volume and mute are editable, saved, and
honored during preview playback and export. Previously saved references without probe
metadata can be re-imported.

The workspace has two GPU monitors: **Source** shows the selected timeline
clip's trimmed range, or the whole file selected in the media bin, and
**Program** shows the sequence at the playhead. Each has its own scrub bar,
Play/Pause, frame stepping and jump-to-start/end; starting one pauses the
other. **Match frame** moves the program playhead to the source monitor's
frame, and **Insert** adds the selected bin file. The timeline is one painted
surface with track headers, a zoomable ruler (1 s to 10 min per view, with zoom buttons, a slider and **Fit**), clip
filmstrips sampled from their actual trimmed source range, audio volume
levels and a playhead across every track.

Tracks can be added from the timeline's **Tracks** button, the **Add
track** button under the track headers or any right-click menu, and deleted
from a header's hover button or its menu (one video and one audio track always
remain). Right-click opens a context menu on clips (cut, copy, paste,
duplicate, split, detach/link audio, mute, delete, ripple delete), on empty
track space (paste here, add title here, move playhead, track actions), on
track headers and on media bin items (insert at end or at the playhead, open in
the source monitor, remove an unused source); Esc or clicking outside closes it.

Editing is direct: drag clips along a track or onto another track of the same
kind (edges snap to other clips, the playhead and the start), drag sources from
the media bin onto a track, or drop files from the desktop anywhere on the
window to import them (over a track they are also placed there). Ctrl+click
and Shift+click add to the selection and dragging over empty track space draws
a selection rectangle; a multi-selection moves, copies and deletes together.
Keyboard shortcuts work whenever a text field is not focused: Ctrl+Z /
Ctrl+Shift+Z (or Ctrl+Y) undo and redo, Delete or Backspace removes the
selection (Shift ripple-deletes), Ctrl+C/X/V/D copy, cut, paste at the playhead
and duplicate, S or Ctrl+K splits at the playhead, Ctrl+A selects all, Esc
clears the selection or cancels a drag, Space or K plays and pauses, the
arrows step a frame (Shift: one second, Alt: nudge the selection), Home/End
jump, +/− zoom, T adds a title, and Ctrl+N/O/S/Shift+S/I/E run the file
actions. Every edit is one undoable step. On the timeline, vertical scrolling
only moves through the tracks (when they overflow) and horizontal scrolling
only moves through time, so two-finger touchpad gestures follow their axis;
with a plain mouse wheel, Shift+wheel scrolls time. Ctrl+wheel (or a touchpad
pinch reported as Ctrl+wheel) zooms around the pointer. Selecting several
clips turns the inspector into a selection view with a summary, shared
actions and the list of selected clips. Filmstrips regenerate in the
background and do not change the project file. The timeline can expand to give
tracks more room. Video decoding happens on workers and frames are presented
on the same GPU device as CreamUI. On a CPU-only window the monitors instead
explain why video cannot be displayed. On narrower windows the media bin and
inspector share a tabbed sidebar.

The titlebar gear opens an in-editor settings dialog with Appearance and About
sections. About shows the package version, build commit and author, with links
to the source, issue tracker and releases in the default browser. Appearance can
follow the system through CreamUI or use Light, Dark, or Midnight. Custom themes offer
CreamUI's built-in Blue, Cyan, Green, Pink, Purple, and Orange accent presets;
Orange is the initial custom accent. Preferences are saved independently of
project files in `Ocus/settings.toml` under the platform configuration
directory (XDG config on Linux, AppData on Windows).

**Export** (disabled while the timeline is empty) opens a settings dialog with
built-in presets for the original project, 1080p, 720p, vertical 9:16, square,
4K, WebM and HEVC. You can set even output dimensions (up to 8192 × 8192),
frame rate (including rational rates such as 30000/1001), video and audio
bitrates or constant-quality CRF, encoder speed, container (MP4, MKV or WebM)
and compatible video/audio codecs
(H.264, HEVC or VP9; AAC or Opus). Audio mixing remains at 48 kHz stereo and
video uses 8-bit YUV420p; the exporter requires the selected encoders to be
present in FFmpeg. When the output aspect ratio differs from the
project, it fits the entire composition inside the output with black bars
instead of stretching it. Presets can be saved with a custom name in
`Ocus/export-presets.json` under the platform config directory and used in
other projects. Successful exports remember the settings and destination in
the project file; a saved project is automatically saved after exporting.
**Export again** generates a new numbered file using the last settings
without reopening the dialog; **Options…** reopens it. Outputs are never
silently overwritten. An unsaved project still needs to be saved to retain
its export settings between sessions.

The default MP4 export encodes H.264
video composited from every video track (V1 is the base; V2 and lower rows are
drawn above it), transparent
still images and all title overlays, plus AAC stereo audio from audio tracks and
linked clip sound. Titles honor font, size, position, color, transparency,
bold/italic, outline and enabled HTML/Markdown styling. Cuts, trims, gaps
(black video/silent audio), per-clip volume and mute are honored. Audio-only
timelines export with black video. Export runs in the background, shows
progress, can be cancelled, and publishes a completed file atomically.
Available H.264 and AAC encoders are required in FFmpeg for the default MP4.

The Program monitor uses the same visual compositor as export, including
overlapping video, transparent images and titles. The Source monitor shows a
single source. Preview sound plays through the default output device, with
picture timed to the audio clock. New titles are placed above existing footage
and create a video track if needed; V1 is marked BASE and other video tracks
ABOVE in the timeline. The manual end-to-end workflow check remains outstanding.
Building requires FFmpeg development libraries (available
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
- `src/media.rs`, `src/media/preview.rs`, `src/media/composite.rs` and
  `src/media/audio.rs`: sources, decoding, and shared preview/export composition.
- `src/playback.rs`: default audio device and playback sessions.
- `src/export.rs`: H.264/AAC MP4 encoding and cancellation.
- `src/ui/`: compact CreamUI workspace and background file workflow.
- `src/main.rs`: executable entry point.

See [architecture](ARCHITECTURE.md) for dependencies and planned media
boundaries, [contributing](CONTRIBUTING.md) for the workflow, and
[code style](CODESTYLE.md) for source conventions. Concrete release blockers
are listed in [TODO.md](TODO.md).
