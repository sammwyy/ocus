use super::Project;
use super::export_settings::LastExport;
use crate::media::{Media, MediaKind};
use crate::timeline::{Clip, ClipContent, Title, Track, TrackKind};
use std::collections::HashSet;

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    RenameProject(String),
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
    SetClipAudio {
        track_id: u64,
        clip_id: u64,
        volume: f32,
        muted: bool,
    },
    SetTitle {
        track_id: u64,
        clip_id: u64,
        title: Title,
    },
    RippleDelete {
        track_id: u64,
        clip_id: u64,
    },
    RippleDeleteSelection(Vec<(u64, u64)>),
    ReplaceTracks(Vec<Track>),
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
            .find(|track| track.id == track_id)?
            .clips
            .iter()
            .find(|clip| clip.id == clip_id)?;
        ranges.push((
            clip.start_frame,
            clip.start_frame.checked_add(clip.duration_frames)?,
        ));
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
        track
            .clips
            .retain(|clip| !selected.contains(&(track.id, clip.id)));
    }
    for (start, end) in merged.into_iter().rev() {
        let length = end - start;
        for track in &mut tracks {
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
                        right.source_start_frame = right
                            .source_start_frame
                            .checked_add(end - clip.start_frame)?;
                        right.start_frame = start;
                        right.duration_frames = clip_end - end;
                        clip.duration_frames = start - clip.start_frame;
                        kept.push(clip);
                        kept.push(right);
                    } else {
                        clip.duration_frames = start - clip.start_frame;
                        kept.push(clip);
                    }
                } else if clip_end > end {
                    clip.source_start_frame = clip
                        .source_start_frame
                        .checked_add(end - clip.start_frame)?;
                    clip.duration_frames = clip_end - end;
                    clip.start_frame = start;
                    kept.push(clip);
                }
            }
            track.clips = kept;
        }
    }
    Some(tracks)
}

pub struct Editor {
    project: Project,
    undo: Vec<Command>,
    redo: Vec<Command>,
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
                if media.kind == MediaKind::Image {
                    return None;
                }
                media.duration_us.map(|duration| {
                    (u128::from(duration) * u128::from(project.settings().frame_rate.numerator)
                        / (1_000_000 * u128::from(project.settings().frame_rate.denominator)))
                    .min(u128::from(u64::MAX)) as u64
                })
            }),
        _ => None,
    };
    let (start_frame, source_start_frame, duration_frames) = if left {
        let minimum = if matches!(clip.content, ClipContent::Media { .. }) {
            clip.start_frame.saturating_sub(clip.source_start_frame)
        } else {
            0
        }
        .max(previous);
        let start = frame.clamp(minimum, end - 1);
        let source = if start >= clip.start_frame {
            clip.source_start_frame
                .saturating_add(start - clip.start_frame)
        } else {
            clip.source_start_frame
                .saturating_sub(clip.start_frame - start)
        };
        (start, source, end - start)
    } else {
        let maximum = available
            .map_or(u64::MAX, |frames| {
                clip.start_frame
                    .saturating_add(frames.saturating_sub(clip.source_start_frame))
            })
            .min(next);
        let end = frame.clamp(clip.start_frame + 1, maximum);
        (
            clip.start_frame,
            clip.source_start_frame,
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

impl Editor {
    pub fn new(project: Project) -> Self {
        Self {
            project,
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    pub fn project(&self) -> &Project {
        &self.project
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    fn execute(&mut self, command: Command) -> Option<Command> {
        match command {
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
                        .checked_add(clip.duration_frames)
                        .is_none()
                    || !clip.volume.is_finite()
                    || !(0.0..=2.0).contains(&clip.volume)
                    || self
                        .project
                        .tracks
                        .iter()
                        .flat_map(|track| &track.clips)
                        .any(|existing| existing.id == clip.id)
                {
                    return None;
                }
                let kind = self
                    .project
                    .tracks
                    .iter()
                    .find(|track| track.id == track_id)?
                    .kind;
                let compatible = match &clip.content {
                    ClipContent::Title(title) => kind == TrackKind::Video && title.valid(),
                    ClipContent::Media { media_id } => self
                        .project
                        .media
                        .iter()
                        .find(|media| media.id == *media_id)
                        .is_some_and(|media| {
                            (media.duration_us.is_some() || media.kind == MediaKind::Image)
                                && self.project.media_clip_fits(media, kind, &clip)
                        }),
                };
                if !compatible {
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
                let mut tracks = self.project.tracks.clone();
                let track = tracks.iter_mut().find(|t| t.id == track_id)?;
                let index = track.clips.iter().position(|c| c.id == clip_id)?;
                let clip = &mut track.clips[index];
                let offset = at_frame.checked_sub(clip.start_frame)?;
                if offset == 0 || offset >= clip.duration_frames {
                    return None;
                }
                let right = Clip {
                    id: new_id,
                    start_frame: at_frame,
                    source_start_frame: clip.source_start_frame.checked_add(offset)?,
                    duration_frames: clip.duration_frames - offset,
                    ..clip.clone()
                };
                clip.duration_frames = offset;
                track.clips.insert(index + 1, right);
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
                let index = source.clips.iter().position(|c| c.id == clip_id)?;
                let mut clip = source.clips.remove(index);
                clip.start_frame = to_frame;
                let target = tracks.iter_mut().find(|t| t.id == target_track_id)?;
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
                clip.start_frame = start_frame;
                clip.source_start_frame = source_start_frame;
                clip.duration_frames = duration_frames;
                self.execute(Command::ReplaceTracks(tracks))
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
        }
    }

    pub fn apply(&mut self, command: Command) -> bool {
        let Some(inverse) = self.execute(command) else {
            return false;
        };
        self.undo.push(inverse);
        self.redo.clear();
        true
    }

    pub fn undo(&mut self) -> bool {
        let Some(command) = self.undo.pop() else {
            return false;
        };
        if let Some(inverse) = self.execute(command) {
            self.redo.push(inverse);
        }
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(command) = self.redo.pop() else {
            return false;
        };
        if let Some(inverse) = self.execute(command) {
            self.undo.push(inverse);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            id: 1,
            start_frame: 40,
            source_start_frame: 20,
            duration_frames: 30,
            content: ClipContent::Media { media_id: 1 },
            volume: 1.0,
            muted: false,
            linked_audio: false,
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
            linked_audio: false,
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
                id: 1,
                start_frame: 0,
                source_start_frame: 0,
                duration_frames: 90,
                content: ClipContent::Title(title.clone()),
                volume: 1.0,
                muted: false,
                linked_audio: false,
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
    fn clip_insertion_rejects_invalid_ranges_overlap_and_missing_media() {
        let mut editor = Editor::new(Project::new("Film"));
        let title = Clip {
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
            linked_audio: false,
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
                id: 2,
                start_frame: 99,
                ..title.clone()
            }
        ));
        assert!(insert(
            &mut editor,
            Clip {
                id: 2,
                start_frame: 100,
                ..title
            }
        ));
        assert_eq!(editor.project().tracks()[0].clips.len(), 2);
    }

    #[test]
    fn split_move_trim_and_ripple_delete_are_atomic_and_undoable() {
        let mut editor = Editor::new(Project::new("Film"));
        let make_clip = |id, start_frame| Clip {
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
            linked_audio: false,
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
                    linked_audio: id == 2,
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
            id,
            start_frame,
            source_start_frame: 0,
            duration_frames,
            content: ClipContent::Title(Title::default()),
            volume: 1.0,
            muted: false,
            linked_audio: false,
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
                    id: 6,
                    start_frame: 0,
                    source_start_frame: 0,
                    duration_frames: 120,
                    content: ClipContent::Media { media_id: 1 },
                    volume: 0.5,
                    muted: true,
                    linked_audio: false,
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
                    id,
                    start_frame,
                    source_start_frame: 0,
                    duration_frames,
                    content: ClipContent::Title(Title::default()),
                    volume: 1.0,
                    muted: false,
                    linked_audio: false,
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
            id: 2,
            start_frame: 0,
            source_start_frame: 0,
            duration_frames: 31,
            content: ClipContent::Media { media_id: 1 },
            volume: 1.0,
            muted: false,
            linked_audio: false,
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
                id: 1,
                start_frame: 0,
                source_start_frame: 0,
                duration_frames: 30,
                content: ClipContent::Media { media_id: 1 },
                volume: 1.0,
                muted: false,
                linked_audio: false,
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
                id: 1,
                start_frame: 0,
                source_start_frame: 0,
                duration_frames: 30,
                content: ClipContent::Media { media_id: 1 },
                volume: 1.0,
                muted: false,
                linked_audio: false,
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
                id: 1,
                start_frame: 0,
                source_start_frame: 0,
                duration_frames: 30,
                content: ClipContent::Media { media_id: 1 },
                volume: 1.0,
                muted: false,
                linked_audio: false,
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
}
