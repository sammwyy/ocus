# TODO — v0.1 MVP

This checklist breaks down the [v0.1 milestone](ROADMAP.md#v01-mvp) into
implementation work. The four tracks, JSON string conversion, and initial
clip commands do not complete the corresponding editor workflows.

## Project and editing foundation

- [ ] Extend existing settings/ID/range/compatibility and probed duration
  checks with linked AV clips and media path migration/repair.
- [x] Add project file open/save with atomic writes, external media paths,
  useful errors, and round-trip tests; retain explicit schema-version handling.
- [ ] Extend existing insert/split/move/trim/delete and probed media
  import commands with proper drag/drop gestures and linked audio.
- [ ] Extend ripple delete across overlapping clips that span the removed range;
  existing implementation rejects these instead of silently dropping footage.
- [ ] Make all video and audio tracks selectable as destinations from the UI;
  current import appends to the first compatible track.

## Media, timeline, and preview

- [x] Probe and import video, audio, and image files without modifying the
  originals; report unsupported or missing files.
- [x] Generate regenerable thumbnails and show them in a responsive media
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
- [ ] Extend editable title text with font, size, color, and position controls;
  render titles consistently in the video preview and export.
- [ ] Export the edited project to MP4 with H.264 video and AAC audio.
- [x] Encode silent H.264 MP4 from contiguous video and still-image clips
  on one track; reject unsupported sequences instead of dropping them.
- [ ] Add and test export presets for 1080p, 720p, and vertical 9:16.
- [x] Run export off the UI thread with progress, cancellation, and cleanup
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
- [x] Integrate FFmpeg library bindings for background probing, thumbnails,
  and limited H.264 export; see [DEVELOPMENT.md](DEVELOPMENT.md). Playback
  decoding, audio processing, and full A/V encoding remain open.
