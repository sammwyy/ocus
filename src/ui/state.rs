use super::edits::{self, ClipRef};
use super::interaction::{DragState, Gesture, MenuState};
use crate::export::{self, ExportControl, presets};
use crate::media::audio::{self, AudioPlan, AudioSource, Clock};
use crate::media::composite::VisualPlan;
use crate::media::preview::{
    FrameImage, FrameSource, PreviewEvent, PreviewPlan, StoryboardKey, decode_sequence, storyboard,
};
use crate::media::{Media, MediaKind, MediaProbe, Thumbnail, probe, thumbnail};
use crate::playback::{self, Session};
use crate::project::export_settings::{
    AudioCodec, Container, EncodingSpeed, ExportSettings, LastExport, VideoCodec,
};
use crate::project::{Command, Editor, Project, store};
use crate::settings::{self, Settings, ThemeMode};
use crate::timeline::{Clip, ClipContent, Title, TrackKind};
use creamui::core::{Modifiers, Point, Rect, RgbaImage};
use creamui::widgets::{ColorPickerController, ScrollController, TextController};
use creamui::{AppHandle, GpuFrame, GpuFrameSlot, Signal, WindowHandle};
use rfd::{FileDialog, MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Sidebar {
    Media,
    Inspector,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExportPage {
    Presets,
    Video,
    Audio,
    Output,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsPage {
    Appearance,
    About,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UnsavedChoice {
    Save,
    Discard,
    Cancel,
}

type AfterSave = Box<dyn FnOnce(&UiState)>;

const SAVE_CHOICE: &str = "Save";
const DISCARD_CHOICE: &str = "Don't Save";
const CANCEL_CHOICE: &str = "Cancel";
const MAX_THUMBNAILS: usize = 256;
const MAX_STORYBOARDS: usize = 64;
/// Seconds shown across the timeline at each zoom level, closest first.
pub(crate) const ZOOM_SECONDS: [u64; 16] = [
    1, 2, 3, 5, 8, 10, 15, 20, 30, 45, 60, 90, 120, 180, 300, 600,
];
const DEFAULT_ZOOM: usize = 5;

pub(crate) type Storyboard = Arc<Vec<Arc<RgbaImage>>>;

fn storyboard_requests(
    keys: &[(StoryboardKey, bool)],
    cache: &mut HashMap<StoryboardKey, Storyboard>,
    recent: &mut Vec<StoryboardKey>,
) -> Vec<StoryboardKey> {
    let live: HashSet<_> = keys.iter().map(|(key, _)| key.clone()).collect();
    cache.retain(|key, _| live.contains(key));
    recent.retain(|key| cache.contains_key(key));
    let visible: HashSet<_> = keys
        .iter()
        .filter(|(_, visible)| *visible)
        .map(|(key, _)| key.clone())
        .collect();
    let mut selected = Vec::new();
    let mut seen = HashSet::new();
    for key in keys
        .iter()
        .map(|(key, _)| key)
        .filter(|key| visible.contains(*key) && cache.contains_key(*key))
        .chain(
            keys.iter()
                .map(|(key, _)| key)
                .filter(|key| visible.contains(*key)),
        )
        .chain(recent.iter().rev())
        .chain(
            keys.iter()
                .map(|(key, _)| key)
                .filter(|key| cache.contains_key(*key)),
        )
        .chain(keys.iter().map(|(key, _)| key))
    {
        if seen.insert(key.clone()) {
            selected.push(key.clone());
            if selected.len() == MAX_STORYBOARDS {
                break;
            }
        }
    }
    cache.retain(|key, _| selected.contains(key));
    recent.retain(|key| selected.contains(key) && !visible.contains(key));
    for key in &selected {
        if !recent.contains(key) && !visible.contains(key) {
            recent.insert(0, key.clone());
        }
    }
    recent.extend(
        selected
            .iter()
            .filter(|key| visible.contains(*key))
            .cloned(),
    );
    selected
        .into_iter()
        .filter(|key| !cache.contains_key(key))
        .collect()
}

/// One GPU preview surface with its own position and decode stream.
#[derive(Clone)]
pub(crate) struct Monitor {
    pub slot: GpuFrameSlot,
    pub playing: Signal<bool>,
    pub position: Signal<u64>,
    cancel: Rc<RefCell<Option<Arc<AtomicBool>>>>,
    serial: Rc<Cell<u64>>,
    audio: Rc<RefCell<Option<Session>>>,
}

impl Monitor {
    fn new() -> Self {
        Self {
            slot: GpuFrameSlot::new(),
            playing: Signal::new(false),
            position: Signal::new(0),
            cancel: Rc::new(RefCell::new(None)),
            serial: Rc::new(Cell::new(0)),
            audio: Rc::new(RefCell::new(None)),
        }
    }

    fn cancel(&self) {
        if let Some(cancel) = self.cancel.borrow_mut().take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.audio.borrow_mut().take();
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

pub(crate) struct TitleGesture {
    pub clip: ClipRef,
    pub title: Title,
    pub pointer: Point,
    pub resize: bool,
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
    pub title_duration: TextController,
    pub title_font: TextController,
    pub title_size: TextController,
    pub title_color: TextController,
    pub title_picker: ColorPickerController,
    pub title_preview: Signal<Option<(ClipRef, Title)>>,
    pub title_gesture: Rc<RefCell<Option<TitleGesture>>>,
    pub media_query: TextController,
    pub media_filter: Signal<MediaFilter>,
    pub media_view: Signal<MediaView>,
    pub status: Signal<String>,
    pub dirty: Signal<bool>,
    pub busy: Signal<bool>,
    pub exporting: Signal<bool>,
    pub export_total: Signal<u64>,
    pub export_modal: Signal<bool>,
    pub export_page: Signal<ExportPage>,
    pub export_draft: Signal<ExportSettings>,
    pub export_preset: Signal<String>,
    pub export_library: Signal<presets::PresetLibrary>,
    pub export_presets_ready: Signal<bool>,
    pub export_path: TextController,
    pub export_name: TextController,
    pub export_width: TextController,
    pub export_height: TextController,
    pub export_fps_num: TextController,
    pub export_fps_den: TextController,
    pub export_video_bitrate: TextController,
    pub export_audio_bitrate: TextController,
    pub export_quality: TextController,
    pub export_scroll: ScrollController,
    pub thumbnails: Rc<RefCell<HashMap<PathBuf, Arc<RgbaImage>>>>,
    pub storyboards: Rc<RefCell<HashMap<StoryboardKey, Storyboard>>>,
    pub media_scroll: ScrollController,
    pub inspector_scroll: ScrollController,
    pub timeline_start: Signal<u64>,
    pub timeline_zoom: Signal<usize>,
    pub track_scroll: Signal<f32>,
    /// Ctrl+wheel travel not yet turned into a zoom step, so touchpads
    /// with many tiny deltas zoom at the same pace as a mouse wheel.
    pub zoom_travel: Rc<Cell<f32>>,
    pub timeline_expanded: Signal<bool>,
    pub gesture: Rc<RefCell<Gesture>>,
    pub drag: Signal<Option<DragState>>,
    pub trim_draft: Signal<Option<Command>>,
    pub trim_hover: Signal<bool>,
    pub marquee: Signal<Option<Rect>>,
    pub timeline_rect: Rc<Cell<Rect>>,
    pub modifiers: Rc<Cell<Modifiers>>,
    pub clipboard: Rc<RefCell<Vec<(u64, Clip)>>>,
    pub menu: Signal<Option<MenuState>>,
    pub pointer: Rc<Cell<Point>>,
    pub sidebar: Signal<Sidebar>,
    pub settings_modal: Signal<bool>,
    pub settings_page: Signal<SettingsPage>,
    pub settings_scroll: ScrollController,
    pub settings: Signal<Settings>,
    app: Rc<RefCell<Option<AppHandle>>>,
    window: Rc<RefCell<Option<WindowHandle>>>,
    settings_serial: Arc<AtomicU64>,
    settings_write: Arc<Mutex<()>>,
    storyboard_cancel: Rc<RefCell<Option<Arc<AtomicBool>>>>,
    storyboard_serial: Rc<Cell<u64>>,
    storyboard_recent: Rc<RefCell<Vec<StoryboardKey>>>,
    export_control: Rc<RefCell<Option<Arc<ExportControl>>>>,
    audio_output: Rc<RefCell<Option<playback::Output>>>,
    audio_failed: Rc<Cell<bool>>,
}

impl UiState {
    pub fn new() -> Self {
        let settings = settings::path()
            .and_then(|path| settings::load(&path).ok())
            .unwrap_or_default();
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
            title_duration: TextController::default(),
            title_font: TextController::default(),
            title_size: TextController::default(),
            title_color: TextController::default(),
            title_picker: ColorPickerController::new(),
            title_preview: Signal::new(None),
            title_gesture: Rc::new(RefCell::new(None)),
            media_query: TextController::default(),
            media_filter: Signal::new(MediaFilter::All),
            media_view: Signal::new(MediaView::Grid),
            status: Signal::new("Ready · Project not saved".into()),
            dirty: Signal::new(false),
            busy: Signal::new(false),
            exporting: Signal::new(false),
            export_total: Signal::new(0),
            export_modal: Signal::new(false),
            export_page: Signal::new(ExportPage::Presets),
            export_draft: Signal::new(ExportSettings::for_project(
                1920,
                1080,
                crate::project::FrameRate {
                    numerator: 30,
                    denominator: 1,
                },
            )),
            export_preset: Signal::new("Project original".into()),
            export_library: Signal::new(presets::PresetLibrary::default()),
            export_presets_ready: Signal::new(false),
            export_path: TextController::default(),
            export_name: TextController::default(),
            export_width: TextController::default(),
            export_height: TextController::default(),
            export_fps_num: TextController::default(),
            export_fps_den: TextController::default(),
            export_video_bitrate: TextController::default(),
            export_audio_bitrate: TextController::default(),
            export_quality: TextController::default(),
            export_scroll: ScrollController::default(),
            thumbnails: Rc::new(RefCell::new(HashMap::new())),
            storyboards: Rc::new(RefCell::new(HashMap::new())),
            media_scroll: ScrollController::default(),
            inspector_scroll: ScrollController::default(),
            timeline_start: Signal::new(0),
            timeline_zoom: Signal::new(DEFAULT_ZOOM),
            track_scroll: Signal::new(0.0),
            zoom_travel: Rc::new(Cell::new(0.0)),
            timeline_expanded: Signal::new(false),
            gesture: Rc::new(RefCell::new(Gesture::Idle)),
            drag: Signal::new(None),
            trim_draft: Signal::new(None),
            trim_hover: Signal::new(false),
            marquee: Signal::new(None),
            timeline_rect: Rc::new(Cell::new(Rect::default())),
            modifiers: Rc::new(Cell::new(Modifiers::default())),
            clipboard: Rc::new(RefCell::new(Vec::new())),
            menu: Signal::new(None),
            pointer: Rc::new(Cell::new(Point::default())),
            sidebar: Signal::new(Sidebar::Media),
            settings_modal: Signal::new(false),
            settings_page: Signal::new(SettingsPage::Appearance),
            settings_scroll: ScrollController::default(),
            settings: Signal::new(settings),
            app: Rc::new(RefCell::new(None)),
            window: Rc::new(RefCell::new(None)),
            settings_serial: Arc::new(AtomicU64::new(0)),
            settings_write: Arc::new(Mutex::new(())),
            storyboard_cancel: Rc::new(RefCell::new(None)),
            storyboard_serial: Rc::new(Cell::new(0)),
            storyboard_recent: Rc::new(RefCell::new(Vec::new())),
            export_control: Rc::new(RefCell::new(None)),
            audio_output: Rc::new(RefCell::new(None)),
            audio_failed: Rc::new(Cell::new(false)),
        }
    }

    pub fn connect(&self, app: AppHandle) {
        *self.app.borrow_mut() = Some(app.clone());
        if let Some(path) = presets::path() {
            let state = self.clone();
            app.spawn_background(
                move || presets::load(&path),
                move |result| match result {
                    Ok(library) => {
                        state.export_library.set(library);
                        state.export_presets_ready.set(true);
                    }
                    Err(error) => state
                        .status
                        .set(format!("Could not load export presets: {error}")),
                },
            );
        }
    }

    pub fn connect_window(&self, window: WindowHandle) {
        self.preview_available.set(window.gpu_device().is_some());
        self.install_input(&window);
        *self.window.borrow_mut() = Some(window);
        self.request_preview();
        self.request_source_preview();
    }

    fn update_settings(&self, settings: Settings) {
        if let Err(error) = settings.validate() {
            self.status.set(error);
            return;
        }
        if settings == self.settings.peek() {
            return;
        }
        self.settings.set(settings.clone());
        if settings.theme != ThemeMode::System
            && let Some(window) = self.window.borrow().as_ref()
        {
            window.set_theme(settings.theme());
        }
        if let (Some(app), Some(path)) = (self.app.borrow().as_ref(), settings::path()) {
            let state = self.clone();
            let serial = self.settings_serial.fetch_add(1, Ordering::Relaxed) + 1;
            let latest = self.settings_serial.clone();
            let write = self.settings_write.clone();
            app.spawn_background(
                move || {
                    let _guard = write
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    if serial != latest.load(Ordering::Relaxed) {
                        return Ok(());
                    }
                    settings::save(&path, &settings).map_err(|error| error.to_string())
                },
                move |result| {
                    if let Err(error) = result {
                        state
                            .status
                            .set(format!("Could not save settings: {error}"));
                    }
                },
            );
        }
    }

    pub fn set_theme_mode(&self, mode: ThemeMode) {
        let mut settings = self.settings.peek();
        settings.theme = mode;
        if mode == ThemeMode::System {
            settings.accent = None;
        }
        self.update_settings(settings);
    }

    pub fn set_accent(&self, id: &str) {
        let mut settings = self.settings.peek();
        settings.accent = Some(id.to_owned());
        self.update_settings(settings);
    }

    pub fn sync_system_theme(&self) {
        if self.settings.get().theme != ThemeMode::System || self.window.borrow().is_none() {
            return;
        }
        let appearance = creamui::use_system_appearance().get();
        if let Ok(resolved) =
            creamui::theme_loader::SystemThemeLoader::new().load_with_system(&appearance)
            && let Some(window) = self.window.borrow().as_ref()
            && window.theme() != resolved.theme
        {
            window.set_theme(resolved.theme);
        }
    }

    pub fn open_settings(&self) {
        if self.export_modal.peek() {
            return;
        }
        self.close_menu();
        self.settings_scroll.set(0.0);
        self.settings_page.set(SettingsPage::Appearance);
        self.settings_modal.set(true);
    }

    pub fn close_settings(&self) {
        self.settings_modal.set(false);
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
            self.sync_title_controllers();
        }
    }

    pub fn redo(&self) {
        if self.editor.borrow_mut().redo() {
            self.refresh_after_edit("Redid edit · Unsaved changes");
            self.sync_title_controllers();
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
            self.queue_storyboards();
        }
    }

    /// Zooms keeping the playhead where it is on screen when it is visible,
    /// otherwise keeping the middle of the view.
    pub fn zoom_timeline(&self, zoom: usize) {
        let (start, span) = (self.timeline_start.peek(), self.visible_frames());
        let playhead = self.program.position.peek();
        let (anchor, fraction) = if playhead >= start && playhead < start + span {
            (playhead, (playhead - start) as f64 / span as f64)
        } else {
            (start + span / 2, 0.5)
        };
        self.zoom_timeline_around(zoom, anchor, fraction);
    }

    /// Zooms so `anchor` stays `fraction` of the way across the view.
    pub fn zoom_timeline_around(&self, zoom: usize, anchor: u64, fraction: f64) {
        let zoom = zoom.min(ZOOM_SECONDS.len() - 1);
        if zoom == self.timeline_zoom.peek() {
            return;
        }
        self.timeline_zoom.set(zoom);
        let span = self.visible_frames() as f64;
        let start = (anchor as f64 - fraction.clamp(0.0, 1.0) * span)
            .max(0.0)
            .round() as u64;
        self.timeline_start
            .set(start.min(self.sequence_end().saturating_sub(1)));
        self.queue_storyboards();
    }

    /// The closest zoom that shows the whole sequence, from its start.
    pub fn zoom_to_fit(&self) {
        let (numerator, denominator) = self.frame_rate();
        let seconds =
            self.sequence_end() as f64 * f64::from(denominator) / f64::from(numerator.max(1));
        let zoom = ZOOM_SECONDS
            .iter()
            .position(|level| *level as f64 >= seconds * 1.05)
            .unwrap_or(ZOOM_SECONDS.len() - 1);
        self.timeline_zoom.set(zoom);
        self.timeline_start.set(0);
        self.queue_storyboards();
    }

    pub fn scroll_timeline(&self, steps: i64) {
        let step = (self.visible_frames() / 8).max(1) as i64;
        self.scroll_timeline_frames(step.saturating_mul(steps));
    }

    /// Moves the view by `frames`, staying within the sequence.
    pub fn scroll_timeline_frames(&self, frames: i64) {
        let start = self.timeline_start.peek();
        let next = start
            .saturating_add_signed(frames)
            .min(self.sequence_end().saturating_sub(1));
        if next != start {
            self.timeline_start.set(next);
            self.queue_storyboards();
        }
    }

    pub fn focus_frame(&self, frame: u64) {
        self.program.position.set(frame);
        self.follow_playhead(frame);
        self.request_preview();
    }

    pub fn play(&self) {
        let project = self.editor.borrow();
        let mut has_picture = false;
        let mut playable = false;
        for track in project.project().tracks() {
            for clip in &track.clips {
                if track.kind == TrackKind::Video && matches!(clip.content, ClipContent::Title(_)) {
                    playable = true;
                }
                let ClipContent::Media { media_id } = clip.content else {
                    continue;
                };
                let Some(media) = project
                    .project()
                    .media()
                    .iter()
                    .find(|media| media.id == media_id)
                else {
                    continue;
                };
                if track.kind == TrackKind::Video && media.kind != MediaKind::Audio {
                    has_picture = true;
                    playable = true;
                } else if audible(track.kind, clip, media) {
                    playable = true;
                }
            }
        }
        drop(project);
        if has_picture && !self.preview_available.peek() {
            self.status
                .set("Video preview is not available on this display".into());
            return;
        }
        let end = self.sequence_end();
        if !playable || end == 0 {
            self.status
                .set("Add a clip to the timeline to play the sequence".into());
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

    fn sync_title_controllers(&self) {
        let Some((track_id, clip_id)) = self.primary_clip() else {
            return;
        };
        let editor = self.editor.borrow();
        let title = editor
            .project()
            .tracks()
            .iter()
            .find(|track| track.id == track_id)
            .and_then(|track| track.clips.iter().find(|clip| clip.id == clip_id))
            .and_then(|clip| match &clip.content {
                ClipContent::Title(title) => Some((title.clone(), clip.duration_frames)),
                ClipContent::Media { .. } => None,
            });
        drop(editor);
        let Some((title, duration)) = title else {
            return;
        };
        let fps = self.frame_rate();
        self.title_duration
            .set_value((duration as f64 * f64::from(fps.1) / f64::from(fps.0)).to_string());
        self.title_text.set_value(title.text);
        self.title_font.set_value(title.font_family);
        self.title_size.set_value(title.font_size.to_string());
        self.title_color.set_value(format!(
            "#{:02X}{:02X}{:02X}{:02X}",
            title.color[0], title.color[1], title.color[2], title.color[3]
        ));
    }

    pub(super) fn clear_title_gesture(&self) {
        self.title_gesture.borrow_mut().take();
        if self.title_preview.peek().is_some() {
            self.title_preview.set(None);
            self.request_preview();
        }
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
        self.clear_title_gesture();
        self.title_picker.set_open(false);
        self.selected.set(selection);
        let primary = self.primary_clip();
        if primary.is_some() {
            self.selected_media.set(None);
            self.sidebar.set(Sidebar::Inspector);
            self.sync_title_controllers();
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
        self.clear_title_gesture();
        self.title_picker.set_open(false);
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
        let Some(range) = self.source_range() else {
            self.status
                .set("Select a clip or source to preview it".into());
            return;
        };
        if range.kind != MediaKind::Audio && !self.preview_available.peek() {
            self.status
                .set("Video preview is not available on this display".into());
            return;
        }
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

    pub(super) fn request_preview(&self) {
        let editor = self.editor.borrow();
        let project = editor.project();
        let mut sources = Vec::new();
        let mut audio_sources = Vec::new();
        for track in project.tracks() {
            for clip in &track.clips {
                let ClipContent::Media { media_id } = clip.content else {
                    continue;
                };
                let Some(media) = project.media().iter().find(|media| media.id == media_id) else {
                    continue;
                };
                if track.kind == TrackKind::Video {
                    sources.push(FrameSource {
                        clip_id: clip.id,
                        path: media.path.clone(),
                        kind: media.kind,
                        start: clip.start_frame,
                        source_start: clip.source_start_frame,
                        duration: clip.duration_frames,
                    });
                }
                if audible(track.kind, clip, media) {
                    audio_sources.push(AudioSource {
                        path: media.path.clone(),
                        start: clip.start_frame,
                        source_start: clip.source_start_frame,
                        duration: clip.duration_frames,
                        volume: clip.volume,
                    });
                }
            }
        }
        let fps = (
            project.settings().frame_rate.numerator,
            project.settings().frame_rate.denominator,
        );
        let resolution = (project.settings().width, project.settings().height);
        let mut visual = VisualPlan::from_project(project);
        if let Some(((.., clip_id), title)) = self.title_preview.peek() {
            visual.replace_title(clip_id, title);
        }
        drop(editor);
        let end = self.sequence_end();
        let plan = PreviewPlan {
            sources,
            visual: Some(visual),
            end,
            fps,
            resolution,
        };
        let audio = AudioPlan {
            sources: audio_sources,
            end,
            fps,
        };
        self.stream(&self.program, plan, audio, true);
    }

    fn request_source_preview(&self) {
        let range = self.source_range();
        let editor = self.editor.borrow();
        let settings = editor.project().settings();
        let fps = (
            settings.frame_rate.numerator,
            settings.frame_rate.denominator,
        );
        let end = range.as_ref().map_or(0, |range| range.duration);
        let plan = PreviewPlan {
            end,
            visual: None,
            sources: range
                .iter()
                .filter(|range| range.kind != MediaKind::Audio)
                .map(|range| FrameSource {
                    clip_id: 0,
                    path: range.path.clone(),
                    kind: range.kind,
                    start: 0,
                    source_start: range.source_start,
                    duration: range.duration,
                })
                .collect(),
            fps,
            resolution: (settings.width, settings.height),
        };
        let audio = AudioPlan {
            end,
            sources: range
                .iter()
                .filter(|range| range.kind != MediaKind::Image)
                .map(|range| AudioSource {
                    path: range.path.clone(),
                    start: 0,
                    source_start: range.source_start,
                    duration: range.duration,
                    volume: 1.0,
                })
                .collect(),
            fps,
        };
        drop(editor);
        if plan.end > 0 && self.source.position.peek() >= plan.end {
            self.source.position.set(plan.end - 1);
        }
        self.stream(&self.source, plan, audio, false);
    }

    fn stream(
        &self,
        monitor: &Monitor,
        plan: PreviewPlan,
        audio: AudioPlan,
        follow_timeline: bool,
    ) {
        monitor.cancel();
        let Some(window) = self.window.borrow().clone() else {
            return;
        };
        if window.is_closed() {
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
        if !plan.sources.is_empty() && !self.preview_available.peek() {
            return;
        }
        let serial = monitor.serial.get();
        let cancelled = Arc::new(AtomicBool::new(false));
        *monitor.cancel.borrow_mut() = Some(cancelled.clone());
        let clock = if playing && start < plan.end {
            self.start_audio(monitor, audio, start, &app, serial)
        } else {
            None
        };
        let state = self.clone();
        let monitor = monitor.clone();
        let end = plan.end;
        let pending = Arc::new(AtomicUsize::new(0));
        let worker_pending = pending.clone();
        app.spawn_stream(
            move |emit| {
                if let Err(error) =
                    decode_sequence(&plan, start, playing, cancelled, clock, |event| {
                        if matches!(event, PreviewEvent::Frame(..))
                            && worker_pending
                                .try_update(Ordering::Relaxed, Ordering::Relaxed, |count| {
                                    (count < 2).then_some(count + 1)
                                })
                                .is_err()
                        {
                            return;
                        }
                        emit(event);
                    })
                {
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
                        monitor.position.set(end - 1);
                        if follow_timeline {
                            state.follow_playhead(end - 1);
                        }
                        state.stop(&monitor);
                        return;
                    }
                    PreviewEvent::Failed(error) => {
                        state.stop(&monitor);
                        state.status.set(format!("Preview failed: {error}"));
                        return;
                    }
                };
                let image = if follow_timeline && !state.preview_available.peek() {
                    None
                } else {
                    image
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

    /// Starts a sound session for a playing stream and returns the clock
    /// video pacing follows; `None` keeps playback timed by the wall clock.
    fn start_audio(
        &self,
        monitor: &Monitor,
        plan: AudioPlan,
        start: u64,
        app: &AppHandle,
        serial: u64,
    ) -> Option<Clock> {
        if plan.sources.is_empty() {
            return None;
        }
        if self.audio_output.borrow().is_none() && !self.audio_failed.get() {
            match playback::Output::open() {
                Ok(output) => *self.audio_output.borrow_mut() = Some(output),
                Err(error) => {
                    self.audio_failed.set(true);
                    self.status.set(error);
                }
            }
        }
        let output = self.audio_output.borrow();
        let session = playback::play(output.as_ref()?);
        let clock = session.clock.clone();
        let queue = session.queue.clone();
        let cancelled = session.cancel.clone();
        drop(output);
        *monitor.audio.borrow_mut() = Some(session);
        let state = self.clone();
        let monitor = monitor.clone();
        app.spawn_background(
            move || audio::feed(&plan, start, queue, cancelled),
            move |result| {
                if monitor.serial.get() == serial
                    && let Err(error) = result
                {
                    state.status.set(format!("Audio preview: {error}"));
                }
            },
        );
        Some(clock)
    }

    fn reset_monitors(&self) {
        self.pause();
        self.pause_source();
        self.clear_title_gesture();
        self.title_picker.set_open(false);
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

    pub(super) fn queue_storyboards(&self) {
        self.cancel_storyboards();
        let Some(app) = self.app.borrow().clone() else {
            return;
        };
        let requests = self.storyboard_requests();
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

    fn storyboard_requests(&self) -> Vec<StoryboardKey> {
        let editor = self.editor.borrow();
        let project = editor.project();
        let fps = (
            project.settings().frame_rate.numerator,
            project.settings().frame_rate.denominator,
        );
        let start = self.timeline_start.peek();
        let end = start.saturating_add(self.visible_frames());
        let weights: Vec<_> = project
            .tracks()
            .iter()
            .map(|track| {
                super::timeline_view::lane_weight(
                    track.kind,
                    track.clips.iter().any(|clip| clip.linked_audio),
                )
            })
            .collect();
        let geometry = super::timeline_view::Geometry::new(
            self.timeline_rect.get(),
            &weights,
            start,
            self.visible_frames(),
            self.track_scroll.peek(),
        );
        let keys: Vec<_> = project
            .tracks()
            .iter()
            .enumerate()
            .filter(|(_, track)| track.kind == TrackKind::Video)
            .flat_map(|(index, track)| track.clips.iter().map(move |clip| (index, clip)))
            .filter_map(|(index, clip)| {
                let ClipContent::Media { media_id } = clip.content else {
                    return None;
                };
                let media = project.media().iter().find(|media| media.id == media_id)?;
                Some((
                    StoryboardKey {
                        path: media.path.clone(),
                        kind: media.kind,
                        source_start: clip.source_start_frame,
                        duration: clip.duration_frames,
                        fps,
                    },
                    geometry.lane_visible(index)
                        && clip.start_frame < end
                        && clip.start_frame.saturating_add(clip.duration_frames) > start,
                ))
            })
            .collect();
        drop(editor);
        let mut cache = self.storyboards.borrow_mut();
        storyboard_requests(&keys, &mut cache, &mut self.storyboard_recent.borrow_mut())
    }

    /// Returns true when the caller may go ahead now. Choosing Save returns
    /// false and runs `after_save` only once that save succeeds.
    pub fn settle_unsaved_changes(&self, after_save: impl FnOnce(&Self) + 'static) -> bool {
        !self.dirty.peek() || self.apply_unsaved_choice(self.ask_unsaved_changes(), after_save)
    }

    fn ask_unsaved_changes(&self) -> UnsavedChoice {
        let name = self.project_path.peek().map_or_else(
            || self.editor.borrow().project().name().to_owned(),
            |path| {
                path.file_stem().map_or_else(
                    || path.display().to_string(),
                    |name| name.to_string_lossy().into_owned(),
                )
            },
        );
        unsaved_choice(
            MessageDialog::new()
                .set_level(MessageLevel::Warning)
                .set_title("Unsaved changes")
                .set_description(format!(
                    "Save changes to \u{201c}{name}\u{201d}? Changes you don't save will be lost."
                ))
                .set_buttons(MessageButtons::YesNoCancelCustom(
                    SAVE_CHOICE.into(),
                    DISCARD_CHOICE.into(),
                    CANCEL_CHOICE.into(),
                ))
                .show(),
        )
    }

    fn apply_unsaved_choice(
        &self,
        choice: UnsavedChoice,
        after_save: impl FnOnce(&Self) + 'static,
    ) -> bool {
        match choice {
            UnsavedChoice::Discard => true,
            UnsavedChoice::Cancel => false,
            UnsavedChoice::Save => {
                self.save_then(Some(Box::new(after_save)));
                false
            }
        }
    }

    pub fn close_window(&self) {
        if let Some(window) = self.window.borrow().as_ref() {
            window.close();
        }
    }

    pub fn new_project(&self) {
        if self.busy.peek() || !self.settle_unsaved_changes(Self::new_project) {
            return;
        }
        *self.editor.borrow_mut() = Editor::new(Project::new("Untitled"));
        self.export_modal.set(false);
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
        {
            let retry = path.clone();
            if self.settle_unsaved_changes(move |state| state.load_path(retry)) {
                self.load_path(path);
            }
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
                        state.export_modal.set(false);
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
        self.save_then(None);
    }

    fn save_then(&self, after_save: Option<AfterSave>) {
        if let Some(path) = self.project_path.peek() {
            self.save_to(path, after_save);
        } else {
            self.save_as_then(after_save);
        }
    }

    pub fn save_as_dialog(&self) {
        self.save_as_then(None);
    }

    fn save_as_then(&self, after_save: Option<AfterSave>) {
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
                    state.save_to(path, after_save);
                },
            );
            return;
        }
        self.save_to(path, after_save);
    }

    fn save_to(&self, path: PathBuf, after_save: Option<AfterSave>) {
        self.save_to_with_export(path, None, after_save);
    }

    fn save_to_with_export(
        &self,
        path: PathBuf,
        exported: Option<PathBuf>,
        after_save: Option<AfterSave>,
    ) {
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
            move |result| state.finish_save(path, revision, exported, after_save, result),
        );
    }

    fn finish_save(
        &self,
        path: PathBuf,
        revision: u64,
        exported: Option<PathBuf>,
        after_save: Option<AfterSave>,
        result: std::io::Result<()>,
    ) {
        self.busy.set(false);
        let saved_current = result.is_ok() && self.revision.peek() == revision;
        self.status.set(match result {
            Ok(()) if saved_current => {
                self.project_path.set(Some(path));
                self.dirty.set(false);
                exported.map_or_else(
                    || "Project saved".into(),
                    |path| format!("Exported: {} · project saved", path.display()),
                )
            }
            Ok(()) => "Saved previous version · Unsaved changes".into(),
            Err(error) => exported.map_or_else(
                || format!("Save failed: {error}"),
                |path| {
                    format!(
                        "Exported: {} · project save failed: {error}",
                        path.display()
                    )
                },
            ),
        });
        if saved_current && let Some(after_save) = after_save {
            after_save(self);
        }
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
        if self.busy.peek() || !export::can_render(self.editor.borrow().project()) {
            return;
        }
        let (settings, path, has_last) = {
            let editor = self.editor.borrow();
            let project = editor.project();
            let last = project.last_export();
            let settings = last.map_or_else(
                || {
                    ExportSettings::for_project(
                        project.settings().width,
                        project.settings().height,
                        project.settings().frame_rate.clone(),
                    )
                },
                |last| last.settings.clone(),
            );
            let path = last.map_or_else(
                || {
                    self.project_path
                        .peek()
                        .and_then(|path| path.parent().map(Path::to_path_buf))
                        .or_else(|| std::env::current_dir().ok())
                        .unwrap_or_default()
                        .join(format!("export.{}", settings.container.extension()))
                },
                |last| {
                    if last.path.is_absolute() {
                        next_export_path(&last.path)
                    } else {
                        self.project_path
                            .peek()
                            .and_then(|path| path.parent().map(Path::to_path_buf))
                            .or_else(|| std::env::current_dir().ok())
                            .unwrap_or_default()
                            .join(format!("export.{}", settings.container.extension()))
                    }
                },
            );
            (settings, path, last.is_some())
        };
        self.set_export_draft(settings);
        self.export_preset.set(if has_last {
            "Last used".into()
        } else {
            "Project original".into()
        });
        self.export_path
            .set_value(path.to_string_lossy().into_owned());
        self.export_name.set_value(String::new());
        self.export_page.set(ExportPage::Presets);
        self.export_scroll.set(0.0);
        self.export_modal.set(true);
    }

    pub fn close_export_dialog(&self) {
        self.export_modal.set(false);
    }

    pub fn set_export_draft(&self, settings: ExportSettings) {
        self.export_width.set_value(settings.width.to_string());
        self.export_height.set_value(settings.height.to_string());
        self.export_fps_num
            .set_value(settings.frame_rate.numerator.to_string());
        self.export_fps_den
            .set_value(settings.frame_rate.denominator.to_string());
        self.export_video_bitrate
            .set_value(settings.video_bitrate_kbps.to_string());
        self.export_audio_bitrate
            .set_value(settings.audio_bitrate_kbps.to_string());
        self.export_quality
            .set_value(settings.video_quality.unwrap_or(23).to_string());
        let old = self.export_draft.peek().container.extension();
        let path = self.export_path.peek();
        if Path::new(&path)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case(old))
            && settings.container.extension() != old
        {
            self.export_path.set_value(
                Path::new(&path)
                    .with_extension(settings.container.extension())
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        self.export_draft.set(settings);
    }

    pub fn select_export_preset(&self, name: &str) {
        let selected = {
            let project = self.editor.borrow();
            presets::builtins(project.project())
                .into_iter()
                .chain(self.export_library.peek().presets)
                .find(|preset| preset.name == name)
        };
        if let Some(preset) = selected {
            self.set_export_draft(preset.settings);
            self.export_preset.set(preset.name);
        }
    }

    pub fn export_form(&self) -> Result<ExportSettings, String> {
        self.export_form_for_quality(self.export_draft.peek().video_quality.is_some())
    }

    fn export_form_for_quality(&self, constant_quality: bool) -> Result<ExportSettings, String> {
        let mut settings = self.export_draft.peek();
        let field = |controller: &TextController, label: &str| {
            controller
                .peek()
                .trim()
                .parse::<u32>()
                .map_err(|_| format!("Enter a valid {label}"))
        };
        settings.width = field(&self.export_width, "width")?;
        settings.height = field(&self.export_height, "height")?;
        settings.frame_rate.numerator = field(&self.export_fps_num, "frame rate numerator")?;
        settings.frame_rate.denominator = field(&self.export_fps_den, "frame rate denominator")?;
        settings.audio_bitrate_kbps = field(&self.export_audio_bitrate, "audio bitrate")?;
        if constant_quality {
            settings.video_quality = Some(
                u8::try_from(field(&self.export_quality, "CRF quality")?)
                    .map_err(|_| "CRF quality is out of range")?,
            );
        } else {
            settings.video_quality = None;
            settings.video_bitrate_kbps = field(&self.export_video_bitrate, "video bitrate")?;
        }
        settings.validate()?;
        Ok(settings)
    }

    pub fn set_export_quality_mode(&self, constant_quality: bool) {
        let settings = match self.export_form_for_quality(constant_quality) {
            Ok(settings) => settings,
            Err(error) => {
                self.status.set(error);
                return;
            }
        };
        self.set_export_draft(settings);
        self.export_preset.set("Custom".into());
    }

    pub fn set_export_speed(&self, speed: EncodingSpeed) {
        let Ok(mut settings) = self.export_form() else {
            self.status.set("Fix the export fields first".into());
            return;
        };
        settings.speed = speed;
        self.set_export_draft(settings);
        self.export_preset.set("Custom".into());
    }

    pub fn change_export_codecs(
        &self,
        container: Option<Container>,
        video: Option<VideoCodec>,
        audio: Option<AudioCodec>,
    ) {
        let Ok(mut settings) = self.export_form() else {
            self.status
                .set("Fix the numeric export fields first".into());
            return;
        };
        if let Some(container) = container {
            settings.container = container;
            match container {
                Container::Mp4 => {
                    if settings.video_codec == VideoCodec::Vp9 {
                        settings.video_codec = VideoCodec::H264;
                    }
                    settings.audio_codec = AudioCodec::Aac;
                }
                Container::WebM => {
                    settings.video_codec = VideoCodec::Vp9;
                    settings.audio_codec = AudioCodec::Opus;
                }
                Container::Matroska => {}
            }
        }
        if let Some(video) = video {
            settings.video_codec = video;
            if settings.container == Container::WebM && video != VideoCodec::Vp9
                || settings.container == Container::Mp4 && video == VideoCodec::Vp9
            {
                settings.container = Container::Matroska;
            }
        }
        if let Some(audio) = audio {
            settings.audio_codec = audio;
            if settings.container == Container::WebM && audio != AudioCodec::Opus
                || settings.container == Container::Mp4 && audio == AudioCodec::Opus
            {
                settings.container = Container::Matroska;
            }
        }
        if let Some(quality) = &mut settings.video_quality {
            *quality = (*quality).min(if settings.video_codec == VideoCodec::Vp9 {
                63
            } else {
                51
            });
        }
        self.set_export_draft(settings);
        self.export_preset.set("Custom".into());
    }

    pub fn browse_export_path(&self) {
        let settings = self.export_draft.peek();
        let path = PathBuf::from(self.export_path.peek());
        let mut dialog = FileDialog::new()
            .set_title("Export video")
            .add_filter("Video", &[settings.container.extension()])
            .set_file_name(
                path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("export"),
            );
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            dialog = dialog.set_directory(parent);
        }
        if let Some(selected) = dialog.save_file() {
            self.export_path.set_value(
                selected
                    .with_extension(settings.container.extension())
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }

    pub fn save_export_preset(&self) {
        if !self.export_presets_ready.peek() {
            self.status
                .set("Preset library is not ready; check the config file".into());
            return;
        }
        let settings = match self.export_form() {
            Ok(settings) => settings,
            Err(error) => {
                self.status.set(error);
                return;
            }
        };
        let mut library = self.export_library.peek();
        let name = self.export_name.peek().trim().to_owned();
        if presets::builtins(self.editor.borrow().project())
            .iter()
            .any(|preset| preset.name.eq_ignore_ascii_case(&name))
        {
            self.status
                .set("That name belongs to a built-in preset".into());
            return;
        }
        if let Err(error) = library.insert(name.clone(), settings.clone()) {
            self.status.set(error);
            return;
        }
        let (Some(app), Some(path)) = (self.app.borrow().as_ref().cloned(), presets::path()) else {
            self.status.set("Preset folder is not available".into());
            return;
        };
        self.busy.set(true);
        let state = self.clone();
        let save_name = name.clone();
        app.spawn_background(
            move || -> Result<presets::PresetLibrary, String> {
                let mut library = presets::load(&path)?;
                library.insert(save_name, settings)?;
                presets::save(&path, &library)?;
                Ok(library)
            },
            move |result| {
                state.busy.set(false);
                match result {
                    Ok(library) => {
                        state.export_library.set(library);
                        state.export_preset.set(name);
                        state.export_name.set_value(String::new());
                        state
                            .status
                            .set("Export preset saved for all projects".into());
                    }
                    Err(error) => state.status.set(format!("Could not save preset: {error}")),
                }
            },
        );
    }

    pub fn submit_export(&self) {
        let settings = match self.export_form() {
            Ok(settings) => settings,
            Err(error) => {
                self.status.set(error);
                return;
            }
        };
        let mut path = PathBuf::from(self.export_path.peek().trim());
        if !path.is_absolute() {
            self.status.set("Choose an absolute output path".into());
            return;
        }
        if path.extension().is_none() {
            path.set_extension(settings.container.extension());
        }
        self.start_export(path, settings, false);
    }

    pub fn quick_export(&self) {
        if self.busy.peek() {
            return;
        }
        let last = self.editor.borrow().project().last_export().cloned();
        if let Some(last) = last.filter(|last| last.path.is_absolute()) {
            self.start_export(next_export_path(&last.path), last.settings, true);
        } else {
            self.export_dialog();
        }
    }

    fn start_export(&self, path: PathBuf, settings: ExportSettings, quick: bool) {
        if self.busy.peek() || !export::can_render(self.editor.borrow().project()) {
            return;
        }
        let last = LastExport {
            settings: settings.clone(),
            path: path.clone(),
        };
        if let Err(error) = last.validate() {
            self.status.set(error);
            return;
        }
        let Some(app) = self.app.borrow().clone() else {
            self.status.set("Export requires a running app".into());
            return;
        };
        let project = self.editor.borrow().project().clone();
        let project_path = self.project_path.peek();
        let was_modal = self.export_modal.peek();
        let control = Arc::new(ExportControl::default());
        let end = self.sequence_end();
        let fps = &project.settings().frame_rate;
        self.export_total.set(
            export::output_frames(
                end,
                (fps.numerator, fps.denominator),
                (
                    settings.frame_rate.numerator,
                    settings.frame_rate.denominator,
                ),
            )
            .unwrap_or(0),
        );
        self.export_modal.set(false);
        *self.export_control.borrow_mut() = Some(control.clone());
        self.exporting.set(true);
        self.busy.set(true);
        self.status
            .set(format!("Encoding {}…", settings.container.extension()));
        let state = self.clone();
        app.spawn_background(
            move || {
                let destination = if quick {
                    unused_export_path(path)
                } else {
                    path
                };
                let result =
                    export::render_with_settings(&project, &destination, &settings, control);
                (destination, settings, result)
            },
            move |(path, settings, result)| {
                state.exporting.set(false);
                state.export_control.borrow_mut().take();
                state.busy.set(false);
                if result
                    .as_ref()
                    .is_err_and(|error| error != "Export cancelled")
                    && was_modal
                {
                    state.export_modal.set(true);
                }
                state.status.set(match result {
                    Ok(()) => {
                        state.edit(Command::SetLastExport(Some(LastExport {
                            settings,
                            path: path.clone(),
                        })));
                        if let Some(project_path) = &project_path {
                            state.save_to_with_export(
                                project_path.clone(),
                                Some(path.clone()),
                                None,
                            );
                        }
                        if project_path.is_some() {
                            format!("Exported: {}", path.display())
                        } else {
                            format!(
                                "Exported: {} · Save project to keep settings",
                                path.display()
                            )
                        }
                    }
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
        let dialog_app = app.clone();
        app.spawn_background(
            move || {
                paths
                    .into_iter()
                    .map(|path| {
                        let result = inspect_source(&path);
                        (path, result)
                    })
                    .collect::<Vec<_>>()
            },
            move |results| {
                state.busy.set(false);
                let total = results.len();
                let mut imported = 0;
                let mut ready = Vec::new();
                let mut errors = Vec::new();
                for (source_path, result) in results {
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
                                    errors.push((
                                        source_path,
                                        "Source rejected by project validation".into(),
                                    ));
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
                                errors.push((source_path, "Media ID limit reached".into()));
                            }
                        }
                        Err(error) => errors.push((source_path, error)),
                    }
                }
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
                let summary = import_summary(imported, total, &errors);
                if errors.is_empty() {
                    state.status.set(summary);
                } else {
                    state.status.set(format!(
                        "{imported} of {total} files imported · {} failed",
                        errors.len()
                    ));
                    dialog_app.spawn_background(
                        move || {
                            MessageDialog::new()
                                .set_level(MessageLevel::Warning)
                                .set_title("Import incomplete")
                                .set_description(&summary)
                                .set_buttons(MessageButtons::Ok)
                                .show()
                        },
                        |_| {},
                    );
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

/// Whether a clip contributes sound to preview playback.
pub(crate) fn audible(kind: TrackKind, clip: &Clip, media: &Media) -> bool {
    !clip.muted
        && match kind {
            TrackKind::Video => clip.linked_audio && media.has_audio,
            TrackKind::Audio => media.kind == MediaKind::Audio || media.has_audio,
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

fn next_export_path(path: &Path) -> PathBuf {
    let stem = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("export");
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("mp4");
    let (prefix, next) = stem
        .rsplit_once('-')
        .and_then(|(prefix, number)| {
            number
                .parse::<u32>()
                .ok()
                .and_then(|n| n.checked_add(1))
                .map(|next| (prefix, next))
        })
        .unwrap_or((stem, 2));
    path.with_file_name(format!("{prefix}-{next}.{extension}"))
}

fn unused_export_path(mut path: PathBuf) -> PathBuf {
    for _ in 0..10_000 {
        if !path.exists() {
            break;
        }
        path = next_export_path(&path);
    }
    path
}

/// Platforms without custom button labels report the Yes/No/Cancel buttons.
fn unsaved_choice(result: MessageDialogResult) -> UnsavedChoice {
    match result {
        MessageDialogResult::Yes => UnsavedChoice::Save,
        MessageDialogResult::No => UnsavedChoice::Discard,
        MessageDialogResult::Custom(label) if label == SAVE_CHOICE => UnsavedChoice::Save,
        MessageDialogResult::Custom(label) if label == DISCARD_CHOICE => UnsavedChoice::Discard,
        _ => UnsavedChoice::Cancel,
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

fn import_summary(imported: usize, total: usize, errors: &[(PathBuf, String)]) -> String {
    if errors.is_empty() {
        format!("{imported} of {total} files imported")
    } else {
        let mut summary = format!("{} of {total} files couldn't be imported:", errors.len());
        for (path, error) in errors.iter().take(8) {
            let name = path
                .file_name()
                .unwrap_or(path.as_os_str())
                .to_string_lossy();
            summary.push_str(&format!("\n{name} — {error}"));
        }
        if errors.len() > 8 {
            summary.push_str(&format!("\nand {} more", errors.len() - 8));
        }
        summary
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn storyboard_ranges() -> Vec<(StoryboardKey, bool)> {
        (0..100)
            .map(|index| {
                (
                    StoryboardKey {
                        path: "video.mp4".into(),
                        kind: MediaKind::Video,
                        source_start: index * 30,
                        duration: 30,
                        fps: (30, 1),
                    },
                    index >= 90,
                )
            })
            .collect()
    }

    #[test]
    fn storyboard_visible_later_ranges_replace_oldest_hidden_entries() {
        let keys = storyboard_ranges();
        let mut cache: HashMap<_, _> = keys[..64]
            .iter()
            .map(|(key, _)| (key.clone(), Arc::new(Vec::new())))
            .collect();
        let mut recent: Vec<_> = keys[..64].iter().map(|(key, _)| key.clone()).collect();
        let requests = storyboard_requests(&keys, &mut cache, &mut recent);
        let visible_queued = requests
            .iter()
            .filter(|key| keys[90..].iter().any(|(visible, _)| visible == *key))
            .count();
        println!(
            "100 ranges, viewport 90..100: {visible_queued}/10 visible queued; {} cached + {} queued",
            cache.len(),
            requests.len()
        );
        assert_eq!(visible_queued, 10);
        assert!(cache.len() + requests.len() <= MAX_STORYBOARDS);
        assert!(keys[..10].iter().all(|(key, _)| !cache.contains_key(key)));
        assert!(keys[10..64].iter().all(|(key, _)| cache.contains_key(key)));
    }

    #[test]
    fn storyboard_visible_cached_entries_are_protected() {
        let mut keys = storyboard_ranges();
        keys[0].1 = true;
        let mut cache: HashMap<_, _> = keys[..64]
            .iter()
            .map(|(key, _)| (key.clone(), Arc::new(Vec::new())))
            .collect();
        let mut recent: Vec<_> = keys[..64].iter().map(|(key, _)| key.clone()).collect();
        let requests = storyboard_requests(&keys, &mut cache, &mut recent);
        assert!(cache.contains_key(&keys[0].0));
        assert_eq!(requests.len(), 10);
        assert!(cache.len() + requests.len() <= MAX_STORYBOARDS);
    }

    #[test]
    fn storyboard_viewport_scroll_and_zoom_request_later_clip_ranges() {
        let state = UiState::new();
        state.timeline_rect.set(Rect {
            x: 0.0,
            y: 0.0,
            width: 1000.0,
            height: 400.0,
        });
        assert!(state.edit(Command::ImportMedia(Media {
            id: 1,
            path: "video.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(100_000_000),
            has_audio: false,
        })));
        for index in 0..100 {
            assert!(state.edit(Command::InsertClip {
                track_id: 0,
                clip: Clip {
                    id: index,
                    start_frame: index * 30,
                    source_start_frame: index * 30,
                    duration_frames: 30,
                    content: ClipContent::Media { media_id: 1 },
                    volume: 1.0,
                    muted: false,
                    linked_audio: false,
                }
            }));
        }
        for key in state.storyboard_requests() {
            state
                .storyboards
                .borrow_mut()
                .insert(key, Arc::new(Vec::new()));
        }
        state.scroll_timeline_frames(2700);
        let requests = state.storyboard_requests();
        assert_eq!(requests.len(), 10);
        assert!(requests.iter().all(|key| key.source_start >= 2700));
        for key in requests {
            state
                .storyboards
                .borrow_mut()
                .insert(key, Arc::new(Vec::new()));
        }
        state.zoom_timeline_around(5 + 1, 2700, 0.0);
        assert!(state.storyboard_requests().is_empty());
        assert_eq!(state.storyboards.borrow().len(), MAX_STORYBOARDS);
        state.zoom_to_fit();
        assert!(state.storyboard_requests().is_empty());
        assert_eq!(state.storyboards.borrow().len(), MAX_STORYBOARDS);
    }

    #[test]
    fn storyboard_empty_obsolete_duplicate_and_failed_requests_stay_bounded() {
        let mut keys = storyboard_ranges();
        keys.iter_mut().for_each(|(_, visible)| *visible = true);
        keys.push(keys[0].clone());
        let mut cache = HashMap::new();
        let mut recent = Vec::new();
        let requests = storyboard_requests(&keys, &mut cache, &mut recent);
        assert_eq!(requests.len(), MAX_STORYBOARDS);
        assert_eq!(
            requests.iter().collect::<HashSet<_>>().len(),
            MAX_STORYBOARDS
        );
        // A failed decode leaves no cache entry and can be requested again.
        assert_eq!(
            storyboard_requests(&keys, &mut cache, &mut recent),
            requests
        );
        cache.insert(keys[0].0.clone(), Arc::new(Vec::new()));
        assert!(storyboard_requests(&[], &mut cache, &mut recent).is_empty());
        assert!(cache.is_empty());
        assert!(recent.is_empty());
    }

    #[test]
    fn import_summary_names_each_failed_file() {
        let errors = vec![
            (PathBuf::from("/media/notes.txt"), "No media streams".into()),
            (
                PathBuf::from("/media/broken.mp4"),
                "Cannot probe source".into(),
            ),
        ];
        assert_eq!(
            import_summary(1, 3, &errors),
            "2 of 3 files couldn't be imported:\nnotes.txt — No media streams\nbroken.mp4 — Cannot probe source"
        );
        assert_eq!(
            import_summary(0, 1, &errors[..1]),
            "1 of 1 files couldn't be imported:\nnotes.txt — No media streams"
        );
    }

    #[test]
    fn import_summary_handles_success_and_empty_batches() {
        assert_eq!(import_summary(3, 3, &[]), "3 of 3 files imported");
        assert_eq!(import_summary(0, 0, &[]), "0 of 0 files imported");
    }

    #[test]
    fn import_summary_bounds_long_failure_lists() {
        let errors: Vec<_> = (0..12)
            .map(|index| {
                (
                    PathBuf::from(format!("clip-{index}.mp4")),
                    "Invalid data".into(),
                )
            })
            .collect();
        let summary = import_summary(1, 13, &errors);
        assert!(summary.contains("clip-7.mp4 — Invalid data"));
        assert!(!summary.contains("clip-8.mp4"));
        assert!(summary.ends_with("and 4 more"));
    }

    #[test]
    fn quick_exports_get_new_numbered_names_without_file_access() {
        let first = Path::new("/tmp/video.webm");
        assert_eq!(next_export_path(first), Path::new("/tmp/video-2.webm"));
        assert_eq!(
            next_export_path(&next_export_path(first)),
            Path::new("/tmp/video-3.webm")
        );
    }

    #[test]
    fn quick_export_skips_existing_files_in_background() {
        let dir = std::env::temp_dir().join(format!(
            "ocus-quick-path-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&dir).unwrap();
        let first = dir.join("video-2.webm");
        let second = dir.join("video-3.webm");
        std::fs::write(&first, b"first").unwrap();
        std::fs::write(&second, b"second").unwrap();
        assert_eq!(unused_export_path(first.clone()), dir.join("video-4.webm"));
        assert_eq!(std::fs::read(&first).unwrap(), b"first");
        assert_eq!(std::fs::read(&second).unwrap(), b"second");
        std::fs::remove_file(first).unwrap();
        std::fs::remove_file(second).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }

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
    fn unsaved_dialog_buttons_map_to_save_discard_or_cancel() {
        for (result, choice) in [
            (
                MessageDialogResult::Custom(SAVE_CHOICE.into()),
                UnsavedChoice::Save,
            ),
            (
                MessageDialogResult::Custom(DISCARD_CHOICE.into()),
                UnsavedChoice::Discard,
            ),
            (
                MessageDialogResult::Custom(CANCEL_CHOICE.into()),
                UnsavedChoice::Cancel,
            ),
            (MessageDialogResult::Yes, UnsavedChoice::Save),
            (MessageDialogResult::No, UnsavedChoice::Discard),
            (MessageDialogResult::Cancel, UnsavedChoice::Cancel),
        ] {
            assert_eq!(unsaved_choice(result), choice);
        }
    }

    fn dirty_state() -> (UiState, Rc<Cell<u32>>) {
        let state = UiState::new();
        state.project_path.set(Some("cut.ocus".into()));
        state.dirty.set(true);
        (state, Rc::new(Cell::new(0)))
    }

    fn counting(runs: &Rc<Cell<u32>>) -> impl FnOnce(&UiState) + 'static {
        let runs = runs.clone();
        move |_| runs.set(runs.get() + 1)
    }

    #[test]
    fn clean_projects_continue_without_asking() {
        let state = UiState::new();
        let runs = Rc::new(Cell::new(0));
        assert!(state.settle_unsaved_changes(counting(&runs)));
        assert_eq!(runs.get(), 0);
    }

    #[test]
    fn discard_continues_now_and_cancel_keeps_the_changes() {
        let (state, runs) = dirty_state();
        assert!(state.apply_unsaved_choice(UnsavedChoice::Discard, counting(&runs)));
        assert!(!state.apply_unsaved_choice(UnsavedChoice::Cancel, counting(&runs)));
        assert!(state.dirty.peek());
        assert_eq!(runs.get(), 0);
    }

    #[test]
    fn save_choice_waits_for_the_save_before_continuing() {
        let (state, runs) = dirty_state();
        assert!(!state.apply_unsaved_choice(UnsavedChoice::Save, counting(&runs)));
        assert!(state.dirty.peek());
        assert_eq!(runs.get(), 0);
    }

    #[test]
    fn only_a_successful_save_of_the_current_edit_continues() {
        let (state, runs) = dirty_state();
        let path = PathBuf::from("cut.ocus");
        let failure = std::io::Error::other("disk full");
        state.finish_save(
            path.clone(),
            0,
            None,
            Some(Box::new(counting(&runs))),
            Err(failure),
        );
        assert_eq!(runs.get(), 0);
        assert!(state.dirty.peek());
        assert_eq!(state.status.peek(), "Save failed: disk full");

        state.revision.set(1);
        state.finish_save(
            path.clone(),
            0,
            None,
            Some(Box::new(counting(&runs))),
            Ok(()),
        );
        assert_eq!(runs.get(), 0);
        assert!(state.dirty.peek());

        state.finish_save(path, 1, None, Some(Box::new(counting(&runs))), Ok(()));
        assert_eq!(runs.get(), 1);
        assert!(!state.dirty.peek());
        assert_eq!(state.status.peek(), "Project saved");
    }

    #[test]
    fn saving_before_new_project_resumes_new_project() {
        let (state, _) = dirty_state();
        state.focus_frame(600);
        state.finish_save(
            "cut.ocus".into(),
            state.revision.peek(),
            None,
            Some(Box::new(UiState::new_project)),
            Ok(()),
        );
        assert_eq!(state.project_path.peek(), None);
        assert_eq!(state.timeline_start.peek(), 0);
        assert_eq!(state.status.peek(), "New project · Not saved");
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
        state.scroll_timeline_frames(150);
        assert_eq!(state.timeline_start.peek(), 1_050);
        state.focus_frame(1_200);
        state.zoom_timeline(0);
        assert_eq!(state.visible_frames(), 30);
        assert_eq!(state.timeline_start.peek(), 1_200 - 150 / 10);
        state.zoom_timeline_around(3, 300, 0.5);
        assert_eq!(state.visible_frames(), 150);
        assert_eq!(state.timeline_start.peek(), 225);
        state.zoom_timeline(99);
        assert_eq!(state.visible_frames(), 18_000);
        state.zoom_to_fit();
        assert_eq!(ZOOM_SECONDS[state.timeline_zoom.peek()], 90);
        assert_eq!(state.timeline_start.peek(), 0);
        state.scroll_timeline_frames(-50);
        assert_eq!(state.timeline_start.peek(), 0);
    }
}
