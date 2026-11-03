//! Pointer gestures, drag and drop, keyboard shortcuts and the clip edits
//! they trigger. Everything here is transient editor state; project changes
//! still go through [`UiState::edit`].

use super::UiState;
use super::edits::{self, ClipRef};
use super::timeline_view::{Geometry, Ghost};
use crate::media::MediaKind;
use crate::project::Command;
use crate::timeline::TrackKind;
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
                if self.drag.peek().is_some() {
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
        let Some((track_id, clip)) = edits::title_clip(self.editor.borrow().project()) else {
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
        let kinds: Vec<_> = self
            .editor
            .borrow()
            .project()
            .tracks()
            .iter()
            .map(|track| track.kind)
            .collect();
        Geometry::new(
            self.timeline_rect.get(),
            &kinds,
            self.timeline_start.peek(),
            self.visible_frames(),
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
        state.run(Action::Redo);
        assert_eq!(state.selected.peek().len(), 2);
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
}
