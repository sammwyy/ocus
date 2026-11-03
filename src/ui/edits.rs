//! Multi-clip edits planned against the current project and applied as one
//! undoable command.

use super::state::media_frames;
use crate::media::MediaKind;
use crate::project::{Command, Editor, Project};
use crate::timeline::{Clip, ClipContent, Track, TrackKind};
use std::collections::HashSet;

/// A clip identity: `(track_id, clip_id)`.
pub(crate) type ClipRef = (u64, u64);

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
        let source = tracks.iter_mut().find(|track| track.id == track_id)?;
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
            .find(|track| track.id == target)?
            .clips
            .push(clip);
    }
    Some(Planned {
        tracks: sorted_without_overlap(tracks)?,
        selection,
    })
}

/// The smallest shift that lands one of `edges` on another clip's edge, the
/// playhead, or the sequence start, when within `tolerance` frames. Clips in
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
    let mut id = next_clip_id(project)?;
    let mut tracks = project.tracks().to_vec();
    let mut changed = false;
    for &(track_id, clip_id) in targets {
        let track = tracks.iter_mut().find(|track| track.id == track_id)?;
        let Some(clip) = track.clips.iter_mut().find(|clip| clip.id == clip_id) else {
            continue;
        };
        let Some(offset) = at.checked_sub(clip.start_frame) else {
            continue;
        };
        if offset == 0 || offset >= clip.duration_frames {
            continue;
        }
        let right = Clip {
            id,
            start_frame: at,
            source_start_frame: clip.source_start_frame.checked_add(offset)?,
            duration_frames: clip.duration_frames - offset,
            ..clip.clone()
        };
        clip.duration_frames = offset;
        track.clips.push(right);
        id = id.checked_add(1)?;
        changed = true;
    }
    changed.then(|| sorted_without_overlap(tracks)).flatten()
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
        .find(|track| track.id == preferred && track.kind == wanted)
        .or_else(|| project.tracks().iter().find(|track| track.kind == wanted))
        .map(|track| track.id)
}

/// A three-second title after the last clip of the first video track.
pub(crate) fn title_clip(project: &Project) -> Option<(u64, Clip)> {
    let track = project
        .tracks()
        .iter()
        .find(|track| track.kind == TrackKind::Video)?;
    let start_frame = track
        .clips
        .iter()
        .map(|clip| clip.start_frame.checked_add(clip.duration_frames))
        .try_fold(0_u64, |latest, end| Some(latest.max(end?)))?;
    let id = next_clip_id(project)?;
    let rate = &project.settings().frame_rate;
    Some((
        track.id,
        Clip {
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
            }),
            volume: 1.0,
            muted: false,
        },
    ))
}

pub(crate) fn remove_clips(project: &Project, selection: &[ClipRef]) -> Vec<Track> {
    let selected: HashSet<_> = selection.iter().copied().collect();
    let mut tracks = project.tracks().to_vec();
    for track in &mut tracks {
        let id = track.id;
        track
            .clips
            .retain(|clip| !selected.contains(&(id, clip.id)));
    }
    tracks
}

/// Ripple-deletes each selected clip, latest first, closing every gap.
pub(crate) fn ripple_remove_clips(project: &Project, selection: &[ClipRef]) -> Option<Vec<Track>> {
    let mut ordered: Vec<_> = selection
        .iter()
        .filter_map(|&(track_id, clip_id)| {
            let track = project.tracks().iter().find(|track| track.id == track_id)?;
            let clip = track.clips.iter().find(|clip| clip.id == clip_id)?;
            Some((clip.start_frame, track_id, clip_id))
        })
        .collect();
    ordered.sort_by(|a, b| b.cmp(a));
    let mut editor = Editor::new(project.clone());
    for (_, track_id, clip_id) in ordered {
        let exists =
            editor.project().tracks().iter().any(|track| {
                track.id == track_id && track.clips.iter().any(|clip| clip.id == clip_id)
            });
        if exists && !editor.apply(Command::RippleDelete { track_id, clip_id }) {
            return None;
        }
    }
    Some(editor.project().tracks().to_vec())
}

/// Places copies of `clips` with fresh IDs so the earliest starts at `at`,
/// keeping their relative timing and tracks.
pub(crate) fn paste_clips(project: &Project, clips: &[(u64, Clip)], at: u64) -> Option<Planned> {
    let first = clips.iter().map(|(_, clip)| clip.start_frame).min()?;
    let mut id = next_clip_id(project)?;
    let mut tracks = project.tracks().to_vec();
    let mut selection = Vec::new();
    for (track_id, clip) in clips {
        let track = tracks.iter_mut().find(|track| track.id == *track_id)?;
        track.clips.push(Clip {
            id,
            start_frame: at.checked_add(clip.start_frame - first)?,
            ..clip.clone()
        });
        selection.push((*track_id, id));
        id = id.checked_add(1)?;
    }
    Some(Planned {
        tracks: sorted_without_overlap(tracks)?,
        selection,
    })
}

/// Plans inserting a source as a clip, at `placement` (`track_id`, frame) or
/// after the last clip of its first compatible track. Video with audio also
/// gets an aligned clip on the matching audio track.
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
    let primary = match placement {
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
            .find(|track| track.kind == wanted)
            .ok_or("No compatible track for this source")?,
    };
    let audio = if media.kind == MediaKind::Video && media.has_audio {
        let lane = project
            .tracks()
            .iter()
            .filter(|track| track.kind == TrackKind::Video)
            .position(|track| track.id == primary.id)
            .unwrap_or(0);
        let audio_tracks: Vec<_> = project
            .tracks()
            .iter()
            .filter(|track| track.kind == TrackKind::Audio)
            .collect();
        Some(
            *audio_tracks
                .get(lane)
                .or(audio_tracks.first())
                .ok_or("Video has audio but no audio track exists")?,
        )
    } else {
        None
    };
    let rate = &project.settings().frame_rate;
    let duration = media_frames(media, (rate.numerator, rate.denominator))
        .ok_or("Source has no usable duration; import it again to probe")?;
    let start = match placement {
        Some((_, frame)) => frame,
        None => std::iter::once(primary)
            .chain(audio)
            .flat_map(|track| &track.clips)
            .map(clip_end)
            .max()
            .unwrap_or(0),
    };
    let clip_id = next_clip_id(project).ok_or("Clip ID limit reached")?;
    let clip = Clip {
        id: clip_id,
        start_frame: start,
        source_start_frame: 0,
        duration_frames: duration,
        content: ClipContent::Media { media_id },
        volume: 1.0,
        muted: false,
    };
    let mut placed = vec![(primary.id, clip.clone())];
    if let Some(audio) = audio {
        let linked = clip_id.checked_add(1).ok_or("Clip ID limit reached")?;
        placed.push((audio.id, Clip { id: linked, ..clip }));
    }
    let mut tracks = project.tracks().to_vec();
    for (track_id, clip) in &placed {
        if let Some(track) = tracks.iter_mut().find(|track| track.id == *track_id) {
            track.clips.push(clip.clone());
        }
    }
    let tracks = sorted_without_overlap(tracks).ok_or("That spot on the track is already taken")?;
    let selection = placed
        .iter()
        .rev()
        .map(|(track_id, clip)| (*track_id, clip.id))
        .collect();
    let track_ids = placed.iter().map(|(track_id, _)| *track_id).collect();
    let command = if placed.len() == 1 {
        let (track_id, clip) = placed.remove(0);
        Command::InsertClip { track_id, clip }
    } else {
        Command::ReplaceTracks(tracks)
    };
    Ok(Insertion {
        command,
        selection,
        start,
        duration,
        tracks: track_ids,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::Media;
    use crate::timeline::Title;

    fn title(id: u64, start_frame: u64) -> Clip {
        Clip {
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
            }),
            volume: 1.0,
            muted: false,
        }
    }

    fn project_with(clips: &[(u64, Clip)]) -> Project {
        let mut editor = Editor::new(Project::new("Film"));
        for (track_id, clip) in clips {
            assert!(editor.apply(Command::InsertClip {
                track_id: *track_id,
                clip: clip.clone(),
            }));
        }
        editor.project().clone()
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
        let copied = vec![(0, title(1, 0)), (1, title(3, 60))];
        let pasted = paste_clips(&project, &copied, 90).unwrap();
        assert_eq!(pasted.selection, vec![(0, 4), (1, 5)]);
        assert_eq!(pasted.tracks[1].clips[1].start_frame, 150);
        assert!(paste_clips(&project, &copied, 10).is_none());
    }

    #[test]
    fn placed_insert_checks_track_kind_overlap_and_links_matching_audio_lane() {
        let mut editor = Editor::new(Project::new("Film"));
        assert!(editor.apply(Command::ImportMedia(Media {
            id: 1,
            path: "scene.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(1_000_000),
            has_audio: true,
        })));
        let project = editor.project().clone();
        let insertion = insert_media(&project, 1, Some((1, 45))).unwrap();
        assert_eq!(insertion.tracks, vec![1, 3]);
        assert_eq!((insertion.start, insertion.duration), (45, 30));
        assert_eq!(insertion.selection, vec![(3, 2), (1, 1)]);
        assert!(editor.apply(insertion.command));
        assert!(insert_media(editor.project(), 1, Some((2, 0))).is_err());
        assert!(insert_media(editor.project(), 1, Some((1, 60))).is_err());
        assert_eq!(insert_media(editor.project(), 1, None).unwrap().start, 0);
    }
}
