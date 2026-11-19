//! Pointer gestures, drag and drop, keyboard shortcuts and the clip edits
//! they trigger. Everything here is transient editor state; project changes
//! still go through [`UiState::edit`].

use super::UiState;
use super::controls::Glyph;
use super::edits::{self, ClipRef, CopiedClip};
use super::state::PlaybackMonitor;
use super::timeline_view::{Geometry, Ghost, lane_weight};
use crate::media::MediaKind;
use crate::project::Command;
use crate::timeline::{ClipContent, TrackKind};
use creamui::core::{Key, Modifiers, Point};
use creamui::platform::Key as PlatformKey;
use creamui::{Shortcut, ShortcutScope, WindowHandle};
use std::path::PathBuf;

/// A pointer press being tracked until release.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) enum Gesture {
    #[default]
    Idle,
    Scrub,
    Trim {
        anchor: ClipRef,
        left: bool,
    },
    /// Pressed on a selected clip; becomes a move once the pointer travels.
    /// `click` is the frame to seek to if it is released without moving.
    Clip {
        anchor: ClipRef,
        grab: f64,
        origin: Point,
        click: Option<u64>,
    },
    /// Pressed on empty track space; becomes a rubber band once it travels.
    Marquee {
        origin: Point,
        base: Vec<ClipRef>,
        frame: u64,
    },
    /// Pressed on a media bin item.
    Media {
        id: u64,
        origin: Point,
    },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum DragPayload {
    Media(u64),
    /// The selection, grabbed at `grab` frames on the anchor clip's track.
    Clips {
        anchor: ClipRef,
        grab: f64,
    },
}

/// An active drag; `pointer` is in window coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct DragState {
    pub payload: DragPayload,
    pub pointer: Point,
}

enum DropAction {
    Insert {
        media_id: u64,
        placement: (u64, u64),
    },
    Replace {
        command: Command,
        selection: Vec<ClipRef>,
    },
    Nothing,
}

pub(super) struct DropPlan {
    pub ghosts: Vec<Ghost>,
    pub label: String,
    pub valid: bool,
    action: DropAction,
}

const DRAG_THRESHOLD: f32 = 4.0;
const SNAP_PIXELS: f32 = 8.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Undo,
    Redo,
    Delete,
    RippleDelete,
    SelectAll,
    Escape,
    Copy,
    Cut,
    Paste,
    Duplicate,
    Split,
    PlayPause,
    PlayPauseLastMonitor,
    StepBack,
    StepForward,
    SecondBack,
    SecondForward,
    ToStart,
    ToEnd,
    NudgeLeft,
    NudgeRight,
    ZoomIn,
    ZoomOut,
    AddTitle,
    ToggleMarker,
    PreviousMarker,
    NextMarker,
    New,
    Open,
    Save,
    SaveAs,
    Import,
    Export,
}

fn with(key: Key, ctrl: bool, shift: bool, alt: bool) -> Shortcut {
    let mut shortcut = if ctrl {
        Shortcut::primary(key)
    } else {
        Shortcut::new(key, Modifiers::default())
    };
    shortcut.modifiers.shift = shift;
    shortcut.modifiers.alt = alt;
    shortcut
}

const KEY_BINDINGS: &[(Key, bool, bool, Action)] = &[
    (Key::Char('z'), true, false, Action::Undo),
    (Key::Char('z'), true, true, Action::Redo),
    (Key::Char('y'), true, false, Action::Redo),
    (Key::Delete, false, false, Action::Delete),
    (Key::Backspace, false, false, Action::Delete),
    (Key::Delete, false, true, Action::RippleDelete),
    (Key::Backspace, false, true, Action::RippleDelete),
    (Key::Char('a'), true, false, Action::SelectAll),
    (Key::Escape, false, false, Action::Escape),
    (Key::Char('c'), true, false, Action::Copy),
    (Key::Char('x'), true, false, Action::Cut),
    (Key::Char('v'), true, false, Action::Paste),
    (Key::Char('d'), true, false, Action::Duplicate),
    (Key::Char('k'), true, false, Action::Split),
    (Key::Char('s'), false, false, Action::Split),
    (Key::Char(' '), false, false, Action::PlayPauseLastMonitor),
    (Key::Char('k'), false, false, Action::PlayPause),
    (Key::Left, false, false, Action::StepBack),
    (Key::Right, false, false, Action::StepForward),
    (Key::Left, false, true, Action::SecondBack),
    (Key::Right, false, true, Action::SecondForward),
    (Key::Home, false, false, Action::ToStart),
    (Key::End, false, false, Action::ToEnd),
    (Key::Left, false, false, Action::NudgeLeft),
    (Key::Right, false, false, Action::NudgeRight),
    (Key::Char('='), false, false, Action::ZoomIn),
    (Key::Char('+'), false, false, Action::ZoomIn),
    (Key::Char('+'), false, true, Action::ZoomIn),
    (Key::Char('-'), false, false, Action::ZoomOut),
    (Key::Char('t'), false, false, Action::AddTitle),
    (Key::Char('m'), false, false, Action::ToggleMarker),
    (Key::Char('m'), true, true, Action::PreviousMarker),
    (Key::Char('m'), false, true, Action::NextMarker),
    (Key::Char('n'), true, false, Action::New),
    (Key::Char('o'), true, false, Action::Open),
    (Key::Char('s'), true, false, Action::Save),
    (Key::Char('s'), true, true, Action::SaveAs),
    (Key::Char('i'), true, false, Action::Import),
    (Key::Char('e'), true, false, Action::Export),
];

/// Every keyboard shortcut. They are inactive while a text field has focus.
pub(crate) fn shortcuts() -> Vec<(Shortcut, Action)> {
    KEY_BINDINGS
        .iter()
        .map(|&(key, ctrl, shift, action)| {
            let alt = matches!(action, Action::NudgeLeft | Action::NudgeRight);
            (with(key, ctrl, shift, alt), action)
        })
        .collect()
}

impl Action {
    fn shortcut_group(self) -> &'static str {
        match self {
            Self::PlayPause
            | Self::PlayPauseLastMonitor
            | Self::StepBack
            | Self::StepForward
            | Self::SecondBack
            | Self::SecondForward
            | Self::ToStart
            | Self::ToEnd
            | Self::PreviousMarker
            | Self::NextMarker => "Playback",
            Self::Undo
            | Self::Redo
            | Self::Delete
            | Self::RippleDelete
            | Self::SelectAll
            | Self::Escape
            | Self::Copy
            | Self::Cut
            | Self::Paste
            | Self::Duplicate
            | Self::Split
            | Self::AddTitle
            | Self::ToggleMarker => "Editing",
            Self::NudgeLeft | Self::NudgeRight | Self::ZoomIn | Self::ZoomOut => "Timeline",
            Self::New | Self::Open | Self::Save | Self::SaveAs | Self::Import | Self::Export => {
                "File"
            }
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Undo => "Undo",
            Self::Redo => "Redo",
            Self::Delete => "Remove selection",
            Self::RippleDelete => "Ripple delete",
            Self::SelectAll => "Select all",
            Self::Escape => "Clear / close",
            Self::Copy => "Copy",
            Self::Cut => "Cut",
            Self::Paste => "Paste",
            Self::Duplicate => "Duplicate",
            Self::Split => "Split at playhead",
            Self::PlayPause => "Play / pause",
            Self::PlayPauseLastMonitor => "Play / pause last used monitor",
            Self::StepBack => "Previous frame",
            Self::StepForward => "Next frame",
            Self::SecondBack => "Back one second",
            Self::SecondForward => "Forward one second",
            Self::ToStart => "Go to start",
            Self::ToEnd => "Go to end",
            Self::NudgeLeft => "Nudge left",
            Self::NudgeRight => "Nudge right",
            Self::ZoomIn => "Zoom in",
            Self::ZoomOut => "Zoom out",
            Self::AddTitle => "Add title",
            Self::ToggleMarker => "Add / remove marker",
            Self::PreviousMarker => "Previous marker",
            Self::NextMarker => "Next marker",
            Self::New => "New project",
            Self::Open => "Open project",
            Self::Save => "Save project",
            Self::SaveAs => "Save project as",
            Self::Import => "Import media",
            Self::Export => "Export",
        }
    }
}

fn shortcut_label(shortcut: &Shortcut) -> String {
    let key = match shortcut.key {
        Key::Char(' ') => "Space".into(),
        Key::Char(c) => c.to_uppercase().to_string(),
        Key::Left => "←".into(),
        Key::Right => "→".into(),
        Key::Delete => "Del".into(),
        Key::Backspace => "Bksp".into(),
        Key::Escape => "Esc".into(),
        Key::Home => "Home".into(),
        Key::End => "End".into(),
        key => format!("{key:?}"),
    };
    format!(
        "{}{}{}{key}",
        if shortcut.modifiers.logo {
            "Cmd "
        } else if shortcut.modifiers.ctrl {
            "Ctrl "
        } else {
            ""
        },
        if shortcut.modifiers.shift {
            "Shift "
        } else {
            ""
        },
        if shortcut.modifiers.alt { "Alt " } else { "" },
    )
}

pub(super) struct ShortcutHelp {
    pub title: &'static str,
    pub bindings: Vec<(String, &'static str)>,
}

pub(super) fn shortcut_help() -> Vec<ShortcutHelp> {
    let mut groups: Vec<(Action, Vec<String>)> = Vec::new();
    for (shortcut, action) in shortcuts() {
        let label = shortcut_label(&shortcut);
        if let Some((_, keys)) = groups.iter_mut().find(|(group, _)| *group == action) {
            keys.push(label);
        } else {
            groups.push((action, vec![label]));
        }
    }
    ["Playback", "Editing", "Timeline", "File"]
        .into_iter()
        .map(|title| ShortcutHelp {
            title,
            bindings: groups
                .iter()
                .filter(|(action, _)| action.shortcut_group() == title)
                .map(|(action, keys)| (keys.join(" / "), action.description()))
                .collect(),
        })
        .collect()
}

fn distance(a: Point, b: Point) -> f32 {
    (a.x - b.x).hypot(a.y - b.y)
}

impl UiState {
    /// Wires window-level file drops, modifier tracking and shortcuts.
    pub(super) fn install_input(&self, window: &WindowHandle) {
        let state = self.clone();
        window.on_files_dropped_at(move |paths, point| state.drop_files(paths, point));
        let state = self.clone();
        window.on_pointer_moved(move |point| state.pointer.set(point));
        let state = self.clone();
        window.on_keyboard_input(move |event, held| {
            let mut modifiers = Modifiers {
                ctrl: held.ctrl,
                shift: held.shift,
                alt: held.alt,
                logo: held.logo,
            };
            match event.key {
                PlatformKey::Control => modifiers.ctrl = event.pressed,
                PlatformKey::Shift => modifiers.shift = event.pressed,
                PlatformKey::Alt => modifiers.alt = event.pressed,
                PlatformKey::Super => modifiers.logo = event.pressed,
                _ => {}
            }
            state.modifiers.set(modifiers);
            false
        });
        for (shortcut, action) in shortcuts() {
            let state = self.clone();
            window.register_shortcut(shortcut, ShortcutScope::WithoutTextInput, move || {
                state.run(action)
            });
        }
    }

    pub fn run(&self, action: Action) {
        if action == Action::Escape {
            let controllers: Vec<_> = self
                .tooltips
                .borrow()
                .values()
                .map(|tooltip| tooltip.controller.clone())
                .collect();
            creamui::reactive::batch(|| {
                for controller in controllers {
                    controller.hover(false);
                    controller.focus(false);
                }
            });
        }
        if self.settings_modal.peek() {
            if action == Action::Escape {
                self.close_settings();
            }
            return;
        }
        if self.export_modal.peek() {
            if action == Action::Escape {
                self.close_export_dialog();
            }
            return;
        }
        let busy = self.busy.peek();
        let second = self.frames_per_second_step();
        let frame = self.program.position.peek();
        let last = self.sequence_end().saturating_sub(1);
        match action {
            Action::Undo if !busy => self.undo(),
            Action::Redo if !busy => self.redo(),
            Action::Delete if !busy => self.delete_selection(false),
            Action::RippleDelete if !busy => self.delete_selection(true),
            Action::SelectAll => self.select_all(),
            Action::Escape => {
                if self.menu.peek().is_some() {
                    self.close_menu();
                } else if self.drag.peek().is_some()
                    || matches!(*self.gesture.borrow(), Gesture::Trim { .. })
                {
                    self.cancel_drag();
                } else if self.title_gesture.borrow().is_some() {
                    self.clear_title_gesture();
                } else {
                    self.select_clip(None);
                }
            }
            Action::Copy => {
                self.copy_selection();
            }
            Action::Cut if !busy => {
                if self.copy_selection() {
                    self.delete_selection(false);
                }
            }
            Action::Paste if !busy => self.paste(None),
            Action::Duplicate if !busy => self.duplicate_selection(),
            Action::Split if !busy => self.split_at_playhead(),
            Action::PlayPause => self.toggle_playback_monitor(if self.source.playing.peek() {
                PlaybackMonitor::Source
            } else {
                PlaybackMonitor::Program
            }),
            Action::PlayPauseLastMonitor => self.toggle_last_playback_monitor(),
            Action::StepBack => self.seek(frame.saturating_sub(1)),
            Action::StepForward => self.seek((frame + 1).min(last)),
            Action::SecondBack => self.seek(frame.saturating_sub(second)),
            Action::SecondForward => self.seek((frame + second).min(last)),
            Action::ToStart => self.seek(0),
            Action::ToEnd => self.seek(last),
            Action::NudgeLeft if !busy => self.nudge_selection(-1),
            Action::NudgeRight if !busy => self.nudge_selection(1),
            Action::ZoomIn => self.zoom_timeline(self.timeline_zoom.peek().saturating_sub(1)),
            Action::ZoomOut => self.zoom_timeline(self.timeline_zoom.peek() + 1),
            Action::AddTitle if !busy => {
                self.add_title();
            }
            Action::ToggleMarker if !busy => self.toggle_marker(),
            Action::PreviousMarker => self.jump_to_marker(false),
            Action::NextMarker => self.jump_to_marker(true),
            Action::New => self.new_project(),
            Action::Open => self.open_dialog(),
            Action::Save => self.save(),
            Action::SaveAs => self.save_as_dialog(),
            Action::Import => self.import_dialog(),
            Action::Export if !self.exporting.peek() && self.sequence_end() > 0 => {
                self.quick_export()
            }
            _ => {}
        }
    }

    pub fn add_title(&self) -> bool {
        self.add_title_at(None)
    }

    pub fn add_title_at(&self, placement: Option<(u64, u64)>) -> bool {
        let placement = if placement.is_none() {
            let editor = self.editor.borrow();
            let project = editor.project();
            let frame = self.program.position.peek();
            let duration = u64::from(project.settings().frame_rate.numerator).saturating_mul(3)
                / u64::from(project.settings().frame_rate.denominator);
            let end = frame.saturating_add(duration);
            let top_footage = project
                .tracks()
                .iter()
                .enumerate()
                .filter(|(_, track)| {
                    track.kind == TrackKind::Video
                        && track.clips.iter().any(|clip| {
                            matches!(clip.content, ClipContent::Media { .. })
                                && clip.start_frame <= frame
                                && frame - clip.start_frame < clip.duration_frames
                        })
                })
                .map(|(index, _)| index)
                .max();
            if let Some(top) = top_footage {
                if let Some(track) = project.tracks().iter().skip(top + 1).find(|track| {
                    track.kind == TrackKind::Video
                        && track.clips.iter().all(|clip| {
                            clip.start_frame >= end
                                || clip.start_frame.saturating_add(clip.duration_frames) <= frame
                        })
                }) {
                    Some((track.id, frame))
                } else {
                    let Some((mut tracks, track_id)) = edits::add_track(project, TrackKind::Video)
                    else {
                        self.status.set("Cannot add an overlay track".into());
                        return false;
                    };
                    let Some((_, mut clip)) = edits::title_clip(project, None) else {
                        return false;
                    };
                    clip.start_frame = frame;
                    let id = clip.id;
                    tracks
                        .iter_mut()
                        .find(|track| track.id == track_id)
                        .unwrap()
                        .clips
                        .push(clip);
                    drop(editor);
                    if self.edit(Command::ReplaceTracks(tracks)) {
                        self.select_clip(Some((track_id, id)));
                        self.focus_frame(frame);
                        return true;
                    }
                    return false;
                }
            } else {
                None
            }
        } else {
            placement
        };
        let title = edits::title_clip(self.editor.borrow().project(), placement);
        let Some((track_id, clip)) = title else {
            return false;
        };
        let (id, start) = (clip.id, clip.start_frame);
        if self.edit(Command::InsertClip { track_id, clip }) {
            self.select_clip(Some((track_id, id)));
            self.focus_frame(start);
            true
        } else {
            false
        }
    }

    pub fn delete_selection(&self, ripple: bool) {
        let selection = self.selected.peek();
        if selection.is_empty() {
            return;
        }
        let tracks = {
            let editor = self.editor.borrow();
            if ripple {
                edits::ripple_remove_clips(editor.project(), &selection)
            } else {
                Some(edits::remove_clips(editor.project(), &selection))
            }
        };
        match tracks {
            Some(tracks) => {
                if self.edit(Command::ReplaceTracks(tracks)) {
                    self.select_clip(None);
                }
            }
            None => self
                .status
                .set("Cannot ripple delete this selection".into()),
        }
    }

    fn copy_selection(&self) -> bool {
        let editor = self.editor.borrow();
        let selection = self.selected.peek();
        let mut copied: Vec<_> = editor
            .project()
            .tracks()
            .iter()
            .flat_map(|track| {
                track
                    .clips
                    .iter()
                    .filter(|clip| selection.contains(&(track.id, clip.id)))
                    .map(|clip| CopiedClip {
                        track_id: track.id,
                        track_kind: track.kind,
                        clip: clip.clone(),
                    })
            })
            .collect();
        drop(editor);
        if copied.is_empty() {
            return false;
        }
        copied.sort_by_key(|copied| copied.clip.start_frame);
        self.status.set(match copied.len() {
            1 => "Copied 1 clip".into(),
            count => format!("Copied {count} clips"),
        });
        *self.clipboard.borrow_mut() = copied;
        true
    }

    /// Pastes the clipboard at `at`, or at the playhead.
    fn paste(&self, at: Option<u64>) {
        let clips = self.clipboard.borrow().clone();
        if clips.is_empty() {
            return;
        }
        let at = at.unwrap_or_else(|| self.program.position.peek());
        let plan = edits::paste_clips(self.editor.borrow().project(), &clips, at);
        match plan {
            Some(plan) => {
                if self.edit(Command::ReplaceTracks(plan.tracks)) {
                    self.select_clips(plan.selection);
                }
            }
            None => self
                .status
                .set("Not enough room on those tracks to paste here".into()),
        }
    }

    fn duplicate_selection(&self) {
        let saved = self.clipboard.borrow().clone();
        if self.copy_selection() {
            let end = self
                .clipboard
                .borrow()
                .iter()
                .map(|copied| copied.clip.start_frame + copied.clip.duration_frames)
                .max();
            self.paste(end);
        }
        *self.clipboard.borrow_mut() = saved;
    }

    /// Selected clips under the playhead, or every clip there if none are.
    pub fn split_targets(&self) -> Vec<ClipRef> {
        let editor = self.editor.borrow();
        let under = edits::clips_at(editor.project(), self.program.position.peek());
        let selection = self.selected.peek();
        let selected: Vec<_> = under
            .iter()
            .copied()
            .filter(|clip| selection.contains(clip))
            .collect();
        if selected.is_empty() && selection.is_empty() {
            under
        } else {
            selected
        }
    }

    /// Adds a marker at the playhead, or removes the one already there.
    pub fn toggle_marker(&self) {
        let frame = self.program.position.peek();
        let exists = self
            .editor
            .borrow()
            .project()
            .markers()
            .iter()
            .any(|marker| marker.frame == frame);
        if exists {
            self.edit(Command::RemoveMarker(frame));
        } else {
            self.edit(Command::AddMarker(frame));
        }
    }

    fn jump_to_marker(&self, forward: bool) {
        let at = self.program.position.peek();
        let target = {
            let editor = self.editor.borrow();
            let mut frames = editor.project().markers().iter().map(|marker| marker.frame);
            if forward {
                frames.find(|&frame| frame > at)
            } else {
                frames.rev().find(|&frame| frame < at)
            }
        };
        match target {
            Some(frame) => self.seek(frame),
            None => self.status.set(
                if forward {
                    "There is no marker after the playhead"
                } else {
                    "There is no marker before the playhead"
                }
                .into(),
            ),
        }
    }

    pub fn split_at_playhead(&self) {
        let targets = self.split_targets();
        let at = self.program.position.peek();
        let tracks = edits::split_clips(self.editor.borrow().project(), &targets, at);
        match tracks {
            Some(tracks) => {
                self.edit(Command::ReplaceTracks(tracks));
            }
            None => self
                .status
                .set("Place the playhead inside a clip to split it".into()),
        }
    }

    fn nudge_selection(&self, delta: i64) {
        let selection = self.selected.peek();
        let Some(anchor) = selection.last().copied() else {
            return;
        };
        let plan = edits::move_clips(
            self.editor.borrow().project(),
            &selection,
            anchor.0,
            0,
            delta,
        );
        match plan {
            Some(plan) => {
                if self.edit(Command::ReplaceTracks(plan.tracks)) {
                    self.select_clips(plan.selection);
                }
            }
            None => self.status.set("No room to nudge the selection".into()),
        }
    }

    pub fn begin_media_drag(&self, id: u64, point: Point) {
        *self.gesture.borrow_mut() = Gesture::Media { id, origin: point };
    }

    /// Follows the pointer (window coordinates) during a media or clip press,
    /// starting a drag once it has travelled far enough.
    pub fn drag_pointer(&self, point: Point) {
        let payload = match &*self.gesture.borrow() {
            Gesture::Media { id, origin } => (self.drag.peek().is_some()
                || distance(*origin, point) >= DRAG_THRESHOLD)
                .then_some(DragPayload::Media(*id)),
            Gesture::Clip {
                anchor,
                grab,
                origin,
                ..
            } => (self.drag.peek().is_some() || distance(*origin, point) >= DRAG_THRESHOLD)
                .then_some(DragPayload::Clips {
                    anchor: *anchor,
                    grab: *grab,
                }),
            _ => None,
        };
        if let Some(payload) = payload {
            self.drag.set(Some(DragState {
                payload,
                pointer: point,
            }));
        }
    }

    pub fn end_media_drag(&self) {
        let gesture = std::mem::take(&mut *self.gesture.borrow_mut());
        if matches!(gesture, Gesture::Media { .. }) && self.drag.peek().is_some() {
            self.finish_drag();
        }
    }

    pub fn cancel_drag(&self) {
        *self.gesture.borrow_mut() = Gesture::Idle;
        self.trim_draft.set(None);
        self.drag.set(None);
        self.marquee.set(None);
    }

    fn timeline_geometry(&self) -> Geometry {
        let weights: Vec<_> = self
            .editor
            .borrow()
            .project()
            .tracks()
            .iter()
            .map(|track| lane_weight(track.kind, track.clips.iter().any(|clip| clip.linked_audio)))
            .collect();
        Geometry::new(
            self.timeline_rect.get(),
            &weights,
            self.timeline_start.peek(),
            self.visible_frames(),
            self.track_scroll.peek(),
        )
    }

    /// What releasing the current drag would do.
    pub(super) fn drop_plan(&self) -> Option<DropPlan> {
        let drag = self.drag.peek()?;
        let geometry = self.timeline_geometry();
        let editor = self.editor.borrow();
        let project = editor.project();
        let tolerance = (SNAP_PIXELS / geometry.pixels_per_frame()).round() as i64;
        let playhead = self.program.position.peek();
        let pointer_frame = geometry.frame_at(drag.pointer.x);
        let refuse = |label: &str| DropPlan {
            ghosts: Vec::new(),
            label: label.into(),
            valid: false,
            action: DropAction::Nothing,
        };
        match drag.payload {
            DragPayload::Media(media_id) => {
                let Some(media) = project.media().iter().find(|media| media.id == media_id) else {
                    return Some(refuse("Source was removed"));
                };
                let name = media.path.file_name().map_or_else(
                    || "Source".into(),
                    |name| name.to_string_lossy().into_owned(),
                );
                if !geometry.in_lanes(drag.pointer) {
                    return Some(refuse(&format!("{name} · drop on a track")));
                }
                let wanted = if media.kind == MediaKind::Audio {
                    TrackKind::Audio
                } else {
                    TrackKind::Video
                };
                let lane = geometry
                    .nearest_lane(drag.pointer.y, |index| {
                        project.tracks()[index].kind == wanted
                    })
                    .unwrap_or(0);
                let track_id = project.tracks()[lane].id;
                let rate = &project.settings().frame_rate;
                let duration =
                    super::state::media_frames(media, (rate.numerator, rate.denominator))
                        .unwrap_or(0);
                let start = if pointer_frame < 0.0 {
                    0
                } else {
                    let start = pointer_frame.round() as i64;
                    (start
                        + edits::snap(
                            project,
                            &[],
                            &[start, start + duration as i64],
                            playhead,
                            tolerance,
                        ))
                    .max(0) as u64
                };
                Some(
                    match edits::insert_media(project, media_id, Some((track_id, start))) {
                        Ok(insertion) => DropPlan {
                            ghosts: insertion
                                .tracks
                                .iter()
                                .map(|track_id| Ghost {
                                    track_id: *track_id,
                                    start,
                                    duration,
                                    valid: true,
                                })
                                .collect(),
                            label: name,
                            valid: true,
                            action: DropAction::Insert {
                                media_id,
                                placement: (track_id, start),
                            },
                        },
                        Err(error) => DropPlan {
                            ghosts: vec![Ghost {
                                track_id,
                                start,
                                duration,
                                valid: false,
                            }],
                            label: error,
                            valid: false,
                            action: DropAction::Nothing,
                        },
                    },
                )
            }
            DragPayload::Clips { anchor, grab } => {
                let selection = self.selected.peek();
                let tracks = project.tracks();
                let anchor_kind = tracks.iter().find(|track| track.id == anchor.0)?.kind;
                let same: Vec<_> = tracks
                    .iter()
                    .enumerate()
                    .filter(|(_, track)| track.kind == anchor_kind)
                    .map(|(index, _)| index)
                    .collect();
                let target = geometry
                    .nearest_lane(drag.pointer.y, |index| same.contains(&index))
                    .and_then(|lane| same.iter().position(|index| *index == lane))?;
                let from = same
                    .iter()
                    .position(|index| tracks[*index].id == anchor.0)?;
                let shift = target as i64 - from as i64;
                let delta = (pointer_frame - grab).round() as i64;
                let edges: Vec<_> = edits::selection_edges(project, &selection)
                    .into_iter()
                    .map(|edge| edge + delta)
                    .collect();
                let first = selection
                    .iter()
                    .filter_map(|&(track_id, clip_id)| {
                        tracks
                            .iter()
                            .find(|track| track.id == track_id)?
                            .clips
                            .iter()
                            .find(|clip| clip.id == clip_id)
                            .map(|clip| clip.start_frame)
                    })
                    .min()?;
                let delta = (delta + edits::snap(project, &selection, &edges, playhead, tolerance))
                    .max(-(first.min(i64::MAX as u64) as i64));
                let plan = edits::move_clips(project, &selection, anchor.0, shift, delta);
                let ghosts = selection
                    .iter()
                    .filter_map(|&(track_id, clip_id)| {
                        let track = tracks.iter().find(|track| track.id == track_id)?;
                        let clip = track.clips.iter().find(|clip| clip.id == clip_id)?;
                        let lane = if track.kind == anchor_kind {
                            let index = same.iter().position(|i| tracks[*i].id == track_id)?;
                            tracks[*same.get((index as i64 + shift).max(0) as usize)?].id
                        } else {
                            track_id
                        };
                        Some(Ghost {
                            track_id: lane,
                            start: clip.start_frame.saturating_add_signed(delta),
                            duration: clip.duration_frames,
                            valid: plan.is_some(),
                        })
                    })
                    .collect();
                let fps = (
                    project.settings().frame_rate.numerator,
                    project.settings().frame_rate.denominator,
                );
                let offset =
                    delta.unsigned_abs() as f64 * f64::from(fps.1) / f64::from(fps.0.max(1));
                let count = selection.len();
                let what = if count == 1 {
                    "1 clip".into()
                } else {
                    format!("{count} clips")
                };
                Some(match plan {
                    Some(plan) if delta != 0 || shift != 0 => DropPlan {
                        ghosts,
                        label: format!(
                            "Move {what} {}{offset:.2}s",
                            if delta < 0 { "−" } else { "+" }
                        ),
                        valid: true,
                        action: DropAction::Replace {
                            command: Command::ReplaceTracks(plan.tracks),
                            selection: plan.selection,
                        },
                    },
                    Some(_) => DropPlan {
                        ghosts: Vec::new(),
                        label: format!("Move {what}"),
                        valid: true,
                        action: DropAction::Nothing,
                    },
                    None => DropPlan {
                        ghosts,
                        label: "Clips would overlap or leave the timeline".into(),
                        valid: false,
                        action: DropAction::Nothing,
                    },
                })
            }
        }
    }

    pub fn finish_drag(&self) {
        let plan = self.drop_plan();
        self.cancel_drag();
        let Some(plan) = plan else {
            return;
        };
        match plan.action {
            DropAction::Insert {
                media_id,
                placement,
            } => {
                self.insert_media_at(media_id, Some(placement));
            }
            DropAction::Replace { command, selection } => {
                if self.edit(command) {
                    self.select_clips(selection);
                }
            }
            DropAction::Nothing if !plan.valid => self.status.set(plan.label),
            DropAction::Nothing => {}
        }
    }

    /// Imports files dropped on the window; over a track they are also
    /// placed there, one after another.
    pub fn drop_files(&self, paths: Vec<PathBuf>, point: Point) {
        let geometry = self.timeline_geometry();
        let placement = geometry.in_lanes(point).then(|| {
            let lane = geometry.lane_at(point.y).unwrap_or(0);
            let track_id = self.editor.borrow().project().tracks()[lane].id;
            (track_id, geometry.frame_at(point.x).max(0.0).round() as u64)
        });
        self.import_paths(paths, placement);
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum MenuCommand {
    Action(Action),
    Seek(u64),
    PasteAt(u64),
    TitleAt(u64, u64),
    AddMarker(u64),
    RemoveMarker(u64),
    DetachAudio,
    LinkAudio,
    ToggleMute,
    AddTrack(TrackKind),
    RemoveTrack(u64),
    SelectTrack(u64),
    InsertMedia(u64),
    InsertMediaAt(u64, u64),
    ShowMedia(u64),
    RemoveMedia(u64),
    RelinkMedia(u64),
    LocateMediaFolder,
}

/// One row of a context menu; `command: None` draws a separator.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MenuEntry {
    pub label: String,
    pub icon: Option<Glyph>,
    pub shortcut: &'static str,
    pub enabled: bool,
    pub danger: bool,
    pub command: Option<MenuCommand>,
}

impl MenuEntry {
    fn new(
        label: impl Into<String>,
        icon: Glyph,
        shortcut: &'static str,
        command: MenuCommand,
    ) -> Self {
        Self {
            label: label.into(),
            icon: Some(icon),
            shortcut,
            enabled: true,
            danger: false,
            command: Some(command),
        }
    }

    fn separator() -> Self {
        Self {
            label: String::new(),
            icon: None,
            shortcut: "",
            enabled: false,
            danger: false,
            command: None,
        }
    }

    fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    fn danger(mut self) -> Self {
        self.danger = true;
        self
    }
}

/// An open context menu; `anchor` is in window coordinates.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct MenuState {
    pub anchor: Point,
    pub title: String,
    pub entries: Vec<MenuEntry>,
}

impl UiState {
    pub fn close_menu(&self) {
        if self.menu.peek().is_some() {
            self.menu.set(None);
        }
    }

    fn open_menu(&self, anchor: Point, title: impl Into<String>, entries: Vec<MenuEntry>) {
        self.cancel_drag();
        self.menu.set(Some(MenuState {
            anchor,
            title: title.into(),
            entries,
        }));
    }

    pub fn run_menu(&self, command: MenuCommand) {
        self.close_menu();
        match command {
            MenuCommand::Action(action) => self.run(action),
            MenuCommand::Seek(frame) => self.seek(frame),
            MenuCommand::PasteAt(frame) => self.paste(Some(frame)),
            MenuCommand::TitleAt(track_id, frame) => {
                if !self.add_title_at(Some((track_id, frame))) {
                    self.status.set("There is no room for a title here".into());
                }
            }
            MenuCommand::AddMarker(frame) => {
                self.edit(Command::AddMarker(frame));
            }
            MenuCommand::RemoveMarker(frame) => {
                self.edit(Command::RemoveMarker(frame));
            }
            MenuCommand::DetachAudio => self.detach_audio(),
            MenuCommand::LinkAudio => self.link_audio(),
            MenuCommand::ToggleMute => self.toggle_mute(),
            MenuCommand::AddTrack(kind) => self.add_track(kind),
            MenuCommand::RemoveTrack(track_id) => self.remove_track(track_id),
            MenuCommand::SelectTrack(track_id) => {
                let clips = self
                    .editor
                    .borrow()
                    .project()
                    .tracks()
                    .iter()
                    .filter(|track| track.id == track_id)
                    .flat_map(|track| track.clips.iter().map(|clip| (track.id, clip.id)))
                    .collect();
                self.select_clips(clips);
            }
            MenuCommand::InsertMedia(media_id) => self.insert_media(media_id),
            MenuCommand::InsertMediaAt(media_id, frame) => {
                let track =
                    edits::compatible_track_at(self.editor.borrow().project(), media_id, frame);
                match track {
                    Ok(track) => {
                        self.insert_media_at(media_id, Some((track, frame)));
                    }
                    Err(error) => self.status.set(error),
                }
            }
            MenuCommand::ShowMedia(media_id) => self.select_media(media_id),
            MenuCommand::RelinkMedia(media_id) => self.relink_dialog(Some(media_id)),
            MenuCommand::LocateMediaFolder => self.relink_dialog(None),
            MenuCommand::RemoveMedia(media_id) => {
                if self.edit(Command::RemoveMedia(media_id)) {
                    self.selected_media.set(None);
                    self.status.set("Source removed from the project".into());
                }
            }
        }
    }

    pub fn add_track(&self, kind: TrackKind) {
        let planned = edits::add_track(self.editor.borrow().project(), kind);
        if let Some((tracks, _)) = planned {
            self.edit(Command::ReplaceTracks(tracks));
        }
    }

    pub fn remove_track(&self, track_id: u64) {
        let planned = edits::remove_track(self.editor.borrow().project(), track_id);
        match planned {
            Ok(tracks) => {
                self.edit(Command::ReplaceTracks(tracks));
            }
            Err(error) => self.status.set(error),
        }
    }

    pub fn detach_audio(&self) {
        let planned = edits::detach_audio(self.editor.borrow().project(), &self.selected.peek());
        match planned {
            Some(planned) => {
                if self.edit(Command::ReplaceTracks(planned.tracks)) {
                    self.select_clips(planned.selection);
                    self.status.set("Audio detached onto its own track".into());
                }
            }
            None => self
                .status
                .set("Select a video clip with sound to detach its audio".into()),
        }
    }

    pub fn link_audio(&self) {
        let planned = edits::link_audio(self.editor.borrow().project(), &self.selected.peek());
        match planned {
            Some(planned) => {
                if self.edit(Command::ReplaceTracks(planned.tracks)) {
                    self.select_clips(planned.selection);
                    self.status.set("Audio linked to its video again".into());
                }
            }
            None => self
                .status
                .set("Select a video clip and its matching audio clip to link them".into()),
        }
    }

    /// Whether the selection contains a video clip with linked audio, and a
    /// pair that could be linked.
    pub fn audio_link_state(&self) -> (bool, bool) {
        let editor = self.editor.borrow();
        let selection = self.selected.peek();
        let detachable = selection.iter().any(|&(track_id, clip_id)| {
            editor.project().tracks().iter().any(|track| {
                track.id == track_id
                    && track
                        .clips
                        .iter()
                        .any(|clip| clip.id == clip_id && clip.linked_audio)
            })
        });
        (
            detachable,
            edits::link_audio(editor.project(), &selection).is_some(),
        )
    }

    /// Selected clips that make sound, and whether all of them are muted.
    pub(super) fn audible_selection(&self) -> (Vec<ClipRef>, bool) {
        let editor = self.editor.borrow();
        let selection = self.selected.peek();
        let clips: Vec<_> = editor
            .project()
            .tracks()
            .iter()
            .flat_map(|track| {
                track
                    .clips
                    .iter()
                    .filter(move |clip| {
                        matches!(clip.content, ClipContent::Media { .. })
                            && (track.kind == TrackKind::Audio || clip.linked_audio)
                    })
                    .map(move |clip| ((track.id, clip.id), clip.muted))
            })
            .filter(|(clip, _)| selection.contains(clip))
            .collect();
        let all_muted = !clips.is_empty() && clips.iter().all(|(_, muted)| *muted);
        (clips.into_iter().map(|(clip, _)| clip).collect(), all_muted)
    }

    pub fn toggle_mute(&self) {
        let (clips, all_muted) = self.audible_selection();
        if clips.is_empty() {
            return;
        }
        let mut tracks = self.editor.borrow().project().tracks().to_vec();
        for track in &mut tracks {
            let id = track.id;
            for clip in &mut track.clips {
                if clips.contains(&(id, clip.id)) {
                    clip.muted = !all_muted;
                }
            }
        }
        self.edit(Command::ReplaceTracks(tracks));
    }

    fn track_entries(&self, track: Option<(u64, String)>) -> Vec<MenuEntry> {
        let mut entries = vec![
            MenuEntry::new(
                "Add video track",
                Glyph::Film,
                "",
                MenuCommand::AddTrack(TrackKind::Video),
            ),
            MenuEntry::new(
                "Add audio track",
                Glyph::Music,
                "",
                MenuCommand::AddTrack(TrackKind::Audio),
            ),
        ];
        if let Some((track_id, label)) = track {
            let removable = edits::remove_track(self.editor.borrow().project(), track_id).is_ok();
            entries.push(MenuEntry::new(
                format!("Select all on {label}"),
                Glyph::List,
                "",
                MenuCommand::SelectTrack(track_id),
            ));
            entries.push(MenuEntry::separator());
            entries.push(
                MenuEntry::new(
                    format!("Delete track {label}"),
                    Glyph::Trash,
                    "",
                    MenuCommand::RemoveTrack(track_id),
                )
                .enabled(removable)
                .danger(),
            );
        }
        entries
    }

    /// Opens the track menu at the pointer, for toolbar buttons.
    pub fn open_tracks_menu(&self) {
        let entries = self.track_entries(None);
        self.open_menu(self.pointer.get(), "Tracks", entries);
    }

    /// Opens the right-click menu for whatever is under `point` (window
    /// coordinates) on the timeline: a clip, a gap, or a track header.
    pub fn open_timeline_menu(&self, point: Point) {
        let geometry = self.timeline_geometry();
        if point.y < geometry.top() && point.x >= geometry.left() {
            let markers: Vec<u64> = {
                let editor = self.editor.borrow();
                editor
                    .project()
                    .markers()
                    .iter()
                    .map(|marker| marker.frame)
                    .collect()
            };
            let entries = match geometry.marker_near(&markers, point.x) {
                Some(frame) => vec![
                    MenuEntry::new("Go to marker", Glyph::Locate, "", MenuCommand::Seek(frame)),
                    MenuEntry::new(
                        "Remove marker",
                        Glyph::Trash,
                        "",
                        MenuCommand::RemoveMarker(frame),
                    ),
                ],
                None => vec![MenuEntry::new(
                    "Add marker here",
                    Glyph::Marker,
                    "",
                    MenuCommand::AddMarker(geometry.frame_at(point.x).max(0.0).round() as u64),
                )],
            };
            self.open_menu(point, "Marker", entries);
            return;
        }
        let Some(lane) = geometry
            .lane_at(point.y)
            .filter(|_| point.y >= geometry.top())
        else {
            return;
        };
        let (track_id, kind, label, hit) = {
            let editor = self.editor.borrow();
            let tracks = editor.project().tracks();
            let Some(track) = tracks.get(lane) else {
                return;
            };
            let number = tracks[..=lane]
                .iter()
                .filter(|t| t.kind == track.kind)
                .count();
            let label = format!(
                "{}{number}",
                if track.kind == TrackKind::Video {
                    "V"
                } else {
                    "A"
                }
            );
            let frame = geometry.frame_at(point.x).max(0.0) as u64;
            let hit = (point.x >= geometry.left())
                .then(|| {
                    track
                        .clips
                        .iter()
                        .find(|clip| {
                            clip.start_frame <= frame
                                && frame - clip.start_frame < clip.duration_frames
                        })
                        .map(|clip| (track.id, clip.id))
                })
                .flatten();
            (track.id, track.kind, label, hit)
        };
        if point.x < geometry.left() {
            let entries = self.track_entries(Some((track_id, label.clone())));
            self.open_menu(point, format!("Track {label}"), entries);
            return;
        }
        let frame = geometry.frame_at(point.x).max(0.0).round() as u64;
        let can_paste = !self.clipboard.borrow().is_empty();
        let Some(clip) = hit else {
            let mut entries = vec![
                MenuEntry::new("Paste here", Glyph::Import, "", MenuCommand::PasteAt(frame))
                    .enabled(can_paste),
            ];
            if kind == TrackKind::Video {
                entries.push(MenuEntry::new(
                    "Add title here",
                    Glyph::Title,
                    "",
                    MenuCommand::TitleAt(track_id, frame),
                ));
            }
            entries.push(MenuEntry::new(
                "Move playhead here",
                Glyph::Locate,
                "",
                MenuCommand::Seek(frame),
            ));
            entries.push(MenuEntry::new(
                "Select all",
                Glyph::Grid,
                "Ctrl A",
                MenuCommand::Action(Action::SelectAll),
            ));
            entries.push(MenuEntry::separator());
            entries.extend(self.track_entries(Some((track_id, label.clone()))));
            self.open_menu(point, format!("Track {label}"), entries);
            return;
        };
        if !self.selected.peek().contains(&clip) {
            self.select_clip(Some(clip));
        }
        let count = self.selected.peek().len();
        let (detachable, linkable) = self.audio_link_state();
        let (audible, all_muted) = self.audible_selection();
        let mut entries = vec![
            MenuEntry::new(
                "Cut",
                Glyph::Split,
                "Ctrl X",
                MenuCommand::Action(Action::Cut),
            ),
            MenuEntry::new(
                "Copy",
                Glyph::Copy,
                "Ctrl C",
                MenuCommand::Action(Action::Copy),
            ),
            MenuEntry::new(
                "Paste at playhead",
                Glyph::Import,
                "Ctrl V",
                MenuCommand::Action(Action::Paste),
            )
            .enabled(can_paste),
            MenuEntry::new(
                "Duplicate",
                Glyph::Copy,
                "Ctrl D",
                MenuCommand::Action(Action::Duplicate),
            ),
            MenuEntry::separator(),
            MenuEntry::new(
                "Split at playhead",
                Glyph::Split,
                "S",
                MenuCommand::Action(Action::Split),
            )
            .enabled(!self.split_targets().is_empty()),
        ];
        if detachable {
            entries.push(MenuEntry::new(
                "Detach audio",
                Glyph::Unlink,
                "",
                MenuCommand::DetachAudio,
            ));
        }
        if linkable {
            entries.push(MenuEntry::new(
                "Link audio",
                Glyph::Link,
                "",
                MenuCommand::LinkAudio,
            ));
        }
        if !audible.is_empty() {
            entries.push(MenuEntry::new(
                if all_muted {
                    "Unmute audio"
                } else {
                    "Mute audio"
                },
                if all_muted {
                    Glyph::Volume
                } else {
                    Glyph::Mute
                },
                "",
                MenuCommand::ToggleMute,
            ));
        }
        entries.push(MenuEntry::separator());
        entries.push(
            MenuEntry::new(
                "Delete",
                Glyph::Trash,
                "Del",
                MenuCommand::Action(Action::Delete),
            )
            .danger(),
        );
        entries.push(
            MenuEntry::new(
                "Ripple delete",
                Glyph::Ripple,
                "Shift Del",
                MenuCommand::Action(Action::RippleDelete),
            )
            .danger(),
        );
        let title = if count == 1 {
            label_of_clip(self, clip)
        } else {
            format!("{count} clips")
        };
        self.open_menu(point, title, entries);
    }

    pub fn open_media_menu(&self, media_id: u64, point: Point) {
        let editor = self.editor.borrow();
        let project = editor.project();
        let Some(media) = project.media().iter().find(|media| media.id == media_id) else {
            return;
        };
        let name = media.path.file_name().map_or_else(
            || "Source".into(),
            |name| name.to_string_lossy().into_owned(),
        );
        let insertable = super::state::media_frames(
            media,
            (
                project.settings().frame_rate.numerator,
                project.settings().frame_rate.denominator,
            ),
        )
        .is_some();
        let used = project
            .tracks()
            .iter()
            .flat_map(|track| &track.clips)
            .any(|clip| clip.content == ClipContent::Media { media_id });
        drop(editor);
        let playhead = self.program.position.peek();
        self.open_menu(
            point,
            name,
            vec![
                MenuEntry::new(
                    "Insert at end",
                    Glyph::Plus,
                    "",
                    MenuCommand::InsertMedia(media_id),
                )
                .enabled(insertable),
                MenuEntry::new(
                    "Insert at playhead",
                    Glyph::Locate,
                    "",
                    MenuCommand::InsertMediaAt(media_id, playhead),
                )
                .enabled(insertable),
                MenuEntry::new(
                    "Open in source monitor",
                    Glyph::Play,
                    "",
                    MenuCommand::ShowMedia(media_id),
                ),
                MenuEntry::new(
                    "Relink file…",
                    Glyph::Locate,
                    "",
                    MenuCommand::RelinkMedia(media_id),
                )
                .enabled(self.missing_media.peek().contains(&media_id)),
                MenuEntry::new(
                    "Locate folder…",
                    Glyph::Locate,
                    "",
                    MenuCommand::LocateMediaFolder,
                )
                .enabled(self.missing_media.peek().contains(&media_id)),
                MenuEntry::separator(),
                MenuEntry::new(
                    "Remove from project",
                    Glyph::Trash,
                    "",
                    MenuCommand::RemoveMedia(media_id),
                )
                .enabled(!used)
                .danger(),
            ],
        );
    }
}

fn label_of_clip(state: &UiState, (track_id, clip_id): ClipRef) -> String {
    let editor = state.editor.borrow();
    let project = editor.project();
    let clip = project
        .tracks()
        .iter()
        .find(|track| track.id == track_id)
        .and_then(|track| track.clips.iter().find(|clip| clip.id == clip_id));
    match clip.map(|clip| &clip.content) {
        Some(ClipContent::Title(title)) => title.text.clone(),
        Some(ClipContent::Media { media_id }) => project
            .media()
            .iter()
            .find(|media| media.id == *media_id)
            .and_then(|media| media.path.file_name())
            .map_or_else(|| "Clip".into(), |name| name.to_string_lossy().into_owned()),
        None => "Clip".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::Media;
    use crate::timeline::Clip;
    use creamui::core::Rect;
    use std::collections::HashSet;

    fn press_space(state: &UiState) {
        let (_, action) = shortcuts()
            .into_iter()
            .find(|(shortcut, _)| shortcut.key == Key::Char(' '))
            .unwrap();
        state.run(action);
    }

    fn playback_state() -> (UiState, tempfile::NamedTempFile) {
        let state = UiState::new();
        let source = tempfile::NamedTempFile::new().unwrap();
        assert!(state.edit(Command::ImportMedia(Media {
            id: 1,
            path: source.path().into(),
            kind: MediaKind::Audio,
            duration_us: Some(2_000_000),
            has_audio: true,
        })));
        state.insert_media(1);
        state.select_media(1);
        (state, source)
    }

    #[test]
    fn space_resumes_the_paused_source_instead_of_starting_program() {
        let (state, _source) = playback_state();
        state.play_source();
        assert!(state.source.playing.peek());
        state.pause_source();

        press_space(&state);
        assert_eq!(
            (state.source.playing.peek(), state.program.playing.peek()),
            (true, false)
        );
        press_space(&state);
        assert!(!state.source.playing.peek());
    }

    #[test]
    fn space_follows_seeks_between_source_and_program() {
        let (state, _source) = playback_state();
        press_space(&state);
        assert!(state.program.playing.peek());

        state.seek_source(15);
        press_space(&state);
        assert!(state.source.playing.peek());
        assert_eq!(state.source.position.peek(), 15);
        assert!(!state.program.playing.peek());

        state.seek(20);
        press_space(&state);
        assert!(state.program.playing.peek());
        assert_eq!(state.program.position.peek(), 20);
        assert!(!state.source.playing.peek());
    }

    #[test]
    fn space_falls_back_to_program_when_source_is_cleared() {
        let (state, _source) = playback_state();
        state.insert_media(1);
        state.seek_source(15);
        state.select_clip(None);
        assert!(state.source_range().is_none());

        press_space(&state);
        assert!(state.program.playing.peek());
        assert!(!state.source.playing.peek());
    }

    fn second_step_state(fps: (u32, u32), duration: u64) -> UiState {
        let state = UiState::new();
        let mut fixture = serde_json::to_value(state.editor.borrow().project()).unwrap();
        fixture["settings"]["frame_rate"] = serde_json::json!({
            "numerator": fps.0,
            "denominator": fps.1,
        });
        let project = serde_json::from_value(fixture).unwrap();
        *state.editor.borrow_mut() = crate::project::Editor::new(project);
        if duration > 0 {
            assert!(state.add_title());
            assert!(state.edit(Command::Trim {
                track_id: 0,
                clip_id: 1,
                start_frame: 0,
                source_start_frame: 0,
                duration_frames: duration,
            }));
        }
        state
    }

    #[test]
    fn second_seek_fractional_rate_rounds_to_nearest_frame() {
        let state = second_step_state((30_000, 1001), 3000);
        state.run(Action::SecondForward);
        assert_eq!(state.program.position.peek(), 30);
        state.run(Action::SecondBack);
        assert_eq!(state.program.position.peek(), 0);
    }

    #[test]
    fn second_seek_sixty_jump_probe() {
        let state = second_step_state((30_000, 1001), 3000);
        for _ in 0..60 {
            state.run(Action::SecondForward);
        }
        let frame = state.program.position.peek();
        println!(
            "60 SecondForward at 30000/1001: frame {frame}, {:.2} s",
            frame as f64 * 1001.0 / 30_000.0
        );
        assert_eq!(frame, 1800);
    }

    #[test]
    fn second_seek_integer_rates_and_minimum_frame_are_unchanged() {
        for (fps, step) in [((25, 1), 25), ((30, 1), 30), ((1, 3), 1)] {
            let state = second_step_state(fps, 3000);
            state.run(Action::SecondForward);
            assert_eq!(state.program.position.peek(), step);
            state.run(Action::SecondBack);
            assert_eq!(state.program.position.peek(), 0);
        }
    }

    #[test]
    fn second_seek_clamps_at_start_end_and_empty_sequence() {
        let state = second_step_state((30_000, 1001), 100);
        state.run(Action::SecondBack);
        assert_eq!(state.program.position.peek(), 0);
        state.seek(10);
        state.run(Action::SecondBack);
        assert_eq!(state.program.position.peek(), 0);
        state.seek(90);
        state.run(Action::SecondForward);
        assert_eq!(state.program.position.peek(), 99);
        state.run(Action::SecondForward);
        assert_eq!(state.program.position.peek(), 99);

        let empty = second_step_state((30_000, 1001), 0);
        empty.run(Action::SecondForward);
        assert_eq!(empty.program.position.peek(), 0);
        empty.run(Action::SecondBack);
        assert_eq!(empty.program.position.peek(), 0);
    }

    fn starts(state: &UiState, track: usize) -> Vec<u64> {
        state.editor.borrow().project().tracks()[track]
            .clips
            .iter()
            .map(|clip| clip.start_frame)
            .collect()
    }

    #[test]
    fn insert_at_playhead_uses_the_next_free_compatible_track() {
        let state = UiState::new();
        assert!(state.edit(Command::ImportMedia(crate::media::Media {
            id: 1,
            path: "/tmp/footage.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(3_000_000),
            has_audio: true,
        })));
        assert!(state.edit(Command::InsertClip {
            track_id: 0,
            clip: Clip {
                id: 1,
                start_frame: 45,
                source_start_frame: 0,
                duration_frames: 90,
                content: ClipContent::Media { media_id: 1 },
                volume: 1.0,
                muted: false,
                transition: None,
                fade_in_frames: 0,
                fade_out_frames: 0,
                linked_audio: true,
            },
        }));

        state.run_menu(MenuCommand::InsertMediaAt(1, 45));
        assert_eq!(starts(&state, 0), vec![45]);
        assert_eq!(starts(&state, 1), vec![45]);
        {
            let editor = state.editor.borrow();
            let clip = &editor.project().tracks()[1].clips[0];
            assert_eq!(clip.content, ClipContent::Media { media_id: 1 });
            assert_eq!(clip.duration_frames, 90);
            assert!(clip.linked_audio);
            assert!(editor.project().tracks()[2].clips.is_empty());
        }

        state.run_menu(MenuCommand::InsertMediaAt(1, 45));
        assert_eq!(
            state.status.peek(),
            "That spot on the track is already taken"
        );
        assert_eq!(starts(&state, 0), vec![45]);
        assert_eq!(starts(&state, 1), vec![45]);

        state.run_menu(MenuCommand::InsertMediaAt(1, 135));
        assert_eq!(starts(&state, 0), vec![45, 135]);
        assert_eq!(starts(&state, 1), vec![45]);
    }

    #[test]
    fn escape_dismisses_help_without_losing_editor_or_dialog_actions() {
        let state = UiState::new();
        let help = state.tooltip_controller(("Undo", 0));
        assert!(state.add_title());
        help.hover(true);
        help.focus(true);
        state.run(Action::Escape);
        assert!(!help.visible());
        assert!(state.selected.peek().is_empty());
        state.open_settings();
        help.focus(true);
        state.run(Action::Escape);
        assert!(!help.visible());
        assert!(!state.settings_modal.peek());
    }

    #[test]
    fn dismissing_help_can_synchronously_rebuild_the_tooltip_tree() {
        use crate::ui::controls::{Glyph, Help, icon_button};
        creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(creamui::ThemeProvider::new(creamui::Theme::dark()));
            let state = UiState::new();
            let build_state = state.clone();
            let _effect = creamui::reactive::create_effect(move || {
                let _ = icon_button(Glyph::Undo, Help::new("Undo"), false, &build_state, |_| {});
            });
            let help = state.tooltip_controller(("Undo", 0));
            help.focus(true);
            assert!(help.visible());
            state.run(Action::Escape);
            assert!(!help.visible());
        });
    }

    #[test]
    fn inspector_help_covers_every_registered_shortcut() {
        let help = shortcut_help();
        assert!(help.iter().all(|group| !group.bindings.is_empty()));
        let help: Vec<_> = help.iter().flat_map(|group| &group.bindings).collect();
        for (shortcut, action) in shortcuts() {
            let keys = help
                .iter()
                .find(|(_, description)| *description == action.description())
                .expect("every registered action must have inspector help");
            assert!(
                keys.0
                    .split(" / ")
                    .any(|key| key == shortcut_label(&shortcut))
            );
        }
        assert_eq!(
            help.iter()
                .map(|(keys, _)| keys.split(" / ").count())
                .sum::<usize>(),
            shortcuts().len()
        );
        assert!(
            help.iter()
                .all(|(keys, description)| !keys.is_empty() && !description.is_empty())
        );
    }

    #[test]
    fn every_shortcut_chord_is_unique_and_editing_keys_are_bound() {
        let bindings = shortcuts();
        let chords: HashSet<_> = bindings
            .iter()
            .map(|(shortcut, _)| format!("{shortcut:?}"))
            .collect();
        assert_eq!(chords.len(), bindings.len());
        let bound = |key, ctrl, shift| {
            bindings
                .iter()
                .find(|(shortcut, _)| {
                    shortcut.key == key
                        && shortcut.modifiers.shift == shift
                        && (shortcut.modifiers.ctrl || shortcut.modifiers.logo) == ctrl
                })
                .map(|(_, action)| *action)
        };
        assert_eq!(bound(Key::Char('z'), true, false), Some(Action::Undo));
        assert_eq!(bound(Key::Char('z'), true, true), Some(Action::Redo));
        assert_eq!(bound(Key::Delete, false, false), Some(Action::Delete));
        assert_eq!(bound(Key::Delete, false, true), Some(Action::RippleDelete));
        assert_eq!(
            bound(Key::Char(' '), false, false),
            Some(Action::PlayPauseLastMonitor)
        );
        assert_eq!(bound(Key::Char('k'), false, false), Some(Action::PlayPause));
        assert_eq!(bound(Key::Char('a'), true, false), Some(Action::SelectAll));
    }

    #[test]
    fn adding_a_title_creates_an_overlay_above_occupied_video_tracks_in_one_edit() {
        let state = UiState::new();
        assert!(state.edit(Command::ImportMedia(crate::media::Media {
            id: 1,
            path: "/tmp/footage.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(3_000_000),
            has_audio: false,
        })));
        for (track_id, clip_id) in [(0, 1), (1, 2)] {
            assert!(state.edit(Command::InsertClip {
                track_id,
                clip: Clip {
                    id: clip_id,
                    start_frame: 0,
                    source_start_frame: 0,
                    duration_frames: 90,
                    content: ClipContent::Media { media_id: 1 },
                    volume: 1.0,
                    muted: false,
                    transition: None,
                    fade_in_frames: 0,
                    fade_out_frames: 0,
                    linked_audio: false,
                },
            }));
        }
        assert!(state.add_title());
        let project = state.editor.borrow();
        assert_eq!(project.project().tracks().len(), 5);
        let overlay = &project.project().tracks()[2];
        assert_eq!(overlay.kind, TrackKind::Video);
        assert!(matches!(overlay.clips[0].content, ClipContent::Title(_)));
        drop(project);
        state.undo();
        assert_eq!(state.editor.borrow().project().tracks().len(), 4);
        assert!(state.editor.borrow().project().tracks()[2].kind == TrackKind::Audio);
    }

    #[test]
    fn paste_after_track_deletion_keeps_surviving_destinations_and_timing() {
        let state = UiState::new();
        state.add_track(TrackKind::Video);
        assert!(state.add_title_at(Some((0, 10))));
        assert!(state.add_title_at(Some((1, 0))));
        state.select_clips(vec![(0, 1), (1, 2)]);
        state.run(Action::Copy);
        state.run_menu(MenuCommand::RemoveTrack(1));
        let before_paste = state.editor.borrow().project().clone();

        state.seek(120);
        state.run(Action::Paste);
        assert_eq!(starts(&state, 1), vec![120], "{}", state.status.peek());
        assert_eq!(starts(&state, 0), vec![10, 130]);
        assert_eq!(state.selected.peek(), vec![(4, 2), (0, 3)]);
        state.undo();
        assert_eq!(state.editor.borrow().project(), &before_paste);
    }

    #[test]
    fn paste_after_track_deletion_keeps_detached_audio_on_an_audio_track() {
        let state = UiState::new();
        assert!(state.edit(Command::ImportMedia(crate::media::Media {
            id: 1,
            path: "footage.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(3_000_000),
            has_audio: true,
        })));
        let detached = Clip {
            id: 1,
            start_frame: 0,
            source_start_frame: 15,
            duration_frames: 30,
            content: ClipContent::Media { media_id: 1 },
            volume: 0.5,
            muted: false,
            transition: None,
            fade_in_frames: 0,
            fade_out_frames: 0,
            linked_audio: false,
        };
        assert!(state.edit(Command::InsertClip {
            track_id: 3,
            clip: detached.clone(),
        }));
        state.select_clip(Some((3, 1)));
        state.run(Action::Copy);
        state.run_menu(MenuCommand::RemoveTrack(3));
        state.add_track(TrackKind::Video); // Reuses track ID 3.

        state.seek(60);
        state.run(Action::Paste);
        let editor = state.editor.borrow();
        let tracks = editor.project().tracks();
        assert_eq!(tracks[2].id, 3);
        assert!(tracks[2].clips.is_empty());
        assert_eq!(tracks[3].id, 2);
        assert_eq!(
            tracks[3].clips,
            vec![Clip {
                start_frame: 60,
                ..detached
            }],
            "{}",
            state.status.peek()
        );
    }

    #[test]
    fn dragging_clips_or_media_before_the_start_places_them_at_zero() {
        let state = UiState::new();
        state.timeline_rect.set(Rect {
            x: 0.0,
            y: 0.0,
            width: 1128.0,
            height: 300.0,
        });
        assert!(state.add_title());
        assert!(state.add_title());
        assert!(state.edit(Command::Move {
            track_id: 0,
            target_track_id: 0,
            clip_id: 2,
            to_frame: 135,
        }));
        assert!(state.edit(Command::Move {
            track_id: 0,
            target_track_id: 0,
            clip_id: 1,
            to_frame: 45,
        }));
        state.select_clips(vec![(0, 1), (0, 2)]);
        let geometry = state.timeline_geometry();
        state.drag.set(Some(DragState {
            payload: DragPayload::Clips {
                anchor: (0, 1),
                grab: 0.0,
            },
            pointer: Point {
                x: geometry.left() - 300.0,
                y: geometry.top() + 10.0,
            },
        }));
        assert!(state.drop_plan().unwrap().valid);
        state.finish_drag();
        assert_eq!(starts(&state, 0), vec![0, 90]);
        state.undo();
        assert_eq!(starts(&state, 0), vec![45, 135]);

        assert!(state.edit(Command::ImportMedia(crate::media::Media {
            id: 3,
            path: "other.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(1_000_000),
            has_audio: false,
        })));
        state.drag.set(Some(DragState {
            payload: DragPayload::Media(3),
            pointer: Point {
                x: geometry.left() - 80.0,
                y: geometry.top() + 75.0,
            },
        }));
        let plan = state.drop_plan().unwrap();
        assert!(plan.valid, "{}", plan.label);
        assert_eq!(plan.ghosts[0].start, 0);
        state.finish_drag();
        assert_eq!(starts(&state, 1), vec![0]);
    }

    #[test]
    fn busy_blocks_commands_and_history_without_losing_undo_or_redo() {
        let state = UiState::new();
        assert!(state.add_title());
        let before = state.editor.borrow().project().clone();
        state.busy.set(true);
        assert!(!state.edit(Command::RenameProject("Blocked".into())));
        state.undo();
        state.run_menu(MenuCommand::RemoveTrack(0));
        assert_eq!(state.editor.borrow().project(), &before);

        state.busy.set(false);
        state.undo();
        assert!(starts(&state, 0).is_empty());
        state.busy.set(true);
        state.redo();
        assert!(starts(&state, 0).is_empty());
        state.delete_selection(false);
        state.busy.set(false);
        state.redo();
        assert_eq!(state.editor.borrow().project(), &before);
        assert!(state.edit(Command::RenameProject("Allowed".into())));
    }

    #[test]
    fn busy_export_blocks_toolbar_delete() {
        let state = UiState::new();
        assert!(state.add_title());
        state.select_clip(Some((0, 1)));
        let before = state.editor.borrow().project().clone();
        state.exporting.set(true);
        state.busy.set(true);

        state.delete_selection(false);

        assert_eq!(state.editor.borrow().project(), &before);
        assert_eq!(state.selected.peek(), vec![(0, 1)]);
    }

    #[test]
    fn busy_export_blocks_destructive_keyboard_actions() {
        for action in [
            Action::Cut,
            Action::Paste,
            Action::Duplicate,
            Action::Split,
            Action::NudgeLeft,
            Action::NudgeRight,
        ] {
            assert_busy_export_blocks(action);
        }
    }

    #[test]
    fn busy_export_blocks_delete_shortcut() {
        assert_busy_export_blocks(Action::Delete);
    }

    #[test]
    fn busy_export_blocks_shift_delete_shortcut() {
        assert_busy_export_blocks(Action::RippleDelete);
    }

    fn assert_busy_export_blocks(action: Action) {
        let state = UiState::new();
        assert!(state.add_title());
        state.select_clip(Some((0, 1)));
        state.run(Action::Copy);
        state.seek(if action == Action::Paste { 180 } else { 45 });
        let before = state.editor.borrow().project().clone();
        state.exporting.set(true);
        state.busy.set(true);

        state.run(action);

        assert_eq!(state.editor.borrow().project(), &before, "{action:?}");
        assert_eq!(state.selected.peek(), vec![(0, 1)], "{action:?}");
    }

    #[test]
    fn keyboard_actions_edit_the_selection_as_one_undo_step() {
        let state = UiState::new();
        assert!(state.add_title());
        assert!(state.add_title());
        state.run(Action::SelectAll);
        assert_eq!(state.selected.peek().len(), 2);
        state.run(Action::Copy);
        state.seek(300);
        state.run(Action::Paste);
        assert_eq!(starts(&state, 0), vec![0, 90, 300, 390]);
        state.run(Action::Undo);
        assert_eq!(starts(&state, 0), vec![0, 90]);
        assert!(state.selected.peek().is_empty());
        state.run(Action::Redo);
        state.select_clips(vec![(0, 3), (0, 4)]);
        state.run(Action::NudgeRight);
        assert_eq!(starts(&state, 0), vec![0, 90, 301, 391]);
        state.run(Action::Cut);
        assert_eq!(starts(&state, 0), vec![0, 90]);
        state.run(Action::Escape);
        state.seek(45);
        state.run(Action::Split);
        assert_eq!(starts(&state, 0), vec![0, 45, 90]);
        state.select_clip(Some((0, 2)));
        state.run(Action::Duplicate);
        assert_eq!(starts(&state, 0), vec![0, 45, 90, 180]);
        state.run(Action::SelectAll);
        state.run(Action::RippleDelete);
        assert!(starts(&state, 0).is_empty());
        state.run(Action::ToEnd);
        state.run(Action::StepBack);
        assert_eq!(state.program.position.peek(), 0);
    }

    #[test]
    fn ripple_delete_action_splits_a_spanning_clip_and_undoes_in_one_step() {
        let state = UiState::new();
        for (track_id, id, start_frame, duration_frames) in [(0, 1, 20, 20), (1, 2, 0, 60)] {
            assert!(state.edit(Command::InsertClip {
                track_id,
                clip: Clip {
                    id,
                    start_frame,
                    source_start_frame: 0,
                    duration_frames,
                    content: ClipContent::Title(crate::timeline::Title::default()),
                    volume: 1.0,
                    muted: false,
                    transition: None,
                    fade_in_frames: 0,
                    fade_out_frames: 0,
                    linked_audio: false,
                },
            }));
        }
        state.select_clip(Some((0, 1)));
        state.run(Action::RippleDelete);
        assert!(starts(&state, 0).is_empty());
        assert_eq!(starts(&state, 1), vec![0, 20]);
        assert!(state.selected.peek().is_empty());
        state.run(Action::Undo);
        assert_eq!(starts(&state, 0), vec![20]);
        assert_eq!(starts(&state, 1), vec![0]);
        state.run(Action::Redo);
        assert_eq!(starts(&state, 1), vec![0, 20]);
    }

    #[test]
    fn markers_toggle_navigate_snap_and_open_from_the_ruler() {
        let state = UiState::new();
        state.timeline_rect.set(Rect {
            x: 0.0,
            y: 0.0,
            width: 1128.0,
            height: 300.0,
        });
        assert!(state.edit(Command::ImportMedia(Media {
            id: 1,
            path: "scene.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(4_000_000),
            has_audio: false,
        })));
        state.insert_media(1);
        let markers = |state: &UiState| {
            state
                .editor
                .borrow()
                .project()
                .markers()
                .iter()
                .map(|marker| marker.frame)
                .collect::<Vec<_>>()
        };
        for frame in [40, 10] {
            state.seek(frame);
            state.run(Action::ToggleMarker);
        }
        assert_eq!(markers(&state), [10, 40]);
        state.seek(0);
        state.run(Action::NextMarker);
        assert_eq!(state.program.position.peek(), 10);
        state.run(Action::NextMarker);
        assert_eq!(state.program.position.peek(), 40);
        state.run(Action::NextMarker);
        assert_eq!(state.program.position.peek(), 40);
        assert_eq!(state.status.peek(), "There is no marker after the playhead");
        state.run(Action::PreviousMarker);
        assert_eq!(state.program.position.peek(), 10);
        state.run(Action::ToggleMarker);
        assert_eq!(markers(&state), [40]);
        state.run(Action::Undo);
        assert_eq!(markers(&state), [10, 40]);

        state.seek(90);
        let edges = edits::selection_edges(state.editor.borrow().project(), &[(0, 1)]);
        assert_eq!(
            edits::snap(
                state.editor.borrow().project(),
                &[(0, 1)],
                &[edges[0] + 38],
                90,
                3
            ),
            2
        );

        let geometry = state.timeline_geometry();
        let x = |frame: u64| {
            geometry.left()
                + (frame - state.timeline_start.peek()) as f32 * geometry.pixels_per_frame()
        };
        let labels = |state: &UiState| {
            state
                .menu
                .peek()
                .map(|menu| {
                    menu.entries
                        .iter()
                        .filter_map(|entry| {
                            entry.command.map(|command| (entry.label.clone(), command))
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        state.open_timeline_menu(Point {
            x: x(40) + 3.0,
            y: 10.0,
        });
        assert_eq!(
            labels(&state),
            [
                ("Go to marker".to_string(), MenuCommand::Seek(40)),
                ("Remove marker".to_string(), MenuCommand::RemoveMarker(40)),
            ]
        );
        state.run_menu(MenuCommand::RemoveMarker(40));
        assert_eq!(markers(&state), [10]);
        state.open_timeline_menu(Point { x: x(70), y: 10.0 });
        assert_eq!(
            labels(&state),
            [("Add marker here".to_string(), MenuCommand::AddMarker(70))]
        );
        state.run_menu(MenuCommand::AddMarker(70));
        assert_eq!(markers(&state), [10, 70]);
    }

    #[test]
    fn right_click_menus_offer_clip_gap_and_track_actions() {
        use crate::media::Media;
        use creamui::core::Rect;
        let state = UiState::new();
        state.timeline_rect.set(Rect {
            x: 0.0,
            y: 0.0,
            width: 1128.0,
            height: 300.0,
        });
        assert!(state.edit(Command::ImportMedia(Media {
            id: 1,
            path: "scene.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(2_000_000),
            has_audio: true,
        })));
        state.insert_media(1);
        state.select_clip(None);
        let labels = |state: &UiState| {
            state
                .menu
                .peek()
                .map(|menu| {
                    menu.entries
                        .iter()
                        .filter(|entry| entry.command.is_some())
                        .map(|entry| entry.label.clone())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        let clip_point = Point {
            x: 128.0 + 50.0,
            y: 50.0,
        };
        state.open_timeline_menu(clip_point);
        assert_eq!(state.selected.peek(), vec![(0, 1)]);
        assert!(labels(&state).contains(&"Detach audio".to_string()));
        assert!(labels(&state).contains(&"Mute audio".to_string()));
        state.run_menu(MenuCommand::ToggleMute);
        assert!(state.editor.borrow().project().tracks()[0].clips[0].muted);
        state.run_menu(MenuCommand::DetachAudio);
        assert!(state.menu.peek().is_none());
        assert_eq!(starts(&state, 2), vec![0]);
        assert!(state.editor.borrow().project().tracks()[2].clips[0].muted);

        state.open_timeline_menu(Point {
            x: 128.0 + 900.0,
            y: 50.0,
        });
        let gap = labels(&state);
        assert!(gap.contains(&"Add title here".to_string()));
        assert!(gap.contains(&"Delete track V1".to_string()));
        state.run(Action::Escape);
        assert!(state.menu.peek().is_none());

        state.run_menu(MenuCommand::AddTrack(TrackKind::Audio));
        assert_eq!(state.editor.borrow().project().tracks().len(), 5);
        state.run_menu(MenuCommand::RemoveTrack(2));
        assert_eq!(state.editor.borrow().project().tracks().len(), 4);
        assert!(state.selected.peek().iter().all(|(track, _)| *track != 2));
        state.run(Action::Undo);
        assert_eq!(state.editor.borrow().project().tracks().len(), 5);

        state.open_media_menu(1, Point::default());
        let media = labels(&state);
        assert_eq!(media[0], "Insert at end");
        assert!(
            !state.menu.peek().unwrap().entries.last().unwrap().enabled,
            "a source in use cannot be removed"
        );
    }
}
