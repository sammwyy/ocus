use super::Project;
use crate::media::{Media, MediaKind};
use crate::timeline::{Clip, ClipContent, Track, TrackKind};

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    RenameProject(String),
    InsertClip {
        track_id: u64,
        clip: Clip,
    },
    RemoveClip {
        track_id: u64,
        clip_id: u64,
    },
    ImportMedia(Media),
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
    RippleDelete {
        track_id: u64,
        clip_id: u64,
    },
    ReplaceTracks(Vec<Track>),
}

pub struct Editor {
    project: Project,
    undo: Vec<Command>,
    redo: Vec<Command>,
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
                let track = self.project.tracks.iter_mut().find(|t| t.id == track_id)?;
                let compatible = match &clip.content {
                    ClipContent::Title(title) => {
                        track.kind == TrackKind::Video
                            && title.font_size.is_finite()
                            && title.font_size > 0.0
                            && title.x.is_finite()
                            && title.y.is_finite()
                            && (0.0..=1.0).contains(&title.x)
                            && (0.0..=1.0).contains(&title.y)
                    }
                    ClipContent::Media { media_id } => self
                        .project
                        .media
                        .iter()
                        .find(|media| media.id == *media_id)
                        .is_some_and(|media| {
                            matches!(
                                (track.kind, media.kind),
                                (TrackKind::Video, MediaKind::Video | MediaKind::Image)
                                    | (TrackKind::Audio, MediaKind::Audio)
                            )
                        }),
                };
                if !compatible {
                    return None;
                }
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
                    || self.project.media.iter().any(|m| m.id == media.id)
                {
                    return None;
                }
                let id = media.id;
                self.project.media.push(media);
                Some(Command::RemoveMedia(id))
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
            Command::RippleDelete { track_id, clip_id } => {
                let mut tracks = self.project.tracks.clone();
                let track = tracks.iter_mut().find(|t| t.id == track_id)?;
                let index = track.clips.iter().position(|c| c.id == clip_id)?;
                let removed = track.clips.remove(index);
                let start = removed.start_frame;
                let end = start.checked_add(removed.duration_frames)?;
                if tracks.iter().flat_map(|track| &track.clips).any(|clip| {
                    clip.start_frame < start
                        && clip
                            .start_frame
                            .checked_add(clip.duration_frames)
                            .is_some_and(|clip_end| clip_end > end)
                }) {
                    return None;
                }
                for track in &mut tracks {
                    track.clips.retain_mut(|clip| {
                        let clip_end = clip.start_frame + clip.duration_frames;
                        if clip_end <= start {
                            return true;
                        }
                        if clip.start_frame >= end {
                            clip.start_frame -= removed.duration_frames;
                        } else if clip.start_frame < start {
                            clip.duration_frames = start - clip.start_frame;
                        } else if clip_end > end {
                            clip.source_start_frame += end - clip.start_frame;
                            clip.start_frame = start;
                            clip.duration_frames = clip_end - end;
                        } else {
                            return false;
                        }
                        true
                    });
                }
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
            }),
            volume: 1.0,
            muted: false,
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
            }),
            volume: 1.0,
            muted: false,
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
            }),
            volume: 1.0,
            muted: false,
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
    fn ripple_delete_rejects_spanning_clips_without_losing_source_frames() {
        let mut editor = Editor::new(Project::new("Film"));
        for (track_id, id, start_frame, duration_frames) in [(0, 1, 20, 20), (1, 2, 0, 60)] {
            assert!(editor.apply(Command::InsertClip {
                track_id,
                clip: Clip {
                    id,
                    start_frame,
                    duration_frames,
                    source_start_frame: 0,
                    content: ClipContent::Title(crate::timeline::Title {
                        text: "Test".into(),
                        font_family: "Sans".into(),
                        font_size: 48.0,
                        color: [255; 4],
                        x: 0.5,
                        y: 0.5,
                    }),
                    volume: 1.0,
                    muted: false,
                },
            }));
        }
        let before = editor.project().tracks().to_vec();
        assert!(!editor.apply(Command::RippleDelete {
            track_id: 0,
            clip_id: 1
        }));
        assert_eq!(editor.project().tracks(), before);
    }
}
