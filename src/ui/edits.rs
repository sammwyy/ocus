//! Multi-clip edits planned against the current project and applied as one
//! undoable command.

use super::state::media_frames;
use crate::media::MediaKind;
use crate::project::edit::split_tracks;
use crate::project::{Command, Editor, Project};
use crate::timeline::{Clip, ClipContent, Track, TrackKind, Transform};
use std::collections::HashSet;

/// A clip identity: `(track_id, clip_id)`.
pub(crate) type ClipRef = (u64, u64);

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CopiedClip {
    pub track_id: u64,
    pub track_kind: TrackKind,
    pub clip: Clip,
}

pub(crate) struct Planned {
    pub tracks: Vec<Track>,
    pub selection: Vec<ClipRef>,
}

pub(crate) struct Insertion {
    pub command: Command,
    pub selection: Vec<ClipRef>,
    pub start: u64,
    pub duration: u64,
    pub tracks: Vec<u64>,
}

pub(crate) fn next_clip_id(project: &Project) -> Option<u64> {
    project
        .tracks()
        .iter()
        .flat_map(|track| &track.clips)
        .map(|clip| clip.id)
        .max()
        .unwrap_or(0)
        .checked_add(1)
}

fn clip_end(clip: &Clip) -> u64 {
    clip.start_frame.saturating_add(clip.duration_frames)
}

fn sorted_without_overlap(mut tracks: Vec<Track>) -> Option<Vec<Track>> {
    for track in &mut tracks {
        track.clips.sort_by_key(|clip| clip.start_frame);
        if track
            .clips
            .windows(2)
            .any(|pair| clip_end(&pair[0]) > pair[1].start_frame)
        {
            return None;
        }
    }
    Some(tracks)
}

/// The track of the same kind `shift` lanes away from `track_id`.
fn shifted_track(tracks: &[Track], track_id: u64, shift: i64) -> Option<u64> {
    let kind = tracks.iter().find(|track| track.id == track_id)?.kind;
    let same: Vec<_> = tracks
        .iter()
        .filter(|track| track.kind == kind)
        .map(|track| track.id)
        .collect();
    let index = same.iter().position(|id| *id == track_id)? as i64 + shift;
    same.get(usize::try_from(index).ok()?).copied()
}

/// Moves the selected clips by `delta` frames. Clips on tracks of the anchor
/// track's kind also move `shift` lanes of that kind; others keep their track.
pub(crate) fn move_clips(
    project: &Project,
    selection: &[ClipRef],
    anchor_track: u64,
    shift: i64,
    delta: i64,
) -> Option<Planned> {
    let anchor_kind = project
        .tracks()
        .iter()
        .find(|track| track.id == anchor_track)?
        .kind;
    let mut tracks = project.tracks().to_vec();
    let mut moved = Vec::new();
    for &(track_id, clip_id) in selection {
        let source = tracks
            .iter_mut()
            .find(|track| track.id == track_id && !track.locked)?;
        let kind = source.kind;
        let index = source.clips.iter().position(|clip| clip.id == clip_id)?;
        let mut clip = source.clips.remove(index);
        clip.start_frame = clip.start_frame.checked_add_signed(delta)?;
        let target = if kind == anchor_kind {
            shifted_track(project.tracks(), track_id, shift)?
        } else {
            track_id
        };
        moved.push((target, clip));
    }
    let mut selection = Vec::new();
    for (target, clip) in moved {
        selection.push((target, clip.id));
        tracks
            .iter_mut()
            .find(|track| track.id == target && !track.locked)?
            .clips
            .push(clip);
    }
    Some(Planned {
        tracks: sorted_without_overlap(tracks)?,
        selection,
    })
}

/// The smallest shift that lands one of `edges` on another clip's edge, a
/// marker, the playhead, or the sequence start, when within `tolerance` frames. Clips in
/// `exclude` are the ones being moved and never attract.
pub(crate) fn snap(
    project: &Project,
    exclude: &[ClipRef],
    edges: &[i64],
    playhead: u64,
    tolerance: i64,
) -> i64 {
    let excluded: HashSet<_> = exclude.iter().copied().collect();
    let targets: Vec<i64> = [0, playhead as i64]
        .into_iter()
        .chain(project.markers().iter().map(|marker| marker.frame as i64))
        .chain(project.tracks().iter().flat_map(|track| {
            let excluded = &excluded;
            track
                .clips
                .iter()
                .filter(move |clip| !excluded.contains(&(track.id, clip.id)))
                .flat_map(|clip| [clip.start_frame as i64, clip_end(clip) as i64])
        }))
        .collect();
    edges
        .iter()
        .flat_map(|edge| targets.iter().map(move |target| target - edge))
        .filter(|distance| distance.abs() <= tolerance)
        .min_by_key(|distance| distance.abs())
        .unwrap_or(0)
}

/// Start and end frames of the selected clips.
pub(crate) fn selection_edges(project: &Project, selection: &[ClipRef]) -> Vec<i64> {
    selection
        .iter()
        .filter_map(|&(track_id, clip_id)| {
            let track = project.tracks().iter().find(|track| track.id == track_id)?;
            track.clips.iter().find(|clip| clip.id == clip_id)
        })
        .flat_map(|clip| [clip.start_frame as i64, clip_end(clip) as i64])
        .collect()
}

/// Splits each target clip that spans `at` into two adjacent clips.
pub(crate) fn split_clips(project: &Project, targets: &[ClipRef], at: u64) -> Option<Vec<Track>> {
    split_tracks(project.tracks(), targets, at, next_clip_id(project)?)
}

/// The clips under `frame`, on every track.
pub(crate) fn clips_at(project: &Project, frame: u64) -> Vec<ClipRef> {
    project
        .tracks()
        .iter()
        .flat_map(|track| {
            track
                .clips
                .iter()
                .filter(move |clip| clip.start_frame <= frame && frame < clip_end(clip))
                .map(move |clip| (track.id, clip.id))
        })
        .collect()
}

/// `preferred` when it can hold the source, else the first track that can.
pub(crate) fn compatible_track(project: &Project, media_id: u64, preferred: u64) -> Option<u64> {
    let media = project.media().iter().find(|media| media.id == media_id)?;
    let wanted = if media.kind == MediaKind::Audio {
        TrackKind::Audio
    } else {
        TrackKind::Video
    };
    project
        .tracks()
        .iter()
        .find(|track| {
            track.id == preferred && track.kind == wanted && !track.subtitles && !track.locked
        })
        .or_else(|| {
            project
                .tracks()
                .iter()
                .find(|track| track.kind == wanted && !track.subtitles && !track.locked)
        })
        .map(|track| track.id)
}

/// The first compatible track with room for the whole source at `frame`.
pub(crate) fn compatible_track_at(
    project: &Project,
    media_id: u64,
    frame: u64,
) -> Result<u64, String> {
    let mut error = "No compatible track for this source".into();
    for track in project
        .tracks()
        .iter()
        .filter(|track| compatible_track(project, media_id, track.id) == Some(track.id))
    {
        match insert_media(project, media_id, Some((track.id, frame))) {
            Ok(_) => return Ok(track.id),
            Err(reason) => error = reason,
        }
    }
    Err(error)
}

/// A three-second title at `placement` (`track_id`, frame), or after the
/// last clip of the first video track.
pub(crate) fn title_clip(project: &Project, placement: Option<(u64, u64)>) -> Option<(u64, Clip)> {
    let track = project.tracks().iter().find(|track| {
        !track.locked
            && track.kind == TrackKind::Video
            && placement.is_none_or(|(id, _)| id == track.id)
    })?;
    let start_frame = match placement {
        Some((_, frame)) => frame,
        None => track
            .clips
            .iter()
            .map(|clip| clip.start_frame.checked_add(clip.duration_frames))
            .try_fold(0_u64, |latest, end| Some(latest.max(end?)))?,
    };
    let id = next_clip_id(project)?;
    let rate = &project.settings().frame_rate;
    Some((
        track.id,
        Clip {
            effect_stack: Vec::new(),
            playback: crate::timeline::Playback::Forward,
            effect: None,
            mask: None,
            lut: None,
            chroma_key: None,
            id,
            start_frame,
            source_start_frame: 0,
            duration_frames: u64::from(rate.numerator).saturating_mul(3)
                / u64::from(rate.denominator),
            content: ClipContent::Title(crate::timeline::Title {
                text: format!("Title {id}"),
                font_family: "Sans".into(),
                font_size: 64.0,
                color: [255, 255, 255, 255],
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
            stabilization: None,
            keyframes: Vec::new(),
            transform: Transform::default(),
        },
    ))
}

pub(crate) fn remove_clips(project: &Project, selection: &[ClipRef]) -> Vec<Track> {
    let selected: HashSet<_> = selection.iter().copied().collect();
    let mut tracks = project.tracks().to_vec();
    for track in &mut tracks {
        if track.locked {
            continue;
        }
        let id = track.id;
        track
            .clips
            .retain(|clip| !selected.contains(&(id, clip.id)));
    }
    tracks
}

/// Ripple-deletes the original selection's time ranges as one edit.
pub(crate) fn ripple_remove_clips(project: &Project, selection: &[ClipRef]) -> Option<Vec<Track>> {
    let mut editor = Editor::new(project.clone());
    editor
        .apply(Command::RippleDeleteSelection(selection.to_vec()))
        .then_some(())?;
    Some(editor.project().tracks().to_vec())
}

/// Places copies of `clips` with fresh IDs so the earliest starts at `at`,
/// keeping relative timing and using the first free same-kind track when
/// the original track is gone.
pub(crate) fn paste_clips(project: &Project, clips: &[CopiedClip], at: u64) -> Option<Planned> {
    let first = clips.iter().map(|copied| copied.clip.start_frame).min()?;
    let mut id = next_clip_id(project)?;
    let mut tracks = project.tracks().to_vec();
    let mut pending = Vec::with_capacity(clips.len());
    for copied in clips {
        let preferred = tracks.iter().position(|track| {
            !track.locked && track.id == copied.track_id && track.kind == copied.track_kind
        });
        pending.push((
            preferred,
            copied.track_kind,
            Clip {
                id,
                start_frame: at.checked_add(copied.clip.start_frame - first)?,
                ..copied.clip.clone()
            },
        ));
        id = id.checked_add(1)?;
    }
    // Reserve surviving destinations before placing clips from deleted tracks.
    pending.sort_by_key(|(preferred, _, _)| preferred.is_none());
    let mut selection = Vec::with_capacity(clips.len());
    for (preferred, kind, clip) in pending {
        let track = match preferred {
            Some(index) => &mut tracks[index],
            None => {
                let end = clip.start_frame.checked_add(clip.duration_frames)?;
                tracks.iter_mut().find(|track| {
                    !track.locked
                        && track.kind == kind
                        && track.clips.iter().all(|existing| {
                            end <= existing.start_frame || clip_end(existing) <= clip.start_frame
                        })
                })?
            }
        };
        selection.push((track.id, clip.id));
        track.clips.push(clip);
    }
    selection.sort_unstable_by_key(|&(_, id)| id);
    Some(Planned {
        tracks: sorted_without_overlap(tracks)?,
        selection,
    })
}

/// Plans inserting a source as a clip, at `placement` (`track_id`, frame) or
/// after the last clip of its first compatible track. A video's own sound
/// travels inside the clip as linked audio.
pub(crate) fn insert_media(
    project: &Project,
    media_id: u64,
    placement: Option<(u64, u64)>,
) -> Result<Insertion, String> {
    let media = project
        .media()
        .iter()
        .find(|media| media.id == media_id)
        .ok_or("Source is no longer in the project")?;
    let wanted = if media.kind == MediaKind::Audio {
        TrackKind::Audio
    } else {
        TrackKind::Video
    };
    let track = match placement {
        Some((track_id, _)) => {
            let track = project
                .tracks()
                .iter()
                .find(|track| track.id == track_id)
                .ok_or("No compatible track for this source")?;
            if track.kind != wanted {
                return Err(if wanted == TrackKind::Audio {
                    "Drop audio on an audio track".into()
                } else {
                    "Drop video and stills on a video track".into()
                });
            }
            track
        }
        None => project
            .tracks()
            .iter()
            .find(|track| !track.locked && track.kind == wanted)
            .ok_or("No compatible track for this source")?,
    };
    if track.locked {
        return Err("Unlock this track before inserting clips".into());
    }
    let rate = &project.settings().frame_rate;
    let duration = media_frames(media, (rate.numerator, rate.denominator))
        .ok_or("Source has no usable duration; import it again to probe")?;
    let start = match placement {
        Some((_, frame)) => frame,
        None => track.clips.iter().map(clip_end).max().unwrap_or(0),
    };
    let clip = Clip {
        effect_stack: Vec::new(),
        playback: crate::timeline::Playback::Forward,
        effect: None,
        mask: None,
        lut: None,
        chroma_key: None,
        id: next_clip_id(project).ok_or("Clip ID limit reached")?,
        start_frame: start,
        source_start_frame: 0,
        duration_frames: duration,
        speed: 1.0,
        content: ClipContent::Media { media_id },
        volume: 1.0,
        muted: false,
        color: Default::default(),
        transition: None,
        transition_out: None,
        fade_in_frames: 0,
        fade_out_frames: 0,
        noise_reduction: false,
        linked_audio: media.kind == MediaKind::Video && media.has_audio,
        stabilization: None,
        keyframes: Vec::new(),
        transform: Transform::default(),
    };
    let end = start.checked_add(duration).ok_or("Clip range overflows")?;
    if track
        .clips
        .iter()
        .any(|existing| existing.start_frame < end && start < clip_end(existing))
    {
        return Err("That spot on the track is already taken".into());
    }
    Ok(Insertion {
        selection: vec![(track.id, clip.id)],
        start,
        duration,
        tracks: vec![track.id],
        command: Command::InsertClip {
            track_id: track.id,
            clip: Box::new(clip),
        },
    })
}

/// Adds an empty track after the last one of its kind, returning its ID.
fn push_track(tracks: &mut Vec<Track>, kind: TrackKind) -> Option<u64> {
    let id = tracks
        .iter()
        .map(|track| track.id)
        .max()
        .map_or(Some(0), |id| id.checked_add(1))?;
    let index = tracks.iter().rposition(|track| track.kind == kind).map_or(
        if kind == TrackKind::Video {
            0
        } else {
            tracks.len()
        },
        |index| index + 1,
    );
    tracks.insert(
        index,
        Track {
            id,
            kind,
            subtitles: false,
            name: String::new(),
            locked: false,
            hidden: false,
            clips: Vec::new(),
        },
    );
    Some(id)
}

pub(crate) fn add_track(project: &Project, kind: TrackKind) -> Option<(Vec<Track>, u64)> {
    let mut tracks = project.tracks().to_vec();
    let id = push_track(&mut tracks, kind)?;
    Some((tracks, id))
}

/// Removes a track and its clips; the last track of a kind stays.
pub(crate) fn remove_track(project: &Project, track_id: u64) -> Result<Vec<Track>, String> {
    if project
        .tracks()
        .iter()
        .any(|track| track.id == track_id && track.locked)
    {
        return Err("Unlock this track before deleting it".into());
    }
    let kind = project
        .tracks()
        .iter()
        .find(|track| track.id == track_id)
        .ok_or("Track no longer exists")?
        .kind;
    if project
        .tracks()
        .iter()
        .filter(|track| track.kind == kind)
        .count()
        < 2
    {
        return Err(if kind == TrackKind::Video {
            "Keep at least one video track".into()
        } else {
            "Keep at least one audio track".into()
        });
    }
    let mut tracks = project.tracks().to_vec();
    tracks.retain(|track| track.id != track_id);
    Ok(tracks)
}

/// Moves each selected video's linked audio onto a free audio track,
/// creating a new track when none has room.
pub(crate) fn detach_audio(project: &Project, selection: &[ClipRef]) -> Option<Planned> {
    let mut tracks = project.tracks().to_vec();
    let mut id = next_clip_id(project)?;
    let mut detached = Vec::new();
    let mut kept = Vec::new();
    for &(track_id, clip_id) in selection {
        let track = tracks.iter_mut().find(|track| track.id == track_id)?;
        let Some(clip) = track
            .clips
            .iter_mut()
            .find(|clip| clip.id == clip_id && clip.linked_audio)
        else {
            continue;
        };
        clip.linked_audio = false;
        detached.push(Clip {
            id,
            effect: None,
            mask: None,
            lut: None,
            stabilization: None,
            chroma_key: None,
            ..clip.clone()
        });
        clip.fade_in_frames = 0;
        clip.fade_out_frames = 0;
        clip.noise_reduction = false;
        kept.push((track_id, clip_id));
        id = id.checked_add(1)?;
    }
    if detached.is_empty() {
        return None;
    }
    for new in detached {
        let target = match tracks.iter().find(|track| {
            !track.locked
                && track.kind == TrackKind::Audio
                && track.clips.iter().all(|clip| {
                    clip_end(clip) <= new.start_frame || clip_end(&new) <= clip.start_frame
                })
        }) {
            Some(track) => track.id,
            None => push_track(&mut tracks, TrackKind::Audio)?,
        };
        let track = tracks.iter_mut().find(|track| track.id == target)?;
        kept.push((target, new.id));
        track.clips.push(new);
    }
    Some(Planned {
        tracks: sorted_without_overlap(tracks)?,
        selection: kept,
    })
}

/// Folds selected audio clips back into selected video clips that show the
/// same source frames, so they edit together again.
pub(crate) fn link_audio(project: &Project, selection: &[ClipRef]) -> Option<Planned> {
    let find = |(track_id, clip_id): ClipRef| {
        let track = project.tracks().iter().find(|track| track.id == track_id)?;
        Some((
            track.kind,
            track.clips.iter().find(|clip| clip.id == clip_id)?,
        ))
    };
    let mut pairs = Vec::new();
    for &video in selection {
        let Some((TrackKind::Video, picture)) = find(video) else {
            continue;
        };
        let ClipContent::Media { media_id } = picture.content else {
            continue;
        };
        let sound = selection.iter().copied().find(|&audio| {
            find(audio).is_some_and(|(kind, clip)| {
                kind == TrackKind::Audio
                    && clip.content == ClipContent::Media { media_id }
                    && clip.start_frame == picture.start_frame
                    && clip.source_start_frame == picture.source_start_frame
                    && clip.playback == picture.playback
                    && clip.duration_frames == picture.duration_frames
                    && clip.speed == picture.speed
            })
        });
        let has_audio = project
            .media()
            .iter()
            .any(|media| media.id == media_id && media.has_audio && media.kind == MediaKind::Video);
        if let Some(sound) = sound.filter(|_| has_audio && !picture.linked_audio) {
            pairs.push((video, sound));
        }
    }
    if pairs.is_empty() {
        return None;
    }
    let mut tracks = project.tracks().to_vec();
    for &(video, (audio_track, audio_id)) in &pairs {
        let track = tracks.iter_mut().find(|track| track.id == audio_track)?;
        let index = track.clips.iter().position(|clip| clip.id == audio_id)?;
        let sound = track.clips.remove(index);
        let clip = tracks
            .iter_mut()
            .find(|track| track.id == video.0)?
            .clips
            .iter_mut()
            .find(|clip| clip.id == video.1)?;
        clip.linked_audio = true;
        clip.volume = sound.volume;
        clip.muted = sound.muted;
        clip.fade_in_frames = sound.fade_in_frames;
        clip.fade_out_frames = sound.fade_out_frames;
        clip.noise_reduction = sound.noise_reduction;
    }
    Some(Planned {
        tracks,
        selection: pairs.into_iter().map(|(video, _)| video).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::Media;
    use crate::timeline::Title;

    fn title(id: u64, start_frame: u64) -> Clip {
        Clip {
            effect_stack: Vec::new(),
            playback: crate::timeline::Playback::Forward,
            effect: None,
            mask: None,
            lut: None,
            chroma_key: None,
            id,
            start_frame,
            source_start_frame: 0,
            duration_frames: 30,
            content: ClipContent::Title(Title {
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
            stabilization: None,
            keyframes: Vec::new(),
            transform: Transform::default(),
        }
    }

    fn project_with(clips: &[(u64, Clip)]) -> Project {
        let mut editor = Editor::new(Project::new("Film"));
        for (track_id, clip) in clips {
            assert!(editor.apply(Command::InsertClip {
                track_id: *track_id,
                clip: Box::new(clip.clone()),
            }));
        }
        editor.project().clone()
    }

    #[test]
    fn locked_tracks_are_skipped_by_placement_and_refuse_clip_plans() {
        let project = project_with(&[(0, title(1, 0)), (1, title(2, 0))]);
        let copied = CopiedClip {
            track_id: 0,
            track_kind: TrackKind::Video,
            clip: title(1, 0),
        };
        let mut editor = Editor::new(project);
        assert!(editor.apply(Command::ImportMedia(Media {
            id: 1,
            path: "/tmp/picture.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(1_000_000),
            has_audio: false,
        })));
        assert!(editor.apply(Command::SetTrackLocked {
            track_id: 0,
            locked: true,
        }));
        let project = editor.project();
        assert_eq!(compatible_track(project, 1, 0), Some(1));
        assert_eq!(title_clip(project, None).unwrap().0, 1);
        assert!(title_clip(project, Some((0, 60))).is_none());
        assert!(insert_media(project, 1, Some((0, 60))).is_err());
        assert!(remove_track(project, 0).is_err());
        assert_eq!(remove_clips(project, &[(0, 1)])[0], project.tracks()[0]);
        assert!(move_clips(project, &[(0, 1)], 0, 1, 60).is_none());
        assert!(move_clips(project, &[(1, 2)], 1, -1, 60).is_none());
        let pasted = paste_clips(project, std::slice::from_ref(&copied), 60).unwrap();
        assert_eq!(pasted.tracks[0], project.tracks()[0]);
        assert_eq!(pasted.selection[0].0, 1);
        assert!(editor.apply(Command::SetTrackLocked {
            track_id: 1,
            locked: true,
        }));
        assert!(compatible_track(editor.project(), 1, 0).is_none());
        assert!(title_clip(editor.project(), None).is_none());
        assert!(insert_media(editor.project(), 1, None).is_err());
        assert!(paste_clips(editor.project(), &[copied], 60).is_none());
    }

    #[test]
    fn moving_a_selection_keeps_spacing_changes_lanes_and_rejects_overlap() {
        let project = project_with(&[(0, title(1, 0)), (0, title(2, 40)), (1, title(3, 100))]);
        let moved = move_clips(&project, &[(0, 1), (0, 2)], 0, 1, 10).unwrap();
        assert_eq!(moved.selection, vec![(1, 1), (1, 2)]);
        let lane = &moved.tracks[1].clips;
        assert_eq!(
            lane.iter()
                .map(|c| (c.id, c.start_frame))
                .collect::<Vec<_>>(),
            vec![(1, 10), (2, 50), (3, 100)]
        );
        assert!(move_clips(&project, &[(0, 2)], 0, 1, 50).is_none());
        assert!(move_clips(&project, &[(0, 1)], 0, 0, -1).is_none());
        assert!(move_clips(&project, &[(0, 1)], 0, 2, 0).is_none());
        let snapped = |delta: i64, playhead| {
            let edges: Vec<_> = selection_edges(&project, &[(0, 2)])
                .into_iter()
                .map(|edge| edge + delta)
                .collect();
            delta + snap(&project, &[(0, 2)], &edges, playhead, 3)
        };
        assert_eq!(snapped(32, 0), 30);
        assert_eq!(snapped(20, 0), 20);
        assert_eq!(snapped(22, 95), 25);
        let split = split_clips(&project, &clips_at(&project, 15), 15).unwrap();
        assert_eq!(
            split[0]
                .clips
                .iter()
                .map(|c| (c.id, c.start_frame, c.duration_frames))
                .collect::<Vec<_>>(),
            vec![(1, 0, 15), (4, 15, 15), (2, 40, 30)]
        );
        assert!(split_clips(&project, &[(0, 1)], 30).is_none());
    }

    #[test]
    fn deleting_pasting_and_rippling_several_clips() {
        let project = project_with(&[(0, title(1, 0)), (0, title(2, 30)), (1, title(3, 60))]);
        let tracks = remove_clips(&project, &[(0, 1), (1, 3)]);
        assert_eq!(tracks[0].clips.len(), 1);
        assert!(tracks[1].clips.is_empty());
        let rippled = ripple_remove_clips(&project, &[(0, 1), (0, 2)]).unwrap();
        assert!(rippled[0].clips.is_empty());
        assert_eq!(rippled[1].clips[0].start_frame, 0);
        let copied = [(0, title(1, 0)), (1, title(3, 60))].map(|(track_id, clip)| CopiedClip {
            track_id,
            track_kind: TrackKind::Video,
            clip,
        });
        let pasted = paste_clips(&project, &copied, 90).unwrap();
        assert_eq!(pasted.selection, vec![(0, 4), (1, 5)]);
        assert_eq!(pasted.tracks[1].clips[1].start_frame, 150);
        assert!(paste_clips(&project, &copied, 10).is_none());
    }

    #[test]
    fn copied_video_keeps_lut_and_detached_audio_drops_it() {
        let mut editor = Editor::new(Project::new("LUT copies"));
        assert!(editor.apply(Command::ImportMedia(Media {
            id: 1,
            path: "scene.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(3_000_000),
            has_audio: true
        })));
        let inserted = insert_media(editor.project(), 1, Some((0, 0))).unwrap();
        assert!(editor.apply(inserted.command));
        let path = Some(std::path::PathBuf::from("look.cube"));
        let key = Some(crate::timeline::ChromaKey::default());
        assert!(editor.apply(Command::SetClipChromaKey {
            track_id: 0,
            clip_id: 1,
            key,
        }));
        assert!(editor.apply(Command::SetClipLut {
            track_id: 0,
            clip_id: 1,
            path: path.clone()
        }));
        let copied = CopiedClip {
            track_id: 0,
            track_kind: TrackKind::Video,
            clip: editor.project().tracks()[0].clips[0].clone(),
        };
        let pasted = paste_clips(editor.project(), &[copied], 90).unwrap();
        assert!(editor.apply(Command::ReplaceTracks(pasted.tracks)));
        assert!(
            editor.project().tracks()[0]
                .clips
                .iter()
                .all(|clip| clip.lut == path && clip.chroma_key == key)
        );
        let detached = detach_audio(editor.project(), &[(0, 1)]).unwrap();
        assert!(editor.apply(Command::ReplaceTracks(detached.tracks)));
        assert!(editor.project().tracks()[2].clips[0].lut.is_none());
        assert!(editor.project().tracks()[2].clips[0].chroma_key.is_none());
        assert_eq!(editor.project().tracks()[0].clips[0].chroma_key, key);
        assert_eq!(editor.project().tracks()[0].clips[0].lut, path);
    }

    #[test]
    fn ripple_selection_across_tracks_uses_original_overlapping_ranges() {
        let project = project_with(&[
            (0, title(1, 20)),
            (0, title(2, 50)),
            (1, title(3, 30)),
            (1, title(4, 80)),
        ]);
        let selection = [(0, 1), (1, 3)];
        let tracks = ripple_remove_clips(&project, &selection).unwrap();
        assert_eq!(
            tracks[0]
                .clips
                .iter()
                .map(|clip| (clip.id, clip.start_frame))
                .collect::<Vec<_>>(),
            vec![(2, 20)]
        );
        assert_eq!(
            tracks[1]
                .clips
                .iter()
                .map(|clip| (clip.id, clip.start_frame))
                .collect::<Vec<_>>(),
            vec![(4, 40)]
        );
        let mut editor = Editor::new(project);
        assert!(editor.apply(Command::ReplaceTracks(tracks)));
        assert!(editor.undo());
        assert_eq!(editor.project().tracks()[0].clips[0].start_frame, 20);
        assert_eq!(editor.project().tracks()[1].clips[1].start_frame, 80);
    }

    #[test]
    fn detaching_stabilized_video_keeps_motion_only_on_the_picture() {
        let mut editor = Editor::new(Project::new("Stabilized video"));
        assert!(editor.apply(Command::ImportMedia(Media {
            id: 1,
            path: "shake.mkv".into(),
            kind: MediaKind::Video,
            duration_us: Some(3_000_000),
            has_audio: true,
        })));
        let insertion = insert_media(editor.project(), 1, Some((0, 0))).unwrap();
        assert!(editor.apply(insertion.command));
        let data = crate::timeline::Stabilization::new(0, vec![[0.0, 0.0]; 90]);
        assert!(editor.apply(Command::SetClipStabilization {
            track_id: 0,
            clip_id: 1,
            stabilization: Some(data.clone())
        }));
        let detached = detach_audio(editor.project(), &[(0, 1)]).unwrap();
        assert!(editor.apply(Command::ReplaceTracks(detached.tracks)));
        assert_eq!(
            editor.project().tracks()[0].clips[0].stabilization,
            Some(data)
        );
        assert!(
            editor
                .project()
                .tracks()
                .iter()
                .filter(|t| t.kind == TrackKind::Audio)
                .flat_map(|t| &t.clips)
                .all(|c| c.stabilization.is_none())
        );
        assert!(editor.undo());
        assert!(editor.project().tracks()[0].clips[0].linked_audio);
    }

    #[test]
    fn overlapping_video_audio_detaches_to_tracks_with_room_in_one_edit() {
        for occupied in 0..=2 {
            let mut editor = Editor::new(Project::new("Film"));
            assert!(editor.apply(Command::ImportMedia(Media {
                id: 1,
                path: "scene.mp4".into(),
                kind: MediaKind::Video,
                duration_us: Some(3_000_000),
                has_audio: true,
            })));
            for track_id in 0..2 {
                let insertion = insert_media(editor.project(), 1, Some((track_id, 45))).unwrap();
                assert!(editor.apply(insertion.command));
            }
            for track_id in 2..2 + occupied {
                let mut clip = title(track_id + 10, 45);
                clip.content = ClipContent::Media { media_id: 1 };
                assert!(editor.apply(Command::InsertClip {
                    track_id,
                    clip: Box::new(clip)
                }));
            }
            let before = editor.project().tracks().to_vec();
            assert!(detach_audio(editor.project(), &[]).is_none());
            let detached = detach_audio(editor.project(), &[(0, 1), (1, 2)])
                .expect("overlapping selected videos should detach onto separate audio tracks");
            assert_eq!(detached.selection.len(), 4);
            assert_eq!(detached.tracks.len(), 4 + occupied as usize);
            for (track_id, video) in [(0, &before[0].clips[0]), (1, &before[1].clips[0])] {
                let mut expected = video.clone();
                expected.linked_audio = false;
                assert_eq!(detached.tracks[track_id].clips, vec![expected.clone()]);
                let audio_track = &detached.tracks[2 + occupied as usize + track_id];
                assert_eq!(audio_track.kind, TrackKind::Audio);
                assert_eq!(audio_track.clips.len(), 1);
                expected.id = audio_track.clips[0].id;
                assert_eq!(audio_track.clips[0], expected);
            }
            assert!(editor.apply(Command::ReplaceTracks(detached.tracks.clone())));
            assert!(detach_audio(editor.project(), &[(0, 1), (1, 2)]).is_none());
            assert!(editor.undo());
            assert_eq!(editor.project().tracks(), before);
            assert!(editor.redo());
            assert_eq!(editor.project().tracks(), detached.tracks);
        }
    }

    #[test]
    fn video_with_sound_inserts_one_linked_clip_that_detaches_and_relinks() {
        let mut editor = Editor::new(Project::new("Film"));
        assert!(editor.apply(Command::ImportMedia(Media {
            id: 1,
            path: "scene.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(3_000_000),
            has_audio: true,
        })));
        let insertion = insert_media(editor.project(), 1, Some((1, 45))).unwrap();
        assert_eq!(insertion.tracks, vec![1]);
        assert_eq!((insertion.start, insertion.duration), (45, 90));
        assert!(editor.apply(insertion.command));
        assert!(editor.project().tracks()[1].clips[0].linked_audio);
        assert!(editor.project().tracks()[2].clips.is_empty());
        assert!(insert_media(editor.project(), 1, Some((2, 0))).is_err());
        assert!(insert_media(editor.project(), 1, Some((1, 60))).is_err());
        assert!(editor.apply(Command::SetClipAudio {
            track_id: 1,
            clip_id: 1,
            volume: 0.5,
            muted: false,
        }));
        assert!(editor.apply(Command::SetClipFades {
            track_id: 1,
            clip_id: 1,
            fade_in_frames: 15,
            fade_out_frames: 15,
        }));
        assert!(editor.apply(Command::SetClipNoiseReduction {
            track_id: 1,
            clip_id: 1,
            enabled: true,
        }));

        let split = split_clips(editor.project(), &[(1, 1)], 75).unwrap();
        assert!(editor.apply(Command::ReplaceTracks(split)));
        let split = split_clips(editor.project(), &[(1, 2)], 105).unwrap();
        assert!(editor.apply(Command::ReplaceTracks(split)));
        let rippled = ripple_remove_clips(editor.project(), &[(1, 2)]).unwrap();
        assert!(editor.apply(Command::ReplaceTracks(rippled)));
        let pieces = &editor.project().tracks()[1].clips;
        assert!(pieces.iter().all(|c| c.noise_reduction));
        assert_eq!(
            pieces
                .iter()
                .map(|c| (
                    c.start_frame,
                    c.source_start_frame,
                    c.duration_frames,
                    c.linked_audio
                ))
                .collect::<Vec<_>>(),
            vec![(45, 0, 30, true), (75, 60, 30, true)]
        );

        let detached = detach_audio(editor.project(), &[(1, 1), (1, 3)]).unwrap();
        assert!(editor.apply(Command::ReplaceTracks(detached.tracks)));
        let audio = &editor.project().tracks()[2].clips;
        assert_eq!((audio[0].fade_in_frames, audio[0].fade_out_frames), (15, 0));
        assert_eq!((audio[1].fade_in_frames, audio[1].fade_out_frames), (0, 15));
        assert!(
            editor.project().tracks()[1]
                .clips
                .iter()
                .all(|clip| clip.fade_in_frames == 0 && clip.fade_out_frames == 0)
        );
        assert!(audio.iter().all(|c| c.noise_reduction));
        assert_eq!(
            audio
                .iter()
                .map(|c| (c.start_frame, c.source_start_frame, c.volume))
                .collect::<Vec<_>>(),
            vec![(45, 0, 0.5), (75, 60, 0.5)]
        );
        assert!(
            editor.project().tracks()[1]
                .clips
                .iter()
                .all(|c| !c.linked_audio && !c.noise_reduction)
        );
        assert_eq!(detached.selection.len(), 4);

        let relinked = link_audio(editor.project(), &detached.selection).unwrap();
        assert!(editor.apply(Command::ReplaceTracks(relinked.tracks)));
        assert!(editor.project().tracks()[2].clips.is_empty());
        let picture = &editor.project().tracks()[1].clips;
        assert_eq!(
            (picture[0].fade_in_frames, picture[0].fade_out_frames),
            (15, 0)
        );
        assert_eq!(
            (picture[1].fade_in_frames, picture[1].fade_out_frames),
            (0, 15)
        );
        assert!(
            editor.project().tracks()[1]
                .clips
                .iter()
                .all(|c| c.linked_audio && c.noise_reduction)
        );
    }

    #[test]
    fn tracks_are_added_beside_their_kind_and_the_last_one_stays() {
        let project = Project::new("Film");
        let (tracks, id) = add_track(&project, TrackKind::Video).unwrap();
        assert_eq!(id, 4);
        assert_eq!(
            tracks.iter().map(|t| (t.id, t.kind)).collect::<Vec<_>>(),
            vec![
                (0, TrackKind::Video),
                (1, TrackKind::Video),
                (4, TrackKind::Video),
                (2, TrackKind::Audio),
                (3, TrackKind::Audio),
            ]
        );
        let mut editor = Editor::new(project);
        assert!(editor.apply(Command::ReplaceTracks(
            remove_track(editor.project(), 3).unwrap()
        )));
        assert!(remove_track(editor.project(), 2).is_err());
        assert!(editor.apply(Command::ReplaceTracks(
            remove_track(editor.project(), 1).unwrap()
        )));
        assert!(remove_track(editor.project(), 0).is_err());

        let mut full = Editor::new(Project::new("Busy"));
        assert!(full.apply(Command::ImportMedia(Media {
            id: 1,
            path: "scene.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(1_000_000),
            has_audio: true,
        })));
        for (track_id, audio) in [(2, false), (3, false), (0, true)] {
            let mut clip = title(track_id + 10, 0);
            clip.content = ClipContent::Media { media_id: 1 };
            clip.linked_audio = audio;
            assert!(full.apply(Command::InsertClip {
                track_id,
                clip: Box::new(clip)
            }));
        }
        let detached = detach_audio(full.project(), &[(0, 10)]).unwrap();
        assert_eq!(detached.tracks.len(), 5);
        assert_eq!(detached.tracks[4].kind, TrackKind::Audio);
        assert_eq!(detached.tracks[4].clips.len(), 1);
        assert!(full.apply(Command::ReplaceTracks(detached.tracks)));
    }
}
