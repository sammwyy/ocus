# TODO — v0.1 MVP

This checklist breaks down the [v0.1 milestone](ROADMAP.md#v01-mvp) into
implementation work. The four tracks, JSON string conversion, and initial
clip commands do not complete the corresponding editor workflows.

## Project and editing foundation

- [ ] Extend existing settings/ID/range/compatibility and probed duration
  checks with linked AV clips and media path migration/repair.
- [x] Add project file open/save with atomic writes, external media paths,
  useful errors, and round-trip tests; retain explicit schema-version handling.
- [x] Keep a video's own sound linked inside its clip so edits apply to both;
  detach it to an audio track (creating one if needed) or link a matching pair
  back. Add and delete video and audio tracks.
- [ ] Name, reorder, lock and hide tracks; the project format does not store
  track names or flags yet.
- [ ] Extend ripple delete across overlapping clips that span the removed range;
  existing implementation rejects these instead of silently dropping footage.
- [ ] Make all video and audio tracks selectable as destinations from the UI;
  current import appends to the first compatible track.

## Media, timeline, and preview

- [x] Probe and import video, audio, and image files without modifying the
  originals; report unsupported or missing files.
- [x] Generate regenerable thumbnails and show them in a responsive media
  bin without blocking the UI.
- [x] Render sampled filmstrips for video clips and let clicks seek and select
  clips; add search and source-type filters to the media bin.
- [x] Drag clips (one or a multi-selection) along and across tracks with
  snapping, drag sources from the bin onto a track, and drop files on the
  window or a track; each drop is one undoable command.
- [ ] Trim clips by dragging their edges; trimming still uses the inspector.
- [ ] Generate filmstrips lazily for clips beyond the bounded first 64
  cached source ranges in large projects.
- [x] Decode video on a worker with a bounded frame buffer; support
  play/pause, frame stepping and frame-accurate timeline ruler seeks.
- [x] Present video and still-image frames through CreamUI's GPU viewport.
- [ ] Composite overlapping video layers and titles consistently between
  preview and export; support transition effects.

## Audio, titles, and export

- [ ] Decode and play audio with an audio master clock; synchronize video
  playback and seeking to it.
- [ ] Add per-clip volume and mute controls that affect both preview and
  export, with tests.
- [ ] Draw audio waveforms in timeline clips and the source monitor; both
  currently show only the clip's volume level.
- [x] Extend editable title text with font, size, color, position, style and
  opt-in markup controls; drag to move or resize titles in the video preview.
- [ ] Include title overlays in exported video; export currently rejects them.
- [ ] Export the edited project to MP4 with H.264 video and AAC audio.
- [x] Encode silent H.264 MP4 from contiguous video and still-image clips
  on one track; reject unsupported sequences instead of dropping them.
- [ ] Add and test export presets for 1080p, 720p, and vertical 9:16.
- [x] Run export off the UI thread with progress, cancellation, and cleanup
  of incomplete outputs.

## End-to-end usability

- [ ] Wire import, playback, editing, project open/save, and export into the
  window with clear empty, loading, and error states.
- [x] Add keyboard shortcuts for the main actions without stealing text
  input (they are inactive while a text field has focus).
- [ ] Exercise the roadmap's complete workflow with mixed media, a title,
  volume changes, project reload, and a watchable MP4 export.

## External prerequisite

- [x] Pin CreamUI to a published GitHub commit with GPU viewport and
  background stream APIs. Do not add a sibling path dependency.
- [x] Integrate FFmpeg library bindings for background probing, thumbnails,
  and limited H.264 export; see [DEVELOPMENT.md](DEVELOPMENT.md). Audio
  processing and full A/V encoding remain open.
