use super::edits::{self, ClipRef, CopiedClip};
use super::interaction::{DragState, Gesture, MenuState};
use crate::export::{
    self, presets,
    queue::{Queue, Request, next_export_path},
};
use crate::media::audio::{self, AudioPlan, AudioSource, Clock, Waveform, carries_sound};
use crate::media::cache::{storyboard, thumbnail};
use crate::media::composite::VisualPlan;
use crate::media::preview::{
    FrameImage, FrameSource, PreviewEvent, PreviewPlan, StoryboardKey, decode_sequence,
};
use crate::media::{Media, MediaKind, MediaProbe, Thumbnail, probe};
use crate::playback::{self, Session};
use crate::project::export_settings::{
    AudioCodec, Container, EncodingSpeed, ExportSettings, LastExport, VideoCodec,
};
use crate::project::interchange::{self, Format, Imported};
use crate::project::{Command, Editor, FrameRate, Project, VideoSettings, recovery, store};
use crate::settings::{self, Settings, ThemeMode};
use crate::timeline::{Clip, ClipContent, Playback, Title, TrackKind, Transform};
use creamui::core::{Modifiers, Point, Rect, RgbaImage};
use creamui::widgets::{
    ColorPickerController, ScrollController, TextController, TooltipController,
};
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
    Performance,
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

enum TimelineImport {
    ChooseEdlRate { drop: bool },
    Ready(Box<Imported>),
}

enum ExportEvent {
    Destination(PathBuf),
    Finished(LastExport, Result<(), String>),
}

const SAVE_CHOICE: &str = "Save";
const DISCARD_CHOICE: &str = "Don't Save";
const CANCEL_CHOICE: &str = "Cancel";
const MAX_THUMBNAILS: usize = 256;

#[derive(Clone, Default)]
struct Waveforms {
    peaks: Rc<RefCell<HashMap<PathBuf, Option<Arc<Waveform>>>>>,
    pending: Rc<RefCell<Option<Arc<AtomicBool>>>>,
}

fn missing_sources(project: &Project) -> HashSet<u64> {
    project
        .media()
        .iter()
        .filter(|media| !media.path.is_file())
        .map(|media| media.id)
        .collect()
}

fn folder_matches(
    sources: &[Media],
    folder: &Path,
    files: &HashSet<PathBuf>,
) -> Vec<(Media, PathBuf)> {
    sources
        .iter()
        .filter_map(|source| {
            let candidate = folder.join(source.path.file_name()?);
            files
                .contains(&candidate)
                .then(|| (source.clone(), candidate))
        })
        .collect()
}
const MAX_STORYBOARDS: usize = 64;
/// Seconds shown across the timeline at each zoom level, closest first.
pub(crate) const ZOOM_SECONDS: [u64; 16] = [
    1, 2, 3, 5, 8, 10, 15, 20, 30, 45, 60, 90, 120, 180, 300, 600,
];
const DEFAULT_ZOOM: usize = 5;

/// An empty cached filmstrip marks a preview that could not be generated.
pub(crate) type Storyboard = Arc<Vec<Arc<RgbaImage>>>;

pub(super) fn clip_storyboard_key(
    clip: &Clip,
    media: &Media,
    fps: (u32, u32),
) -> Option<StoryboardKey> {
    let first = clip.source_frame_at(0)?;
    let last = clip.source_frame_at(clip.duration_frames.checked_sub(1)?)?;
    Some(StoryboardKey {
        transform_bits: StoryboardKey::transform_bits(clip.transform),
        color_bits: [
            clip.color.brightness,
            clip.color.contrast,
            clip.color.saturation,
        ]
        .map(f32::to_bits),
        effect: clip
            .effect
            .as_ref()
            .and_then(|value| serde_json::to_string(value).ok()),
        mask: clip
            .mask
            .as_deref()
            .copied()
            .map(crate::timeline::Mask::bits),
        lut: clip.lut.clone(),
        chroma_key: clip.chroma_key,
        playback: clip.playback,
        path: media.path.clone(),
        kind: media.kind,
        source_start: first.min(last),
        duration: first.abs_diff(last).saturating_add(1),
        fps,
    })
}

type InspectedSource = Result<ImportedSource, String>;

enum ImportEvent {
    Progress(usize, PathBuf),
    Complete(Vec<(PathBuf, InspectedSource)>),
}

fn storyboard_requests(
    keys: &[(StoryboardKey, bool)],
    cache: &mut HashMap<StoryboardKey, Storyboard>,
    recent: &mut Vec<StoryboardKey>,
) -> Vec<StoryboardKey> {
    let live: HashSet<_> = keys.iter().map(|(key, _)| key).collect();
    recent.retain(|key| live.contains(key) && cache.contains_key(key));
    let visible: HashSet<_> = keys
        .iter()
        .filter(|(_, visible)| *visible)
        .map(|(key, _)| key)
        .collect();
    // On-screen ranges may overflow the cache; off-screen retention never does.
    let capacity = MAX_STORYBOARDS.max(visible.len());
    let mut selected = Vec::new();
    let mut seen = HashSet::new();
    for key in keys
        .iter()
        .map(|(key, _)| key)
        .filter(|key| visible.contains(*key))
        .chain(recent.iter().rev())
        .chain(keys.iter().map(|(key, _)| key))
    {
        if seen.insert(key.clone()) {
            selected.push(key.clone());
            if selected.len() == capacity {
                break;
            }
        }
    }
    cache.retain(|key, _| seen.contains(key));
    recent.retain(|key| seen.contains(key) && !visible.contains(key));
    for key in selected
        .iter()
        .filter(|key| !visible.contains(*key) && !cache.contains_key(*key))
    {
        recent.insert(0, key.clone());
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

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PlaybackMonitor {
    Program,
    Source,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum PreviewState {
    Ready,
    Loading,
    Failed(String),
}

/// One GPU preview surface with its own position and decode stream.
#[derive(Clone)]
pub(crate) struct Monitor {
    pub slot: GpuFrameSlot,
    pub playing: Signal<bool>,
    pub position: Signal<u64>,
    pub(super) preview: Signal<PreviewState>,
    failure_status: Rc<RefCell<Option<String>>>,
    cancel: Rc<RefCell<Option<Arc<AtomicBool>>>>,
    serial: Rc<Cell<u64>>,
    audio: Rc<RefCell<Option<Session>>>,
}

impl Monitor {
    fn begin_preview(&self, visual: bool) {
        self.preview.set(if visual {
            PreviewState::Loading
        } else {
            PreviewState::Ready
        });
    }

    fn frame_ready(&self) {
        self.preview.set(PreviewState::Ready);
    }

    fn new() -> Self {
        Self {
            slot: GpuFrameSlot::new(),
            playing: Signal::new(false),
            position: Signal::new(0),
            preview: Signal::new(PreviewState::Ready),
            failure_status: Rc::new(RefCell::new(None)),
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
    pub playback: crate::timeline::Playback,
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

#[derive(Clone, Copy)]
pub(crate) struct ChromaGesture {
    pub target: ClipRef,
    pub property: usize,
    pub rect: creamui::core::Rect,
    pub key: crate::timeline::ChromaKey,
}

pub(super) struct TooltipState {
    pub controller: TooltipController,
    _effect: creamui::reactive::Effect,
}

#[derive(Clone)]
pub(crate) struct UiState {
    autosaver: Option<Rc<recovery::Autosaver>>,
    recovery_dir: Option<PathBuf>,
    pub editor: Rc<RefCell<Editor>>,
    pub revision: Signal<u64>,
    pub ui_revision: Signal<u64>,
    pub(super) tooltips: Rc<RefCell<HashMap<(&'static str, u64), TooltipState>>>,
    pub tooltip_viewport: Rc<Cell<creamui::Size>>,
    tooltip_keys: Rc<RefCell<HashSet<(&'static str, u64)>>>,
    pub selected: Signal<Vec<ClipRef>>,
    pub selected_media: Signal<Option<u64>>,
    pub missing_media: Signal<HashSet<u64>>,
    pub program: Monitor,
    pub source: Monitor,
    last_playback_monitor: Rc<Cell<PlaybackMonitor>>,
    pub preview_available: Signal<bool>,
    pub project_path: Signal<Option<PathBuf>>,
    pub edl_rate_prompt: Signal<Option<(PathBuf, bool)>>,
    pub title_text: TextController,
    pub rename_track: Signal<Option<u64>>,
    pub track_name: TextController,
    pub title_duration: TextController,
    title_duration_synced: Rc<RefCell<String>>,
    pub title_font: TextController,
    pub title_size: TextController,
    pub title_color: TextController,
    pub title_background: TextController,
    pub title_picker: ColorPickerController,
    pub title_preview: Signal<Option<(ClipRef, Title)>>,
    pub chroma_preview: Signal<Option<(ClipRef, crate::timeline::ChromaKey)>>,
    pub chroma_gesture: Rc<RefCell<Option<ChromaGesture>>>,
    pub title_gesture: Rc<RefCell<Option<TitleGesture>>>,
    pub effect_drag: Signal<Option<(ClipRef, f32)>>,
    pub effect_select: creamui::widgets::SelectController,
    pub media_query: TextController,
    pub media_filter: Signal<MediaFilter>,
    pub media_view: Signal<MediaView>,
    pub status: Signal<String>,
    pub autosave_error: Signal<Option<String>>,
    pub dirty: Signal<bool>,
    pub busy: Signal<bool>,
    pub export_queue: Signal<Queue>,
    pub export_queue_modal: Signal<bool>,
    pub export_queue_scroll: ScrollController,
    pub export_modal: Signal<bool>,
    pub export_error: Signal<Option<String>>,
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
    pub thumbnail_loading: Signal<HashSet<PathBuf>>,
    pub storyboards: Rc<RefCell<HashMap<StoryboardKey, Storyboard>>>,
    waveforms: Waveforms,
    pub proxy_status: Signal<HashMap<PathBuf, String>>,
    proxy_pending: Rc<RefCell<Option<Arc<AtomicBool>>>>,
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
    pub stabilization_drag: Signal<Option<(u64, u64, crate::timeline::Stabilization)>>,
    pub stabilization_analyzing: Signal<bool>,
    pub mask_drag: Signal<Option<(u64, u64, usize, crate::timeline::Mask)>>,
    pub trim_hover: Signal<bool>,
    pub marquee: Signal<Option<Rect>>,
    pub timeline_rect: Rc<Cell<Rect>>,
    pub modifiers: Rc<Cell<Modifiers>>,
    pub clipboard: Rc<RefCell<Vec<CopiedClip>>>,
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
    export_directory: Signal<PathBuf>,
    storyboard_cancel: Rc<RefCell<Option<Arc<AtomicBool>>>>,
    storyboard_serial: Rc<Cell<u64>>,
    storyboard_recent: Rc<RefCell<Vec<StoryboardKey>>>,
    audio_output: Rc<RefCell<Option<playback::Output>>>,
    audio_failed: Rc<Cell<bool>>,
    pub(super) transcription_cancel: Rc<RefCell<Option<Arc<AtomicBool>>>>,
}

impl UiState {
    pub(super) fn begin_tooltips(&self) {
        self.tooltip_keys.borrow_mut().clear();
    }

    pub(super) fn finish_tooltips(&self) {
        let keys = self.tooltip_keys.borrow();
        self.tooltips
            .borrow_mut()
            .retain(|key, _| keys.contains(key));
    }

    pub(super) fn tooltip_controller(&self, key: (&'static str, u64)) -> TooltipController {
        self.tooltip_keys.borrow_mut().insert(key);
        if let Some(tooltip) = self.tooltips.borrow().get(&key) {
            return tooltip.controller.clone();
        }
        let controller = TooltipController::default();
        let weak_controllers = Rc::downgrade(&self.tooltips);
        let active = controller.clone();
        let effect = creamui::reactive::create_effect(move || {
            if active.visible() {
                let Some(controllers) = weak_controllers.upgrade() else {
                    return;
                };
                let others: Vec<_> = controllers
                    .borrow()
                    .iter()
                    .filter(|(other, _)| **other != key)
                    .map(|(_, tooltip)| tooltip.controller.clone())
                    .collect();
                creamui::reactive::batch(|| {
                    for controller in others {
                        controller.hover(false);
                        controller.focus(false);
                    }
                });
            }
        });
        self.tooltips.borrow_mut().insert(
            key,
            TooltipState {
                controller: controller.clone(),
                _effect: effect,
            },
        );
        controller
    }

    pub fn new() -> Self {
        Self::with_recovery_dir(if cfg!(test) {
            None
        } else {
            recovery::directory()
        })
    }

    fn with_recovery_dir(recovery_dir: Option<PathBuf>) -> Self {
        let settings = settings::path()
            .and_then(|path| settings::load(&path).ok())
            .unwrap_or_default();
        crate::media::hardware::set_enabled(settings.hardware_acceleration);
        let export_directory =
            settings::export_directory(settings.last_export_directory.as_deref());
        Self {
            autosaver: recovery_dir
                .clone()
                .map(recovery::Autosaver::new)
                .map(Rc::new),
            recovery_dir,
            editor: Rc::new(RefCell::new(Editor::new(Project::new("Untitled")))),
            revision: Signal::new(0),
            ui_revision: Signal::new(0),
            tooltips: Rc::new(RefCell::new(HashMap::new())),
            tooltip_viewport: Rc::new(Cell::new(creamui::Size::default())),
            tooltip_keys: Rc::new(RefCell::new(HashSet::new())),
            selected: Signal::new(Vec::new()),
            selected_media: Signal::new(None),
            missing_media: Signal::new(HashSet::new()),
            program: Monitor::new(),
            source: Monitor::new(),
            last_playback_monitor: Rc::new(Cell::new(PlaybackMonitor::Program)),
            preview_available: Signal::new(false),
            project_path: Signal::new(None),
            edl_rate_prompt: Signal::new(None),
            title_text: TextController::default(),
            rename_track: Signal::new(None),
            track_name: TextController::default(),
            title_duration: TextController::default(),
            title_duration_synced: Rc::new(RefCell::new(String::new())),
            title_font: TextController::default(),
            title_size: TextController::default(),
            title_color: TextController::default(),
            title_background: TextController::default(),
            title_picker: ColorPickerController::new(),
            title_preview: Signal::new(None),
            chroma_preview: Signal::new(None),
            chroma_gesture: Rc::new(RefCell::new(None)),
            title_gesture: Rc::new(RefCell::new(None)),
            effect_drag: Signal::new(None),
            effect_select: creamui::widgets::SelectController::new(0),
            media_query: TextController::default(),
            media_filter: Signal::new(MediaFilter::All),
            media_view: Signal::new(MediaView::Grid),
            status: Signal::new("Ready · Project not saved".into()),
            autosave_error: Signal::new(None),
            dirty: Signal::new(false),
            busy: Signal::new(false),
            export_queue: Signal::new(Queue::default()),
            export_queue_modal: Signal::new(false),
            export_queue_scroll: ScrollController::default(),
            export_modal: Signal::new(false),
            export_error: Signal::new(None),
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
            thumbnail_loading: Signal::new(HashSet::new()),
            storyboards: Rc::new(RefCell::new(HashMap::new())),
            waveforms: Waveforms::default(),
            proxy_status: Signal::new(HashMap::new()),
            proxy_pending: Rc::new(RefCell::new(None)),
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
            stabilization_drag: Signal::new(None),
            stabilization_analyzing: Signal::new(false),
            mask_drag: Signal::new(None),
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
            export_directory: Signal::new(export_directory),
            storyboard_cancel: Rc::new(RefCell::new(None)),
            storyboard_serial: Rc::new(Cell::new(0)),
            storyboard_recent: Rc::new(RefCell::new(Vec::new())),
            audio_output: Rc::new(RefCell::new(None)),
            audio_failed: Rc::new(Cell::new(false)),
            transcription_cancel: Rc::new(RefCell::new(None)),
        }
    }

    pub fn connect(&self, app: AppHandle) {
        *self.app.borrow_mut() = Some(app.clone());
        if let Some(results) = self
            .autosaver
            .as_ref()
            .and_then(|saver| saver.take_results())
        {
            let state = self.clone();
            app.spawn_stream(
                move |emit| {
                    while let Ok(result) = results.recv() {
                        emit(result);
                    }
                },
                move |result| state.finish_autosave(result),
            );
        }
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

    fn finish_autosave(&self, result: Result<(), String>) {
        self.autosave_error.set(
            result
                .err()
                .map(|error| format!("Autosave failed: {error}")),
        );
    }

    pub(super) fn show_file_error(&self, title: &'static str, reason: String) {
        let Some(app) = self.app.borrow().clone() else {
            return;
        };
        app.spawn_background(
            move || {
                MessageDialog::new()
                    .set_level(MessageLevel::Error)
                    .set_title(title)
                    .set_description(reason)
                    .set_buttons(MessageButtons::Ok)
                    .show()
            },
            |_| {},
        );
    }

    pub fn connect_window(&self, window: WindowHandle) {
        self.preview_available.set(window.gpu_device().is_some());
        self.install_input(&window);
        *self.window.borrow_mut() = Some(window);
        self.request_preview();
        self.request_source_preview();
        self.offer_recovery();
    }

    fn offer_recovery(&self) {
        let (Some(app), Some(dir)) = (self.app.borrow().clone(), self.recovery_dir.clone()) else {
            return;
        };
        let revision = self.revision.peek();
        let state = self.clone();
        app.spawn_background(
            move || {
                recovery::read_with_repairs(&dir).map(|snapshot| {
                    snapshot.map(|(snapshot, repaired)| {
                        let missing = missing_sources(&snapshot.project);
                        (snapshot, missing, repaired)
                    })
                })
            },
            move |result| match result {
                Ok(snapshot) => state.finish_recovery(revision, snapshot),
                Err(reason) => {
                    state.status.set(format!("Recovery failed: {reason}"));
                    if let Some(app) = state.app.borrow().clone() {
                        let state = state.clone();
                        app.spawn_background(
                            move || {
                                MessageDialog::new()
                                    .set_level(MessageLevel::Warning)
                                    .set_title("Could not recover unsaved work")
                                    .set_description(format!(
                                        "{reason}\nThe recovery file will be discarded."
                                    ))
                                    .set_buttons(MessageButtons::Ok)
                                    .show()
                            },
                            move |_| {
                                if !state.busy.peek() && state.revision.peek() == revision {
                                    state.clear_recovery();
                                }
                            },
                        );
                    }
                }
            },
        );
    }

    fn finish_recovery(
        &self,
        revision: u64,
        result: Option<(recovery::Snapshot, HashSet<u64>, usize)>,
    ) {
        if self.busy.peek() || self.revision.peek() != revision {
            return;
        }
        if let Some((snapshot, missing, repaired)) = result {
            let result = MessageDialog::new()
                .set_level(MessageLevel::Warning)
                .set_title("Recover unsaved work?")
                .set_description(format!(
                    "Ocus closed before your changes to “{}” were saved. Restore them now?",
                    snapshot.project.name()
                ))
                .set_buttons(MessageButtons::OkCancelCustom(
                    "Restore".into(),
                    "Discard".into(),
                ))
                .show();
            let restore = matches!(result, MessageDialogResult::Ok)
                || result == MessageDialogResult::Custom("Restore".into());
            if restore {
                self.install_project(snapshot.project, snapshot.original_path, true, missing);
                self.report_repairs(repaired);
            } else {
                self.clear_recovery();
            }
        }
    }

    fn clear_recovery(&self) {
        if let Some(saver) = &self.autosaver {
            saver.clear();
        }
    }

    fn update_recovery(&self) {
        if let Some(saver) = &self.autosaver {
            let editor = self.editor.borrow();
            if editor.is_dirty() {
                saver.write(recovery::Snapshot {
                    project: editor.project().clone(),
                    original_path: self.project_path.peek(),
                });
            } else {
                saver.clear();
            }
        }
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
        crate::media::hardware::set_enabled(settings.hardware_acceleration);
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

    pub fn toggle_proxies(&self) {
        let mut settings = self.settings.peek();
        settings.use_proxies = !settings.use_proxies;
        self.update_settings(settings);
        self.cancel_storyboards();
        self.storyboards.borrow_mut().clear();
        self.queue_storyboards();
        self.request_preview();
        self.request_source_preview();
    }

    pub fn toggle_hardware_acceleration(&self) {
        let mut settings = self.settings.peek();
        settings.hardware_acceleration = !settings.hardware_acceleration;
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
        if self.export_modal.peek() || self.export_queue_modal.peek() {
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
        self.effect_drag.set(None);
        {
            let editor = self.editor.borrow();
            self.clipboard
                .borrow_mut()
                .retain(|copied| match copied.clip.content {
                    ClipContent::Media { media_id } => editor
                        .project()
                        .media()
                        .iter()
                        .any(|media| media.id == media_id),
                    ClipContent::Title(_) => true,
                });
        }
        let existing: HashSet<_> = self
            .editor
            .borrow()
            .project()
            .tracks()
            .iter()
            .filter(|track| !track.locked)
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
        self.sync_title_duration(false);
        self.revision.update(|revision| *revision += 1);
        let dirty = self.editor.borrow().is_dirty();
        self.dirty.set(dirty);
        self.update_recovery();
        self.refresh_missing_media();
        let checkpoint = if dirty {
            "Unsaved changes"
        } else if self.project_path.peek().is_some() {
            "Project saved"
        } else {
            "No changes · Project not saved"
        };
        self.status.set(if status.is_empty() {
            checkpoint.into()
        } else {
            format!("{status} · {checkpoint}")
        });
    }

    pub(super) fn has_timeline_audio(&self) -> bool {
        !audio::AudioPlan::from_project(self.editor.borrow().project())
            .sources
            .is_empty()
    }

    pub(super) fn generate_subtitles(&self) {
        if let Some(cancel) = self.transcription_cancel.borrow().as_ref() {
            cancel.store(true, Ordering::Relaxed);
            self.status.set("Cancelling subtitle generation…".into());
            return;
        }
        if self.busy.peek() || !self.has_timeline_audio() {
            return;
        }
        let Some(app) = self.app.borrow().clone() else {
            return;
        };
        let plan = audio::AudioPlan::from_project(self.editor.borrow().project());
        let revision = self.revision.peek();
        let path = match crate::media::transcribe::model_path() {
            Ok(path) => path,
            Err(error) => {
                self.finish_transcription(revision, Err(error));
                return;
            }
        };
        let cancel = Arc::new(AtomicBool::new(false));
        *self.transcription_cancel.borrow_mut() = Some(cancel.clone());
        self.busy.set(true);
        self.status.set("Checking speech model…".into());
        let state = self.clone();
        let check_path = path.clone();
        let check_cancel = cancel.clone();
        app.spawn_background(
            move || crate::media::transcribe::check_model(&check_path, &check_cancel),
            move |result| {
                let Some(download) = state.finish_model_check(revision, result, || {
                    MessageDialog::new()
                            .set_title("Generate subtitles")
                            .set_description("Download speech model (~142 MB)? It runs locally on your CPU. Your audio never leaves this computer.")
                            .set_buttons(MessageButtons::OkCancelCustom("Download".into(), "Cancel".into()))
                            .show() == MessageDialogResult::Custom("Download".into())
                }) else { return };
                state.start_transcription(revision, plan, path, cancel, download);
            },
        );
    }

    fn finish_model_check(
        &self,
        revision: u64,
        result: Result<bool, String>,
        consent: impl FnOnce() -> bool,
    ) -> Option<bool> {
        if self.revision.peek() != revision
            || self
                .transcription_cancel
                .borrow()
                .as_ref()
                .is_some_and(|cancel| cancel.load(Ordering::Relaxed))
        {
            self.finish_transcription(revision, Err("Cancelled".into()));
            return None;
        }
        match result {
            Ok(true) => Some(false),
            Ok(false) if consent() => Some(true),
            Ok(false) => {
                self.finish_transcription(revision, Err("Cancelled".into()));
                None
            }
            Err(error) => {
                self.finish_transcription(revision, Err(error));
                None
            }
        }
    }

    fn start_transcription(
        &self,
        revision: u64,
        plan: audio::AudioPlan,
        path: PathBuf,
        cancel: Arc<AtomicBool>,
        download: bool,
    ) {
        let Some(app) = self.app.borrow().clone() else {
            self.finish_transcription(revision, Err("Window closed".into()));
            return;
        };
        let state = self.clone();
        app.spawn_stream(
            move |emit| {
                let result = (|| {
                    if download {
                        crate::media::transcribe::download_model(&path, &cancel, |message| {
                            emit(Err(message))
                        })?;
                    }
                    emit(Err("Analyzing speech…".into()));
                    crate::media::transcribe::transcribe(&plan, &path, cancel)
                })();
                emit(Ok(result));
            },
            move |event| match event {
                Err(progress) => state.status.set(progress),
                Ok(result) => state.finish_transcription(revision, result),
            },
        );
    }

    fn finish_transcription(
        &self,
        revision: u64,
        result: Result<Vec<crate::subtitles::Cue>, String>,
    ) {
        let cancelled = self
            .transcription_cancel
            .borrow_mut()
            .take()
            .is_some_and(|cancel| cancel.load(Ordering::Relaxed));
        self.busy.set(false);
        if self.revision.peek() != revision {
            self.status
                .set("Subtitle generation cancelled: project changed".into());
        } else if cancelled {
            self.status.set("Subtitle generation cancelled".into());
        } else {
            match result {
                Ok(cues) if cues.is_empty() => self.status.set("No speech found".into()),
                Ok(cues) => match self.import_subtitle_cues(&cues) {
                    Ok(count) => {
                        let noun = if count == 1 { "cue" } else { "cues" };
                        self.status
                            .set(format!("Generated {count} subtitle {noun}"));
                    }
                    Err(error) => self
                        .status
                        .set(format!("Cannot generate subtitles: {error}")),
                },
                Err(error) if error == "Cancelled" => {
                    self.status.set("Subtitle generation cancelled".into())
                }
                Err(error) => self
                    .status
                    .set(format!("Cannot generate subtitles: {error}")),
            }
        }
    }

    pub(super) fn remove_silences(&self) {
        if self.busy.peek() {
            return;
        }
        let Some(app) = self.app.borrow().clone() else {
            return;
        };
        let editor = self.editor.borrow();
        let project = editor.project();
        let mut jobs = Vec::new();
        for (track_id, clip_id) in self.selected.peek() {
            let Some(track) = project
                .tracks()
                .iter()
                .find(|track| track.id == track_id && !track.locked)
            else {
                continue;
            };
            let Some(clip) = track.clips.iter().find(|clip| clip.id == clip_id) else {
                continue;
            };
            let ClipContent::Media { media_id } = clip.content else {
                continue;
            };
            let Some(media) = project.media().iter().find(|media| media.id == media_id) else {
                continue;
            };
            if carries_sound(track.kind, clip, media)
                && clip.playback != crate::timeline::Playback::Freeze
            {
                jobs.push((media.path.clone(), clip.clone()));
            }
        }
        if jobs.is_empty() {
            return;
        }
        let fps = self.frame_rate();
        let revision = self.revision.peek();
        drop(editor);
        self.busy.set(true);
        self.status.set("Analyzing silences…".into());
        let state = self.clone();
        app.spawn_background(
            move || {
                let mut ranges = Vec::new();
                let rate = f64::from(fps.0) / f64::from(fps.1);
                for (path, clip) in jobs {
                    for (start, end) in
                        audio::silences(&path, clip.source_start_frame, clip.source_span(), fps)?
                    {
                        let (start, end) = if clip.playback == crate::timeline::Playback::Reverse {
                            (
                                clip.duration_frames as f64 - end * rate / clip.speed,
                                clip.duration_frames as f64 - start * rate / clip.speed,
                            )
                        } else {
                            (start * rate / clip.speed, end * rate / clip.speed)
                        };
                        let start = start.ceil().max(0.0) as u64;
                        let end = (end.floor().max(0.0) as u64).min(clip.duration_frames);
                        if start < end {
                            ranges.push((clip.start_frame + start, clip.start_frame + end));
                        }
                    }
                }
                Ok(ranges)
            },
            move |result| state.finish_silence_removal(revision, result),
        );
    }

    fn finish_silence_removal(&self, revision: u64, result: Result<Vec<(u64, u64)>, String>) {
        self.busy.set(false);
        if self.revision.peek() != revision {
            self.status
                .set("Silence removal cancelled: project changed".into());
            return;
        }
        match result {
            Ok(mut ranges) => {
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
                if merged.is_empty() {
                    self.status.set("No silence found".into());
                } else {
                    let count = merged.len();
                    let frames: u64 = merged.iter().map(|(start, end)| end - start).sum();
                    let fps = self.frame_rate();
                    if self.edit(Command::RemoveSilences(merged)) {
                        let part = if count == 1 { "part" } else { "parts" };
                        self.status.set(format!(
                            "Removed {count} silent {part} ({:.1} s)",
                            frames as f64 * f64::from(fps.1) / f64::from(fps.0)
                        ));
                    }
                }
            }
            Err(error) => self.status.set(format!("Cannot remove silences: {error}")),
        }
    }

    pub(super) fn normalize_loudness(&self) {
        let Some(app) = self.app.borrow().clone() else {
            return;
        };
        let Some((track_id, clip_id)) = self.primary_clip() else {
            return;
        };
        let editor = self.editor.borrow();
        let project = editor.project();
        let Some(track) = project.tracks().iter().find(|track| track.id == track_id) else {
            return;
        };
        let Some(clip) = track.clips.iter().find(|clip| clip.id == clip_id).cloned() else {
            return;
        };
        let ClipContent::Media { media_id } = clip.content else {
            return;
        };
        let Some(media) = project.media().iter().find(|media| media.id == media_id) else {
            return;
        };
        if !carries_sound(track.kind, &clip, media) {
            return;
        }
        let path = media.path.clone();
        let fps = self.frame_rate();
        let revision = self.revision.peek();
        drop(editor);
        self.status.set("Analyzing clip loudness…".into());
        let state = self.clone();
        app.spawn_background(
            move || audio::loudness(&path, clip.source_start_frame, clip.source_span(), fps),
            move |result| state.finish_normalization(revision, track_id, clip_id, result),
        );
    }

    pub(super) fn stabilize_clip(&self) {
        if self.stabilization_analyzing.peek() {
            return;
        }
        let Some(app) = self.app.borrow().clone() else {
            return;
        };
        let Some((track_id, clip_id)) = self.primary_clip() else {
            return;
        };
        let editor = self.editor.borrow();
        let project = editor.project();
        let Some(track) = project
            .tracks()
            .iter()
            .find(|t| t.id == track_id && t.kind == TrackKind::Video && !t.locked)
        else {
            return;
        };
        let Some(clip) = track.clips.iter().find(|c| c.id == clip_id) else {
            return;
        };
        let ClipContent::Media { media_id } = clip.content else {
            return;
        };
        let Some(media) = project
            .media()
            .iter()
            .find(|m| m.id == media_id && m.kind == MediaKind::Video)
        else {
            return;
        };
        let source = crate::media::preview::FrameSource {
            chroma_key: None,
            stabilization: None,
            effect: None,
            mask: None,
            playback: crate::timeline::Playback::Forward,
            lut: None,
            color: Default::default(),
            clip_id,
            path: media.path.clone(),
            kind: media.kind,
            start: 0,
            source_start: clip.source_start_frame,
            duration: clip.source_span().max(1),
            speed: 1.0,
            transform: Default::default(),
            keyframes: Vec::new(),
        };
        let fps = self.frame_rate();
        let revision = self.revision.peek();
        drop(editor);
        self.status.set("Analyzing camera shake…".into());
        self.stabilization_analyzing.set(true);
        let state = self.clone();
        app.spawn_background(
            move || crate::media::stabilize::analyze(source, fps),
            move |result| {
                state.stabilization_analyzing.set(false);
                if state.revision.peek() != revision {
                    state.status.set(
                        "Stabilization discarded because the project changed. Try again.".into(),
                    );
                    return;
                }
                match result {
                    Ok(stabilization) => {
                        if state.busy.peek() {
                            state.status.set(
                                "Analysis finished, but the editor is busy. Try again.".into(),
                            );
                            return;
                        }
                        state.edit(Command::SetClipStabilization {
                            track_id,
                            clip_id,
                            stabilization: Some(stabilization),
                        });
                        state
                            .status
                            .set("Video stabilized. Adjust Strength to taste.".into());
                    }
                    Err(error) => state.status.set(format!("Cannot stabilize video: {error}")),
                }
            },
        );
    }

    fn finish_normalization(
        &self,
        revision: u64,
        track_id: u64,
        clip_id: u64,
        result: Result<audio::Loudness, String>,
    ) {
        if self.revision.peek() != revision {
            return;
        }
        let muted = self
            .editor
            .borrow()
            .project()
            .tracks()
            .iter()
            .find(|track| track.id == track_id)
            .and_then(|track| track.clips.iter().find(|clip| clip.id == clip_id))
            .map(|clip| clip.muted);
        let Some(muted) = muted else { return };
        match result {
            Ok(measurement) => {
                let Some((volume, clamped)) = measurement.gain() else {
                    self.status
                        .set("Clip is silent or below the loudness gate; volume unchanged".into());
                    return;
                };
                if self.edit(Command::SetClipAudio {
                    track_id,
                    clip_id,
                    volume,
                    muted,
                }) {
                    self.status.set(if clamped {
                        "Raised to the maximum volume (+6 dB)".into()
                    } else {
                        "Loudness normalized (−14 LUFS target, −1 dB peak limit)".into()
                    });
                }
            }
            Err(error) => self
                .status
                .set(format!("Cannot normalize loudness: {error}")),
        }
    }

    pub fn edit(&self, command: Command) -> bool {
        if self.busy.peek() {
            return false;
        }
        let changed = self.editor.borrow_mut().apply(command);
        if changed {
            self.refresh_after_edit("");
        } else {
            self.status
                .set("Edit rejected: check range, track and clip".into());
        }
        changed
    }

    pub fn begin_track_rename(&self, track_id: u64) {
        let name = self
            .editor
            .borrow()
            .project()
            .tracks()
            .iter()
            .find(|track| track.id == track_id)
            .map(|track| track.name.clone());
        if let Some(name) = name {
            self.track_name.set_value(name);
            self.rename_track.set(Some(track_id));
        }
    }

    pub fn apply_track_rename(&self) {
        if let Some(track_id) = self.rename_track.peek() {
            let name = self.track_name.value().trim().to_owned();
            if !crate::timeline::Track::valid_name(&name) {
                return;
            }
            self.edit(Command::RenameTrack { track_id, name });
            self.rename_track.set(None);
        }
    }

    fn refresh_missing_media(&self) {
        let Some(app) = self.app.borrow().clone() else {
            return;
        };
        let sources = self.editor.borrow().project().media().to_vec();
        let revision = self.revision.peek();
        let state = self.clone();
        app.spawn_background(
            move || {
                sources
                    .into_iter()
                    .filter(|media| !media.path.is_file())
                    .map(|media| media.id)
                    .collect::<HashSet<_>>()
            },
            move |missing| {
                if state.revision.peek() == revision {
                    state.missing_media.set(missing);
                    state.request_preview();
                    state.request_source_preview();
                    state.queue_storyboards();
                    state.queue_waveforms();
                }
            },
        );
    }

    pub fn undo(&self) {
        if self.busy.peek() {
            return;
        }
        if self.editor.borrow_mut().undo() {
            self.refresh_after_edit("Undid edit");
            self.sync_title_controllers();
        }
    }

    pub fn redo(&self) {
        if self.busy.peek() {
            return;
        }
        if self.editor.borrow_mut().redo() {
            self.refresh_after_edit("Redid edit");
            self.sync_title_controllers();
        }
    }

    pub fn frame_rate(&self) -> (u32, u32) {
        let editor = self.editor.borrow();
        let rate = &editor.project().settings().frame_rate;
        (rate.numerator, rate.denominator)
    }

    pub(super) fn frames_per_second_step(&self) -> u64 {
        let fps = self.frame_rate();
        (f64::from(fps.0) / f64::from(fps.1.max(1)))
            .round()
            .max(1.0) as u64
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

    /// Moves the Program playhead, pausing playback first.
    pub fn seek(&self, frame: u64) {
        self.last_playback_monitor.set(PlaybackMonitor::Program);
        if self.program.playing.peek() {
            self.pause();
        }
        if self.program.position.peek() != frame
            || matches!(self.program.preview.peek(), PreviewState::Failed(_))
        {
            self.focus_frame(frame);
        }
    }

    pub(super) fn toggle_last_playback_monitor(&self) {
        let target = if self.last_playback_monitor.get() == PlaybackMonitor::Source
            && (self.source.playing.peek()
                || self.source_range().is_some_and(|range| {
                    range.kind == MediaKind::Audio || self.preview_available.peek()
                })) {
            PlaybackMonitor::Source
        } else {
            PlaybackMonitor::Program
        };
        self.toggle_playback_monitor(target);
    }

    pub(super) fn toggle_playback_monitor(&self, target: PlaybackMonitor) {
        match target {
            PlaybackMonitor::Program if self.program.playing.peek() => {
                self.last_playback_monitor.set(target);
                self.pause();
            }
            PlaybackMonitor::Source if self.source.playing.peek() => {
                self.last_playback_monitor.set(target);
                self.pause_source();
            }
            PlaybackMonitor::Program => self.play(),
            PlaybackMonitor::Source => self.play_source(),
        }
    }

    pub fn play(&self) {
        let content = SequenceContent::of(self.editor.borrow().project());
        if content.has_picture && !self.preview_available.peek() {
            self.status
                .set("Video preview is not available on this display".into());
            return;
        }
        let end = self.sequence_end();
        if !content.playable || end == 0 {
            self.status
                .set("Add a clip to the timeline to play the sequence".into());
            return;
        }
        self.last_playback_monitor.set(PlaybackMonitor::Program);
        self.pause_source();
        self.audio_failed.set(false);
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
            if self.missing_media.peek().contains(&media_id) {
                return None;
            }
            let media = project.media().iter().find(|media| media.id == media_id)?;
            return Some(SourceRange {
                playback: clip.playback,
                path: media.path.clone(),
                kind: media.kind,
                source_start: clip.source_start_frame,
                duration: clip.source_span(),
            });
        }
        let media_id = self.selected_media.peek()?;
        if self.missing_media.peek().contains(&media_id) {
            return None;
        }
        let media = project.media().iter().find(|media| media.id == media_id)?;
        let rate = &project.settings().frame_rate;
        Some(SourceRange {
            playback: crate::timeline::Playback::Forward,
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
                ClipContent::Title(title) => Some(title.clone()),
                ClipContent::Media { .. } => None,
            });
        drop(editor);
        let Some(title) = title else {
            return;
        };
        self.title_text.set_value(title.text);
        self.title_font.set_value(title.font_family);
        self.title_size.set_value(title.font_size.to_string());
        self.title_color.set_value(format!(
            "#{:02X}{:02X}{:02X}{:02X}",
            title.color[0], title.color[1], title.color[2], title.color[3]
        ));
        let background = title.background.unwrap_or([0, 0, 0, 179]);
        self.title_background.set_value(format!(
            "#{:02X}{:02X}{:02X}{:02X}",
            background[0], background[1], background[2], background[3]
        ));
    }

    pub(super) fn sync_title_duration(&self, force: bool) {
        if !force && self.title_duration.peek() != *self.title_duration_synced.borrow() {
            return;
        }
        let Some((track_id, clip_id)) = self.primary_clip() else {
            return;
        };
        let editor = self.editor.borrow();
        let Some(clip) = editor
            .project()
            .tracks()
            .iter()
            .find(|track| track.id == track_id)
            .and_then(|track| track.clips.iter().find(|clip| clip.id == clip_id))
            .filter(|clip| matches!(clip.content, ClipContent::Title(_)))
        else {
            return;
        };
        let fps = &editor.project().settings().frame_rate;
        let value = (clip.duration_frames as f64 * f64::from(fps.denominator)
            / f64::from(fps.numerator))
        .to_string();
        drop(editor);
        *self.title_duration_synced.borrow_mut() = value.clone();
        if self.title_duration.peek() != value {
            self.title_duration.set_value(value);
        }
    }

    pub(super) fn cancel_mask_drag(&self) {
        if self.mask_drag.peek().is_some() {
            self.mask_drag.set(None);
            self.request_preview();
        }
    }

    pub(super) fn preview_chroma_drag(&self, pointer: Point) {
        let Some(gesture) = *self.chroma_gesture.borrow() else {
            return;
        };
        if self.busy.peek() || self.primary_clip() != Some(gesture.target) {
            return;
        }
        let draft = self.chroma_preview.peek();
        let mut key = draft
            .filter(|(clip, _)| *clip == gesture.target)
            .map_or(gesture.key, |(_, key)| key);
        let x = pointer.x - gesture.rect.x;
        let value = (((x - 6.0) / (gesture.rect.width - 12.0).max(1.0)).clamp(0.0, 1.0) * 100.0)
            .round()
            / 100.0;
        match gesture.property {
            0 => key.similarity = value,
            1 => key.smoothness = value,
            _ => key.spill = value,
        }
        if draft != Some((gesture.target, key)) {
            self.chroma_preview.set(Some((gesture.target, key)));
            self.request_preview();
        }
    }

    pub(super) fn clear_title_gesture(&self) {
        self.title_gesture.borrow_mut().take();
        self.chroma_gesture.borrow_mut().take();
        if self.chroma_preview.peek().is_some() {
            self.chroma_preview.set(None);
            self.request_preview();
        }
        if self.title_preview.peek().is_some() {
            self.title_preview.set(None);
            self.request_preview();
        }
    }

    /// Replaces the clip selection; the last entry becomes the primary clip.
    pub fn select_clips(&self, mut selection: Vec<ClipRef>) {
        self.effect_drag.set(None);
        selection.retain(|(track_id, clip_id)| {
            self.editor.borrow().project().tracks().iter().any(|track| {
                track.id == *track_id
                    && !track.locked
                    && track.clips.iter().any(|clip| clip.id == *clip_id)
            })
        });
        let mut seen = HashSet::new();
        selection.reverse();
        selection.retain(|clip| seen.insert(*clip));
        selection.reverse();
        let previous = self.primary_clip();
        if self.selected.peek() == selection {
            return;
        }
        self.clear_title_gesture();
        self.cancel_mask_drag();
        self.title_picker.set_open(false);
        self.selected.set(selection);
        let primary = self.primary_clip();
        if primary.is_some() {
            self.selected_media.set(None);
            self.sidebar.set(Sidebar::Inspector);
            self.sync_title_controllers();
            self.sync_title_duration(true);
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
        self.queue_waveforms();
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
        self.last_playback_monitor.set(PlaybackMonitor::Source);
        self.pause();
        self.audio_failed.set(false);
        self.source.playing.set(true);
        if self.source.position.peek() >= range.duration.saturating_sub(1) {
            self.source.position.set(0);
        }
        self.request_source_preview();
    }

    pub fn pause_source(&self) {
        self.stop(&self.source);
    }

    pub fn seek_source(&self, frame: u64) {
        if self.source.position.peek() == frame
            && !matches!(self.source.preview.peek(), PreviewState::Failed(_))
        {
            return;
        }
        self.last_playback_monitor.set(PlaybackMonitor::Source);
        if self.source.playing.peek() {
            self.pause_source();
        }
        self.source.position.set(frame);
        self.request_source_preview();
    }

    pub(super) fn request_preview(&self) {
        let (plan, audio) = self.program_plan();
        self.stream(&self.program, plan, audio, true);
    }

    /// The Program monitor's video and audio plans, leaving out missing media
    /// so the rest of the sequence still previews.
    fn program_plan(&self) -> (PreviewPlan, AudioPlan) {
        let missing = self.missing_media.peek();
        let mask_drag = self.mask_drag.peek();
        let editor = self.editor.borrow();
        let project = editor.project();
        let mut sources = Vec::new();
        let audio = AudioPlan::without_media(project, &missing);
        for track in project.tracks() {
            if track.hidden {
                continue;
            }
            for clip in &track.clips {
                let ClipContent::Media { media_id } = clip.content else {
                    continue;
                };
                if missing.contains(&media_id) {
                    continue;
                }
                let Some(media) = project.media().iter().find(|media| media.id == media_id) else {
                    continue;
                };
                if track.kind == TrackKind::Video {
                    sources.push(FrameSource {
                        speed: clip.speed,
                        playback: clip.playback,
                        effect: clip.effect.clone(),
                        mask: match mask_drag {
                            Some((track_id, clip_id, _, mask))
                                if track_id == track.id && clip_id == clip.id =>
                            {
                                Some(mask)
                            }
                            _ => clip.mask.as_deref().copied(),
                        },
                        lut: clip.lut.clone(),
                        chroma_key: clip.chroma_key,
                        color: clip.color,
                        clip_id: clip.id,
                        path: media.path.clone(),
                        kind: media.kind,
                        start: clip.start_frame,
                        source_start: clip.source_start_frame,
                        duration: clip.duration_frames,
                        stabilization: clip.stabilization.as_ref().map(|s| {
                            (
                                s.analysis.start_frame,
                                std::sync::Arc::new(crate::media::stabilize::corrections(s)),
                            )
                        }),
                        keyframes: clip.keyframes.clone(),
                        transform: clip.transform,
                    });
                }
            }
        }
        let fps = (
            project.settings().frame_rate.numerator,
            project.settings().frame_rate.denominator,
        );
        let resolution = (project.settings().width, project.settings().height);
        let mut visual = VisualPlan::without_media(project, &missing);
        if let Some(((.., clip_id), amount)) = self.effect_drag.peek() {
            visual.set_effect_amount(clip_id, amount);
        }
        if let Some((_, clip_id, _, _)) = mask_drag
            && let Some(source) = sources.iter().find(|source| source.clip_id == clip_id)
        {
            visual.replace_mask(clip_id, source.mask);
        }
        if let Some(((.., clip_id), title)) = self.title_preview.peek() {
            visual.replace_title(clip_id, title);
        }
        if let Some(((.., clip_id), key)) = self.chroma_preview.peek() {
            for source in &mut sources {
                if source.clip_id == clip_id {
                    source.chroma_key = Some(key);
                }
            }
            visual.replace_chroma_key(clip_id, key);
        }
        drop(editor);
        let end = self.sequence_end();
        let plan = PreviewPlan {
            use_proxies: self.settings.peek().use_proxies,
            sources,
            visual: Some(visual),
            end,
            fps,
            resolution,
        };
        (plan, audio)
    }

    fn request_source_preview(&self) {
        self.source.preview.set(PreviewState::Ready);
        if self
            .selected_media
            .peek()
            .is_some_and(|id| self.missing_media.peek().contains(&id))
        {
            self.stop(&self.source);
            return;
        }
        let range = self.source_range();
        let editor = self.editor.borrow();
        let settings = editor.project().settings();
        let fps = (
            settings.frame_rate.numerator,
            settings.frame_rate.denominator,
        );
        let end = range.as_ref().map_or(0, |range| range.duration);
        let plan = PreviewPlan {
            use_proxies: self.settings.peek().use_proxies,
            end,
            visual: None,
            sources: range
                .iter()
                .filter(|range| range.kind != MediaKind::Audio)
                .map(|range| FrameSource {
                    speed: 1.0,
                    playback: range.playback,
                    effect: None,
                    mask: None,
                    lut: None,
                    chroma_key: None,
                    color: Default::default(),
                    clip_id: 0,
                    path: range.path.clone(),
                    kind: range.kind,
                    start: 0,
                    source_start: range.source_start,
                    duration: range.duration,
                    stabilization: None,
                    keyframes: Vec::new(),
                    transform: Transform::default(),
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
                    speed: 1.0,
                    playback: range.playback,
                    fade_in_frames: 0,
                    fade_out_frames: 0,
                    path: range.path.clone(),
                    start: 0,
                    source_start: range.source_start,
                    duration: range.duration,
                    volume: 1.0,
                    noise_reduction: false,
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
        monitor.preview.set(PreviewState::Ready);
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
        let visual = plan
            .sources
            .iter()
            .any(|source| start >= source.start && start - source.start < source.duration)
            || (follow_timeline && self.program_has_title(start));
        monitor.begin_preview(visual && self.preview_available.peek());
        if visual {
            monitor.slot.clear(&window);
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
                        state.fail_preview(&monitor, error);
                        return;
                    }
                };
                let image = if follow_timeline && !state.preview_available.peek() {
                    None
                } else {
                    image
                };
                match image.map(|image| upload_preview_frame(&window, image)) {
                    Some(Ok(texture)) => {
                        monitor.slot.publish(texture, &window);
                        state.preview_frame_ready(&monitor);
                    }
                    Some(Err(error)) => {
                        state.fail_preview(&monitor, error);
                    }
                    None => {
                        monitor.slot.clear(&window);
                        state.preview_frame_ready(&monitor);
                    }
                }
            },
        );
    }

    fn program_has_title(&self, frame: u64) -> bool {
        self.editor.borrow().project().tracks().iter().any(|track| {
            track.kind == TrackKind::Video
                && track.clips.iter().any(|clip| {
                    matches!(clip.content, ClipContent::Title(_))
                        && frame >= clip.start_frame
                        && frame - clip.start_frame < clip.duration_frames
                })
        })
    }

    fn preview_frame_ready(&self, monitor: &Monitor) {
        monitor.frame_ready();
        if let Some(failure) = monitor.failure_status.borrow_mut().take()
            && self.status.peek() == failure
        {
            self.status.set("Preview ready".into());
        }
    }

    fn fail_preview(&self, monitor: &Monitor, error: String) {
        self.stop(monitor);
        monitor.preview.set(PreviewState::Failed(error.clone()));
        let status = format!("Preview failed: {error}");
        *monitor.failure_status.borrow_mut() = Some(status.clone());
        self.status.set(status);
    }

    pub(super) fn retry_preview(&self, target: PlaybackMonitor) {
        match target {
            PlaybackMonitor::Program => self.request_preview(),
            PlaybackMonitor::Source => self.request_source_preview(),
        }
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
        if self.audio_output.borrow().is_none() {
            let output = self.open_audio_output(playback::Output::open);
            *self.audio_output.borrow_mut() = output;
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

    fn open_audio_output<T>(&self, open: impl FnOnce() -> Result<T, String>) -> Option<T> {
        if self.audio_failed.get() {
            return None;
        }
        match open() {
            Ok(output) => Some(output),
            Err(error) => {
                self.audio_failed.set(true);
                self.status.set(error);
                None
            }
        }
    }

    fn reset_monitors(&self) {
        self.program.preview.set(PreviewState::Ready);
        self.source.preview.set(PreviewState::Ready);
        self.last_playback_monitor.set(PlaybackMonitor::Program);
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

    pub(super) fn waveform(&self, path: &Path) -> Option<Arc<Waveform>> {
        self.waveforms.peaks.borrow().get(path)?.clone()
    }

    fn queue_waveforms(&self) {
        self.queue_proxies();
        let Some(app) = self.app.borrow().clone() else {
            return;
        };
        if self.waveforms.pending.borrow().is_some() {
            return;
        }
        let source = self
            .source_range()
            .filter(|range| range.kind == MediaKind::Audio);
        let editor = self.editor.borrow();
        let project = editor.project();
        let missing = self.missing_media.peek();
        let mut peaks = self.waveforms.peaks.borrow_mut();
        peaks.retain(|path, _| {
            project
                .media()
                .iter()
                .any(|media| media.path == *path && !missing.contains(&media.id))
        });
        let needed = project.tracks().iter().flat_map(|track| {
            track.clips.iter().filter_map(|clip| {
                let ClipContent::Media { media_id } = clip.content else {
                    return None;
                };
                if missing.contains(&media_id) {
                    return None;
                }
                let media = project.media().iter().find(|media| media.id == media_id)?;
                carries_sound(track.kind, clip, media).then_some(media.path.as_path())
            })
        });
        let path = source
            .iter()
            .map(|range| range.path.as_path())
            .chain(needed)
            .find(|path| !peaks.contains_key(*path))
            .map(Path::to_path_buf);
        drop(peaks);
        drop(editor);
        let Some(path) = path else { return };
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancelled.clone();
        *self.waveforms.pending.borrow_mut() = Some(cancelled.clone());
        let state = self.clone();
        app.spawn_background(
            move || {
                let result = audio::waveform(&path, worker_cancel);
                (path, result)
            },
            move |(path, result)| {
                if cancelled.load(Ordering::Relaxed) {
                    return;
                }
                state.waveforms.pending.borrow_mut().take();
                let peaks = result.ok().flatten().map(Arc::new);
                let changed = peaks.is_some();
                state.waveforms.peaks.borrow_mut().insert(path, peaks);
                if changed {
                    state.ui_revision.update(|revision| *revision += 1);
                }
                state.queue_waveforms();
            },
        );
    }

    fn reset_waveforms(&self) {
        if let Some(cancelled) = self.proxy_pending.borrow().as_ref() {
            cancelled.store(true, Ordering::Relaxed);
        }
        self.proxy_status.set(HashMap::new());
        if let Some(cancelled) = self.waveforms.pending.borrow_mut().take() {
            cancelled.store(true, Ordering::Relaxed);
        }
        self.waveforms.peaks.borrow_mut().clear();
    }

    fn queue_proxies(&self) {
        let Some(app) = self.app.borrow().clone() else {
            return;
        };
        if self.proxy_pending.borrow().is_some() {
            return;
        }
        let statuses = self.proxy_status.peek();
        let path = self
            .editor
            .borrow()
            .project()
            .media()
            .iter()
            .find(|media| media.kind == MediaKind::Video && !statuses.contains_key(&media.path))
            .map(|media| media.path.clone());
        let Some(path) = path else { return };
        self.proxy_status.update(|statuses| {
            statuses.insert(path.clone(), "Generating proxy…".into());
        });
        let cancelled = Arc::new(AtomicBool::new(false));
        *self.proxy_pending.borrow_mut() = Some(cancelled.clone());
        let worker_cancel = cancelled.clone();
        let state = self.clone();
        app.spawn_background(
            move || {
                let result = crate::media::proxy::generate(&path, &worker_cancel);
                (path, result)
            },
            move |(path, result)| {
                state.proxy_pending.borrow_mut().take();
                if !cancelled.load(Ordering::Relaxed) {
                    let label = match result {
                        Ok(true) => "Proxy",
                        Ok(false) => "",
                        Err(_) => "Proxy unavailable",
                    };
                    state.proxy_status.update(|statuses| {
                        statuses.insert(path, label.into());
                    });
                    if !state.program.playing.peek() {
                        state.request_preview();
                    }
                    if !state.source.playing.peek() {
                        state.request_source_preview();
                    }
                }
                state.queue_proxies();
            },
        );
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
        let use_proxies = self.settings.peek().use_proxies;
        app.spawn_stream(
            move |emit| {
                for key in requests {
                    if cancelled.load(Ordering::Relaxed) {
                        break;
                    }
                    let result = storyboard(&key, cancelled.clone(), use_proxies);
                    emit((key, result));
                }
            },
            move |(key, result): (StoryboardKey, Result<Vec<FrameImage>, String>)| {
                if state.storyboard_serial.get() != serial {
                    return;
                }
                state.finish_storyboard(key, result);
            },
        );
    }

    pub(super) fn clip_storyboard(
        &self,
        clip: &Clip,
        media: &Media,
        fps: (u32, u32),
    ) -> Option<Storyboard> {
        let key = clip_storyboard_key(clip, media, fps)?;
        let images = self.storyboards.borrow().get(&key)?.clone();
        if clip.playback == Playback::Freeze && !images.is_empty() {
            Some(Arc::new(vec![images.first()?.clone()]))
        } else {
            Some(images)
        }
    }

    pub(super) fn clip_thumbnail(
        &self,
        clip: &Clip,
        media: &Media,
        fps: (u32, u32),
    ) -> Option<Arc<RgbaImage>> {
        self.clip_storyboard(clip, media, fps)
            .and_then(|images| images.first().cloned())
            .or_else(|| {
                (clip.color.neutral()
                    && clip.transform.is_identity()
                    && clip.lut.is_none()
                    && clip.effect.is_none()
                    && (media.kind == MediaKind::Image || clip.source_frame_at(0) == Some(0)))
                .then(|| self.thumbnails.borrow().get(&media.path).cloned())
                .flatten()
            })
    }

    fn finish_storyboard(&self, key: StoryboardKey, result: Result<Vec<FrameImage>, String>) {
        let images = result
            .unwrap_or_default()
            .into_iter()
            .filter_map(|frame| {
                RgbaImage::new(frame.width, frame.height, frame.pixels).map(Arc::new)
            })
            .collect();
        self.storyboards.borrow_mut().insert(key, Arc::new(images));
        self.ui_revision.update(|revision| *revision += 1);
    }

    fn storyboard_requests(&self) -> Vec<StoryboardKey> {
        let editor = self.editor.borrow();
        let project = editor.project();
        let fps = (
            project.settings().frame_rate.numerator,
            project.settings().frame_rate.denominator,
        );
        let start = self.timeline_start.peek();
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
                if self.missing_media.peek().contains(&media_id) {
                    return None;
                }
                let media = project.media().iter().find(|media| media.id == media_id)?;
                Some((
                    clip_storyboard_key(clip, media, fps)?,
                    geometry.clip_visible(index, clip.start_frame, clip.duration_frames),
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
            UnsavedChoice::Discard => {
                self.clear_recovery();
                true
            }
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

    fn replace_project(&self, project: Project) {
        self.clear_recovery();
        *self.editor.borrow_mut() = Editor::new(project);
        self.clipboard.borrow_mut().clear();
    }

    pub fn new_project(&self) {
        if self.busy.peek() || !self.settle_unsaved_changes(Self::new_project) {
            return;
        }
        self.replace_project(Project::new("Untitled"));
        self.reset_waveforms();
        self.missing_media.set(HashSet::new());
        self.export_modal.set(false);
        self.project_path.set(None);
        self.thumbnails.borrow_mut().clear();
        self.thumbnail_loading.set(HashSet::new());
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

    pub fn choose_effect(&self, track_id: u64, clip_id: u64) {
        if self.busy.peek() {
            return;
        }
        let Some(app) = self.app.borrow().clone() else {
            return;
        };
        let Some(path) = FileDialog::new()
            .set_title("Choose a WGSL effect")
            .add_filter("WGSL shader", &["wgsl"])
            .pick_file()
        else {
            return;
        };
        self.busy.set(true);
        self.status.set("Checking effect shader…".into());
        let revision = self.revision.peek();
        let state = self.clone();
        app.spawn_background(
            move || {
                let shader = crate::timeline::EffectShader::File(path);
                crate::media::effects::validate(&shader).map(|_| shader)
            },
            move |result| {
                state.busy.set(false);
                match result {
                    Ok(shader) if state.revision.peek() == revision => {
                        state.edit(Command::SetClipEffect {
                            track_id,
                            clip_id,
                            effect: Some(crate::timeline::Effect {
                                shader,
                                amount: 1.0,
                            }),
                        });
                    }
                    Ok(_) => state
                        .status
                        .set("Effect selection cancelled: project changed".into()),
                    Err(error) => {
                        state.show_file_error("Could not load effect shader", error.clone());
                        state.status.set(error);
                    }
                }
            },
        );
    }

    pub fn choose_lut(&self, track_id: u64, clip_id: u64) {
        if self.busy.peek() {
            return;
        }
        let Some(app) = self.app.borrow().clone() else {
            return;
        };
        let Some(path) = FileDialog::new()
            .set_title("Choose a color LUT")
            .add_filter("Cube color LUT", &["cube"])
            .pick_file()
        else {
            return;
        };
        self.busy.set(true);
        self.status.set("Checking color LUT…".into());
        let revision = self.revision.peek();
        let state = self.clone();
        app.spawn_background(
            move || crate::media::lut::Lut::load(&path).map(|_| path),
            move |result| {
                state.busy.set(false);
                match result {
                    Ok(path) if state.revision.peek() == revision => {
                        state.edit(Command::SetClipLut {
                            track_id,
                            clip_id,
                            path: Some(path),
                        });
                    }
                    Ok(_) => state
                        .status
                        .set("LUT selection cancelled: project changed".into()),
                    Err(error) => {
                        state.show_file_error("Could not load color LUT", error.clone());
                        state.status.set(error);
                    }
                }
            },
        );
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

    pub(super) fn import_timeline_dialog(&self) {
        if self.busy.peek() {
            return;
        }
        let Some(path) = FileDialog::new()
            .set_title("Import timeline as a new project")
            .add_filter("Timeline interchange", &["otio", "edl"])
            .pick_file()
        else {
            return;
        };
        self.import_timeline(path, None);
    }

    pub(super) fn select_edl_rate(&self, numerator: u32, denominator: u32) {
        if let Some((path, _)) = self.edl_rate_prompt.peek() {
            self.edl_rate_prompt.set(None);
            self.import_timeline(
                path,
                Some(FrameRate {
                    numerator,
                    denominator,
                }),
            );
        }
    }

    pub(super) fn cancel_edl_rate_prompt(&self) {
        self.edl_rate_prompt.set(None);
        self.status.set("Timeline import cancelled".into());
    }

    fn import_timeline(&self, path: PathBuf, rate: Option<FrameRate>) {
        let Some(app) = self.app.borrow().clone() else {
            return;
        };
        let revision = self.revision.peek();
        self.busy.set(true);
        self.status.set("Importing timeline…".into());
        let state = self.clone();
        app.spawn_background(
            {
                let path = path.clone();
                move || {
                    if rate.is_none()
                        && let Some(drop) = interchange::edl_rate_needed(&path)?
                    {
                        return Ok(TimelineImport::ChooseEdlRate { drop });
                    }
                    interchange::load(
                        &path,
                        &rate.unwrap_or_else(|| VideoSettings::default().frame_rate),
                    )
                    .map(|imported| TimelineImport::Ready(Box::new(imported)))
                }
            },
            move |result| {
                state.busy.set(false);
                match result {
                    Ok(TimelineImport::ChooseEdlRate { drop })
                        if state.revision.peek() == revision =>
                    {
                        state.edl_rate_prompt.set(Some((path, drop)));
                        state.status.set("Choose the EDL frame rate".into());
                    }
                    Ok(TimelineImport::Ready(imported)) if state.revision.peek() == revision => {
                        if !state.dirty.peek() {
                            state.install_imported_timeline(*imported);
                        } else {
                            match state.ask_unsaved_changes() {
                                UnsavedChoice::Discard => {
                                    state.install_imported_timeline(*imported)
                                }
                                UnsavedChoice::Save => {
                                    state.save_then(Some(Box::new(move |state| {
                                        state.install_imported_timeline(*imported);
                                    })))
                                }
                                UnsavedChoice::Cancel => {
                                    state.status.set("Timeline import cancelled".into())
                                }
                            }
                        }
                    }
                    Ok(_) => state
                        .status
                        .set("Import cancelled: project changed while loading".into()),
                    Err(error) => {
                        state.show_file_error("Timeline import failed", error.clone());
                        state.status.set(format!("Timeline import failed: {error}"));
                    }
                }
            },
        );
    }

    fn install_imported_timeline(&self, imported: Imported) {
        let missing = imported.missing.len();
        let mut notes = imported.notes;
        if missing > 0 {
            notes.push(format!(
                "{missing} media files not found. Relink them from the Media bin."
            ));
        }
        self.install_project(
            imported.project,
            None,
            true,
            imported.missing.into_iter().collect(),
        );
        self.update_recovery();
        self.status.set(if missing == 0 {
            "Timeline imported · Save as an Ocus project to keep it".into()
        } else {
            format!("Timeline imported · {missing} media files missing")
        });
        self.timeline_file_notes("Timeline imported", notes);
    }

    pub(super) fn export_timeline_dialog(&self, format: Format) {
        if self.busy.peek() || self.sequence_end() == 0 {
            return;
        }
        let Some(app) = self.app.borrow().clone() else {
            return;
        };
        let extension = format.extension();
        let mut dialog = FileDialog::new()
            .set_title(format!("Export {} timeline", format.label()))
            .add_filter(format.label(), &[extension])
            .set_file_name(format!("timeline.{extension}"));
        if let Some(directory) = self.project_path.peek().as_deref().and_then(Path::parent) {
            dialog = dialog.set_directory(directory);
        }
        let Some(path) = dialog.save_file() else {
            return;
        };
        match path.extension().and_then(|ext| ext.to_str()) {
            Some(ext) if ext.eq_ignore_ascii_case(extension) => self.export_timeline(path, format),
            None if path.extension().is_none() => {
                let path = path.with_extension(extension);
                self.busy.set(true);
                let state = self.clone();
                app.spawn_background(
                    {
                        let path = path.clone();
                        move || path.exists()
                    },
                    move |exists| {
                        state.busy.set(false);
                        if !exists
                            || MessageDialog::new()
                                .set_level(MessageLevel::Warning)
                                .set_title("Replace timeline file?")
                                .set_description(format!(
                                    "{} already exists. Replace it?",
                                    path.display()
                                ))
                                .set_buttons(MessageButtons::YesNo)
                                .show()
                                == MessageDialogResult::Yes
                        {
                            state.export_timeline(path, format);
                        }
                    },
                );
            }
            _ => self.show_file_error(
                "Wrong file extension",
                format!("Use .{extension} for a {} timeline.", format.label()),
            ),
        }
    }

    fn export_timeline(&self, path: PathBuf, format: Format) {
        let Some(app) = self.app.borrow().clone() else {
            return;
        };
        let project = self.editor.borrow().project().clone();
        self.busy.set(true);
        self.status
            .set(format!("Exporting {} timeline…", format.label()));
        let state = self.clone();
        let output = path.display().to_string();
        app.spawn_background(
            move || interchange::save(&path, &project, format),
            move |result| {
                state.busy.set(false);
                match result {
                    Ok(notes) => {
                        state
                            .status
                            .set(format!("Exported {} timeline: {output}", format.label()));
                        state.timeline_file_notes("Timeline exported", notes);
                    }
                    Err(error) => {
                        state.show_file_error("Timeline export failed", error.clone());
                        state.status.set(format!("Timeline export failed: {error}"));
                    }
                }
            },
        );
    }

    fn timeline_file_notes(&self, title: &'static str, notes: Vec<String>) {
        if !notes.is_empty()
            && let Some(app) = self.app.borrow().clone()
        {
            app.spawn_background(
                move || {
                    MessageDialog::new()
                        .set_level(MessageLevel::Warning)
                        .set_title(title)
                        .set_description(notes.join("\n"))
                        .set_buttons(MessageButtons::Ok)
                        .show()
                },
                |_| {},
            );
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
                move || {
                    store::load_with_repairs(&path).map(|(project, repaired)| {
                        let missing = missing_sources(&project);
                        (project, missing, repaired)
                    })
                }
            },
            move |result| {
                state.busy.set(false);
                match result {
                    Ok((project, missing, repaired)) if state.revision.peek() == revision => {
                        state.install_project(project, Some(path), false, missing);
                        state.report_repairs(repaired);
                    }
                    Ok(_) => state
                        .status
                        .set("Open cancelled: project changed while loading".into()),
                    Err(error) => {
                        state.show_file_error("Open failed", error.to_string());
                        state.status.set(format!("Open failed: {error}"));
                    }
                }
            },
        );
    }

    pub fn save(&self) {
        self.save_then(None);
    }

    fn install_project(
        &self,
        project: Project,
        path: Option<PathBuf>,
        recovered: bool,
        missing: HashSet<u64>,
    ) {
        self.reset_waveforms();
        let sources = project
            .media()
            .iter()
            .filter(|media| media.kind != MediaKind::Audio && !missing.contains(&media.id))
            .map(|media| media.path.clone())
            .take(MAX_THUMBNAILS)
            .collect();
        if recovered {
            *self.editor.borrow_mut() = Editor::recovered(project);
            self.clipboard.borrow_mut().clear();
        } else {
            self.replace_project(project);
        }
        self.export_modal.set(false);
        self.thumbnails.borrow_mut().clear();
        self.thumbnail_loading.set(HashSet::new());
        self.cancel_storyboards();
        self.storyboards.borrow_mut().clear();
        self.project_path.set(path);
        self.reset_monitors();
        self.media_query.set_value(String::new());
        self.media_filter.set(MediaFilter::All);
        self.revision.update(|revision| *revision += 1);
        self.dirty.set(recovered);
        self.sidebar.set(Sidebar::Media);
        self.missing_media.set(missing);
        self.status.set(if recovered {
            "Restored unsaved work · Save to keep it".into()
        } else if self.missing_media.peek().is_empty() {
            "Project opened".into()
        } else {
            format!(
                "Project opened · {} media files missing",
                self.missing_media.peek().len()
            )
        });
        self.load_thumbnails(sources);
        self.queue_storyboards();
        self.queue_waveforms();
        self.request_preview();
    }

    fn report_repairs(&self, repaired: usize) {
        if repaired > 0 {
            self.status.update(|status| {
                let clips = if repaired == 1 { "clip" } else { "clips" };
                status.push_str(&format!(
                    " · {repaired} linked {clips} unlinked (source has no sound)"
                ));
            });
        }
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
        let revision = self.editor.borrow().revision();
        self.busy.set(true);
        self.status.set("Saving project…".into());
        let state = self.clone();
        app.spawn_background(
            {
                let path = path.clone();
                move || store::save(&path, &project)
            },
            move |result| {
                creamui::reactive::batch(|| {
                    state.finish_save(path, revision, exported, after_save, result);
                });
            },
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
        let from_export = exported.is_some();
        self.busy.set(false);
        if let Err(error) = &result {
            self.show_file_error("Save failed", error.to_string());
        } else {
            self.autosave_error.set(None);
        }
        let saved_current = result.is_ok() && self.editor.borrow().revision() == revision;
        self.status.set(match result {
            Ok(()) => {
                self.editor.borrow_mut().mark_saved(revision);
                self.project_path.set(Some(path));
                let dirty = self.editor.borrow().is_dirty();
                self.dirty.set(dirty);
                self.update_recovery();
                if dirty {
                    "Saved previous version · Unsaved changes".into()
                } else {
                    exported.map_or_else(
                        || "Project saved".into(),
                        |path| format!("Exported: {} · project saved", path.display()),
                    )
                }
            }
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
        if from_export {
            self.start_next_export();
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

    pub fn subtitle_dialog(&self) {
        if self.busy.peek() {
            return;
        }
        if let Some(paths) = FileDialog::new()
            .set_title("Import subtitles")
            .add_filter("SubRip subtitles", &["srt"])
            .pick_files()
        {
            self.import_paths(paths, None);
        }
    }

    fn import_subtitle_cues(&self, cues: &[crate::subtitles::Cue]) -> Result<usize, String> {
        let track = crate::subtitles::track(self.editor.borrow().project(), cues)?;
        let count = track.clips.len();
        let mut tracks = self.editor.borrow().project().tracks().to_vec();
        let index = tracks
            .iter()
            .rposition(|track| track.kind == TrackKind::Video)
            .map_or(0, |index| index + 1);
        tracks.insert(index, track);
        if self.edit(Command::ReplaceTracks(tracks)) {
            Ok(count)
        } else {
            Err("Subtitles rejected by project validation".into())
        }
    }

    pub fn export_dialog(&self) {
        if !self.can_queue_export() || !export::can_render(self.editor.borrow().project()) {
            return;
        }
        self.export_error.set(None);
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
            let path = last
                .filter(|last| last.path.is_absolute())
                .map(|last| next_export_path(&last.path))
                .unwrap_or_else(|| {
                    self.project_path
                        .peek()
                        .filter(|path| path.is_absolute())
                        .and_then(|path| path.parent().map(Path::to_path_buf))
                        .unwrap_or_else(|| self.export_directory.peek())
                        .join(format!("export.{}", settings.container.extension()))
                });
            (settings, path, last.is_some())
        };
        self.set_export_draft(settings);
        self.export_preset.set(if has_last {
            "Last used".into()
        } else {
            "Project original".into()
        });
        let path = self.export_queue.peek().suggested_path(path);
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

    pub fn open_export_queue(&self) {
        self.close_menu();
        self.export_modal.set(false);
        self.export_queue_scroll.set(0.0);
        self.export_queue_modal.set(true);
    }

    pub fn close_export_queue(&self) {
        self.export_queue_modal.set(false);
    }

    fn can_queue_export(&self) -> bool {
        !self.busy.peek() || self.export_queue.peek().accepts_jobs_while_busy()
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
        if !settings.container.has_video() {
            return Ok(settings);
        }
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
        if !settings.container.uses_video_codec() {
            settings.validate()?;
            return Ok(settings);
        }
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

    pub fn toggle_burn_subtitles(&self) {
        let Ok(mut settings) = self.export_form() else {
            self.status.set("Fix the export fields first".into());
            return;
        };
        settings.burn_subtitles = !settings.burn_subtitles;
        self.set_export_draft(settings);
        self.export_preset.set("Custom".into());
    }

    pub fn change_export_codecs(
        &self,
        container: Option<Container>,
        video: Option<VideoCodec>,
        audio: Option<AudioCodec>,
    ) {
        let current = self.export_draft.peek().container;
        let settings = match container {
            Some(Container::Gif) if current != Container::Gif => {
                Ok(presets::gif_settings(self.editor.borrow().project()))
            }
            Some(next)
                if next != current && (next == Container::Wav || !current.uses_video_codec()) =>
            {
                let editor = self.editor.borrow();
                let project = editor.project().settings();
                Ok(ExportSettings::for_project(
                    project.width,
                    project.height,
                    project.frame_rate.clone(),
                ))
            }
            _ => self.export_form(),
        };
        let Ok(mut settings) = settings else {
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
                Container::Matroska | Container::Gif | Container::Wav => {}
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
            .set_title("Export timeline")
            .add_filter(
                match settings.container {
                    Container::Gif => "Animated GIF",
                    Container::Wav => "WAV audio",
                    _ => "Video",
                },
                &[settings.container.extension()],
            )
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
            self.remember_export_directory(&selected);
            self.export_path.set_value(
                selected
                    .with_extension(settings.container.extension())
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }

    fn remember_export_directory(&self, path: &Path) {
        let Some(directory) = path.parent().filter(|directory| directory.is_absolute()) else {
            return;
        };
        self.export_directory.set(directory.to_path_buf());
        let mut settings = self.settings.peek();
        settings.last_export_directory = Some(directory.to_path_buf());
        self.update_settings(settings);
    }

    pub fn save_export_preset(&self) {
        if self.busy.peek() {
            return;
        }
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
        self.export_error.set(None);
        let settings = match self.export_form() {
            Ok(settings) => settings,
            Err(error) => {
                self.set_export_error(error);
                return;
            }
        };
        let mut path = PathBuf::from(self.export_path.peek().trim());
        if !path.is_absolute() {
            self.set_export_error("Choose an absolute output path".into());
            return;
        }
        if path.extension().is_none() {
            path.set_extension(settings.container.extension());
        }
        self.start_export(path, settings, false);
    }

    fn set_export_error(&self, reason: String) {
        self.export_error.set(Some(reason.clone()));
        self.status.set(reason);
    }

    pub fn quick_export(&self) {
        if !self.can_queue_export() {
            return;
        }
        if self.busy.peek() {
            self.export_dialog();
            return;
        }
        let last = self.editor.borrow().project().last_export().cloned();
        if let Some(last) = last.filter(|last| last.path.is_absolute()) {
            self.start_export(next_export_path(&last.path), last.settings, true);
        } else {
            self.export_dialog();
        }
    }

    pub fn has_missing_timeline_media(&self) -> bool {
        let missing = self.missing_media.get();
        self.editor.borrow().project().tracks().iter().flat_map(|track| &track.clips).any(|clip| matches!(clip.content, ClipContent::Media { media_id } if missing.contains(&media_id)))
    }

    fn start_export(&self, path: PathBuf, settings: ExportSettings, quick: bool) {
        if self.has_missing_timeline_media() {
            self.set_export_error(
                "Cannot export: relink the missing media used in your timeline first".into(),
            );
            return;
        }
        if !self.can_queue_export() || !export::can_render(self.editor.borrow().project()) {
            return;
        }
        let last = LastExport {
            settings: settings.clone(),
            path: path.clone(),
        };
        if let Err(error) = last.validate() {
            self.set_export_error(error);
            return;
        }
        if self.app.borrow().is_none() {
            self.set_export_error("Export requires a running app".into());
            return;
        }
        let project = self.editor.borrow().project().clone();
        let end = self.sequence_end();
        let fps = &project.settings().frame_rate;
        let total = if settings.container.has_video() {
            export::output_frames(
                end,
                (fps.numerator, fps.denominator),
                (
                    settings.frame_rate.numerator,
                    settings.frame_rate.denominator,
                ),
            )
            .unwrap_or(0)
        } else {
            end
        };
        let request = Request {
            project,
            output: last,
            total,
            auto_number: quick,
        };
        let mut queue = self.export_queue.peek();
        if let Err(error) = queue.enqueue(request) {
            self.set_export_error(error);
            return;
        }
        creamui::reactive::batch(|| {
            self.export_queue.set(queue);
            self.export_modal.set(false);
            self.export_error.set(None);
            self.open_export_queue();
            if self.busy.peek() {
                self.status.set("Export added to the queue".into());
            } else {
                self.start_next_export();
            }
        });
    }

    fn start_next_export(&self) {
        let Some(app) = self.app.borrow().clone() else {
            return;
        };
        if self.busy.peek() {
            return;
        }
        let mut queue = self.export_queue.peek();
        let Some(work) = queue.start_next() else {
            return;
        };
        let id = work.id;
        let format = work.request.output.settings.container.extension();
        self.export_queue.set(queue);
        self.busy.set(true);
        self.status.set(format!("Encoding {format}…"));
        let state = self.clone();
        app.spawn_stream(
            move |send| {
                let (output, result) = work.render(|path| send(ExportEvent::Destination(path)));
                send(ExportEvent::Finished(output, result));
            },
            move |event| match event {
                ExportEvent::Destination(path) => state
                    .export_queue
                    .update(|queue| queue.set_active_path(id, path)),
                ExportEvent::Finished(output, result) => state.finish_export(id, output, result),
            },
        );
    }

    fn finish_export(&self, id: u64, output: LastExport, result: Result<(), String>) {
        creamui::reactive::batch(|| {
            let path = output.path.clone();
            self.export_queue
                .update(|queue| queue.finish(id, path.clone(), &result));
            self.busy.set(false);
            match result {
                Ok(()) => {
                    self.remember_export_directory(&path);
                    self.edit(Command::SetLastExport(Some(output)));
                    if let Some(project_path) = self.project_path.peek() {
                        self.save_to_with_export(project_path, Some(path), None);
                        return;
                    }
                    self.status.set(format!(
                        "Exported: {} · Save project to keep settings",
                        path.display()
                    ));
                }
                Err(error) if error == export::CANCELLED => self.status.set(error),
                Err(error) => self.status.set(format!("Export failed: {error}")),
            }
            self.start_next_export();
        });
    }

    pub fn cancel_export(&self, id: u64) {
        self.export_queue.update(|queue| queue.cancel(id));
    }

    pub fn clear_finished_exports(&self) {
        self.export_queue.update(Queue::clear_finished);
    }

    pub fn relink_dialog(&self, media_id: Option<u64>) {
        if self.busy.peek() {
            return;
        }
        let path = if media_id.is_some() {
            FileDialog::new()
                .set_title("Locate missing media file")
                .pick_file()
        } else {
            FileDialog::new()
                .set_title("Locate folder containing missing media")
                .pick_folder()
        };
        if let Some(path) = path {
            self.relink_path(media_id, path);
        }
    }

    pub fn relink_path(&self, media_id: Option<u64>, path: PathBuf) {
        let Some(app) = self.app.borrow().clone() else {
            return;
        };
        if self.busy.peek() {
            return;
        }
        let missing = self.missing_media.peek();
        let sources: Vec<_> = self
            .editor
            .borrow()
            .project()
            .media()
            .iter()
            .filter(|media| missing.contains(&media.id))
            .cloned()
            .collect();
        let revision = self.revision.peek();
        self.busy.set(true);
        self.status.set("Relinking media…".into());
        let state = self.clone();
        app.spawn_background(
            move || {
                let folder = if media_id.is_some() {
                    path.parent().unwrap_or(Path::new(".")).to_path_buf()
                } else {
                    path.clone()
                };
                let entries = match std::fs::read_dir(&folder) {
                    Ok(entries) => entries,
                    Err(error) => return vec![Err(format!("Cannot read folder: {error}"))],
                };
                let files = entries
                    .filter_map(Result::ok)
                    .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
                    .map(|entry| entry.path())
                    .collect();
                let mut matches = folder_matches(&sources, &folder, &files);
                if let Some(id) = media_id {
                    matches.retain(|(source, _)| source.id != id);
                    if let Some(source) = sources.iter().find(|source| source.id == id) {
                        matches.insert(0, (source.clone(), path));
                    }
                }
                matches
                    .into_iter()
                    .map(|(source, path)| {
                        let info = probe(&path)?;
                        if info.kind != source.kind {
                            return Err("The selected file has a different media type".to_string());
                        }
                        Ok(Media {
                            path,
                            duration_us: info.duration_us,
                            has_audio: info.has_audio,
                            ..source
                        })
                    })
                    .collect::<Vec<Result<Media, String>>>()
            },
            move |results| {
                state.busy.set(false);
                if state.revision.peek() != revision {
                    state.status.set("Relink cancelled: project changed".into());
                    return;
                }
                let mut count = 0;
                let mut errors = Vec::new();
                let mut paths = Vec::new();
                for result in results {
                    match result {
                        Ok(media) => {
                            let id = media.id;
                            let path = media.path.clone();
                            if state.edit(Command::RelinkMedia(media)) {
                                state.missing_media.update(|missing| {
                                    missing.remove(&id);
                                });
                                paths.push(path);
                                count += 1;
                            } else {
                                errors.push("File does not fit the existing timeline".to_string());
                            }
                        }
                        Err(error) => errors.push(error),
                    }
                }
                state.load_thumbnails(paths);
                state.queue_storyboards();
                state.queue_waveforms();
                state.request_preview();
                state.request_source_preview();
                let remaining = state.missing_media.peek().len();
                let mut status = format!("Relinked {count} files");
                if remaining > 0 {
                    status.push_str(&format!(" · {remaining} still missing"));
                }
                if let Some(error) = errors.first() {
                    status.push_str(&format!(" · {error}"));
                }
                state.status.set(status);
            },
        );
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
        self.import_progress(0, paths.len(), &paths[0]);
        let total = paths.len();
        let state = self.clone();
        let dialog_app = app.clone();
        app.spawn_stream(
            move |emit| {
                let mut results = Vec::new();
                for (index, path) in paths.into_iter().enumerate() {
                    let result = inspect_import(&path);
                    emit(ImportEvent::Progress(index + 1, path.clone()));
                    results.push((path, result));
                }
                emit(ImportEvent::Complete(results));
            },
            move |event| {
                let results = match event {
                    ImportEvent::Progress(completed, path) => {
                        state.import_progress(completed, total, &path);
                        return;
                    }
                    ImportEvent::Complete(results) => results,
                };
                state.busy.set(false);
                let total = results.len();
                let mut imported = 0;
                let mut ready = Vec::new();
                let mut errors = Vec::new();
                let mut subtitle_summary = None;
                for (source_path, result) in results {
                    match result {
                        Ok(ImportedSource::Subtitles(cues)) => {
                            match state.import_subtitle_cues(&cues) {
                                Ok(count) => {
                                    imported += 1;
                                    subtitle_summary = Some(format!(
                                        "Imported {count} subtitles from {}",
                                        source_path
                                            .file_name()
                                            .unwrap_or_default()
                                            .to_string_lossy()
                                    ));
                                }
                                Err(error) => errors.push((source_path, error)),
                            }
                        }
                        Ok(ImportedSource::Media(path, info, thumb)) => {
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
                                if state
                                    .editor
                                    .borrow()
                                    .project()
                                    .media()
                                    .iter()
                                    .any(|media| media.id == id && media.path == path)
                                {
                                    let thumbnails = state.thumbnails.borrow();
                                    if thumbnails.len() < MAX_THUMBNAILS
                                        || thumbnails.contains_key(&path)
                                    {
                                        drop(thumbnails);
                                        state.finish_thumbnail(path, thumb);
                                    }
                                }
                            } else {
                                errors.push((source_path, "Media ID limit reached".into()));
                            }
                        }
                        Err(error) => errors.push((source_path, error)),
                    }
                }
                if let Some(placement) = placement {
                    errors.extend(state.place_imported_media(ready, placement));
                }
                let summary = import_summary(imported, total, &errors);
                if errors.is_empty() {
                    state.status.set(if total == 1 {
                        subtitle_summary.unwrap_or(summary)
                    } else {
                        summary
                    });
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

    fn import_progress(&self, completed: usize, total: usize, path: &Path) {
        let name = path
            .file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy();
        self.status
            .set(format!("Importing {completed} of {total} · {name}"));
    }

    fn place_imported_media(
        &self,
        ready: Vec<u64>,
        (track_id, mut frame): (u64, u64),
    ) -> Vec<(PathBuf, String)> {
        let mut errors = Vec::new();
        for id in ready {
            let path = self
                .editor
                .borrow()
                .project()
                .media()
                .iter()
                .find(|media| media.id == id)
                .map(|media| media.path.clone())
                .unwrap_or_default();
            let track = edits::compatible_track(self.editor.borrow().project(), id, track_id);
            let Some(track) = track else {
                errors.push((path, "No compatible timeline track".into()));
                continue;
            };
            match self.insert_media_at(id, Some((track, frame))) {
                Some(next) => frame = next,
                None => errors.push((
                    path,
                    format!("Could not place on timeline: {}", self.status.peek()),
                )),
            }
        }
        errors
    }

    fn load_thumbnails(&self, sources: Vec<PathBuf>) {
        let Some(app) = self.app.borrow().clone() else {
            return;
        };
        let state = self.clone();
        let missing = self.missing_media.peek();
        let missing_paths: HashSet<_> = self
            .editor
            .borrow()
            .project()
            .media()
            .iter()
            .filter(|media| missing.contains(&media.id))
            .map(|media| media.path.clone())
            .collect();
        let sources: Vec<_> = sources
            .into_iter()
            .filter(|path| !missing_paths.contains(path))
            .collect();
        self.thumbnail_loading
            .update(|pending| pending.extend(sources.iter().cloned()));
        app.spawn_background(
            move || {
                sources
                    .into_iter()
                    .map(|path| {
                        let image = thumbnail(&path).ok();
                        (path, image)
                    })
                    .collect::<Vec<_>>()
            },
            move |results| {
                let editor = state.editor.borrow();
                for (path, thumb) in results {
                    if editor
                        .project()
                        .media()
                        .iter()
                        .any(|media| media.path == path)
                    {
                        state.finish_thumbnail(path, thumb);
                    }
                }
                drop(editor);
                state.ui_revision.update(|revision| *revision += 1);
            },
        );
    }

    fn finish_thumbnail(&self, path: PathBuf, thumb: Option<Thumbnail>) {
        self.thumbnail_loading.update(|pending| {
            pending.remove(&path);
        });
        let image = thumb.and_then(|thumb| RgbaImage::new(thumb.width, thumb.height, thumb.pixels));
        let mut thumbnails = self.thumbnails.borrow_mut();
        if let Some(image) = image {
            thumbnails.insert(path, Arc::new(image));
        } else {
            thumbnails.remove(&path);
        }
        drop(thumbnails);
        self.ui_revision.update(|revision| *revision += 1);
    }

    pub(super) fn thumbnail_label(&self, media: &Media) -> Option<&'static str> {
        if media.kind == MediaKind::Audio
            || self.missing_media.get().contains(&media.id)
            || self.thumbnails.borrow().contains_key(&media.path)
        {
            None
        } else if self.thumbnail_loading.get().contains(&media.path) {
            Some("Loading…")
        } else {
            Some("No preview")
        }
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

/// What the timeline gives program playback: whether anything can play
/// (muted sound still plays, silently) and whether any of it is picture.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SequenceContent {
    pub playable: bool,
    pub has_picture: bool,
}

impl SequenceContent {
    pub(crate) fn of(project: &Project) -> Self {
        let mut content = Self::default();
        for track in project.tracks() {
            for clip in &track.clips {
                let media_id = match clip.content {
                    ClipContent::Title(_) => {
                        content.playable |= track.kind == TrackKind::Video;
                        continue;
                    }
                    ClipContent::Media { media_id } => media_id,
                };
                let Some(media) = project.media().iter().find(|media| media.id == media_id) else {
                    continue;
                };
                if track.kind == TrackKind::Video && media.kind != MediaKind::Audio {
                    content.has_picture = true;
                    content.playable = true;
                } else if carries_sound(track.kind, clip, media) {
                    content.playable = true;
                }
            }
        }
        content
    }
}

/// Timeline frames a source occupies when inserted; stills default to 3 s.
pub(crate) fn media_frames(media: &Media, fps: (u32, u32)) -> Option<u64> {
    let (numerator, denominator) = (u64::from(fps.0), u64::from(fps.1.max(1)));
    if let Some(duration_us) = media.duration_us {
        let count = u128::from(duration_us) * u128::from(numerator)
            / (1_000_000_u128 * u128::from(denominator));
        let count = if duration_us > 0 && numerator > 0 && media.kind != MediaKind::Image {
            count.max(1)
        } else {
            count
        };
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

enum ImportedSource {
    Media(PathBuf, MediaProbe, Option<Thumbnail>),
    Subtitles(Vec<crate::subtitles::Cue>),
}

fn inspect_import(path: &Path) -> Result<ImportedSource, String> {
    if path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("srt"))
    {
        std::fs::read_to_string(path)
            .map_err(|error| error.to_string())
            .and_then(|source| crate::subtitles::parse(&source))
            .map(ImportedSource::Subtitles)
    } else {
        inspect_source(path).map(|(path, info, thumb)| ImportedSource::Media(path, info, thumb))
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
    use crate::timeline::Clip;

    fn transcription_cues() -> Vec<crate::subtitles::Cue> {
        vec![crate::subtitles::Cue {
            start_us: 0,
            end_us: 1_000_000,
            text: "Hello world".into(),
        }]
    }

    #[test]
    fn model_check_only_downloads_after_consent_and_decline_finishes_cancelled() {
        let state = UiState::new();
        let revision = state.revision.peek();
        let cancel = Arc::new(AtomicBool::new(false));
        *state.transcription_cancel.borrow_mut() = Some(cancel.clone());
        state.busy.set(true);
        assert_eq!(
            state.finish_model_check(revision, Ok(true), || panic!("valid model must not ask")),
            Some(false)
        );
        assert_eq!(
            state.finish_model_check(revision, Ok(false), || true),
            Some(true)
        );
        assert!(state.busy.peek());
        assert!(Arc::ptr_eq(
            state.transcription_cancel.borrow().as_ref().unwrap(),
            &cancel
        ));
        let original = state.editor.borrow().project().tracks().to_vec();
        assert_eq!(
            state.finish_model_check(revision, Ok(false), || false),
            None
        );
        assert_eq!(state.status.peek(), "Subtitle generation cancelled");
        assert!(!state.busy.peek());
        assert!(state.transcription_cancel.borrow().is_none());
        assert_eq!(state.editor.borrow().project().tracks(), original);
    }

    #[test]
    fn stale_cancelled_or_failed_model_checks_never_ask_or_start_downloads() {
        let state = UiState::new();
        let revision = state.revision.peek();
        let cancel = Arc::new(AtomicBool::new(false));
        *state.transcription_cancel.borrow_mut() = Some(cancel.clone());
        state.busy.set(true);
        state.generate_subtitles();
        assert!(cancel.load(Ordering::Relaxed));
        assert_eq!(
            state.finish_model_check(revision, Ok(false), || panic!("cancelled")),
            None
        );
        assert_eq!(state.status.peek(), "Subtitle generation cancelled");
        state.revision.update(|revision| *revision += 1);
        assert_eq!(
            state.finish_model_check(revision, Ok(false), || panic!("stale")),
            None
        );
        assert_eq!(
            state.status.peek(),
            "Subtitle generation cancelled: project changed"
        );
        assert_eq!(
            state.finish_model_check(
                state.revision.peek(),
                Err("check failed".into()),
                || panic!("failed")
            ),
            None
        );
        assert_eq!(
            state.status.peek(),
            "Cannot generate subtitles: check failed"
        );
    }

    #[test]
    fn transcription_status_pluralizes_multiple_cues() {
        let state = UiState::new();
        let mut cues = transcription_cues();
        cues.push(crate::subtitles::Cue {
            start_us: 1_000_000,
            end_us: 2_000_000,
            text: "Again".into(),
        });
        state.finish_transcription(state.revision.peek(), Ok(cues));
        assert_eq!(state.status.peek(), "Generated 2 subtitle cues");
    }

    #[test]
    fn transcription_finish_is_undoable_and_rejects_stale_empty_and_failed_results() {
        let state = super::UiState::new();
        let original = state.editor.borrow().project().tracks().to_vec();
        let revision = state.revision.peek();
        state.finish_transcription(revision, Ok(transcription_cues()));
        assert_eq!(
            state.editor.borrow().project().tracks().len(),
            original.len() + 1
        );
        assert_eq!(state.status.peek(), "Generated 1 subtitle cue");
        state.undo();
        assert_eq!(state.editor.borrow().project().tracks(), original);
        state.finish_transcription(revision, Ok(transcription_cues()));
        assert_eq!(
            state.status.peek(),
            "Subtitle generation cancelled: project changed"
        );
        assert_eq!(state.editor.borrow().project().tracks(), original);
        state.finish_transcription(state.revision.peek(), Ok(vec![]));
        assert_eq!(state.status.peek(), "No speech found");
        state.finish_transcription(state.revision.peek(), Err("decode failed".into()));
        assert_eq!(
            state.status.peek(),
            "Cannot generate subtitles: decode failed"
        );
        state.finish_transcription(state.revision.peek(), Err("Cancelled".into()));
        assert_eq!(state.status.peek(), "Subtitle generation cancelled");
    }

    #[test]
    fn srt_file_import_is_one_undoable_track_edit() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("captions.SRT");
        std::fs::write(
            &path,
            "1\n00:00:00,000 --> 00:00:01,000\nHello\n\n2\n00:00:01,000 --> 00:00:02,000\nWorld",
        )
        .unwrap();
        let state = super::UiState::new();
        let super::ImportedSource::Subtitles(cues) = super::inspect_import(&path).unwrap() else {
            panic!("SRT was probed as media")
        };
        assert_eq!(state.import_subtitle_cues(&cues).unwrap(), 2);
        let imported = state.editor.borrow().project().tracks().to_vec();
        assert_eq!(imported.len(), 5);
        assert!(imported[2].subtitles);
        assert_eq!(imported[2].clips.len(), 2);
        assert!(
            imported[2]
                .clips
                .iter()
                .all(|clip| matches!(clip.content, crate::timeline::ClipContent::Title(_)))
        );
        state.undo();
        assert_eq!(state.editor.borrow().project().tracks().len(), 4);
        state.redo();
        assert_eq!(state.editor.borrow().project().tracks(), imported);
        state.import_subtitle_cues(&cues).unwrap();
        assert_eq!(
            state
                .editor
                .borrow()
                .project()
                .tracks()
                .iter()
                .filter(|track| track.subtitles)
                .count(),
            2
        );
        state.editor.borrow().project().validate().unwrap();
        std::fs::write(&path, "not subtitles").unwrap();
        assert!(super::inspect_import(&path).is_err());
        std::fs::remove_file(&path).unwrap();
        assert!(super::inspect_import(&path).is_err());
    }

    #[test]
    fn proxy_preference_is_applied_to_preview_plans_without_changing_project() {
        let state = UiState::new();
        let before = crate::project::format::to_json(state.editor.borrow().project()).unwrap();
        state
            .settings
            .update(|settings| settings.use_proxies = true);
        assert!(state.program_plan().0.use_proxies);
        state.toggle_proxies();
        assert!(!state.program_plan().0.use_proxies);
        assert!(!state.settings.peek().use_proxies);
        assert_eq!(
            before,
            crate::project::format::to_json(state.editor.borrow().project()).unwrap()
        );
    }

    #[test]
    fn export_validation_preserves_a_dedicated_error() {
        let state = UiState::new();
        state.set_export_draft(state.export_draft.peek());
        state.export_path.set_value("relative.mp4");
        state.submit_export();
        assert_eq!(
            state.export_error.peek().as_deref(),
            Some("Choose an absolute output path")
        );
        state.export_width.set_value("invalid");
        state.submit_export();
        assert!(state.export_error.peek().unwrap().contains("width"));
        state.set_export_error("Export failed: encoder unavailable".into());
        state.status.set("Other operation".into());
        assert_eq!(
            state.export_error.peek().as_deref(),
            Some("Export failed: encoder unavailable")
        );
    }

    #[test]
    fn import_progress_advances_with_each_completed_file() {
        let state = UiState::new();
        state.import_progress(1, 3, Path::new("/media/one.mp4"));
        assert_eq!(state.status.peek(), "Importing 1 of 3 · one.mp4");
        state.import_progress(2, 3, Path::new("/media/two.mp4"));
        assert_eq!(state.status.peek(), "Importing 2 of 3 · two.mp4");
        state.import_progress(3, 3, Path::new("/media/three.mp4"));
        assert_eq!(state.status.peek(), "Importing 3 of 3 · three.mp4");
    }

    #[test]
    fn thumbnail_completion_distinguishes_pending_failed_and_ready() {
        let state = UiState::new();
        let media = Media {
            id: 1,
            path: "video.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(1_000_000),
            has_audio: false,
        };
        state
            .thumbnail_loading
            .set(HashSet::from([media.path.clone()]));
        assert_eq!(state.thumbnail_label(&media), Some("Loading…"));
        state.finish_thumbnail(media.path.clone(), None);
        assert_eq!(state.thumbnail_label(&media), Some("No preview"));
        assert!(state.thumbnail_loading.peek().is_empty());
        state.finish_thumbnail(
            media.path.clone(),
            Some(Thumbnail {
                width: 1,
                height: 1,
                pixels: vec![255; 4],
            }),
        );
        assert_eq!(state.thumbnail_label(&media), None);
        state.finish_thumbnail(
            media.path.clone(),
            Some(Thumbnail {
                width: 1,
                height: 1,
                pixels: Vec::new(),
            }),
        );
        assert_eq!(state.thumbnail_label(&media), Some("No preview"));
        state.missing_media.set(HashSet::from([1]));
        assert_eq!(state.thumbnail_label(&media), None);
        assert_eq!(
            state.thumbnail_label(&Media {
                kind: MediaKind::Audio,
                ..media
            }),
            None
        );
    }

    #[test]
    fn storyboard_failures_and_empty_results_are_cached_without_requeueing() {
        let state = UiState::new();
        let key = storyboard_ranges()[0].0.clone();
        for result in [Err("decode failed".into()), Ok(Vec::new())] {
            state.finish_storyboard(key.clone(), result);
            assert!(state.storyboards.borrow()[&key].is_empty());
            let requests = storyboard_requests(
                &[(key.clone(), true)],
                &mut state.storyboards.borrow_mut(),
                &mut Vec::new(),
            );
            assert!(requests.is_empty());
        }
        state.finish_storyboard(
            key.clone(),
            Ok(vec![FrameImage {
                picture: (0, 0, 1, 1),
                width: 1,
                height: 1,
                pixels: vec![255; 4],
            }]),
        );
        assert_eq!(state.storyboards.borrow()[&key].len(), 1);
    }

    #[test]
    fn autosave_error_survives_other_status_updates_until_success() {
        let state = UiState::new();
        state.finish_autosave(Err("disk full".into()));
        state.status.set("Playing".into());
        assert_eq!(
            state.autosave_error.peek().as_deref(),
            Some("Autosave failed: disk full")
        );
        state.finish_autosave(Ok(()));
        assert_eq!(state.autosave_error.peek(), None);
        state.finish_autosave(Err("disk full".into()));
        let revision = state.editor.borrow().revision();
        state.finish_save(
            "film.ocus".into(),
            revision,
            None,
            None,
            Err(std::io::Error::other("denied")),
        );
        assert!(state.autosave_error.peek().is_some());
        state.finish_save("film.ocus".into(), revision, None, None, Ok(()));
        assert_eq!(state.autosave_error.peek(), None);
    }

    #[test]
    fn successful_preview_frame_clears_only_its_stale_failure_status() {
        let state = UiState::new();
        for monitor in [&state.source, &state.program] {
            state.status.set("Ready".into());
            state.preview_frame_ready(monitor);
            assert_eq!(state.status.peek(), "Ready");

            state.fail_preview(monitor, "Decode failed".into());
            monitor.begin_preview(true);
            state.preview_frame_ready(monitor);
            assert_eq!(monitor.preview.peek(), PreviewState::Ready);
            assert_eq!(state.status.peek(), "Preview ready");

            state.fail_preview(monitor, "Upload failed".into());
            monitor.begin_preview(true);
            state.status.set("Project saved".into());
            state.preview_frame_ready(monitor);
            assert_eq!(monitor.preview.peek(), PreviewState::Ready);
            assert_eq!(state.status.peek(), "Project saved");
        }
        state.fail_preview(&state.source, "Source decode failed".into());
        state.fail_preview(&state.program, "Program decode failed".into());
        state.preview_frame_ready(&state.source);
        assert_eq!(state.status.peek(), "Preview failed: Program decode failed");
        state.preview_frame_ready(&state.program);
        assert_eq!(state.status.peek(), "Preview ready");
    }

    #[test]
    fn monitor_preview_transitions_clear_errors_and_loading() {
        let state = UiState::new();
        for (target, monitor) in [
            (PlaybackMonitor::Source, &state.source),
            (PlaybackMonitor::Program, &state.program),
        ] {
            monitor.begin_preview(true);
            assert_eq!(monitor.preview.peek(), PreviewState::Loading);
            monitor.frame_ready();
            assert_eq!(monitor.preview.peek(), PreviewState::Ready);
            state.fail_preview(monitor, "Decode failed".into());
            assert_eq!(
                monitor.preview.peek(),
                PreviewState::Failed("Decode failed".into())
            );
            assert_eq!(state.status.peek(), "Preview failed: Decode failed");
            assert!(!monitor.playing.peek());
            state.retry_preview(target);
            assert_eq!(monitor.preview.peek(), PreviewState::Ready);
            state.fail_preview(monitor, "Upload failed".into());
            match target {
                PlaybackMonitor::Source => state.seek_source(0),
                PlaybackMonitor::Program => state.seek(0),
            }
            assert_eq!(monitor.preview.peek(), PreviewState::Ready);
            monitor.begin_preview(false);
            assert_eq!(monitor.preview.peek(), PreviewState::Ready);
        }
        state.fail_preview(&state.source, "Decode failed".into());
        state.select_media(1);
        assert_eq!(state.source.preview.peek(), PreviewState::Ready);
    }

    #[test]
    fn next_play_retries_failed_audio_output() {
        for source in [false, true] {
            let state = UiState::new();
            assert!(state.edit(Command::ImportMedia(Media {
                id: 1,
                path: "tone.wav".into(),
                kind: MediaKind::Audio,
                duration_us: Some(2_000_000),
                has_audio: true,
            })));
            state.insert_media(1);
            state.select_media(1);
            let play = || {
                if source {
                    state.play_source();
                } else {
                    state.play();
                }
            };
            play();
            assert_eq!(
                state.open_audio_output::<()>(|| Err("Device unavailable".into())),
                None
            );
            assert_eq!(state.status.peek(), "Device unavailable");
            assert_eq!(
                state.open_audio_output(|| panic!("must not retry within one Play")),
                None::<()>
            );
            state.pause();
            state.pause_source();
            play();
            assert_eq!(state.open_audio_output(|| Ok(())), Some(()));
        }
    }

    #[test]
    fn subframe_media_frames_preserves_unknown_and_zero_durations() {
        let mut media = Media {
            id: 1,
            path: "short.wav".into(),
            kind: MediaKind::Audio,
            duration_us: Some(20_000),
            has_audio: true,
        };
        for kind in [MediaKind::Audio, MediaKind::Video] {
            media.kind = kind;
            for fps in [(30, 1), (30_000, 1001)] {
                assert_eq!(media_frames(&media, fps), Some(1));
            }
            media.duration_us = Some(1_000_000);
            assert_eq!(media_frames(&media, (30, 1)), Some(30));
            media.duration_us = Some(0);
            assert_eq!(media_frames(&media, (30, 1)), None);
            media.duration_us = None;
            assert_eq!(media_frames(&media, (30, 1)), None);
            media.duration_us = Some(20_000);
        }
        media.kind = MediaKind::Image;
        assert_eq!(media_frames(&media, (30, 1)), None);
        media.duration_us = None;
        assert_eq!(media_frames(&media, (30, 1)), Some(90));
    }

    #[test]
    fn normalization_import_insert_is_undoable_and_ignores_stale_results() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("quiet.wav");
        assert!(
            std::process::Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=d=2:r=48000",
                    "-af",
                    "volume=0.25"
                ])
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        let (path, info, _) = inspect_source(&path).unwrap();
        let state = UiState::new();
        assert!(state.edit(Command::ImportMedia(Media {
            id: 1,
            path: path.clone(),
            kind: info.kind,
            duration_us: info.duration_us,
            has_audio: info.has_audio,
        })));
        state.insert_media_at(1, None).unwrap();
        let (track_id, clip_id) = state.primary_clip().unwrap();
        let volume = || {
            state
                .editor
                .borrow()
                .project()
                .tracks()
                .iter()
                .find(|track| track.id == track_id)
                .unwrap()
                .clips[0]
                .volume
        };
        let revision = state.revision.peek();
        let measurement = audio::loudness(&path, 0, 60, (30, 1)).unwrap();
        state.finish_normalization(revision, track_id, clip_id, Ok(measurement));
        assert_eq!(volume(), 2.0);
        assert!(state.status.peek().contains("maximum volume"));
        state.undo();
        assert_eq!(volume(), 1.0);
        state.finish_normalization(revision, track_id, clip_id, Ok(measurement));
        assert_eq!(volume(), 1.0);
        let revision = state.revision.peek();
        state.finish_normalization(
            revision,
            track_id,
            clip_id,
            Ok(audio::Loudness {
                integrated: f64::NEG_INFINITY,
                peak: 0.0,
            }),
        );
        assert_eq!(volume(), 1.0);
        assert!(state.status.peek().contains("silent"));
        state.finish_normalization(revision, track_id, clip_id, Err("decode failed".into()));
        assert!(state.status.peek().contains("decode failed"));
    }

    #[test]
    fn silence_removal_real_audio_ripples_and_undoes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pauses.wav");
        assert!(
            std::process::Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=d=3:r=48000",
                    "-af",
                    "volume=0:enable='between(t,1,2)'",
                ])
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        let (_, info, _) = inspect_source(&path).unwrap();
        let state = UiState::new();
        assert!(state.edit(Command::ImportMedia(Media {
            id: 1,
            path: path.clone(),
            kind: info.kind,
            duration_us: info.duration_us,
            has_audio: true,
        })));
        state.insert_media_at(1, None).unwrap();
        let original = state.editor.borrow().project().tracks().to_vec();
        let fps = state.frame_rate();
        let duration = original
            .iter()
            .flat_map(|track| &track.clips)
            .next()
            .unwrap()
            .duration_frames;
        let ranges = audio::silences(&path, 0, duration, fps).unwrap();
        assert_eq!(ranges.len(), 1);
        let rate = f64::from(fps.0) / f64::from(fps.1);
        let ranges = ranges
            .into_iter()
            .map(|(start, end)| ((start * rate).ceil() as u64, (end * rate).floor() as u64))
            .collect();
        let revision = state.revision.peek();
        state.finish_silence_removal(revision, Ok(ranges));
        assert!(state.status.peek().contains("Removed 1 silent part ("));
        let edited = state.editor.borrow().project().tracks().to_vec();
        let clips: Vec<_> = edited.iter().flat_map(|track| &track.clips).collect();
        assert_eq!(clips.len(), 2);
        assert_eq!(
            clips[0].start_frame + clips[0].duration_frames,
            clips[1].start_frame
        );
        state.undo();
        assert_eq!(state.editor.borrow().project().tracks(), original);
        state.redo();
        assert_eq!(state.editor.borrow().project().tracks(), edited);
        state.finish_silence_removal(state.revision.peek(), Ok(vec![]));
        assert_eq!(state.status.peek(), "No silence found");
        state.finish_silence_removal(state.revision.peek(), Err("decode failed".into()));
        assert!(state.status.peek().contains("decode failed"));
    }

    #[test]
    fn subframe_audio_imports_inserts_and_pads_with_silence() {
        use crate::media::audio::{AudioPlan, AudioQueue, AudioSource, feed};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("click.wav");
        assert!(
            std::process::Command::new("ffmpeg")
                .args(["-v", "error", "-f", "lavfi", "-i", "sine=d=0.02:r=48000"])
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        let (path, info, _) = inspect_source(&path).unwrap();
        assert_eq!(info.duration_us, Some(20_000));
        let state = UiState::new();
        assert!(state.edit(Command::ImportMedia(Media {
            id: 1,
            path: path.clone(),
            kind: info.kind,
            duration_us: info.duration_us,
            has_audio: info.has_audio,
        })));
        assert_eq!(state.insert_media_at(1, None), Some(1));
        let editor = state.editor.borrow();
        let clip = &editor.project().tracks()[2].clips[0];
        assert_eq!(clip.duration_frames, 1);
        assert!(editor.project().validate().is_ok());
        let media = &editor.project().media()[0];
        let mut beyond_source = clip.clone();
        beyond_source.duration_frames = 2;
        assert!(
            !editor
                .project()
                .media_clip_fits(media, TrackKind::Audio, &beyond_source)
        );
        beyond_source.duration_frames = 1;
        beyond_source.source_start_frame = 1;
        assert!(
            !editor
                .project()
                .media_clip_fits(media, TrackKind::Audio, &beyond_source)
        );
        let plan = AudioPlan {
            sources: vec![AudioSource {
                speed: clip.speed,
                playback: clip.playback,
                fade_in_frames: 0,
                fade_out_frames: 0,
                path,
                start: clip.start_frame,
                source_start: clip.source_start_frame,
                duration: clip.duration_frames,
                volume: 1.0,
                noise_reduction: false,
            }],
            end: 1,
            fps: (30, 1),
        };
        let queue = Arc::new(AudioQueue::new());
        feed(&plan, 0, queue.clone(), Arc::new(AtomicBool::new(false))).unwrap();
        let samples: Vec<_> = std::iter::from_fn(|| queue.pop()).collect();
        assert_eq!(samples.len(), 1600 * 2);
        assert!(samples[..960 * 2].iter().any(|sample| sample.abs() > 0.01));
        assert!(samples[960 * 2..].iter().all(|sample| *sample == 0.0));
        drop(editor);
        state.undo();
        assert!(state.editor.borrow().project().tracks()[2].clips.is_empty());
    }

    #[test]
    fn open_repairs_keep_missing_files_note_and_clean_history() {
        let state = UiState::new();
        state.install_project(Project::new("Repair"), None, false, HashSet::from([4]));
        state.report_repairs(2);
        assert_eq!(
            state.status.peek(),
            "Project opened · 1 media files missing · 2 linked clips unlinked (source has no sound)"
        );
        assert!(!state.dirty.peek());
        assert!(!state.editor.borrow().is_dirty());
        assert!(!state.editor.borrow().can_undo());
        let status = state.status.peek();
        state.report_repairs(0);
        assert_eq!(state.status.peek(), status);
    }

    #[test]
    fn empty_recovery_check_leaves_editor_available_and_does_not_finish_other_work() {
        let state = UiState::new();
        state.finish_recovery(state.revision.peek(), None);
        assert!(!state.busy.peek());
        assert!(state.add_title());
        state.busy.set(true);
        state.finish_recovery(state.revision.peek(), None);
        assert!(state.busy.peek());
    }

    #[test]
    fn late_recovery_does_not_replace_work_started_during_startup() {
        let state = UiState::new();
        let revision = state.revision.peek();
        assert!(state.add_title());
        state.finish_recovery(
            revision,
            Some((
                recovery::Snapshot {
                    original_path: Some("previous.ocus".into()),
                    project: Project::new("Previous"),
                },
                HashSet::new(),
                0,
            )),
        );
        assert_eq!(state.editor.borrow().project().name(), "Untitled");
        assert_eq!(state.editor.borrow().project().tracks()[0].clips.len(), 1);
        assert!(state.project_path.peek().is_none());
        assert!(!state.busy.peek());
    }

    #[test]
    fn recovery_tracks_edits_save_discard_and_restore() {
        let dir = std::env::temp_dir().join(format!("ocus-ui-recovery-{}", std::process::id()));
        let mut state = UiState::with_recovery_dir(Some(dir.clone()));
        assert!(state.edit(Command::RenameProject("Recovered film".into())));
        drop(state.autosaver.take());
        let snapshot = recovery::read(&dir).unwrap().unwrap();
        assert_eq!(snapshot.project.name(), "Recovered film");

        state.autosaver = Some(Rc::new(recovery::Autosaver::new(dir.clone())));
        let revision = state.editor.borrow().revision();
        state.finish_save("film.ocus".into(), revision, None, None, Ok(()));
        drop(state.autosaver.take());
        assert!(recovery::read(&dir).unwrap().is_none());

        state.autosaver = Some(Rc::new(recovery::Autosaver::new(dir.clone())));
        assert!(state.edit(Command::RenameProject("Discard me".into())));
        assert!(state.apply_unsaved_choice(UnsavedChoice::Discard, |_| {}));
        drop(state.autosaver.take());
        assert!(recovery::read(&dir).unwrap().is_none());

        let original = PathBuf::from("original.ocus");
        state.install_project(
            snapshot.project,
            Some(original.clone()),
            true,
            HashSet::new(),
        );
        assert!(state.editor.borrow().is_dirty());
        assert!(state.dirty.peek());
        assert_eq!(state.project_path.peek(), Some(original));
        assert_eq!(
            state.status.peek(),
            "Restored unsaved work · Save to keep it"
        );
        let revision = state.editor.borrow().revision();
        state.finish_save("original.ocus".into(), revision, None, None, Ok(()));
        assert!(!state.editor.borrow().is_dirty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn tooltip_resources_are_released_with_removed_controls_and_projects() {
        let state = UiState::new();
        let build = || {
            creamui::reactive::with_context_scope(|| {
                creamui::reactive::provide_context(creamui::ThemeProvider::default());
                drop(crate::ui::build(
                    &state,
                    creamui::Size {
                        width: 1280.0,
                        height: 760.0,
                    },
                ));
            });
        };
        build();
        let baseline = state.tooltips.borrow().len();
        for id in 1..=2 {
            assert!(state.edit(Command::ImportMedia(Media {
                id,
                path: format!("/tmp/tooltip-{id}.png").into(),
                kind: MediaKind::Image,
                duration_us: None,
                has_audio: false,
            })));
        }
        build();
        assert!(
            state
                .tooltips
                .borrow()
                .contains_key(&("Insert at timeline end", 2))
        );
        let removed = state.tooltip_controller(("Insert at timeline end", 1));
        assert!(state.edit(Command::RemoveMedia(1)));
        build();
        assert!(
            !state
                .tooltips
                .borrow()
                .contains_key(&("Insert at timeline end", 1))
        );
        let undo = state.tooltip_controller(("Undo", 0));
        undo.focus(true);
        build();
        assert!(state.tooltip_controller(("Undo", 0)).visible());
        removed.focus(true);
        assert!(
            undo.visible(),
            "a removed controller's effect must be dropped"
        );
        state.dirty.set(false);
        state.new_project();
        build();
        assert!(
            !state
                .tooltips
                .borrow()
                .contains_key(&("Insert at timeline end", 2))
        );
        assert_eq!(state.tooltips.borrow().len(), baseline);
    }

    fn storyboard_ranges() -> Vec<(StoryboardKey, bool)> {
        (0..100)
            .map(|index| {
                (
                    StoryboardKey {
                        transform_bits: StoryboardKey::transform_bits(
                            crate::timeline::Transform::default(),
                        ),
                        effect: None,
                        mask: None,
                        lut: None,
                        chroma_key: None,
                        color_bits: [0.0f32, 1.0, 1.0].map(f32::to_bits),
                        playback: crate::timeline::Playback::Forward,
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
    fn storyboard_loads_every_visible_range_and_trims_overflow_when_hidden() {
        let mut keys = storyboard_ranges();
        for (_, visible) in &mut keys {
            *visible = true;
        }
        let mut cache = HashMap::new();
        let mut recent = Vec::new();
        let requests = storyboard_requests(&keys, &mut cache, &mut recent);
        println!("100 visible ranges: {} queued", requests.len());
        assert!(requests.iter().eq(keys.iter().map(|(key, _)| key)));
        for key in requests {
            cache.insert(key, Arc::new(Vec::new()));
        }
        assert!(storyboard_requests(&keys, &mut cache, &mut recent).is_empty());

        for (_, visible) in &mut keys[..90] {
            *visible = false;
        }
        assert!(storyboard_requests(&keys, &mut cache, &mut recent).is_empty());
        assert_eq!(cache.len(), MAX_STORYBOARDS);
        assert!(keys[90..].iter().all(|(key, _)| cache.contains_key(key)));
        assert_eq!(recent.len(), MAX_STORYBOARDS);
        println!("10 visible ranges after zoom: {} retained", cache.len());
    }

    #[test]
    fn storyboard_does_not_warm_hidden_ranges_past_the_initial_budget() {
        let mut keys = storyboard_ranges();
        for (_, visible) in &mut keys {
            *visible = false;
        }
        keys[90].1 = true;
        let mut cache = HashMap::new();
        let mut recent = Vec::new();
        let requests = storyboard_requests(&keys, &mut cache, &mut recent);
        cache.insert(keys[90].0.clone(), Arc::new(Vec::new()));
        keys[90].1 = false;
        assert_eq!(requests.first(), Some(&keys[90].0));
        assert_eq!(requests.len(), MAX_STORYBOARDS);
        let requests = storyboard_requests(&keys, &mut cache, &mut recent);
        assert!(
            requests
                .iter()
                .eq(keys[..MAX_STORYBOARDS - 1].iter().map(|(key, _)| key))
        );
    }

    #[test]
    fn storyboard_viewport_scroll_and_zoom_request_later_clip_ranges() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("video.mkv");
        assert!(
            std::process::Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=size=32x18:rate=30:duration=100",
                    "-c:v",
                    "ffv1",
                ])
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        let state = UiState::new();
        state.timeline_rect.set(Rect {
            x: 0.0,
            y: 0.0,
            width: 1000.0,
            height: 400.0,
        });
        assert!(state.edit(Command::ImportMedia(Media {
            id: 1,
            path,
            kind: MediaKind::Video,
            duration_us: Some(100_000_000),
            has_audio: false,
        })));
        for index in 0..100 {
            assert!(state.edit(Command::InsertClip {
                track_id: 0,
                clip: Box::new(Clip {
                    playback: crate::timeline::Playback::Forward,
                    effect: None,
                    mask: None,
                    lut: None,
                    chroma_key: None,
                    id: index,
                    start_frame: index * 30,
                    source_start_frame: index * 30,
                    duration_frames: 30,
                    content: ClipContent::Media { media_id: 1 },
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
                })
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
        let requests = state.storyboard_requests();
        assert_eq!(requests.len(), 100 - MAX_STORYBOARDS);
        let range = requests
            .iter()
            .find(|key| key.source_start == 89 * 30)
            .unwrap();
        let frames = storyboard(range, Arc::new(AtomicBool::new(false)), false).unwrap();
        assert_eq!(frames.len(), 3);
        assert!(
            frames
                .iter()
                .all(|frame| (frame.width, frame.height) == (160, 90))
        );
        println!(
            "Zoom to Fit decoded {} real frames for clip 90",
            frames.len()
        );
        for key in requests {
            state
                .storyboards
                .borrow_mut()
                .insert(key, Arc::new(Vec::new()));
        }
        assert_eq!(state.storyboards.borrow().len(), 100);
        state.missing_media.set(HashSet::from([1]));
        assert!(state.storyboard_requests().is_empty());
        assert!(state.storyboards.borrow().is_empty());
        state.select_media(1);
        assert!(state.source_range().is_none());
        state.select_clip(Some((0, 0)));
        assert!(state.source_range().is_none());
    }

    #[test]
    fn storyboard_empty_obsolete_duplicate_and_failed_requests_stay_bounded() {
        let mut keys = storyboard_ranges();
        keys.iter_mut().for_each(|(_, visible)| *visible = true);
        keys.push(keys[0].clone());
        let mut cache = HashMap::new();
        let mut recent = Vec::new();
        let requests = storyboard_requests(&keys, &mut cache, &mut recent);
        assert_eq!(requests.len(), 100);
        assert_eq!(requests.iter().collect::<HashSet<_>>().len(), 100);
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
    fn program_plan_previews_mask_draft_and_escape_restores_saved_mask() {
        use crate::media::composite::Compositor;
        use crate::timeline::Mask;
        use crate::ui::interaction::Action;
        use std::sync::atomic::AtomicBool;

        let state = UiState::new();
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("mask.ppm");
        let mut ppm = b"P6\n32 32\n255\n".to_vec();
        ppm.extend([255, 255, 255].repeat(32 * 32));
        std::fs::write(&path, ppm).unwrap();
        assert!(state.edit(Command::ImportMedia(Media {
            id: 1,
            path,
            kind: MediaKind::Image,
            duration_us: None,
            has_audio: false,
        })));
        state.insert_media_at(1, Some((0, 0))).unwrap();
        let (_, clip_id) = state.primary_clip().unwrap();
        let saved = Mask::default();
        assert!(state.edit(Command::SetClipMask {
            track_id: 0,
            clip_id,
            mask: Some(saved),
        }));
        let render = || {
            let plan = state.program_plan().0;
            Compositor::new(plan.visual.unwrap(), 32, 32, plan.fps)
                .frame(0, &Arc::new(AtomicBool::new(false)))
                .unwrap()
                .unwrap()
                .pixels
                .clone()
        };
        let baseline = render();
        let revision = state.revision.peek();
        for draft in [
            Mask {
                width: 0.2,
                ..saved
            },
            Mask {
                feather: 0.8,
                ..saved
            },
        ] {
            state.mask_drag.set(Some((99, clip_id, 2, draft)));
            assert_eq!(state.program_plan().0.sources[0].mask, Some(saved));
            assert_eq!(render(), baseline);
            state.mask_drag.set(Some((0, clip_id, 2, draft)));
            assert_eq!(state.program_plan().0.sources[0].mask, Some(draft));
            assert_ne!(render(), baseline);
            assert_eq!(
                state.editor.borrow().project().tracks()[0].clips[0]
                    .mask
                    .as_deref()
                    .copied(),
                Some(saved)
            );
            state.run(Action::Escape);
            assert!(state.mask_drag.peek().is_none());
            assert_eq!(state.program_plan().0.sources[0].mask, Some(saved));
            assert_eq!(render(), baseline);
            assert_eq!(state.revision.peek(), revision);
        }
        state.undo();
        assert_eq!(state.program_plan().0.sources[0].mask, None);
    }

    #[test]
    fn chroma_draft_updates_preview_plan_without_editing_the_project() {
        let state = UiState::new();
        assert!(state.edit(Command::ImportMedia(Media {
            id: 1,
            path: "green.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(1_000_000),
            has_audio: false,
        })));
        state.insert_media(1);
        let target = state.primary_clip().unwrap();
        let saved = state.editor.borrow().project().clone();
        let key = crate::timeline::ChromaKey {
            similarity: 0.4,
            ..Default::default()
        };
        state.chroma_preview.set(Some((target, key)));
        let (plan, _) = state.program_plan();
        assert_eq!(plan.sources[0].chroma_key, Some(key));
        assert_eq!(state.editor.borrow().project(), &saved);
        state.select_clip(None);
        assert!(state.chroma_preview.peek().is_none());
        assert!(state.program_plan().0.sources[0].chroma_key.is_none());
    }

    #[test]
    fn program_plan_skips_only_missing_media() {
        let state = UiState::new();
        for (id, path) in [(1, "lost.mp4"), (2, "kept.mp4")] {
            assert!(state.edit(Command::ImportMedia(Media {
                id,
                path: path.into(),
                kind: MediaKind::Video,
                duration_us: Some(2_000_000),
                has_audio: true,
            })));
            state.insert_media(id);
        }
        state.missing_media.set(HashSet::from([1]));
        let (plan, audio) = state.program_plan();
        let kept = [PathBuf::from("kept.mp4")];
        assert!(plan.sources.iter().map(|source| &source.path).eq(&kept));
        assert!(audio.sources.iter().map(|source| &source.path).eq(&kept));
        assert_eq!(plan.end, state.sequence_end());
    }

    #[test]
    fn undoing_all_edits_clears_dirty_state_and_discard_warning() {
        let state = UiState::new();
        assert!(!state.dirty.peek());
        assert!(state.edit(Command::ImportMedia(Media {
            id: 4,
            path: "scene.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(2_000_000),
            has_audio: true,
        })));
        state.insert_media(4);
        state.add_title();
        assert!(state.dirty.peek());

        state.undo();
        state.undo();
        state.undo();

        let editor = state.editor.borrow();
        assert!(editor.project().media().is_empty());
        assert!(
            editor
                .project()
                .tracks()
                .iter()
                .all(|track| track.clips.is_empty())
        );
        assert!(!state.dirty.peek());
        assert!(!state.status.peek().contains("Unsaved changes"));
        assert!(state.settle_unsaved_changes(|_| {}));
        drop(editor);

        state.redo();
        assert!(state.dirty.peek());
        assert!(state.status.peek().contains("Unsaved changes"));
        state.undo();
        assert!(!state.dirty.peek());
    }

    #[test]
    fn saving_an_older_snapshot_marks_only_that_history_revision_clean() {
        let state = UiState::new();
        assert!(state.edit(Command::RenameProject("Saved".into())));
        let saved = state.editor.borrow().revision();
        assert!(state.edit(Command::RenameProject("Later".into())));
        let path = PathBuf::from("cut.ocus");
        state.finish_save(path.clone(), saved, None, None, Ok(()));
        assert_eq!(state.project_path.peek(), Some(path));
        assert!(state.dirty.peek());

        state.undo();
        assert!(!state.dirty.peek());
        assert_eq!(state.status.peek(), "Undid edit · Project saved");
        assert!(state.settle_unsaved_changes(|_| {}));
        state.undo();
        assert!(state.dirty.peek());
        state.redo();
        assert!(!state.dirty.peek());
        assert_eq!(state.status.peek(), "Redid edit · Project saved");
        state.redo();
        assert!(state.dirty.peek());
    }

    #[test]
    fn returning_to_an_in_flight_save_clears_dirty_state_on_completion() {
        let state = UiState::new();
        let runs = Rc::new(Cell::new(0));
        assert!(state.edit(Command::RenameProject("Saved".into())));
        let saved = state.editor.borrow().revision();
        state.undo();
        state.redo();
        state.finish_save(
            "cut.ocus".into(),
            saved,
            None,
            Some(Box::new(counting(&runs))),
            Ok(()),
        );
        assert_eq!(runs.get(), 1);
        assert!(!state.dirty.peek());
        assert_eq!(state.status.peek(), "Project saved");
    }

    #[test]
    fn failed_save_preserves_the_previous_path_and_clean_checkpoint() {
        let state = UiState::new();
        let saved = state.editor.borrow().revision();
        state.finish_save("original.ocus".into(), saved, None, None, Ok(()));
        assert!(state.edit(Command::RenameProject("Unsaved".into())));
        let failed = state.editor.borrow().revision();
        state.finish_save(
            "failed.ocus".into(),
            failed,
            None,
            None,
            Err(std::io::Error::other("disk full")),
        );
        assert_eq!(state.project_path.peek(), Some("original.ocus".into()));
        assert_eq!(state.status.peek(), "Save failed: disk full");
        assert!(state.dirty.peek());
        state.undo();
        assert!(!state.dirty.peek());
        state.redo();
        assert!(state.dirty.peek());
    }

    #[test]
    fn relink_marks_changes_for_the_unsaved_prompt_and_undo_restores_source() {
        let state = UiState::new();
        let original = Media {
            id: 1,
            path: "old.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(1_000_000),
            has_audio: false,
        };
        assert!(state.edit(Command::ImportMedia(original.clone())));
        let revision = state.editor.borrow().revision();
        state.finish_save("cut.ocus".into(), revision, None, None, Ok(()));
        assert!(!state.dirty.peek());
        assert!(state.edit(Command::RelinkMedia(Media {
            path: "new.mp4".into(),
            ..original.clone()
        })));
        assert!(state.dirty.peek());
        assert!(!state.apply_unsaved_choice(UnsavedChoice::Cancel, |_| panic!("cancelled")));
        assert!(state.apply_unsaved_choice(UnsavedChoice::Discard, |_| panic!("discarded")));
        state.undo();
        assert_eq!(state.editor.borrow().project().media(), &[original]);
        assert!(!state.dirty.peek());
        state.redo();
        assert!(state.dirty.peek());
    }

    #[test]
    fn folder_relink_matches_only_exact_filenames() {
        let source = Media {
            id: 1,
            path: "/old/clip.mp4".into(),
            kind: MediaKind::Video,
            duration_us: None,
            has_audio: false,
        };
        let folder = Path::new("/new");
        assert!(folder_matches(&[], folder, &HashSet::new()).is_empty());
        assert!(
            folder_matches(
                std::slice::from_ref(&source),
                folder,
                &HashSet::from([folder.join("CLIP.mp4"), folder.join("nested/clip.mp4")])
            )
            .is_empty()
        );
        assert_eq!(
            folder_matches(
                std::slice::from_ref(&source),
                folder,
                &HashSet::from([folder.join("clip.mp4")])
            ),
            vec![(source, folder.join("clip.mp4"))]
        );
    }
    #[test]
    fn export_destinations_keep_project_paths_ahead_of_the_user_default() {
        let state = UiState::new();
        let root = std::env::temp_dir();
        state.export_directory.set(root.join("Videos"));
        assert!(state.add_title());
        state.export_dialog();
        assert_eq!(
            PathBuf::from(state.export_path.peek()),
            root.join("Videos/export.mp4")
        );

        let chosen = root.join("chosen/output.mp4");
        state.remember_export_directory(&chosen);
        assert_eq!(
            state.settings.peek().last_export_directory,
            Some(root.join("chosen"))
        );
        state.export_dialog();
        assert_eq!(
            PathBuf::from(state.export_path.peek()),
            root.join("chosen/export.mp4")
        );

        state.project_path.set(Some(root.join("project/edit.ocus")));
        state.export_dialog();
        assert_eq!(
            PathBuf::from(state.export_path.peek()),
            root.join("project/export.mp4")
        );

        assert!(state.edit(Command::SetLastExport(Some(LastExport {
            path: chosen,
            settings: state.export_draft.peek(),
        }))));
        state.export_dialog();
        assert_eq!(
            PathBuf::from(state.export_path.peek()),
            root.join("chosen/output-2.mp4")
        );
    }

    #[test]
    fn occupied_drop_reports_each_unplaced_file() {
        let state = UiState::new();
        for id in 1..=3 {
            assert!(state.edit(Command::ImportMedia(Media {
                id,
                path: format!("source-{id}.mp4").into(),
                kind: MediaKind::Video,
                duration_us: Some(1_000_000),
                has_audio: false,
            })));
        }
        state.insert_media(1);
        let track = state.editor.borrow().project().tracks()[0].id;
        let errors = state.place_imported_media(vec![2, 3], (track, 0));
        assert_eq!(errors.len(), 2);
        assert_eq!(errors[0].0, PathBuf::from("source-2.mp4"));
        assert_eq!(
            errors[0].1,
            "Could not place on timeline: That spot on the track is already taken"
        );
        assert_eq!(errors[1].0, PathBuf::from("source-3.mp4"));
        assert!(import_summary(2, 2, &errors).contains("source-2.mp4"));
        assert_eq!(state.editor.borrow().project().tracks()[0].clips.len(), 1);
    }

    #[test]
    fn dropped_batch_places_later_files_when_they_fit() {
        let state = UiState::new();
        for (id, kind) in [(1, MediaKind::Video), (2, MediaKind::Audio)] {
            assert!(state.edit(Command::ImportMedia(Media {
                id,
                path: format!("source-{id}.mp4").into(),
                kind,
                duration_us: Some(1_000_000),
                has_audio: false,
            })));
        }
        state.insert_media(1);
        let track = state.editor.borrow().project().tracks()[0].id;
        assert!(state.place_imported_media(vec![], (track, 0)).is_empty());
        let errors = state.place_imported_media(vec![1, 2], (track, 0));
        assert_eq!(errors.len(), 1);
        let editor = state.editor.borrow();
        let audio = editor
            .project()
            .tracks()
            .iter()
            .find(|track| track.kind == TrackKind::Audio)
            .unwrap();
        assert_eq!(audio.clips.len(), 1);
        assert_eq!(audio.clips[0].start_frame, 0);
        drop(editor);
        assert!(
            state
                .place_imported_media(vec![1, 1], (track, 30))
                .is_empty()
        );
        assert_eq!(state.editor.borrow().project().tracks()[0].clips.len(), 3);
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
        assert!(state.edit(Command::RenameProject("Unsaved".into())));
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

        assert!(state.edit(Command::RenameProject("Later edit".into())));
        state.finish_save(
            path.clone(),
            1,
            None,
            Some(Box::new(counting(&runs))),
            Ok(()),
        );
        assert_eq!(runs.get(), 0);
        assert!(state.dirty.peek());

        state.finish_save(path, 2, None, Some(Box::new(counting(&runs))), Ok(()));
        assert_eq!(runs.get(), 1);
        assert!(!state.dirty.peek());
        assert_eq!(state.status.peek(), "Project saved");
    }

    #[test]
    fn saving_before_new_project_resumes_new_project() {
        let (state, _) = dirty_state();
        state.focus_frame(600);
        let revision = state.editor.borrow().revision();
        state.finish_save(
            "cut.ocus".into(),
            revision,
            None,
            Some(Box::new(UiState::new_project)),
            Ok(()),
        );
        assert_eq!(state.project_path.peek(), None);
        assert_eq!(state.timeline_start.peek(), 0);
        assert_eq!(state.status.peek(), "New project · Not saved");
    }

    #[test]
    fn removed_source_does_not_paste_reimported_footage() {
        use super::super::interaction::Action;

        let state = UiState::new();
        let media = |path: &str| Media {
            id: 1,
            path: path.into(),
            kind: MediaKind::Video,
            duration_us: Some(1_000_000),
            has_audio: false,
        };
        assert!(state.edit(Command::ImportMedia(media("red.mp4"))));
        state.insert_media(1);
        state.run(Action::Cut);
        assert_eq!(state.clipboard.borrow().len(), 1);
        assert!(state.edit(Command::RemoveMedia(1)));
        assert!(state.edit(Command::ImportMedia(media("blue.mp4"))));
        state.run(Action::Paste);
        assert!(
            state
                .editor
                .borrow()
                .project()
                .tracks()
                .iter()
                .all(|track| track.clips.is_empty())
        );
        assert!(state.clipboard.borrow().is_empty());
    }

    #[test]
    fn source_removal_prunes_only_matching_clipboard_entries() {
        use super::super::interaction::Action;

        let state = UiState::new();
        for id in 1..=2 {
            assert!(state.edit(Command::ImportMedia(Media {
                id,
                path: format!("source-{id}.mp4").into(),
                kind: MediaKind::Video,
                duration_us: Some(1_000_000),
                has_audio: false,
            })));
            state.insert_media(id);
            state.run(Action::Copy);
            if id == 1 {
                state.run(Action::Cut);
            }
        }
        let other = state.clipboard.borrow()[0].clone();
        let mut removed = other.clone();
        removed.clip.content = ClipContent::Media { media_id: 1 };
        let mut title = other.clone();
        title.clip.content = ClipContent::Title(Title::default());
        *state.clipboard.borrow_mut() = vec![removed, other.clone(), title.clone()];

        assert!(!state.edit(Command::RemoveMedia(2)));
        assert_eq!(state.clipboard.borrow().len(), 3);
        assert!(state.edit(Command::RemoveMedia(1)));
        assert_eq!(
            *state.clipboard.borrow(),
            vec![other.clone(), title.clone()]
        );
        state.undo();
        assert_eq!(
            *state.clipboard.borrow(),
            vec![other.clone(), title.clone()]
        );
        state.redo();
        assert_eq!(*state.clipboard.borrow(), vec![other, title]);
    }

    #[test]
    fn undo_import_prunes_copied_clips() {
        use super::super::interaction::Action;

        let state = UiState::new();
        assert!(state.edit(Command::ImportMedia(Media {
            id: 1,
            path: "red.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(1_000_000),
            has_audio: false,
        })));
        state.insert_media(1);
        state.run(Action::Copy);
        state.undo();
        assert_eq!(state.clipboard.borrow().len(), 1);
        state.undo();
        assert!(state.clipboard.borrow().is_empty());
        state.redo();
        assert!(state.clipboard.borrow().is_empty());
    }

    #[test]
    fn new_project_does_not_paste_clips_from_the_previous_project() {
        use super::super::interaction::Action;

        let state = UiState::new();
        assert!(state.edit(Command::ImportMedia(Media {
            id: 1,
            path: "project-a.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(1_000_000),
            has_audio: false,
        })));
        state.insert_media(1);
        state.run(Action::Copy);
        assert_eq!(state.clipboard.borrow().len(), 1);
        state.seek(60);
        state.run(Action::Paste);
        assert_eq!(state.editor.borrow().project().tracks()[0].clips.len(), 2);

        state.dirty.set(false);
        state.new_project();
        assert!(state.edit(Command::ImportMedia(Media {
            id: 1,
            path: "project-b.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(1_000_000),
            has_audio: false,
        })));
        state.run(Action::Paste);
        assert!(
            state
                .editor
                .borrow()
                .project()
                .tracks()
                .iter()
                .all(|track| track.clips.is_empty())
        );
        assert!(state.clipboard.borrow().is_empty());
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
        state.seek_source(25);
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

    #[test]
    fn muted_only_audio_sequence_still_plays() {
        let state = UiState::new();
        assert!(state.edit(Command::ImportMedia(Media {
            id: 1,
            path: "voice.wav".into(),
            kind: MediaKind::Audio,
            duration_us: Some(2_000_000),
            has_audio: true,
        })));
        assert!(state.edit(Command::InsertClip {
            track_id: 2,
            clip: Box::new(Clip {
                playback: crate::timeline::Playback::Forward,
                effect: None,
                mask: None,
                lut: None,
                chroma_key: None,
                id: 0,
                start_frame: 0,
                source_start_frame: 0,
                duration_frames: 60,
                content: ClipContent::Media { media_id: 1 },
                volume: 1.0,
                muted: true,
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
            })
        }));
        state.play();
        assert!(state.program.playing.peek());
    }
}
