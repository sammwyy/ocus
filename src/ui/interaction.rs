//! Pointer gestures, drag and drop, keyboard shortcuts and the clip edits
//! they trigger. Everything here is transient editor state; project changes
//! still go through [`UiState::edit`].

use super::UiState;
use super::controls::Glyph;
use super::edits::{self, ClipRef};
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

/// Every keyboard shortcut. They are inactive while a text field has focus.
pub(crate) fn shortcuts() -> Vec<(Shortcut, Action)> {
    use Action::*;
    let char = Key::Char;
    [
        (char('z'), true, false, Undo),
        (char('z'), true, true, Redo),
        (char('y'), true, false, Redo),
        (Key::Delete, false, false, Delete),
        (Key::Backspace, false, false, Delete),
        (Key::Delete, false, true, RippleDelete),
        (Key::Backspace, false, true, RippleDelete),
        (char('a'), true, false, SelectAll),
        (Key::Escape, false, false, Escape),
        (char('c'), true, false, Copy),
        (char('x'), true, false, Cut),
        (char('v'), true, false, Paste),
        (char('d'), true, false, Duplicate),
        (char('k'), true, false, Split),
        (char('s'), false, false, Split),
        (char(' '), false, false, PlayPause),
        (char('k'), false, false, PlayPause),
        (Key::Left, false, false, StepBack),
        (Key::Right, false, false, StepForward),
        (Key::Left, false, true, SecondBack),
        (Key::Right, false, true, SecondForward),
        (Key::Home, false, false, ToStart),
        (Key::End, false, false, ToEnd),
        (Key::Left, false, false, NudgeLeft),
        (Key::Right, false, false, NudgeRight),
        (char('='), false, false, ZoomIn),
        (char('+'), false, false, ZoomIn),
        (char('+'), false, true, ZoomIn),
        (char('-'), false, false, ZoomOut),
        (char('t'), false, false, AddTitle),
        (char('n'), true, false, New),
        (char('o'), true, false, Open),
        (char('s'), true, false, Save),
        (char('s'), true, true, SaveAs),
        (char('i'), true, false, Import),
        (char('e'), true, false, Export),
    ]
    .into_iter()
    .map(|(key, ctrl, shift, action)| {
        let alt = matches!(action, NudgeLeft | NudgeRight);
        (with(key, ctrl, shift, alt), action)
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
        let busy = self.busy.peek();
        let fps = self.frame_rate();
        let second = (u64::from(fps.0) / u64::from(fps.1.max(1))).max(1);
        let frame = self.program.position.peek();
        let last = self.sequence_end().saturating_sub(1);
        match action {
            Action::Undo if !busy => self.undo(),
            Action::Redo if !busy => self.redo(),
            Action::Delete => self.delete_selection(false),
            Action::RippleDelete => self.delete_selection(true),
            Action::SelectAll => self.select_all(),
            Action::Escape => {
                if self.menu.peek().is_some() {
                    self.close_menu();
                } else if self.drag.peek().is_some() {
                    self.cancel_drag();
                } else {
                    self.select_clip(None);
                }
            }
            Action::Copy => {
                self.copy_selection();
            }
            Action::Cut => {
                if self.copy_selection() {
                    self.delete_selection(false);
                }
            }
            Action::Paste => self.paste(None),
            Action::Duplicate => self.duplicate_selection(),
            Action::Split => self.split_at_playhead(),
            Action::PlayPause => {
                if self.source.playing.peek() {
                    self.pause_source();
                } else if self.program.playing.peek() {
                    self.pause();
                } else {
                    self.play();
                }
            }
            Action::StepBack => self.seek(frame.saturating_sub(1)),
            Action::StepForward => self.seek((frame + 1).min(last)),
            Action::SecondBack => self.seek(frame.saturating_sub(second)),
            Action::SecondForward => self.seek((frame + second).min(last)),
            Action::ToStart => self.seek(0),
            Action::ToEnd => self.seek(last),
            Action::NudgeLeft => self.nudge_selection(-1),
            Action::NudgeRight => self.nudge_selection(1),
            Action::ZoomIn => self.zoom_timeline(self.timeline_zoom.peek().saturating_sub(1)),
            Action::ZoomOut => self.zoom_timeline(self.timeline_zoom.peek() + 1),
            Action::AddTitle if !busy => {
                self.add_title();
            }
            Action::New => self.new_project(),
            Action::Open => self.open_dialog(),
            Action::Save => self.save(),
            Action::SaveAs => self.save_as_dialog(),
            Action::Import => self.import_dialog(),
            Action::Export if !self.exporting.peek() => self.export_dialog(),
            _ => {}
        }
    }

    /// Moves the program playhead, pausing playback first.
    pub fn seek(&self, frame: u64) {
        if self.program.playing.peek() {
            self.pause();
        }
        if self.program.position.peek() != frame {
            self.focus_frame(frame);
        }
    }

    pub fn add_title(&self) -> bool {
        self.add_title_at(None)
    }

    pub fn add_title_at(&self, placement: Option<(u64, u64)>) -> bool {
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
                .set("Ripple delete would cut through a clip spanning the gap".into()),
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
                    .map(|clip| (track.id, clip.clone()))
            })
            .collect();
        drop(editor);
        if copied.is_empty() {
            return false;
        }
        copied.sort_by_key(|(_, clip)| clip.start_frame);
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
                .map(|(_, clip)| clip.start_frame + clip.duration_frames)
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
                let start = pointer_frame.max(0.0).round() as i64;
                let start = (start
                    + edits::snap(
                        project,
                        &[],
                        &[start, start + duration as i64],
                        playhead,
                        tolerance,
                    ))
                .max(0) as u64;
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
                let delta = delta + edits::snap(project, &selection, &edges, playhead, tolerance);
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
                let track = self
                    .editor
                    .borrow()
                    .project()
                    .tracks()
                    .iter()
                    .find_map(|track| {
                        edits::compatible_track(self.editor.borrow().project(), media_id, track.id)
                    });
                if let Some(track) = track {
                    self.insert_media_at(media_id, Some((track, frame)));
                }
            }
            MenuCommand::ShowMedia(media_id) => self.select_media(media_id),
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
    fn audible_selection(&self) -> (Vec<ClipRef>, bool) {
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
    use std::collections::HashSet;

    fn starts(state: &UiState, track: usize) -> Vec<u64> {
        state.editor.borrow().project().tracks()[track]
            .clips
            .iter()
            .map(|clip| clip.start_frame)
            .collect()
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
        assert_eq!(bound(Key::Char(' '), false, false), Some(Action::PlayPause));
        assert_eq!(bound(Key::Char('a'), true, false), Some(Action::SelectAll));
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
