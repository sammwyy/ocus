use super::Project;
use super::export_settings::LastExport;
use crate::media::{Media, MediaKind};
use crate::timeline::{
    Clip, ClipContent, ColorAdjust, Marker, Playback, Title, Track, TrackKind, Transform,
    Transition,
};
use std::collections::HashSet;

pub(crate) fn split_tracks(
    tracks: &[Track],
    targets: &[(u64, u64)],
    at_frame: u64,
    first_new_id: u64,
) -> Option<Vec<Track>> {
    let mut tracks = tracks.to_vec();
    let mut next_id = first_new_id;
    let mut changed = false;
    for &(track_id, clip_id) in targets {
        let track = tracks.iter_mut().find(|track| track.id == track_id)?;
        if track.locked {
            return None;
        }
        let Some(index) = track.clips.iter().position(|clip| clip.id == clip_id) else {
            continue;
        };
        let clip = &mut track.clips[index];
        clip.start_frame.checked_add(clip.duration_frames)?;
        let Some(offset) = at_frame.checked_sub(clip.start_frame) else {
            continue;
        };
        if offset == 0 || offset >= clip.duration_frames {
            continue;
        }
        let right = Clip {
            keyframes: clip.keyframes_for_range(i128::from(offset)),
            transition: None,
            id: next_id,
            start_frame: at_frame,
            source_start_frame: clip
                .source_start_for_range(offset, clip.duration_frames - offset)?,
            duration_frames: clip.duration_frames - offset,
            fade_in_frames: 0,
            fade_out_frames: clip.fade_out_frames.min(clip.duration_frames - offset),
            ..clip.clone()
        };
        next_id = next_id.checked_add(1)?;
        clip.source_start_frame = clip.source_start_for_range(0, offset)?;
        clip.duration_frames = offset;
        clip.transition_out = None;
        clip.fade_out_frames = 0;
        clip.clamp_fades();
        track.clips.insert(index + 1, right);
        changed = true;
    }
    changed.then_some(tracks)
}

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    SetClipKeyframes {
        track_id: u64,
        clip_id: u64,
        keyframes: Vec<crate::timeline::Keyframe>,
    },
    SetPlayback {
        track_id: u64,
        clip_id: u64,
        playback: Playback,
    },
    SetClipTransform {
        track_id: u64,
        clip_id: u64,
        transform: Transform,
    },
    SetClipLut {
        track_id: u64,
        clip_id: u64,
        path: Option<std::path::PathBuf>,
    },
    SetTransitionOut {
        track_id: u64,
        clip_id: u64,
        transition: Option<Transition>,
    },
    SetTransition {
        track_id: u64,
        clip_id: u64,
        transition: Option<Transition>,
    },
    RenameProject(String),
    RenameTrack {
        track_id: u64,
        name: String,
    },
    SetTrackLocked {
        track_id: u64,
        locked: bool,
    },
    SetTrackHidden {
        track_id: u64,
        hidden: bool,
    },
    MoveTrack {
        track_id: u64,
        down: bool,
    },
    SetLastExport(Option<LastExport>),
    InsertClip {
        track_id: u64,
        clip: Clip,
    },
    RemoveClip {
        track_id: u64,
        clip_id: u64,
    },
    ImportMedia(Media),
    ReprobeMedia(Media),
    RelinkMedia(Media),
    RestoreMedia {
        media: Media,
        linked_audio: Vec<(u64, bool)>,
    },
    RemoveMedia(u64),
    Split {
        track_id: u64,
        clip_id: u64,
        at_frame: u64,
        new_id: u64,
    },
    Move {
        track_id: u64,
        target_track_id: u64,
        clip_id: u64,
        to_frame: u64,
    },
    Trim {
        track_id: u64,
        clip_id: u64,
        start_frame: u64,
        source_start_frame: u64,
        duration_frames: u64,
    },
    SetClipColor {
        track_id: u64,
        clip_id: u64,
        color: ColorAdjust,
    },
    SetClipAudio {
        track_id: u64,
        clip_id: u64,
        volume: f32,
        muted: bool,
    },
    SetClipSpeed {
        track_id: u64,
        clip_id: u64,
        speed: f64,
    },
    SetClipFades {
        track_id: u64,
        clip_id: u64,
        fade_in_frames: u64,
        fade_out_frames: u64,
    },
    SetClipNoiseReduction {
        track_id: u64,
        clip_id: u64,
        enabled: bool,
    },
    SetTitle {
        track_id: u64,
        clip_id: u64,
        title: Title,
    },
    SetSubtitleStyle {
        track_id: u64,
        style: Title,
    },
    RippleDelete {
        track_id: u64,
        clip_id: u64,
    },
    RippleDeleteSelection(Vec<(u64, u64)>),
    RemoveSilences(Vec<(u64, u64)>),
    ReplaceTracks(Vec<Track>),
    AddMarker(u64),
    RemoveMarker(u64),
}

fn ripple_delete(project: &Project, selection: &[(u64, u64)]) -> Option<Vec<Track>> {
    if selection.is_empty() {
        return None;
    }
    let selected: HashSet<_> = selection.iter().copied().collect();
    if selected.len() != selection.len() {
        return None;
    }
    let mut ranges = Vec::with_capacity(selection.len());
    for &(track_id, clip_id) in selection {
        let clip = project
            .tracks
            .iter()
            .find(|track| track.id == track_id && !track.locked)?
            .clips
            .iter()
            .find(|clip| clip.id == clip_id)?;
        ranges.push((
            clip.start_frame,
            clip.start_frame.checked_add(clip.duration_frames)?,
        ));
    }
    ripple_ranges(project, ranges, &selected)
}

fn ripple_ranges(
    project: &Project,
    mut ranges: Vec<(u64, u64)>,
    selected: &HashSet<(u64, u64)>,
) -> Option<Vec<Track>> {
    if ranges.is_empty() || ranges.iter().any(|(start, end)| start >= end) {
        return None;
    }
    ranges.sort_unstable();
    let mut merged: Vec<(u64, u64)> = Vec::new();
    for (start, end) in ranges {
        if let Some(last) = merged.last_mut()
            && start <= last.1
        {
            last.1 = last.1.max(end);
        } else {
            merged.push((start, end));
        }
    }
    let mut tracks = project.tracks.clone();
    let mut next_id = tracks
        .iter()
        .flat_map(|track| &track.clips)
        .map(|clip| clip.id)
        .max()
        .unwrap_or(0);
    for track in &mut tracks {
        if track.locked {
            continue;
        }
        track
            .clips
            .retain(|clip| !selected.contains(&(track.id, clip.id)));
    }
    for (start, end) in merged.into_iter().rev() {
        let length = end - start;
        for track in &mut tracks {
            if track.locked {
                continue;
            }
            let mut kept = Vec::with_capacity(track.clips.len());
            for mut clip in std::mem::take(&mut track.clips) {
                let clip_end = clip.start_frame.checked_add(clip.duration_frames)?;
                if clip_end <= start {
                    kept.push(clip);
                } else if clip.start_frame >= end {
                    clip.start_frame -= length;
                    kept.push(clip);
                } else if clip.start_frame < start {
                    if clip_end > end {
                        let mut right = clip.clone();
                        next_id = next_id.checked_add(1)?;
                        right.id = next_id;
                        right.transition = None;
                        right.keyframes =
                            clip.keyframes_for_range(i128::from(end - clip.start_frame));
                        right.source_start_frame =
                            clip.source_start_for_range(end - clip.start_frame, clip_end - end)?;
                        right.start_frame = start;
                        right.duration_frames = clip_end - end;
                        clip.source_start_frame =
                            clip.source_start_for_range(0, start - clip.start_frame)?;
                        right.fade_in_frames = 0;
                        right.clamp_fades();
                        clip.fade_out_frames = 0;
                        clip.transition_out = None;
                        clip.duration_frames = start - clip.start_frame;
                        kept.push(clip);
                        kept.push(right);
                    } else {
                        clip.source_start_frame =
                            clip.source_start_for_range(0, start - clip.start_frame)?;
                        clip.duration_frames = start - clip.start_frame;
                        kept.push(clip);
                    }
                } else if clip_end > end {
                    clip.keyframes = clip.keyframes_for_range(i128::from(end - clip.start_frame));
                    clip.source_start_frame =
                        clip.source_start_for_range(end - clip.start_frame, clip_end - end)?;
                    clip.duration_frames = clip_end - end;
                    clip.start_frame = start;
                    kept.push(clip);
                }
            }
            for clip in &mut kept {
                clip.clamp_fades();
            }
            track.clips = kept;
        }
    }
    Some(tracks)
}

struct HistoryEntry {
    command: Command,
    revision: u64,
}

pub struct Editor {
    project: Project,
    undo: Vec<HistoryEntry>,
    redo: Vec<HistoryEntry>,
    revision: u64,
    next_revision: u64,
    saved_revision: u64,
}

pub fn freeze_at(project: &Project, track_id: u64, clip_id: u64, frame: u64) -> Option<Command> {
    let track = project
        .tracks()
        .iter()
        .find(|t| t.id == track_id && !t.locked)?;
    let clip = track.clips.iter().find(|c| c.id == clip_id)?;
    let ClipContent::Media { media_id } = clip.content else {
        return None;
    };
    if track.kind != TrackKind::Video
        || !project
            .media()
            .iter()
            .any(|m| m.id == media_id && m.kind == MediaKind::Video)
    {
        return None;
    }
    let offset = frame.checked_sub(clip.start_frame)?;
    let held = clip.source_frame_at(offset)?;
    let new_id = project
        .tracks()
        .iter()
        .flat_map(|t| &t.clips)
        .map(|c| c.id)
        .max()
        .unwrap_or(0)
        .checked_add(1)?;
    let mut tracks = if offset == 0 {
        project.tracks().to_vec()
    } else {
        split_tracks(project.tracks(), &[(track_id, clip_id)], frame, new_id)?
    };
    let right = tracks
        .iter_mut()
        .find(|t| t.id == track_id)?
        .clips
        .iter_mut()
        .find(|c| c.id == if offset == 0 { clip_id } else { new_id })?;
    right.playback = Playback::Freeze;
    right.speed = 1.0;
    right.source_start_frame = held;
    Some(Command::ReplaceTracks(tracks))
}

pub(crate) fn trim_edge(
    project: &Project,
    track_id: u64,
    clip_id: u64,
    left: bool,
    frame: u64,
) -> Option<Command> {
    let track = project.tracks().iter().find(|track| track.id == track_id)?;
    let clip = track.clips.iter().find(|clip| clip.id == clip_id)?;
    let end = clip.start_frame.checked_add(clip.duration_frames)?;
    let previous = track
        .clips
        .iter()
        .filter(|other| other.id != clip_id && other.start_frame < clip.start_frame)
        .map(|other| other.start_frame + other.duration_frames)
        .max()
        .unwrap_or(0);
    let next = track
        .clips
        .iter()
        .filter(|other| other.start_frame > clip.start_frame)
        .map(|other| other.start_frame)
        .min()
        .unwrap_or(u64::MAX);
    let available = match clip.content {
        ClipContent::Media { media_id } => project
            .media()
            .iter()
            .find(|media| media.id == media_id)
            .and_then(|media| {
                if media.kind == MediaKind::Image || clip.playback == Playback::Freeze {
                    return None;
                }
                media.duration_us.map(|duration| {
                    (u128::from(duration) * u128::from(project.settings().frame_rate.numerator)
                        / (1_000_000 * u128::from(project.settings().frame_rate.denominator)))
                    .max(u128::from(duration > 0))
                    .min(u128::from(u64::MAX)) as u64
                })
            }),
        _ => None,
    };
    let (start_frame, source_start_frame, duration_frames) = if left {
        let timed_media = match clip.content {
            ClipContent::Media { media_id } => project
                .media()
                .iter()
                .any(|media| media.id == media_id && media.kind != MediaKind::Image),
            _ => false,
        };
        let minimum = if timed_media && clip.playback != Playback::Freeze {
            let extension = if clip.playback == Playback::Reverse {
                available
                    .unwrap_or(u64::MAX)
                    .saturating_sub(clip.source_start_frame + clip.source_span())
            } else {
                clip.source_start_frame
            };
            clip.start_frame
                .saturating_sub(clip.timeline_span(extension))
        } else {
            0
        }
        .max(previous);
        let start = frame.clamp(minimum, end - 1);
        let source = if clip.playback != Playback::Forward {
            clip.source_start_frame
        } else if start >= clip.start_frame {
            clip.source_start_frame
                .saturating_add(clip.source_offset(start - clip.start_frame))
        } else {
            clip.source_start_frame
                .saturating_sub(clip.source_offset(clip.start_frame - start))
        };
        (start, source, end - start)
    } else {
        let maximum = if clip.playback == Playback::Reverse {
            clip.start_frame.saturating_add(
                clip.timeline_span(clip.source_start_frame.checked_add(clip.source_span())?),
            )
        } else {
            available.map_or(u64::MAX, |frames| {
                clip.start_frame.saturating_add(
                    clip.timeline_span(frames.saturating_sub(clip.source_start_frame)),
                )
            })
        }
        .min(next);
        let end = frame.clamp(clip.start_frame + 1, maximum);
        (
            clip.start_frame,
            if clip.playback == Playback::Reverse {
                (clip.source_start_frame + clip.source_span())
                    .checked_sub(((end - clip.start_frame) as f64 * clip.speed).ceil() as u64)?
            } else {
                clip.source_start_frame
            },
            end - clip.start_frame,
        )
    };
    Some(Command::Trim {
        track_id,
        clip_id,
        start_frame,
        source_start_frame,
        duration_frames,
    })
}

#[cfg(test)]
mod playback_tests {
    use super::*;

    fn editor(playback: Playback) -> Editor {
        let mut editor = Editor::new(Project::new("Playback"));
        assert!(editor.apply(Command::ImportMedia(Media {
            id: 1,
            path: "video.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(1_000_000),
            has_audio: true
        })));
        assert!(editor.apply(Command::InsertClip {
            track_id: 0,
            clip: Clip {
                keyframes: Vec::new(),
                speed: 1.0,
                transform: crate::timeline::Transform::default(),
                noise_reduction: false,
                lut: None,
                transition_out: None,
                color: Default::default(),
                transition: None,
                fade_in_frames: 0,
                fade_out_frames: 0,
                playback,
                id: 1,
                start_frame: 10,
                source_start_frame: 5,
                duration_frames: 20,
                content: ClipContent::Media { media_id: 1 },
                volume: 1.0,
                muted: false,
                linked_audio: true
            }
        }));
        editor
    }

    #[test]
    fn split_and_ripple_preserve_reverse_and_freeze_frames() {
        for playback in [Playback::Reverse, Playback::Freeze] {
            let mut editor = editor(playback);
            let original = editor.project().tracks()[0].clips[0].clone();
            let tracks = split_tracks(editor.project().tracks(), &[(0, 1)], 18, 2).unwrap();
            for (part, offset) in tracks[0].clips.iter().zip([0, 8]) {
                for k in 0..part.duration_frames {
                    assert_eq!(
                        part.source_frame_at(k),
                        original.source_frame_at(offset + k)
                    );
                }
            }
            assert!(editor.apply(Command::InsertClip {
                track_id: 1,
                clip: Clip {
                    keyframes: Vec::new(),
                    speed: 1.0,
                    transform: crate::timeline::Transform::default(),
                    noise_reduction: false,
                    lut: None,
                    transition_out: None,
                    color: Default::default(),
                    transition: None,
                    fade_in_frames: 0,
                    fade_out_frames: 0,
                    playback: Playback::Forward,
                    id: 3,
                    start_frame: 16,
                    source_start_frame: 0,
                    duration_frames: 5,
                    content: ClipContent::Title(Title::default()),
                    volume: 1.0,
                    muted: false,
                    linked_audio: false
                }
            }));
            assert!(editor.apply(Command::RippleDelete {
                track_id: 1,
                clip_id: 3
            }));
            let clips = &editor.project().tracks()[0].clips;
            for (part, offset) in clips.iter().zip([0, 11]) {
                for k in 0..part.duration_frames {
                    assert_eq!(
                        part.source_frame_at(k),
                        original.source_frame_at(offset + k)
                    );
                }
            }
            assert!(editor.undo());
            assert_eq!(editor.project().tracks()[0].clips, vec![original]);
        }
    }

    #[test]
    fn trim_edges_preserve_reverse_source_end_and_freeze_hold() {
        for (playback, left, frame, source, duration) in [
            (Playback::Reverse, true, 15, 5, 15),
            (Playback::Reverse, true, 0, 5, 25),
            (Playback::Reverse, false, 25, 10, 15),
            (Playback::Reverse, false, 100, 0, 25),
            (Playback::Freeze, true, 0, 5, 30),
            (Playback::Freeze, false, 100, 5, 90),
        ] {
            let mut editor = editor(playback);
            let before = editor.project().tracks().to_vec();
            let command = trim_edge(editor.project(), 0, 1, left, frame).unwrap();
            assert!(editor.apply(command));
            let clip = &editor.project().tracks()[0].clips[0];
            assert_eq!(
                (clip.source_start_frame, clip.duration_frames),
                (source, duration)
            );
            assert!(editor.undo());
            assert_eq!(editor.project().tracks(), before);
        }
    }

    #[test]
    fn playback_switches_are_undoable_and_leaving_freeze_clamps_to_held_frame() {
        let mut editor = editor(Playback::Forward);
        assert!(editor.apply(Command::SetClipFades {
            track_id: 0,
            clip_id: 1,
            fade_in_frames: 10,
            fade_out_frames: 10,
        }));
        let before = editor.project().tracks().to_vec();
        for playback in [Playback::Reverse, Playback::Freeze, Playback::Forward] {
            assert!(editor.apply(Command::SetPlayback {
                track_id: 0,
                clip_id: 1,
                playback
            }));
        }
        let clip = &editor.project().tracks()[0].clips[0];
        assert_eq!((clip.source_start_frame, clip.duration_frames), (24, 6));
        assert_eq!((clip.fade_in_frames, clip.fade_out_frames), (6, 0));
        let after = editor.project().tracks().to_vec();
        for _ in 0..3 {
            assert!(editor.undo());
        }
        assert_eq!(editor.project().tracks(), before);
        for _ in 0..3 {
            assert!(editor.redo());
        }
        assert_eq!(editor.project().tracks(), after);
        assert!(!editor.apply(Command::SetPlayback {
            track_id: 99,
            clip_id: 1,
            playback: Playback::Freeze
        }));
    }

    #[test]
    fn frame_hold_splits_at_the_displayed_frame_as_one_undo_step() {
        for playback in [Playback::Forward, Playback::Reverse, Playback::Freeze] {
            for frame in [10, 18] {
                let mut editor = editor(playback);
                let before = editor.project().tracks().to_vec();
                let held = before[0].clips[0].source_frame_at(frame - 10).unwrap();
                let command = freeze_at(editor.project(), 0, 1, frame).unwrap();
                let changed = editor.apply(command);
                if playback == Playback::Freeze && frame == 10 {
                    assert!(!changed);
                    continue;
                }
                assert!(changed);
                let right = editor.project().tracks()[0].clips.last().unwrap();
                assert_eq!(
                    (
                        right.playback,
                        right.source_start_frame,
                        right.duration_frames
                    ),
                    (Playback::Freeze, held, 30 - frame)
                );
                assert!(editor.undo());
                assert_eq!(editor.project().tracks(), before);
                assert!(editor.redo());
            }
        }
        let editor = editor(Playback::Forward);
        for frame in [0, 30, 100] {
            assert!(freeze_at(editor.project(), 0, 1, frame).is_none());
        }
        assert!(freeze_at(editor.project(), 99, 1, 10).is_none());
    }

    #[test]
    fn playback_and_frame_holds_respect_track_locks_but_allow_hidden_tracks() {
        for playback in [Playback::Forward, Playback::Reverse, Playback::Freeze] {
            let mut editor = editor(playback);
            assert!(editor.apply(Command::SetTrackLocked {
                track_id: 0,
                locked: true
            }));
            let locked = editor.project().clone();
            for frame in [10, 18] {
                assert!(freeze_at(editor.project(), 0, 1, frame).is_none());
            }
            let change = Command::SetPlayback {
                track_id: 0,
                clip_id: 1,
                playback: if playback == Playback::Reverse {
                    Playback::Forward
                } else {
                    Playback::Reverse
                },
            };
            assert!(!editor.apply(change.clone()));
            assert_eq!(editor.project(), &locked);
            assert!(editor.undo());
            assert!(editor.apply(Command::SetTrackHidden {
                track_id: 0,
                hidden: true
            }));
            assert!(editor.apply(change));
            let command = freeze_at(editor.project(), 0, 1, 18).unwrap();
            assert!(editor.apply(command));
            assert!(editor.project().tracks()[0].hidden);
            assert_eq!(
                editor.project().tracks()[0].clips[1].playback,
                Playback::Freeze
            );
            editor.project().validate().unwrap();
        }
    }

    #[test]
    fn validation_rejects_incompatible_playback_and_bounds_only_the_held_frame() {
        let mut editor = editor(Playback::Freeze);
        let mut tracks = editor.project().tracks().to_vec();
        tracks[0].clips[0].duration_frames = 1000;
        assert!(editor.apply(Command::ReplaceTracks(tracks.clone())));
        tracks[0].clips[0].source_start_frame = 30;
        assert!(!editor.apply(Command::ReplaceTracks(tracks)));
        for kind in [MediaKind::Image, MediaKind::Audio] {
            let media_id = if kind == MediaKind::Image { 2 } else { 3 };
            assert!(editor.apply(Command::ImportMedia(Media {
                id: media_id,
                path: format!("other-{media_id}").into(),
                kind,
                duration_us: Some(1_000_000),
                has_audio: kind == MediaKind::Audio
            })));
            let mut clip = editor.project().tracks()[0].clips[0].clone();
            clip.id = 2;
            clip.start_frame = 0;
            clip.duration_frames = 5;
            clip.linked_audio = false;
            clip.content = ClipContent::Media { media_id };
            for playback in [Playback::Reverse, Playback::Freeze] {
                clip.playback = playback;
                let accepted = editor.apply(Command::InsertClip {
                    track_id: if kind == MediaKind::Audio { 2 } else { 1 },
                    clip: clip.clone(),
                });
                assert_eq!(
                    accepted,
                    kind == MediaKind::Audio && playback == Playback::Reverse
                );
                if accepted {
                    assert!(editor.undo());
                }
            }
        }
        assert!(!editor.apply(Command::Move {
            track_id: 0,
            target_track_id: 2,
            clip_id: 1,
            to_frame: 100
        }));
        let mut tracks = editor.project().tracks().to_vec();
        tracks[1].clips.push(Clip {
            keyframes: Vec::new(),
            speed: 1.0,
            transform: crate::timeline::Transform::default(),
            noise_reduction: false,
            lut: None,
            transition_out: None,
            color: Default::default(),
            transition: None,
            fade_in_frames: 0,
            fade_out_frames: 0,
            playback: Playback::Reverse,
            id: 99,
            start_frame: 0,
            source_start_frame: 0,
            duration_frames: 1,
            content: ClipContent::Title(Title::default()),
            volume: 1.0,
            muted: false,
            linked_audio: false,
        });
        assert!(!editor.apply(Command::ReplaceTracks(tracks)));
    }
}

impl Editor {
    pub fn new(project: Project) -> Self {
        Self {
            project,
            undo: Vec::new(),
            redo: Vec::new(),
            revision: 0,
            next_revision: 1,
            saved_revision: 0,
        }
    }

    pub fn project(&self) -> &Project {
        &self.project
    }

    pub fn can_set_clip_speed(&self, track_id: u64, clip_id: u64, speed: f64) -> bool {
        Self::new(self.project.clone())
            .execute(Command::SetClipSpeed {
                track_id,
                clip_id,
                speed,
            })
            .is_some()
    }

    pub fn recovered(project: Project) -> Self {
        Self {
            saved_revision: u64::MAX,
            ..Self::new(project)
        }
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn mark_saved(&mut self, revision: u64) {
        self.saved_revision = revision;
    }

    pub fn is_dirty(&self) -> bool {
        self.revision != self.saved_revision
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    fn execute(&mut self, command: Command) -> Option<Command> {
        let editable = |id| {
            self.project
                .tracks
                .iter()
                .any(|track| track.id == id && !track.locked)
        };
        let allowed = match &command {
            Command::InsertClip { track_id, .. }
            | Command::RemoveClip { track_id, .. }
            | Command::Split { track_id, .. }
            | Command::Trim { track_id, .. }
            | Command::SetClipSpeed { track_id, .. }
            | Command::SetClipAudio { track_id, .. }
            | Command::SetClipColor { track_id, .. }
            | Command::SetPlayback { track_id, .. }
            | Command::SetClipTransform { track_id, .. }
            | Command::SetClipKeyframes { track_id, .. }
            | Command::SetClipLut { track_id, .. }
            | Command::SetClipFades { track_id, .. }
            | Command::SetClipNoiseReduction { track_id, .. }
            | Command::SetTransition { track_id, .. }
            | Command::SetTransitionOut { track_id, .. }
            | Command::SetTitle { track_id, .. } => editable(*track_id),
            Command::Move {
                track_id,
                target_track_id,
                ..
            } => editable(*track_id) && editable(*target_track_id),
            Command::ReplaceTracks(tracks) => self
                .project
                .tracks
                .iter()
                .filter(|track| track.locked)
                .all(|old| {
                    tracks.iter().any(|new| {
                        new.id == old.id && new.kind == old.kind && new.clips == old.clips
                    })
                }),
            _ => true,
        };
        if !allowed {
            return None;
        }
        match command {
            Command::SetPlayback {
                track_id,
                clip_id,
                playback,
            } => {
                let mut tracks = self.project.tracks.clone();
                let clip = tracks
                    .iter_mut()
                    .find(|t| t.id == track_id)?
                    .clips
                    .iter_mut()
                    .find(|c| c.id == clip_id)?;
                if clip.playback == playback {
                    return None;
                }
                if playback == Playback::Freeze {
                    clip.source_start_frame = clip.source_frame_at(0)?;
                    clip.speed = 1.0;
                } else if clip.playback == Playback::Freeze {
                    let ClipContent::Media { media_id } = clip.content else {
                        return None;
                    };
                    let media = self.project.media.iter().find(|m| m.id == media_id)?;
                    if let Some(duration) = media.duration_us {
                        let rate = &self.project.settings.frame_rate;
                        let frames = (u128::from(duration) * u128::from(rate.numerator)
                            / (1_000_000 * u128::from(rate.denominator)))
                        .max(1)
                        .min(u128::from(u64::MAX)) as u64;
                        clip.duration_frames = clip
                            .duration_frames
                            .min(frames.checked_sub(clip.source_start_frame)?);
                    }
                }
                clip.playback = playback;
                clip.clamp_fades();
                self.execute(Command::ReplaceTracks(tracks))
            }
            Command::RenameTrack { track_id, name } => {
                let name = name.trim().to_owned();
                if !Track::valid_name(&name) {
                    return None;
                }
                let track = self
                    .project
                    .tracks
                    .iter_mut()
                    .find(|track| track.id == track_id)?;
                if track.name == name {
                    return None;
                }
                Some(Command::RenameTrack {
                    track_id,
                    name: std::mem::replace(&mut track.name, name),
                })
            }
            Command::SetTrackLocked { track_id, locked } => {
                let track = self
                    .project
                    .tracks
                    .iter_mut()
                    .find(|track| track.id == track_id)?;
                if track.locked == locked {
                    return None;
                }
                Some(Command::SetTrackLocked {
                    track_id,
                    locked: std::mem::replace(&mut track.locked, locked),
                })
            }
            Command::SetTrackHidden { track_id, hidden } => {
                let track = self
                    .project
                    .tracks
                    .iter_mut()
                    .find(|track| track.id == track_id)?;
                if track.hidden == hidden {
                    return None;
                }
                Some(Command::SetTrackHidden {
                    track_id,
                    hidden: std::mem::replace(&mut track.hidden, hidden),
                })
            }
            Command::MoveTrack { track_id, down } => {
                let index = self
                    .project
                    .tracks
                    .iter()
                    .position(|track| track.id == track_id)?;
                let kind = self.project.tracks[index].kind;
                let target = if down {
                    (index + 1..self.project.tracks.len())
                        .find(|&i| self.project.tracks[i].kind == kind)
                } else {
                    (0..index)
                        .rev()
                        .find(|&i| self.project.tracks[i].kind == kind)
                }?;
                self.project.tracks.swap(index, target);
                Some(Command::MoveTrack {
                    track_id,
                    down: !down,
                })
            }
            Command::RenameProject(name)
                if name != self.project.name() && !name.trim().is_empty() =>
            {
                Some(Command::RenameProject(self.project.replace_name(name)))
            }
            Command::RenameProject(_) => None,
            Command::SetLastExport(export) => {
                if export == self.project.last_export
                    || export
                        .as_ref()
                        .is_some_and(|value| value.validate().is_err())
                {
                    return None;
                }
                Some(Command::SetLastExport(std::mem::replace(
                    &mut self.project.last_export,
                    export,
                )))
            }
            Command::InsertClip { track_id, clip } => {
                let end = clip.start_frame.checked_add(clip.duration_frames)?;
                if clip.duration_frames == 0
                    || clip
                        .source_start_frame
                        .checked_add(if clip.playback == Playback::Freeze {
                            0
                        } else {
                            clip.source_span()
                        })
                        .is_none()
                    || !clip.transform.valid()
                    || !crate::timeline::valid_keyframes(&clip.keyframes)
                    || (!clip.keyframes.is_empty()
                        && !matches!(clip.content, ClipContent::Media { .. }))
                    || !clip.color.valid()
                    || !clip.volume.is_finite()
                    || !(0.0..=2.0).contains(&clip.volume)
                    || !clip.valid_fades()
                    || self
                        .project
                        .tracks
                        .iter()
                        .flat_map(|track| &track.clips)
                        .any(|existing| existing.id == clip.id)
                {
                    return None;
                }
                let track = self
                    .project
                    .tracks
                    .iter()
                    .find(|track| track.id == track_id)?;
                let kind = track.kind;
                let compatible = match &clip.content {
                    ClipContent::Title(title) => {
                        kind == TrackKind::Video
                            && title.valid()
                            && clip.speed == 1.0
                            && clip.playback == Playback::Forward
                    }
                    ClipContent::Media { media_id } => {
                        !track.subtitles
                            && self
                                .project
                                .media
                                .iter()
                                .find(|media| media.id == *media_id)
                                .is_some_and(|media| {
                                    (media.duration_us.is_some() || media.kind == MediaKind::Image)
                                        && self.project.media_clip_fits(media, kind, &clip)
                                })
                    }
                };
                if !compatible
                    || (!clip.keyframes.is_empty() && kind != TrackKind::Video)
                    || (clip.lut.is_some()
                        && (kind != TrackKind::Video
                            || !matches!(clip.content, ClipContent::Media { .. })))
                {
                    return None;
                }
                let track = self.project.tracks.iter_mut().find(|t| t.id == track_id)?;
                if track.clips.iter().any(|existing| {
                    existing
                        .start_frame
                        .checked_add(existing.duration_frames)
                        .is_none_or(|existing_end| {
                            clip.start_frame < existing_end && existing.start_frame < end
                        })
                }) {
                    return None;
                }
                let index = track
                    .clips
                    .partition_point(|item| item.start_frame < clip.start_frame);
                let clip_id = clip.id;
                track.clips.insert(index, clip);
                Some(Command::RemoveClip { track_id, clip_id })
            }
            Command::RemoveClip { track_id, clip_id } => {
                let track = self.project.tracks.iter_mut().find(|t| t.id == track_id)?;
                let index = track.clips.iter().position(|clip| clip.id == clip_id)?;
                Some(Command::InsertClip {
                    track_id,
                    clip: track.clips.remove(index),
                })
            }
            Command::ImportMedia(media) => {
                if media.path.as_os_str().is_empty()
                    || media.duration_us == Some(0)
                    || self
                        .project
                        .media
                        .iter()
                        .any(|m| m.id == media.id || m.path == media.path)
                {
                    return None;
                }
                let id = media.id;
                self.project.media.push(media);
                Some(Command::RemoveMedia(id))
            }
            Command::RelinkMedia(media) => {
                let index = self
                    .project
                    .media
                    .iter()
                    .position(|item| item.id == media.id)?;
                if self.project.media[index].kind != media.kind
                    || self.project.media[index] == media
                {
                    return None;
                }
                let has_sound = media.kind == MediaKind::Video && media.has_audio;
                let linked_audio = self
                    .project
                    .tracks
                    .iter()
                    .flat_map(|track| &track.clips)
                    .filter(|clip| clip.content == ClipContent::Media { media_id: media.id })
                    .map(|clip| (clip.id, clip.linked_audio && has_sound))
                    .collect();
                self.execute(Command::RestoreMedia {
                    media,
                    linked_audio,
                })
            }
            Command::RestoreMedia {
                media,
                linked_audio,
            } => {
                let index = self
                    .project
                    .media
                    .iter()
                    .position(|item| item.id == media.id)?;
                let original = std::mem::replace(&mut self.project.media[index], media);
                let mut previous = Vec::new();
                for clip in self
                    .project
                    .tracks
                    .iter_mut()
                    .flat_map(|track| &mut track.clips)
                {
                    if let Some((_, linked)) = linked_audio.iter().find(|(id, _)| *id == clip.id) {
                        previous
                            .push((clip.id, std::mem::replace(&mut clip.linked_audio, *linked)));
                    }
                }
                if self.project.validate().is_err() {
                    self.project.media[index] = original;
                    for clip in self
                        .project
                        .tracks
                        .iter_mut()
                        .flat_map(|track| &mut track.clips)
                    {
                        if let Some((_, linked)) = previous.iter().find(|(id, _)| *id == clip.id) {
                            clip.linked_audio = *linked;
                        }
                    }
                    return None;
                }
                Some(Command::RestoreMedia {
                    media: original,
                    linked_audio: previous,
                })
            }
            Command::ReprobeMedia(media) => {
                let index = self
                    .project
                    .media
                    .iter()
                    .position(|item| item.id == media.id)?;
                if self.project.media[index].path != media.path
                    || self.project.media[index].kind != media.kind
                    || self.project.media[index] == media
                {
                    return None;
                }
                let original = std::mem::replace(&mut self.project.media[index], media);
                if self.project.validate().is_err() {
                    self.project.media[index] = original;
                    return None;
                }
                Some(Command::ReprobeMedia(original))
            }
            Command::RemoveMedia(id) => {
                if self
                    .project
                    .tracks
                    .iter()
                    .flat_map(|t| &t.clips)
                    .any(|c| matches!(c.content, ClipContent::Media { media_id } if media_id == id))
                {
                    return None;
                }
                let index = self.project.media.iter().position(|m| m.id == id)?;
                Some(Command::ImportMedia(self.project.media.remove(index)))
            }
            Command::AddMarker(frame) => {
                let index = self
                    .project
                    .markers
                    .binary_search_by_key(&frame, |marker| marker.frame)
                    .err()?;
                self.project.markers.insert(index, Marker { frame });
                Some(Command::RemoveMarker(frame))
            }
            Command::RemoveMarker(frame) => {
                let index = self
                    .project
                    .markers
                    .binary_search_by_key(&frame, |marker| marker.frame)
                    .ok()?;
                self.project.markers.remove(index);
                Some(Command::AddMarker(frame))
            }
            Command::ReplaceTracks(tracks) => {
                if self.project.tracks == tracks {
                    return None;
                }
                let original = std::mem::replace(&mut self.project.tracks, tracks);
                if self.project.validate().is_err() {
                    self.project.tracks = original;
                    return None;
                }
                Some(Command::ReplaceTracks(original))
            }
            Command::Split {
                track_id,
                clip_id,
                at_frame,
                new_id,
            } => {
                let tracks = split_tracks(
                    &self.project.tracks,
                    &[(track_id, clip_id)],
                    at_frame,
                    new_id,
                )?;
                self.execute(Command::ReplaceTracks(tracks))
            }
            Command::Move {
                track_id,
                target_track_id,
                clip_id,
                to_frame,
            } => {
                let mut tracks = self.project.tracks.clone();
                let source = tracks.iter_mut().find(|t| t.id == track_id)?;
                let subtitles = source.subtitles;
                let index = source.clips.iter().position(|c| c.id == clip_id)?;
                let mut clip = source.clips.remove(index);
                clip.start_frame = to_frame;
                let target = tracks.iter_mut().find(|t| t.id == target_track_id)?;
                if target.subtitles != subtitles {
                    return None;
                }
                let index = target.clips.partition_point(|c| c.start_frame < to_frame);
                target.clips.insert(index, clip);
                self.execute(Command::ReplaceTracks(tracks))
            }
            Command::Trim {
                track_id,
                clip_id,
                start_frame,
                source_start_frame,
                duration_frames,
            } => {
                let mut tracks = self.project.tracks.clone();
                let clip = tracks
                    .iter_mut()
                    .find(|t| t.id == track_id)?
                    .clips
                    .iter_mut()
                    .find(|c| c.id == clip_id)?;
                clip.keyframes = clip
                    .keyframes_for_range(i128::from(start_frame) - i128::from(clip.start_frame));
                clip.start_frame = start_frame;
                clip.source_start_frame = source_start_frame;
                clip.duration_frames = duration_frames;
                clip.clamp_fades();
                self.execute(Command::ReplaceTracks(tracks))
            }
            Command::SetClipKeyframes {
                track_id,
                clip_id,
                keyframes,
            } => {
                if !crate::timeline::valid_keyframes(&keyframes) {
                    return None;
                }
                let track = self
                    .project
                    .tracks
                    .iter_mut()
                    .find(|t| t.id == track_id && t.kind == TrackKind::Video)?;
                let clip = track.clips.iter_mut().find(|c| c.id == clip_id)?;
                if !matches!(clip.content, ClipContent::Media { .. }) || clip.keyframes == keyframes
                {
                    return None;
                }
                let old = std::mem::replace(&mut clip.keyframes, keyframes);
                Some(Command::SetClipKeyframes {
                    track_id,
                    clip_id,
                    keyframes: old,
                })
            }
            Command::SetClipTransform {
                track_id,
                clip_id,
                transform,
            } => {
                if !transform.valid() {
                    return None;
                }
                let track = self
                    .project
                    .tracks
                    .iter_mut()
                    .find(|t| t.id == track_id && t.kind == TrackKind::Video)?;
                let clip = track.clips.iter_mut().find(|c| c.id == clip_id)?;
                if !matches!(clip.content, ClipContent::Media { .. }) || clip.transform == transform
                {
                    return None;
                }
                let inverse = Command::SetClipTransform {
                    track_id,
                    clip_id,
                    transform: clip.transform,
                };
                clip.transform = transform;
                Some(inverse)
            }
            Command::SetClipColor {
                track_id,
                clip_id,
                color,
            } => {
                if !color.valid() {
                    return None;
                }
                let track = self
                    .project
                    .tracks
                    .iter_mut()
                    .find(|track| track.id == track_id && track.kind == TrackKind::Video)?;
                let clip = track.clips.iter_mut().find(|clip| clip.id == clip_id)?;
                if !matches!(clip.content, ClipContent::Media { .. }) || clip.color == color {
                    return None;
                }
                let old = std::mem::replace(&mut clip.color, color);
                Some(Command::SetClipColor {
                    track_id,
                    clip_id,
                    color: old,
                })
            }
            Command::SetClipFades {
                track_id,
                clip_id,
                fade_in_frames,
                fade_out_frames,
            } => {
                let track = self
                    .project
                    .tracks
                    .iter_mut()
                    .find(|track| track.id == track_id)?;
                let audio = track.kind == TrackKind::Audio;
                let clip = track.clips.iter_mut().find(|clip| clip.id == clip_id)?;
                if (!audio && !clip.linked_audio)
                    || fade_in_frames > clip.duration_frames
                    || fade_out_frames > clip.duration_frames - fade_in_frames
                    || (fade_in_frames == clip.fade_in_frames
                        && fade_out_frames == clip.fade_out_frames)
                {
                    return None;
                }
                let inverse = Command::SetClipFades {
                    track_id,
                    clip_id,
                    fade_in_frames: clip.fade_in_frames,
                    fade_out_frames: clip.fade_out_frames,
                };
                clip.fade_in_frames = fade_in_frames;
                clip.fade_out_frames = fade_out_frames;
                Some(inverse)
            }
            Command::SetClipAudio {
                track_id,
                clip_id,
                volume,
                muted,
            } => {
                if !volume.is_finite() || !(0.0..=2.0).contains(&volume) {
                    return None;
                }
                let track = self
                    .project
                    .tracks
                    .iter_mut()
                    .find(|track| track.id == track_id)?;
                let audio_track = track.kind == TrackKind::Audio;
                let clip = track.clips.iter_mut().find(|clip| clip.id == clip_id)?;
                if !audio_track && !clip.linked_audio {
                    return None;
                }
                if clip.volume == volume && clip.muted == muted {
                    return None;
                }
                let inverse = Command::SetClipAudio {
                    track_id,
                    clip_id,
                    volume: clip.volume,
                    muted: clip.muted,
                };
                clip.volume = volume;
                clip.muted = muted;
                Some(inverse)
            }
            Command::SetClipSpeed {
                track_id,
                clip_id,
                speed,
            } => {
                if !speed.is_finite() || !(0.25..=4.0).contains(&speed) {
                    return None;
                }
                let mut tracks = self.project.tracks.clone();
                let track = tracks.iter_mut().find(|track| track.id == track_id)?;
                let index = track.clips.iter().position(|clip| clip.id == clip_id)?;
                let next = track
                    .clips
                    .iter()
                    .filter(|clip| clip.start_frame > track.clips[index].start_frame)
                    .map(|clip| clip.start_frame)
                    .min()
                    .unwrap_or(u64::MAX);
                let clip = &mut track.clips[index];
                let ClipContent::Media { media_id } = clip.content else {
                    return None;
                };
                let media = self
                    .project
                    .media
                    .iter()
                    .find(|media| media.id == media_id)?;
                if media.kind == MediaKind::Image
                    || clip.playback == Playback::Freeze
                    || clip.speed == speed
                {
                    return None;
                }
                let source_end = clip.source_start_frame.checked_add(clip.source_span())?;
                let duration = (clip.duration_frames as f64 * clip.speed / speed).round() as u64;
                clip.speed = speed;
                clip.duration_frames = duration.max(1).min(next - clip.start_frame);
                if clip.playback == Playback::Reverse {
                    clip.duration_frames = clip.duration_frames.min(clip.timeline_span(source_end));
                    clip.source_start_frame = source_end.checked_sub(clip.source_span())?;
                } else if let Some(duration_us) = media.duration_us {
                    let rate = &self.project.settings.frame_rate;
                    let frames = (u128::from(duration_us) * u128::from(rate.numerator)
                        / (1_000_000 * u128::from(rate.denominator)))
                    .max(1)
                    .min(u128::from(u64::MAX)) as u64;
                    clip.duration_frames = clip
                        .duration_frames
                        .min(clip.timeline_span(frames.saturating_sub(clip.source_start_frame)));
                }
                let last = clip.duration_frames.saturating_sub(1);
                if clip.keyframes.last().is_some_and(|key| key.frame > last) {
                    let boundary = crate::timeline::Keyframe::new(
                        last,
                        clip.transform_at(clip.start_frame + last),
                    );
                    if let Some(index) = clip.keyframes.iter().rposition(|key| key.frame < last) {
                        let left = clip.keyframes[index];
                        let right = clip.keyframes[index + 1];
                        let t = (last - left.frame) as f64 / (right.frame - left.frame) as f64;
                        clip.keyframes[index].easing = left.easing.split(t).0;
                    }
                    clip.keyframes.retain(|key| key.frame < last);
                    clip.keyframes.push(boundary);
                }
                clip.clamp_fades();
                self.execute(Command::ReplaceTracks(tracks))
            }
            Command::SetSubtitleStyle { track_id, style } => {
                if !style.valid() {
                    return None;
                }
                let mut tracks = self.project.tracks.clone();
                let track = tracks
                    .iter_mut()
                    .find(|track| track.id == track_id && track.subtitles)?;
                for clip in &mut track.clips {
                    if let ClipContent::Title(title) = &mut clip.content {
                        *title = Title {
                            text: title.text.clone(),
                            parse_html: title.parse_html,
                            parse_markdown: title.parse_markdown,
                            ..style.clone()
                        };
                    }
                }
                self.execute(Command::ReplaceTracks(tracks))
            }
            Command::SetClipNoiseReduction {
                track_id,
                clip_id,
                enabled,
            } => {
                let track = self
                    .project
                    .tracks
                    .iter_mut()
                    .find(|track| track.id == track_id)?;
                let clip = track.clips.iter_mut().find(|clip| clip.id == clip_id)?;
                if !(track.kind == TrackKind::Audio || clip.linked_audio)
                    || clip.noise_reduction == enabled
                {
                    return None;
                }
                Some(Command::SetClipNoiseReduction {
                    track_id,
                    clip_id,
                    enabled: std::mem::replace(&mut clip.noise_reduction, enabled),
                })
            }
            Command::SetClipLut {
                track_id,
                clip_id,
                path,
            } => {
                let track = self
                    .project
                    .tracks
                    .iter_mut()
                    .find(|track| track.id == track_id)?;
                if track.kind != TrackKind::Video {
                    return None;
                }
                let clip = track.clips.iter_mut().find(|clip| clip.id == clip_id)?;
                if !matches!(clip.content, ClipContent::Media { .. }) || clip.lut == path {
                    return None;
                }
                Some(Command::SetClipLut {
                    track_id,
                    clip_id,
                    path: std::mem::replace(&mut clip.lut, path),
                })
            }
            Command::SetTransitionOut {
                track_id,
                clip_id,
                transition,
            } => {
                if transition
                    .as_ref()
                    .is_some_and(|value| value.duration_frames == 0)
                {
                    return None;
                }
                let track = self
                    .project
                    .tracks
                    .iter_mut()
                    .find(|track| track.id == track_id)?;
                if track.kind != TrackKind::Video {
                    return None;
                }
                let clip = track.clips.iter_mut().find(|clip| clip.id == clip_id)?;
                if clip.transition_out == transition {
                    return None;
                }
                Some(Command::SetTransitionOut {
                    track_id,
                    clip_id,
                    transition: std::mem::replace(&mut clip.transition_out, transition),
                })
            }
            Command::SetTransition {
                track_id,
                clip_id,
                transition,
            } => {
                if transition
                    .as_ref()
                    .is_some_and(|value| value.duration_frames == 0)
                {
                    return None;
                }
                let track = self
                    .project
                    .tracks
                    .iter_mut()
                    .find(|track| track.id == track_id)?;
                if track.kind != TrackKind::Video {
                    return None;
                }
                let clip = track.clips.iter_mut().find(|clip| clip.id == clip_id)?;
                if clip.transition == transition {
                    return None;
                }
                Some(Command::SetTransition {
                    track_id,
                    clip_id,
                    transition: std::mem::replace(&mut clip.transition, transition),
                })
            }
            Command::SetTitle {
                track_id,
                clip_id,
                title,
            } => {
                if !title.valid() {
                    return None;
                }
                let clip = self
                    .project
                    .tracks
                    .iter_mut()
                    .find(|track| track.id == track_id)?
                    .clips
                    .iter_mut()
                    .find(|clip| clip.id == clip_id)?;
                let ClipContent::Title(current) = &mut clip.content else {
                    return None;
                };
                if *current == title {
                    return None;
                }
                Some(Command::SetTitle {
                    track_id,
                    clip_id,
                    title: std::mem::replace(current, title),
                })
            }
            Command::RippleDelete { track_id, clip_id } => {
                let tracks = ripple_delete(&self.project, &[(track_id, clip_id)])?;
                self.execute(Command::ReplaceTracks(tracks))
            }
            Command::RippleDeleteSelection(selection) => {
                let tracks = ripple_delete(&self.project, &selection)?;
                self.execute(Command::ReplaceTracks(tracks))
            }
            Command::RemoveSilences(ranges) => {
                let tracks = ripple_ranges(&self.project, ranges, &HashSet::new())?;
                self.execute(Command::ReplaceTracks(tracks))
            }
        }
    }

    pub fn apply(&mut self, command: Command) -> bool {
        let Some(inverse) = self.execute(command) else {
            return false;
        };
        self.undo.push(HistoryEntry {
            command: inverse,
            revision: self.revision,
        });
        self.redo.clear();
        self.revision = self.next_revision;
        self.next_revision += 1;
        true
    }

    pub fn undo(&mut self) -> bool {
        let Some(entry) = self.undo.pop() else {
            return false;
        };
        if let Some(inverse) = self.execute(entry.command) {
            self.redo.push(HistoryEntry {
                command: inverse,
                revision: self.revision,
            });
            self.revision = entry.revision;
        }
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(entry) = self.redo.pop() else {
            return false;
        };
        if let Some(inverse) = self.execute(entry.command) {
            self.undo.push(HistoryEntry {
                command: inverse,
                revision: self.revision,
            });
            self.revision = entry.revision;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn subtitle_clips_only_support_forward_playback() {
        let project = super::Project::new("Captions");
        let cues = crate::subtitles::parse("1\n00:00:00,000 --> 00:00:01,000\nFirst").unwrap();
        let track = crate::subtitles::track(&project, &cues).unwrap();
        let track_id = track.id;
        let clip_id = track.clips[0].id;
        assert_eq!(track.clips[0].playback, super::Playback::Forward);
        let mut tracks = project.tracks().to_vec();
        tracks.push(track);
        let mut editor = super::Editor::new(project);
        assert!(editor.apply(super::Command::ReplaceTracks(tracks)));
        let before = editor.project().clone();
        for playback in [super::Playback::Reverse, super::Playback::Freeze] {
            assert!(!editor.apply(super::Command::SetPlayback {
                track_id,
                clip_id,
                playback
            }));
            let mut invalid = before.clone();
            let clip = &mut invalid
                .tracks
                .iter_mut()
                .find(|track| track.id == track_id)
                .unwrap()
                .clips[0];
            clip.playback = playback;
            let mut inserted = clip.clone();
            inserted.id += 1;
            inserted.start_frame = 60;
            assert!(invalid.validate().is_err());
            assert!(!editor.apply(super::Command::InsertClip {
                track_id,
                clip: inserted
            }));
            assert!(!editor.apply(super::Command::ReplaceTracks(invalid.tracks)));
            assert_eq!(editor.project(), &before);
        }
        assert!(super::freeze_at(editor.project(), track_id, clip_id, 0).is_none());
        assert!(editor.undo());
        assert!(editor.redo());
        assert_eq!(editor.project(), &before);
    }

    #[test]
    fn subtitle_style_changes_a_whole_track_in_one_undo_step() {
        let project = super::Project::new("Captions");
        let cues = crate::subtitles::parse("1\n00:00:00,000 --> 00:00:01,000\n<i>First</i>\n\n2\n00:00:01,000 --> 00:00:02,000\nSecond").unwrap();
        let first = crate::subtitles::track(&project, &cues).unwrap();
        let id = first.id;
        let mut tracks = project.tracks().to_vec();
        tracks.push(first);
        let mut editor = super::Editor::new(project);
        assert!(editor.apply(super::Command::ReplaceTracks(tracks)));
        let second = crate::subtitles::track(editor.project(), &cues).unwrap();
        let empty_id = second.id + 1;
        let mut tracks = editor.project().tracks().to_vec();
        tracks.push(second);
        tracks.push(crate::timeline::Track {
            name: String::new(),
            locked: false,
            hidden: false,
            id: empty_id,
            kind: crate::timeline::TrackKind::Video,
            subtitles: true,
            clips: Vec::new(),
        });
        assert!(editor.apply(super::Command::ReplaceTracks(tracks)));
        let before = editor.project().clone();
        let style = crate::timeline::Title {
            font_family: "Serif".into(),
            font_size: 55.0,
            color: [255, 210, 0, 255],
            background: Some([0, 0, 0, 179]),
            outline: true,
            y: 0.1,
            ..Default::default()
        };
        assert!(editor.apply(super::Command::SetSubtitleStyle {
            track_id: id,
            style: style.clone()
        }));
        let after = editor.project().clone();
        for (old_track, new_track) in before.tracks().iter().zip(after.tracks()) {
            if old_track.id != id {
                assert_eq!(old_track, new_track);
                continue;
            }
            for (old, new) in old_track.clips.iter().zip(&new_track.clips) {
                let crate::timeline::ClipContent::Title(old_title) = &old.content else {
                    unreachable!()
                };
                let crate::timeline::ClipContent::Title(new_title) = &new.content else {
                    unreachable!()
                };
                assert_eq!(new_title.text, old_title.text);
                assert_eq!(new_title.parse_html, old_title.parse_html);
                assert_eq!(new_title.font_size, 55.0);
                assert_eq!(new_title.background, style.background);
                assert_eq!(new_title.y, 0.1);
                let mut expected = old.clone();
                expected.content = new.content.clone();
                assert_eq!(&expected, new);
            }
        }
        assert!(editor.undo());
        assert_eq!(editor.project(), &before);
        assert!(editor.redo());
        assert_eq!(editor.project(), &after);
        assert!(!editor.apply(super::Command::SetSubtitleStyle {
            track_id: empty_id,
            style: style.clone()
        }));
        assert!(!editor.apply(super::Command::SetSubtitleStyle {
            track_id: 0,
            style: style.clone()
        }));
        assert!(!editor.apply(super::Command::SetSubtitleStyle {
            track_id: id,
            style: crate::timeline::Title {
                font_size: -1.0,
                ..style
            }
        }));
    }

    #[test]
    fn track_metadata_and_order_are_validated_and_undoable() {
        let original = super::Project::new("Tracks");
        let mut editor = super::Editor::new(original.clone());
        use super::Command;
        assert!(editor.apply(Command::RenameTrack {
            track_id: 0,
            name: "  Main picture  ".into()
        }));
        assert_eq!(editor.project().tracks()[0].name, "Main picture");
        assert!(!editor.apply(Command::RenameTrack {
            track_id: 0,
            name: "x".repeat(33)
        }));
        assert!(!editor.apply(Command::RenameTrack {
            track_id: 99,
            name: "Missing".into()
        }));
        assert!(editor.apply(Command::SetTrackLocked {
            track_id: 0,
            locked: true
        }));
        assert!(editor.apply(Command::SetTrackHidden {
            track_id: 0,
            hidden: true
        }));
        assert!(!editor.apply(Command::MoveTrack {
            track_id: 0,
            down: false
        }));
        assert!(editor.apply(Command::MoveTrack {
            track_id: 0,
            down: true
        }));
        assert_eq!(editor.project().tracks()[1].id, 0);
        for _ in 0..4 {
            assert!(editor.undo());
        }
        assert_eq!(editor.project(), &original);
        for _ in 0..4 {
            assert!(editor.redo());
        }
        assert!(editor.project().tracks()[1].locked);
        assert!(editor.apply(Command::RenameTrack {
            track_id: 0,
            name: String::new()
        }));
        assert_eq!(editor.project().tracks()[1].display_name("V2"), "V2");
    }

    #[test]
    fn locked_tracks_refuse_clip_edits_and_survive_ripple_delete() {
        use super::{Command, Editor, Project};
        use crate::timeline::{Clip, ClipContent, Title};
        let mut editor = Editor::new(Project::new("Locked"));
        let clip = Clip {
            speed: 1.0,
            playback: Playback::Forward,
            lut: None,
            color: ColorAdjust::default(),
            transition: None,
            transition_out: None,
            id: 1,
            start_frame: 0,
            source_start_frame: 0,
            duration_frames: 90,
            content: ClipContent::Title(Title::default()),
            volume: 1.0,
            fade_in_frames: 0,
            fade_out_frames: 0,
            muted: false,
            noise_reduction: false,
            linked_audio: false,
            keyframes: Vec::new(),
            transform: Transform::default(),
        };
        assert!(editor.apply(Command::InsertClip {
            track_id: 0,
            clip: clip.clone()
        }));
        assert!(editor.apply(Command::InsertClip {
            track_id: 1,
            clip: Clip {
                id: 2,
                ..clip.clone()
            }
        }));
        assert!(editor.apply(Command::SetTrackLocked {
            track_id: 0,
            locked: true
        }));
        let locked = editor.project().clone();
        for command in [
            Command::InsertClip {
                track_id: 0,
                clip: Clip {
                    id: 3,
                    start_frame: 90,
                    ..clip.clone()
                },
            },
            Command::RemoveClip {
                track_id: 0,
                clip_id: 1,
            },
            Command::Split {
                track_id: 0,
                clip_id: 1,
                at_frame: 30,
                new_id: 3,
            },
            Command::Move {
                track_id: 0,
                target_track_id: 1,
                clip_id: 1,
                to_frame: 90,
            },
            Command::Move {
                track_id: 1,
                target_track_id: 0,
                clip_id: 2,
                to_frame: 90,
            },
            Command::Trim {
                track_id: 0,
                clip_id: 1,
                start_frame: 0,
                source_start_frame: 0,
                duration_frames: 30,
            },
            Command::SetTitle {
                track_id: 0,
                clip_id: 1,
                title: Title {
                    text: "Changed".into(),
                    ..Title::default()
                },
            },
            Command::SetClipAudio {
                track_id: 0,
                clip_id: 1,
                volume: 0.5,
                muted: true,
            },
            Command::SetClipFades {
                track_id: 0,
                clip_id: 1,
                fade_in_frames: 10,
                fade_out_frames: 10,
            },
            Command::SetTransition {
                track_id: 0,
                clip_id: 1,
                transition: Some(crate::timeline::Transition {
                    kind: crate::timeline::TransitionKind::Fade,
                    duration_frames: 10,
                    direction: Default::default(),
                }),
            },
            Command::SetTransitionOut {
                track_id: 0,
                clip_id: 1,
                transition: Some(crate::timeline::Transition {
                    kind: crate::timeline::TransitionKind::Fade,
                    duration_frames: 10,
                    direction: Default::default(),
                }),
            },
            Command::RippleDelete {
                track_id: 0,
                clip_id: 1,
            },
        ] {
            assert!(!editor.apply(command));
            assert_eq!(editor.project(), &locked);
        }
        let mut tracks = locked.tracks().to_vec();
        tracks[0].clips.clear();
        assert!(!editor.apply(Command::ReplaceTracks(tracks)));
        let mut tracks = locked.tracks().to_vec();
        tracks.remove(0);
        assert!(!editor.apply(Command::ReplaceTracks(tracks)));
        assert!(editor.apply(Command::RippleDelete {
            track_id: 1,
            clip_id: 2
        }));
        assert_eq!(editor.project().tracks()[0], locked.tracks()[0]);
        assert!(editor.undo());
        assert_eq!(editor.project(), &locked);
    }
    use super::*;

    fn speed_project() -> Project {
        let mut value = serde_json::to_value(Project::new("Speed")).unwrap();
        value["media"] = serde_json::json!([{
            "id": 1, "path": "video.mp4", "kind": "video", "duration_us": 10_000_000,
            "has_audio": true
        }]);
        value["tracks"][0]["clips"] = serde_json::json!([{
            "id": 1, "start_frame": 10, "source_start_frame": 30, "duration_frames": 60,
            "content": {"media": {"media_id": 1}}, "volume": 1.0, "muted": false,
            "linked_audio": true
        }]);
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn silence_ranges_keep_linked_audio_speed_reverse_and_history() {
        for playback in [Playback::Forward, Playback::Reverse] {
            let mut project = speed_project();
            project.tracks[0].clips[0].speed = 2.0;
            project.tracks[0].clips[0].playback = playback;
            let original = project.tracks[0].clips[0].clone();
            let mut later = original.clone();
            later.id = 2;
            later.start_frame = 80;
            project.tracks[0].clips.push(later);
            let before = project.tracks.clone();
            let mut editor = Editor::new(project);
            assert!(!editor.apply(Command::RemoveSilences(vec![])));
            assert!(!editor.apply(Command::RemoveSilences(vec![(30, 20)])));
            assert!(editor.apply(Command::RemoveSilences(vec![(30, 40)])));
            let pieces = &editor.project().tracks()[0].clips;
            assert_eq!(pieces.len(), 3);
            assert_eq!(pieces[0].duration_frames, 20);
            assert_eq!(pieces[1].start_frame, 30);
            assert_eq!(pieces[2].start_frame, 70);
            assert!(pieces.iter().all(|clip| clip.linked_audio));
            assert_eq!(
                pieces[0].source_start_frame,
                original.source_start_for_range(0, 20).unwrap()
            );
            assert_eq!(
                pieces[1].source_start_frame,
                original.source_start_for_range(30, 30).unwrap()
            );
            let after = editor.project().tracks().to_vec();
            assert!(editor.undo());
            assert_eq!(editor.project().tracks(), before);
            assert!(editor.redo());
            assert_eq!(editor.project().tracks(), after);
        }
    }

    #[test]
    fn speed_changes_bound_keys_and_preserve_visible_animation_with_undo() {
        use crate::timeline::{Keyframe, Transform};
        for playback in [Playback::Forward, Playback::Reverse] {
            let mut project = speed_project();
            let clip = &mut project.tracks[0].clips[0];
            clip.playback = playback;
            clip.keyframes = vec![
                Keyframe::new(0, Transform::default()),
                Keyframe::new(
                    59,
                    Transform {
                        x: 1.0,
                        ..Default::default()
                    },
                ),
            ];
            let original = clip.clone();
            let mut editor = Editor::new(project);
            assert!(editor.apply(Command::SetClipSpeed {
                track_id: 0,
                clip_id: 1,
                speed: 2.0
            }));
            let faster = editor.project().tracks()[0].clips[0].clone();
            assert_eq!(faster.duration_frames, 30);
            assert_eq!(faster.keyframes.last().unwrap().frame, 29);
            assert!((faster.transform_at(25).x - original.transform_at(25).x).abs() < 0.00001);
            assert!(editor.undo());
            assert_eq!(editor.project().tracks()[0].clips[0], original);
            assert!(editor.redo());
            assert_eq!(editor.project().tracks()[0].clips[0], faster);
            assert!(editor.apply(Command::SetClipSpeed {
                track_id: 0,
                clip_id: 1,
                speed: 0.5
            }));
            let slower = &editor.project().tracks()[0].clips[0];
            assert_eq!(slower.duration_frames, 120);
            assert_eq!(slower.keyframes, faster.keyframes);
            assert!(!editor.apply(Command::SetClipSpeed {
                track_id: 0,
                clip_id: 1,
                speed: 0.0
            }));
        }
    }

    #[test]
    fn reverse_speed_split_trim_ripple_and_gap_keep_playback_source_ranges() {
        for speed in [0.5, 2.0] {
            let mut editor = Editor::new(speed_project());
            assert!(editor.apply(Command::SetPlayback {
                track_id: 0,
                clip_id: 1,
                playback: Playback::Reverse,
            }));
            assert!(editor.apply(Command::SetClipSpeed {
                track_id: 0,
                clip_id: 1,
                speed
            }));
            let before = editor.project().clone();
            let original = before.tracks()[0].clips[0].clone();
            let frames: Vec<_> = (0..original.duration_frames)
                .map(|offset| original.source_frame_at(offset).unwrap())
                .collect();
            assert_eq!(frames[0], 89);
            assert_eq!(original.source_frame_at(original.duration_frames), None);
            assert!(
                original
                    .source_start_for_range(original.duration_frames, 1)
                    .is_none()
            );
            assert!(editor.apply(Command::Split {
                track_id: 0,
                clip_id: 1,
                at_frame: original.start_frame + original.duration_frames / 2,
                new_id: 2,
            }));
            let split_frames: Vec<_> = editor.project().tracks()[0]
                .clips
                .iter()
                .flat_map(|clip| {
                    (0..clip.duration_frames).map(|offset| clip.source_frame_at(offset).unwrap())
                })
                .collect();
            assert_eq!(split_frames, frames);
            assert!(editor.undo());
            assert_eq!(editor.project(), &before);
            for left in [false, true] {
                let frame = if left {
                    original.start_frame + 4
                } else {
                    original.start_frame + original.duration_frames - 4
                };
                assert!(editor.apply(trim_edge(editor.project(), 0, 1, left, frame).unwrap()));
                let clip = &editor.project().tracks()[0].clips[0];
                let trimmed: Vec<_> = (0..clip.duration_frames)
                    .map(|offset| clip.source_frame_at(offset).unwrap())
                    .collect();
                assert_eq!(
                    trimmed,
                    if left {
                        &frames[4..]
                    } else {
                        &frames[..frames.len() - 4]
                    }
                );
                assert!(editor.undo());
                assert_eq!(editor.project(), &before);
            }
            let mut tracks = before.tracks().to_vec();
            let mut cut = original.clone();
            cut.id = 99;
            cut.start_frame += 4;
            cut.duration_frames = 4;
            tracks[1].clips.push(cut);
            assert!(editor.apply(Command::ReplaceTracks(tracks)));
            assert!(editor.apply(Command::RippleDelete {
                track_id: 1,
                clip_id: 99
            }));
            let remaining: Vec<_> = editor.project().tracks()[0]
                .clips
                .iter()
                .flat_map(|clip| {
                    (0..clip.duration_frames).map(|offset| clip.source_frame_at(offset).unwrap())
                })
                .collect();
            assert_eq!(remaining, [&frames[..4], &frames[8..]].concat());
            assert!(editor.undo());
            assert!(editor.undo());
            let mut tracks = before.tracks().to_vec();
            let mut next = original.clone();
            next.id = 2;
            next.start_frame += original.duration_frames;
            tracks[0].clips.push(next);
            assert!(editor.apply(Command::ReplaceTracks(tracks)));
            assert!(editor.apply(Command::SetClipSpeed {
                track_id: 0,
                clip_id: 1,
                speed: speed / 2.0
            }));
            let clamped = &editor.project().tracks()[0].clips[0];
            assert_eq!(clamped.duration_frames, original.duration_frames);
            assert_eq!(clamped.source_frame_at(0), Some(89));
            assert_eq!(clamped.source_start_frame + clamped.source_span(), 90);
            editor.project().validate().unwrap();
            let freeze = freeze_at(editor.project(), 0, 1, clamped.start_frame + 4).unwrap();
            assert!(editor.apply(freeze));
            assert_eq!(editor.project().tracks()[0].clips[1].speed, 1.0);
        }
    }

    #[test]
    fn media_speed_keeps_subtitle_timeline_timing_and_export_unchanged() {
        let mut editor = Editor::new(speed_project());
        let cues = crate::subtitles::parse("1\n00:00:00,500 --> 00:00:01,500\nCaption\n").unwrap();
        let subtitles = crate::subtitles::track(editor.project(), &cues).unwrap();
        assert!(subtitles.clips.iter().all(|clip| clip.speed == 1.0));
        let mut tracks = editor.project().tracks().to_vec();
        tracks.push(subtitles.clone());
        assert!(editor.apply(Command::ReplaceTracks(tracks)));
        let before = editor.project().clone();
        let srt = crate::subtitles::write(&before).unwrap();
        assert!(editor.apply(Command::SetClipSpeed {
            track_id: 0,
            clip_id: 1,
            speed: 2.0,
        }));
        assert_eq!(editor.project().tracks().last().unwrap(), &subtitles);
        assert_eq!(crate::subtitles::write(editor.project()).unwrap(), srt);
        assert!(!editor.apply(Command::SetClipSpeed {
            track_id: subtitles.id,
            clip_id: subtitles.clips[0].id,
            speed: 2.0,
        }));
        assert!(editor.undo());
        assert_eq!(editor.project(), &before);
    }

    #[test]
    fn speed_availability_matches_commands_without_changing_history() {
        let original = speed_project();
        let editor = Editor::new(original.clone());
        for (track_id, clip_id, speed) in [
            (0, 1, 0.25),
            (0, 1, 2.0),
            (0, 1, 1.0),
            (0, 1, 0.0),
            (0, 1, f64::NAN),
            (0, 99, 2.0),
            (99, 1, 2.0),
        ] {
            let mut trial = Editor::new(original.clone());
            assert_eq!(
                editor.can_set_clip_speed(track_id, clip_id, speed),
                trial.apply(Command::SetClipSpeed {
                    track_id,
                    clip_id,
                    speed
                }),
            );
        }
        assert_eq!(editor.project(), &original);
        assert!(!editor.can_undo());
        assert!(!editor.can_redo());
    }

    #[test]
    fn speed_preserves_source_span_and_undo_restores_exact_duration() {
        for (speed, duration) in [(0.25, 240), (0.5, 120), (1.5, 40), (2.0, 30), (4.0, 15)] {
            let original = speed_project();
            let mut editor = Editor::new(original.clone());
            assert!(editor.apply(Command::SetClipSpeed {
                track_id: 0,
                clip_id: 1,
                speed
            }));
            let clip = &editor.project().tracks()[0].clips[0];
            assert_eq!(
                (
                    clip.start_frame,
                    clip.source_start_frame,
                    clip.duration_frames
                ),
                (10, 30, duration)
            );
            assert_eq!(clip.source_span(), 60);
            assert!(clip.linked_audio);
            assert!(editor.undo());
            assert_eq!(editor.project(), &original);
            assert!(editor.redo());
            assert_eq!(editor.project().tracks()[0].clips[0].speed, speed);
        }
    }

    #[test]
    fn speed_keeps_transitions_and_clamps_timeline_fades_with_undo() {
        let mut original = speed_project();
        let clip = &mut original.tracks[0].clips[0];
        clip.lut = Some("look.cube".into());
        clip.color = ColorAdjust {
            brightness: 0.25,
            contrast: 1.5,
            saturation: 0.5,
        };
        clip.fade_in_frames = 20;
        clip.fade_out_frames = 30;
        clip.transition = Some(Transition {
            kind: crate::timeline::TransitionKind::Dissolve,
            duration_frames: 10,
            direction: crate::timeline::SlideDirection::Left,
        });
        clip.transition_out = Some(Transition {
            kind: crate::timeline::TransitionKind::Slide,
            duration_frames: 10,
            direction: crate::timeline::SlideDirection::Down,
        });
        for (speed, fades) in [(0.5, (20, 30)), (2.0, (20, 10)), (4.0, (15, 0))] {
            let mut editor = Editor::new(original.clone());
            assert!(editor.apply(Command::SetClipSpeed {
                track_id: 0,
                clip_id: 1,
                speed
            }));
            let clip = &editor.project().tracks()[0].clips[0];
            assert_eq!((clip.fade_in_frames, clip.fade_out_frames), fades);
            assert_eq!(clip.transition, original.tracks[0].clips[0].transition);
            assert_eq!(
                clip.transition_out,
                original.tracks[0].clips[0].transition_out
            );
            assert_eq!(clip.color, original.tracks[0].clips[0].color);
            assert_eq!(clip.lut, original.tracks[0].clips[0].lut);
            editor.project().validate().unwrap();
            assert!(editor.undo());
            assert_eq!(editor.project(), &original);
        }
    }

    #[test]
    fn locked_tracks_reject_speed_changes() {
        let mut editor = Editor::new(speed_project());
        assert!(editor.apply(Command::SetTrackLocked {
            track_id: 0,
            locked: true,
        }));
        let locked = editor.project().clone();
        assert!(!editor.apply(Command::SetClipSpeed {
            track_id: 0,
            clip_id: 1,
            speed: 2.0,
        }));
        assert_eq!(editor.project(), &locked);
        assert!(editor.undo());
        assert!(editor.apply(Command::SetClipSpeed {
            track_id: 0,
            clip_id: 1,
            speed: 2.0,
        }));
    }

    #[test]
    fn speed_clamps_to_gap_and_rejects_invalid_or_empty_targets() {
        let mut project = speed_project();
        let mut next = project.tracks[0].clips[0].clone();
        next.id = 2;
        next.start_frame = 100;
        project.tracks[0].clips.push(next);
        let mut editor = Editor::new(project.clone());
        for speed in [0.0, -1.0, 0.249, 4.001, f64::NAN, f64::INFINITY, 1.0] {
            assert!(!editor.apply(Command::SetClipSpeed {
                track_id: 0,
                clip_id: 1,
                speed
            }));
            assert_eq!(editor.project(), &project);
        }
        assert!(!editor.apply(Command::SetClipSpeed {
            track_id: 0,
            clip_id: 99,
            speed: 2.0
        }));
        assert!(editor.apply(Command::SetClipSpeed {
            track_id: 0,
            clip_id: 1,
            speed: 0.5
        }));
        assert_eq!(editor.project().tracks()[0].clips[0].duration_frames, 90);
        assert!(editor.undo());
        assert_eq!(editor.project(), &project);
    }

    #[test]
    fn speed_rounds_duration_without_extending_past_media_end() {
        let mut project = speed_project();
        project.tracks[0].clips[0].duration_frames = 31;
        let mut editor = Editor::new(project.clone());
        assert!(editor.apply(Command::SetClipSpeed {
            track_id: 0,
            clip_id: 1,
            speed: 2.0
        }));
        assert_eq!(editor.project().tracks()[0].clips[0].duration_frames, 16);
        project.tracks[0].clips[0].source_start_frame = 270;
        project.tracks[0].clips[0].duration_frames = 30;
        let mut editor = Editor::new(project);
        assert!(editor.apply(Command::SetClipSpeed {
            track_id: 0,
            clip_id: 1,
            speed: 4.0
        }));
        assert_eq!(editor.project().tracks()[0].clips[0].duration_frames, 7);
        assert!(editor.project().validate().is_ok());
    }

    #[test]
    fn split_and_both_trim_edges_use_source_speed() {
        let mut project = speed_project();
        project.tracks[0].clips[0].speed = 2.0;
        let tracks = split_tracks(project.tracks(), &[(0, 1)], 25, 2).unwrap();
        assert_eq!(tracks[0].clips[1].source_start_frame, 60);
        let mut editor = Editor::new(project.clone());
        let command = trim_edge(&project, 0, 1, true, 20).unwrap();
        assert!(editor.apply(command));
        let clip = &editor.project().tracks()[0].clips[0];
        assert_eq!((clip.source_start_frame, clip.duration_frames), (50, 50));
        assert!(editor.undo());
        assert!(editor.apply(trim_edge(&project, 0, 1, true, 0).unwrap()));
        assert_eq!(editor.project().tracks()[0].clips[0].source_start_frame, 10);
        assert!(editor.undo());
        assert!(editor.apply(trim_edge(&project, 0, 1, false, 1000).unwrap()));
        assert_eq!(editor.project().tracks()[0].clips[0].duration_frames, 135);
    }

    #[test]
    fn speed_format_round_trip_legacy_and_validation() {
        let mut project = speed_project();
        let legacy = super::super::format::to_json(&project).unwrap();
        assert!(!legacy.contains("\"speed\""));
        assert_eq!(super::super::format::from_json(&legacy).unwrap(), project);
        for speed in [0.25, 0.5, 1.5, 2.0, 4.0] {
            project.tracks[0].clips[0].speed = speed;
            let json = super::super::format::to_json(&project).unwrap();
            assert_eq!(super::super::format::from_json(&json).unwrap(), project);
        }
        for speed in [0.0, 4.01] {
            project.tracks[0].clips[0].speed = speed;
            assert!(
                super::super::format::from_json(&super::super::format::to_json(&project).unwrap())
                    .is_err()
            );
        }
        project.tracks[0].clips[0].speed = 2.0;
        project.media[0].kind = MediaKind::Image;
        assert!(project.validate().is_err());
        project.tracks[0].clips[0].content = ClipContent::Title(Title::default());
        assert!(project.validate().is_err());
    }

    #[test]
    fn ripple_delete_preserves_sped_up_source_ranges() {
        let mut project = speed_project();
        project.tracks[0].clips[0].speed = 2.0;
        let mut removed = project.tracks[0].clips[0].clone();
        removed.id = 2;
        removed.start_frame = 25;
        removed.duration_frames = 10;
        project.tracks[1].clips.push(removed);
        let mut editor = Editor::new(project.clone());
        assert!(editor.apply(Command::RippleDelete {
            track_id: 1,
            clip_id: 2
        }));
        let clips = &editor.project().tracks()[0].clips;
        assert_eq!(clips.len(), 2);
        assert_eq!(
            (
                clips[0].duration_frames,
                clips[1].start_frame,
                clips[1].source_start_frame
            ),
            (15, 25, 80)
        );
        assert_eq!(clips[1].speed, 2.0);
        assert!(editor.undo());
        assert_eq!(editor.project(), &project);
    }

    #[test]
    fn locked_tracks_reject_clip_color_changes() {
        let mut editor = Editor::new(Project::new("Locked color"));
        assert!(editor.apply(Command::ImportMedia(Media {
            id: 1,
            path: "/color.png".into(),
            kind: MediaKind::Image,
            duration_us: None,
            has_audio: false,
        })));
        let clip = serde_json::from_value(serde_json::json!({
            "id": 1, "start_frame": 0, "source_start_frame": 0,
            "duration_frames": 30, "content": {"media": {"media_id": 1}},
            "volume": 1.0, "muted": false
        }))
        .unwrap();
        assert!(editor.apply(Command::InsertClip { track_id: 0, clip }));
        let command = Command::SetClipColor {
            track_id: 0,
            clip_id: 1,
            color: ColorAdjust {
                brightness: 0.2,
                contrast: 1.3,
                saturation: 0.5,
            },
        };
        assert!(editor.apply(command.clone()));
        assert!(editor.undo());
        assert!(editor.apply(Command::SetTrackLocked {
            track_id: 0,
            locked: true
        }));
        let locked = editor.project().clone();
        assert!(!editor.apply(command.clone()));
        assert_eq!(editor.project(), &locked);
        assert!(editor.undo());
        assert!(editor.apply(command));
    }

    #[test]
    fn clip_color_command_rejects_missing_clip() {
        let mut editor = Editor::new(Project::new("Color"));
        assert!(!editor.apply(Command::SetClipColor {
            track_id: 0,
            clip_id: 1,
            color: ColorAdjust::default()
        }));
    }

    #[test]
    fn clip_color_validation_undo_and_serialization() {
        let mut editor = Editor::new(Project::new("Color"));
        assert!(editor.apply(Command::ImportMedia(Media {
            id: 1,
            path: "/color.png".into(),
            kind: MediaKind::Image,
            duration_us: None,
            has_audio: false
        })));
        let clip = Clip {
            playback: Playback::Forward,
            id: 1,
            start_frame: 0,
            source_start_frame: 0,
            duration_frames: 30,
            content: ClipContent::Media { media_id: 1 },
            speed: 1.0,
            lut: None,
            volume: 1.0,
            muted: false,
            noise_reduction: false,
            linked_audio: false,
            color: ColorAdjust::default(),
            transition: None,
            transition_out: None,
            fade_in_frames: 0,
            fade_out_frames: 0,
            keyframes: Vec::new(),
            transform: Transform::default(),
        };
        assert!(editor.apply(Command::InsertClip {
            track_id: 0,
            clip: clip.clone()
        }));
        let json = crate::project::format::to_json(editor.project()).unwrap();
        assert!(!json.contains("\"brightness\""));
        assert_eq!(
            crate::project::format::to_json(&crate::project::format::from_json(&json).unwrap())
                .unwrap(),
            json
        );
        let command = |color| Command::SetClipColor {
            track_id: 0,
            clip_id: 1,
            color,
        };
        assert!(!editor.apply(command(ColorAdjust::default())));
        for color in [
            ColorAdjust {
                brightness: f32::NAN,
                ..Default::default()
            },
            ColorAdjust {
                brightness: 1.1,
                ..Default::default()
            },
            ColorAdjust {
                contrast: -0.1,
                ..Default::default()
            },
            ColorAdjust {
                saturation: 2.1,
                ..Default::default()
            },
        ] {
            assert!(!editor.apply(command(color)));
            let mut invalid = clip.clone();
            invalid.id = 2;
            invalid.start_frame = 30;
            invalid.color = color;
            assert!(!editor.apply(Command::InsertClip {
                track_id: 0,
                clip: invalid
            }));
        }
        let color = ColorAdjust {
            brightness: 0.2,
            contrast: 1.3,
            saturation: 0.0,
        };
        assert!(editor.apply(command(color)));
        assert!(!editor.apply(command(color)));
        let json = crate::project::format::to_json(editor.project()).unwrap();
        assert_eq!(
            crate::project::format::from_json(&json).unwrap().tracks()[0].clips[0].color,
            color
        );
        let invalid = json.replace("\"brightness\": 0.2", "\"brightness\": 2.0");
        assert!(crate::project::format::from_json(&invalid).is_err());
        assert!(editor.undo());
        assert!(editor.project().tracks()[0].clips[0].color.neutral());
        assert!(editor.redo());
        assert_eq!(editor.project().tracks()[0].clips[0].color, color);
        let split = split_tracks(editor.project().tracks(), &[(0, 1)], 15, 10).unwrap();
        assert!(split[0].clips.iter().all(|clip| clip.color == color));
        assert!(editor.apply(Command::InsertClip {
            track_id: 1,
            clip: Clip {
                id: 2,
                content: ClipContent::Title(Title::default()),
                ..clip.clone()
            }
        }));
        assert!(!editor.apply(Command::SetClipColor {
            track_id: 1,
            clip_id: 2,
            color
        }));
        assert!(editor.apply(Command::ImportMedia(Media {
            id: 2,
            path: "/sound.wav".into(),
            kind: MediaKind::Audio,
            duration_us: Some(1_000_000),
            has_audio: true
        })));
        assert!(editor.apply(Command::InsertClip {
            track_id: 2,
            clip: Clip {
                id: 3,
                content: ClipContent::Media { media_id: 2 },
                ..clip
            }
        }));
        assert!(!editor.apply(Command::SetClipColor {
            track_id: 2,
            clip_id: 3,
            color
        }));
    }

    #[test]
    fn saved_checkpoint_survives_undo_redo_but_not_a_diverging_branch() {
        let mut editor = Editor::new(Project::new("Untitled"));
        assert!(!editor.is_dirty());
        assert!(editor.apply(Command::RenameProject("Saved".into())));
        let saved = editor.revision();
        editor.mark_saved(saved);
        assert!(!editor.is_dirty());
        assert!(editor.apply(Command::RenameProject("Later".into())));
        assert!(editor.is_dirty());
        assert!(editor.undo());
        assert!(!editor.is_dirty());
        assert!(editor.undo());
        assert!(editor.is_dirty());
        assert!(editor.redo());
        assert!(!editor.is_dirty());
        assert!(editor.redo());
        assert!(editor.is_dirty());
        assert!(editor.undo());
        assert!(editor.undo());

        assert!(editor.apply(Command::RenameProject("Different branch".into())));
        assert_ne!(editor.revision(), saved);
        assert!(editor.is_dirty());
        assert!(!editor.redo());
        assert!(editor.undo());
        assert!(editor.is_dirty());
        assert!(editor.redo());
        assert!(editor.is_dirty());
    }

    #[test]
    fn markers_stay_sorted_unique_and_undoable() {
        let mut editor = Editor::new(Project::new("Untitled"));
        assert!(editor.apply(Command::AddMarker(90)));
        assert!(editor.apply(Command::AddMarker(30)));
        assert!(!editor.apply(Command::AddMarker(30)));
        assert!(!editor.apply(Command::RemoveMarker(60)));
        let frames = |editor: &Editor| {
            editor
                .project()
                .markers()
                .iter()
                .map(|marker| marker.frame)
                .collect::<Vec<_>>()
        };
        assert_eq!(frames(&editor), [30, 90]);
        assert!(editor.apply(Command::RemoveMarker(90)));
        assert_eq!(frames(&editor), [30]);
        assert!(editor.undo());
        assert_eq!(frames(&editor), [30, 90]);
        assert!(editor.undo());
        assert!(editor.undo());
        assert!(frames(&editor).is_empty());
        assert!(!editor.is_dirty());
        assert!(editor.redo());
        assert_eq!(frames(&editor), [90]);
    }

    #[test]
    fn rejected_edits_and_empty_history_keep_the_clean_checkpoint() {
        let mut editor = Editor::new(Project::new("Untitled"));
        let revision = editor.revision();
        assert!(!editor.apply(Command::RenameProject("Untitled".into())));
        assert!(!editor.apply(Command::RenameProject("   ".into())));
        assert!(!editor.undo());
        assert!(!editor.redo());
        assert_eq!(editor.revision(), revision);
        assert!(!editor.is_dirty());
    }

    #[test]
    fn relink_preserves_identity_and_undo_restores_source() {
        let original = Media {
            id: 1,
            path: "old.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(1_000_000),
            has_audio: false,
        };
        let mut editor = Editor::new(Project::new("Relink"));
        assert!(editor.apply(Command::ImportMedia(original.clone())));
        let replacement = Media {
            path: "new.mp4".into(),
            has_audio: true,
            ..original.clone()
        };
        assert!(!editor.apply(Command::RelinkMedia(Media {
            kind: MediaKind::Audio,
            ..replacement.clone()
        })));
        assert!(!editor.apply(Command::RelinkMedia(Media {
            id: 99,
            ..replacement.clone()
        })));
        assert!(editor.apply(Command::RelinkMedia(replacement.clone())));
        assert_eq!(editor.project().media(), std::slice::from_ref(&replacement));
        assert!(editor.undo());
        assert_eq!(editor.project().media(), &[original]);
        assert!(editor.redo());
        assert_eq!(editor.project().media(), &[replacement]);
    }

    #[test]
    fn relink_silent_video_unlinks_and_undo_restores_both() {
        let mut editor = Editor::new(Project::new("Relink"));
        let original = Media {
            id: 4,
            path: "old.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(2_000_000),
            has_audio: true,
        };
        assert!(editor.apply(Command::ImportMedia(original.clone())));
        assert!(editor.apply(Command::InsertClip {
            track_id: 0,
            clip: Clip {
                speed: 1.0,
                playback: Playback::Forward,
                lut: None,
                id: 1,
                start_frame: 0,
                source_start_frame: 0,
                duration_frames: 30,
                content: ClipContent::Media { media_id: 4 },
                volume: 1.0,
                transition_out: None,
                fade_in_frames: 0,
                fade_out_frames: 0,
                muted: false,
                noise_reduction: false,
                transition: None,
                linked_audio: true,
                color: Default::default(),
                keyframes: Vec::new(),
                transform: Transform::default(),
            }
        }));
        let before = editor.project().clone();
        assert!(!editor.apply(Command::RelinkMedia(Media {
            path: "short.mp4".into(),
            duration_us: Some(500_000),
            has_audio: false,
            ..original.clone()
        })));
        assert_eq!(editor.project(), &before);
        assert!(editor.apply(Command::RelinkMedia(Media {
            path: "silent.mp4".into(),
            has_audio: false,
            ..original
        })));
        assert!(!editor.project().tracks()[0].clips[0].linked_audio);
        assert!(editor.undo());
        assert_eq!(editor.project(), &before);
        assert!(editor.redo());
        assert!(!editor.project().tracks()[0].clips[0].linked_audio);
    }

    #[test]
    fn rename_undo_redo_and_new_edit_invalidate_redo() {
        let mut editor = Editor::new(Project::new("Untitled"));
        assert!(editor.apply(Command::RenameProject("Film".into())));
        assert_eq!(editor.project().name(), "Film");
        assert!(editor.undo());
        assert_eq!(editor.project().name(), "Untitled");
        assert!(editor.redo());
        assert_eq!(editor.project().name(), "Film");
        assert!(editor.undo());
        assert!(editor.apply(Command::RenameProject("New name".into())));
        assert!(!editor.redo());
        assert_eq!(editor.project().name(), "New name");
        assert!(!editor.apply(Command::RenameProject("New name".into())));
        assert!(!editor.apply(Command::RenameProject("   ".into())));
        assert!(editor.undo());
        assert!(!editor.undo());
    }

    #[test]
    fn left_trim_images_extend_into_gaps_but_timed_media_keep_source_start() {
        for kind in [MediaKind::Image, MediaKind::Video, MediaKind::Audio] {
            let mut editor = Editor::new(Project::new("Trim"));
            assert!(editor.apply(Command::ImportMedia(Media {
                id: 1,
                path: "/source".into(),
                kind,
                duration_us: Some(10_000_000),
                has_audio: false,
            })));
            let track_id = if kind == MediaKind::Audio { 2 } else { 0 };
            let clip = Clip {
                playback: crate::timeline::Playback::Forward,
                lut: None,
                id: 1,
                start_frame: 100,
                source_start_frame: 0,
                duration_frames: 30,
                content: ClipContent::Media { media_id: 1 },
                volume: 1.0,
                muted: false,
                speed: 1.0,
                color: Default::default(),
                transition: None,
                transition_out: None,
                fade_in_frames: 0,
                fade_out_frames: 0,
                noise_reduction: false,
                linked_audio: false,
                keyframes: Vec::new(),
                transform: Transform::default(),
            };
            assert!(editor.apply(Command::InsertClip {
                track_id,
                clip: clip.clone(),
            }));
            for target in [75, 0] {
                let start = if kind == MediaKind::Image {
                    target
                } else {
                    100
                };
                let command =
                    super::trim_edge(editor.project(), track_id, 1, true, target).unwrap();
                assert_eq!(
                    command,
                    Command::Trim {
                        track_id,
                        clip_id: 1,
                        start_frame: start,
                        source_start_frame: 0,
                        duration_frames: 130 - start,
                    }
                );
                if kind == MediaKind::Image {
                    assert!(editor.apply(command));
                    assert!(editor.undo());
                    assert_eq!(editor.project().tracks()[0].clips[0], clip);
                }
            }
            assert!(editor.apply(Command::InsertClip {
                track_id,
                clip: Clip {
                    playback: crate::timeline::Playback::Forward,
                    id: 2,
                    start_frame: 20,
                    duration_frames: 30,
                    ..clip
                },
            }));
            let command = super::trim_edge(editor.project(), track_id, 1, true, 0).unwrap();
            assert_eq!(
                command,
                Command::Trim {
                    track_id,
                    clip_id: 1,
                    start_frame: if kind == MediaKind::Image { 50 } else { 100 },
                    source_start_frame: 0,
                    duration_frames: if kind == MediaKind::Image { 80 } else { 30 },
                }
            );
        }
    }

    #[test]
    fn trim_edges_clamp_to_media_and_neighbours_and_undo_once() {
        let mut editor = Editor::new(Project::new("Trim"));
        let rate = &editor.project().settings().frame_rate;
        let frames = u64::from(rate.numerator) * 10 / u64::from(rate.denominator);
        assert!(editor.apply(Command::ImportMedia(Media {
            id: 1,
            path: "/video.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(10_000_000),
            has_audio: false
        })));
        let clip = Clip {
            playback: crate::timeline::Playback::Forward,
            lut: None,
            id: 1,
            start_frame: 40,
            source_start_frame: 20,
            duration_frames: 30,
            content: ClipContent::Media { media_id: 1 },
            volume: 1.0,
            muted: false,
            speed: 1.0,
            color: Default::default(),
            transition: None,
            transition_out: None,
            fade_in_frames: 0,
            fade_out_frames: 0,
            noise_reduction: false,
            linked_audio: false,
            keyframes: Vec::new(),
            transform: Transform::default(),
        };
        assert!(editor.apply(Command::InsertClip {
            track_id: 0,
            clip: clip.clone()
        }));
        let planned = |editor: &Editor, left, frame| {
            super::trim_edge(editor.project(), 0, 1, left, frame).unwrap()
        };
        assert_eq!(
            planned(&editor, true, 0),
            Command::Trim {
                track_id: 0,
                clip_id: 1,
                start_frame: 20,
                source_start_frame: 0,
                duration_frames: 50
            }
        );
        assert_eq!(
            planned(&editor, false, u64::MAX),
            Command::Trim {
                track_id: 0,
                clip_id: 1,
                start_frame: 40,
                source_start_frame: 20,
                duration_frames: frames - 20
            }
        );
        assert!(editor.apply(planned(&editor, true, u64::MAX)));
        assert_eq!(editor.project().tracks()[0].clips[0].duration_frames, 1);
        assert!(editor.undo());
        assert_eq!(editor.project().tracks()[0].clips[0], clip);
        assert!(editor.redo());
        assert!(editor.undo());
        for (id, start_frame, duration_frames) in [(2, 10, 20), (3, 80, 10)] {
            assert!(editor.apply(Command::InsertClip {
                track_id: 0,
                clip: Clip {
                    playback: crate::timeline::Playback::Forward,
                    id,
                    start_frame,
                    duration_frames,
                    source_start_frame: 0,
                    ..clip.clone()
                }
            }));
        }
        assert!(editor.apply(planned(&editor, true, 0)));
        assert_eq!(editor.project().tracks()[0].clips[1].start_frame, 30);
        assert!(editor.undo());
        assert!(editor.apply(planned(&editor, false, u64::MAX)));
        assert_eq!(editor.project().tracks()[0].clips[1].duration_frames, 40);
        assert!(super::trim_edge(editor.project(), 99, 1, false, 0).is_none());
        assert!(super::trim_edge(editor.project(), 0, 99, false, 0).is_none());
    }

    #[test]
    fn title_insertion_and_removal_are_reversible() {
        use crate::timeline::Title;
        let mut editor = Editor::new(Project::new("Film"));
        let title = Clip {
            playback: crate::timeline::Playback::Forward,
            lut: None,
            id: 1,
            start_frame: 0,
            source_start_frame: 0,
            duration_frames: 90,
            content: ClipContent::Title(Title {
                text: "Opening title".into(),
                font_family: "Sans".into(),
                font_size: 48.0,
                color: [255; 4],
                x: 0.5,
                y: 0.5,
                ..Default::default()
            }),
            volume: 1.0,
            muted: false,
            speed: 1.0,
            color: Default::default(),
            transition: None,
            transition_out: None,
            fade_in_frames: 0,
            fade_out_frames: 0,
            noise_reduction: false,
            linked_audio: false,
            keyframes: Vec::new(),
            transform: Transform::default(),
        };
        assert!(editor.apply(Command::InsertClip {
            track_id: 0,
            clip: title.clone(),
        }));
        assert!(!editor.apply(Command::InsertClip {
            track_id: 0,
            clip: title.clone(),
        }));
        assert!(!editor.apply(Command::InsertClip {
            track_id: 2,
            clip: Clip {
                playback: crate::timeline::Playback::Forward,
                id: 2,
                ..title.clone()
            },
        }));
        assert_eq!(editor.project().tracks()[0].clips, vec![title.clone()]);
        assert!(editor.undo());
        assert!(editor.project().tracks()[0].clips.is_empty());
        assert!(editor.redo());
        assert!(editor.apply(Command::RemoveClip {
            track_id: 0,
            clip_id: 1,
        }));
        assert!(editor.undo());
        assert_eq!(editor.project().tracks()[0].clips, vec![title]);
    }

    #[test]
    fn title_style_edits_are_validated_and_undoable() {
        let mut editor = Editor::new(Project::new("Styled"));
        let title = Title::default();
        assert!(editor.apply(Command::InsertClip {
            track_id: 0,
            clip: Clip {
                playback: crate::timeline::Playback::Forward,
                lut: None,
                id: 1,
                start_frame: 0,
                source_start_frame: 0,
                duration_frames: 90,
                content: ClipContent::Title(title.clone()),
                volume: 1.0,
                muted: false,
                speed: 1.0,
                color: Default::default(),
                transition: None,
                transition_out: None,
                fade_in_frames: 0,
                fade_out_frames: 0,
                noise_reduction: false,
                linked_audio: false,
                keyframes: Vec::new(),
                transform: Transform::default(),
            },
        }));
        let changed = Title {
            text: "<b>Hello</b>".into(),
            x: 0.25,
            font_size: 108.0,
            italic: true,
            parse_html: true,
            outline: true,
            ..title.clone()
        };
        let set = |title| Command::SetTitle {
            track_id: 0,
            clip_id: 1,
            title,
        };
        assert!(!editor.apply(set(Title {
            font_size: f32::NAN,
            ..title.clone()
        })));
        assert!(!editor.apply(set(Title {
            x: 2.0,
            ..title.clone()
        })));
        assert!(editor.apply(set(changed.clone())));
        assert_eq!(
            editor.project().tracks()[0].clips[0].content,
            ClipContent::Title(changed.clone())
        );
        assert!(editor.undo());
        assert_eq!(
            editor.project().tracks()[0].clips[0].content,
            ClipContent::Title(title)
        );
        assert!(editor.redo());
        assert_eq!(
            editor.project().tracks()[0].clips[0].content,
            ClipContent::Title(changed)
        );
    }

    #[test]
    fn out_transitions_are_undoable_and_stay_on_the_right_of_splits() {
        use crate::timeline::{SlideDirection, TransitionKind};
        let mut editor = Editor::new(Project::new("Out transitions"));
        let clip: Clip = serde_json::from_value(serde_json::json!({
            "id": 1, "start_frame": 0, "source_start_frame": 0,
            "duration_frames": 10, "content": {"title": Title::default()},
            "volume": 1.0, "muted": false
        }))
        .unwrap();
        assert!(editor.apply(Command::InsertClip { track_id: 0, clip }));
        let transition = Transition {
            kind: TransitionKind::Slide,
            direction: SlideDirection::Left,
            duration_frames: 3,
        };
        let set = |transition| Command::SetTransitionOut {
            track_id: 0,
            clip_id: 1,
            transition,
        };
        assert!(!editor.apply(set(Some(Transition {
            duration_frames: 0,
            ..transition.clone()
        }))));
        assert!(editor.apply(set(Some(transition.clone()))));
        assert!(!editor.apply(set(Some(transition.clone()))));
        assert!(editor.undo());
        assert!(
            editor.project().tracks()[0].clips[0]
                .transition_out
                .is_none()
        );
        assert!(editor.redo());
        assert!(editor.apply(Command::SetTransition {
            track_id: 0,
            clip_id: 1,
            transition: Some(transition.clone())
        }));
        let before = editor.project().clone();
        assert!(editor.apply(Command::Split {
            track_id: 0,
            clip_id: 1,
            at_frame: 5,
            new_id: 2
        }));
        let clips = &editor.project().tracks()[0].clips;
        assert_eq!(clips[0].transition, Some(transition.clone()));
        assert!(clips[0].transition_out.is_none());
        assert!(clips[1].transition.is_none());
        assert_eq!(clips[1].transition_out, Some(transition.clone()));
        assert!(editor.undo());
        assert_eq!(editor.project(), &before);
        assert!(editor.redo());
        assert!(editor.undo());
        assert!(editor.apply(set(None)));
        assert!(editor.undo());
        assert_eq!(editor.project(), &before);
        assert!(!editor.apply(Command::SetTransitionOut {
            track_id: 99,
            clip_id: 1,
            transition: Some(transition)
        }));
    }

    #[test]
    fn transitions_are_undoable_and_stay_on_the_left_of_splits() {
        use crate::timeline::{Transition, TransitionKind};
        let mut editor = Editor::new(Project::new("Transitions"));
        let clip: Clip = serde_json::from_value(serde_json::json!({
            "id": 1, "start_frame": 0, "source_start_frame": 0,
            "duration_frames": 10, "content": {"title": Title::default()},
            "volume": 1.0, "muted": false
        }))
        .unwrap();
        assert!(editor.apply(Command::InsertClip { track_id: 0, clip }));
        let transition = Some(Transition {
            direction: Default::default(),
            kind: TransitionKind::Dissolve,
            duration_frames: 20,
        });
        let set = |transition| Command::SetTransition {
            track_id: 0,
            clip_id: 1,
            transition,
        };
        assert!(!editor.apply(set(Some(Transition {
            direction: Default::default(),
            kind: TransitionKind::Fade,
            duration_frames: 0
        }))));
        assert!(editor.apply(set(transition.clone())));
        assert!(!editor.apply(set(transition.clone())));
        assert!(editor.undo());
        assert!(editor.project().tracks()[0].clips[0].transition.is_none());
        assert!(editor.redo());
        assert_eq!(editor.project().tracks()[0].clips[0].transition, transition);
        assert!(editor.apply(set(None)));
        assert!(editor.undo());
        assert!(editor.apply(Command::Split {
            track_id: 0,
            clip_id: 1,
            at_frame: 5,
            new_id: 2
        }));
        assert_eq!(editor.project().tracks()[0].clips[0].transition, transition);
        assert!(editor.project().tracks()[0].clips[1].transition.is_none());
        assert!(editor.undo());
        assert!(editor.redo());
        assert!(editor.project().tracks()[0].clips[1].transition.is_none());
        assert!(editor.apply(Command::Move {
            track_id: 0,
            target_track_id: 1,
            clip_id: 1,
            to_frame: 20
        }));
        assert_eq!(editor.project().tracks()[1].clips[0].transition, transition);
        assert!(editor.apply(Command::Trim {
            track_id: 1,
            clip_id: 1,
            start_frame: 21,
            source_start_frame: 1,
            duration_frames: 2
        }));
        assert_eq!(editor.project().tracks()[1].clips[0].transition, transition);
        assert!(editor.apply(Command::RippleDelete {
            track_id: 0,
            clip_id: 2
        }));
        assert_eq!(editor.project().tracks()[1].clips[0].transition, transition);
        assert!(editor.undo());
        assert!(editor.redo());
        assert_eq!(editor.project().tracks()[1].clips[0].transition, transition);
    }

    #[test]
    fn clip_luts_validate_round_trip_and_undo() {
        let mut editor = Editor::new(Project::new("LUT"));
        assert!(editor.apply(Command::ImportMedia(Media {
            id: 4,
            path: "image.png".into(),
            kind: MediaKind::Image,
            duration_us: None,
            has_audio: false,
        })));
        let clip: Clip = serde_json::from_value(serde_json::json!({
            "id": 1, "start_frame": 0, "source_start_frame": 0,
            "duration_frames": 10, "content": {"media": {"media_id": 4}},
            "volume": 1.0, "muted": false
        }))
        .unwrap();
        assert!(clip.lut.is_none());
        assert!(!serde_json::to_string(&clip).unwrap().contains("lut"));
        assert!(editor.apply(Command::InsertClip { track_id: 0, clip }));
        let set = |track_id, clip_id, path| Command::SetClipLut {
            track_id,
            clip_id,
            path,
        };
        let path = Some(std::path::PathBuf::from("look.cube"));
        assert!(!editor.apply(set(99, 1, path.clone())));
        assert!(!editor.apply(set(0, 99, path.clone())));
        assert!(!editor.apply(set(0, 1, None)));
        assert!(editor.apply(Command::SetTrackLocked {
            track_id: 0,
            locked: true
        }));
        let locked = editor.project().clone();
        assert!(!editor.apply(set(0, 1, path.clone())));
        assert_eq!(editor.project(), &locked);
        assert!(editor.apply(Command::SetTrackLocked {
            track_id: 0,
            locked: false
        }));
        assert!(editor.apply(set(0, 1, path.clone())));
        assert!(!editor.apply(set(0, 1, path.clone())));
        let json = crate::project::format::to_json(editor.project()).unwrap();
        assert_eq!(
            crate::project::format::from_json(&json).unwrap(),
            *editor.project()
        );
        assert!(editor.undo());
        assert!(editor.project().tracks()[0].clips[0].lut.is_none());
        assert!(editor.redo());
        assert_eq!(editor.project().tracks()[0].clips[0].lut, path);
        assert!(editor.apply(set(0, 1, None)));
        assert!(editor.undo());
        assert!(editor.apply(Command::Split {
            track_id: 0,
            clip_id: 1,
            at_frame: 5,
            new_id: 2
        }));
        assert!(
            editor.project().tracks()[0]
                .clips
                .iter()
                .all(|clip| clip.lut == path)
        );
        let mut title = editor.project().tracks()[0].clips[0].clone();
        title.id = 3;
        title.content = ClipContent::Title(Title::default());
        assert!(!editor.apply(Command::InsertClip {
            track_id: 1,
            clip: title.clone()
        }));
        title.lut = None;
        assert!(editor.apply(Command::InsertClip {
            track_id: 1,
            clip: title
        }));
        assert!(!editor.apply(set(1, 3, path)));
    }

    #[test]
    fn clip_insertion_rejects_invalid_ranges_overlap_and_missing_media() {
        let mut editor = Editor::new(Project::new("Film"));
        let title = Clip {
            playback: crate::timeline::Playback::Forward,
            lut: None,
            id: 1,
            start_frame: 10,
            source_start_frame: 0,
            duration_frames: 90,
            content: ClipContent::Title(crate::timeline::Title {
                text: "Opening".into(),
                font_family: "Sans".into(),
                font_size: 48.0,
                color: [255; 4],
                x: 0.5,
                y: 0.5,
                ..Default::default()
            }),
            volume: 1.0,
            muted: false,
            speed: 1.0,
            color: Default::default(),
            transition: None,
            transition_out: None,
            fade_in_frames: 0,
            fade_out_frames: 0,
            noise_reduction: false,
            linked_audio: false,
            keyframes: Vec::new(),
            transform: Transform::default(),
        };
        let insert =
            |editor: &mut Editor, clip| editor.apply(Command::InsertClip { track_id: 0, clip });
        assert!(!insert(
            &mut editor,
            Clip {
                duration_frames: 0,
                ..title.clone()
            }
        ));
        assert!(!insert(
            &mut editor,
            Clip {
                start_frame: u64::MAX,
                ..title.clone()
            }
        ));
        assert!(!insert(
            &mut editor,
            Clip {
                content: ClipContent::Media { media_id: 99 },
                ..title.clone()
            }
        ));
        assert!(insert(&mut editor, title.clone()));
        assert!(!insert(
            &mut editor,
            Clip {
                playback: crate::timeline::Playback::Forward,
                id: 2,
                start_frame: 99,
                ..title.clone()
            }
        ));
        assert!(insert(
            &mut editor,
            Clip {
                playback: crate::timeline::Playback::Forward,
                id: 2,
                start_frame: 100,
                ..title
            }
        ));
        assert_eq!(editor.project().tracks()[0].clips.len(), 2);
    }

    #[test]
    fn split_tracks_preserves_trimmed_source_offsets_and_clip_fields() {
        let tracks = split_fixture();
        let split = split_tracks(&tracks, &[(0, 1)], 20, 5).unwrap();
        let original = &tracks[0].clips[0];
        assert_eq!(
            split[0].clips[0],
            Clip {
                duration_frames: 10,
                ..original.clone()
            }
        );
        assert_eq!(
            split[0].clips[1],
            Clip {
                playback: crate::timeline::Playback::Forward,
                id: 5,
                start_frame: 20,
                source_start_frame: 50,
                duration_frames: 20,
                ..original.clone()
            }
        );
    }

    fn split_fixture() -> Vec<Track> {
        let mut project = Project::new("Split");
        let clip = Clip {
            playback: crate::timeline::Playback::Forward,
            lut: None,
            id: 1,
            start_frame: 10,
            source_start_frame: 40,
            duration_frames: 30,
            content: ClipContent::Title(Title::default()),
            volume: 0.75,
            muted: true,
            speed: 1.0,
            color: Default::default(),
            transition: None,
            transition_out: None,
            fade_in_frames: 0,
            fade_out_frames: 0,
            noise_reduction: false,
            linked_audio: false,
            keyframes: Vec::new(),
            transform: Transform::default(),
        };
        project.tracks[0].clips = vec![
            clip.clone(),
            Clip {
                playback: crate::timeline::Playback::Forward,
                id: 2,
                start_frame: 60,
                ..clip.clone()
            },
        ];
        project.tracks[1].clips = vec![Clip { id: 3, ..clip }];
        project.tracks
    }

    #[test]
    fn split_tracks_requires_a_strictly_interior_frame() {
        let tracks = split_fixture();
        for at in [0, 9, 10, 40, 41, u64::MAX] {
            assert!(split_tracks(&tracks, &[(0, 1)], at, 4).is_none());
        }
        for at in [11, 39] {
            let split = split_tracks(&tracks, &[(0, 1)], at, 4).unwrap();
            assert_eq!(split[0].clips[0].duration_frames, at - 10);
            assert_eq!(split[0].clips[1].duration_frames, 40 - at);
        }
    }

    #[test]
    fn split_tracks_handles_multiple_tracks_and_skips_non_overlapping_targets() {
        let tracks = split_fixture();
        let split = split_tracks(&tracks, &[(0, 2), (0, 1), (1, 3)], 20, 4).unwrap();
        assert_eq!(split[0].clips[1].id, 4);
        assert_eq!(split[1].clips[1].id, 5);
        assert_eq!(split[0].clips[2], tracks[0].clips[1]);
        assert_eq!(split[1].clips[0].duration_frames, 10);
        assert_eq!(&split[2..], &tracks[2..]);
        for track in &split {
            assert!(track.clips.windows(2).all(|pair| {
                pair[0].start_frame + pair[0].duration_frames <= pair[1].start_frame
            }));
        }
    }

    #[test]
    fn split_tracks_rejects_empty_missing_and_overflowing_plans() {
        let tracks = split_fixture();
        assert!(split_tracks(&tracks, &[], 20, 4).is_none());
        assert!(split_tracks(&[], &[(0, 1)], 20, 4).is_none());
        assert!(split_tracks(&tracks, &[(0, 99)], 20, 4).is_none());
        assert!(split_tracks(&tracks, &[(0, 1), (99, 1)], 20, 4).is_none());
        assert!(split_tracks(&tracks, &[(0, 1)], 20, u64::MAX).is_none());
        assert!(split_tracks(&tracks, &[(0, 1), (1, 3)], 20, u64::MAX - 1).is_none());
        let mut overflowing = tracks.clone();
        overflowing[0].clips[0].source_start_frame = u64::MAX;
        assert!(split_tracks(&overflowing, &[(0, 1)], 20, 4).is_none());
        overflowing[0].clips[0] = Clip {
            start_frame: u64::MAX - 10,
            duration_frames: 30,
            ..tracks[0].clips[0].clone()
        };
        assert!(split_tracks(&overflowing, &[(0, 1)], u64::MAX - 5, 4).is_none());
    }

    #[test]
    fn multi_track_split_replace_tracks_undoes_in_one_step() {
        let original = split_fixture();
        let mut project = Project::new("Split");
        project.tracks = original.clone();
        let mut editor = Editor::new(project);
        let split = split_tracks(editor.project().tracks(), &[(0, 1), (1, 3)], 20, 4).unwrap();
        assert!(editor.apply(Command::ReplaceTracks(split.clone())));
        assert_eq!(editor.project().tracks(), split);
        assert!(editor.undo());
        assert_eq!(editor.project().tracks(), original);
        assert!(!editor.undo());
        assert!(editor.redo());
        assert_eq!(editor.project().tracks(), split);
    }

    #[test]
    fn split_move_trim_and_ripple_delete_are_atomic_and_undoable() {
        let mut editor = Editor::new(Project::new("Film"));
        let make_clip = |id, start_frame| Clip {
            playback: crate::timeline::Playback::Forward,
            lut: None,
            id,
            start_frame,
            source_start_frame: 0,
            duration_frames: 30,
            content: ClipContent::Title(crate::timeline::Title {
                text: format!("Title {id}"),
                font_family: "Sans".into(),
                font_size: 40.0,
                color: [255; 4],
                x: 0.5,
                y: 0.5,
                ..Default::default()
            }),
            volume: 1.0,
            muted: false,
            speed: 1.0,
            color: Default::default(),
            transition: None,
            transition_out: None,
            fade_in_frames: 0,
            fade_out_frames: 0,
            noise_reduction: false,
            linked_audio: false,
            keyframes: Vec::new(),
            transform: Transform::default(),
        };
        for (id, start_frame, track_id) in [(1, 0, 0), (2, 30, 0), (3, 30, 1)] {
            assert!(editor.apply(Command::InsertClip {
                track_id,
                clip: make_clip(id, start_frame)
            }));
        }
        let initial = editor.project().tracks().to_vec();
        assert!(!editor.apply(Command::Split {
            track_id: 0,
            clip_id: 1,
            at_frame: 0,
            new_id: 4
        }));
        assert!(editor.apply(Command::Split {
            track_id: 0,
            clip_id: 1,
            at_frame: 15,
            new_id: 4
        }));
        assert_eq!(editor.project().tracks()[0].clips[1].source_start_frame, 15);
        assert!(!editor.apply(Command::Move {
            track_id: 0,
            target_track_id: 1,
            clip_id: 4,
            to_frame: 35
        }));
        assert!(editor.apply(Command::Move {
            track_id: 0,
            target_track_id: 1,
            clip_id: 4,
            to_frame: 0
        }));
        assert!(!editor.apply(Command::Trim {
            track_id: 1,
            clip_id: 4,
            start_frame: 0,
            source_start_frame: 15,
            duration_frames: 35
        }));
        assert!(editor.apply(Command::Trim {
            track_id: 1,
            clip_id: 4,
            start_frame: 0,
            source_start_frame: 15,
            duration_frames: 20
        }));
        assert!(editor.apply(Command::RippleDelete {
            track_id: 0,
            clip_id: 2
        }));
        assert_eq!(editor.project().tracks()[1].clips.len(), 1);
        for _ in 0..4 {
            assert!(editor.undo());
        }
        assert_eq!(editor.project().tracks(), initial);
        for _ in 0..4 {
            assert!(editor.redo());
        }
        assert_eq!(editor.project().tracks()[0].clips.len(), 1);
    }

    #[test]
    fn ripple_delete_splits_spanning_clips_without_losing_source_frames() {
        let mut editor = Editor::new(Project::new("Film"));
        assert!(editor.apply(Command::ImportMedia(Media {
            id: 1,
            path: "footage.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(2_000_000),
            has_audio: true,
        })));
        for (track_id, id, start_frame, duration_frames) in [(0, 1, 20, 20), (1, 2, 0, 60)] {
            assert!(editor.apply(Command::InsertClip {
                track_id,
                clip: Clip {
                    playback: crate::timeline::Playback::Forward,
                    lut: None,
                    id,
                    start_frame,
                    duration_frames,
                    source_start_frame: 0,
                    content: if id == 2 {
                        ClipContent::Media { media_id: 1 }
                    } else {
                        ClipContent::Title(Title::default())
                    },
                    volume: if id == 2 { 0.4 } else { 1.0 },
                    muted: id == 2,
                    speed: 1.0,
                    color: Default::default(),
                    transition: None,
                    transition_out: None,
                    fade_in_frames: 0,
                    fade_out_frames: 0,
                    noise_reduction: false,
                    linked_audio: id == 2,
                    keyframes: Vec::new(),
                    transform: Transform::default(),
                },
            }));
        }
        let before = editor.project().tracks().to_vec();
        assert!(editor.apply(Command::RippleDelete {
            track_id: 0,
            clip_id: 1
        }));
        let pieces = &editor.project().tracks()[1].clips;
        assert_eq!(
            pieces
                .iter()
                .map(|clip| (
                    clip.start_frame,
                    clip.source_start_frame,
                    clip.duration_frames
                ))
                .collect::<Vec<_>>(),
            vec![(0, 0, 20), (20, 40, 20)]
        );
        assert_ne!(pieces[0].id, pieces[1].id);
        assert!(
            pieces
                .iter()
                .all(|clip| clip.linked_audio && clip.muted && clip.volume == 0.4)
        );
        let after = editor.project().tracks().to_vec();
        assert!(editor.undo());
        assert_eq!(editor.project().tracks(), before);
        assert!(editor.redo());
        assert_eq!(editor.project().tracks(), after);
    }

    #[test]
    fn ripple_delete_selection_merges_ranges_and_preserves_audio_source_offsets() {
        let mut editor = Editor::new(Project::new("Film"));
        assert!(editor.apply(Command::ImportMedia(Media {
            id: 1,
            path: "sound.wav".into(),
            kind: MediaKind::Audio,
            duration_us: Some(4_000_000),
            has_audio: true,
        })));
        let title = |id, start_frame, duration_frames| Clip {
            playback: crate::timeline::Playback::Forward,
            lut: None,
            id,
            start_frame,
            source_start_frame: 0,
            duration_frames,
            content: ClipContent::Title(Title::default()),
            volume: 1.0,
            muted: false,
            speed: 1.0,
            color: Default::default(),
            transition: None,
            transition_out: None,
            fade_in_frames: 0,
            fade_out_frames: 0,
            noise_reduction: false,
            linked_audio: false,
            keyframes: Vec::new(),
            transform: Transform::default(),
        };
        for (track_id, clip) in [
            (0, title(1, 20, 20)),
            (0, title(2, 80, 10)),
            (0, title(3, 95, 15)),
            (0, title(7, 45, 5)),
            (1, title(4, 30, 30)),
            (1, title(5, 70, 10)),
            (1, title(8, 10, 20)),
            (
                2,
                Clip {
                    playback: crate::timeline::Playback::Forward,
                    lut: None,
                    id: 6,
                    start_frame: 0,
                    source_start_frame: 0,
                    duration_frames: 120,
                    content: ClipContent::Media { media_id: 1 },
                    volume: 0.5,
                    muted: true,
                    speed: 1.0,
                    color: Default::default(),
                    transition: None,
                    transition_out: None,
                    fade_in_frames: 0,
                    fade_out_frames: 0,
                    noise_reduction: false,
                    linked_audio: false,
                    keyframes: Vec::new(),
                    transform: Transform::default(),
                },
            ),
        ] {
            assert!(editor.apply(Command::InsertClip { track_id, clip }));
        }
        let original = editor.project().tracks().to_vec();
        let selection = vec![(0, 2), (1, 4), (0, 1)];
        assert!(editor.apply(Command::RippleDeleteSelection(selection)));
        editor.project().validate().unwrap();
        let tracks = editor.project().tracks();
        assert_eq!(
            tracks[0]
                .clips
                .iter()
                .map(|clip| (clip.id, clip.start_frame, clip.duration_frames))
                .collect::<Vec<_>>(),
            vec![(3, 45, 15)]
        );
        assert_eq!(
            tracks[1]
                .clips
                .iter()
                .map(|clip| (clip.id, clip.start_frame, clip.duration_frames))
                .collect::<Vec<_>>(),
            vec![(8, 10, 10), (5, 30, 10)]
        );
        let audio = &tracks[2].clips;
        assert_eq!(
            audio
                .iter()
                .map(|clip| (
                    clip.start_frame,
                    clip.source_start_frame,
                    clip.duration_frames
                ))
                .collect::<Vec<_>>(),
            vec![(0, 0, 20), (20, 60, 20), (40, 90, 30)]
        );
        assert_eq!(audio[0].id, 6);
        assert!(audio[0].id != audio[1].id && audio[1].id != audio[2].id);
        assert!(audio.iter().all(|clip| clip.volume == 0.5 && clip.muted));
        let after = tracks.to_vec();
        assert!(editor.undo());
        assert_eq!(editor.project().tracks(), original);
        assert!(editor.redo());
        assert_eq!(editor.project().tracks(), after);
        assert!(!editor.apply(Command::RippleDeleteSelection(vec![(0, 1)])));
        assert_eq!(editor.project().tracks(), after);
    }

    #[test]
    fn ripple_delete_rejects_new_id_overflow_without_changing_project() {
        let mut editor = Editor::new(Project::new("Film"));
        for (track_id, id, start_frame, duration_frames) in [(0, u64::MAX, 20, 20), (1, 1, 0, 60)] {
            assert!(editor.apply(Command::InsertClip {
                track_id,
                clip: Clip {
                    playback: crate::timeline::Playback::Forward,
                    lut: None,
                    id,
                    start_frame,
                    source_start_frame: 0,
                    duration_frames,
                    content: ClipContent::Title(Title::default()),
                    volume: 1.0,
                    muted: false,
                    speed: 1.0,
                    color: Default::default(),
                    transition: None,
                    transition_out: None,
                    fade_in_frames: 0,
                    fade_out_frames: 0,
                    noise_reduction: false,
                    linked_audio: false,
                    keyframes: Vec::new(),
                    transform: Transform::default(),
                },
            }));
        }
        let original = editor.project().tracks().to_vec();
        assert!(!editor.apply(Command::RippleDelete {
            track_id: 0,
            clip_id: u64::MAX,
        }));
        assert_eq!(editor.project().tracks(), original);
    }

    #[test]
    fn imported_media_clips_cannot_extend_past_probed_source_duration() {
        let mut editor = Editor::new(Project::new("Audio"));
        assert!(editor.apply(Command::ImportMedia(Media {
            id: 1,
            path: "tone.wav".into(),
            kind: MediaKind::Audio,
            duration_us: Some(1_000_000),
            has_audio: true,
        })));
        let clip = Clip {
            playback: crate::timeline::Playback::Forward,
            lut: None,
            id: 2,
            start_frame: 0,
            source_start_frame: 0,
            duration_frames: 31,
            content: ClipContent::Media { media_id: 1 },
            volume: 1.0,
            muted: false,
            speed: 1.0,
            color: Default::default(),
            transition: None,
            transition_out: None,
            fade_in_frames: 0,
            fade_out_frames: 0,
            noise_reduction: false,
            linked_audio: false,
            keyframes: Vec::new(),
            transform: Transform::default(),
        };
        assert!(!editor.apply(Command::InsertClip {
            track_id: 2,
            clip: clip.clone(),
        }));
        assert!(editor.apply(Command::InsertClip {
            track_id: 2,
            clip: Clip {
                duration_frames: 30,
                ..clip
            },
        }));
        assert!(editor.undo());
        assert!(editor.undo());
        assert!(editor.project().media().is_empty());
    }

    #[test]
    fn reprobe_updates_duration_without_invalidating_existing_clips() {
        let mut editor = Editor::new(Project::new("Audio"));
        let media = Media {
            id: 1,
            path: "tone.wav".into(),
            kind: MediaKind::Audio,
            duration_us: None,
            has_audio: true,
        };
        assert!(editor.apply(Command::ImportMedia(media.clone())));
        assert!(!editor.apply(Command::InsertClip {
            track_id: 2,
            clip: Clip {
                playback: crate::timeline::Playback::Forward,
                lut: None,
                id: 1,
                start_frame: 0,
                source_start_frame: 0,
                duration_frames: 30,
                content: ClipContent::Media { media_id: 1 },
                volume: 1.0,
                muted: false,
                speed: 1.0,
                color: Default::default(),
                transition: None,
                transition_out: None,
                fade_in_frames: 0,
                fade_out_frames: 0,
                noise_reduction: false,
                linked_audio: false,
                keyframes: Vec::new(),
                transform: Transform::default(),
            },
        }));
        let known = Media {
            duration_us: Some(1_000_000),
            ..media.clone()
        };
        assert!(editor.apply(Command::ReprobeMedia(known.clone())));
        assert!(editor.apply(Command::InsertClip {
            track_id: 2,
            clip: Clip {
                playback: crate::timeline::Playback::Forward,
                lut: None,
                id: 1,
                start_frame: 0,
                source_start_frame: 0,
                duration_frames: 30,
                content: ClipContent::Media { media_id: 1 },
                volume: 1.0,
                muted: false,
                speed: 1.0,
                color: Default::default(),
                transition: None,
                transition_out: None,
                fade_in_frames: 0,
                fade_out_frames: 0,
                noise_reduction: false,
                linked_audio: false,
                keyframes: Vec::new(),
                transform: Transform::default(),
            },
        }));
        assert!(!editor.apply(Command::ReprobeMedia(Media {
            duration_us: Some(500_000),
            ..known.clone()
        })));
        assert_eq!(editor.project().media(), &[known]);
        assert!(editor.undo());
        assert!(editor.undo());
        assert_eq!(editor.project().media(), &[media]);
    }

    #[test]
    fn audio_clip_volume_and_mute_are_undoable() {
        let mut editor = Editor::new(Project::new("Audio"));
        assert!(editor.apply(Command::ImportMedia(Media {
            id: 1,
            path: "tone.wav".into(),
            kind: MediaKind::Audio,
            duration_us: Some(1_000_000),
            has_audio: true,
        })));
        assert!(editor.apply(Command::InsertClip {
            track_id: 2,
            clip: Clip {
                playback: crate::timeline::Playback::Forward,
                lut: None,
                id: 1,
                start_frame: 0,
                source_start_frame: 0,
                duration_frames: 30,
                content: ClipContent::Media { media_id: 1 },
                volume: 1.0,
                muted: false,
                speed: 1.0,
                color: Default::default(),
                transition: None,
                transition_out: None,
                fade_in_frames: 0,
                fade_out_frames: 0,
                noise_reduction: false,
                linked_audio: false,
                keyframes: Vec::new(),
                transform: Transform::default(),
            },
        }));
        assert!(!editor.apply(Command::SetClipAudio {
            track_id: 2,
            clip_id: 1,
            volume: f32::NAN,
            muted: true,
        }));
        assert!(!editor.apply(Command::SetClipAudio {
            track_id: 0,
            clip_id: 1,
            volume: 0.5,
            muted: true,
        }));
        assert!(editor.apply(Command::SetClipAudio {
            track_id: 2,
            clip_id: 1,
            volume: 0.5,
            muted: true,
        }));
        assert_eq!(editor.project().tracks()[2].clips[0].volume, 0.5);
        assert!(editor.project().tracks()[2].clips[0].muted);
        assert!(editor.undo());
        assert_eq!(editor.project().tracks()[2].clips[0].volume, 1.0);
        assert!(!editor.project().tracks()[2].clips[0].muted);
        assert!(editor.redo());
        assert!(editor.project().tracks()[2].clips[0].muted);
    }

    #[test]
    fn locked_tracks_reject_clip_transforms() {
        let mut editor = Editor::new(Project::new("Locked transform"));
        assert!(editor.apply(Command::ImportMedia(Media {
            id: 1,
            path: "/image.png".into(),
            kind: MediaKind::Image,
            duration_us: None,
            has_audio: false,
        })));
        let clip = serde_json::from_value(serde_json::json!({
            "id": 1, "start_frame": 0, "source_start_frame": 0,
            "duration_frames": 30, "content": {"media": {"media_id": 1}},
            "volume": 1.0, "muted": false
        }))
        .unwrap();
        assert!(editor.apply(Command::InsertClip { track_id: 0, clip }));
        let command = Command::SetClipTransform {
            track_id: 0,
            clip_id: 1,
            transform: Transform {
                scale: 0.5,
                ..Default::default()
            },
        };
        assert!(editor.apply(command.clone()));
        assert!(editor.undo());
        assert!(editor.apply(Command::SetTrackLocked {
            track_id: 0,
            locked: true
        }));
        let locked = editor.project().clone();
        assert!(!editor.apply(command.clone()));
        assert_eq!(editor.project(), &locked);
        assert!(editor.undo());
        assert!(editor.apply(command));
    }

    #[test]
    fn media_transforms_validate_and_survive_history_and_split() {
        let mut editor = Editor::new(Project::new("Transforms"));
        assert!(editor.apply(Command::ImportMedia(Media {
            id: 1,
            path: "image.png".into(),
            kind: MediaKind::Image,
            duration_us: None,
            has_audio: false
        })));
        let clip = Clip {
            speed: 1.0,
            playback: crate::timeline::Playback::Forward,
            noise_reduction: false,
            lut: None,
            transition_out: None,
            id: 1,
            start_frame: 0,
            source_start_frame: 0,
            duration_frames: 30,
            content: ClipContent::Media { media_id: 1 },
            volume: 1.0,
            muted: false,
            transition: None,
            fade_in_frames: 0,
            fade_out_frames: 0,
            linked_audio: false,
            color: Default::default(),
            keyframes: Vec::new(),
            transform: Transform::default(),
        };
        assert!(editor.apply(Command::InsertClip { track_id: 0, clip }));
        let change = |transform| Command::SetClipTransform {
            track_id: 0,
            clip_id: 1,
            transform,
        };
        assert!(!editor.apply(change(Transform::default())));
        for transform in [
            Transform {
                scale: 0.0,
                ..Default::default()
            },
            Transform {
                x: f32::INFINITY,
                ..Default::default()
            },
            Transform {
                crop_top: 0.5,
                crop_bottom: 0.5,
                ..Default::default()
            },
        ] {
            assert!(!editor.apply(change(transform)));
        }
        assert!(!editor.apply(Command::SetClipTransform {
            track_id: 2,
            clip_id: 1,
            transform: Transform {
                scale: 0.5,
                ..Default::default()
            }
        }));
        let transformed = Transform {
            scale: 0.5,
            x: 0.1,
            rotation: 90.0,
            crop_left: 0.1,
            opacity: 0.5,
            ..Default::default()
        };
        assert!(editor.apply(change(transformed)));
        assert!(editor.undo());
        assert!(
            editor.project().tracks()[0].clips[0]
                .transform
                .is_identity()
        );
        assert!(editor.redo());
        assert_eq!(editor.project().tracks()[0].clips[0].transform, transformed);
        assert!(editor.apply(Command::Split {
            track_id: 0,
            clip_id: 1,
            at_frame: 15,
            new_id: 2
        }));
        assert!(
            editor.project().tracks()[0]
                .clips
                .iter()
                .all(|c| c.transform == transformed)
        );
    }

    #[test]
    fn keyframes_validate_round_trip_and_follow_content_edits() {
        use crate::timeline::Keyframe;
        let mut editor = Editor::new(Project::new("Animation"));
        assert!(editor.apply(Command::ImportMedia(Media {
            id: 1,
            path: "image.png".into(),
            kind: MediaKind::Image,
            duration_us: None,
            has_audio: false,
        })));
        let clip: Clip = serde_json::from_value(serde_json::json!({
            "id": 1, "start_frame": 10, "source_start_frame": 20,
            "duration_frames": 30, "content": {"media": {"media_id": 1}},
            "volume": 1.0, "muted": false,
            "transform": {"scale":1.0,"x":0.0,"y":0.0,"rotation":0.0,
                "opacity":1.0,"crop_left":0.1,"crop_right":0.0,"crop_top":0.0,"crop_bottom":0.0}
        }))
        .unwrap();
        assert!(editor.apply(Command::InsertClip { track_id: 0, clip }));
        let command = |keyframes| Command::SetClipKeyframes {
            track_id: 0,
            clip_id: 1,
            keyframes,
        };
        assert!(!editor.apply(command(vec![])));
        let first = Keyframe::new(5, Transform::default());
        let last = Keyframe::new(
            15,
            Transform {
                scale: 2.0,
                x: 1.0,
                y: -1.0,
                rotation: 90.0,
                opacity: 0.0,
                ..Default::default()
            },
        );
        for invalid in [
            vec![last, first],
            vec![first, first],
            vec![Keyframe {
                opacity: 2.0,
                ..first
            }],
            vec![Keyframe {
                scale: f32::NAN,
                ..first
            }],
            vec![Keyframe {
                easing: crate::timeline::Easing::Curve {
                    x1: -0.1,
                    y1: 0.0,
                    x2: 1.0,
                    y2: 1.0,
                },
                ..first
            }],
        ] {
            assert!(!editor.apply(command(invalid)));
        }
        let keys = vec![first, last];
        assert!(editor.apply(command(keys.clone())));
        assert!(!editor.apply(command(keys.clone())));
        let clip = &editor.project().tracks()[0].clips[0];
        assert_eq!(clip.transform_at(10).scale, 1.0);
        assert_eq!(clip.transform_at(39).scale, 2.0);
        let middle = clip.transform_at(20);
        assert_eq!(
            (
                middle.scale,
                middle.x,
                middle.y,
                middle.rotation,
                middle.opacity
            ),
            (1.5, 0.5, -0.5, 45.0, 0.5)
        );
        assert_eq!(middle.crop_left, 0.1);
        let json = crate::project::format::to_json(editor.project()).unwrap();
        assert_eq!(
            &crate::project::format::from_json(&json).unwrap(),
            editor.project()
        );
        let mut invalid: serde_json::Value = serde_json::from_str(&json).unwrap();
        invalid["tracks"][0]["clips"][0]["keyframes"][0]["opacity"] = 2.0.into();
        assert!(crate::project::format::from_json(&invalid.to_string()).is_err());
        assert!(editor.undo());
        assert!(editor.project().tracks()[0].clips[0].keyframes.is_empty());
        assert!(editor.redo());
        assert!(editor.apply(Command::Split {
            track_id: 0,
            clip_id: 1,
            at_frame: 20,
            new_id: 2
        }));
        let right = &editor.project().tracks()[0].clips[1];
        assert_eq!(right.keyframes[0].frame, 0);
        assert_eq!(right.transform_at(20), middle);
        assert!(editor.apply(Command::Trim {
            track_id: 0,
            clip_id: 2,
            start_frame: 22,
            source_start_frame: 32,
            duration_frames: 18
        }));
        let right = &editor.project().tracks()[0].clips[1];
        assert_eq!(right.keyframes[0].frame, 0);
        assert_eq!(right.transform_at(22).rotation, 63.0);
    }

    #[test]
    fn fades_validate_undo_split_and_trim() {
        let mut editor = Editor::new(Project::new("Fades"));
        assert!(editor.apply(Command::ImportMedia(Media {
            id: 1,
            path: "tone.wav".into(),
            kind: MediaKind::Audio,
            duration_us: Some(4_000_000),
            has_audio: true,
        })));
        let old = r#"{"id":1,"start_frame":0,"source_start_frame":0,"duration_frames":120,"content":{"media":{"media_id":1}},"volume":1.0,"muted":false}"#;
        let clip: Clip = serde_json::from_str(old).unwrap();
        assert_eq!((clip.fade_in_frames, clip.fade_out_frames), (0, 0));
        assert!(!serde_json::to_string(&clip).unwrap().contains("fade_"));
        assert!(editor.apply(Command::InsertClip { track_id: 2, clip }));
        let fades = |track_id, incoming, outgoing| Command::SetClipFades {
            track_id,
            clip_id: 1,
            fade_in_frames: incoming,
            fade_out_frames: outgoing,
        };
        assert!(!editor.apply(fades(2, 0, 0)));
        assert!(!editor.apply(fades(0, 20, 20)));
        assert!(!editor.apply(fades(2, u64::MAX, 1)));
        assert!(!editor.apply(fades(2, 80, 50)));
        assert!(editor.apply(fades(2, 30, 30)));
        assert!(editor.undo());
        assert_eq!(editor.project().tracks()[2].clips[0].fade_in_frames, 0);
        assert!(editor.redo());
        let tracks = split_tracks(editor.project().tracks(), &[(2, 1)], 20, 2).unwrap();
        assert_eq!(
            (
                tracks[2].clips[0].fade_in_frames,
                tracks[2].clips[0].fade_out_frames
            ),
            (20, 0)
        );
        assert_eq!(
            (
                tracks[2].clips[1].fade_in_frames,
                tracks[2].clips[1].fade_out_frames
            ),
            (0, 30)
        );
        assert!(editor.apply(Command::Trim {
            track_id: 2,
            clip_id: 1,
            start_frame: 0,
            source_start_frame: 0,
            duration_frames: 40
        }));
        let clip = &editor.project().tracks()[2].clips[0];
        assert_eq!((clip.fade_in_frames, clip.fade_out_frames), (30, 10));
        assert!(editor.undo());
        assert_eq!(editor.project().tracks()[2].clips[0].fade_out_frames, 30);
        let mut invalid = editor.project().tracks().to_vec();
        invalid[2].clips[0].fade_out_frames = 100;
        assert!(!editor.apply(Command::ReplaceTracks(invalid)));
        assert!(editor.apply(Command::SetTrackLocked {
            track_id: 2,
            locked: true
        }));
        let locked = editor.project().clone();
        assert!(!editor.apply(fades(2, 10, 10)));
        assert_eq!(editor.project(), &locked);
        assert!(editor.undo());
        assert!(editor.apply(fades(2, 10, 10)));
    }
}
