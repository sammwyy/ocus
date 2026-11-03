use super::edits::{self, ClipRef};
use super::interaction::{DragState, Gesture, MenuState};
use crate::export::{self, ExportControl};
use crate::media::preview::{
    FrameImage, FrameSource, PreviewEvent, PreviewPlan, StoryboardKey, decode_sequence, storyboard,
};
use crate::media::{Media, MediaKind, MediaProbe, Thumbnail, probe, thumbnail};
use crate::project::{Command, Editor, Project, store};
use crate::timeline::{Clip, ClipContent, TrackKind};
use creamui::core::{Modifiers, Point, Rect, RgbaImage};
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

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum MediaView {
    Grid,
    List,
}

const MAX_THUMBNAILS: usize = 256;
const MAX_STORYBOARDS: usize = 64;
pub(crate) const ZOOM_SECONDS: [u64; 6] = [2, 5, 10, 30, 60, 180];
const DEFAULT_ZOOM: usize = 2;

pub(crate) type Storyboard = Arc<Vec<Arc<RgbaImage>>>;

/// One GPU preview surface with its own position and decode stream.
#[derive(Clone)]
pub(crate) struct Monitor {
    pub slot: GpuFrameSlot,
    pub playing: Signal<bool>,
    pub position: Signal<u64>,
    cancel: Rc<RefCell<Option<Arc<AtomicBool>>>>,
    serial: Rc<Cell<u64>>,
}

impl Monitor {
    fn new() -> Self {
        Self {
            slot: GpuFrameSlot::new(),
            playing: Signal::new(false),
            position: Signal::new(0),
            cancel: Rc::new(RefCell::new(None)),
            serial: Rc::new(Cell::new(0)),
        }
    }

    fn cancel(&self) {
        if let Some(cancel) = self.cancel.borrow_mut().take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.serial.set(self.serial.get().wrapping_add(1));
    }
}

/// The source frames shown by the source monitor, in project frames.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SourceRange {
    pub path: PathBuf,
    pub kind: MediaKind,
    pub source_start: u64,
    pub duration: u64,
}

#[derive(Clone)]
pub(crate) struct UiState {
    pub editor: Rc<RefCell<Editor>>,
    pub revision: Signal<u64>,
    pub ui_revision: Signal<u64>,
    pub selected: Signal<Vec<ClipRef>>,
    pub selected_media: Signal<Option<u64>>,
    pub program: Monitor,
    pub source: Monitor,
    pub preview_available: Signal<bool>,
    pub project_path: Signal<Option<PathBuf>>,
    pub title_text: TextController,
    pub media_query: TextController,
    pub media_filter: Signal<MediaFilter>,
    pub media_view: Signal<MediaView>,
    pub status: Signal<String>,
    pub dirty: Signal<bool>,
    pub busy: Signal<bool>,
    pub exporting: Signal<bool>,
    pub export_total: Signal<u64>,
    pub thumbnails: Rc<RefCell<HashMap<PathBuf, Arc<RgbaImage>>>>,
    pub storyboards: Rc<RefCell<HashMap<StoryboardKey, Storyboard>>>,
    pub media_scroll: ScrollController,
    pub inspector_scroll: ScrollController,
    pub timeline_start: Signal<u64>,
    pub timeline_zoom: Signal<usize>,
    pub track_scroll: Signal<f32>,
    pub timeline_expanded: Signal<bool>,
    pub gesture: Rc<RefCell<Gesture>>,
    pub drag: Signal<Option<DragState>>,
    pub marquee: Signal<Option<Rect>>,
    pub timeline_rect: Rc<Cell<Rect>>,
    pub modifiers: Rc<Cell<Modifiers>>,
    pub clipboard: Rc<RefCell<Vec<(u64, Clip)>>>,
    pub menu: Signal<Option<MenuState>>,
    pub pointer: Rc<Cell<Point>>,
    pub sidebar: Signal<Sidebar>,
    app: Rc<RefCell<Option<AppHandle>>>,
    window: Rc<RefCell<Option<WindowHandle>>>,
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
            selected: Signal::new(Vec::new()),
            selected_media: Signal::new(None),
            program: Monitor::new(),
            source: Monitor::new(),
            preview_available: Signal::new(false),
            project_path: Signal::new(None),
            title_text: TextController::default(),
            media_query: TextController::default(),
            media_filter: Signal::new(MediaFilter::All),
            media_view: Signal::new(MediaView::Grid),
            status: Signal::new("Ready · Project not saved".into()),
            dirty: Signal::new(false),
            busy: Signal::new(false),
            exporting: Signal::new(false),
            export_total: Signal::new(0),
            thumbnails: Rc::new(RefCell::new(HashMap::new())),
            storyboards: Rc::new(RefCell::new(HashMap::new())),
            media_scroll: ScrollController::default(),
            inspector_scroll: ScrollController::default(),
            timeline_start: Signal::new(0),
            timeline_zoom: Signal::new(DEFAULT_ZOOM),
            track_scroll: Signal::new(0.0),
            timeline_expanded: Signal::new(false),
            gesture: Rc::new(RefCell::new(Gesture::Idle)),
            drag: Signal::new(None),
            marquee: Signal::new(None),
            timeline_rect: Rc::new(Cell::new(Rect::default())),
            modifiers: Rc::new(Cell::new(Modifiers::default())),
            clipboard: Rc::new(RefCell::new(Vec::new())),
            menu: Signal::new(None),
            pointer: Rc::new(Cell::new(Point::default())),
            sidebar: Signal::new(Sidebar::Media),
            app: Rc::new(RefCell::new(None)),
            window: Rc::new(RefCell::new(None)),
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
        self.install_input(&window);
        *self.window.borrow_mut() = Some(window);
        self.request_preview();
        self.request_source_preview();
    }

    fn refresh_after_edit(&self, status: &str) {
        let existing: HashSet<_> = self
            .editor
            .borrow()
            .project()
            .tracks()
            .iter()
            .flat_map(|track| track.clips.iter().map(|clip| (track.id, clip.id)))
            .collect();
        let selection = self.selected.peek();
        if selection.iter().any(|clip| !existing.contains(clip)) {
            self.select_clips(
                selection
                    .into_iter()
                    .filter(|clip| existing.contains(clip))
                    .collect(),
            );
        }
        self.revision.update(|revision| *revision += 1);
        self.dirty.set(true);
        self.status.set(status.into());
        self.request_preview();
        self.request_source_preview();
        self.queue_storyboards();
    }

    pub fn edit(&self, command: Command) -> bool {
        let changed = self.editor.borrow_mut().apply(command);
        if changed {
            self.refresh_after_edit("Unsaved changes");
        } else {
            self.status
                .set("Edit rejected: check range, track and clip".into());
        }
        changed
    }

    pub fn undo(&self) {
        if self.editor.borrow_mut().undo() {
            self.refresh_after_edit("Undid edit · Unsaved changes");
        }
    }

    pub fn redo(&self) {
        if self.editor.borrow_mut().redo() {
            self.refresh_after_edit("Redid edit · Unsaved changes");
        }
    }

    pub fn frame_rate(&self) -> (u32, u32) {
        let editor = self.editor.borrow();
        let rate = &editor.project().settings().frame_rate;
        (rate.numerator, rate.denominator)
    }

    pub fn sequence_end(&self) -> u64 {
        self.editor
            .borrow()
            .project()
            .tracks()
            .iter()
            .flat_map(|track| &track.clips)
            .filter_map(|clip| clip.start_frame.checked_add(clip.duration_frames))
            .max()
            .unwrap_or(0)
    }

    /// Frames shown across the timeline at the current zoom level.
    pub fn visible_frames(&self) -> u64 {
        let (numerator, denominator) = self.frame_rate();
        let seconds = ZOOM_SECONDS[self.timeline_zoom.peek().min(ZOOM_SECONDS.len() - 1)];
        (u64::from(numerator) * seconds / u64::from(denominator.max(1))).max(1)
    }

    fn follow_playhead(&self, frame: u64) {
        let span = self.visible_frames();
        let start = self.timeline_start.peek();
        if frame < start || frame >= start.saturating_add(span) {
            self.timeline_start.set(frame / span * span);
        }
    }

    pub fn zoom_timeline(&self, zoom: usize) {
        let zoom = zoom.min(ZOOM_SECONDS.len() - 1);
        if zoom == self.timeline_zoom.peek() {
            return;
        }
        self.timeline_zoom.set(zoom);
        let span = self.visible_frames();
        self.timeline_start
            .set(self.program.position.peek().saturating_sub(span / 4));
    }

    pub fn scroll_timeline(&self, steps: i64) {
        let step = (self.visible_frames() / 8).max(1);
        let start = self.timeline_start.peek();
        let distance = step.saturating_mul(steps.unsigned_abs());
        let next = if steps < 0 {
            start.saturating_sub(distance)
        } else {
            start.saturating_add(distance)
        };
        let next = next.min(self.sequence_end().saturating_sub(1));
        if next != start {
            self.timeline_start.set(next);
        }
    }

    pub fn focus_frame(&self, frame: u64) {
        self.program.position.set(frame);
        self.follow_playhead(frame);
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
        drop(project);
        let end = self.sequence_end();
        if !has_video || end == 0 {
            self.status
                .set("Add video or an image to the timeline to preview".into());
            return;
        }
        self.pause_source();
        self.program.playing.set(true);
        if self.program.position.peek() >= end.saturating_sub(1) {
            self.focus_frame(0);
        } else {
            self.request_preview();
        }
    }

    pub fn pause(&self) {
        self.stop(&self.program);
    }

    fn stop(&self, monitor: &Monitor) {
        monitor.playing.set(false);
        monitor.cancel();
    }

    /// The selected timeline clip's source range, or else the whole selected
    /// media-bin source.
    pub fn source_range(&self) -> Option<SourceRange> {
        let editor = self.editor.borrow();
        let project = editor.project();
        if let Some((track_id, clip_id)) = self.primary_clip() {
            let clip = project
                .tracks()
                .iter()
                .find(|track| track.id == track_id)?
                .clips
                .iter()
                .find(|clip| clip.id == clip_id)?;
            let ClipContent::Media { media_id } = clip.content else {
                return None;
            };
            let media = project.media().iter().find(|media| media.id == media_id)?;
            return Some(SourceRange {
                path: media.path.clone(),
                kind: media.kind,
                source_start: clip.source_start_frame,
                duration: clip.duration_frames,
            });
        }
        let media_id = self.selected_media.peek()?;
        let media = project.media().iter().find(|media| media.id == media_id)?;
        let rate = &project.settings().frame_rate;
        Some(SourceRange {
            path: media.path.clone(),
            kind: media.kind,
            source_start: 0,
            duration: media_frames(media, (rate.numerator, rate.denominator))?,
        })
    }

    /// The most recently selected clip, shown in the inspector and source
    /// monitor.
    pub fn primary_clip(&self) -> Option<ClipRef> {
        self.selected.peek().last().copied()
    }

    pub fn select_clip(&self, selection: Option<ClipRef>) {
        self.select_clips(selection.into_iter().collect());
    }

    /// Replaces the clip selection; the last entry becomes the primary clip.
    pub fn select_clips(&self, mut selection: Vec<ClipRef>) {
        let mut seen = HashSet::new();
        selection.reverse();
        selection.retain(|clip| seen.insert(*clip));
        selection.reverse();
        let previous = self.primary_clip();
        if self.selected.peek() == selection {
            return;
        }
        self.selected.set(selection);
        let primary = self.primary_clip();
        if let Some((track_id, clip_id)) = primary {
            self.selected_media.set(None);
            self.sidebar.set(Sidebar::Inspector);
            let title = self
                .editor
                .borrow()
                .project()
                .tracks()
                .iter()
                .find(|track| track.id == track_id)
                .and_then(|track| track.clips.iter().find(|clip| clip.id == clip_id))
                .and_then(|clip| match &clip.content {
                    ClipContent::Title(title) => Some(title.text.clone()),
                    ClipContent::Media { .. } => None,
                });
            if let Some(title) = title {
                self.title_text.set_value(title);
            }
        }
        if previous.map(|(_, clip)| clip) != primary.map(|(_, clip)| clip) {
            self.reset_source();
        }
    }

    /// Adds or removes a clip, as with Ctrl+click.
    pub fn toggle_clip(&self, clip: ClipRef) {
        let mut selection = self.selected.peek();
        if let Some(index) = selection.iter().position(|item| *item == clip) {
            selection.remove(index);
        } else {
            selection.push(clip);
        }
        self.select_clips(selection);
    }

    /// Adds every clip on the primary clip's track between it and `clip`, as
    /// with Shift+click; other tracks just add the clip.
    pub fn extend_selection(&self, clip: ClipRef) {
        let mut selection = self.selected.peek();
        let range = self
            .primary_clip()
            .filter(|primary| primary.0 == clip.0)
            .and_then(|primary| {
                let editor = self.editor.borrow();
                let track = editor.project().tracks().iter().find(|t| t.id == clip.0)?;
                let start_of = |id| {
                    track
                        .clips
                        .iter()
                        .find(|c| c.id == id)
                        .map(|c| c.start_frame)
                };
                let (a, b) = (start_of(primary.1)?, start_of(clip.1)?);
                let (low, high) = (a.min(b), a.max(b));
                Some(
                    track
                        .clips
                        .iter()
                        .filter(|c| c.start_frame >= low && c.start_frame <= high && c.id != clip.1)
                        .map(|c| (track.id, c.id))
                        .collect::<Vec<_>>(),
                )
            });
        selection.extend(range.unwrap_or_default());
        selection.push(clip);
        self.select_clips(selection);
    }

    pub fn select_all(&self) {
        let all = self
            .editor
            .borrow()
            .project()
            .tracks()
            .iter()
            .flat_map(|track| track.clips.iter().map(|clip| (track.id, clip.id)))
            .collect();
        self.select_clips(all);
    }

    pub fn select_media(&self, media_id: u64) {
        if self.selected_media.peek() == Some(media_id) && self.selected.peek().is_empty() {
            return;
        }
        self.selected.set(Vec::new());
        self.selected_media.set(Some(media_id));
        self.reset_source();
    }

    fn reset_source(&self) {
        self.stop(&self.source);
        self.source.position.set(0);
        self.request_source_preview();
    }

    pub fn play_source(&self) {
        if !self.preview_available.peek() {
            self.status
                .set("GPU preview is not available on this display".into());
            return;
        }
        let Some(range) = self
            .source_range()
            .filter(|range| range.kind != MediaKind::Audio)
        else {
            self.status
                .set("Select a video clip or source to preview it".into());
            return;
        };
        self.pause();
        self.source.playing.set(true);
        if self.source.position.peek() >= range.duration.saturating_sub(1) {
            self.source.position.set(0);
        }
        self.request_source_preview();
    }

    pub fn pause_source(&self) {
        self.stop(&self.source);
    }

    pub fn focus_source_frame(&self, frame: u64) {
        self.source.position.set(frame);
        self.request_source_preview();
    }

    fn request_preview(&self) {
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
        let fps = (
            project.settings().frame_rate.numerator,
            project.settings().frame_rate.denominator,
        );
        let resolution = (project.settings().width, project.settings().height);
        drop(editor);
        let plan = PreviewPlan {
            sources,
            end: self.sequence_end(),
            fps,
            resolution,
        };
        self.stream(&self.program, plan, true);
    }

    fn request_source_preview(&self) {
        let range = self
            .source_range()
            .filter(|range| range.kind != MediaKind::Audio);
        let editor = self.editor.borrow();
        let settings = editor.project().settings();
        let plan = PreviewPlan {
            end: range.as_ref().map_or(0, |range| range.duration),
            sources: range
                .into_iter()
                .map(|range| FrameSource {
                    clip_id: 0,
                    path: range.path,
                    kind: range.kind,
                    start: 0,
                    source_start: range.source_start,
                    duration: range.duration,
                })
                .collect(),
            fps: (
                settings.frame_rate.numerator,
                settings.frame_rate.denominator,
            ),
            resolution: (settings.width, settings.height),
        };
        drop(editor);
        if plan.end > 0 && self.source.position.peek() >= plan.end {
            self.source.position.set(plan.end - 1);
        }
        self.stream(&self.source, plan, false);
    }

    fn stream(&self, monitor: &Monitor, plan: PreviewPlan, follow_timeline: bool) {
        monitor.cancel();
        let Some(window) = self.window.borrow().clone() else {
            return;
        };
        if !self.preview_available.peek() || window.is_closed() {
            return;
        }
        let Some(app) = self.app.borrow().clone() else {
            return;
        };
        window.request_repaint();
        let start = monitor.position.peek();
        let playing = monitor.playing.peek();
        if plan.end == 0 || (!playing && start >= plan.end) {
            monitor.slot.clear(&window);
            return;
        }
        let serial = monitor.serial.get();
        let cancelled = Arc::new(AtomicBool::new(false));
        *monitor.cancel.borrow_mut() = Some(cancelled.clone());
        let state = self.clone();
        let monitor = monitor.clone();
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
                if monitor.serial.get() != serial {
                    return;
                }
                let image = match event {
                    PreviewEvent::Frame(frame, image) => {
                        monitor.position.set(frame);
                        if follow_timeline {
                            state.follow_playhead(frame);
                        }
                        image
                    }
                    PreviewEvent::Finished => {
                        state.stop(&monitor);
                        return;
                    }
                    PreviewEvent::Failed(error) => {
                        state.stop(&monitor);
                        state.status.set(format!("Preview failed: {error}"));
                        return;
                    }
                };
                match image.map(|image| upload_preview_frame(&window, image)) {
                    Some(Ok(texture)) => monitor.slot.publish(texture, &window),
                    Some(Err(error)) => {
                        state.stop(&monitor);
                        state.status.set(format!("Preview failed: {error}"));
                    }
                    None => monitor.slot.clear(&window),
                }
            },
        );
    }

    fn reset_monitors(&self) {
        self.pause();
        self.pause_source();
        if let Some(window) = self.window.borrow().as_ref() {
            self.program.slot.clear(window);
            self.source.slot.clear(window);
        }
        self.selected.set(Vec::new());
        self.selected_media.set(None);
        self.program.position.set(0);
        self.source.position.set(0);
        self.timeline_start.set(0);
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
        self.reset_monitors();
        self.media_query.set_value(String::new());
        self.media_filter.set(MediaFilter::All);
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
                        state.reset_monitors();
                        state.media_query.set_value(String::new());
                        state.media_filter.set(MediaFilter::All);
                        state.revision.update(|revision| *revision += 1);
                        state.dirty.set(false);
                        state.sidebar.set(Sidebar::Media);
                        state.status.set("Project opened".into());
                        state.load_thumbnails(sources);
                        state.queue_storyboards();
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
            self.import_paths(paths, None);
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

    /// Imports files on a worker; with a placement (`track_id`, frame) the
    /// imported sources are then laid out one after another from there.
    pub fn import_paths(&self, paths: Vec<PathBuf>, placement: Option<(u64, u64)>) {
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
                let mut ready = Vec::new();
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
                                    ready.push(id);
                                } else if state.edit(if existing.is_some() {
                                    Command::ReprobeMedia(media)
                                } else {
                                    Command::ImportMedia(media)
                                }) {
                                    imported += 1;
                                    ready.push(id);
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
                if let Some((track_id, mut frame)) = placement {
                    for id in ready {
                        let track =
                            edits::compatible_track(state.editor.borrow().project(), id, track_id);
                        let Some(track) = track else {
                            continue;
                        };
                        match state.insert_media_at(id, Some((track, frame))) {
                            Some(next) => frame = next,
                            None => break,
                        }
                    }
                }
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
        self.insert_media_at(media_id, None);
    }

    /// Inserts a source at `placement` (`track_id`, frame), or after the last
    /// clip of its first compatible track, returning the frame after it.
    pub fn insert_media_at(&self, media_id: u64, placement: Option<(u64, u64)>) -> Option<u64> {
        let plan = edits::insert_media(self.editor.borrow().project(), media_id, placement);
        let insertion = match plan {
            Ok(insertion) => insertion,
            Err(error) => {
                self.status.set(error);
                return None;
            }
        };
        if !self.edit(insertion.command) {
            return None;
        }
        self.select_clips(insertion.selection);
        if placement.is_none() {
            self.focus_frame(insertion.start);
        }
        insertion.start.checked_add(insertion.duration)
    }
}

/// Timeline frames a source occupies when inserted; stills default to 3 s.
pub(crate) fn media_frames(media: &Media, fps: (u32, u32)) -> Option<u64> {
    let (numerator, denominator) = (u64::from(fps.0), u64::from(fps.1.max(1)));
    if let Some(duration_us) = media.duration_us {
        let count = u128::from(duration_us) * u128::from(numerator)
            / (1_000_000_u128 * u128::from(denominator));
        u64::try_from(count).ok()
    } else if media.kind == MediaKind::Image {
        Some((numerator * 3 / denominator).max(1))
    } else {
        None
    }
    .filter(|count| *count > 0)
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
    fn inserting_video_with_audio_links_its_sound_and_undoes_in_one_step() {
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
        assert_eq!(video.len(), 1);
        assert!(video[0].linked_audio);
        assert!(editor.project().tracks()[2].clips.is_empty());
        drop(editor);
        state.detach_audio();
        let editor = state.editor.borrow();
        assert!(!editor.project().tracks()[0].clips[0].linked_audio);
        assert_eq!(editor.project().tracks()[2].clips[0].duration_frames, 60);
        drop(editor);
        assert_eq!(state.selected.peek().len(), 2);
        state.link_audio();
        assert!(state.editor.borrow().project().tracks()[0].clips[0].linked_audio);
        state.undo();
        state.undo();
        state.undo();
        assert!(state.editor.borrow().project().tracks()[0].clips.is_empty());
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
        assert_eq!(state.timeline_start.peek(), 600);
        state.sidebar.set(Sidebar::Inspector);
        state.select_media(3);
        state.new_project();
        assert_eq!(state.project_path.peek(), None);
        assert_eq!(state.program.position.peek(), 0);
        assert_eq!(state.timeline_start.peek(), 0);
        assert_eq!(state.selected_media.peek(), None);
        assert!(state.sidebar.peek() == Sidebar::Media);
        assert_eq!(state.editor.borrow().project().name(), "Untitled");
    }

    #[test]
    fn source_monitor_follows_the_latest_bin_or_timeline_selection() {
        let state = UiState::new();
        assert!(state.edit(Command::ImportMedia(Media {
            id: 4,
            path: "scene.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(2_000_000),
            has_audio: false,
        })));
        state.select_media(4);
        let range = state.source_range().unwrap();
        assert_eq!((range.source_start, range.duration), (0, 60));
        state.insert_media(4);
        assert_eq!(state.selected_media.peek(), None);
        let (track_id, clip_id) = state.primary_clip().unwrap();
        assert!(state.edit(Command::Trim {
            track_id,
            clip_id,
            start_frame: 0,
            source_start_frame: 10,
            duration_frames: 20,
        }));
        state.focus_source_frame(25);
        let range = state.source_range().unwrap();
        assert_eq!((range.source_start, range.duration), (10, 20));
        assert_eq!(state.source.position.peek(), 19);
        state.edit(Command::RemoveClip { track_id, clip_id });
        assert_eq!(state.source_range(), None);
        state.select_media(4);
        assert!(state.selected.peek().is_empty());
        assert_eq!(state.source.position.peek(), 0);
    }

    #[test]
    fn timeline_zoom_and_scroll_keep_the_view_inside_the_sequence() {
        let state = UiState::new();
        assert_eq!(state.visible_frames(), 300);
        assert!(state.edit(Command::ImportMedia(Media {
            id: 1,
            path: "long.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(60_000_000),
            has_audio: false,
        })));
        state.insert_media(1);
        state.scroll_timeline(3);
        assert_eq!(state.timeline_start.peek(), 111);
        state.scroll_timeline(-10);
        assert_eq!(state.timeline_start.peek(), 0);
        state.scroll_timeline(1_000);
        assert_eq!(state.timeline_start.peek(), 1_799);
        state.focus_frame(900);
        state.zoom_timeline(0);
        assert_eq!(state.visible_frames(), 60);
        assert_eq!(state.timeline_start.peek(), 885);
        state.zoom_timeline(99);
        assert_eq!(state.visible_frames(), 5_400);
    }
}
