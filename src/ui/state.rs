use crate::media::{Media, MediaKind};
use crate::project::{Command, Editor, Project, store};
use creamui::widgets::{ScrollController, TextController};
use creamui::{AppHandle, Signal};
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

#[derive(Clone)]
pub(crate) struct UiState {
    pub editor: Rc<RefCell<Editor>>,
    pub revision: Signal<u64>,
    pub selected: Signal<Option<(u64, u64)>>,
    pub playhead: Signal<u64>,
    pub project_path: TextController,
    pub media_path: TextController,
    pub title_text: TextController,
    pub status: Signal<String>,
    pub dirty: Signal<bool>,
    pub busy: Signal<bool>,
    pub media_scroll: ScrollController,
    pub confirm_close: Signal<bool>,
    confirm_open: Signal<Option<String>>,
    app: Rc<RefCell<Option<AppHandle>>>,
}

impl UiState {
    pub fn new() -> Self {
        Self {
            editor: Rc::new(RefCell::new(Editor::new(Project::new("Untitled")))),
            revision: Signal::new(0),
            selected: Signal::new(None),
            playhead: Signal::new(0),
            project_path: TextController::default(),
            media_path: TextController::default(),
            title_text: TextController::default(),
            status: Signal::new("Ready · Project not saved".into()),
            dirty: Signal::new(false),
            busy: Signal::new(false),
            media_scroll: ScrollController::default(),
            confirm_close: Signal::new(false),
            confirm_open: Signal::new(None),
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
            self.confirm_close.set(false);
            self.confirm_open.set(None);
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
            self.confirm_close.set(false);
            self.status.set("Undid edit · Unsaved changes".into());
        }
    }

    pub fn redo(&self) {
        if self.editor.borrow_mut().redo() {
            self.revision.update(|revision| *revision += 1);
            self.dirty.set(true);
            self.confirm_close.set(false);
            self.status.set("Redid edit · Unsaved changes".into());
        }
    }

    pub fn load(&self) {
        let Some(app) = self.app.borrow().clone() else {
            return;
        };
        if self.busy.peek() {
            return;
        }
        let path_text = self.project_path.peek();
        let path = PathBuf::from(&path_text);
        if path.as_os_str().is_empty() {
            self.status.set("Enter a project path to open".into());
            return;
        }
        if self.dirty.peek() && self.confirm_open.peek().as_ref() != Some(&path_text) {
            self.confirm_open.set(Some(path_text));
            self.status
                .set("Unsaved changes · Click Open again to discard them".into());
            return;
        }
        self.confirm_open.set(None);
        self.busy.set(true);
        let revision = self.revision.peek();
        self.status.set("Opening project…".into());
        let state = self.clone();
        app.spawn_background(
            move || store::load(&path),
            move |result| {
                state.busy.set(false);
                match result {
                    Ok(project) if state.revision.peek() == revision => {
                        *state.editor.borrow_mut() = Editor::new(project);
                        state.selected.set(None);
                        state.playhead.set(0);
                        state.revision.update(|revision| *revision += 1);
                        state.dirty.set(false);
                        state.confirm_close.set(false);
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
        let Some(app) = self.app.borrow().clone() else {
            return;
        };
        if self.busy.peek() {
            return;
        }
        let path = PathBuf::from(self.project_path.peek());
        if path.as_os_str().is_empty() {
            self.status.set("Enter a project path to save".into());
            return;
        }
        let project = self.editor.borrow().project().clone();
        let revision = self.revision.peek();
        self.busy.set(true);
        self.status.set("Saving project…".into());
        let state = self.clone();
        app.spawn_background(
            move || store::save(&path, &project),
            move |result| {
                state.busy.set(false);
                state.status.set(match result {
                    Ok(()) if state.revision.peek() == revision => {
                        state.dirty.set(false);
                        state.confirm_close.set(false);
                        "Project saved".into()
                    }
                    Ok(()) => "Saved previous version · Unsaved changes".into(),
                    Err(error) => format!("Save failed: {error}"),
                });
            },
        );
    }

    pub fn import(&self) {
        let Some(app) = self.app.borrow().clone() else {
            return;
        };
        if self.busy.peek() {
            return;
        }
        let path = PathBuf::from(self.media_path.peek());
        if path.as_os_str().is_empty() {
            self.status.set("Enter a media file path".into());
            return;
        }
        self.busy.set(true);
        self.status.set("Checking source…".into());
        let state = self.clone();
        app.spawn_background(
            move || inspect_source(&path),
            move |result| {
                state.busy.set(false);
                match result {
                    Ok((path, kind)) => {
                        let id = state
                            .editor
                            .borrow()
                            .project()
                            .media()
                            .iter()
                            .map(|m| m.id)
                            .max()
                            .unwrap_or(0)
                            .checked_add(1);
                        if let Some(id) = id {
                            if state.edit(Command::ImportMedia(Media { id, path, kind })) {
                                state
                                    .status
                                    .set("Source referenced · Playback/probing unavailable".into());
                            }
                        } else {
                            state.status.set("Media ID limit reached".into());
                        }
                    }
                    Err(error) => state.status.set(error),
                }
            },
        );
    }
}

fn inspect_source(path: &Path) -> Result<(PathBuf, MediaKind), String> {
    let kind = match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "mp4" | "mov" | "mkv" | "webm" => MediaKind::Video,
        "wav" | "mp3" | "flac" | "ogg" | "m4a" => MediaKind::Audio,
        "png" | "jpg" | "jpeg" | "webp" | "bmp" => MediaKind::Image,
        _ => return Err("Unsupported source extension".into()),
    };
    let metadata =
        std::fs::metadata(path).map_err(|error| format!("Cannot inspect source: {error}"))?;
    if !metadata.is_file() {
        return Err("Source is not a regular file".into());
    }
    let path = path
        .canonicalize()
        .map_err(|error| format!("Cannot resolve source: {error}"))?;
    Ok((path, kind))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reference_import_checks_file_and_extension_without_modifying_source() {
        let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        assert_eq!(
            inspect_source(&file).unwrap_err(),
            "Unsupported source extension"
        );
        let missing = std::env::temp_dir().join("ocus-no-such-media.mp4");
        assert!(inspect_source(&missing).is_err());
    }
}
