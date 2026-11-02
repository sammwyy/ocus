use super::Project;
use crate::media::MediaKind;
use crate::timeline::{Clip, ClipContent, TrackKind};

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    RenameProject(String),
    InsertClip { track_id: u64, clip: Clip },
    RemoveClip { track_id: u64, clip_id: u64 },
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
            Command::RenameProject(name) if name != self.project.name() => {
                Some(Command::RenameProject(self.project.replace_name(name)))
            }
            Command::RenameProject(_) => None,
            Command::InsertClip { track_id, clip } => {
                let end = clip.start_frame.checked_add(clip.duration_frames)?;
                if clip.duration_frames == 0
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
                    ClipContent::Title(_) => track.kind == TrackKind::Video,
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
}
