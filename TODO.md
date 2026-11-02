# TODO — v0.1 MVP

This checklist breaks down the [v0.1 milestone](ROADMAP.md#v01-mvp) into
implementation work. The four tracks, JSON string conversion, and initial
clip commands do not complete the corresponding editor workflows.

## Project and editing foundation

- [ ] Validate project settings, clip ranges, track compatibility, media
  references, and IDs when loading or editing a project.
- [ ] Add project file open/save with atomic writes, external media paths,
  useful errors, and round-trip tests; retain explicit schema-version handling.
- [ ] Extend the initial insert/remove clip commands with importing media,
  splitting, moving, trimming, and ripple deletion; test undo and redo for
  every operation.
- [ ] Add ripple delete that shifts later clips correctly across affected
  tracks, with command/undo tests.
- [ ] Make at least two video and two audio tracks usable for editing; the
  initial empty tracks alone are not sufficient.

## Media, timeline, and preview

- [ ] Probe and import video, audio, and image files without modifying the
  originals; report unsupported or missing files.
- [ ] Generate regenerable thumbnails and show them in a responsive media
  bin without blocking the UI.
- [ ] Build timeline clip rendering and interactions for selection, split,
  move, trim, delete, and ripple delete; connect gestures to commands.
- [ ] Implement video decoding, a bounded frame cache, and play/pause/seek
  with frame-accurate scrubbing.
- [ ] Connect the video compositor to CreamUI's GPU presentation path and
  keep the UI responsive during decoding and seeks.

## Audio, titles, and export

- [ ] Decode and play audio with an audio master clock; synchronize video
  playback and seeking to it.
- [ ] Add per-clip volume and mute controls that affect both preview and
  export, with tests.
- [ ] Make default text clips editable (font, size, color, and position) and
  render them consistently in preview and export.
- [ ] Export the edited project to MP4 with H.264 video and AAC audio.
- [ ] Add and test export presets for 1080p, 720p, and vertical 9:16.
- [ ] Run export off the UI thread with progress, cancellation, and cleanup
  of incomplete outputs.

## End-to-end usability

- [ ] Wire import, playback, editing, project open/save, and export into the
  window with clear empty, loading, and error states.
- [ ] Add keyboard shortcuts for the main actions without stealing text
  input; test scope and focus behavior.
- [ ] Exercise the roadmap's complete workflow with mixed media, a title,
  volume changes, project reload, and a watchable MP4 export.

## External prerequisite

- [ ] Publish the CreamUI GPU viewport, split-pane, and related input APIs
  to crates.io, then upgrade Ocus's pinned `creamui = "=0.3.0"` dependency
  before using them. Do not add a sibling path dependency.
