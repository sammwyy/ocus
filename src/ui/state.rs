use crate::export::{self, ExportControl};
use crate::media::preview::{
    FrameImage, FrameSource, PreviewEvent, PreviewPlan, StoryboardKey, decode_sequence, storyboard,
};
use crate::media::{Media, MediaKind, MediaProbe, Thumbnail, probe, thumbnail};
use crate::project::{Command, Editor, Project, store};
use crate::timeline::{ClipContent, TrackKind};
use creamui::core::RgbaImage;
use creamui::widgets::{ScrollController, TextController};
use creamui::{AppHandle, GpuFrame, GpuFrameSlot, Signal, WindowHandle};
use rfd::{FileDialog, MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Sidebar {
    Media,
    Inspector,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum MediaFilter {
    All,
    Video,
    Audio,
    Image,
}

const MAX_THUMBNAILS: usize = 256;
const MAX_STORYBOARDS: usize = 64;

pub(crate) type Storyboard = Arc<Vec<Arc<RgbaImage>>>;

#[derive(Clone)]
pub(crate) struct UiState {
    pub editor: Rc<RefCell<Editor>>,
    pub revision: Signal<u64>,
    pub ui_revision: Signal<u64>,
    pub selected: Signal<Option<(u64, u64)>>,
    pub playhead: Signal<u64>,
    pub playing: Signal<bool>,
    pub preview_available: Signal<bool>,
    pub preview_slot: GpuFrameSlot,
    pub project_path: Signal<Option<PathBuf>>,
    pub title_text: TextController,
    pub media_query: TextController,
    pub media_filter: Signal<MediaFilter>,
    pub status: Signal<String>,
    pub dirty: Signal<bool>,
    pub busy: Signal<bool>,
    pub exporting: Signal<bool>,
    pub export_total: Signal<u64>,
    pub thumbnails: Rc<RefCell<HashMap<PathBuf, Arc<RgbaImage>>>>,
    pub storyboards: Rc<RefCell<HashMap<StoryboardKey, Storyboard>>>,
    pub media_scroll: ScrollController,
    pub inspector_scroll: ScrollController,
    pub timeline_scroll: ScrollController,
    pub timeline_page: Signal<u64>,
    pub timeline_expanded: Signal<bool>,
    pub sidebar: Signal<Sidebar>,
    app: Rc<RefCell<Option<AppHandle>>>,
    window: Rc<RefCell<Option<WindowHandle>>>,
    preview_cancel: Rc<RefCell<Option<Arc<AtomicBool>>>>,
    preview_serial: Rc<Cell<u64>>,
    storyboard_cancel: Rc<RefCell<Option<Arc<AtomicBool>>>>,
    storyboard_serial: Rc<Cell<u64>>,
    export_control: Rc<RefCell<Option<Arc<ExportControl>>>>,
}

impl UiState {
    pub fn new() -> Self {
        Self {
            editor: Rc::new(RefCell::new(Editor::new(Project::new("Untitled")))),
            revision: Signal::new(0),
            ui_revision: Signal::new(0),
            selected: Signal::new(None),
            playhead: Signal::new(0),
            playing: Signal::new(false),
            preview_available: Signal::new(false),
            preview_slot: GpuFrameSlot::new(),
            project_path: Signal::new(None),
            title_text: TextController::default(),
            media_query: TextController::default(),
            media_filter: Signal::new(MediaFilter::All),
            status: Signal::new("Ready · Project not saved".into()),
            dirty: Signal::new(false),
            busy: Signal::new(false),
            exporting: Signal::new(false),
            export_total: Signal::new(0),
            thumbnails: Rc::new(RefCell::new(HashMap::new())),
            storyboards: Rc::new(RefCell::new(HashMap::new())),
            media_scroll: ScrollController::default(),
            inspector_scroll: ScrollController::default(),
            timeline_scroll: ScrollController::default(),
            timeline_page: Signal::new(0),
            timeline_expanded: Signal::new(false),
            sidebar: Signal::new(Sidebar::Media),
            app: Rc::new(RefCell::new(None)),
            window: Rc::new(RefCell::new(None)),
            preview_cancel: Rc::new(RefCell::new(None)),
            preview_serial: Rc::new(Cell::new(0)),
            storyboard_cancel: Rc::new(RefCell::new(None)),
            storyboard_serial: Rc::new(Cell::new(0)),
            export_control: Rc::new(RefCell::new(None)),
        }
    }

    pub fn connect(&self, app: AppHandle) {
        *self.app.borrow_mut() = Some(app);
    }

    pub fn connect_window(&self, window: WindowHandle) {
        self.preview_available.set(window.gpu_device().is_some());
        *self.window.borrow_mut() = Some(window);
        self.request_preview();
    }

    pub fn edit(&self, command: Command) -> bool {
        let changed = self.editor.borrow_mut().apply(command);
        if changed {
            self.revision.update(|revision| *revision += 1);
            self.dirty.set(true);
            self.status.set("Unsaved changes".into());
            self.request_preview();
            self.queue_storyboards();
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
            self.request_preview();
            self.queue_storyboards();
        }
    }

    pub fn redo(&self) {
        if self.editor.borrow_mut().redo() {
            self.revision.update(|revision| *revision += 1);
            self.dirty.set(true);
            self.status.set("Redid edit · Unsaved changes".into());
            self.request_preview();
            self.queue_storyboards();
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
        self.request_preview();
    }

    pub fn play(&self) {
        if !self.preview_available.peek() {
            self.status
                .set("GPU preview is not available on this display".into());
            return;
        }
        let project = self.editor.borrow();
        let has_video = project.project().tracks().iter().any(|track| {
            track.kind == TrackKind::Video && track.clips.iter().any(|clip| {
                matches!(clip.content, ClipContent::Media { media_id }
                    if project.project().media().iter().any(|media| media.id == media_id && media.kind != MediaKind::Audio))
            })
        });
        let end = project
            .project()
            .tracks()
            .iter()
            .flat_map(|track| &track.clips)
            .filter_map(|clip| clip.start_frame.checked_add(clip.duration_frames))
            .max()
            .unwrap_or(0);
        drop(project);
        if !has_video || end == 0 {
            self.status
                .set("Add video or an image to the timeline to preview".into());
            return;
        }
        self.playing.set(true);
        if self.playhead.peek() >= end.saturating_sub(1) {
            self.focus_frame(0);
        } else {
            self.request_preview();
        }
    }

    pub fn pause(&self) {
        self.playing.set(false);
        self.cancel_preview();
    }

    fn cancel_preview(&self) {
        if let Some(cancel) = self.preview_cancel.borrow_mut().take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.preview_serial
            .set(self.preview_serial.get().wrapping_add(1));
    }

    fn request_preview(&self) {
        self.cancel_preview();
        let Some(window) = self.window.borrow().clone() else {
            return;
        };
        if !self.preview_available.peek() || window.is_closed() {
            return;
        }
        let Some(app) = self.app.borrow().clone() else {
            return;
        };
        let editor = self.editor.borrow();
        let project = editor.project();
        let mut sources = Vec::new();
        for track in project
            .tracks()
            .iter()
            .filter(|track| track.kind == TrackKind::Video)
        {
            for clip in &track.clips {
                if let ClipContent::Media { media_id } = clip.content
                    && let Some(media) = project.media().iter().find(|media| media.id == media_id)
                {
                    sources.push(FrameSource {
                        clip_id: clip.id,
                        path: media.path.clone(),
                        kind: media.kind,
                        start: clip.start_frame,
                        source_start: clip.source_start_frame,
                        duration: clip.duration_frames,
                    });
                }
            }
        }
        let plan = PreviewPlan {
            sources,
            end: project
                .tracks()
                .iter()
                .flat_map(|track| &track.clips)
                .filter_map(|clip| clip.start_frame.checked_add(clip.duration_frames))
                .max()
                .unwrap_or(0),
            fps: (
                project.settings().frame_rate.numerator,
                project.settings().frame_rate.denominator,
            ),
            resolution: (project.settings().width, project.settings().height),
        };
        drop(editor);
        window.request_repaint();
        if plan.end == 0 || (!self.playing.peek() && self.playhead.peek() >= plan.end) {
            self.preview_slot.clear(&window);
            return;
        }
        let serial = self.preview_serial.get();
        let start = self.playhead.peek();
        let playing = self.playing.peek();
        let cancelled = Arc::new(AtomicBool::new(false));
        *self.preview_cancel.borrow_mut() = Some(cancelled.clone());
        let state = self.clone();
        let pending = Arc::new(AtomicUsize::new(0));
        let worker_pending = pending.clone();
        app.spawn_stream(
            move |emit| {
                if let Err(error) = decode_sequence(&plan, start, playing, cancelled, |event| {
                    if matches!(event, PreviewEvent::Frame(..))
                        && worker_pending
                            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                                (count < 2).then_some(count + 1)
                            })
                            .is_err()
                    {
                        return;
                    }
                    emit(event);
                }) {
                    emit(PreviewEvent::Failed(error));
                }
            },
            move |event| {
                if matches!(event, PreviewEvent::Frame(..)) {
                    pending.fetch_sub(1, Ordering::Relaxed);
                }
                if state.preview_serial.get() != serial {
                    return;
                }
                match event {
                    PreviewEvent::Frame(frame, Some(image)) => {
                        match upload_preview_frame(&window, image) {
                            Ok(texture) => {
                                state.preview_slot.publish(texture, &window);
                                state.playhead.set(frame);
                                state.update_timeline_page(frame);
                            }
                            Err(error) => {
                                state.pause();
                                state.status.set(format!("Preview failed: {error}"));
                            }
                        }
                    }
                    PreviewEvent::Frame(frame, None) => {
                        state.preview_slot.clear(&window);
                        state.playhead.set(frame);
                        state.update_timeline_page(frame);
                    }
                    PreviewEvent::Finished => state.pause(),
                    PreviewEvent::Failed(error) => {
                        state.pause();
                        state.status.set(format!("Preview failed: {error}"));
                    }
                }
            },
        );
    }

    fn update_timeline_page(&self, frame: u64) {
        let editor = self.editor.borrow();
        let rate = &editor.project().settings().frame_rate;
        let page_frames = (u64::from(rate.numerator) * 10 / u64::from(rate.denominator)).max(1);
        drop(editor);
        self.timeline_page.set(frame / page_frames);
    }

    fn cancel_storyboards(&self) {
        if let Some(cancel) = self.storyboard_cancel.borrow_mut().take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.storyboard_serial
            .set(self.storyboard_serial.get().wrapping_add(1));
    }

    fn queue_storyboards(&self) {
        self.cancel_storyboards();
        let Some(app) = self.app.borrow().clone() else {
            return;
        };
        let editor = self.editor.borrow();
        let project = editor.project();
        let fps = (
            project.settings().frame_rate.numerator,
            project.settings().frame_rate.denominator,
        );
        let keys: Vec<_> = project
            .tracks()
            .iter()
            .filter(|track| track.kind == TrackKind::Video)
            .flat_map(|track| &track.clips)
            .filter_map(|clip| {
                let ClipContent::Media { media_id } = clip.content else {
                    return None;
                };
                let media = project.media().iter().find(|media| media.id == media_id)?;
                Some(StoryboardKey {
                    path: media.path.clone(),
                    kind: media.kind,
                    source_start: clip.source_start_frame,
                    duration: clip.duration_frames,
                    fps,
                })
            })
            .collect();
        drop(editor);
        let live: HashSet<_> = keys.iter().cloned().collect();
        let mut cache = self.storyboards.borrow_mut();
        cache.retain(|key, _| live.contains(key));
        let mut requested = HashSet::new();
        let requests: Vec<_> = keys
            .into_iter()
            .filter(|key| requested.insert(key.clone()) && !cache.contains_key(key))
            .take(MAX_STORYBOARDS.saturating_sub(cache.len()))
            .collect();
        drop(cache);
        if requests.is_empty() {
            return;
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        *self.storyboard_cancel.borrow_mut() = Some(cancelled.clone());
        let serial = self.storyboard_serial.get();
        let state = self.clone();
        app.spawn_stream(
            move |emit| {
                for key in requests {
                    if cancelled.load(Ordering::Relaxed) {
                        break;
                    }
                    let result = storyboard(&key, cancelled.clone());
                    emit((key, result));
                }
            },
            move |(key, result): (StoryboardKey, Result<Vec<FrameImage>, String>)| {
                if state.storyboard_serial.get() != serial {
                    return;
                }
                match result {
                    Ok(frames) if !frames.is_empty() => {
                        let images: Vec<_> = frames
                            .into_iter()
                            .filter_map(|frame| {
                                RgbaImage::new(frame.width, frame.height, frame.pixels)
                                    .map(Arc::new)
                            })
                            .collect();
                        if !images.is_empty() {
                            state.storyboards.borrow_mut().insert(key, Arc::new(images));
                            state.ui_revision.update(|revision| *revision += 1);
                        }
                    }
                    Err(error) => state.status.set(format!("Timeline thumbnail: {error}")),
                    _ => {}
                }
            },
        );
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
        self.thumbnails.borrow_mut().clear();
        self.cancel_storyboards();
        self.storyboards.borrow_mut().clear();
        self.pause();
        if let Some(window) = self.window.borrow().as_ref() {
            self.preview_slot.clear(window);
        }
        self.selected.set(None);
        self.media_query.set_value(String::new());
        self.media_filter.set(MediaFilter::All);
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
                        let sources: Vec<_> = project
                            .media()
                            .iter()
                            .filter(|media| media.kind != MediaKind::Audio)
                            .map(|media| media.path.clone())
                            .take(MAX_THUMBNAILS)
                            .collect();
                        *state.editor.borrow_mut() = Editor::new(project);
                        state.thumbnails.borrow_mut().clear();
                        state.cancel_storyboards();
                        state.storyboards.borrow_mut().clear();
                        state.project_path.set(Some(path));
                        state.selected.set(None);
                        state.media_query.set_value(String::new());
                        state.media_filter.set(MediaFilter::All);
                        state.playhead.set(0);
                        state.timeline_page.set(0);
                        state.revision.update(|revision| *revision += 1);
                        state.dirty.set(false);
                        state.sidebar.set(Sidebar::Media);
                        state.status.set("Project opened".into());
                        state.load_thumbnails(sources);
                        state.queue_storyboards();
                        state.pause();
                        state.request_preview();
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

    pub fn export_dialog(&self) {
        if self.busy.peek() {
            return;
        }
        let project = self.editor.borrow().project().clone();
        if !export::can_render(&project) {
            self.status.set("MP4 export supports one continuous video track without titles or audio; other sequences are not yet supported".into());
            return;
        }
        let Some(selected) = FileDialog::new()
            .set_title("Export video-only MP4")
            .add_filter("MP4 video", &["mp4"])
            .set_file_name("export")
            .save_file()
        else {
            return;
        };
        let path = match selected
            .extension()
            .and_then(|extension| extension.to_str())
        {
            None => selected.with_extension("mp4"),
            Some(extension) if extension.eq_ignore_ascii_case("mp4") => {
                if selected
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .is_some_and(|stem| stem.to_ascii_lowercase().ends_with(".mp4"))
                {
                    selected.with_extension("")
                } else {
                    selected.clone()
                }
            }
            _ => {
                self.status
                    .set("Export destination must be an MP4 file".into());
                return;
            }
        };
        let Some(app) = self.app.borrow().clone() else {
            return;
        };
        let control = Arc::new(ExportControl::default());
        self.export_total.set(
            project
                .tracks()
                .iter()
                .flat_map(|track| &track.clips)
                .map(|clip| clip.duration_frames)
                .sum(),
        );
        *self.export_control.borrow_mut() = Some(control.clone());
        self.exporting.set(true);
        self.busy.set(true);
        self.status.set("Encoding silent MP4…".into());
        let state = self.clone();
        app.spawn_background(
            move || {
                if path != selected && path.exists() {
                    return (
                        path,
                        Err(
                            "Destination exists under the corrected .mp4 name; choose another name"
                                .into(),
                        ),
                    );
                }
                let result = export::render_with_control(&project, &path, control);
                (path, result)
            },
            move |(path, result)| {
                state.exporting.set(false);
                state.export_control.borrow_mut().take();
                state.busy.set(false);
                state.status.set(match result {
                    Ok(()) => format!("Silent MP4 exported: {}", path.display()),
                    Err(ref error) if error == "Export cancelled" => error.clone(),
                    Err(error) => format!("Export failed: {error}"),
                });
            },
        );
    }

    pub fn cancel_export(&self) {
        if let Some(control) = self.export_control.borrow().as_ref() {
            control.cancel();
            self.status.set("Cancelling export…".into());
        }
    }

    pub fn export_control(&self) -> Option<Arc<ExportControl>> {
        self.export_control.borrow().clone()
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
                        Ok((path, info, thumb)) => {
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
                                    path: path.clone(),
                                    kind: info.kind,
                                    duration_us: info.duration_us,
                                    has_audio: info.has_audio,
                                };
                                if existing.as_ref() == Some(&media) {
                                } else if state.edit(if existing.is_some() {
                                    Command::ReprobeMedia(media)
                                } else {
                                    Command::ImportMedia(media)
                                }) {
                                    imported += 1;
                                } else {
                                    errors.push("Source rejected by project validation".into());
                                }
                                if let Some(thumb) = thumb
                                    && state
                                        .editor
                                        .borrow()
                                        .project()
                                        .media()
                                        .iter()
                                        .any(|media| media.id == id && media.path == path)
                                    && let Some(image) =
                                        RgbaImage::new(thumb.width, thumb.height, thumb.pixels)
                                {
                                    let mut thumbnails = state.thumbnails.borrow_mut();
                                    if thumbnails.len() < MAX_THUMBNAILS
                                        || thumbnails.contains_key(&path)
                                    {
                                        thumbnails.insert(path, Arc::new(image));
                                        drop(thumbnails);
                                        state.ui_revision.update(|revision| *revision += 1);
                                    }
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

    fn load_thumbnails(&self, sources: Vec<PathBuf>) {
        let Some(app) = self.app.borrow().clone() else {
            return;
        };
        let state = self.clone();
        app.spawn_background(
            move || {
                sources
                    .into_iter()
                    .filter_map(|path| thumbnail(&path).ok().map(|image| (path, image)))
                    .collect::<Vec<_>>()
            },
            move |results| {
                let editor = state.editor.borrow();
                let mut thumbnails = state.thumbnails.borrow_mut();
                for (path, thumb) in results {
                    if editor
                        .project()
                        .media()
                        .iter()
                        .any(|media| media.path == path)
                        && let Some(image) = RgbaImage::new(thumb.width, thumb.height, thumb.pixels)
                    {
                        thumbnails.insert(path, Arc::new(image));
                    }
                }
                drop(thumbnails);
                drop(editor);
                state.ui_revision.update(|revision| *revision += 1);
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

fn upload_preview_frame(window: &WindowHandle, image: FrameImage) -> Result<GpuFrame, String> {
    let row_bytes = image
        .width
        .checked_mul(4)
        .ok_or("Preview image is too wide")?;
    if image.width == 0
        || image.height == 0
        || image.pixels.len()
            != usize::try_from(row_bytes)
                .ok()
                .and_then(|row| row.checked_mul(image.height as usize))
                .ok_or("Preview image is too large")?
    {
        return Err("Invalid preview image data".into());
    }
    let (device, queue) = window
        .gpu_device()
        .ok_or("GPU device is not available for preview")?;
    let size = wgpu::Extent3d {
        width: image.width,
        height: image.height,
        depth_or_array_layers: 1,
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("ocus-preview-frame"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::ImageCopyTexture {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &image.pixels,
        wgpu::ImageDataLayout {
            offset: 0,
            bytes_per_row: Some(row_bytes),
            rows_per_image: Some(image.height),
        },
        size,
    );
    Ok(GpuFrame::new(
        texture.create_view(&wgpu::TextureViewDescriptor::default()),
    ))
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

fn inspect_source(path: &Path) -> Result<(PathBuf, MediaProbe, Option<Thumbnail>), String> {
    let info = probe(path)?;
    let path = path
        .canonicalize()
        .map_err(|error| format!("Cannot resolve source: {error}"))?;
    let thumb = (info.kind != MediaKind::Audio)
        .then(|| thumbnail(&path).ok())
        .flatten();
    Ok((path, info, thumb))
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
