# Roadmap

A lightweight, open source video editor for beginners and intermediate users. Fast to start, easy on RAM, and powerful enough for everyday projects: social clips, tutorials, vlogs, short films and presentations.

This document describes where the project is going and in what order. Plans can change as the project evolves, so treat the lists below as a guide and not as a promise. Anything marked `[ ]` is open for discussion or contribution.

---

## Goals

- **Light and fast.** Quick startup, low memory usage, smooth timeline even on modest hardware.
- **Simple by default.** The common tasks (cut, trim, add text, export) should take a few clicks. Advanced options stay out of the way until needed.
- **Reliable.** Projects never get corrupted, crashes are recoverable, undo always works.
- **Open.** Permissive licensing, documented project format, standard interchange formats.
- **Cross-platform.** Windows, macOS and Linux as first-class targets.

## Non-goals

These are intentionally out of scope, at least until after 1.0:

- Node-based compositing
- 3D or motion graphics tooling
- Multicam editing
- Real-time collaboration
- Cloud rendering or hosted services

They add a lot of complexity and are not needed by the target audience. They can be revisited later if there is clear demand.

---

## Tech stack

| Area | Choice |
| --- | --- |
| Language | Rust |
| Decoding / encoding | FFmpeg through library bindings (`ffmpeg-next` or `rsmpeg`), not the CLI |
| Preview rendering | `wgpu` (frames decoded on CPU/GPU, composited and displayed on GPU) |
| Audio playback | `cpal` for output, `symphonia` for decoding where FFmpeg is not needed |
| Project format | `serde` + versioned JSON |
| Noise reduction | `nnnoiseless` |
| Speech-to-text | `whisper-rs` (local, optional) |
| Stabilization | `vid.stab` via FFmpeg |
| UI toolkit | `creamui` (pinned GitHub revision for GPU preview) |

FFmpeg covers most of the codec and filter work. Where it falls short, the project pulls in focused Rust crates instead of reimplementing things.

## Architecture notes

Some decisions need to be made at the start because they are painful to change later.

- **Project model.** A single, serializable data structure describes the whole project (media references, tracks, clips, effects, settings). The file format carries a schema version from day one so older projects can be migrated.
- **Edit history.** Every change to the project goes through a command layer that supports undo and redo. No direct mutation of the model from the UI.
- **Playback pipeline.** Decoder threads feed a frame cache. The compositor reads from the cache and renders to a `wgpu` surface. Audio runs on its own clock and video syncs to it.
- **Export pipeline.** The exporter walks the same project model as the preview so that what you see is what you get.
- **Media handling.** Source files are referenced, never copied or modified. Thumbnails, waveforms and proxies live in a cache directory and can be regenerated at any time.

## Performance targets

Rough targets to track from the first release onward. They will be measured on mid-range hardware and checked before each release.

- Cold start under 2 seconds
- Idle memory under 300 MB with a typical 1080p project open
- Scrubbing and playback of 1080p footage at the project frame rate
- No UI freezes during import, thumbnail generation or export

---

## v0.1: MVP

The smallest version that can take raw footage and produce a finished video.

**Media**
- [x] Import video, audio and image files
- [x] Media bin with thumbnails

**Timeline**
- [x] At least 2 video tracks and 2 audio tracks
- [x] Split, move, trim and delete clips
- [x] Ripple delete
- [x] Undo and redo

**Preview**
- [x] Play, pause and seek
- [x] Frame-accurate scrubbing

**Audio**
- [x] Volume and mute per clip

**Titles**
- [x] Basic text clips (font, size, color, position)

**Export**
- [x] MP4 output (H.264 video, AAC audio)
- [x] Presets: 1080p, 720p and vertical 9:16
- [x] Progress bar and cancel

**Project**
- [x] Save and open projects
- [x] Keyboard shortcuts for the main actions

**Done when:** a user can import a few clips, cut and arrange them, add a title, adjust volume and export a watchable MP4 without leaving the app.

---

## v0.5 to v0.9: Pre-release

Feature-complete for most everyday editing, with a focus on polish and stability. Work is grouped into rough milestones.

### v0.5: Timeline and motion
- [x] Transitions: fade, dissolve, slide
- [ ] Speed control from 0.25x to 4x
- [ ] Reverse clip and freeze frame
- [x] Transforms: scale, position, rotation, crop, opacity
- [ ] Picture-in-picture
- [x] Snapping and timeline zoom
- [x] Timeline markers

### v0.6: Color and audio
- [x] Brightness, contrast and saturation
- [x] LUT support (.cube)
- [x] Audio waveforms on the timeline and in the Source monitor
- [x] Audio fade in and fade out
- [x] Loudness normalization
- [ ] Noise reduction

### v0.7: Subtitles and keyframes
- [ ] Import SRT files
- [ ] Subtitle styling (font, color, outline, background, position)
- [ ] Burn subtitles into the exported video
- [ ] Basic keyframes for position, scale, rotation and opacity

### v0.8: Performance and safety
- [x] Hardware accelerated encoding and decoding (NVENC, VAAPI, VideoToolbox)
- [ ] Proxy media for smooth 4K editing
- [x] Frame and thumbnail cache
- [x] Autosave
- [x] Crash recovery

### v0.9: Formats and stabilization
- [x] WebM export
- [x] GIF export
- [x] Audio-only export (WAV)
- [ ] Bug fixing, UX cleanup and feature freeze
- [ ] Packaging and installer tests on all platforms

**Done when:** the feature set is frozen, known crashes are fixed and the app handles long projects and 4K footage without trouble.

---

## v1.0.0: First stable release

**Editing**
- [ ] Full keyframe support with easing curves
- [ ] Chroma key (green screen)
- [ ] Simple masks
- [ ] Video stabilization
- [ ] Animated title templates

**Automation**
- [ ] Local automatic subtitles (Whisper)
- [ ] Automatic silence removal

**Export**
- [ ] Export queue
- [x] Custom export presets

**Extensibility**
- [ ] Effects written as WGSL shaders, used as the base for a future plugin system

**Interoperability**
- [ ] Import and export OpenTimelineIO (OTIO) and EDL

**Release quality**
- [ ] Stable installers for Windows, macOS and Linux
- [ ] Internationalization (i18n) with community translations
- [ ] User documentation and a getting started guide
- [ ] Performance benchmarks in CI
- [ ] Stable, documented project file format

**Done when:** the project file format is stable, the targets in the performance section are met, and installers are available for all three platforms.

---

## After 1.0

Ideas under consideration, in no particular order:

- Plugin API for third-party effects and exporters
- Audio ducking and basic mixing tools
- Text-based editing from transcripts
- Scene detection
- Additional export targets (ProRes, AV1)
- Color scopes (waveform, vectorscope, histogram)
- Mobile or web companion apps

---

## Contributing

Contributions are welcome at any stage. If you want to work on something from this roadmap, open an issue first so the approach can be discussed and the work is not duplicated. Items near the top of each milestone are the best candidates for new contributors.

Bug reports, usability feedback and translation help are just as valuable as code.
