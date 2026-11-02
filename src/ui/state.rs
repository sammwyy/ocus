use crate::media::{Media, MediaProbe, probe};
use crate::project::{Command, Editor, Project, store};
use creamui::widgets::{ScrollController, TextController};
use creamui::{AppHandle, Signal};
use rfd::{FileDialog, MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Sidebar {
    Media,
    Inspector,
}

#[derive(Clone)]
pub(crate) struct UiState {
    pub editor: Rc<RefCell<Editor>>,
    pub revision: Signal<u64>,
    pub selected: Signal<Option<(u64, u64)>>,
    pub playhead: Signal<u64>,
    pub project_path: Signal<Option<PathBuf>>,
    pub title_text: TextController,
    pub status: Signal<String>,
    pub dirty: Signal<bool>,
    pub busy: Signal<bool>,
    pub media_scroll: ScrollController,
    pub inspector_scroll: ScrollController,
    pub timeline_scroll: ScrollController,
    pub timeline_page: Signal<u64>,
    pub sidebar: Signal<Sidebar>,
    app: Rc<RefCell<Option<AppHandle>>>,
}

impl UiState {
    pub fn new() -> Self {
        Self {
            editor: Rc::new(RefCell::new(Editor::new(Project::new("Untitled")))),
            revision: Signal::new(0),
            selected: Signal::new(None),
            playhead: Signal::new(0),
            project_path: Signal::new(None),
            title_text: TextController::default(),
            status: Signal::new("Ready · Project not saved".into()),
            dirty: Signal::new(false),
            busy: Signal::new(false),
            media_scroll: ScrollController::default(),
            inspector_scroll: ScrollController::default(),
            timeline_scroll: ScrollController::default(),
            timeline_page: Signal::new(0),
            sidebar: Signal::new(Sidebar::Media),
            app: Rc::new(RefCell::new(None)),
        }
    }

    pub fn connect(&self, app: AppHandle) {
        *self.app.borrow_mut() = Some(app);
    }

    pub fn edit(&self, command: Command) -> bool {
        let changed = self.editor.borrow_mut().apply(command);
        if changed {
            self.revision.update(|revision| *revision += 1);
            self.dirty.set(true);
            self.status.set("Unsaved changes".into());
        } else {
            self.status
                .set("Edit rejected: check range, track and clip".into());
        }
        changed
    }

    pub fn undo(&self) {
        if self.editor.borrow_mut().undo() {
            self.revision.update(|revision| *revision += 1);
            self.dirty.set(true);
            self.status.set("Undid edit · Unsaved changes".into());
        }
    }

    pub fn redo(&self) {
        if self.editor.borrow_mut().redo() {
            self.revision.update(|revision| *revision += 1);
            self.dirty.set(true);
            self.status.set("Redid edit · Unsaved changes".into());
        }
    }

    pub fn focus_frame(&self, frame: u64) {
        let editor = self.editor.borrow();
        let frame_rate = &editor.project().settings().frame_rate;
        let page_frames =
            (u64::from(frame_rate.numerator) * 10 / u64::from(frame_rate.denominator)).max(1);
        drop(editor);
        self.playhead.set(frame);
        self.timeline_page.set(frame / page_frames);
    }

    pub fn confirm_discard(&self) -> bool {
        !self.dirty.peek()
            || MessageDialog::new()
                .set_level(MessageLevel::Warning)
                .set_title("Unsaved changes")
                .set_description("Discard your unsaved changes?")
                .set_buttons(MessageButtons::YesNo)
                .show()
                == MessageDialogResult::Yes
    }

    pub fn new_project(&self) {
        if self.busy.peek() || !self.confirm_discard() {
            return;
        }
        *self.editor.borrow_mut() = Editor::new(Project::new("Untitled"));
        self.project_path.set(None);
        self.selected.set(None);
        self.playhead.set(0);
        self.timeline_page.set(0);
        self.sidebar.set(Sidebar::Media);
        self.dirty.set(false);
        self.revision.update(|revision| *revision += 1);
        self.status.set("New project · Not saved".into());
    }

    pub fn open_dialog(&self) {
        if self.busy.peek() {
            return;
        }
        if let Some(path) = FileDialog::new()
            .set_title("Open Ocus project")
            .add_filter("Ocus project", &["ocus"])
            .pick_file()
            && self.confirm_discard()
        {
            self.load_path(path);
        }
    }

    pub fn load_path(&self, path: PathBuf) {
        let Some(app) = self.app.borrow().clone() else {
            return;
        };
        if self.busy.peek() {
            return;
        }
        self.busy.set(true);
        let revision = self.revision.peek();
        self.status.set("Opening project…".into());
        let state = self.clone();
        app.spawn_background(
            {
                let path = path.clone();
                move || store::load(&path)
            },
            move |result| {
                state.busy.set(false);
                match result {
                    Ok(project) if state.revision.peek() == revision => {
                        *state.editor.borrow_mut() = Editor::new(project);
                        state.project_path.set(Some(path));
                        state.selected.set(None);
                        state.playhead.set(0);
                        state.timeline_page.set(0);
                        state.revision.update(|revision| *revision += 1);
                        state.dirty.set(false);
                        state.sidebar.set(Sidebar::Media);
                        state.status.set("Project opened".into());
                    }
                    Ok(_) => state
                        .status
                        .set("Open cancelled: project changed while loading".into()),
                    Err(error) => state.status.set(format!("Open failed: {error}")),
                }
            },
        );
    }

    pub fn save(&self) {
        if let Some(path) = self.project_path.peek() {
            self.save_to(path);
        } else {
            self.save_as_dialog();
        }
    }

    pub fn save_as_dialog(&self) {
        if self.busy.peek() {
            return;
        }
        let current = self.project_path.peek();
        let mut dialog = FileDialog::new()
            .set_title("Save Ocus project")
            .add_filter("Ocus project", &["ocus"])
            .set_file_name(
                current
                    .as_ref()
                    .and_then(|path| path.file_name())
                    .and_then(|name| name.to_str())
                    .unwrap_or("Untitled.ocus"),
            );
        if let Some(directory) = current.as_ref().and_then(|path| path.parent()) {
            dialog = dialog.set_directory(directory);
        }
        let Some(selected_path) = dialog.save_file() else {
            return;
        };
        let path = match project_file(selected_path.clone()) {
            Ok(path) => path,
            Err(error) => {
                self.status.set(error);
                return;
            }
        };
        if path != selected_path && self.project_path.peek().as_ref() != Some(&path) {
            let Some(app) = self.app.borrow().clone() else {
                return;
            };
            self.busy.set(true);
            let state = self.clone();
            app.spawn_background(
                {
                    let path = path.clone();
                    move || path.exists()
                },
                move |exists| {
                    state.busy.set(false);
                    if exists
                        && MessageDialog::new()
                            .set_level(MessageLevel::Warning)
                            .set_title("Replace project?")
                            .set_description("A project with this name already exists. Replace it?")
                            .set_buttons(MessageButtons::YesNo)
                            .show()
                            != MessageDialogResult::Yes
                    {
                        return;
                    }
                    state.save_to(path);
                },
            );
            return;
        }
        self.save_to(path);
    }

    pub fn save_to(&self, path: PathBuf) {
        let Some(app) = self.app.borrow().clone() else {
            return;
        };
        if self.busy.peek() {
            return;
        }
        let project = self.editor.borrow().project().clone();
        let revision = self.revision.peek();
        self.busy.set(true);
        self.status.set("Saving project…".into());
        let state = self.clone();
        app.spawn_background(
            {
                let path = path.clone();
                move || store::save(&path, &project)
            },
            move |result| {
                state.busy.set(false);
                state.status.set(match result {
                    Ok(()) if state.revision.peek() == revision => {
                        state.project_path.set(Some(path));
                        state.dirty.set(false);
                        "Project saved".into()
                    }
                    Ok(()) => "Saved previous version · Unsaved changes".into(),
                    Err(error) => format!("Save failed: {error}"),
                });
            },
        );
    }

    pub fn import_dialog(&self) {
        if self.busy.peek() {
            return;
        }
        if let Some(paths) = FileDialog::new().set_title("Import media").pick_files() {
            self.import_paths(paths);
        }
    }

    pub fn import_paths(&self, paths: Vec<PathBuf>) {
        let Some(app) = self.app.borrow().clone() else {
            return;
        };
        if self.busy.peek() || paths.is_empty() {
            return;
        }
        self.busy.set(true);
        self.status.set(format!("Probing {} files…", paths.len()));
        let state = self.clone();
        app.spawn_background(
            move || {
                paths
                    .into_iter()
                    .map(|path| inspect_source(&path))
                    .collect::<Vec<_>>()
            },
            move |results| {
                state.busy.set(false);
                let total = results.len();
                let mut imported = 0;
                let mut errors = Vec::new();
                for result in results {
                    match result {
                        Ok((path, info)) => {
                            let existing = state
                                .editor
                                .borrow()
                                .project()
                                .media()
                                .iter()
                                .find(|media| media.path == path)
                                .cloned();
                            let id = existing.as_ref().map(|media| media.id).or_else(|| {
                                state
                                    .editor
                                    .borrow()
                                    .project()
                                    .media()
                                    .iter()
                                    .map(|media| media.id)
                                    .max()
                                    .unwrap_or(0)
                                    .checked_add(1)
                            });
                            if let Some(id) = id {
                                let media = Media {
                                    id,
                                    path,
                                    kind: info.kind,
                                    duration_us: info.duration_us,
                                    has_audio: info.has_audio,
                                };
                                if existing.as_ref() == Some(&media) {
                                    continue;
                                } else if state.edit(if existing.is_some() {
                                    Command::ReprobeMedia(media)
                                } else {
                                    Command::ImportMedia(media)
                                }) {
                                    imported += 1;
                                } else {
                                    errors.push("Source rejected by project validation".into());
                                }
                            } else {
                                errors.push("Media ID limit reached".to_string());
                            }
                        }
                        Err(error) => errors.push(error),
                    }
                }
                state.status.set(if errors.is_empty() {
                    format!("{imported} of {total} files imported")
                } else {
                    format!(
                        "{imported} of {total} imported · {} failed: {}",
                        errors.len(),
                        errors[0]
                    )
                });
            },
        );
    }

    pub fn insert_media(&self, media_id: u64) {
        let editor = self.editor.borrow();
        let project = editor.project();
        let Some(media) = project.media().iter().find(|media| media.id == media_id) else {
            return;
        };
        let primary_kind = match media.kind {
            crate::media::MediaKind::Audio => crate::timeline::TrackKind::Audio,
            _ => crate::timeline::TrackKind::Video,
        };
        let Some(primary) = project
            .tracks()
            .iter()
            .find(|track| track.kind == primary_kind)
        else {
            self.status
                .set("No compatible track for this source".into());
            return;
        };
        let audio = if media.kind == crate::media::MediaKind::Video && media.has_audio {
            match project
                .tracks()
                .iter()
                .find(|track| track.kind == crate::timeline::TrackKind::Audio)
            {
                Some(track) => Some(track),
                None => {
                    self.status
                        .set("Video has audio but no audio track exists".into());
                    return;
                }
            }
        } else {
            None
        };
        let frame_rate = &project.settings().frame_rate;
        let frame_count = if let Some(duration_us) = media.duration_us {
            let count = u128::from(duration_us) * u128::from(frame_rate.numerator)
                / (1_000_000_u128 * u128::from(frame_rate.denominator));
            u64::try_from(count).ok()
        } else if media.kind == crate::media::MediaKind::Image {
            Some((u64::from(frame_rate.numerator) * 3 / u64::from(frame_rate.denominator)).max(1))
        } else {
            None
        };
        let Some(duration_frames) = frame_count.filter(|count| *count > 0) else {
            self.status
                .set("Source has no usable duration; import it again to probe".into());
            return;
        };
        let Some(start_frame) = std::iter::once(primary)
            .chain(audio)
            .flat_map(|track| &track.clips)
            .map(|clip| clip.start_frame.checked_add(clip.duration_frames))
            .try_fold(0_u64, |latest, end| Some(latest.max(end?)))
        else {
            return;
        };
        let Some(clip_id) = project
            .tracks()
            .iter()
            .flat_map(|track| &track.clips)
            .map(|clip| clip.id)
            .max()
            .unwrap_or(0)
            .checked_add(1)
        else {
            return;
        };
        let track_id = primary.id;
        let audio_track_id = audio.map(|track| track.id);
        let clip = crate::timeline::Clip {
            id: clip_id,
            start_frame,
            source_start_frame: 0,
            duration_frames,
            content: crate::timeline::ClipContent::Media { media_id },
            volume: 1.0,
            muted: false,
        };
        let command = if let Some(audio_id) = audio_track_id {
            let Some(linked_id) = clip_id.checked_add(1) else {
                self.status.set("Clip ID limit reached".into());
                return;
            };
            let mut tracks = project.tracks().to_vec();
            for (id, clip) in [
                (track_id, clip.clone()),
                (
                    audio_id,
                    crate::timeline::Clip {
                        id: linked_id,
                        ..clip
                    },
                ),
            ] {
                let Some(track) = tracks.iter_mut().find(|track| track.id == id) else {
                    return;
                };
                track.clips.push(clip);
            }
            Command::ReplaceTracks(tracks)
        } else {
            Command::InsertClip { track_id, clip }
        };
        drop(editor);
        if self.edit(command) {
            self.selected.set(Some((track_id, clip_id)));
            self.sidebar.set(Sidebar::Inspector);
            self.focus_frame(start_frame);
        }
    }
}

fn project_file(path: PathBuf) -> Result<PathBuf, String> {
    match path.extension().and_then(|extension| extension.to_str()) {
        None => Ok(path.with_extension("ocus")),
        Some(extension) if extension.eq_ignore_ascii_case("ocus") => {
            if path
                .file_stem()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.to_ascii_lowercase().ends_with(".ocus"))
            {
                Ok(path.with_extension(""))
            } else {
                Ok(path)
            }
        }
        _ => Err("Save the project with a .ocus extension".into()),
    }
}

fn inspect_source(path: &Path) -> Result<(PathBuf, MediaProbe), String> {
    let info = probe(path)?;
    let path = path
        .canonicalize()
        .map_err(|error| format!("Cannot resolve source: {error}"))?;
    Ok((path, info))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reference_import_checks_file_and_extension_without_modifying_source() {
        let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        assert!(inspect_source(&file).is_err());
        let missing = std::env::temp_dir().join("ocus-no-such-media.mp4");
        assert!(inspect_source(&missing).is_err());
    }

    #[test]
    fn inserting_probed_audio_creates_reversible_clip_on_audio_track() {
        let state = UiState::new();
        assert!(state.edit(Command::ImportMedia(Media {
            id: 4,
            path: "tone.wav".into(),
            kind: crate::media::MediaKind::Audio,
            duration_us: Some(1_000_000),
            has_audio: true,
        })));
        state.insert_media(4);
        let editor = state.editor.borrow();
        assert_eq!(editor.project().tracks()[2].clips.len(), 1);
        assert_eq!(editor.project().tracks()[2].clips[0].duration_frames, 30);
        assert!(editor.project().tracks()[0].clips.is_empty());
        drop(editor);
        state.undo();
        assert!(state.editor.borrow().project().tracks()[2].clips.is_empty());
    }

    #[test]
    fn inserting_video_with_audio_aligns_tracks_and_undoes_together() {
        let state = UiState::new();
        assert!(state.edit(Command::ImportMedia(Media {
            id: 4,
            path: "scene.mp4".into(),
            kind: crate::media::MediaKind::Video,
            duration_us: Some(2_000_000),
            has_audio: true,
        })));
        state.insert_media(4);
        let editor = state.editor.borrow();
        let video = &editor.project().tracks()[0].clips;
        let audio = &editor.project().tracks()[2].clips;
        assert_eq!(video.len(), 1);
        assert_eq!(audio.len(), 1);
        assert_eq!(video[0].start_frame, audio[0].start_frame);
        assert_eq!(video[0].duration_frames, audio[0].duration_frames);
        assert_ne!(video[0].id, audio[0].id);
        drop(editor);
        state.undo();
        assert!(state.editor.borrow().project().tracks()[0].clips.is_empty());
        assert!(state.editor.borrow().project().tracks()[2].clips.is_empty());
    }

    #[test]
    fn saving_projects_adds_the_extension_without_overwriting_another_format() {
        assert_eq!(
            project_file(PathBuf::from("cut")).unwrap(),
            PathBuf::from("cut.ocus")
        );
        assert_eq!(
            project_file(PathBuf::from("cut.OCUS")).unwrap(),
            PathBuf::from("cut.OCUS")
        );
        assert_eq!(
            project_file(PathBuf::from("cut.ocus.ocus")).unwrap(),
            PathBuf::from("cut.ocus")
        );
        assert!(project_file(PathBuf::from("cut.mp4")).is_err());
    }

    #[test]
    fn new_project_clears_previous_file_and_timeline_position() {
        let state = UiState::new();
        state.project_path.set(Some("previous.ocus".into()));
        state.focus_frame(600);
        state.sidebar.set(Sidebar::Inspector);
        state.new_project();
        assert_eq!(state.project_path.peek(), None);
        assert_eq!(state.playhead.peek(), 0);
        assert_eq!(state.timeline_page.peek(), 0);
        assert!(state.sidebar.peek() == Sidebar::Media);
        assert_eq!(state.editor.borrow().project().name(), "Untitled");
    }
}
