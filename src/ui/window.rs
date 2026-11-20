use super::controls::{
    AUDIO, DragOverlay, ExportMeter, Glyph, Help, IMAGE, Layer, NumericSpec, PLAYHEAD, STAGE,
    Scrubber, TITLE, Thumb, VIDEO, ZoomSlider, accent, accent_button, background, border, chip,
    enabled_chip, faint, fit, glyph, heading, icon_button, kind_color, list_row, media_item,
    menu_row, muted, numeric_row, numeric_row_preview, panel, panel_raised, play_button,
    secondary_button, segment, short_duration, sized_icon_button, surface, surface_hover, text,
    text_color, timecode, tool_button, with_help,
};
use super::edits::ClipRef;
use super::interaction::{Action, BarMenu, DragPayload, MenuState};
use super::state::{
    ChromaGesture, ClipDetail, ExportPage, LibraryTab, MediaView, PlaybackMonitor, PreviewState,
    SequenceContent, SettingsPage, SourceRange, TitleGesture, ZOOM_SECONDS, media_frames,
};
use super::timeline_view::{ClipLook, Lane, LaneClip, TimelineView};
use super::waveform::WaveformSlice;
use super::{MediaFilter, Sidebar, UiState};
use super::{title_fonts, title_markup};
use crate::export::queue::{Queue, Status as ExportStatus};
use crate::media::audio::carries_sound;
use crate::media::composite::title_outline_radius;
use crate::media::{Media, MediaKind};
use crate::project::export_settings::{AudioCodec, Container, EncodingSpeed, VideoCodec};
use crate::project::{Command, Project};
use crate::settings::ThemeMode;
use crate::timeline::effects::{EffectCategory, EffectKind};
use crate::timeline::{
    Clip, ClipContent, ColorAdjust, Mask, MaskShape, Playback, SlideDirection, Title, Track,
    TrackKind, Transform, Transition, TransitionKind,
};
use creamui::core::layout::{Dimension, Style};
use creamui::core::{Painter, Point, Rect, TextAlign, Widget};
use creamui::fonts::{FontWeight, HorizontalAlign, LayoutSettings, cached_layout, resolve};
use creamui::widgets::layout::Flex;
use creamui::widgets::layout::{Align, Grid, Justify, StyleExt};
use creamui::widgets::{
    AppTitlebar, Checkbox, ColorPicker, RawButton, RawLink, RawScrollView, TextArea,
    TextController, TextInput,
};
use creamui::{BoxedWidget, Color, GpuFrameSlot, GpuViewport, ImageData, Size, StateStyle, Styled};
use std::collections::HashSet;
use std::process::Command as ProcessCommand;
use std::rc::Rc;
use std::sync::OnceLock;

fn logo() -> ImageData {
    static LOGO: OnceLock<ImageData> = OnceLock::new();
    LOGO.get_or_init(|| {
        ImageData::from_bytes(include_bytes!("../../assets/ocus-video-logo-dark-64.png"))
            .expect("bundled Ocus logo must be a PNG")
    })
    .clone()
}

struct Logo;

struct MenuTitle {
    state: UiState,
    menu: BarMenu,
    rect: Rc<std::cell::Cell<Rect>>,
}

impl Widget for MenuTitle {
    fn paint(&self, _painter: &mut dyn Painter, _rect: Rect) {}

    fn style(&self) -> creamui::Style {
        Style::default().size(44.0, 28.0).into()
    }

    fn reports_layout(&self) -> bool {
        true
    }

    fn on_layout(&self, rect: Rect) -> bool {
        self.rect.set(rect);
        false
    }

    fn children(&mut self) -> Vec<BoxedWidget> {
        let state = self.state.clone();
        let menu = self.menu;
        let rect = self.rect.clone();
        let active = state.menu.peek().is_some_and(|open| open.bar == Some(menu));
        vec![Box::new(
            RawButton::new(
                Style::default()
                    .size(44.0, 28.0)
                    .align(Align::Center)
                    .justify(Justify::Center),
                move || {
                    let rect = rect.get();
                    state.show_bar_menu(
                        menu,
                        Point {
                            x: rect.x,
                            y: rect.y + rect.height + 8.0,
                        },
                        true,
                    );
                },
            )
            .background(if active { surface_hover() } else { panel() })
            .corner_radius(4.0)
            .hover_style(StateStyle::new().background(surface_hover()))
            .focus_style(StateStyle::new().outline(accent(), 2.0))
            .child(text(menu.label(), text_color(), 12.0)),
        )]
    }

    fn on_hover(&self) -> Option<Rc<dyn Fn(bool)>> {
        let state = self.state.clone();
        let menu = self.menu;
        let rect = self.rect.clone();
        Some(Rc::new(move |hovered| {
            if hovered && !state.menu.peek().is_some_and(|open| open.bar == Some(menu)) {
                let rect = rect.get();
                state.show_bar_menu(
                    menu,
                    Point {
                        x: rect.x,
                        y: rect.y + rect.height + 8.0,
                    },
                    false,
                );
            }
        }))
    }
}

impl Widget for Logo {
    fn style(&self) -> creamui::Style {
        Style::default().size(26.0, 26.0).into()
    }

    fn paint(&self, painter: &mut dyn Painter, rect: Rect) {
        painter.draw_image(rect, logo().image(), None);
    }
}

struct SourceWaveform {
    waveform: WaveformSlice,
    position: u64,
}

impl Widget for SourceWaveform {
    fn style(&self) -> creamui::Style {
        fill_style().into()
    }

    fn paint(&self, painter: &mut dyn Painter, rect: Rect) {
        let width = rect.width - 32.0;
        let height = (rect.height - 52.0).min(152.0);
        if width < 4.0 || height < 4.0 {
            return;
        }
        let wave = Rect {
            x: rect.x + 16.0,
            y: rect.y + (rect.height - height + 20.0) * 0.5,
            width,
            height,
        };
        painter.fill_text(
            Rect {
                y: wave.y - 26.0,
                height: 18.0,
                ..wave
            },
            "Audio source",
            muted(),
            11.0,
            TextAlign::Start,
        );
        self.waveform.paint(painter, wave, wave, 1.0, AUDIO);
        let progress = (self.position as f64 / self.waveform.clip.duration_frames.max(1) as f64)
            .clamp(0.0, 1.0);
        painter.fill_rect(
            Rect {
                x: wave.x + (wave.width - 1.0) * progress as f32,
                width: 1.0,
                ..wave
            },
            VIDEO,
            0.0,
        );
    }
}

fn selected_clip(state: &UiState) -> Option<(u64, Clip)> {
    let (track_id, clip_id) = state.selected.get().last().copied()?;
    state
        .editor
        .borrow()
        .project()
        .tracks()
        .iter()
        .find(|track| track.id == track_id)?
        .clips
        .iter()
        .find(|clip| clip.id == clip_id)
        .map(|clip| (track_id, clip.clone()))
}

fn edit_selection(state: &UiState, edit: impl FnOnce(&Clip, u64, u64) -> Command) {
    let Some((track_id, clip)) = selected_clip(state) else {
        return;
    };
    let next_id = state
        .editor
        .borrow()
        .project()
        .tracks()
        .iter()
        .flat_map(|t| &t.clips)
        .map(|clip| clip.id)
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    let command = edit(&clip, track_id, next_id);
    state.edit(command);
}

fn set_title_text(state: &UiState) {
    change_title(state, |title| title.text = state.title_text.peek());
}

fn timing_steps(state: &UiState, source: bool) -> BoxedWidget {
    let Some((track_id, clip)) = selected_clip(state) else {
        return text("No clip", muted(), 11.0);
    };
    let fps = state.frame_rate();
    let seconds = f64::from(fps.1) / f64::from(fps.0);
    let editor = state.editor.borrow();
    let project = editor.project();
    let source_frames = match clip.content {
        ClipContent::Media { media_id } => project
            .media()
            .iter()
            .find(|media| media.id == media_id && media.kind != MediaKind::Image)
            .and_then(|media| media_frames(media, fps)),
        _ => None,
    };
    let max = if source {
        source_frames
            .map(|frames| frames.saturating_sub(clip.source_span()))
            .unwrap_or(u64::MAX - clip.source_span())
    } else {
        let next = project
            .tracks()
            .iter()
            .find(|track| track.id == track_id)
            .and_then(|track| {
                track
                    .clips
                    .iter()
                    .filter(|c| c.start_frame > clip.start_frame)
                    .map(|c| c.start_frame - clip.start_frame)
                    .min()
            });
        let available = match clip.playback {
            Playback::Freeze => None,
            Playback::Reverse => {
                Some(clip.timeline_span(clip.source_start_frame.saturating_add(clip.source_span())))
            }
            Playback::Forward => source_frames
                .map(|frames| clip.timeline_span(frames.saturating_sub(clip.source_start_frame))),
        };
        next.into_iter()
            .chain(available)
            .min()
            .unwrap_or(u64::MAX - clip.start_frame)
    };
    drop(editor);
    numeric_row(
        state,
        NumericSpec {
            label: if source { "Source in" } else { "Length" },
            min: if source { 0.0 } else { seconds },
            max: max as f64 * seconds,
            slider_max: Some((clip.duration_frames as f64 * seconds * 2.0).max(60.0)),
            step: seconds,
            precision: 3,
            unit: "s",
            default: if source { 0.0 } else { 1.0 },
        },
        if source {
            clip.source_start_frame
        } else {
            clip.duration_frames
        } as f64
            * seconds,
        true,
        move |state, value| {
            if let Some((track_id, clip)) = selected_clip(state) {
                let frame = (value / seconds).round() as u64;
                let editor = state.editor.borrow();
                let command = if source {
                    Some(Command::Trim {
                        track_id,
                        clip_id: clip.id,
                        start_frame: clip.start_frame,
                        source_start_frame: frame,
                        duration_frames: clip.duration_frames,
                    })
                } else {
                    crate::project::edit::trim_edge(
                        editor.project(),
                        track_id,
                        clip.id,
                        false,
                        clip.start_frame.saturating_add(frame.max(1)),
                    )
                };
                drop(editor);
                if let Some(command) = command {
                    state.edit(command);
                }
            }
        },
    )
}

fn selected_video_clip(state: &UiState) -> Option<(u64, Clip)> {
    if state.busy.peek() {
        return None;
    }
    let (track_id, clip) = selected_clip(state)?;
    state
        .editor
        .borrow()
        .project()
        .tracks()
        .iter()
        .any(|track| track.id == track_id && track.kind == TrackKind::Video)
        .then_some((track_id, clip))
}

fn transition_frames(state: &UiState, seconds: f64) -> u64 {
    let fps = state.frame_rate();
    (seconds * f64::from(fps.0) / f64::from(fps.1))
        .round()
        .max(1.0) as u64
}

fn clip_transition(clip: &Clip, out: bool) -> Option<&Transition> {
    if out {
        clip.transition_out.as_ref()
    } else {
        clip.transition.as_ref()
    }
}

fn edit_transition(
    state: &UiState,
    track_id: u64,
    clip_id: u64,
    out: bool,
    transition: Option<Transition>,
) {
    state.edit(if out {
        Command::SetTransitionOut {
            track_id,
            clip_id,
            transition,
        }
    } else {
        Command::SetTransition {
            track_id,
            clip_id,
            transition,
        }
    });
}

fn set_transition_kind(state: &UiState, out: bool, kind: Option<TransitionKind>) {
    let Some((track_id, clip)) = selected_video_clip(state) else {
        return;
    };
    let current = clip_transition(&clip, out);
    let transition = kind.map(|kind| Transition {
        direction: current.map_or(SlideDirection::Right, |transition| transition.direction),
        kind,
        duration_frames: current
            .map_or_else(
                || transition_frames(state, 0.5),
                |transition| transition.duration_frames,
            )
            .clamp(1, clip.duration_frames),
    });
    edit_transition(state, track_id, clip.id, out, transition);
}

fn set_slide_direction(state: &UiState, out: bool, direction: SlideDirection) {
    let Some((track_id, clip)) = selected_video_clip(state) else {
        return;
    };
    let Some(mut transition) = clip_transition(&clip, out).cloned() else {
        return;
    };
    if transition.kind != TransitionKind::Slide {
        return;
    }
    transition.direction = direction;
    edit_transition(state, track_id, clip.id, out, Some(transition));
}

fn transition_row(state: &UiState, clip: &Clip, busy: bool, out: bool) -> BoxedWidget {
    let transition = clip_transition(clip, out);
    let current = transition.map(|transition| transition.kind);
    let mut choices = Flex::row().gap(2.0).align(Align::Center).child(Box::new(
        Flex::row()
            .width(22.0)
            .child(heading(if out { "Out" } else { "In" }, muted(), 11.0)),
    ));
    for (label, kind, help) in [
        ("None", None, "Remove this transition"),
        (
            "Fade",
            Some(TransitionKind::Fade),
            if out {
                "Fade out to reveal the track below, or black"
            } else {
                "Fade in; dip through black between adjacent clips"
            },
        ),
        (
            "Dissolve",
            Some(TransitionKind::Dissolve),
            if out {
                "Blend away to reveal the track below, or black"
            } else {
                "Blend smoothly into this clip"
            },
        ),
        (
            "Slide",
            Some(TransitionKind::Slide),
            if out {
                "Slide this clip out toward an edge"
            } else {
                "Slide this clip in from an edge"
            },
        ),
    ] {
        choices = choices.child(with_help(
            chip(label, current == kind, state, move |state| {
                set_transition_kind(state, out, kind)
            }),
            Help::new(help),
            state,
        ));
    }
    let mut row = Flex::column().gap(4.0).child(Box::new(choices));
    if let Some(transition) = transition {
        let duration = transition.duration_frames.min(clip.duration_frames);
        let fps = state.frame_rate();
        let seconds = f64::from(fps.1) / f64::from(fps.0);
        row = row.child(numeric_row(
            state,
            NumericSpec {
                label: "Duration",
                min: seconds,
                max: clip.duration_frames as f64 * seconds,
                slider_max: None,
                step: seconds,
                precision: 3,
                unit: "s",
                default: 0.5,
            },
            duration as f64 * seconds,
            !busy,
            move |state, value| {
                if let Some((track_id, clip)) = selected_video_clip(state) {
                    let current = if out {
                        clip.transition_out
                    } else {
                        clip.transition
                    };
                    if let Some(mut transition) = current {
                        transition.duration_frames =
                            ((value / seconds).round() as u64).clamp(1, clip.duration_frames);
                        state.edit(if out {
                            Command::SetTransitionOut {
                                track_id,
                                clip_id: clip.id,
                                transition: Some(transition),
                            }
                        } else {
                            Command::SetTransition {
                                track_id,
                                clip_id: clip.id,
                                transition: Some(transition),
                            }
                        });
                    }
                }
            },
        ));
        if transition.kind == TransitionKind::Slide {
            let mut directions = Flex::row().gap(2.0).align(Align::Center).child(Box::new(
                Flex::row().width(44.0).child(text(
                    if out { "Toward" } else { "From" },
                    muted(),
                    11.0,
                )),
            ));
            for (label, direction, from, toward) in [
                ("←", SlideDirection::Left, "From left", "Toward left"),
                ("→", SlideDirection::Right, "From right", "Toward right"),
                ("↑", SlideDirection::Up, "From top", "Toward top"),
                ("↓", SlideDirection::Down, "From bottom", "Toward bottom"),
            ] {
                directions = directions.child(with_help(
                    chip(
                        label,
                        transition.direction == direction,
                        state,
                        move |state| set_slide_direction(state, out, direction),
                    ),
                    Help::new(if out { toward } else { from }),
                    state,
                ));
            }
            row = row.child(Box::new(directions));
        }
    }
    Box::new(row)
}

#[cfg(test)]
fn transition_section(state: &UiState, clip: &Clip, busy: bool) -> BoxedWidget {
    let mut rows = vec![
        transition_row(state, clip, busy, false),
        transition_row(state, clip, busy, true),
    ];
    let incoming = clip.transition.as_ref().map_or(0, |transition| {
        transition.duration_frames.min(clip.duration_frames)
    });
    let outgoing = clip.transition_out.as_ref().map_or(0, |transition| {
        transition.duration_frames.min(clip.duration_frames)
    });
    if incoming > clip.duration_frames - outgoing {
        rows.push(text("Shortened to fit without overlap.", muted(), 11.0));
    }
    section("Transition", rows)
}

fn set_title_duration(state: &UiState) {
    let Some((track_id, clip)) = selected_clip(state) else {
        return;
    };
    if !matches!(clip.content, ClipContent::Title(_)) {
        return;
    }
    let seconds = state.title_duration.peek().trim().parse::<f64>();
    let fps = state.frame_rate();
    let frames = seconds
        .ok()
        .map(|value| value * f64::from(fps.0) / f64::from(fps.1));
    let Some(frames) =
        frames.filter(|frames| frames.is_finite() && *frames >= 0.5 && *frames < u64::MAX as f64)
    else {
        state
            .status
            .set("Enter a title duration of at least one frame in seconds".into());
        return;
    };
    let frames = frames.round() as u64;
    if frames != clip.duration_frames
        && state.edit(Command::Trim {
            track_id,
            clip_id: clip.id,
            start_frame: clip.start_frame,
            source_start_frame: clip.source_start_frame,
            duration_frames: frames,
        })
    {
        state.sync_title_duration(true);
    }
}

fn change_title(state: &UiState, change: impl FnOnce(&mut Title)) {
    let Some((track_id, selected)) = selected_clip(state) else {
        return;
    };
    let ClipContent::Title(mut title) = selected.content else {
        return;
    };
    let old = title.clone();
    change(&mut title);
    if title != old {
        let subtitles = state
            .editor
            .borrow()
            .project()
            .tracks()
            .iter()
            .any(|track| track.id == track_id && track.subtitles);
        state.edit(
            if subtitles
                && title.text == old.text
                && title.parse_html == old.parse_html
                && title.parse_markdown == old.parse_markdown
            {
                Command::SetSubtitleStyle {
                    track_id,
                    style: title,
                }
            } else {
                Command::SetTitle {
                    track_id,
                    clip_id: selected.id,
                    title,
                }
            },
        );
    }
}

fn title_toggle(
    state: &UiState,
    label: &str,
    checked: bool,
    change: impl Fn(&mut Title) + 'static,
) -> BoxedWidget {
    let state = state.clone();
    Box::new(
        Flex::row()
            .gap(8.0)
            .align(Align::Center)
            .child(Box::new(Checkbox::new(checked, move || {
                change_title(&state, &change)
            })))
            .child(text(label, text_color(), 11.0)),
    )
}

fn parse_hex_color(value: &str) -> Option<Color> {
    let value = value.trim();
    let hex = value.strip_prefix('#')?;
    (matches!(hex.len(), 3 | 4 | 6 | 8) && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| value.parse().ok())
        .flatten()
}

fn apply_title_background(state: &UiState) {
    let Some(color) = parse_hex_color(&state.title_background.peek()) else {
        state
            .status
            .set("Enter a background color such as #000000B3".into());
        return;
    };
    change_title(state, |title| {
        title.background = Some([color.r, color.g, color.b, color.a])
    });
}

fn apply_title_style(state: &UiState) {
    let family = state.title_font.peek();
    let size = state.title_size.peek().trim().parse::<f32>();
    let color = parse_hex_color(&state.title_color.peek());
    let (Ok(size), Some(color)) = (size, color) else {
        state
            .status
            .set("Enter a font size and a color such as #FFAA00".into());
        return;
    };
    if family.trim().is_empty() || !size.is_finite() || size <= 0.0 {
        state
            .status
            .set("Enter a font family and a positive font size".into());
        return;
    }
    change_title(state, |title| {
        title.font_family = family;
        title.font_size = size;
        title.color = [color.r, color.g, color.b, color.a];
    });
}

fn set_clip_audio(state: &UiState, change: impl FnOnce(&Clip) -> (f32, bool)) {
    if let Some((track_id, clip)) = selected_clip(state) {
        let (volume, muted) = change(&clip);
        state.edit(Command::SetClipAudio {
            track_id,
            clip_id: clip.id,
            volume,
            muted,
        });
    }
}

fn set_clip_transform(state: &UiState, animated: bool, change: impl FnOnce(&mut Transform)) {
    if let Some((track_id, clip)) = selected_clip(state) {
        let position = state.program.position.get();
        if animated && !clip.keyframes.is_empty() {
            let Some(frame) = keyframe_offset(&clip, position) else {
                return;
            };
            let mut transform = clip.transform_at(position);
            change(&mut transform);
            let mut keyframes = clip.keyframes.clone();
            upsert_keyframe(
                &mut keyframes,
                crate::timeline::Keyframe::new(frame, transform),
            );
            if state.edit(Command::SetClipKeyframes {
                track_id,
                clip_id: clip.id,
                keyframes,
            }) {
                state.request_preview();
            }
            return;
        }
        let mut transform = clip.transform;
        change(&mut transform);
        if state.edit(Command::SetClipTransform {
            track_id,
            clip_id: clip.id,
            transform,
        }) {
            state.request_preview();
        }
    }
}

fn keyframe_offset(clip: &Clip, position: u64) -> Option<u64> {
    (position >= clip.start_frame && position - clip.start_frame < clip.duration_frames)
        .then(|| position - clip.start_frame)
}

fn replace_clip_transform(state: &UiState, transform: Transform) {
    let Some((track_id, selected)) = selected_clip(state) else {
        return;
    };
    let mut tracks = state.editor.borrow().project().tracks().to_vec();
    let Some(clip) = tracks
        .iter_mut()
        .find(|track| track.id == track_id)
        .and_then(|track| track.clips.iter_mut().find(|clip| clip.id == selected.id))
    else {
        return;
    };
    clip.transform = transform;
    clip.keyframes.clear();
    if state.edit(Command::ReplaceTracks(tracks)) {
        state.request_preview();
    }
}

fn upsert_keyframe(
    keyframes: &mut Vec<crate::timeline::Keyframe>,
    keyframe: crate::timeline::Keyframe,
) {
    match keyframes.binary_search_by_key(&keyframe.frame, |key| key.frame) {
        Ok(index) => {
            keyframes[index] = crate::timeline::Keyframe {
                easing: keyframes[index].easing,
                ..keyframe
            }
        }
        Err(index) => keyframes.insert(index, keyframe),
    }
}

fn easing_segment(clip: &Clip, position: u64) -> Option<usize> {
    let frame = keyframe_offset(clip, position)?;
    clip.keyframes
        .windows(2)
        .position(|pair| pair[0].frame <= frame && frame < pair[1].frame)
}

fn set_keyframe_easing(state: &UiState, easing: crate::timeline::Easing) {
    let Some((track_id, clip)) = selected_clip(state) else {
        return;
    };
    let Some(index) = easing_segment(&clip, state.program.position.get()) else {
        return;
    };
    let mut keyframes = clip.keyframes.clone();
    keyframes[index].easing = easing;
    if state.edit(Command::SetClipKeyframes {
        track_id,
        clip_id: clip.id,
        keyframes,
    }) {
        state.request_preview();
    }
}

fn toggle_keyframe(state: &UiState) {
    let Some((track_id, clip)) = selected_clip(state) else {
        return;
    };
    let position = state.program.position.get();
    let Some(frame) = keyframe_offset(&clip, position) else {
        return;
    };
    let mut keyframes = clip.keyframes.clone();
    match keyframes.binary_search_by_key(&frame, |key| key.frame) {
        Ok(index) => {
            keyframes.remove(index);
        }
        Err(index) => keyframes.insert(
            index,
            crate::timeline::Keyframe::new(frame, clip.transform_at(position)),
        ),
    }
    if state.edit(Command::SetClipKeyframes {
        track_id,
        clip_id: clip.id,
        keyframes,
    }) {
        state.request_preview();
    }
}

fn neighbouring_keyframe(clip: &Clip, position: u64, next: bool) -> Option<u64> {
    let source = keyframe_offset(clip, position)?;
    let mut frames = clip
        .keyframes
        .iter()
        .map(|key| key.frame)
        .filter(|frame| *frame < clip.duration_frames);
    let frame = if next {
        frames.find(|frame| *frame > source)
    } else {
        frames.rfind(|frame| *frame < source)
    }?;
    Some(clip.start_frame + frame)
}

fn picture_in_picture_section(state: &UiState, current: Transform, editable: bool) -> BoxedWidget {
    let keyed = selected_clip(state).is_some_and(|(_, clip)| !clip.keyframes.is_empty());
    const SCALE: f32 = 0.30;
    const MARGIN: f32 = 0.04;
    let offset = (1.0 - SCALE) / 2.0 - MARGIN;
    let corners = [
        ("Top left", -offset, -offset),
        ("Top right", offset, -offset),
        ("Bottom left", -offset, offset),
        ("Bottom right", offset, offset),
    ];
    section(
        "Picture-in-picture",
        vec![
            text(
                "Replaces the transform and clears keyframes.",
                muted(),
                11.0,
            ),
            Box::new(
                Grid::new().columns(2).gap(6.0).with_children(
                    corners
                        .into_iter()
                        .map(|(label, x, y)| {
                            let preset = Transform {
                                scale: SCALE,
                                x,
                                y,
                                ..Transform::default()
                            };
                            secondary_button(
                                None,
                                label,
                                editable && (current != preset || keyed),
                                state,
                                move |s| {
                                    replace_clip_transform(s, preset);
                                },
                            )
                        })
                        .collect(),
                ),
            ),
        ],
    )
}

#[derive(Clone, Copy)]
struct TransformField {
    label: &'static str,
    accessor: fn(&mut Transform) -> &mut f32,
    min: f32,
    max: f32,
    step: f32,
    display_unit: &'static str,
    display_scale: f32,
}

fn transform_rows(state: &UiState, transform: Transform, inner: f32) -> Vec<BoxedWidget> {
    let selected = selected_clip(state).map(|(_, clip)| clip);
    let position = state.program.position.get();
    let inside = selected
        .as_ref()
        .is_some_and(|clip| keyframe_offset(clip, position).is_some());
    let keyed = selected
        .as_ref()
        .is_some_and(|clip| !clip.keyframes.is_empty());
    let fields = [
        TransformField {
            label: "Scale",
            accessor: |transform| &mut transform.scale,
            min: 0.1,
            max: 4.0,
            step: 0.05,
            display_unit: "%",
            display_scale: 100.0,
        },
        TransformField {
            label: "Position X",
            accessor: |transform| &mut transform.x,
            min: -1.0,
            max: 1.0,
            step: 0.01,
            display_unit: "%",
            display_scale: 100.0,
        },
        TransformField {
            label: "Position Y",
            accessor: |transform| &mut transform.y,
            min: -1.0,
            max: 1.0,
            step: 0.01,
            display_unit: "%",
            display_scale: 100.0,
        },
        TransformField {
            label: "Rotation",
            accessor: |transform| &mut transform.rotation,
            min: -360.0,
            max: 360.0,
            step: 5.0,
            display_unit: "°",
            display_scale: 1.0,
        },
        TransformField {
            label: "Crop left",
            accessor: |transform| &mut transform.crop_left,
            min: 0.0,
            max: (0.99 - transform.crop_right).max(0.0),
            step: 0.01,
            display_unit: "%",
            display_scale: 100.0,
        },
        TransformField {
            label: "Crop top",
            accessor: |transform| &mut transform.crop_top,
            min: 0.0,
            max: (0.99 - transform.crop_bottom).max(0.0),
            step: 0.01,
            display_unit: "%",
            display_scale: 100.0,
        },
        TransformField {
            label: "Crop right",
            accessor: |transform| &mut transform.crop_right,
            min: 0.0,
            max: (0.99 - transform.crop_left).max(0.0),
            step: 0.01,
            display_unit: "%",
            display_scale: 100.0,
        },
        TransformField {
            label: "Crop bottom",
            accessor: |transform| &mut transform.crop_bottom,
            min: 0.0,
            max: (0.99 - transform.crop_top).max(0.0),
            step: 0.01,
            display_unit: "%",
            display_scale: 100.0,
        },
        TransformField {
            label: "Opacity",
            accessor: |transform| &mut transform.opacity,
            min: 0.0,
            max: 1.0,
            step: 0.05,
            display_unit: "%",
            display_scale: 100.0,
        },
    ];
    let mut rows = Vec::new();
    let hint = if keyed {
        let count = selected.as_ref().unwrap().keyframes.len();
        format!(
            "{count} {}",
            if count == 1 { "keyframe" } else { "keyframes" }
        )
    } else {
        "No keyframes".into()
    };
    let mut keys = Flex::row().gap(4.0).align(Align::Center).child(Box::new(
        Flex::column()
            .child(text("Keyframes", muted(), 11.0))
            .child(text(hint, muted(), 10.0)),
    ));
    let navigation = |next| {
        let (glyph, help) = if next {
            (Glyph::StepForward, "Next keyframe")
        } else {
            (Glyph::StepBack, "Previous keyframe")
        };
        let target = selected
            .as_ref()
            .and_then(|clip| neighbouring_keyframe(clip, position, next));
        sized_icon_button(
            glyph,
            Help::new(help),
            24.0,
            target.is_some(),
            false,
            state,
            move |state| {
                if let Some(target) = target {
                    state.seek(target);
                }
            },
        )
    };
    keys = keys.child(navigation(false));
    let at_key = selected.as_ref().is_some_and(|clip| {
        keyframe_offset(clip, position)
            .is_some_and(|frame| clip.keyframes.iter().any(|key| key.frame == frame))
    });
    keys = keys.child(sized_icon_button(
        if at_key { Glyph::Minus } else { Glyph::Plus },
        Help::new(if at_key {
            "Remove keyframe at playhead"
        } else {
            "Add keyframe at playhead"
        }),
        24.0,
        inside,
        at_key,
        state,
        toggle_keyframe,
    ));
    keys = keys.child(navigation(true));
    rows.push(Box::new(keys) as BoxedWidget);
    if let Some((clip, index)) = selected
        .as_ref()
        .and_then(|clip| easing_segment(clip, position).map(|index| (clip, index)))
    {
        use crate::timeline::Easing;
        let choices = [
            ("Linear", Easing::Linear),
            ("Ease in", Easing::EASE_IN),
            ("Ease out", Easing::EASE_OUT),
            ("Ease in-out", Easing::EASE_IN_OUT),
            ("Hold", Easing::Hold),
        ];
        let current = clip.keyframes[index].easing;
        rows.push(text(
            if choices.iter().any(|(_, easing)| *easing == current) {
                "Easing"
            } else {
                "Easing · Custom"
            },
            muted(),
            11.0,
        ));
        for pair in choices.chunks(2) {
            let mut row = Flex::row().gap(4.0);
            for &(label, easing) in pair {
                row = row.child(with_help(
                    export_choice(
                        label,
                        current == easing,
                        (inner - 4.0) / 2.0,
                        state,
                        move |state| set_keyframe_easing(state, easing),
                    ),
                    Help::new(match easing {
                        Easing::Linear => "Move at a steady speed to the next keyframe.",
                        Easing::Hold => "Keep this value until the next keyframe.",
                        Easing::EASE_IN => "Start slowly, then speed up toward the next keyframe.",
                        Easing::EASE_OUT => {
                            "Start quickly, then slow down toward the next keyframe."
                        }
                        _ => "Start slowly, speed up, then slow down at the next keyframe.",
                    }),
                    state,
                ));
            }
            rows.push(Box::new(row));
        }
    }
    for field in fields {
        let animated = !field.label.starts_with("Crop");
        let mut current = transform;
        let value = *(field.accessor)(&mut current);
        let default = *(field.accessor)(&mut Transform::default());
        rows.push(numeric_row_preview(
            state,
            NumericSpec {
                label: field.label,
                min: f64::from(field.min * field.display_scale),
                max: f64::from(field.max * field.display_scale),
                slider_max: None,
                step: f64::from(field.step * field.display_scale),
                precision: 1,
                unit: field.display_unit,
                default: f64::from(default * field.display_scale),
            },
            f64::from(value * field.display_scale),
            !animated || !keyed || inside,
            move |clip, value| {
                let mut transform = if animated && !clip.keyframes.is_empty() {
                    clip.transform_at(position)
                } else {
                    clip.transform
                };
                *(field.accessor)(&mut transform) =
                    (value as f32 / field.display_scale).clamp(field.min, field.max);
                if animated && !clip.keyframes.is_empty() {
                    if let Some(frame) = keyframe_offset(clip, position) {
                        upsert_keyframe(
                            &mut clip.keyframes,
                            crate::timeline::Keyframe::new(frame, transform),
                        );
                    }
                } else {
                    clip.transform = transform;
                }
            },
            move |state, value| {
                set_clip_transform(state, animated, |transform| {
                    *(field.accessor)(transform) =
                        (value as f32 / field.display_scale).clamp(field.min, field.max);
                });
            },
        ));
    }
    rows.push(inline(secondary_button(
        None,
        "Reset transform",
        selected
            .as_ref()
            .is_some_and(|clip| !clip.transform.is_identity() || !clip.keyframes.is_empty()),
        state,
        |state| replace_clip_transform(state, Transform::default()),
    )));
    rows
}
fn set_clip_mask(state: &UiState, mask: Option<Mask>) {
    if let Some((track_id, clip)) = selected_clip(state) {
        state.edit(Command::SetClipMask {
            track_id,
            clip_id: clip.id,
            mask,
        });
    }
}

fn mask_value(mask: &mut Mask, field: usize) -> &mut f32 {
    match field {
        0 => &mut mask.center_x,
        1 => &mut mask.center_y,
        2 => &mut mask.width,
        3 => &mut mask.height,
        _ => &mut mask.feather,
    }
}

struct MaskSlider {
    state: UiState,
    field: usize,
    value: f32,
    width: f32,
}

impl Widget for MaskSlider {
    fn style(&self) -> creamui::Style {
        Style::default().size(self.width, 24.0).into()
    }

    fn paint(&self, painter: &mut dyn Painter, rect: Rect) {
        let track = Rect {
            x: rect.x + 6.0,
            y: rect.y + 10.0,
            width: (rect.width - 12.0).max(1.0),
            height: 4.0,
        };
        painter.fill_rect(track, surface(), 2.0);
        painter.fill_rect(
            Rect {
                width: track.width * self.value,
                ..track
            },
            accent(),
            2.0,
        );
        painter.fill_rect(
            Rect {
                x: track.x + track.width * self.value - 5.0,
                y: rect.y + 7.0,
                width: 10.0,
                height: 10.0,
            },
            text_color(),
            5.0,
        );
    }

    fn on_drag_start(&self) -> Option<Rc<dyn Fn(Point, Rect)>> {
        let state = self.state.clone();
        let field = self.field;
        Some(Rc::new(move |_, _| {
            if let Some((track, clip)) = selected_clip(&state)
                && let Some(mask) = clip.mask.as_deref().copied()
            {
                state.mask_drag.set(Some((track, clip.id, field, mask)));
            }
        }))
    }

    fn on_drag(&self) -> Option<Rc<dyn Fn(Point, Rect)>> {
        let state = self.state.clone();
        Some(Rc::new(move |point, rect| {
            if let Some((track, clip, field, mut mask)) = state.mask_drag.get() {
                *mask_value(&mut mask, field) =
                    ((point.x - 6.0) / (rect.width - 12.0).max(1.0)).clamp(0.0, 1.0);
                state.mask_drag.set(Some((track, clip, field, mask)));
                state.request_preview();
            }
        }))
    }

    fn on_drag_end(&self) -> Option<Rc<dyn Fn()>> {
        let state = self.state.clone();
        Some(Rc::new(move || {
            if let Some((track_id, clip_id, _, mask)) = state.mask_drag.get() {
                state.mask_drag.set(None);
                if !state.edit(Command::SetClipMask {
                    track_id,
                    clip_id,
                    mask: Some(mask),
                }) {
                    state.request_preview();
                }
            }
        }))
    }
}

fn mask_section(state: &UiState, clip: &Clip, inner: f32) -> BoxedWidget {
    let mut rows = Vec::new();
    let mut choices = Flex::row().gap(4.0);
    for (label, shape) in [
        ("None", None),
        ("Rectangle", Some(MaskShape::Rectangle)),
        ("Ellipse", Some(MaskShape::Ellipse)),
    ] {
        choices = choices.child(with_help(
            export_choice(
                label,
                clip.mask.as_deref().map(|mask| mask.shape) == shape,
                if shape.is_none() {
                    inner
                } else {
                    (inner - 4.0) / 2.0
                },
                state,
                move |state| {
                    let mask = shape.map(|shape| Mask {
                        shape,
                        ..selected_clip(state)
                            .and_then(|(_, clip)| clip.mask.map(|mask| *mask))
                            .unwrap_or_default()
                    });
                    set_clip_mask(state, mask);
                },
            ),
            Help::new(match shape {
                None => "Show the whole picture without a mask.",
                Some(MaskShape::Rectangle) => "Keep a rectangular area of this picture visible.",
                Some(MaskShape::Ellipse) => "Keep an oval area of this picture visible.",
            }),
            state,
        ));
        if shape.is_none() {
            rows.push(Box::new(choices) as BoxedWidget);
            choices = Flex::row().gap(4.0);
        }
    }
    rows.push(Box::new(choices) as BoxedWidget);
    if let Some(mut mask) = clip.mask.as_deref().copied() {
        if let Some((_, id, _, draft)) = state.mask_drag.get()
            && id == clip.id
        {
            mask = draft;
        }
        let helps = [
            "Move the mask horizontally within the source picture.",
            "Move the mask vertically within the source picture.",
            "Set mask width as a percentage of the source picture.",
            "Set mask height as a percentage of the source picture.",
            "Soften the edge as a percentage of the smaller mask dimension.",
        ];
        for (field, label) in ["X", "Y", "Width", "Height", "Feather"]
            .into_iter()
            .enumerate()
        {
            let value = *mask_value(&mut mask, field);
            rows.push(Box::new(
                Flex::row()
                    .gap(6.0)
                    .align(Align::Center)
                    .child(Box::new(Flex::row().width(48.0).child(text(
                        label,
                        muted(),
                        11.0,
                    ))))
                    .child(with_help(
                        Box::new(MaskSlider {
                            state: state.clone(),
                            field,
                            value,
                            width: (inner - 96.0).max(24.0),
                        }),
                        Help::new(helps[field]),
                        state,
                    ))
                    .child(text(format!("{:.0}%", value * 100.0), text_color(), 11.0)),
            ));
        }
        rows.push(with_help(
            secondary_button(
                None,
                if mask.invert {
                    "Invert: On"
                } else {
                    "Invert: Off"
                },
                true,
                state,
                move |state| {
                    set_clip_mask(
                        state,
                        Some(Mask {
                            invert: !mask.invert,
                            ..mask
                        }),
                    )
                },
            ),
            Help::new("Invert the mask to hide its inside and show its outside."),
            state,
        ));
        rows.push(with_help(
            secondary_button(
                None,
                "Reset mask",
                mask != Mask::default(),
                state,
                |state| set_clip_mask(state, Some(Mask::default())),
            ),
            Help::new("Restore a centered oval mask with a gently softened edge."),
            state,
        ));
        rows.push(text(
            "The mask follows the clip's transform.",
            faint(),
            10.0,
        ));
    }
    Box::new(append(Flex::column().gap(7.0), rows))
}

#[derive(Clone)]
struct ChromaSlider {
    state: UiState,
    target: ClipRef,
    property: usize,
    key: crate::timeline::ChromaKey,
}

impl ChromaSlider {
    fn draft(&self) -> crate::timeline::ChromaKey {
        self.state
            .chroma_preview
            .get()
            .filter(|(target, _)| *target == self.target)
            .map_or(self.key, |(_, key)| key)
    }

    fn level(&self) -> usize {
        let key = self.draft();
        let value = match self.property {
            0 => key.similarity,
            1 => key.smoothness,
            _ => key.spill,
        };
        (value * 100.0).round() as usize
    }
}

impl Widget for ChromaSlider {
    fn style(&self) -> creamui::Style {
        ZoomSlider {
            level: 0,
            levels: 101,
            set: Rc::new(|_| {}),
        }
        .style()
    }

    fn paint(&self, painter: &mut dyn Painter, rect: Rect) {
        ZoomSlider {
            level: self.level(),
            levels: 101,
            set: Rc::new(|_| {}),
        }
        .paint(painter, rect);
    }

    fn on_drag_start(&self) -> Option<Rc<dyn Fn(Point, Rect)>> {
        let state = self.state.clone();
        let target = self.target;
        let property = self.property;
        let key = self.key;
        Some(Rc::new(move |_, rect| {
            if state.busy.peek() || state.primary_clip() != Some(target) {
                return;
            }
            state.pause();
            *state.chroma_gesture.borrow_mut() = Some(ChromaGesture {
                target,
                property,
                rect,
                key,
            });
        }))
    }

    fn on_drag(&self) -> Option<Rc<dyn Fn(Point, Rect)>> {
        let state = self.state.clone();
        Some(Rc::new(move |pointer, rect| {
            state.preview_chroma_drag(Point {
                x: pointer.x + rect.x,
                y: pointer.y + rect.y,
            });
        }))
    }

    fn on_drag_end(&self) -> Option<Rc<dyn Fn()>> {
        let state = self.state.clone();
        let target = self.target;
        Some(Rc::new(move || {
            let gesture = state.chroma_gesture.borrow_mut().take();
            let draft = state.chroma_preview.peek();
            state.chroma_preview.set(None);
            if let Some((clip, key)) = draft
                && gesture.is_some_and(|gesture| gesture.target == target)
                && clip == target
                && state.primary_clip() == Some(target)
                && selected_clip(&state).is_some_and(|(_, clip)| clip.chroma_key != Some(key))
            {
                state.edit(Command::SetClipChromaKey {
                    track_id: target.0,
                    clip_id: target.1,
                    key: Some(key),
                });
            } else if draft.is_some() {
                state.request_preview();
            }
        }))
    }
}

fn chroma_section(
    state: &UiState,
    track_id: u64,
    clip: &Clip,
    inner: f32,
    busy: bool,
) -> BoxedWidget {
    let clip_id = clip.id;
    let key = state
        .chroma_preview
        .get()
        .filter(|(target, _)| *target == (track_id, clip_id))
        .map(|(_, key)| key)
        .or(clip.chroma_key);
    let mut rows = vec![inline(secondary_button(
        None,
        if key.is_some() { "Turn off" } else { "Turn on" },
        !busy,
        state,
        move |state| {
            state.edit(Command::SetClipChromaKey {
                track_id,
                clip_id,
                key: if key.is_some() {
                    None
                } else {
                    Some(Default::default())
                },
            });
        },
    ))];
    if let Some(key) = key {
        for (label, color) in [("Green", [0, 255, 0]), ("Blue", [0, 0, 255])] {
            rows.push(inline(secondary_button(
                None,
                label,
                !busy && key.color != color,
                state,
                move |state| {
                    state.edit(Command::SetClipChromaKey {
                        track_id,
                        clip_id,
                        key: Some(crate::timeline::ChromaKey { color, ..key }),
                    });
                },
            )));
        }
        let picker_state = state.clone();
        rows.push(Box::new(
            ColorPicker::controlled(
                Color::rgb(key.color[0], key.color[1], key.color[2]),
                &state.title_picker,
                move |color: Color| {
                    picker_state.edit(Command::SetClipChromaKey {
                        track_id,
                        clip_id,
                        key: Some(crate::timeline::ChromaKey {
                            color: [color.r, color.g, color.b],
                            ..key
                        }),
                    });
                },
            )
            .width(inner),
        ));
        for (index, label, value) in [
            (0, "Similarity", key.similarity),
            (1, "Smoothness (soft edge)", key.smoothness),
            (2, "Spill (remove color tint)", key.spill),
        ] {
            rows.push(text(
                format!("{label} · {:.0}%", value * 100.0),
                muted(),
                11.0,
            ));
            rows.push(Box::new(ChromaSlider {
                state: state.clone(),
                target: (track_id, clip_id),
                property: index,
                key,
            }));
        }
    }
    Box::new(append(Flex::column().gap(7.0), rows))
}

fn set_clip_color(state: &UiState, change: impl FnOnce(&mut ColorAdjust)) {
    if let Some((track_id, clip)) = selected_clip(state) {
        let mut color = clip.color;
        change(&mut color);
        state.edit(Command::SetClipColor {
            track_id,
            clip_id: clip.id,
            color,
        });
    }
}

#[derive(Clone, Copy)]
enum ColorProperty {
    Brightness,
    Contrast,
    Saturation,
}

impl ColorProperty {
    fn label(self) -> &'static str {
        match self {
            Self::Brightness => "Brightness",
            Self::Contrast => "Contrast",
            Self::Saturation => "Saturation",
        }
    }

    fn range(self) -> (f32, f32) {
        match self {
            Self::Brightness => (-1.0, 1.0),
            Self::Contrast | Self::Saturation => (0.0, 2.0),
        }
    }

    fn steps_per_unit(self) -> f32 {
        match self {
            Self::Brightness => 20.0,
            Self::Contrast | Self::Saturation => 10.0,
        }
    }

    fn get(self, color: ColorAdjust) -> f32 {
        match self {
            Self::Brightness => color.brightness,
            Self::Contrast => color.contrast,
            Self::Saturation => color.saturation,
        }
    }

    fn set(self, color: &mut ColorAdjust, value: f32) {
        match self {
            Self::Brightness => color.brightness = value,
            Self::Contrast => color.contrast = value,
            Self::Saturation => color.saturation = value,
        }
    }
}

struct EffectAmount {
    state: UiState,
    clip: ClipRef,
    amount: f32,
    enabled: bool,
}

impl Widget for EffectAmount {
    fn style(&self) -> creamui::Style {
        Style::default().size(220.0, 28.0).into()
    }

    fn paint(&self, painter: &mut dyn Painter, rect: Rect) {
        let amount = self
            .state
            .effect_drag
            .get()
            .filter(|(clip, _)| *clip == self.clip)
            .map_or(self.amount, |(_, amount)| amount);
        let track = Rect {
            x: rect.x + 6.0,
            y: rect.y + 12.0,
            width: rect.width - 12.0,
            height: 4.0,
        };
        painter.fill_rect(track, surface(), 2.0);
        painter.fill_rect(
            Rect {
                width: track.width * amount,
                ..track
            },
            accent(),
            2.0,
        );
        painter.fill_rect(
            Rect {
                x: track.x + track.width * amount - 5.0,
                y: rect.y + 9.0,
                width: 10.0,
                height: 10.0,
            },
            text_color(),
            5.0,
        );
    }

    fn on_drag_start(&self) -> Option<Rc<dyn Fn(Point, Rect)>> {
        if !self.enabled {
            return None;
        }
        let state = self.state.clone();
        let clip = self.clip;
        Some(Rc::new(move |point, rect| {
            state.pause();
            state.effect_drag.set(Some((
                clip,
                ((point.x - 6.0) / (rect.width - 12.0).max(1.0)).clamp(0.0, 1.0),
            )));
            state.request_preview();
        }))
    }

    fn on_drag(&self) -> Option<Rc<dyn Fn(Point, Rect)>> {
        if !self.enabled {
            return None;
        }
        let state = self.state.clone();
        let clip = self.clip;
        Some(Rc::new(move |point, rect| {
            if state
                .effect_drag
                .peek()
                .is_some_and(|(target, _)| target == clip)
            {
                state.effect_drag.set(Some((
                    clip,
                    ((point.x - 6.0) / (rect.width - 12.0).max(1.0)).clamp(0.0, 1.0),
                )));
                state.request_preview();
            }
        }))
    }

    fn on_drag_end(&self) -> Option<Rc<dyn Fn()>> {
        if !self.enabled {
            return None;
        }
        let state = self.state.clone();
        let clip = self.clip;
        Some(Rc::new(move || {
            let draft = state.effect_drag.peek();
            state.effect_drag.set(None);
            if let Some((target, amount)) = draft
                && target == clip
                && let Some((track_id, current)) = selected_clip(&state)
                && (track_id, current.id) == target
                && let Some(mut effect) = current.effect
            {
                effect.amount = amount;
                state.edit(Command::SetClipEffect {
                    track_id,
                    clip_id: current.id,
                    effect: Some(effect),
                });
            }
        }))
    }
}

fn effect_section(state: &UiState, track_id: u64, clip: &Clip, busy: bool) -> BoxedWidget {
    use crate::media::effects::error;
    use crate::timeline::EffectShader;
    let clip_id = clip.id;
    let mut rows = Vec::new();
    if let Some(effect) = &clip.effect {
        let amount = state
            .effect_drag
            .get()
            .filter(|(target, _)| *target == (track_id, clip_id))
            .map_or(effect.amount, |(_, amount)| amount);
        rows.push(property("Amount", format!("{:.0}%", amount * 100.0)));
        rows.push(Box::new(EffectAmount {
            state: state.clone(),
            clip: (track_id, clip_id),
            amount,
            enabled: !busy,
        }));
        if matches!(&effect.shader, EffectShader::Builtin(id) if id == "blur") {
            rows.push(text(
                "Amount sets the blur radius (0–8 pixels).",
                muted(),
                10.0,
            ));
        }
        if let Some(error) = error(&effect.shader) {
            rows.push(text(format!("Effect unavailable: {error}"), muted(), 10.0));
        }
    }

    Box::new(append(Flex::column().gap(7.0), rows))
}

fn color_control(
    state: &UiState,
    _inner: f32,
    property: ColorProperty,
    color: ColorAdjust,
) -> BoxedWidget {
    let (min, max) = property.range();
    numeric_row_preview(
        state,
        NumericSpec {
            label: property.label(),
            min: f64::from(min) * 100.0,
            max: f64::from(max) * 100.0,
            slider_max: None,
            step: 100.0 / f64::from(property.steps_per_unit()),
            precision: 1,
            unit: "%",
            default: f64::from(property.get(ColorAdjust::default())) * 100.0,
        },
        f64::from(property.get(color)) * 100.0,
        true,
        move |clip, value| property.set(&mut clip.color, value as f32 / 100.0),
        move |state, value| {
            set_clip_color(state, |color| property.set(color, value as f32 / 100.0));
        },
    )
}

fn fade_row(state: &UiState, clip: &Clip, incoming: bool) -> Box<dyn Widget> {
    let (label, value, other) = if incoming {
        ("Fade in", clip.fade_in_frames, clip.fade_out_frames)
    } else {
        ("Fade out", clip.fade_out_frames, clip.fade_in_frames)
    };
    let fps = state.frame_rate();
    let seconds = f64::from(fps.1) / f64::from(fps.0);
    numeric_row(
        state,
        NumericSpec {
            label,
            min: 0.0,
            max: clip.duration_frames.saturating_sub(other) as f64 * seconds,
            slider_max: None,
            step: seconds,
            precision: 3,
            unit: "s",
            default: 0.0,
        },
        value as f64 * seconds,
        true,
        move |state, value| {
            if let Some((track_id, clip)) = selected_clip(state) {
                let value = (value / seconds).round() as u64;
                state.edit(Command::SetClipFades {
                    track_id,
                    clip_id: clip.id,
                    fade_in_frames: if incoming { value } else { clip.fade_in_frames },
                    fade_out_frames: if incoming {
                        clip.fade_out_frames
                    } else {
                        value
                    },
                });
            }
        },
    )
}

fn media_name(media: &Media) -> String {
    media.path.file_name().map_or_else(
        || media.path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

fn media_details(media: &Media) -> String {
    let duration = media
        .duration_us
        .map(|duration| short_duration(duration as f64 / 1_000_000.0));
    match (media.kind, duration) {
        (MediaKind::Image, _) => "Still image".into(),
        (MediaKind::Video, Some(duration)) if media.has_audio => {
            format!("Video + audio · {duration}")
        }
        (MediaKind::Video, Some(duration)) => format!("Video · {duration}"),
        (MediaKind::Audio, Some(duration)) => format!("Audio · {duration}"),
        (MediaKind::Video, None) => "Video · not probed".into(),
        (MediaKind::Audio, None) => "Audio · not probed".into(),
    }
}

/// A short `12.5s` reading of a frame count.
fn seconds(frames: u64, fps: (u32, u32)) -> String {
    format!(
        "{:.1}s",
        frames as f64 * f64::from(fps.1) / f64::from(fps.0.max(1))
    )
}

fn frame_rate_label(fps: (u32, u32)) -> String {
    let rate = format!("{:.3}", f64::from(fps.0) / f64::from(fps.1.max(1)));
    format!("{} fps", rate.trim_end_matches('0').trim_end_matches('.'))
}

fn track_labels(project: &Project) -> Vec<(u64, String)> {
    let (mut video, mut audio, mut subtitles) = (0, 0, 0);
    project
        .tracks()
        .iter()
        .map(|track| {
            let label = if track.subtitles {
                subtitles += 1;
                format!("S{subtitles}")
            } else if track.kind == TrackKind::Video {
                video += 1;
                format!("V{video}")
            } else {
                audio += 1;
                format!("A{audio}")
            };
            (track.id, track.display_name(&label))
        })
        .collect()
}

/// Appends `items` after any existing children (`with_children` replaces them).
fn append(flex: Flex, items: Vec<BoxedWidget>) -> Flex {
    items.into_iter().fold(flex, Flex::child)
}

fn inline(widget: BoxedWidget) -> BoxedWidget {
    Box::new(Flex::row().child(widget))
}

fn spacer() -> BoxedWidget {
    Box::new(Flex::row().grow(1.0))
}

fn dot(color: Color, size: f32) -> BoxedWidget {
    Box::new(
        Flex::row()
            .size(size, size)
            .background(color)
            .corner_radius(size / 2.0),
    )
}

fn divider() -> BoxedWidget {
    Box::new(Flex::row().size(1.0, 18.0).background(border()))
}

fn fill_style() -> Style {
    Style {
        size: creamui::core::layout::Size {
            width: Dimension::Percent(1.0),
            height: Dimension::Percent(1.0),
        },
        flex_grow: 1.0,
        ..Default::default()
    }
}

fn panel_header(title: &str, detail: BoxedWidget, actions: Vec<BoxedWidget>) -> BoxedWidget {
    Box::new(append(
        Flex::row()
            .height(28.0)
            .gap(8.0)
            .align(Align::Center)
            .child(heading(title, text_color(), 13.0))
            .child(detail)
            .child(spacer()),
        actions,
    ))
}

fn lut_effect_controls(state: &UiState, track_id: u64, clip: &Clip, busy: bool) -> BoxedWidget {
    let clip_id = clip.id;
    let name = clip
        .lut
        .as_ref()
        .and_then(|path| path.file_name())
        .map_or_else(|| "None".into(), |name| name.to_string_lossy().into_owned());
    section(
        "Color LUT",
        vec![
            property("LUT", name),
            inline(secondary_button(
                None,
                "Choose .cube file…",
                !busy,
                state,
                move |state| state.choose_lut(track_id, clip_id),
            )),
            inline(secondary_button(
                None,
                "Clear LUT",
                !busy && clip.lut.is_some(),
                state,
                move |state| {
                    state.edit(Command::SetClipLut {
                        track_id,
                        clip_id,
                        path: None,
                    });
                },
            )),
        ],
    )
}
fn stabilization_effect_controls(
    state: &UiState,
    clip: &Clip,
    busy: bool,
    locked: bool,
) -> BoxedWidget {
    let mut controls = vec![with_help(
        secondary_button(
            None,
            if state.stabilization_analyzing.get() {
                "Analyzing…"
            } else if clip.stabilization.is_some() {
                "Re-analyze"
            } else {
                "Analyze"
            },
            !busy && !locked && !state.stabilization_analyzing.get(),
            state,
            UiState::stabilize_clip,
        ),
        Help::new("Analyze camera shake in the source range without changing your original video"),
        state,
    )];
    if let Some(data) = &clip.stabilization {
        let strength = state
            .stabilization_drag
            .get()
            .filter(|(_, id, _)| *id == clip.id)
            .map_or(data.strength, |(_, _, draft)| draft.strength);
        controls.push(text(
            format!("Strength · {:.0}%", strength * 100.0),
            muted(),
            11.0,
        ));
        controls.push(with_help(
            Box::new(super::controls::StabilizationSlider {
                state: state.clone(),
                level: (strength * 100.0).round() as usize,
            }),
            Help::new("Smooth more camera shake; automatically crops up to 25% to hide borders"),
            state,
        ));
        controls.push(secondary_button(
            None,
            "Remove",
            !busy && !locked,
            state,
            |state| {
                if let Some((track_id, clip)) = selected_clip(state) {
                    state.edit(Command::SetClipStabilization {
                        track_id,
                        clip_id: clip.id,
                        stabilization: None,
                    });
                }
            },
        ));
    }
    controls.push(text("Smooths camera shake.", faint(), 10.0));
    controls.push(text("Original video is untouched.", faint(), 10.0));

    Box::new(append(Flex::column().gap(7.0), controls))
}
fn speed_effect_controls(state: &UiState, track_id: u64, clip: &Clip, busy: bool) -> BoxedWidget {
    let mut presets = Vec::new();
    let mut unavailable = false;
    for speed in [0.25, 0.5, 1.0, 1.5, 2.0, 4.0] {
        let enabled = state
            .editor
            .borrow()
            .can_set_clip_speed(track_id, clip.id, speed);
        unavailable |= !enabled && speed != clip.speed;
        presets.push(enabled_chip(
            &format!("{speed}x"),
            clip.speed == speed,
            !busy && enabled,
            state,
            move |state| {
                if let Some((track_id, clip)) = selected_clip(state) {
                    state.edit(Command::SetClipSpeed {
                        track_id,
                        clip_id: clip.id,
                        speed,
                    });
                }
            },
        ));
    }
    return section(
        &format!("Speed · {}x", clip.speed),
        vec![
            Box::new(Grid::new().columns(3).gap(4.0).with_children(presets)),
            text("Slower is longer · faster is shorter.", faint(), 10.0),
            text(
                if unavailable {
                    if state
                        .editor
                        .borrow()
                        .project()
                        .tracks()
                        .iter()
                        .any(|track| track.id == track_id && track.locked)
                    {
                        "Unlock the track to change speed."
                    } else {
                        "Not enough source left for some speeds."
                    }
                } else {
                    "Audio keeps its pitch. Stops at the next clip."
                },
                faint(),
                10.0,
            ),
        ],
    );
}
fn effects_section(
    state: &UiState,
    track_id: u64,
    clip: &Clip,
    inner: f32,
    busy: bool,
) -> BoxedWidget {
    let locked = state
        .editor
        .borrow()
        .project()
        .tracks()
        .iter()
        .any(|track| track.id == track_id && track.locked);
    let mut cards: Vec<BoxedWidget> = Vec::new();
    let selected = state
        .selected_detail
        .get()
        .and_then(|(target, detail)| match detail {
            ClipDetail::Effect(kind) if target == (track_id, clip.id) => Some(kind),
            _ => None,
        });
    for &kind in clip
        .effect_stack
        .iter()
        .filter(|kind| Some(**kind) == selected)
    {
        let clip_id = clip.id;
        let category = match kind.categories() {
            [EffectCategory::Video] => "Video",
            [EffectCategory::Audio] => "Audio",
            _ => "Video/Audio",
        };
        let header = Box::new(
            Flex::row()
                .gap(5.0)
                .align(Align::Center)
                .child(Box::new(Flex::row().grow(1.0).basis(0.0).child(with_help(
                    heading(fit(kind.label(), inner - 100.0, 11.0), text_color(), 11.0),
                    Help::new(kind.label()),
                    state,
                ))))
                .child(Box::new(
                    Flex::row()
                        .padding_xy(5.0, 3.0)
                        .background(surface_hover())
                        .corner_radius(7.0)
                        .child(text(category, muted(), 9.0)),
                ))
                .child(with_help(
                    secondary_button(None, "Remove", !busy && !locked, state, move |state| {
                        state.edit(Command::RemoveEffect {
                            track_id,
                            clip_id,
                            kind,
                        });
                    }),
                    Help::new("Remove this effect"),
                    state,
                )),
        );
        let body = if kind.is_shader() {
            Box::new(
                Flex::column()
                    .gap(7.0)
                    .child(text(kind.description(), muted(), 10.0))
                    .child(effect_section(state, track_id, clip, busy || locked)),
            ) as BoxedWidget
        } else {
            match kind {
                EffectKind::ChromaKey => {
                    chroma_section(state, track_id, clip, inner - 16.0, busy || locked)
                }
                EffectKind::ColorLut => lut_effect_controls(state, track_id, clip, busy || locked),
                EffectKind::Mask => mask_section(state, clip, inner - 16.0),
                EffectKind::Stabilization => {
                    stabilization_effect_controls(state, clip, busy, locked)
                }
                EffectKind::ColorCorrection => Box::new(append(
                    Flex::column().gap(7.0),
                    vec![
                        color_control(state, inner - 16.0, ColorProperty::Brightness, clip.color),
                        color_control(state, inner - 16.0, ColorProperty::Contrast, clip.color),
                        color_control(state, inner - 16.0, ColorProperty::Saturation, clip.color),
                        inline(secondary_button(
                            None,
                            "Reset color",
                            !busy && !locked && !clip.color.neutral(),
                            state,
                            |s| set_clip_color(s, |color| *color = Default::default()),
                        )),
                        text("Color affects playback and export.", faint(), 10.0),
                    ],
                )),
                EffectKind::Fades => Box::new(
                    Flex::column()
                        .gap(7.0)
                        .child(fade_row(state, clip, true))
                        .child(fade_row(state, clip, false)),
                ),
                EffectKind::Speed => speed_effect_controls(state, track_id, clip, busy || locked),
                _ => text(kind.description(), muted(), 10.0),
            }
        };
        cards.push(Box::new(
            Flex::column()
                .gap(7.0)
                .padding(8.0)
                .background(surface())
                .corner_radius(7.0)
                .child(header)
                .child(body),
        ));
    }
    Box::new(Flex::column().gap(10.0).with_children(cards))
}

fn section(title: &str, rows: Vec<BoxedWidget>) -> BoxedWidget {
    Box::new(append(
        Flex::column()
            .gap(7.0)
            .child(Box::new(
                Flex::row().height(1.0).full_width().background(border()),
            ))
            .child(heading(title.to_uppercase(), faint(), 10.0)),
        rows,
    ))
}

fn property(label: &str, value: impl Into<String>) -> BoxedWidget {
    Box::new(
        Flex::row()
            .gap(8.0)
            .align(Align::Center)
            .child(Box::new(Flex::row().width(86.0).child(text(
                label,
                muted(),
                11.0,
            ))))
            .child(text(value, text_color(), 11.0)),
    )
}

fn stage_message(
    icon: Glyph,
    title: &str,
    detail: &str,
    action: Option<BoxedWidget>,
) -> BoxedWidget {
    let mut content = Flex::column()
        .gap(8.0)
        .align(Align::Center)
        .padding(16.0)
        .child(glyph(icon, faint(), 30.0))
        .child(heading(title, text_color().mix(muted(), 0.3), 14.0))
        .child(text(detail, muted(), 11.0));
    if let Some(action) = action {
        content = content
            .child(Box::new(Flex::row().height(4.0)))
            .child(action);
    }
    Box::new(content)
}

fn monitor_viewport(slot: &GpuFrameSlot, aspect: f32) -> BoxedWidget {
    Box::new(GpuViewport::new(fill_style(), slot.clone()).aspect_ratio(aspect))
}

fn preview_stage(state: &UiState, target: PlaybackMonitor) -> Option<BoxedWidget> {
    let monitor = match target {
        PlaybackMonitor::Program => &state.program,
        PlaybackMonitor::Source => &state.source,
    };
    let message = match monitor.preview.get() {
        PreviewState::Ready => None,
        PreviewState::Loading => Some(stage_message(Glyph::Film, "Loading preview…", "", None)),
        PreviewState::Failed(reason) => Some(stage_message(
            Glyph::Film,
            "Preview failed",
            &reason,
            Some(secondary_button(
                None,
                "Retry",
                !state.busy.get(),
                state,
                move |s| s.retry_preview(target),
            )),
        )),
    };
    message.map(|message| Box::new(Flex::column().full_width().child(message)) as BoxedWidget)
}

/// What plays at `frame`: picture, sound, titles, and the first missing
/// source that would have shown there, if any.
fn program_layers<'a>(
    project: &'a Project,
    missing: &HashSet<u64>,
    frame: u64,
) -> (bool, bool, Vec<(ClipRef, Title)>, Option<&'a Media>) {
    let mut picture = false;
    let mut audio = false;
    let mut titles = Vec::new();
    let mut lost_picture = None;
    let mut lost_sound = None;
    for track in project.tracks() {
        if track.hidden {
            continue;
        }
        for clip in &track.clips {
            if clip.start_frame > frame || frame - clip.start_frame >= clip.duration_frames {
                continue;
            }
            match &clip.content {
                ClipContent::Title(title) if track.kind == TrackKind::Video => {
                    titles.push(((track.id, clip.id), title.clone()));
                }
                ClipContent::Media { media_id } => {
                    if let Some(media) = project.media().iter().find(|media| media.id == *media_id)
                    {
                        let shows =
                            track.kind == TrackKind::Video && media.kind != MediaKind::Audio;
                        let sounds = track.kind == TrackKind::Audio
                            && (media.kind == MediaKind::Audio || media.has_audio);
                        if !missing.contains(media_id) {
                            picture |= shows;
                            audio |= sounds;
                        } else if shows {
                            lost_picture = lost_picture.or(Some(media));
                        } else if sounds {
                            lost_sound = lost_sound.or(Some(media));
                        }
                    }
                }
                ClipContent::Title(_) => {}
            }
        }
    }
    let lost = lost_picture.or(lost_sound.filter(|_| !picture && titles.is_empty()));
    (picture, audio, titles, lost)
}

struct ProgramCanvas {
    slot: GpuFrameSlot,
    picture: bool,
    picture_unavailable: bool,
    titles: Vec<(ClipRef, Title)>,
    selected: Option<ClipRef>,
    state: UiState,
    aspect: f32,
    resolution: (u32, u32),
}

fn run_width(value: &str, size: f32, family: &str, bold: bool) -> f32 {
    let face = resolve(
        family,
        if bold {
            FontWeight::Bold
        } else {
            FontWeight::Regular
        },
    );
    cached_layout(
        &face,
        value,
        size,
        &LayoutSettings {
            horizontal_align: HorizontalAlign::Left,
            ..Default::default()
        },
    )
    .lines
    .first()
    .map_or(0.0, |line| line.width)
}

fn program_frame(rect: Rect, aspect: f32) -> Rect {
    let width = rect.width.min(rect.height * aspect);
    let height = rect.height.min(rect.width / aspect);
    Rect {
        x: rect.x + (rect.width - width) / 2.0,
        y: rect.y + (rect.height - height) / 2.0,
        width,
        height,
    }
}

fn title_box(title: &Title, bounds: Rect, resolution: (u32, u32)) -> Rect {
    let Rect {
        x,
        y,
        width,
        height,
    } = bounds;
    let size = (title.font_size * width / resolution.0.max(1) as f32)
        .max(1.0)
        .min(height * 2.0);
    let mut longest = 0.0_f32;
    let mut line_width = 0.0;
    let mut lines = 1;
    let family = title_fonts::family_stack(&title.font_family);
    for run in title_markup::parse(title) {
        for (index, part) in run.text.split('\n').enumerate() {
            if index > 0 {
                longest = longest.max(line_width);
                line_width = 0.0;
                lines += 1;
            }
            line_width += run_width(part, size, &family, run.bold);
        }
    }
    let (pad_x, pad_y) = if title.background.is_some() {
        crate::media::composite::title_background_padding(size)
    } else {
        (0.0, 0.0)
    };
    let box_width = (longest.max(line_width) + 16.0 + 2.0 * pad_x).max(32.0);
    let box_height = lines as f32 * size * 1.35 + 10.0 + 2.0 * pad_y;
    Rect {
        x: x + width * title.x - box_width / 2.0,
        y: y + height * title.y - box_height / 2.0,
        width: box_width,
        height: box_height,
    }
}

fn title_resize_handle(selection: Rect, bounds: Rect) -> Rect {
    // Keep the handle reachable when the selection corner is clipped.
    let size = 10.0_f32.min(bounds.height);
    Rect {
        x: (selection.x + selection.width + size / 2.0).min(bounds.x + bounds.width) - size,
        y: (selection.y + selection.height + size / 2.0).min(bounds.y + bounds.height) - size,
        width: size,
        height: size,
    }
}

impl ProgramCanvas {
    fn selected_title(&self) -> Option<&Title> {
        let selected = self.selected?;
        self.titles
            .iter()
            .find(|(key, _)| *key == selected)
            .map(|(_, title)| title)
    }

    fn selected_title_box(&self, bounds: Rect) -> Option<Rect> {
        Some(title_box(self.selected_title()?, bounds, self.resolution))
    }
}

impl Widget for ProgramCanvas {
    fn style(&self) -> creamui::Style {
        fill_style().into()
    }

    fn paint(&self, painter: &mut dyn Painter, rect: Rect) {
        if self.picture {
            GpuViewport::new(fill_style(), self.slot.clone())
                .aspect_ratio(self.aspect)
                .paint(painter, rect);
        }
        let bounds = program_frame(rect, self.aspect);
        let (width, height) = (bounds.width, bounds.height);
        if width <= 16.0 || height <= 0.0 {
            return;
        }
        let scale = width / self.resolution.0.max(1) as f32;
        painter.push_clip_rounded(bounds, 0.0);
        for (key, title) in &self.titles {
            if self.picture {
                continue;
            }
            let editor = self.state.editor.borrow();
            let motion = editor
                .project()
                .tracks()
                .iter()
                .flat_map(|track| &track.clips)
                .find(|clip| clip.id == key.1)
                .map(|clip| {
                    title.animation.resolve(
                        self.state
                            .program
                            .position
                            .get()
                            .saturating_sub(clip.start_frame),
                        clip.duration_frames,
                        self.state.frame_rate(),
                    )
                })
                .unwrap_or_else(|| title.animation.resolve(0, 0, self.state.frame_rate()));
            let (title, runs) = title_markup::animated(title, motion);
            let size = (title.font_size * scale).max(1.0).min(height * 2.0);
            let family = title_fonts::family_stack(&title.font_family);
            let lines: Vec<Vec<_>> = {
                let mut lines = vec![Vec::new()];
                for run in runs {
                    let mut parts = run.text.split('\n').peekable();
                    while let Some(part) = parts.next() {
                        if !part.is_empty() {
                            let mut styled = run.clone();
                            styled.text = part.to_owned();
                            lines.last_mut().unwrap().push(styled);
                        }
                        if parts.peek().is_some() {
                            lines.push(Vec::new());
                        }
                    }
                }
                lines
            };
            let line_height = size * 1.35;
            let start_y = bounds.y + height * title.y - line_height * lines.len() as f32 / 2.0;
            for (index, line) in lines.iter().enumerate() {
                let widths: Vec<_> = line
                    .iter()
                    .map(|run| run_width(&run.text, size, &family, run.bold))
                    .collect();
                let line_width: f32 = widths.iter().sum();
                let mut x = bounds.x + width * title.x - line_width / 2.0;
                if let Some(color) = title.background
                    && !line.is_empty()
                {
                    let (pad_x, pad_y) = crate::media::composite::title_background_padding(size);
                    painter.fill_rect(
                        Rect {
                            x: x - pad_x,
                            y: start_y + index as f32 * line_height + pad_y,
                            width: line_width + 2.0 * pad_x,
                            height: line_height - 2.0 * pad_y,
                        },
                        Color::rgba(color[0], color[1], color[2], color[3]),
                        0.0,
                    );
                }
                for (run, text_width) in line.iter().zip(widths) {
                    let line_rect = Rect {
                        x,
                        y: start_y + index as f32 * line_height,
                        width: text_width.max(1.0),
                        height: line_height,
                    };
                    if !run.text.trim().is_empty() && run.color.a > 0 {
                        if title.outline {
                            let radius = title_outline_radius(size);
                            for (dx, dy) in
                                [(-radius, 0.0), (radius, 0.0), (0.0, -radius), (0.0, radius)]
                            {
                                painter.fill_text_font(
                                    Rect {
                                        x: line_rect.x + dx,
                                        y: line_rect.y + dy,
                                        ..line_rect
                                    },
                                    &run.text,
                                    Color::rgba(0, 0, 0, run.color.a),
                                    size,
                                    TextAlign::Start,
                                    Some(&family),
                                    run.bold,
                                    run.italic,
                                );
                            }
                        }
                        painter.fill_text_font(
                            line_rect,
                            &run.text,
                            run.color,
                            size,
                            TextAlign::Start,
                            Some(&family),
                            run.bold,
                            run.italic,
                        );
                    }
                    x += text_width;
                }
            }
        }
        painter.pop_clip();
        if self.picture_unavailable {
            let label = "Video preview unavailable";
            let pill_width = (run_width(label, 11.0, "", false) + 20.0).min(width);
            let pill = Rect {
                x: bounds.x + (width - pill_width) / 2.0,
                y: bounds.y + 8.0,
                width: pill_width,
                height: 22.0,
            };
            painter.fill_rect(pill, surface(), 11.0);
            painter.stroke_rect(pill, border(), 1.0, 11.0);
            painter.fill_text(
                pill,
                label,
                text_color().mix(muted(), 0.3),
                11.0,
                TextAlign::Center,
            );
        }
        if let Some(box_rect) = self.selected_title_box(bounds) {
            painter.push_clip_rounded(bounds, 0.0);
            painter.stroke_rect(box_rect, accent(), 1.5, 0.0);
            painter.fill_rect(title_resize_handle(box_rect, bounds), accent(), 2.0);
            painter.pop_clip();
        }
    }

    fn on_drag_start(&self) -> Option<Rc<dyn Fn(Point, Rect)>> {
        let selected = self.selected?;
        let title = self.selected_title()?.clone();
        let state = self.state.clone();
        let aspect = self.aspect;
        let resolution = self.resolution;
        Some(Rc::new(move |pointer, rect| {
            let bounds = program_frame(rect, aspect);
            if bounds.width <= 16.0 || bounds.height <= 0.0 {
                return;
            }
            let box_rect = title_box(&title, bounds, resolution);
            let position = Point {
                x: pointer.x + rect.x,
                y: pointer.y + rect.y,
            };
            if !bounds.contains(position) || !box_rect.inflate(5.0).contains(position) {
                return;
            }
            state.pause();
            *state.title_gesture.borrow_mut() = Some(TitleGesture {
                clip: selected,
                title: title.clone(),
                pointer,
                resize: title_resize_handle(box_rect, bounds)
                    .inflate(7.0)
                    .contains(position),
            });
        }))
    }

    fn on_drag(&self) -> Option<Rc<dyn Fn(Point, Rect)>> {
        if !self
            .titles
            .iter()
            .any(|(key, _)| Some(*key) == self.selected)
        {
            return None;
        }
        self.selected_title()?;
        let state = self.state.clone();
        let aspect = self.aspect;
        let resolution = self.resolution;
        Some(Rc::new(move |pointer, rect| {
            let (key, draft) = {
                let gesture = state.title_gesture.borrow();
                let Some(gesture) = gesture.as_ref() else {
                    return;
                };
                let width = rect.width.min(rect.height * aspect).max(1.0);
                let height = rect.height.min(rect.width / aspect).max(1.0);
                let dx = pointer.x - gesture.pointer.x;
                let dy = pointer.y - gesture.pointer.y;
                let mut draft = gesture.title.clone();
                if gesture.resize {
                    let horizontal = dx * resolution.0 as f32 / width;
                    let vertical = dy * resolution.1 as f32 / height;
                    let delta = if horizontal.abs() > vertical.abs() {
                        horizontal
                    } else {
                        vertical
                    };
                    draft.font_size = (draft.font_size + delta).clamp(1.0, 8192.0);
                } else {
                    draft.x = (draft.x + dx / width).clamp(0.0, 1.0);
                    draft.y = (draft.y + dy / height).clamp(0.0, 1.0);
                }
                (gesture.clip, draft)
            };
            state.title_preview.set(Some((key, draft)));
            state.request_preview();
        }))
    }

    fn on_drag_end(&self) -> Option<Rc<dyn Fn()>> {
        let state = self.state.clone();
        Some(Rc::new(move || {
            let Some(gesture) = state.title_gesture.borrow_mut().take() else {
                return;
            };
            let draft = state.title_preview.peek();
            let had_draft = draft.is_some();
            state.title_preview.set(None);
            if let Some((clip, title)) = draft
                && clip == gesture.clip
                && title != gesture.title
            {
                state.title_size.set_value(title.font_size.to_string());
                state.edit(Command::SetTitle {
                    track_id: clip.0,
                    clip_id: clip.1,
                    title,
                });
            } else if had_draft {
                state.request_preview();
            }
        }))
    }
}

struct Transport {
    program: bool,
    position: u64,
    end: u64,
    fps: (u32, u32),
    playing: bool,
    can_play: bool,
    color: Color,
    compact: bool,
}

fn transport(
    state: &UiState,
    info: Transport,
    seek: impl Fn(&UiState, u64) + 'static,
    toggle: impl Fn(&UiState) + 'static,
) -> Vec<BoxedWidget> {
    let seek = Rc::new(seek);
    let Transport {
        program,
        position,
        end,
        fps,
        playing,
        can_play,
        color,
        compact,
    } = info;
    let last = end.saturating_sub(1);
    let jump = |icon, help, enabled, target: u64| {
        let seek = seek.clone();
        icon_button(icon, help, enabled, state, move |state| seek(state, target))
    };
    let hint = |source, program_label, shortcut| {
        if program {
            Help::new(program_label).shortcut(shortcut)
        } else {
            Help::new(source)
        }
    };
    let scrub_state = state.clone();
    let scrub_seek = seek.clone();
    vec![
        Box::new(Scrubber {
            position,
            end,
            color,
            seek: Rc::new(move |frame| scrub_seek(&scrub_state, frame)),
        }),
        Box::new(
            Flex::row()
                .height(36.0)
                .align(Align::Center)
                .child(Box::new(Flex::row().grow(1.0).basis(0.0).child(heading(
                    timecode(position, fps),
                    color,
                    if compact { 11.0 } else { 12.0 },
                ))))
                .child(Box::new(append(
                    Flex::row().gap(2.0).align(Align::Center),
                    [
                        (!compact).then(|| {
                            jump(
                                Glyph::ToStart,
                                hint("Go to source start", "Go to sequence start", "Home"),
                                position > 0,
                                0,
                            )
                        }),
                        Some(jump(
                            Glyph::StepBack,
                            hint(
                                "Previous source frame",
                                "Previous sequence frame",
                                "Left arrow",
                            ),
                            position > 0,
                            position.saturating_sub(1),
                        )),
                        Some(Box::new(Flex::row().width(4.0))),
                        Some(play_button(
                            playing,
                            if program {
                                Help::new("Play / pause sequence").shortcut("Space / K")
                            } else {
                                Help::new("Play / pause source").shortcut("Space")
                            },
                            can_play || playing,
                            state,
                            toggle,
                        )),
                        Some(Box::new(Flex::row().width(4.0))),
                        Some(jump(
                            Glyph::StepForward,
                            hint("Next source frame", "Next sequence frame", "Right arrow"),
                            position < last,
                            (position + 1).min(last),
                        )),
                        (!compact).then(|| {
                            jump(
                                Glyph::ToEnd,
                                hint("Go to source end", "Go to sequence end", "End"),
                                end > 0 && position < last,
                                last,
                            )
                        }),
                    ]
                    .into_iter()
                    .flatten()
                    .collect(),
                )))
                .child(Box::new(
                    Flex::row()
                        .grow(1.0)
                        .basis(0.0)
                        .justify(Justify::End)
                        .child(text(
                            timecode(end, fps),
                            muted(),
                            if compact { 10.0 } else { 11.0 },
                        )),
                )),
        ),
    ]
}

fn monitor(
    title: &str,
    accent: Color,
    detail: String,
    actions: Vec<BoxedWidget>,
    stage: BoxedWidget,
    controls: Vec<BoxedWidget>,
) -> BoxedWidget {
    Box::new(append(
        Flex::column()
            .grow(1.0)
            .basis(0.0)
            .gap(8.0)
            .padding(10.0)
            .background(panel())
            .corner_radius(10.0)
            .child(Box::new(append(
                Flex::row()
                    .height(28.0)
                    .gap(8.0)
                    .align(Align::Center)
                    .child(dot(accent, 7.0))
                    .child(heading(title, text_color(), 13.0))
                    .child(text(detail, muted(), 11.0))
                    .child(spacer()),
                actions,
            )))
            .child(Box::new(
                Flex::column()
                    .grow(1.0)
                    .align(Align::Center)
                    .justify(Justify::Center)
                    .background(STAGE)
                    .corner_radius(6.0)
                    .border(border(), 1.0)
                    .child(stage),
            )),
        controls,
    ))
}

fn gallery_effects(query: &str, category: EffectCategory) -> Vec<EffectKind> {
    let query = query.trim().to_lowercase();
    EffectKind::ALL
        .into_iter()
        .filter(|kind| {
            kind.categories().contains(&category)
                && (kind.label().to_lowercase().contains(&query)
                    || kind.description().to_lowercase().contains(&query))
        })
        .collect()
}

fn effects_gallery(state: &UiState, width: f32) -> BoxedWidget {
    let query = state.effect_query.value();
    let mut groups = Vec::new();
    for (category, label) in [
        (EffectCategory::Video, "Video effects"),
        (EffectCategory::Audio, "Audio effects"),
    ] {
        let mut rows = vec![heading(label, muted(), 11.0)];
        for kind in gallery_effects(&query, category) {
            let icon = match kind {
                EffectKind::Reverse => Glyph::Reverse,
                EffectKind::FreezeFrame => Glyph::Freeze,
                EffectKind::Speed => Glyph::StepForward,
                EffectKind::Fades => Glyph::Volume,
                EffectKind::NoiseReduction => Glyph::Music,
                EffectKind::Stabilization => Glyph::Expand,
                EffectKind::Mask => Glyph::Picture,
                EffectKind::CustomShader | EffectKind::ColorLut => Glyph::Document,
                _ => Glyph::Film,
            };
            let (click, start, drag, end) =
                (state.clone(), state.clone(), state.clone(), state.clone());
            let row = RawButton::new(
                Style::default()
                    .size(width - 26.0, 42.0)
                    .flex_row()
                    .gap(8.0)
                    .padding_all(8.0),
                move || click.click_gallery_effect(kind),
            )
            .with_drag_start(move |point, rect| {
                start.begin_effect_drag(
                    kind,
                    Point {
                        x: point.x + rect.x,
                        y: point.y + rect.y,
                    },
                )
            })
            .with_drag(move |point, rect| {
                drag.drag_pointer(Point {
                    x: point.x + rect.x,
                    y: point.y + rect.y,
                })
            })
            .with_drag_end(move || end.end_effect_drag())
            .background(panel())
            .corner_radius(5.0)
            .hover_style(StateStyle::new().background(surface()))
            .focus_style(StateStyle::new().outline(accent(), 2.0))
            .child(glyph(
                icon,
                if category == EffectCategory::Audio {
                    AUDIO
                } else {
                    VIDEO
                },
                18.0,
            ))
            .child(Box::new(
                Flex::column()
                    .gap(3.0)
                    .child(heading(kind.label(), text_color(), 11.0))
                    .child(text(
                        fit(kind.description(), width - 66.0, 10.0),
                        muted(),
                        10.0,
                    )),
            ));
            rows.push(with_help(
                Box::new(row),
                Help::new(kind.description()).instance(if category == EffectCategory::Audio {
                    1
                } else {
                    0
                }),
                state,
            ));
        }
        if rows.len() == 1 {
            rows.push(text("No matching effects", faint(), 10.0));
        }
        groups.push(Box::new(Flex::column().gap(6.0).with_children(rows)) as BoxedWidget);
    }
    Box::new(
        Flex::column()
            .width(width)
            .gap(8.0)
            .padding(10.0)
            .background(panel())
            .corner_radius(10.0)
            .child(library_tabs(state))
            .child(Box::new(
                TextInput::controlled(&state.effect_query)
                    .width(width - 20.0)
                    .height(30.0)
                    .placeholder("Search effects"),
            ))
            .child(Box::new(
                RawScrollView::controlled(fill_style(), state.effect_scroll.clone())
                    .child(Box::new(Flex::column().gap(16.0).with_children(groups))),
            )),
    )
}

fn transitions_gallery(state: &UiState, width: f32) -> BoxedWidget {
    let mut rows = Flex::column()
        .width(width)
        .padding(10.0)
        .gap(8.0)
        .background(panel())
        .corner_radius(10.0)
        .child(library_tabs(state))
        .child(heading("Video transitions", muted(), 11.0));
    for (label, description, kind) in [
        (
            "Fade",
            "Dip through black or reveal the layer below.",
            TransitionKind::Fade,
        ),
        (
            "Dissolve",
            "Blend smoothly between clips.",
            TransitionKind::Dissolve,
        ),
        (
            "Slide",
            "Move the clip in or out from an edge.",
            TransitionKind::Slide,
        ),
    ] {
        let (start, drag, end) = (state.clone(), state.clone(), state.clone());
        rows = rows.child(Box::new(
            RawButton::new(
                Style::default()
                    .size(width - 20.0, 46.0)
                    .padding_all(8.0)
                    .flex_row()
                    .gap(8.0),
                || {},
            )
            .with_drag_start(move |p, rect| {
                start.begin_transition_drag(
                    kind,
                    Point {
                        x: p.x + rect.x,
                        y: p.y + rect.y,
                    },
                )
            })
            .with_drag(move |p, rect| {
                drag.drag_pointer(Point {
                    x: p.x + rect.x,
                    y: p.y + rect.y,
                })
            })
            .with_drag_end(move || end.end_effect_drag())
            .background(panel())
            .hover_style(StateStyle::new().background(surface()))
            .corner_radius(5.0)
            .child(glyph(Glyph::Film, VIDEO, 18.0))
            .child(Box::new(
                Flex::column()
                    .gap(3.0)
                    .child(heading(label, text_color(), 11.0))
                    .child(text(fit(description, width - 66.0, 10.0), muted(), 10.0)),
            )),
        ));
    }
    Box::new(rows.child(text(
        "Drag onto a clip: left half in · right half out",
        faint(),
        10.0,
    )))
}

fn library_tabs(state: &UiState) -> BoxedWidget {
    let tab = state.library_tab.get();
    let compact = state.tooltip_viewport.get().width < 1200.0;
    let mut row = Flex::row().gap(2.0).align(Align::Center);
    for (label, value, sidebar) in [
        ("Media", LibraryTab::Media, Sidebar::Media),
        ("Effects", LibraryTab::Effects, Sidebar::Effects),
        ("Transitions", LibraryTab::Transitions, Sidebar::Transitions),
    ] {
        row = row.child(chip(
            label,
            tab == value && (!compact || state.sidebar.get() != Sidebar::Inspector),
            state,
            move |s| {
                s.library_tab.set(value);
                s.sidebar.set(sidebar);
            },
        ));
    }
    if compact {
        row = row.child(chip(
            "Inspector",
            state.sidebar.get() == Sidebar::Inspector,
            state,
            |s| s.sidebar.set(Sidebar::Inspector),
        ));
    }
    Box::new(row)
}

fn library_panel(state: &UiState, project: &Project, width: f32, busy: bool) -> BoxedWidget {
    match state.library_tab.get() {
        LibraryTab::Media => media_bin(state, project, width, busy),
        LibraryTab::Effects => effects_gallery(state, width),
        LibraryTab::Transitions => transitions_gallery(state, width),
    }
}

fn media_bin(state: &UiState, project: &Project, width: f32, busy: bool) -> BoxedWidget {
    let fps = (
        project.settings().frame_rate.numerator,
        project.settings().frame_rate.denominator,
    );
    let query = state.media_query.value().to_lowercase();
    let filter = state.media_filter.get();
    let view = state.media_view.get();
    let selected = state
        .selected_media
        .get()
        .filter(|_| state.selected.get().is_empty());
    let inner = width - 20.0;
    let items: Vec<BoxedWidget> = project
        .media()
        .iter()
        .filter(|media| {
            (match filter {
                MediaFilter::All => true,
                MediaFilter::Video => media.kind == MediaKind::Video,
                MediaFilter::Audio => media.kind == MediaKind::Audio,
                MediaFilter::Image => media.kind == MediaKind::Image,
            }) && media_name(media).to_lowercase().contains(&query)
        })
        .map(|media| {
            let id = media.id;
            let missing = state.missing_media.get().contains(&id);
            let name = media_name(media);
            let image = (!missing)
                .then(|| state.thumbnails.borrow().get(&media.path).cloned())
                .flatten();
            let insertable = !busy && !missing && media_frames(media, fps).is_some();
            let insert = sized_icon_button(
                Glyph::Plus,
                Help::new("Insert at timeline end").instance(id),
                24.0,
                insertable,
                false,
                state,
                move |s| s.insert_media(id),
            );
            let badge = if missing {
                Some("Missing".into())
            } else if let Some(status) = state
                .proxy_status
                .get()
                .get(&media.path)
                .filter(|status| !status.is_empty())
            {
                Some(status.clone())
            } else {
                media
                    .duration_us
                    .map(|duration| short_duration(duration as f64 / 1_000_000.0))
            };
            if view == MediaView::Grid {
                let tile = (inner - 8.0) / 2.0;
                media_item(
                    Style::default().flex_column().gap(6.0).padding_all(5.0),
                    selected == Some(id),
                    vec![
                        Box::new(Thumb {
                            image,
                            placeholder: state.thumbnail_label(media),
                            kind: media.kind,
                            width: None,
                            height: ((tile - 10.0) * 9.0 / 16.0).round(),
                            badge,
                        }),
                        Box::new(
                            Flex::row()
                                .gap(4.0)
                                .align(Align::Center)
                                .child(Box::new(
                                    Flex::column()
                                        .gap(2.0)
                                        .grow(1.0)
                                        .child(text(
                                            fit(&name, tile - 40.0, 11.0),
                                            text_color(),
                                            11.0,
                                        ))
                                        .child(text(
                                            match media.kind {
                                                MediaKind::Video if media.has_audio => {
                                                    "Video + audio"
                                                }
                                                MediaKind::Video => "Video",
                                                MediaKind::Audio => "Audio",
                                                MediaKind::Image => "Still",
                                            },
                                            kind_color(media.kind).mix(muted(), 0.5),
                                            10.0,
                                        )),
                                ))
                                .child(insert),
                        ),
                    ],
                    state,
                    id,
                )
            } else {
                media_item(
                    Style::default()
                        .flex_row()
                        .align(Align::Center)
                        .gap(9.0)
                        .padding_all(5.0),
                    selected == Some(id),
                    vec![
                        Box::new(Thumb {
                            image,
                            placeholder: state.thumbnail_label(media),
                            kind: media.kind,
                            width: Some(64.0),
                            height: 36.0,
                            badge: None,
                        }),
                        Box::new(
                            Flex::column()
                                .gap(3.0)
                                .grow(1.0)
                                .child(text(fit(&name, inner - 120.0, 12.0), text_color(), 12.0))
                                .child(text(
                                    if missing {
                                        "Missing · Relink to restore".into()
                                    } else {
                                        let status = state
                                            .proxy_status
                                            .get()
                                            .get(&media.path)
                                            .cloned()
                                            .unwrap_or_default();
                                        if status.is_empty() {
                                            media_details(media)
                                        } else {
                                            format!("{} · {status}", media_details(media))
                                        }
                                    },
                                    muted(),
                                    10.0,
                                )),
                        ),
                        insert,
                    ],
                    state,
                    id,
                )
            }
        })
        .collect();
    let count = items.len();
    let content: BoxedWidget = if count == 0 {
        let empty = project.media().is_empty();
        Box::new(
            Flex::column()
                .padding_xy(0.0, 24.0)
                .align(Align::Center)
                .child(stage_message(
                    if empty { Glyph::Import } else { Glyph::Search },
                    if empty {
                        "Your media bin is empty"
                    } else {
                        "No matching media"
                    },
                    if empty {
                        "Import or drop video, audio and stills."
                    } else {
                        "Try another search or filter."
                    },
                    empty.then(|| {
                        accent_button(
                            Some(Glyph::Import),
                            "Import media",
                            !busy,
                            state,
                            UiState::import_dialog,
                        )
                    }),
                )),
        )
    } else if view == MediaView::Grid {
        Box::new(Grid::new().columns(2).gap(8.0).with_children(items))
    } else {
        Box::new(Flex::column().gap(5.0).with_children(items))
    };
    let count_of = |kind: Option<MediaKind>| {
        project
            .media()
            .iter()
            .filter(|media| kind.is_none_or(|kind| media.kind == kind))
            .count()
    };
    let filters = [
        (None, Some("All"), None, MediaFilter::All),
        (
            Some(Glyph::Film),
            None,
            Some(MediaKind::Video),
            MediaFilter::Video,
        ),
        (
            Some(Glyph::Music),
            None,
            Some(MediaKind::Audio),
            MediaFilter::Audio,
        ),
        (
            Some(Glyph::Picture),
            None,
            Some(MediaKind::Image),
            MediaFilter::Image,
        ),
    ]
    .into_iter()
    .map(|(icon, label, kind, option)| {
        segment(
            icon,
            label,
            Help::new(match option {
                MediaFilter::All => "Show all media",
                MediaFilter::Video => "Show videos",
                MediaFilter::Audio => "Show audio",
                MediaFilter::Image => "Show images",
            }),
            count_of(kind),
            filter == option,
            state,
            move |s| s.media_filter.set(option),
        )
    })
    .collect();
    Box::new(append(
        Flex::column()
            .gap(8.0)
            .padding(10.0)
            .width(width)
            .background(panel())
            .corner_radius(10.0),
        vec![
            {
                let actions = Box::new(
                    Flex::row()
                        .gap(2.0)
                        .child(sized_icon_button(
                            Glyph::Grid,
                            Help::new("Grid view"),
                            26.0,
                            true,
                            view == MediaView::Grid,
                            state,
                            |s| s.media_view.set(MediaView::Grid),
                        ))
                        .child(sized_icon_button(
                            Glyph::List,
                            Help::new("List view"),
                            26.0,
                            true,
                            view == MediaView::List,
                            state,
                            |s| s.media_view.set(MediaView::List),
                        ))
                        .child(sized_icon_button(
                            Glyph::Import,
                            Help::new("Import media").shortcut("Ctrl+I"),
                            26.0,
                            !busy,
                            false,
                            state,
                            |s| s.import_dialog(),
                        )),
                );
                if state.tooltip_viewport.get().width < 1200.0 {
                    Box::new(
                        Flex::column()
                            .gap(4.0)
                            .child(library_tabs(state))
                            .child(Box::new(Flex::row().justify(Justify::End).child(actions))),
                    ) as BoxedWidget
                } else {
                    Box::new(
                        Flex::row()
                            .align(Align::Center)
                            .child(library_tabs(state))
                            .child(Box::new(Flex::row().grow(1.0)))
                            .child(actions),
                    ) as BoxedWidget
                }
            },
            if state.missing_media.get().is_empty() {
                Box::new(Flex::row())
            } else {
                Box::new(
                    Flex::row()
                        .align(Align::Center)
                        .gap(8.0)
                        .child(text(
                            format!("{} files missing", state.missing_media.get().len()),
                            text_color(),
                            11.0,
                        ))
                        .child(accent_button(
                            Some(Glyph::Locate),
                            "Relink…",
                            !busy,
                            state,
                            |s| s.relink_dialog(None),
                        )),
                )
            },
            Box::new(
                TextInput::controlled(&state.media_query)
                    .width(inner)
                    .height(30.0)
                    .placeholder("Search media"),
            ),
            Box::new(
                Flex::row()
                    .gap(3.0)
                    .padding(3.0)
                    .background(background())
                    .border(border(), 1.0)
                    .corner_radius(9.0)
                    .with_children(filters),
            ),
            Box::new(
                RawScrollView::controlled(fill_style(), state.media_scroll.clone()).child(content),
            ),
            text(
                "Drag into the timeline · drop files anywhere",
                faint(),
                10.0,
            ),
        ],
    ))
}

fn input(controller: &TextController, width: f32) -> BoxedWidget {
    Box::new(TextInput::controlled(controller).width(width).height(30.0))
}

fn title_input(
    controller: &TextController,
    width: f32,
    state: &UiState,
    apply: fn(&UiState),
) -> TextInput {
    let state = state.clone();
    TextInput::controlled(controller)
        .width(width)
        .height(30.0)
        .on_submit(move || apply(&state))
}

/// The inspector for several selected clips: a summary, the actions that
/// apply to all of them and the list of clips.
fn selection_rows(state: &UiState, project: &Project, inner: f32) -> Vec<BoxedWidget> {
    let fps = (
        project.settings().frame_rate.numerator,
        project.settings().frame_rate.denominator,
    );
    let labels = track_labels(project);
    let selection = state.selected.get();
    let clips: Vec<_> = selection
        .iter()
        .filter_map(|&(track_id, clip_id)| {
            let track = project.tracks().iter().find(|track| track.id == track_id)?;
            let clip = track.clips.iter().find(|clip| clip.id == clip_id)?;
            Some((track_id, track.kind, clip))
        })
        .collect();
    let first = clips
        .iter()
        .map(|(_, _, clip)| clip.start_frame)
        .min()
        .unwrap_or(0);
    let last = clips
        .iter()
        .map(|(_, _, clip)| clip.start_frame + clip.duration_frames)
        .max()
        .unwrap_or(0);
    let total: u64 = clips.iter().map(|(_, _, clip)| clip.duration_frames).sum();
    let (mut video, mut audio, mut titles) = (0, 0, 0);
    for (_, kind, clip) in &clips {
        match (&clip.content, kind) {
            (ClipContent::Title(_), _) => titles += 1,
            (_, TrackKind::Audio) => audio += 1,
            _ => video += 1,
        }
    }
    let kinds = [(video, "video"), (audio, "audio"), (titles, "title")]
        .into_iter()
        .filter(|(count, _)| *count > 0)
        .map(|(count, kind)| {
            format!(
                "{count} {kind}{}",
                if count > 1 && kind == "title" {
                    "s"
                } else {
                    ""
                }
            )
        })
        .collect::<Vec<_>>()
        .join("  ·  ");
    let mut tracks: Vec<String> = Vec::new();
    for (track_id, _, _) in &clips {
        if let Some((_, label)) = labels.iter().find(|(id, _)| id == track_id)
            && !tracks.contains(label)
        {
            tracks.push(label.clone());
        }
    }
    let (detachable, linkable) = state.audio_link_state();
    let (audible, all_muted) = state.audible_selection();
    let splittable = !state.split_targets().is_empty();
    let mut actions = vec![
        secondary_button(Some(Glyph::Copy), "Copy", true, state, |s| {
            s.run(Action::Copy)
        }),
        secondary_button(Some(Glyph::Copy), "Duplicate", true, state, |s| {
            s.run(Action::Duplicate)
        }),
        secondary_button(Some(Glyph::Split), "Split", splittable, state, |s| {
            s.split_at_playhead()
        }),
    ];
    if !audible.is_empty() {
        actions.push(secondary_button(
            Some(if all_muted {
                Glyph::Volume
            } else {
                Glyph::Mute
            }),
            if all_muted { "Unmute" } else { "Mute" },
            true,
            state,
            UiState::toggle_mute,
        ));
    }
    if detachable {
        actions.push(secondary_button(
            Some(Glyph::Unlink),
            "Detach audio",
            true,
            state,
            UiState::detach_audio,
        ));
    }
    if linkable {
        actions.push(secondary_button(
            Some(Glyph::Link),
            "Link audio",
            true,
            state,
            UiState::link_audio,
        ));
    }
    actions.push(secondary_button(
        Some(Glyph::Trash),
        "Delete",
        true,
        state,
        |s| s.delete_selection(false),
    ));
    actions.push(secondary_button(
        Some(Glyph::Ripple),
        "Ripple delete",
        true,
        state,
        |s| s.delete_selection(true),
    ));
    const SHOWN: usize = 12;
    let mut list: Vec<BoxedWidget> = clips
        .iter()
        .take(SHOWN)
        .map(|&(track_id, kind, clip)| {
            let clip_ref = (track_id, clip.id);
            let (name, color) = match &clip.content {
                ClipContent::Title(title) => (title.text.clone(), TITLE),
                ClipContent::Media { media_id } => {
                    let media = project.media().iter().find(|media| media.id == *media_id);
                    (
                        media.map_or_else(|| "Missing source".into(), media_name),
                        if kind == TrackKind::Audio {
                            AUDIO
                        } else {
                            media.map_or(VIDEO, |media| kind_color(media.kind))
                        },
                    )
                }
            };
            let track = labels
                .iter()
                .find(|(id, _)| *id == track_id)
                .map_or_else(String::new, |(_, label)| label.clone());
            Box::new(
                Flex::row()
                    .gap(2.0)
                    .align(Align::Center)
                    .child(Box::new(Flex::row().grow(1.0).child(list_row(
                        vec![
                                Box::new(
                                    Flex::row()
                                        .size(3.0, 20.0)
                                        .background(color)
                                        .corner_radius(1.5),
                                ),
                                Box::new(
                                    Flex::column()
                                        .grow(1.0)
                                        .gap(1.0)
                                        .child(text(
                                            fit(&name, inner - 110.0, 11.0),
                                            text_color(),
                                            11.0,
                                        ))
                                        .child(text(
                                            format!(
                                                "{track}  ·  {}{}",
                                                timecode(clip.start_frame, fps),
                                                if clip.linked_audio {
                                                    "  ·  linked audio"
                                                } else {
                                                    ""
                                                }
                                            ),
                                            faint(),
                                            10.0,
                                        )),
                                ),
                                text(
                                    short_duration(
                                        clip.duration_frames as f64 * f64::from(fps.1)
                                            / f64::from(fps.0.max(1)),
                                    ),
                                    muted(),
                                    10.0,
                                ),
                            ],
                        state,
                        move |s| s.select_clip(Some(clip_ref)),
                    ))))
                    .child(sized_icon_button(
                        Glyph::Close,
                        Help::new("Deselect clip").instance(clip_ref.1),
                        22.0,
                        true,
                        false,
                        state,
                        move |s| s.toggle_clip(clip_ref),
                    )),
            ) as BoxedWidget
        })
        .collect();
    if clips.len() > SHOWN {
        list.push(text(
            format!("+ {} more", clips.len() - SHOWN),
            faint(),
            10.0,
        ));
    }
    vec![
        Box::new(
            Flex::column()
                .gap(10.0)
                .padding(14.0)
                .background(panel_raised())
                .border(border(), 1.0)
                .corner_radius(10.0)
                .child(Box::new(
                    Flex::row()
                        .gap(12.0)
                        .align(Align::Center)
                        .child(Box::new(
                            Flex::row()
                                .size(42.0, 42.0)
                                .align(Align::Center)
                                .justify(Justify::Center)
                                .background(Color::rgba(92, 208, 190, 30))
                                .corner_radius(10.0)
                                .child(heading(clips.len().to_string(), accent(), 18.0)),
                        ))
                        .child(Box::new(
                            Flex::column()
                                .gap(3.0)
                                .child(heading("clips selected", text_color(), 13.0))
                                .child(text(kinds, muted(), 10.0)),
                        )),
                ))
                .child(Box::new(
                    Flex::row().height(1.0).full_width().background(border()),
                ))
                .child(property(
                    "Span",
                    format!("{} → {}", seconds(first, fps), seconds(last, fps)),
                ))
                .child(property("Total length", seconds(total, fps)))
                .child(property("Tracks", tracks.join(", "))),
        ),
        section(
            "Actions",
            vec![Box::new(
                Grid::new().columns(2).gap(6.0).with_children(actions),
            )],
        ),
        section(
            "Clips",
            vec![
                Box::new(Flex::column().gap(1.0).with_children(list)),
                text(
                    "Drag any clip to move them all · Ctrl+click to add or remove",
                    faint(),
                    10.0,
                ),
            ],
        ),
    ]
}

fn compatible_destinations(tracks: &[Track], track_id: u64, clip: &Clip) -> Vec<(u64, bool)> {
    let Some(source) = tracks.iter().find(|track| track.id == track_id) else {
        return Vec::new();
    };
    tracks
        .iter()
        .filter(|track| {
            !track.locked
                && track.id != track_id
                && track.kind == source.kind
                && track.subtitles == source.subtitles
        })
        .map(|track| {
            let occupied = track.clips.iter().any(|other| {
                clip.start_frame < other.start_frame.saturating_add(other.duration_frames)
                    && other.start_frame < clip.start_frame.saturating_add(clip.duration_frames)
            });
            (track.id, occupied)
        })
        .collect()
}

fn inspector(state: &UiState, project: &Project, width: f32, busy: bool) -> BoxedWidget {
    let fps = (
        project.settings().frame_rate.numerator,
        project.settings().frame_rate.denominator,
    );
    let inner = width - 24.0;
    let labels = track_labels(project);
    let label_of = |id: u64| {
        labels
            .iter()
            .find(|(track, _)| *track == id)
            .map_or_else(String::new, |(_, label)| label.clone())
    };
    let mut rows: Vec<BoxedWidget> = Vec::new();
    let subtitle;
    let count = state.selected.get().len();
    let (_, linkable) = state.audio_link_state();
    if count > 1 {
        subtitle = "Selection";
        rows.extend(selection_rows(state, project, inner));
    } else if let Some((track_id, clip)) = selected_clip(state) {
        let track = project.tracks().iter().find(|track| track.id == track_id);
        let track_kind = track.map_or(TrackKind::Video, |track| track.kind);
        let media = match clip.content {
            ClipContent::Media { media_id } => {
                project.media().iter().find(|media| media.id == media_id)
            }
            ClipContent::Title(_) => None,
        };
        let (name, color) = match (&clip.content, media) {
            (ClipContent::Title(title), _) => (title.text.clone(), TITLE),
            (_, Some(media)) if track_kind == TrackKind::Audio => (media_name(media), AUDIO),
            (_, Some(media)) => (media_name(media), kind_color(media.kind)),
            _ => ("Missing source".into(), muted()),
        };
        if let Some((target, detail)) = state
            .selected_detail
            .get()
            .filter(|(target, _)| *target == (track_id, clip.id))
        {
            let title = match detail {
                ClipDetail::Effect(kind) => kind.label(),
                ClipDetail::Transition(false) => "Incoming transition",
                ClipDetail::Transition(true) => "Outgoing transition",
            };
            let content = match detail {
                ClipDetail::Effect(_) => effects_section(state, track_id, &clip, inner, busy),
                ClipDetail::Transition(out) => Box::new(
                    Flex::column()
                        .gap(10.0)
                        .child(heading(title, text_color(), 13.0))
                        .child(transition_row(state, &clip, busy, out))
                        .child(secondary_button(
                            None,
                            "Remove",
                            !busy && track.is_some_and(|track| !track.locked),
                            state,
                            move |s| {
                                edit_transition(s, target.0, target.1, out, None);
                            },
                        )),
                ) as BoxedWidget,
            };
            return Box::new(
                Flex::column()
                    .width(width)
                    .gap(12.0)
                    .padding(12.0)
                    .background(panel())
                    .corner_radius(10.0)
                    .child(if state.tooltip_viewport.get().width < 1200.0 {
                        library_tabs(state)
                    } else {
                        panel_header("Inspector", text("Selection", faint(), 11.0), Vec::new())
                    })
                    .child(Box::new(
                        Flex::row()
                            .gap(6.0)
                            .align(Align::Center)
                            .child(secondary_button(
                                None,
                                &fit(&name, inner * 0.5 - 20.0, 11.0),
                                true,
                                state,
                                |s| s.selected_detail.set(None),
                            ))
                            .child(text("›", faint(), 11.0))
                            .child(text(fit(title, inner * 0.5 - 6.0, 11.0), muted(), 11.0)),
                    ))
                    .child(Box::new(
                        RawScrollView::controlled(fill_style(), state.inspector_scroll.clone())
                            .child(content),
                    )),
            );
        }
        subtitle = match (&clip.content, track_kind) {
            (ClipContent::Title(_), _) => "Title clip",
            (_, TrackKind::Audio) => "Audio clip",
            _ if media.is_some_and(|media| media.kind == MediaKind::Image) => "Still clip",
            _ if clip.linked_audio => "Video + linked audio",
            _ => "Video clip",
        };
        if let Some(media) = media {
            rows.push(Box::new(Thumb {
                placeholder: (track_kind == TrackKind::Video)
                    .then(|| state.thumbnail_label(media))
                    .flatten(),
                image: (track_kind == TrackKind::Video)
                    .then(|| state.clip_thumbnail(&clip, media, fps))
                    .flatten(),
                kind: if track_kind == TrackKind::Audio {
                    MediaKind::Audio
                } else {
                    media.kind
                },
                width: None,
                height: 110.0,
                badge: None,
            }));
        }
        rows.push(Box::new(
            Flex::row()
                .gap(7.0)
                .align(Align::Center)
                .child(dot(color, 8.0))
                .child(heading(fit(&name, inner - 20.0, 13.0), text_color(), 13.0)),
        ));
        let mut timing = vec![
            property("Track", label_of(track_id)),
            property("Start", timecode(clip.start_frame, fps)),
            property(
                "Duration",
                format!(
                    "{}  ·  {} f",
                    timecode(clip.duration_frames, fps),
                    clip.duration_frames
                ),
            ),
        ];
        if matches!(clip.content, ClipContent::Media { .. }) {
            timing.push(timing_steps(state, true));
        }
        timing.push(timing_steps(state, false));
        rows.push(section("Timing", timing));

        if let ClipContent::Title(title) = &clip.content {
            rows.push(section(
                "Title",
                vec![
                    text("Duration (seconds)", muted(), 11.0),
                    Box::new(title_input(
                        &state.title_duration,
                        inner,
                        state,
                        set_title_duration,
                    )),
                    inline(secondary_button(
                        None,
                        "Apply duration",
                        true,
                        state,
                        set_title_duration,
                    )),
                    text("Text", muted(), 11.0),
                    Box::new(
                        TextArea::controlled(&state.title_text)
                            .width(inner)
                            .height(112.0)
                            .wrap(true),
                    ),
                    inline(secondary_button(
                        Some(Glyph::Title),
                        "Apply text",
                        true,
                        state,
                        set_title_text,
                    )),
                    title_toggle(state, "Parse HTML tags", title.parse_html, |title| {
                        title.parse_html = !title.parse_html
                    }),
                    title_toggle(state, "Parse Markdown", title.parse_markdown, |title| {
                        title.parse_markdown = !title.parse_markdown
                    }),
                ],
            ));
            let subtitles = project
                .tracks()
                .iter()
                .any(|track| track.id == track_id && track.subtitles);
            rows.push(section(
                if subtitles {
                    "Subtitle style"
                } else {
                    "Title style"
                },
                vec![
                    text("Font family", muted(), 11.0),
                    Box::new(title_input(
                        &state.title_font,
                        inner,
                        state,
                        apply_title_style,
                    )),
                    numeric_row_preview(
                        state,
                        NumericSpec {
                            label: "Font size (px)",
                            min: 0.0,
                            max: f64::from(f32::MAX),
                            slider_max: Some(f64::from(title.font_size).max(256.0)),
                            step: 1.0,
                            precision: 1,
                            unit: "",
                            default: 64.0,
                        },
                        f64::from(title.font_size),
                        !busy,
                        |clip, value| {
                            if let ClipContent::Title(title) = &mut clip.content {
                                title.font_size = value as f32;
                            }
                        },
                        |state, value| {
                            change_title(state, |title| title.font_size = value as f32);
                            if let Some((
                                _,
                                Clip {
                                    content: ClipContent::Title(title),
                                    ..
                                },
                            )) = selected_clip(state)
                            {
                                state.title_size.set_value(title.font_size.to_string());
                            }
                        },
                    ),
                    text("Color (#RRGGBB or #RRGGBBAA)", muted(), 11.0),
                    Box::new(
                        ColorPicker::controlled(
                            parse_hex_color(&state.title_color.value()).unwrap_or_else(|| {
                                Color::rgba(
                                    title.color[0],
                                    title.color[1],
                                    title.color[2],
                                    title.color[3],
                                )
                            }),
                            &state.title_picker,
                            {
                                let controller = state.title_color.clone();
                                move |color: Color| {
                                    controller.set_value(format!(
                                        "#{:02X}{:02X}{:02X}{:02X}",
                                        color.r, color.g, color.b, color.a
                                    ));
                                }
                            },
                        )
                        .width(inner),
                    ),
                    Box::new(title_input(
                        &state.title_color,
                        inner,
                        state,
                        apply_title_style,
                    )),
                    inline(secondary_button(
                        None,
                        "Apply style",
                        true,
                        state,
                        apply_title_style,
                    )),
                    title_toggle(state, "Bold", title.bold, |title| title.bold = !title.bold),
                    title_toggle(state, "Italic", title.italic, |title| {
                        title.italic = !title.italic
                    }),
                    title_toggle(state, "Outline", title.outline, |title| {
                        title.outline = !title.outline
                    }),
                    title_toggle(state, "Background", title.background.is_some(), |title| {
                        title.background = if title.background.is_some() {
                            None
                        } else {
                            Some([0, 0, 0, 179])
                        };
                    }),
                    text("Background (#RRGGBBAA)", muted(), 11.0),
                    Box::new(title_input(
                        &state.title_background,
                        inner,
                        state,
                        apply_title_background,
                    )),
                    inline(secondary_button(
                        None,
                        "Apply background",
                        true,
                        state,
                        apply_title_background,
                    )),
                    text("Position", muted(), 11.0),
                    Box::new(
                        Flex::row()
                            .gap(4.0)
                            .child(secondary_button(None, "Bottom", true, state, |state| {
                                change_title(state, |title| {
                                    title.x = 0.5;
                                    title.y = 0.9;
                                })
                            }))
                            .child(secondary_button(None, "Top", true, state, |state| {
                                change_title(state, |title| {
                                    title.x = 0.5;
                                    title.y = 0.1;
                                })
                            }))
                            .child(secondary_button(None, "Middle", true, state, |state| {
                                change_title(state, |title| {
                                    title.x = 0.5;
                                    title.y = 0.5;
                                })
                            })),
                    ),
                ],
            ));
            if !subtitles {
                let mut animation_buttons = Vec::new();
                for (label, animation, help) in [
                    (
                        "None",
                        crate::timeline::TitleAnimation::None,
                        "Keep the title still and fully visible",
                    ),
                    (
                        "Fade",
                        crate::timeline::TitleAnimation::Fade,
                        "Fade the title in and out",
                    ),
                    (
                        "Slide up",
                        crate::timeline::TitleAnimation::SlideUp,
                        "Raise the title gently as it appears",
                    ),
                    (
                        "Pop",
                        crate::timeline::TitleAnimation::Pop,
                        "Grow the title as it fades in",
                    ),
                    (
                        "Typewriter",
                        crate::timeline::TitleAnimation::Typewriter,
                        "Reveal the title one character at a time",
                    ),
                ] {
                    animation_buttons.push(with_help(
                        chip(label, title.animation == animation, state, move |state| {
                            change_title(state, |title| title.animation = animation)
                        }),
                        Help::new(help),
                        state,
                    ));
                }
                let mut templates = Vec::new();
                for (label, animation) in crate::timeline::TITLE_TEMPLATES {
                    templates.push(with_help(
                        secondary_button(None, label, true, state, move |state| {
                            change_title(state, |title| title.apply_template(animation))
                        }),
                        Help::new(match animation {
                            crate::timeline::TitleAnimation::Fade => {
                                "Apply a centered white title with a gentle fade"
                            }
                            crate::timeline::TitleAnimation::SlideUp => {
                                "Apply a lower third with a dark translucent box"
                            }
                            crate::timeline::TitleAnimation::Pop => {
                                "Apply a large bold outlined title that pops in"
                            }
                            _ => "Apply a monospace title with a character reveal",
                        }),
                        state,
                    ));
                }
                rows.push(section(
                    "Title animation",
                    vec![
                        text("Animation", muted(), 11.0),
                        Box::new(
                            Grid::new()
                                .columns(3)
                                .gap(4.0)
                                .with_children(animation_buttons),
                        ),
                        text("Template", muted(), 11.0),
                        Box::new(Grid::new().columns(2).gap(4.0).with_children(templates)),
                    ],
                ));
            }
        }
        if track_kind == TrackKind::Video && matches!(clip.content, ClipContent::Media { .. }) {
            rows.push(section(
                "Transform",
                vec![Box::new(append(
                    Flex::column().gap(3.0),
                    transform_rows(
                        state,
                        clip.transform_at(state.program.position.get()),
                        inner,
                    ),
                ))],
            ));
        }
        if track_kind == TrackKind::Video
            && media.is_some_and(|media| media.kind != MediaKind::Audio)
        {
            rows.push(picture_in_picture_section(
                state,
                clip.transform,
                !busy && track.is_some_and(|track| !track.locked),
            ));
        }
        if clip.playback == Playback::Freeze {
            rows.push(section(
                "Audio",
                vec![text("Frozen frames are silent", muted(), 11.0)],
            ));
        } else if (track_kind == TrackKind::Audio || clip.linked_audio) && media.is_some() {
            let volume = clip.volume;
            rows.push(section(
                "Audio",
                vec![
                    numeric_row(state, NumericSpec {
                        label: "Volume", min: 0.0, max: 200.0, slider_max: None, step: 10.0,
                        precision: 1, unit: "%", default: 100.0,
                    }, f64::from(volume) * 100.0, !busy, |state, value| {
                        set_clip_audio(state, |clip| (value as f32 / 100.0, clip.muted));
                    }),
                    inline(secondary_button(
                        Some(if clip.muted {
                            Glyph::Volume
                        } else {
                            Glyph::Mute
                        }),
                        if clip.muted { "Unmute" } else { "Mute" },
                        true,
                        state,
                        |s| set_clip_audio(s, |clip| (clip.volume, !clip.muted)),
                    )),
                    inline(with_help(
                        secondary_button(
                            None,
                            "Normalize loudness",
                            true,
                            state,
                            UiState::normalize_loudness,
                        ),
                        Help::new(
                            "Set volume so this clip plays at a standard loudness (-14 LUFS)",
                        ),
                        state,
                    )),
                    inline(with_help(
                        secondary_button(
                            None,
                            "Remove silences",
                            !state.busy.get(),
                            state,
                            UiState::remove_silences,
                        ),
                        Help::new(
                            "Remove pauses of at least half a second, keeping a little breathing room. Ripples unlocked tracks to keep picture and sound together.",
                        ),
                        state,
                    )),
                    inline(with_help(
                        secondary_button(
                            None,
                            if state.transcription_cancel.borrow().is_some() {
                                "Cancel subtitles"
                            } else {
                                "Generate subtitles"
                            },
                            state.transcription_cancel.borrow().is_some()
                                || (!state.busy.get() && state.has_timeline_audio()),
                            state,
                            UiState::generate_subtitles,
                        ),
                        Help::new(
                            "Generate subtitles from the timeline audio locally with Whisper, or cancel generation",
                        ),
                        state,
                    )),
                    if clip.linked_audio {
                        inline(secondary_button(
                            Some(Glyph::Unlink),
                            "Detach audio to its own track",
                            true,
                            state,
                            UiState::detach_audio,
                        ))
                    } else if linkable {
                        inline(secondary_button(
                            Some(Glyph::Link),
                            "Link to its video",
                            true,
                            state,
                            UiState::link_audio,
                        ))
                    } else {
                        Box::new(Flex::row().height(0.0))
                    },
                    text(
                        "Volume and mute affect playback and export.",
                        faint(),
                        10.0,
                    ),
                ],
            ));
        }
        let destinations = compatible_destinations(project.tracks(), track_id, &clip);
        let mut move_buttons = Flex::row()
            .gap(4.0)
            .wrap(creamui::widgets::layout::Wrap::Wrap);
        for (target_track_id, occupied) in &destinations {
            let target_track_id = *target_track_id;
            let label = if *occupied {
                format!("{} · Occupied", label_of(target_track_id))
            } else {
                label_of(target_track_id)
            };
            move_buttons =
                move_buttons.child(secondary_button(None, &label, !occupied, state, move |s| {
                    edit_selection(s, |clip, track_id, _| Command::Move {
                        track_id,
                        target_track_id,
                        clip_id: clip.id,
                        to_frame: clip.start_frame,
                    })
                }));
        }
        let playhead = state.program.position.get();
        let splittable = playhead > clip.start_frame
            && playhead < clip.start_frame.saturating_add(clip.duration_frames);
        rows.push(section(
            "Arrange",
            vec![
                Box::new(
                    Flex::row()
                        .gap(4.0)
                        .align(Align::Center)
                        .child(Box::new(Flex::row().width(86.0).child(text(
                            "Position",
                            muted(),
                            11.0,
                        ))))
                        .child(sized_icon_button(
                            Glyph::ChevronLeft,
                            Help::new("Move clip one frame earlier").shortcut("Alt+Left"),
                            26.0,
                            clip.start_frame > 0,
                            false,
                            state,
                            |s| {
                                edit_selection(s, |clip, track_id, _| Command::Move {
                                    track_id,
                                    target_track_id: track_id,
                                    clip_id: clip.id,
                                    to_frame: clip.start_frame.saturating_sub(1),
                                })
                            },
                        ))
                        .child(sized_icon_button(
                            Glyph::ChevronRight,
                            Help::new("Move clip one frame later").shortcut("Alt+Right"),
                            26.0,
                            true,
                            false,
                            state,
                            |s| {
                                edit_selection(s, |clip, track_id, _| Command::Move {
                                    track_id,
                                    target_track_id: track_id,
                                    clip_id: clip.id,
                                    to_frame: clip.start_frame.saturating_add(1),
                                })
                            },
                        ))
                        .child(text("1 frame", faint(), 10.0)),
                ),
                timing_steps(state, false),
                Box::new(
                    Flex::column()
                        .gap(6.0)
                        .child(text("Move to", muted(), 11.0))
                        .child(if destinations.is_empty() {
                            text("No other compatible tracks", faint(), 10.0)
                        } else {
                            Box::new(move_buttons)
                        }),
                ),
            ],
        ));
        rows.push(section(
            "Edit",
            vec![Box::new(
                Flex::row()
                    .gap(6.0)
                    .wrap(creamui::widgets::layout::Wrap::Wrap)
                    .child(secondary_button(
                        Some(Glyph::Split),
                        "Split",
                        splittable,
                        state,
                        UiState::split_at_playhead,
                    ))
                    .child(secondary_button(
                        Some(Glyph::Trash),
                        "Delete",
                        true,
                        state,
                        |s| s.delete_selection(false),
                    ))
                    .child(secondary_button(
                        Some(Glyph::Ripple),
                        "Ripple",
                        true,
                        state,
                        |s| s.delete_selection(true),
                    )),
            )],
        ));
    } else if let Some(media) = state
        .selected_media
        .get()
        .and_then(|id| project.media().iter().find(|media| media.id == id))
    {
        subtitle = "Source file";
        let id = media.id;
        rows.push(Box::new(Thumb {
            placeholder: state.thumbnail_label(media),
            image: state.thumbnails.borrow().get(&media.path).cloned(),
            kind: media.kind,
            width: None,
            height: 110.0,
            badge: None,
        }));
        rows.push(Box::new(
            Flex::row()
                .gap(7.0)
                .align(Align::Center)
                .child(dot(kind_color(media.kind), 8.0))
                .child(heading(
                    fit(&media_name(media), inner - 20.0, 13.0),
                    text_color(),
                    13.0,
                )),
        ));
        rows.push(section(
            "Source",
            vec![
                property("Type", media_details(media)),
                property(
                    "Duration",
                    media_frames(media, fps)
                        .map_or_else(|| "Unknown".into(), |frames| timecode(frames, fps)),
                ),
                property(
                    "Folder",
                    media.path.parent().map_or_else(String::new, |parent| {
                        fit(&parent.display().to_string(), inner - 100.0, 11.0)
                    }),
                ),
            ],
        ));
        rows.push(Box::new(Flex::row().height(2.0)));
        rows.push(accent_button(
            Some(Glyph::Plus),
            "Insert at end of timeline",
            !busy && media_frames(media, fps).is_some(),
            state,
            move |s| s.insert_media(id),
        ));
    } else {
        subtitle = "Project";
        let end = state.sequence_end();
        let clips: usize = project.tracks().iter().map(|track| track.clips.len()).sum();
        rows.push(Box::new(
            Flex::column()
                .gap(6.0)
                .padding(12.0)
                .background(panel_raised())
                .corner_radius(8.0)
                .child(heading("Nothing selected", text_color(), 13.0))
                .child(text(
                    "Select a clip on the timeline to edit it,",
                    muted(),
                    11.0,
                ))
                .child(text(
                    "or a file in the media bin to view it.",
                    muted(),
                    11.0,
                )),
        ));
        rows.push(section(
            "Sequence",
            vec![
                property(
                    "Resolution",
                    format!(
                        "{} × {}",
                        project.settings().width,
                        project.settings().height
                    ),
                ),
                property("Frame rate", frame_rate_label(fps)),
                property("Duration", timecode(end, fps)),
                property("Clips", clips.to_string()),
                property("Sources", project.media().len().to_string()),
            ],
        ));
        let groups = super::interaction::shortcut_help()
            .into_iter()
            .map(|group| {
                let mut bindings = group.bindings;
                if group.title == "Timeline" {
                    bindings.push(("Ctrl + wheel".into(), "Zoom timeline"));
                }
                Box::new(append(
                    Flex::column()
                        .gap(5.0)
                        .child(heading(group.title, muted(), 10.0)),
                    bindings
                        .into_iter()
                        .map(|(keys, action)| {
                            Box::new(
                                Flex::row()
                                    .gap(8.0)
                                    .align(Align::Center)
                                    .child(Box::new(Flex::row().width(112.0).child(text(
                                        keys,
                                        text_color(),
                                        10.0,
                                    ))))
                                    .child(text(action, muted(), 10.0)),
                            ) as BoxedWidget
                        })
                        .collect(),
                )) as BoxedWidget
            })
            .collect();
        rows.push(section("Shortcuts", groups));
    }
    Box::new(
        Flex::column()
            .width(width)
            .gap(8.0)
            .padding(12.0)
            .background(panel())
            .corner_radius(10.0)
            .child(if state.tooltip_viewport.get().width < 1200.0 {
                library_tabs(state)
            } else {
                panel_header("Inspector", text(subtitle, faint(), 11.0), Vec::new())
            })
            .child(Box::new(
                RawScrollView::controlled(fill_style(), state.inspector_scroll.clone())
                    .child(Box::new(Flex::column().gap(10.0).with_children(rows))),
            )),
    )
}

fn timeline_lanes(state: &UiState, project: &Project, fps: (u32, u32)) -> Vec<Lane> {
    let labels = track_labels(project);
    let draft = state.trim_draft.get();
    project
        .tracks()
        .iter()
        .zip(&labels)
        .map(|(track, (_, label))| Lane {
            track_id: track.id,
            kind: track.kind,
            label: label.clone(),
            locked: track.locked,
            hidden: track.hidden,
            base: project
                .tracks()
                .iter()
                .find(|other| other.kind == TrackKind::Video)
                .is_some_and(|other| other.id == track.id),
            removable: !track.locked
                && project
                    .tracks()
                    .iter()
                    .filter(|other| other.kind == track.kind)
                    .count()
                    > 1,
            clips: track
                .clips
                .iter()
                .map(|clip| {
                    let preview = match &draft {
                        Some(Command::Trim {
                            track_id,
                            clip_id,
                            start_frame,
                            source_start_frame,
                            duration_frames,
                        }) if *track_id == track.id && *clip_id == clip.id => Some(Clip {
                            start_frame: *start_frame,
                            source_start_frame: *source_start_frame,
                            duration_frames: *duration_frames,
                            ..clip.clone()
                        }),
                        _ => None,
                    };
                    let clip = preview.as_ref().unwrap_or(clip);
                    let media = match clip.content {
                        ClipContent::Media { media_id } => {
                            project.media().iter().find(|media| media.id == media_id)
                        }
                        ClipContent::Title(_) => None,
                    };
                    let video = track.kind == TrackKind::Video;
                    let mut transition_frames = clip.transition.as_ref().map_or(0, |transition| {
                        transition.duration_frames.min(clip.duration_frames)
                    });
                    let mut transition_out_frames =
                        clip.transition_out.as_ref().map_or(0, |transition| {
                            transition.duration_frames.min(clip.duration_frames)
                        });
                    if transition_frames > clip.duration_frames - transition_out_frames {
                        transition_frames = transition_frames.min(clip.duration_frames / 2);
                        transition_out_frames = transition_out_frames.min(clip.duration_frames / 2);
                    }
                    LaneClip {
                        effects: clip.effect_stack.clone(),
                        keyframes: clip
                            .keyframes
                            .iter()
                            .filter(|key| key.frame < clip.duration_frames)
                            .map(|key| (clip.start_frame + key.frame, key.easing))
                            .collect(),
                        transition_frames,
                        playback: clip.playback,
                        transition_out_frames,
                        clip_id: clip.id,
                        start: clip.start_frame,
                        duration: clip.duration_frames,
                        name: match (&clip.content, media) {
                            (ClipContent::Title(title), _) => title.text.clone(),
                            (_, Some(media)) if state.missing_media.get().contains(&media.id) => {
                                format!("Missing source · {}", media_name(media))
                            }
                            (_, Some(media)) => media_name(media),
                            _ => "Missing source".into(),
                        },
                        look: match (&clip.content, media) {
                            (ClipContent::Title(_), _) => ClipLook::Title,
                            _ if !video => ClipLook::Audio,
                            (_, Some(media)) if media.kind == MediaKind::Image => ClipLook::Image,
                            _ => ClipLook::Video,
                        },
                        images: media
                            .filter(|_| video)
                            .and_then(|media| state.clip_storyboard(clip, media, fps)),
                        fallback: media
                            .filter(|_| video)
                            .and_then(|media| state.clip_thumbnail(clip, media, fps)),
                        waveform: media
                            .filter(|media| {
                                !state.missing_media.get().contains(&media.id)
                                    && carries_sound(track.kind, clip, media)
                            })
                            .and_then(|media| state.waveform(&media.path))
                            .map(|peaks| WaveformSlice {
                                peaks,
                                clip: clip.clone(),
                                fps,
                            }),
                        volume: clip.volume,
                        speed: clip.speed,
                        muted: clip.muted,
                        fade_in_frames: clip.fade_in_frames,
                        fade_out_frames: clip.fade_out_frames,
                        linked_audio: clip.linked_audio,
                    }
                })
                .collect(),
        })
        .collect()
}

pub(crate) fn build(state: &UiState, viewport: Size) -> BoxedWidget {
    state.begin_tooltips();
    for label in [
        "Previous keyframe",
        "Next keyframe",
        "Add keyframe at playhead",
        "Remove keyframe at playhead",
    ] {
        Help::new(label).register(state);
    }
    state.tooltip_viewport.set(viewport);
    state.sync_system_theme();
    state.revision.get();
    state.ui_revision.get();
    let editor = state.editor.borrow();
    let project = editor.project();
    let compact = viewport.width < 1200.0;
    let narrow = viewport.width < 1000.0;
    let busy = state.busy.get();
    let exports = state.export_queue.get();
    let exporting = exports.active().is_some();
    let can_add_export = !busy || exports.accepts_jobs_while_busy();
    let dirty = state.dirty.get();
    let fps = (
        project.settings().frame_rate.numerator,
        project.settings().frame_rate.denominator,
    );
    let aspect = project.settings().width as f32 / project.settings().height.max(1) as f32;
    let gpu_preview = state.preview_available.get();
    let project_label = state
        .project_path
        .get()
        .as_ref()
        .and_then(|path| path.file_name())
        .map_or_else(
            || project.name().to_owned(),
            |name| name.to_string_lossy().into_owned(),
        );
    let title_width = (viewport.width - 640.0 - if dirty { 80.0 } else { 0.0 }).clamp(0.0, 220.0);

    let titlebar = AppTitlebar::new(viewport.width)
        .height(46.0)
        .padding(12.0)
        .center_margin(32.0)
        .foreground(text_color())
        .background(panel())
        .start_boxed(Box::new(
            Flex::row()
                .gap(4.0)
                .align(Align::Center)
                .child(Box::new(Logo))
                .child(Box::new(Flex::row().width(4.0)))
                .child(heading("OCUS", text_color(), 15.0))
                .child(Box::new(Flex::row().width(10.0)))
                .child(divider())
                .child(Box::new(Flex::row().width(6.0)))
                .child(Box::new(
                    Flex::row().with_children(
                        [BarMenu::File, BarMenu::Edit, BarMenu::View, BarMenu::Help]
                            .into_iter()
                            .map(|menu| {
                                Box::new(MenuTitle {
                                    state: state.clone(),
                                    menu,
                                    rect: Rc::new(std::cell::Cell::new(Rect::default())),
                                }) as BoxedWidget
                            })
                            .collect(),
                    ),
                )),
        ))
        .center_boxed(Box::new(
            Flex::row()
                .gap(8.0)
                .align(Align::Center)
                .child(text(
                    fit(&project_label, title_width, 12.0),
                    text_color(),
                    12.0,
                ))
                .child(if dirty {
                    Box::new(
                        Flex::row()
                            .gap(5.0)
                            .align(Align::Center)
                            .padding_xy(7.0, 2.0)
                            .background(surface())
                            .corner_radius(9.0)
                            .child(dot(IMAGE, 6.0))
                            .child(text("Edited", muted(), 10.0)),
                    ) as BoxedWidget
                } else {
                    Box::new(Flex::row().width(0.0))
                }),
        ))
        .end_boxed(Box::new(Flex::row().gap(4.0).align(Align::Center).child({
            let button = accent_button(
                Some(Glyph::Export),
                if busy && (exporting || can_add_export) {
                    "Add export…"
                } else if project.last_export().is_some() {
                    "Export again"
                } else {
                    "Export"
                },
                can_add_export && state.sequence_end() > 0,
                state,
                |state| {
                    state.close_menu();
                    state.quick_export();
                },
            );
            if state.sequence_end() == 0 {
                with_help(
                    button,
                    Help::new("Add clips to the timeline to export"),
                    state,
                )
            } else {
                button
            }
        })));

    let frame = state.program.position.get();
    let timeline_end = state.sequence_end();
    let missing = state.missing_media.get();
    let (picture, audio, mut titles, lost) = program_layers(project, &missing, frame);
    if let Some((key, draft)) = state.title_preview.get()
        && let Some((_, title)) = titles.iter_mut().find(|(clip, _)| *clip == key)
    {
        *title = draft;
    }
    let empty_timeline = project.tracks().iter().all(|track| track.clips.is_empty());
    let first_media = project.media().first();
    let program_stage: BoxedWidget = if let Some(media) = lost {
        let id = media.id;
        stage_message(
            Glyph::Film,
            "Missing source",
            &format!("{} is missing at this frame.", media_name(media)),
            Some(accent_button(
                Some(Glyph::Locate),
                "Relink media…",
                !busy,
                state,
                move |s| s.relink_dialog(Some(id)),
            )),
        )
    } else if picture && !gpu_preview && titles.is_empty() {
        stage_message(
            Glyph::Film,
            "Video preview unavailable",
            "This display cannot show video preview.",
            None,
        )
    } else if picture || !titles.is_empty() {
        Box::new(ProgramCanvas {
            slot: state.program.slot.clone(),
            picture: (picture || !titles.is_empty()) && gpu_preview,
            picture_unavailable: picture && !gpu_preview,
            titles,
            selected: state.selected.get().last().copied(),
            state: state.clone(),
            aspect,
            resolution: (project.settings().width, project.settings().height),
        })
    } else if audio {
        stage_message(Glyph::Music, "Audio only", "No video at this frame.", None)
    } else if empty_timeline {
        stage_message(
            Glyph::Film,
            if first_media.is_some() {
                "Add footage to the timeline"
            } else {
                "Start your edit"
            },
            if first_media.is_some() {
                "Add a source to the timeline to start editing."
            } else {
                "Import footage, then add it to the timeline."
            },
            Some(match first_media {
                Some(media) => accent_button(
                    Some(Glyph::Plus),
                    "Add first source",
                    !busy && media_frames(media, fps).is_some(),
                    state,
                    {
                        let id = media.id;
                        move |state| state.insert_media(id)
                    },
                ),
                None => accent_button(
                    Some(Glyph::Import),
                    "Import media",
                    !busy,
                    state,
                    UiState::import_dialog,
                ),
            }),
        )
    } else {
        stage_message(
            Glyph::Film,
            "No clip at this frame",
            "Move the playhead over a clip.",
            None,
        )
    };
    let content = SequenceContent::of(project);
    let program_playing = state.program.playing.get();
    let program = monitor(
        "Program",
        PLAYHEAD,
        "Sequence".into(),
        vec![text(
            format!("{}p · {}", project.settings().height, frame_rate_label(fps)),
            faint(),
            10.0,
        )],
        preview_stage(state, PlaybackMonitor::Program).unwrap_or(program_stage),
        transport(
            state,
            Transport {
                program: true,
                position: frame,
                end: timeline_end,
                fps,
                playing: program_playing,
                can_play: content.playable && (gpu_preview || !content.has_picture),
                color: accent(),
                compact: narrow,
            },
            |state, target| {
                if state.program.position.peek() != target {
                    state.seek(target);
                }
            },
            |state| state.toggle_playback_monitor(PlaybackMonitor::Program),
        ),
    );

    let selection = state.selected.get();
    let primary = selection.last().copied();
    let selected_media = state.selected_media.get();
    let range: Option<SourceRange> = state.source_range();
    let selected_title = selected_clip(state).and_then(|(_, clip)| match clip.content {
        ClipContent::Title(title) => Some(title.text),
        ClipContent::Media { .. } => None,
    });
    let source_name = match (primary, selected_media) {
        (Some(_), _) => selected_clip(state).map(|(_, clip)| match &clip.content {
            ClipContent::Title(title) => title.text.clone(),
            ClipContent::Media { media_id } => project
                .media()
                .iter()
                .find(|media| media.id == *media_id)
                .map_or_else(|| "Missing source".into(), media_name),
        }),
        (None, Some(id)) => project
            .media()
            .iter()
            .find(|media| media.id == id)
            .map(media_name),
        (None, None) => None,
    };
    let missing_source = primary
        .and_then(|_| selected_clip(state))
        .and_then(|(_, clip)| match clip.content {
            ClipContent::Media { media_id } => Some(media_id),
            ClipContent::Title(_) => None,
        })
        .or_else(|| primary.is_none().then_some(selected_media).flatten())
        .filter(|id| missing.contains(id))
        .and_then(|id| project.media().iter().find(|media| media.id == id));
    let source_position = state.source.position.get();
    let source_stage: BoxedWidget = match (&range, &selected_title) {
        _ if missing_source.is_some() => {
            let media = missing_source.unwrap();
            let id = media.id;
            stage_message(
                Glyph::Film,
                "Missing source",
                &format!("{} is missing.", media_name(media)),
                Some(accent_button(
                    Some(Glyph::Locate),
                    "Relink media…",
                    !busy,
                    state,
                    move |s| s.relink_dialog(Some(id)),
                )),
            )
        }
        (Some(range), _) if range.kind == MediaKind::Audio => {
            if let Some(peaks) = state.waveform(&range.path) {
                Box::new(SourceWaveform {
                    waveform: WaveformSlice {
                        peaks,
                        clip: Clip {
                            effect_stack: Vec::new(),
                            stabilization: None,
                            keyframes: Vec::new(),
                            speed: 1.0,
                            transform: crate::timeline::Transform::default(),
                            noise_reduction: false,
                            effect: None,
                            mask: None,
                            lut: None,
                            chroma_key: None,
                            transition_out: None,
                            color: Default::default(),
                            id: 0,
                            transition: None,
                            start_frame: 0,
                            source_start_frame: range.source_start,
                            duration_frames: range.duration,
                            content: ClipContent::Media { media_id: 0 },
                            volume: 1.0,
                            fade_in_frames: 0,
                            fade_out_frames: 0,
                            muted: false,
                            linked_audio: false,
                            playback: range.playback,
                        },
                        fps,
                    },
                    position: source_position,
                })
            } else {
                stage_message(
                    Glyph::Music,
                    "Audio source",
                    "Audio plays through the default output device.",
                    None,
                )
            }
        }
        (Some(_), _) if gpu_preview => monitor_viewport(&state.source.slot, aspect),
        (Some(_), _) => stage_message(
            Glyph::Film,
            "Video preview unavailable",
            "This display cannot show video preview.",
            None,
        ),
        (None, Some(_)) => stage_message(
            Glyph::Title,
            "Title overlay",
            "See it over the footage in the Program monitor.",
            None,
        ),
        (None, None) => stage_message(
            Glyph::Locate,
            "No source selected",
            "Pick a clip on the timeline or a file in the bin.",
            None,
        ),
    };
    let source_stage = preview_stage(state, PlaybackMonitor::Source).unwrap_or(source_stage);
    let source_end = range.as_ref().map_or(0, |range| range.duration);
    let source_actions: Vec<BoxedWidget> =
        match (primary, selected_media) {
            (Some(_), _) => selected_clip(state)
                .filter(|_| range.is_some())
                .map(|(_, clip)| {
                    let start = clip.start_frame;
                    let speed = clip.speed;
                    tool_button(Some(Glyph::Locate), "Match frame", true, state, move |s| {
                        s.pause_source();
                        s.focus_frame(start.saturating_add(
                            (s.source.position.peek() as f64 / speed).floor() as u64,
                        ));
                    })
                })
                .into_iter()
                .collect(),
            (None, Some(id)) => vec![tool_button(
                Some(Glyph::Plus),
                "Insert",
                !busy && source_end > 0,
                state,
                move |s| s.insert_media(id),
            )],
            (None, None) => Vec::new(),
        };
    let source = monitor(
        "Source",
        VIDEO,
        source_name.map_or_else(
            || "Empty".into(),
            |name| fit(&name, if compact { 100.0 } else { 200.0 }, 11.0),
        ),
        source_actions,
        source_stage,
        transport(
            state,
            Transport {
                program: false,
                position: source_position,
                end: source_end,
                fps,
                playing: state.source.playing.get(),
                can_play: range
                    .as_ref()
                    .is_some_and(|r| gpu_preview || r.kind == MediaKind::Audio),
                color: VIDEO,
                compact: narrow,
            },
            UiState::seek_source,
            |state| state.toggle_playback_monitor(PlaybackMonitor::Source),
        ),
    );

    let lanes = timeline_lanes(state, project, fps);
    for lane in &lanes {
        for help in lane.toggle_help() {
            help.register(state);
        }
        for clip in &lane.clips {
            if !clip.effects.is_empty() {
                Help::new("Applied effects")
                    .instance(clip.clip_id)
                    .register(state);
            }
        }
    }
    let zoom = state.timeline_zoom.get();
    let span = state.visible_frames();
    let selected = !selection.is_empty();
    let splittable = !state.split_targets().is_empty();
    let drag = state.drag.get();
    let marquee = state.marquee.get();
    let plan = drag.and_then(|_| state.drop_plan());
    let expanded_timeline = state.timeline_expanded.get();
    let timeline_height = if expanded_timeline {
        (viewport.height * 0.46).clamp(260.0, 430.0)
    } else {
        (viewport.height * 0.34).clamp(212.0, 330.0)
    };
    let timeline = Flex::column()
        .height(timeline_height)
        .gap(6.0)
        .padding(8.0)
        .background(panel())
        .corner_radius(10.0)
        .child(Box::new(
            Flex::row()
                .height(32.0)
                .gap(4.0)
                .align(Align::Center)
                .child(Box::new(Flex::row().width(4.0)))
                .child(heading(timecode(frame, fps), accent(), 15.0))
                .child(Box::new(Flex::row().width(6.0)))
                .child(text(
                    format!("of {}", timecode(timeline_end, fps)),
                    faint(),
                    11.0,
                ))
                .child(Box::new(Flex::row().width(10.0)))
                .child(divider())
                .child(tool_button(
                    Some(Glyph::Split),
                    "Split",
                    splittable && !busy,
                    state,
                    UiState::split_at_playhead,
                ))
                .child(tool_button(
                    Some(Glyph::Trash),
                    "Delete",
                    selected && !busy,
                    state,
                    |s| s.delete_selection(false),
                ))
                .child(tool_button(
                    Some(Glyph::Ripple),
                    "Ripple",
                    selected && !busy,
                    state,
                    |s| s.delete_selection(true),
                ))
                .child(divider())
                .child(tool_button(
                    Some(Glyph::Title),
                    "Title",
                    !busy,
                    state,
                    |s| {
                        s.add_title();
                    },
                ))
                .child(tool_button(
                    Some(Glyph::Marker),
                    "Marker",
                    !busy,
                    state,
                    UiState::toggle_marker,
                ))
                .child(tool_button(
                    Some(Glyph::Tracks),
                    "Tracks",
                    !busy,
                    state,
                    UiState::open_tracks_menu,
                ))
                .child(spacer())
                .child(icon_button(
                    Glyph::ChevronLeft,
                    Help::new("Scroll timeline earlier"),
                    state.timeline_start.get() > 0,
                    state,
                    |s| s.scroll_timeline(-8),
                ))
                .child(icon_button(
                    Glyph::ChevronRight,
                    Help::new("Scroll timeline later"),
                    state.timeline_start.get().saturating_add(span) < timeline_end,
                    state,
                    |s| s.scroll_timeline(8),
                ))
                .child(divider())
                .child(icon_button(
                    Glyph::ZoomOut,
                    Help::new("Zoom out").shortcut("-"),
                    zoom + 1 < ZOOM_SECONDS.len(),
                    state,
                    |s| s.zoom_timeline(s.timeline_zoom.peek() + 1),
                ))
                .child(Box::new(ZoomSlider {
                    level: ZOOM_SECONDS.len() - 1 - zoom.min(ZOOM_SECONDS.len() - 1),
                    levels: ZOOM_SECONDS.len(),
                    set: {
                        let state = state.clone();
                        Rc::new(move |level| state.zoom_timeline(ZOOM_SECONDS.len() - 1 - level))
                    },
                }))
                .child(icon_button(
                    Glyph::ZoomIn,
                    Help::new("Zoom in").shortcut("+ / ="),
                    zoom > 0,
                    state,
                    |s| s.zoom_timeline(s.timeline_zoom.peek().saturating_sub(1)),
                ))
                .child(Box::new(
                    Flex::row().width(40.0).justify(Justify::Center).child(text(
                        short_duration(ZOOM_SECONDS[zoom.min(ZOOM_SECONDS.len() - 1)] as f64),
                        muted(),
                        11.0,
                    )),
                ))
                .child(tool_button(
                    Some(Glyph::Expand),
                    "Fit",
                    timeline_end > 0,
                    state,
                    UiState::zoom_to_fit,
                ))
                .child(divider())
                .child(icon_button(
                    if expanded_timeline {
                        Glyph::ChevronDown
                    } else {
                        Glyph::ChevronUp
                    },
                    Help::new(if expanded_timeline {
                        "Restore timeline height"
                    } else {
                        "Expand timeline"
                    }),
                    true,
                    state,
                    |state| {
                        state
                            .timeline_expanded
                            .update(|expanded| *expanded = !*expanded);
                    },
                )),
        ))
        .child(Box::new(TimelineView {
            lanes,
            start: state.timeline_start.get(),
            span,
            end: timeline_end,
            fps,
            playhead: frame,
            markers: project
                .markers()
                .iter()
                .map(|marker| marker.frame)
                .collect(),
            selected: selection.clone(),
            ghosts: plan.as_ref().map_or_else(
                || match state.trim_draft.get() {
                    Some(Command::Trim {
                        track_id,
                        start_frame,
                        duration_frames,
                        ..
                    }) => vec![super::timeline_view::Ghost {
                        track_id,
                        start: start_frame,
                        duration: duration_frames,
                        valid: true,
                    }],
                    _ => Vec::new(),
                },
                |plan| plan.ghosts.clone(),
            ),
            dragging: matches!(
                drag.map(|drag| drag.payload),
                Some(DragPayload::Clips { .. })
            ),
            marquee,
            scroll: state.track_scroll.get(),
            state: state.clone(),
        }));

    let status_height = 24.0;
    let workspace_height =
        (viewport.height - 46.0 - timeline_height - status_height - 6.0 * 3.0).max(150.0);
    let side_width = if compact { 264.0 } else { 288.0 };
    let monitors = Box::new(Flex::row().gap(6.0).grow(1.0).child(source).child(program));
    let workspace: BoxedWidget = if compact {
        let active = state.sidebar.get();
        Box::new(
            Flex::row()
                .gap(6.0)
                .height(workspace_height)
                .child(Box::new(Flex::column().width(side_width).gap(6.0).child(
                    Box::new(Flex::column().grow(1.0).child(match active {
                        Sidebar::Media => media_bin(state, project, side_width, busy),
                        Sidebar::Effects => effects_gallery(state, side_width),
                        Sidebar::Transitions => transitions_gallery(state, side_width),
                        Sidebar::Inspector => inspector(state, project, side_width, busy),
                    })),
                )))
                .child(monitors),
        )
    } else {
        Box::new(
            Flex::row()
                .gap(6.0)
                .height(workspace_height)
                .child(library_panel(state, project, side_width, busy))
                .child(monitors)
                .child(inspector(state, project, side_width + 4.0, busy)),
        )
    };

    let mut status = Flex::row()
        .height(status_height)
        .gap(8.0)
        .align(Align::Center)
        .padding_xy(14.0, 0.0)
        .child(dot(
            if busy {
                IMAGE
            } else if dirty {
                VIDEO
            } else {
                accent()
            },
            7.0,
        ))
        .child(heading(
            if busy {
                "WORKING"
            } else if dirty {
                "UNSAVED"
            } else {
                "READY"
            },
            muted(),
            10.0,
        ))
        .child(text(
            state
                .autosave_error
                .get()
                .unwrap_or_else(|| state.status.get()),
            muted(),
            11.0,
        ))
        .child(spacer());
    if let Some(ExportStatus::Encoding { control, total, .. }) =
        exports.active().map(|job| &job.status)
    {
        status = status
            .child(text("Exporting", muted(), 10.0))
            .child(Box::new(ExportMeter {
                control: control.clone(),
                total: *total,
            }));
    }
    status = status.child(text(
        format!(
            "{} × {}  ·  {} tracks  ·  {} sources",
            project.settings().width,
            project.settings().height,
            project.tracks().len(),
            project.media().len()
        ),
        faint(),
        10.0,
    ));

    let content = Box::new(
        Flex::column()
            .size(viewport.width, viewport.height)
            .gap(6.0)
            .background(background())
            .child(Box::new(Layer {
                style: Style::default().size(viewport.width, 46.0).into(),
                children: vec![Box::new(titlebar)],
                on_click: Some(Rc::new({
                    let state = state.clone();
                    move || state.close_menu()
                })),
            }))
            .child(Box::new(
                Flex::column()
                    .gap(6.0)
                    .padding_xy(6.0, 0.0)
                    .grow(1.0)
                    .child(workspace)
                    .child(Box::new(timeline)),
            ))
            .child(Box::new(status))
            .child(match (drag, plan) {
                (Some(drag), Some(plan)) => Box::new(DragOverlay {
                    pointer: drag.pointer,
                    label: plan.label,
                    valid: plan.valid,
                    size: (viewport.width, viewport.height),
                }) as BoxedWidget,
                _ => Box::new(Flex::row().size(0.0, 0.0)),
            })
            .child(match state.menu.get() {
                Some(menu) => context_menu(state, menu, viewport),
                None => Box::new(Flex::row().size(0.0, 0.0)),
            })
            .child(match state.edl_rate_prompt.get() {
                Some((_, drop)) => edl_rate_modal(state, viewport, drop),
                None => Box::new(Flex::row().size(0.0, 0.0)),
            })
            .child(if state.export_modal.get() {
                export_modal(state, project, viewport)
            } else if state.export_queue_modal.get() {
                export_queue_modal(state, &exports, viewport, can_add_export)
            } else {
                Box::new(Flex::row().size(0.0, 0.0)) as BoxedWidget
            })
            .child(if state.settings_modal.get() {
                settings_modal(state, viewport)
            } else {
                Box::new(Flex::row().size(0.0, 0.0)) as BoxedWidget
            })
            .child(if state.rename_track.get().is_some() {
                rename_track_modal(state, viewport)
            } else {
                Box::new(Flex::row().size(0.0, 0.0)) as BoxedWidget
            }),
    );
    state.finish_tooltips();
    content
}

fn rename_track_modal(state: &UiState, viewport: Size) -> BoxedWidget {
    let width = 380.0_f32.min((viewport.width - 24.0).max(200.0));
    let height = 210.0;
    let valid = crate::timeline::Track::valid_name(state.track_name.value().trim());
    let submit = state.clone();
    let close = state.clone();
    Box::new(Layer {
        style: Layer::at(0.0, 0.0, viewport.width, viewport.height),
        on_click: Some(Rc::new(move || close.rename_track.set(None))),
        children: vec![
            Box::new(
                Flex::row()
                    .size(viewport.width, viewport.height)
                    .background(Color::rgba(0, 0, 0, 180)),
            ),
            Box::new(Layer {
                style: Layer::at(
                    (viewport.width - width) / 2.0,
                    (viewport.height - height) / 2.0,
                    width,
                    height,
                ),
                on_click: Some(Rc::new(|| {})),
                children: vec![Box::new(
                    Flex::column()
                        .size(width, height)
                        .padding(20.0)
                        .gap(12.0)
                        .background(panel_raised())
                        .border(border(), 1.0)
                        .corner_radius(12.0)
                        .child(heading("Rename track", text_color(), 18.0))
                        .child(text(
                            "Up to 32 characters. Leave blank for V1 / A1.",
                            muted(),
                            11.0,
                        ))
                        .child(Box::new(
                            TextInput::controlled(&state.track_name)
                                .width(width - 40.0)
                                .height(32.0)
                                .on_submit(move || submit.apply_track_rename()),
                        ))
                        .child(text(
                            if valid {
                                ""
                            } else {
                                "Use 32 characters or fewer, without control characters."
                            },
                            TITLE,
                            10.0,
                        ))
                        .child(Box::new(
                            Flex::row()
                                .gap(8.0)
                                .child(spacer())
                                .child(secondary_button(None, "Cancel", true, state, |s| {
                                    s.rename_track.set(None)
                                }))
                                .child(secondary_button(
                                    None,
                                    "Rename",
                                    valid,
                                    state,
                                    UiState::apply_track_rename,
                                )),
                        )),
                )],
            }),
        ],
    })
}

fn export_queue_modal(
    state: &UiState,
    queue: &Queue,
    viewport: Size,
    can_add: bool,
) -> BoxedWidget {
    let width = 600.0_f32.min(viewport.width - 16.0);
    let height = 420.0_f32.min(viewport.height - 16.0);
    let inner = width - 40.0;
    let mut jobs = Flex::column().width(inner).gap(8.0);
    if queue.jobs().is_empty() {
        jobs = jobs.child(Box::new(
            Flex::column()
                .width(inner)
                .padding(16.0)
                .gap(6.0)
                .background(surface())
                .corner_radius(8.0)
                .child(heading("No exports yet", text_color(), 13.0))
                .child(text(
                    "Add an export to see its progress here.",
                    muted(),
                    11.0,
                )),
        ));
    }
    let (pending, finished): (Vec<_>, Vec<_>) =
        queue.jobs().iter().partition(|job| job.status.pending());
    for job in pending
        .iter()
        .copied()
        .chain(finished.iter().rev().copied())
    {
        let (label, color, cancel) = match &job.status {
            ExportStatus::Queued(_) => ("Waiting", muted(), Some("Remove")),
            ExportStatus::Encoding {
                cancelling: true, ..
            } => ("Cancelling…", IMAGE, None),
            ExportStatus::Encoding {
                resolving: true, ..
            } => ("Preparing…", accent(), Some("Cancel")),
            ExportStatus::Encoding { .. } => ("Encoding", accent(), Some("Cancel")),
            ExportStatus::Completed => ("Completed", AUDIO, None),
            ExportStatus::Cancelled => ("Cancelled", muted(), None),
            ExportStatus::Failed(_) => ("Failed", PLAYHEAD, None),
        };
        let id = job.id;
        let name = job.path.file_name().unwrap_or(job.path.as_os_str());
        let mut header = Flex::row()
            .align(Align::Center)
            .gap(8.0)
            .child(heading(
                fit(&name.to_string_lossy(), inner - 160.0, 12.0),
                text_color(),
                12.0,
            ))
            .child(spacer())
            .child(text(label, color, 11.0));
        if let Some(label) = cancel {
            header = header.child(secondary_button(None, label, true, state, move |state| {
                state.cancel_export(id);
            }));
        }
        let mut row = Flex::column()
            .width(inner)
            .gap(6.0)
            .padding(10.0)
            .background(surface())
            .corner_radius(8.0)
            .child(Box::new(header))
            .child(text(
                fit(&job.path.to_string_lossy(), inner - 24.0, 11.0),
                muted(),
                11.0,
            ));
        match &job.status {
            ExportStatus::Encoding { control, total, .. } => {
                row = row.child(Box::new(ExportMeter {
                    control: control.clone(),
                    total: *total,
                }));
            }
            ExportStatus::Failed(error) => {
                row = row.child(text(error.clone(), PLAYHEAD, 11.0));
            }
            _ => {}
        }
        jobs = jobs.child(Box::new(row));
    }
    let active = usize::from(queue.active().is_some());
    let close = state.clone();
    Box::new(Layer {
        style: Layer::at(0.0, 0.0, viewport.width, viewport.height),
        on_click: Some(Rc::new(move || close.close_export_queue())),
        children: vec![
            Box::new(
                Flex::row()
                    .size(viewport.width, viewport.height)
                    .background(Color::rgba(0, 0, 0, 180)),
            ),
            Box::new(Layer {
                style: Layer::at(
                    (viewport.width - width) / 2.0,
                    (viewport.height - height) / 2.0,
                    width,
                    height,
                ),
                on_click: Some(Rc::new(|| {})),
                children: vec![Box::new(
                    Flex::column()
                        .size(width, height)
                        .padding(20.0)
                        .gap(10.0)
                        .background(panel_raised())
                        .border(border(), 1.0)
                        .corner_radius(12.0)
                        .child(Box::new(
                            Flex::row()
                                .align(Align::Center)
                                .child(heading("Export queue", text_color(), 18.0))
                                .child(spacer())
                                .child(icon_button(
                                    Glyph::Close,
                                    Help::new("Close export queue").shortcut("Esc"),
                                    true,
                                    state,
                                    UiState::close_export_queue,
                                )),
                        ))
                        .child(text(
                            format!("{active} active · {} waiting", pending.len() - active),
                            muted(),
                            11.0,
                        ))
                        .child(Box::new(
                            Flex::column().grow(1.0).child(Box::new(
                                RawScrollView::controlled(
                                    fill_style(),
                                    state.export_queue_scroll.clone(),
                                )
                                .child(Box::new(jobs)),
                            )),
                        ))
                        .child(Box::new(
                            Flex::row()
                                .gap(8.0)
                                .child(secondary_button(
                                    None,
                                    "Clear finished",
                                    !finished.is_empty(),
                                    state,
                                    UiState::clear_finished_exports,
                                ))
                                .child(spacer())
                                .child(accent_button(
                                    Some(Glyph::Export),
                                    "Add export…",
                                    can_add && state.sequence_end() > 0,
                                    state,
                                    UiState::export_dialog,
                                )),
                        )),
                )],
            }),
        ],
    })
}

fn export_field(
    label: &str,
    controller: &TextController,
    label_width: f32,
    input_width: f32,
) -> BoxedWidget {
    Box::new(
        Flex::row()
            .gap(8.0)
            .align(Align::Center)
            .child(Box::new(Flex::row().width(label_width).child(text(
                label,
                muted(),
                11.0,
            ))))
            .child(input(controller, input_width)),
    )
}

fn form_section(title: &str, content: BoxedWidget, width: f32) -> BoxedWidget {
    Box::new(
        Flex::column()
            .width(width)
            .gap(8.0)
            .child(heading(title, text_color(), 12.0))
            .child(Box::new(
                Flex::row().height(1.0).full_width().background(border()),
            ))
            .child(content),
    )
}

fn export_tab(
    label: &str,
    active: bool,
    width: f32,
    state: &UiState,
    action: impl Fn(&UiState) + 'static,
) -> BoxedWidget {
    let state = state.clone();
    Box::new(
        Flex::column()
            .width(width)
            .gap(3.0)
            .child(Box::new(
                RawButton::new(
                    Style::default()
                        .flex_row()
                        .align(Align::Center)
                        .justify(Justify::Center)
                        .size(width, 30.0),
                    move || action(&state),
                )
                .with_children(vec![text(
                    fit(label, width - 12.0, 11.0),
                    if active { text_color() } else { muted() },
                    11.0,
                )])
                .background(panel_raised())
                .corner_radius(5.0)
                .hover_style(StateStyle::new().background(surface_hover()))
                .focus_style(StateStyle::new().outline(accent(), 2.0)),
            ))
            .child(Box::new(
                Flex::row()
                    .size(width, 2.0)
                    .background(if active { accent() } else { border() }),
            )),
    )
}

fn export_choice(
    label: &str,
    selected: bool,
    width: f32,
    state: &UiState,
    action: impl Fn(&UiState) + 'static,
) -> BoxedWidget {
    let state = state.clone();
    Box::new(
        RawButton::new(
            Style::default()
                .flex_row()
                .align(Align::Center)
                .gap(7.0)
                .size(width, 28.0)
                .padding_xy(8.0, 0.0),
            move || action(&state),
        )
        .with_children(vec![
            dot(if selected { accent() } else { faint() }, 5.0),
            text(
                fit(label, width - 30.0, 11.0),
                if selected { text_color() } else { muted() },
                11.0,
            ),
        ])
        .background(if selected { surface() } else { panel_raised() })
        .corner_radius(5.0)
        .hover_style(StateStyle::new().background(surface_hover()))
        .focus_style(StateStyle::new().outline(accent(), 2.0)),
    )
}

fn export_modal(state: &UiState, project: &Project, viewport: Size) -> BoxedWidget {
    let width = 790.0_f32.min(viewport.width - 16.0);
    let height = 520.0_f32.min(viewport.height - 16.0);
    let x = (viewport.width - width) / 2.0;
    let y = (viewport.height - height) / 2.0;
    let inner = (width - 48.0).max(180.0);
    let card_width = inner - 8.0;
    let field_width = card_width;
    let label_width = if inner < 360.0 { 84.0 } else { 170.0 };
    let input_width = if inner < 360.0 { 70.0 } else { 104.0 };
    let narrow = inner < 360.0;
    let draft = state.export_draft.get();
    let busy = state.busy.get();
    let can_add = !busy || state.export_queue.get().accepts_jobs_while_busy();
    let page = state.export_page.get();
    let selected = state.export_preset.get();
    let mut all = crate::export::presets::builtins(project);
    all.extend(state.export_library.get().presets);
    let selected_settings = if selected == "Last used" {
        project.last_export().map(|last| &last.settings)
    } else {
        all.iter()
            .find(|preset| preset.name == selected)
            .map(|preset| &preset.settings)
    };
    let active = if state
        .export_form()
        .ok()
        .is_some_and(|settings| selected_settings == Some(&settings))
    {
        selected
    } else {
        "Custom".into()
    };
    let preset_columns = if inner < 370.0 { 1 } else { 2 };
    let preset_rows = all
        .chunks(preset_columns)
        .map(|row| {
            Box::new(append(
                Flex::row().gap(8.0),
                row.iter()
                    .map(|preset| {
                        let name = preset.name.clone();
                        export_choice(
                            &preset.name,
                            active == name,
                            (field_width - if preset_columns == 2 { 8.0 } else { 0.0 })
                                / preset_columns as f32,
                            state,
                            move |state| state.select_export_preset(&name),
                        )
                    })
                    .collect(),
            )) as BoxedWidget
        })
        .collect();
    let formats = [
        ("MP4", Container::Mp4),
        ("MKV", Container::Matroska),
        ("WebM", Container::WebM),
        ("GIF", Container::Gif),
        ("WAV", Container::Wav),
    ]
    .chunks(if narrow { 1 } else { 3 })
    .map(|row| {
        Box::new(append(
            Flex::row().gap(6.0),
            row.iter()
                .map(|&(name, container)| {
                    export_choice(
                        name,
                        draft.container == container,
                        if narrow {
                            field_width
                        } else {
                            (field_width - 12.0) / 3.0
                        },
                        state,
                        move |state| state.change_export_codecs(Some(container), None, None),
                    )
                })
                .collect(),
        )) as BoxedWidget
    })
    .collect();
    let videos = [
        ("H.264", VideoCodec::H264),
        ("HEVC", VideoCodec::Hevc),
        ("VP9", VideoCodec::Vp9),
    ]
    .into_iter()
    .map(|(name, codec)| {
        export_choice(
            name,
            draft.video_codec == codec,
            if narrow {
                field_width
            } else {
                (field_width - 12.0) / 3.0
            },
            state,
            move |state| state.change_export_codecs(None, Some(codec), None),
        )
    })
    .collect();
    let audios = [("AAC", AudioCodec::Aac), ("Opus", AudioCodec::Opus)]
        .into_iter()
        .map(|(name, codec)| {
            export_choice(
                name,
                draft.audio_codec == codec,
                if narrow {
                    field_width
                } else {
                    (field_width - 6.0) / 2.0
                },
                state,
                move |state| state.change_export_codecs(None, None, Some(codec)),
            )
        })
        .collect();
    let body: BoxedWidget = match page {
        ExportPage::Presets => Box::new(
            Flex::column()
                .width(inner)
                .gap(16.0)
                .child(form_section(
                    "Choose a profile",
                    Box::new(append(Flex::column().gap(6.0), preset_rows)),
                    card_width,
                ))
                .child(text(format!("Selected: {active}"), muted(), 11.0))
                .child(form_section(
                    "Save a global preset",
                    Box::new(
                        if narrow { Flex::column() } else { Flex::row() }
                            .gap(8.0)
                            .child(Box::new(
                                TextInput::controlled(&state.export_name)
                                    .width(if narrow {
                                        card_width
                                    } else {
                                        card_width - 138.0
                                    })
                                    .height(30.0)
                                    .placeholder("Preset name"),
                            ))
                            .child(secondary_button(
                                Some(Glyph::Save),
                                "Save preset",
                                !state.busy.get() && state.export_presets_ready.get(),
                                state,
                                UiState::save_export_preset,
                            )),
                    ),
                    card_width,
                )),
        ),
        ExportPage::Video => Box::new(
            Flex::column()
                .width(inner)
                .gap(16.0)
                .child(
                    if draft.container.uses_video_codec()
                        && project.tracks().iter().any(|track| track.subtitles)
                    {
                        let state = state.clone();
                        form_section(
                            "Subtitles",
                            Box::new(
                                Flex::column()
                                    .gap(8.0)
                                    .child(Box::new(
                                        Flex::row()
                                            .gap(8.0)
                                            .align(Align::Center)
                                            .child(Box::new(Checkbox::new(
                                                draft.burn_subtitles,
                                                move || state.toggle_burn_subtitles(),
                                            )))
                                            .child(text("Burn in subtitles", text_color(), 12.0)),
                                    ))
                                    .child(text(
                                        "When off, save an SRT beside the video.",
                                        muted(),
                                        11.0,
                                    )),
                            ),
                            card_width,
                        )
                    } else {
                        Box::new(Flex::row().size(0.0, 0.0))
                    },
                )
                .child(form_section(
                    "Format",
                    Box::new(
                        Flex::column()
                            .gap(8.0)
                            .child(Box::new(append(Flex::column().gap(6.0), formats)))
                            .child(match draft.container {
                                Container::Gif => {
                                    text("Looping animation · no audio", muted(), 11.0)
                                }
                                Container::Wav => text("48 kHz stereo · no video", muted(), 11.0),
                                _ => Box::new(
                                    Flex::column()
                                        .gap(8.0)
                                        .child(heading("Video codec", muted(), 11.0))
                                        .child(Box::new(append(
                                            if narrow { Flex::column() } else { Flex::row() }
                                                .gap(6.0),
                                            videos,
                                        ))),
                                ),
                            }),
                    ),
                    card_width,
                ))
                .child(if draft.container.has_video() {
                    form_section(
                        "Output dimensions and frame rate",
                        Box::new(
                            Flex::column()
                                .gap(9.0)
                                .child(export_field(
                                    "Width (pixels)",
                                    &state.export_width,
                                    label_width,
                                    input_width,
                                ))
                                .child(export_field(
                                    "Height (pixels)",
                                    &state.export_height,
                                    label_width,
                                    input_width,
                                ))
                                .child(if narrow {
                                    Box::new(
                                        Flex::column()
                                            .gap(6.0)
                                            .child(text("FPS (num / den)", muted(), 11.0))
                                            .child(Box::new(
                                                Flex::row()
                                                    .gap(8.0)
                                                    .align(Align::Center)
                                                    .child(input(&state.export_fps_num, 80.0))
                                                    .child(text("/", muted(), 12.0))
                                                    .child(input(&state.export_fps_den, 80.0)),
                                            )),
                                    ) as BoxedWidget
                                } else {
                                    Box::new(
                                        Flex::row()
                                            .gap(8.0)
                                            .align(Align::Center)
                                            .child(Box::new(
                                                Flex::row().width(label_width).child(text(
                                                    "FPS (num / den)",
                                                    muted(),
                                                    11.0,
                                                )),
                                            ))
                                            .child(input(&state.export_fps_num, 70.0))
                                            .child(text("/", muted(), 12.0))
                                            .child(input(&state.export_fps_den, 70.0)),
                                    )
                                }),
                        ),
                        card_width,
                    )
                } else {
                    Box::new(Flex::row().size(0.0, 0.0))
                })
                .child(if draft.container.uses_video_codec() {
                    form_section(
                        "Quality and speed",
                        Box::new(
                            Flex::column()
                                .gap(9.0)
                                .child(Box::new(append(
                                    if narrow { Flex::column() } else { Flex::row() }.gap(6.0),
                                    vec![
                                        export_choice(
                                            "Target bitrate",
                                            draft.video_quality.is_none(),
                                            if narrow {
                                                field_width
                                            } else {
                                                (field_width - 6.0) / 2.0
                                            },
                                            state,
                                            |s| s.set_export_quality_mode(false),
                                        ),
                                        export_choice(
                                            "Constant quality",
                                            draft.video_quality.is_some(),
                                            if narrow {
                                                field_width
                                            } else {
                                                (field_width - 6.0) / 2.0
                                            },
                                            state,
                                            |s| s.set_export_quality_mode(true),
                                        ),
                                    ],
                                )))
                                .child(if draft.video_quality.is_some() {
                                    export_field(
                                        "Quality (CRF)",
                                        &state.export_quality,
                                        label_width,
                                        input_width,
                                    )
                                } else {
                                    export_field(
                                        "Video (kbps)",
                                        &state.export_video_bitrate,
                                        label_width,
                                        input_width,
                                    )
                                })
                                .child(if draft.video_quality.is_some() {
                                    text("CRF: 0–51 (H.264/HEVC), 0–63 (VP9)", faint(), 10.0)
                                } else {
                                    Box::new(Flex::row().size(0.0, 0.0))
                                })
                                .child(heading("Encoder speed", muted(), 11.0))
                                .child(Box::new(append(
                                    if narrow { Flex::column() } else { Flex::row() }.gap(6.0),
                                    [
                                        ("Fast", EncodingSpeed::Fast),
                                        ("Balanced", EncodingSpeed::Balanced),
                                        ("Quality", EncodingSpeed::Quality),
                                    ]
                                    .into_iter()
                                    .map(|(name, speed)| {
                                        export_choice(
                                            name,
                                            draft.speed == speed,
                                            if narrow {
                                                field_width
                                            } else {
                                                (field_width - 12.0) / 3.0
                                            },
                                            state,
                                            move |s| s.set_export_speed(speed),
                                        )
                                    })
                                    .collect(),
                                ))),
                        ),
                        card_width,
                    )
                } else if draft.container == Container::Gif {
                    text("GIF limits: 1280 × 1280 pixels, 50 fps.", muted(), 11.0)
                } else {
                    Box::new(Flex::row().size(0.0, 0.0))
                }),
        ),
        ExportPage::Audio if draft.container == Container::Gif => form_section(
            "No audio",
            text("Animated GIFs do not contain sound.", muted(), 11.0),
            card_width,
        ),
        ExportPage::Audio if draft.container == Container::Wav => form_section(
            "Uncompressed audio",
            text(
                "WAV exports the timeline mix as 48 kHz stereo.",
                muted(),
                11.0,
            ),
            card_width,
        ),
        ExportPage::Audio => Box::new(
            Flex::column()
                .width(inner)
                .gap(16.0)
                .child(form_section(
                    "Audio codec",
                    Box::new(append(
                        if narrow { Flex::column() } else { Flex::row() }.gap(6.0),
                        audios,
                    )),
                    card_width,
                ))
                .child(form_section(
                    "Audio bitrate",
                    export_field(
                        "Audio (kbps)",
                        &state.export_audio_bitrate,
                        label_width,
                        input_width,
                    ),
                    card_width,
                )),
        ),
        ExportPage::Output => Box::new(
            Flex::column()
                .width(inner)
                .gap(16.0)
                .child(form_section(
                    "Output file",
                    Box::new(
                        Flex::column()
                            .gap(10.0)
                            .child(Box::new(
                                TextInput::controlled(&state.export_path)
                                    .width(card_width)
                                    .height(30.0)
                                    .placeholder("Absolute output path"),
                            ))
                            .child(secondary_button(
                                Some(Glyph::Folder),
                                "Browse…",
                                !state.busy.get(),
                                state,
                                UiState::browse_export_path,
                            )),
                    ),
                    card_width,
                ))
                .child(text(
                    "Re-export creates a new numbered file.",
                    faint(),
                    10.0,
                )),
        ),
    };
    let tab_width = (inner - 18.0) / 4.0;
    let tabs = Box::new(append(
        Flex::row().gap(6.0),
        [
            ("Presets", ExportPage::Presets),
            ("Video", ExportPage::Video),
            ("Audio", ExportPage::Audio),
            ("Destination", ExportPage::Output),
        ]
        .into_iter()
        .map(|(label, target)| {
            export_tab(label, page == target, tab_width, state, move |s| {
                s.export_scroll.set(0.0);
                s.export_page.set(target);
            })
        })
        .collect(),
    ));
    let footer_actions = Box::new(
        Flex::row()
            .gap(8.0)
            .child(spacer())
            .child(secondary_button(
                None,
                "Cancel",
                true,
                state,
                UiState::close_export_dialog,
            ))
            .child(accent_button(
                Some(Glyph::Export),
                if busy { "Add to queue" } else { "Export" },
                can_add,
                state,
                UiState::submit_export,
            )),
    );
    let status = if state.export_error.get().is_some() {
        String::new()
    } else {
        state.status.get()
    };
    let footer_status = if narrow {
        Box::new(
            Flex::column()
                .gap(5.0)
                .child(text(fit(&status, inner, 11.0), muted(), 11.0))
                .child(footer_actions),
        ) as BoxedWidget
    } else {
        Box::new(
            Flex::row()
                .gap(8.0)
                .align(Align::Center)
                .child(text(fit(&status, inner - 224.0, 11.0), muted(), 11.0))
                .child(footer_actions),
        )
    };
    let mut footer = Flex::column()
        .width(inner)
        .shrink(0.0)
        .gap(8.0)
        .child(Box::new(
            Flex::row()
                .width(inner)
                .gap(8.0)
                .align(Align::Center)
                .child(text("Save to", muted(), 11.0))
                .child(text(
                    fit(&state.export_path.value(), inner - 150.0, 11.0),
                    text_color(),
                    11.0,
                ))
                .child(spacer())
                .child(secondary_button(None, "Change…", true, state, |state| {
                    state.export_scroll.set(0.0);
                    state.export_page.set(ExportPage::Output);
                })),
        ));
    if state.project_path.get().is_some() {
        footer = footer.child(Box::new(
            Flex::row()
                .gap(6.0)
                .align(Align::Center)
                .child(glyph(Glyph::Save, muted(), 12.0))
                .child(text("Export also saves your project", muted(), 11.0)),
        ));
    }
    if let Some(reason) = state.export_error.get() {
        footer = footer.child(text(reason, PLAYHEAD, 11.0));
    }
    let footer = Box::new(footer.child(footer_status));
    let close = state.clone();
    Box::new(Layer {
        style: Layer::at(0.0, 0.0, viewport.width, viewport.height),
        on_click: Some(Rc::new(move || close.close_export_dialog())),
        children: vec![
            Box::new(
                Flex::row()
                    .size(viewport.width, viewport.height)
                    .background(Color::rgba(0, 0, 0, 180)),
            ),
            Box::new(Layer {
                style: Layer::at(x, y, width, height),
                on_click: Some(Rc::new(|| {})),
                children: vec![Box::new(
                    Flex::column()
                        .size(width, height)
                        .padding(20.0)
                        .background(panel_raised())
                        .border(border(), 1.0)
                        .corner_radius(12.0)
                        .child(Box::new(
                            Flex::column()
                                .size(width - 40.0, height - 40.0)
                                .gap(12.0)
                                .child(Box::new(
                                    Flex::row()
                                        .align(Align::Center)
                                        .child(heading("Export timeline", text_color(), 18.0))
                                        .child(spacer())
                                        .child(icon_button(
                                            Glyph::Close,
                                            Help::new("Close export options").shortcut("Esc"),
                                            true,
                                            state,
                                            UiState::close_export_dialog,
                                        )),
                                ))
                                .child(tabs)
                                .child(Box::new(
                                    Flex::column().grow(1.0).child(Box::new(
                                        RawScrollView::controlled(
                                            fill_style(),
                                            state.export_scroll.clone(),
                                        )
                                        .child(body),
                                    )),
                                ))
                                .child(footer),
                        )),
                )],
            }),
        ],
    })
}

fn settings_nav(
    label: &str,
    selected: bool,
    width: f32,
    state: &UiState,
    page: SettingsPage,
) -> BoxedWidget {
    let state = state.clone();
    Box::new(
        RawButton::new(
            Style::default()
                .flex_row()
                .align(Align::Center)
                .gap(10.0)
                .size(width, 36.0)
                .padding_xy(10.0, 0.0),
            move || {
                state.settings_scroll.set(0.0);
                state.settings_page.set(page);
            },
        )
        .with_children(vec![
            dot(if selected { accent() } else { faint() }, 7.0),
            text(label, if selected { text_color() } else { muted() }, 12.0),
        ])
        .background(if selected {
            creamui::use_theme().colors.selection_background
        } else {
            panel()
        })
        .corner_radius(8.0)
        .hover_style(StateStyle::new().background(surface_hover()))
        .focus_style(StateStyle::new().outline(accent(), 2.0)),
    )
}

fn about_row(label: &str, value: &str, width: f32) -> BoxedWidget {
    Box::new(
        Flex::row()
            .width(width)
            .height(32.0)
            .align(Align::Center)
            .child(text(label, muted(), 11.0))
            .child(spacer())
            .child(text(fit(value, width - 100.0, 11.0), text_color(), 11.0)),
    )
}

fn open_project_link(url: &'static str, state: &UiState) {
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = ProcessCommand::new("cmd");
        command.args(["/C", "start", ""]);
        command
    };
    #[cfg(target_os = "macos")]
    let mut command = ProcessCommand::new("open");
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = ProcessCommand::new("xdg-open");
    match command.arg(url).spawn() {
        Ok(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(error) => state.status.set(format!("Could not open browser: {error}")),
    }
}

fn performance_settings(state: &UiState, hardware: bool, proxies: bool, width: f32) -> BoxedWidget {
    let backends = crate::media::hardware::Backend::PLATFORM
        .iter()
        .map(|backend| backend.name())
        .collect::<Vec<_>>()
        .join(" or ");
    let toggle = state.clone();
    Box::new(
        Flex::column()
            .width(width)
            .gap(12.0)
            .child(heading("Performance", text_color(), 16.0))
            .child(Box::new(
                Flex::row()
                    .gap(10.0)
                    .align(Align::Center)
                    .child(Box::new(Checkbox::new(hardware, move || {
                        toggle.toggle_hardware_acceleration()
                    })))
                    .child(heading("Hardware acceleration", text_color(), 11.0)),
            ))
            .child(text(
                fit(
                    &format!("Decode previews and encode exports with {backends}."),
                    width,
                    11.0,
                ),
                muted(),
                11.0,
            ))
            .child(text(
                fit(
                    "Ocus switches to software when no device supports the media.",
                    width,
                    11.0,
                ),
                muted(),
                11.0,
            ))
            .child(Box::new(
                Flex::row()
                    .gap(10.0)
                    .align(Align::Center)
                    .child(Box::new(Checkbox::new(proxies, {
                        let state = state.clone();
                        move || state.toggle_proxies()
                    })))
                    .child(heading("Use proxies for preview", text_color(), 11.0)),
            ))
            .child(text(
                fit(
                    "Edit 4K footage smoothly with lightweight copies.",
                    width,
                    11.0,
                ),
                muted(),
                11.0,
            ))
            .child(text("Export always uses the originals.", muted(), 11.0)),
    )
}

fn about_settings(state: &UiState, width: f32) -> BoxedWidget {
    let light = creamui::use_theme().colors.surface.r > 128;
    let link_color = if light {
        Color::rgb(16, 94, 190)
    } else {
        Color::rgb(91, 173, 255)
    };
    let hover_color = if light {
        Color::rgb(0, 78, 170)
    } else {
        Color::rgb(130, 199, 255)
    };
    let links = [
        ("Source code", env!("CARGO_PKG_REPOSITORY")),
        (
            "Report an issue",
            concat!(env!("CARGO_PKG_REPOSITORY"), "/issues"),
        ),
        (
            "Releases",
            concat!(env!("CARGO_PKG_REPOSITORY"), "/releases"),
        ),
    ]
    .into_iter()
    .map(|(label, url)| {
        let state = state.clone();
        Box::new(
            Flex::row()
                .height(28.0)
                .align(Align::Center)
                .child(Box::new(
                    RawLink::new(
                        Style::default().size(width, 26.0),
                        label,
                        link_color,
                        12.0,
                        move || open_project_link(url, &state),
                    )
                    .hover_color(hover_color)
                    .focus_style(StateStyle::new().outline(link_color, 2.0)),
                )),
        ) as BoxedWidget
    })
    .collect();
    Box::new(
        Flex::column()
            .width(width)
            .gap(16.0)
            .child(Box::new(
                Flex::row()
                    .gap(16.0)
                    .align(Align::Center)
                    .child(Box::new(
                        Flex::row()
                            .size(56.0, 56.0)
                            .align(Align::Center)
                            .justify(Justify::Center)
                            .background(surface())
                            .corner_radius(14.0)
                            .child(Box::new(Logo)),
                    ))
                    .child(Box::new(
                        Flex::column()
                            .gap(4.0)
                            .child(heading("Ocus", text_color(), 24.0))
                            .child(text("Video editor", muted(), 12.0)),
                    )),
            ))
            .child(form_section(
                "Build",
                Box::new(
                    Flex::column()
                        .child(about_row("Version", env!("CARGO_PKG_VERSION"), width))
                        .child(about_row("Commit", env!("OCUS_COMMIT"), width))
                        .child(about_row("Author", env!("CARGO_PKG_AUTHORS"), width)),
                ),
                width,
            ))
            .child(form_section(
                "Project",
                Box::new(append(Flex::column().gap(2.0), links)),
                width,
            ))
            .child(
                if state.status.get().starts_with("Could not open browser:") {
                    text(state.status.get(), muted(), 11.0)
                } else {
                    Box::new(Flex::row().size(0.0, 0.0))
                },
            ),
    )
}

fn settings_modal(state: &UiState, viewport: Size) -> BoxedWidget {
    let settings = state.settings.get();
    let page = state.settings_page.get();
    let width = 750.0_f32.min(viewport.width - 24.0);
    let height = 480.0_f32.min(viewport.height - 24.0);
    let nav_width = if width < 590.0 { 126.0 } else { 158.0 };
    let content_width = (width - nav_width - 48.0).max(170.0);
    let compact = content_width < 360.0;
    let theme_options = [
        ("System", "Follow your desktop", ThemeMode::System),
        ("Light", "Bright workspace", ThemeMode::Light),
        ("Dark", "Reduced glare", ThemeMode::Dark),
        ("Midnight", "Deep contrast", ThemeMode::Midnight),
    ];
    let theme_rows = theme_options
        .into_iter()
        .map(|(name, detail, mode)| {
            let palette = if mode == ThemeMode::System {
                creamui::use_theme()
            } else {
                crate::settings::Settings {
                    theme: mode,
                    accent: None,
                    ..Default::default()
                }
                .theme()
            }
            .colors;
            let selected = settings.theme == mode;
            let state = state.clone();
            Box::new(
                RawButton::new(
                    Style::default()
                        .flex_row()
                        .align(Align::Center)
                        .gap(10.0)
                        .size(content_width, 40.0)
                        .padding_xy(10.0, 0.0),
                    move || state.set_theme_mode(mode),
                )
                .with_children(vec![
                    Box::new(
                        Flex::row()
                            .size(27.0, 19.0)
                            .background(palette.surface)
                            .corner_radius(4.0)
                            .child(Box::new(
                                Flex::row().size(8.0, 19.0).background(palette.accent),
                            )),
                    ),
                    heading(name, text_color(), 11.0),
                    if compact {
                        Box::new(Flex::row().size(0.0, 0.0))
                    } else {
                        text(detail, muted(), 11.0)
                    },
                    spacer(),
                    dot(if selected { accent() } else { faint() }, 6.0),
                ])
                .background(if selected { surface() } else { panel_raised() })
                .corner_radius(6.0)
                .hover_style(StateStyle::new().background(surface_hover()))
                .focus_style(StateStyle::new().outline(accent(), 2.0)),
            ) as BoxedWidget
        })
        .collect();
    let content: BoxedWidget = {
        let presets = creamui::ThemeDefinition::builtin_default().accent_presets;
        let columns = if compact { 2 } else { 3 };
        let tile_width = (content_width - (columns - 1) as f32 * 8.0) / columns as f32;
        Box::new(
            Flex::column()
                .width(content_width)
                .gap(12.0)
                .child(heading("Appearance", text_color(), 16.0))
                .child(Box::new(append(Flex::column().gap(6.0), theme_rows)))
                .child(heading("Accent color", text_color(), 16.0))
                .child(if settings.theme == ThemeMode::System {
                    text("Choose a manual theme to change the accent.", muted(), 11.0)
                } else {
                    Box::new(Flex::row().size(0.0, 0.0))
                })
                .child(Box::new(append(
                    Flex::column().gap(8.0),
                    presets
                        .chunks(columns)
                        .map(|row| {
                            Box::new(append(
                                Flex::row().gap(8.0),
                                row.iter()
                                    .map(|preset| {
                                        let id = preset.id.clone();
                                        let selected =
                                            settings.accent.as_deref().unwrap_or("orange") == id
                                                && settings.theme != ThemeMode::System;
                                        let state = state.clone();
                                        Box::new(
                                            RawButton::new(
                                                Style::default()
                                                    .flex_row()
                                                    .align(Align::Center)
                                                    .gap(8.0)
                                                    .size(tile_width, 36.0)
                                                    .padding_xy(8.0, 0.0),
                                                move || state.set_accent(&id),
                                            )
                                            .with_children(vec![
                                                dot(preset.color, 14.0),
                                                text(
                                                    fit(&preset.name, tile_width - 36.0, 11.0),
                                                    text_color(),
                                                    11.0,
                                                ),
                                            ])
                                            .disabled(settings.theme == ThemeMode::System)
                                            .background(if selected {
                                                surface()
                                            } else {
                                                panel_raised()
                                            })
                                            .corner_radius(6.0)
                                            .hover_style(
                                                StateStyle::new().background(surface_hover()),
                                            )
                                            .focus_style(StateStyle::new().outline(accent(), 2.0)),
                                        ) as BoxedWidget
                                    })
                                    .collect(),
                            )) as BoxedWidget
                        })
                        .collect(),
                ))),
        )
    };
    let nav = Box::new(
        Flex::column()
            .width(nav_width)
            .height(height)
            .gap(6.0)
            .padding(12.0)
            .background(panel())
            .child(heading("OCUS", text_color(), 13.0))
            .child(Box::new(Flex::row().height(12.0)))
            .child(heading("SETTINGS", faint(), 10.0))
            .child(settings_nav(
                "Appearance",
                page == SettingsPage::Appearance,
                nav_width - 24.0,
                state,
                SettingsPage::Appearance,
            ))
            .child(settings_nav(
                "Performance",
                page == SettingsPage::Performance,
                nav_width - 24.0,
                state,
                SettingsPage::Performance,
            ))
            .child(settings_nav(
                "About",
                page == SettingsPage::About,
                nav_width - 24.0,
                state,
                SettingsPage::About,
            )),
    );
    let close = state.clone();
    Box::new(Layer {
        style: Layer::at(0.0, 0.0, viewport.width, viewport.height),
        on_click: Some(Rc::new(move || close.close_settings())),
        children: vec![
            Box::new(
                Flex::row()
                    .size(viewport.width, viewport.height)
                    .background(Color::rgba(0, 0, 0, 180)),
            ),
            Box::new(Layer {
                style: Layer::at(
                    (viewport.width - width) / 2.0,
                    (viewport.height - height) / 2.0,
                    width,
                    height,
                ),
                on_click: Some(Rc::new(|| {})),
                children: vec![Box::new(
                    Flex::row()
                        .size(width, height)
                        .background(panel_raised())
                        .border(border(), 1.0)
                        .corner_radius(12.0)
                        .child(nav)
                        .child(Box::new(
                            Flex::column()
                                .width(width - nav_width)
                                .gap(12.0)
                                .padding(20.0)
                                .child(Box::new(
                                    Flex::row()
                                        .align(Align::Center)
                                        .child(heading("Settings", text_color(), 18.0))
                                        .child(spacer())
                                        .child(icon_button(
                                            Glyph::Close,
                                            Help::new("Close settings").shortcut("Esc"),
                                            true,
                                            state,
                                            UiState::close_settings,
                                        )),
                                ))
                                .child(Box::new(
                                    Flex::column().grow(1.0).child(Box::new(
                                        RawScrollView::controlled(
                                            fill_style(),
                                            state.settings_scroll.clone(),
                                        )
                                        .child(
                                            match page {
                                                SettingsPage::Appearance => content,
                                                SettingsPage::Performance => performance_settings(
                                                    state,
                                                    settings.hardware_acceleration,
                                                    settings.use_proxies,
                                                    content_width,
                                                ),
                                                SettingsPage::About => {
                                                    about_settings(state, content_width)
                                                }
                                            },
                                        ),
                                    )),
                                )),
                        )),
                )],
            }),
        ],
    })
}

fn edl_rate_modal(state: &UiState, viewport: Size, drop: bool) -> BoxedWidget {
    let rates: &[&[(u32, u32)]] = if drop {
        &[&[(30_000, 1001), (60_000, 1001)]]
    } else {
        &[
            &[(24_000, 1001), (24, 1), (25, 1)],
            &[(30_000, 1001), (30, 1), (50, 1)],
            &[(60_000, 1001), (60, 1)],
        ]
    };
    let width = 290.0;
    let height = 128.0 + rates.len() as f32 * 36.0;
    let mut content = Flex::column()
        .size(width, height)
        .padding(16.0)
        .gap(9.0)
        .background(panel_raised())
        .border(border(), 1.0)
        .corner_radius(12.0)
        .child(heading("Choose the EDL frame rate", text_color(), 16.0))
        .child(text(
            "Match the original timeline's frame rate.",
            muted(),
            12.0,
        ));
    for row in rates {
        let mut buttons = Flex::row().gap(6.0);
        for &(num, den) in *row {
            buttons = buttons.child(secondary_button(
                None,
                &frame_rate_label((num, den)),
                true,
                state,
                move |state| {
                    state.select_edl_rate(num, den);
                },
            ));
        }
        content = content.child(Box::new(buttons));
    }
    content = content
        .child(spacer())
        .child(Box::new(Flex::row().child(spacer()).child(
            secondary_button(None, "Cancel", true, state, UiState::cancel_edl_rate_prompt),
        )));
    let cancel = state.clone();
    Box::new(Layer {
        style: Layer::at(0.0, 0.0, viewport.width, viewport.height),
        on_click: Some(Rc::new(move || cancel.cancel_edl_rate_prompt())),
        children: vec![
            Box::new(
                Flex::row()
                    .size(viewport.width, viewport.height)
                    .background(Color::rgba(0, 0, 0, 180)),
            ),
            Box::new(Layer {
                style: Layer::at(
                    (viewport.width - width) / 2.0,
                    (viewport.height - height) / 2.0,
                    width,
                    height,
                ),
                on_click: Some(Rc::new(|| {})),
                children: vec![Box::new(content)],
            }),
        ],
    })
}

fn menu_height(entries: &[super::interaction::MenuEntry], header: bool, viewport: Size) -> f32 {
    let height = 12.0
        + if header { 26.0 } else { 0.0 }
        + entries
            .iter()
            .map(|e| {
                if e.command.is_none() && e.children.is_empty() && e.label.is_empty() {
                    9.0
                } else {
                    28.0
                }
            })
            .sum::<f32>();
    height.min((viewport.height - 16.0).max(1.0))
}

fn submenu_rect(parent: Rect, row_y: f32, height: f32, viewport: Size) -> Rect {
    let width = 244.0;
    let right = parent.x + parent.width - 2.0;
    Rect {
        x: if right + width <= viewport.width - 8.0 {
            right
        } else {
            parent.x - width + 2.0
        }
        .clamp(8.0, (viewport.width - width - 8.0).max(8.0)),
        y: row_y.clamp(8.0, (viewport.height - height - 8.0).max(8.0)),
        width,
        height,
    }
}

/// The open context menu over a full-window layer that closes it when clicked outside.
fn context_menu(state: &UiState, menu: MenuState, viewport: Size) -> BoxedWidget {
    let width = match menu.bar {
        Some(BarMenu::File) => 308.0,
        Some(BarMenu::Edit) => 288.0,
        Some(_) => 224.0,
        None => 244.0,
    };
    let top = if menu.bar.is_some() { 46.0 } else { 0.0 };
    let height =
        menu_height(&menu.entries, menu.bar.is_none(), viewport).min(viewport.height - top - 8.0);
    let mut bounds = Rect {
        x: menu.anchor.x.min(viewport.width - width - 8.0).max(8.0),
        y: if menu.bar.is_some() {
            top
        } else if menu.anchor.y + height > viewport.height - 8.0 {
            (menu.anchor.y - height).max(8.0)
        } else {
            menu.anchor.y
        },
        width,
        height,
    };
    let mut entries = &menu.entries;
    let mut panels: Vec<BoxedWidget> = Vec::new();
    for depth in 0..=menu.path.len() {
        let header = depth == 0 && menu.bar.is_none();
        let mut content = Flex::column();
        if header {
            content = content.child(Box::new(
                Flex::row()
                    .height(26.0)
                    .align(Align::Center)
                    .padding_xy(10.0, 0.0)
                    .child(heading(
                        fit(&menu.title, bounds.width - 32.0, 11.0),
                        muted(),
                        11.0,
                    )),
            ));
        }
        let active = menu.path.get(depth).copied().or(menu.focused);
        let scroll_offset = if depth == 0 {
            state.menu_scroll.offset()
        } else {
            menu.scroll_offsets[depth - 1]
        };
        let mut row_y = bounds.y + 6.0 + if header { 26.0 } else { 0.0 } - scroll_offset;
        let mut next_y = row_y;
        let mut natural_height = 12.0 + if header { 26.0 } else { 0.0 };
        for (index, entry) in entries.iter().enumerate() {
            if menu.path.get(depth) == Some(&index) {
                next_y = row_y;
            }
            let row: BoxedWidget = if entry.command.is_some() || !entry.children.is_empty() {
                let mut path = menu.path[..depth].to_vec();
                path.push(index);
                let command = entry.command;
                let submenu = !entry.children.is_empty();
                Box::new(
                    Flex::column()
                        .background(if active == Some(index) {
                            surface()
                        } else {
                            Color::rgba(0, 0, 0, 0)
                        })
                        .corner_radius(6.0)
                        .child(menu_row(
                            entry.icon,
                            &entry.label,
                            if submenu {
                                "›"
                            } else if entry.checked {
                                "✓"
                            } else {
                                entry.shortcut
                            },
                            entry.enabled,
                            entry.danger,
                            state,
                            move |s| {
                                if submenu {
                                    s.open_submenu(path.clone());
                                } else if let Some(command) = command {
                                    s.run_menu(command);
                                }
                            },
                        )),
                )
            } else if !entry.label.is_empty() {
                Box::new(
                    Flex::row()
                        .height(28.0)
                        .padding_xy(10.0, 6.0)
                        .child(heading(entry.label.to_uppercase(), faint(), 10.0)),
                )
            } else {
                Box::new(
                    Flex::row()
                        .height(9.0)
                        .align(Align::Center)
                        .padding_xy(8.0, 0.0)
                        .child(Box::new(
                            Flex::row().height(1.0).grow(1.0).background(border()),
                        )),
                )
            };
            let height =
                if entry.command.is_none() && entry.children.is_empty() && entry.label.is_empty() {
                    9.0
                } else {
                    28.0
                };
            natural_height += height;
            row_y += height;
            content = content.child(row);
        }
        let content: BoxedWidget = if natural_height > bounds.height {
            if depth == 0 {
                Box::new(
                    RawScrollView::controlled(fill_style(), state.menu_scroll.clone())
                        .child(Box::new(content)),
                )
            } else {
                let state = state.clone();
                Box::new(
                    RawScrollView::new(fill_style(), scroll_offset, move |offset| {
                        if let Some(mut menu) = state.menu.peek()
                            && let Some(current) = menu.scroll_offsets.get_mut(depth - 1)
                        {
                            *current = offset;
                            state.menu.set(Some(menu));
                        }
                    })
                    .child(Box::new(content)),
                )
            }
        } else {
            Box::new(content)
        };
        panels.push(Box::new(Layer {
            style: Layer::at(bounds.x, bounds.y - top, bounds.width, bounds.height),
            on_click: Some(Rc::new(|| {})),
            children: vec![Box::new(
                Flex::column()
                    .size(bounds.width, bounds.height)
                    .padding(6.0)
                    .background(panel_raised())
                    .border(surface(), 1.0)
                    .corner_radius(9.0)
                    .box_shadow(creamui::BoxShadow::new(
                        0.0,
                        8.0,
                        24.0,
                        0.0,
                        Color::rgba(0, 0, 0, 150),
                    ))
                    .child(content),
            )],
        }));
        let Some(index) = menu.path.get(depth) else {
            break;
        };
        let Some(entry) = entries.get(*index) else {
            break;
        };
        entries = &entry.children;
        bounds = submenu_rect(
            bounds,
            next_y,
            menu_height(entries, false, viewport),
            viewport,
        );
    }
    let close = state.clone();
    Box::new(Layer {
        style: Layer::at(0.0, top, viewport.width, viewport.height - top),
        on_click: Some(Rc::new(move || close.close_menu())),
        children: panels,
    })
}

#[cfg(test)]
mod tests {

    fn numeric_key(widget: &mut dyn Widget) -> Option<Rc<dyn Fn(creamui::core::KeyInput)>> {
        if widget.accepts_text_input() {
            return widget.on_key();
        }
        widget
            .children()
            .iter_mut()
            .find_map(|child| numeric_key(child.as_mut()))
    }

    fn submit_numeric(
        state: &UiState,
        label: &'static str,
        value: &str,
        build: impl FnOnce() -> BoxedWidget,
    ) {
        creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(creamui::ThemeProvider::new(creamui::Theme::dark()));
            let mut widget = build();
            let Some(key) = numeric_key(widget.as_mut()) else {
                return;
            };
            let controller = state.numeric_inputs.borrow()[&(state.primary_clip().unwrap(), label)]
                .controller
                .clone();
            controller.set_value(value);
            key(creamui::core::KeyInput {
                key: creamui::core::Key::Enter,
                modifiers: Default::default(),
            });
        });
    }

    fn adjust_timing(state: &UiState, source: bool, seconds: bool, increase: bool) {
        let Some((_, clip)) = selected_clip(state) else {
            return;
        };
        let step = if seconds {
            state.frames_per_second_step()
        } else {
            1
        };
        let current = if source {
            clip.source_start_frame
        } else {
            clip.duration_frames
        };
        let frames = if increase {
            current.saturating_add(step)
        } else {
            current.saturating_sub(step)
        };
        let fps = state.frame_rate();
        let value = (frames as f64 * f64::from(fps.1) / f64::from(fps.0)).to_string();
        submit_numeric(
            state,
            if source { "Source in" } else { "Length" },
            &value,
            || timing_steps(state, source),
        );
    }

    fn adjust_transition_duration(state: &UiState, out: bool, increase: bool) {
        let Some((_, clip)) = selected_video_clip(state) else {
            return;
        };
        let Some(transition) = clip_transition(&clip, out) else {
            return;
        };
        let step = transition_frames(state, 0.25);
        let frames = if increase {
            transition.duration_frames.saturating_add(step)
        } else {
            transition.duration_frames.saturating_sub(step)
        };
        let fps = state.frame_rate();
        let value = (frames as f64 * f64::from(fps.1) / f64::from(fps.0)).to_string();
        submit_numeric(state, "Duration", &value, || {
            transition_row(state, &clip, state.busy.peek(), out)
        });
    }

    fn change_clip_fade(state: &UiState, incoming: bool, increase: bool) {
        let Some((_, clip)) = selected_clip(state) else {
            return;
        };
        let fps = state.frame_rate();
        let step = (f64::from(fps.0) / f64::from(fps.1) / 2.0).round().max(1.0) as u64;
        let current = if incoming {
            clip.fade_in_frames
        } else {
            clip.fade_out_frames
        };
        let frames = if increase {
            current.saturating_add(step)
        } else {
            current.saturating_sub(step)
        };
        let value = (frames as f64 * f64::from(fps.1) / f64::from(fps.0)).to_string();
        submit_numeric(
            state,
            if incoming { "Fade in" } else { "Fade out" },
            &value,
            || fade_row(state, &clip, incoming),
        );
    }

    #[test]
    fn cascading_menu_clicks_keep_parent_visible_and_apply_child_action() {
        let state = UiState::new();
        state.timeline_rect.set(Rect {
            x: 0.0,
            y: 0.0,
            width: 1128.0,
            height: 400.0,
        });
        assert!(state.edit(Command::ImportMedia(Media {
            id: 1,
            path: "scene.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(6_000_000),
            has_audio: true,
        })));
        state.insert_media(1);
        state.open_timeline_menu(Point {
            x: super::super::timeline_view::HEADER_WIDTH + 10.0,
            y: 50.0,
        });
        let before = state.editor.borrow().project().clone();
        creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(creamui::ThemeProvider::new(creamui::Theme::dark()));
            let render_menu = || {
                let viewport = Size {
                    width: 1280.0,
                    height: 896.0,
                };
                let mut painter = TestPainter::default();
                let scene = Renderer::new().render(
                    context_menu(&state, state.menu.peek().unwrap(), viewport),
                    viewport,
                    &mut painter,
                );
                (scene, painter)
            };
            let point = |painter: &TestPainter, name: &str| {
                let rect = painter
                    .bounds
                    .iter()
                    .find(|(label, _)| label == name)
                    .unwrap_or_else(|| panic!("Missing {name}: {:?}", painter.labels))
                    .1;
                Point {
                    x: rect.x + rect.width / 2.0,
                    y: rect.y + rect.height / 2.0,
                }
            };
            let (scene, painter) = render_menu();
            scene.hit_test(point(&painter, "Add effect")).unwrap()();
            let (scene, painter) = render_menu();
            assert!(painter.labels.iter().any(|label| label == "Cut"));
            assert!(painter.labels.iter().any(|label| label == "VIDEO"));
            assert!(painter.labels.iter().any(|label| label == "AUDIO"));
            scene.hit_test(point(&painter, "Sepia")).unwrap()();
        });
        assert!(state.menu.peek().is_none());
        assert_eq!(
            selected_clip(&state).unwrap().1.effect_stack,
            [EffectKind::Sepia]
        );
        state.undo();
        assert_eq!(state.editor.borrow().project(), &before);
    }

    #[test]
    fn cascading_menu_flips_left_and_stays_inside_viewport() {
        let viewport = Size {
            width: 1280.0,
            height: 896.0,
        };
        let parent = Rect {
            x: 100.0,
            y: 40.0,
            width: 244.0,
            height: 500.0,
        };
        let right = submenu_rect(parent, 300.0, 600.0, viewport);
        assert!(right.x > parent.x);
        assert!(right.y + right.height <= viewport.height - 8.0);
        let parent = Rect {
            x: 1028.0,
            ..parent
        };
        let left = submenu_rect(parent, 300.0, 600.0, viewport);
        assert!(left.x + left.width <= parent.x + 2.0);
        assert!(left.x >= 8.0);
    }

    #[test]
    fn right_clicking_painted_effect_chip_uses_short_menu() {
        let state = UiState::new();
        state.timeline_rect.set(TIMELINE);
        assert!(state.edit(Command::ImportMedia(Media {
            id: 1,
            path: "scene.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(6_000_000),
            has_audio: true,
        })));
        state.insert_media(1);
        apply_selected_effect(&state, EffectKind::Sepia);
        creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(creamui::ThemeProvider::new(creamui::Theme::dark()));
            let mut painter = TestPainter::default();
            let scene = Renderer::new().render(
                Box::new(view(&state)),
                Size {
                    width: TIMELINE.width,
                    height: TIMELINE.height,
                },
                &mut painter,
            );
            let rect = painter
                .bounds
                .iter()
                .find(|(label, _)| label == "Sepia")
                .unwrap()
                .1;
            let point = Point {
                x: rect.x + rect.width / 2.0,
                y: rect.y + rect.height / 2.0,
            };
            scene.context_menu_at(point).unwrap()(point);
        });
        let menu = state.menu.peek().unwrap();
        assert_eq!(menu.title, "Sepia");
        assert_eq!(
            menu.entries
                .iter()
                .map(|e| e.label.as_str())
                .collect::<Vec<_>>(),
            ["Edit", "Remove"]
        );
    }

    #[test]
    fn titlebar_menus_open_switch_dismiss_and_show_about() {
        let state = UiState::new();
        let viewport = Size {
            width: 980.0,
            height: 760.0,
        };
        creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(creamui::ThemeProvider::new(creamui::Theme::dark()));
            let render_scene = || {
                let mut painter = TestPainter::default();
                let scene = Renderer::new().render(build(&state, viewport), viewport, &mut painter);
                (scene, painter)
            };
            let point = |painter: &TestPainter, label: &str| {
                let rect = painter
                    .bounds
                    .iter()
                    .find(|(text, _)| text == label)
                    .unwrap()
                    .1;
                Point {
                    x: rect.x + rect.width / 2.0,
                    y: rect.y + rect.height / 2.0,
                }
            };
            let (scene, painter) = render_scene();
            for title in ["File", "Edit", "View", "Help"] {
                assert!(painter.labels.iter().any(|label| label == title), "{title}");
            }
            scene.hit_test(point(&painter, "File")).unwrap()();
            let (scene, painter) = render_scene();
            assert!(
                !state
                    .menu
                    .peek()
                    .unwrap()
                    .entries
                    .iter()
                    .find(|entry| entry.label == "Export again")
                    .unwrap()
                    .enabled
            );
            assert!(state.menu.peek().unwrap().anchor.y >= 36.0);
            for label in [
                "New",
                "Open…",
                "Save",
                "Save as…",
                "Import media…",
                "Import subtitles…",
                "Import OTIO or EDL…",
                "Export OpenTimelineIO…",
                "Export CMX3600 EDL…",
                "Export…",
                "Export again",
                "Queue",
                "Settings…",
            ] {
                assert!(painter.labels.iter().any(|text| text == label), "{label}");
            }
            scene.hover_hit_test(point(&painter, "Edit")).unwrap().1(true);
            assert_eq!(state.menu.peek().unwrap().title, "Edit");
            assert!(
                !state
                    .menu
                    .peek()
                    .unwrap()
                    .entries
                    .iter()
                    .find(|entry| entry.label == "Undo")
                    .unwrap()
                    .enabled
            );
            state.run(Action::Escape);
            assert!(state.menu.peek().is_none());
            let (scene, painter) = render_scene();
            scene.hit_test(point(&painter, "Help")).unwrap()();
            let (scene, painter) = render_scene();
            scene.hit_test(point(&painter, "About Ocus")).unwrap()();
            assert!(state.menu.peek().is_none());
            assert!(state.settings_modal.peek());
            assert!(state.settings_page.peek() == SettingsPage::About);
            state.close_settings();
            let (scene, painter) = render_scene();
            scene.hit_test(point(&painter, "File")).unwrap()();
            let (scene, _) = render_scene();
            scene.hit_test(Point { x: 900.0, y: 700.0 }).unwrap()();
            assert!(state.menu.peek().is_none());
            let (scene, painter) = render_scene();
            scene.hover_hit_test(point(&painter, "View")).unwrap().1(true);
            assert!(state.menu.peek().is_none());
            scene.hit_test(point(&painter, "File")).unwrap()();
            let (scene, painter) = render_scene();
            scene.hit_test(point(&painter, "File")).unwrap()();
            assert!(state.menu.peek().is_none());
            let (scene, painter) = render_scene();
            scene.hit_test(point(&painter, "File")).unwrap()();
            let (scene, _) = render_scene();
            scene.hit_test(Point { x: 600.0, y: 20.0 }).unwrap()();
            assert!(state.menu.peek().is_none());
        });
    }

    #[test]
    fn titlebar_menus_fit_narrow_windows_and_short_menus_scroll() {
        let state = UiState::new();
        assert!(state.add_title());
        state
            .project_path
            .set(Some("A-long-project-name-for-the-titlebar.ocus".into()));
        for width in [800.0, 980.0, 1280.0] {
            let painter = render(&state, width, 760.0);
            let titles: Vec<_> = ["File", "Edit", "View", "Help", "Export"]
                .into_iter()
                .map(|label| {
                    painter
                        .bounds
                        .iter()
                        .find(|(text, _)| text == label)
                        .unwrap()
                        .1
                })
                .collect();
            for rect in &titles {
                assert!(rect.x >= 0.0 && rect.x + rect.width <= width, "{rect:?}");
            }
            for pair in titles.windows(2) {
                assert!(pair[0].x + pair[0].width <= pair[1].x);
            }
            let title = painter
                .bounds
                .iter()
                .find(|(text, _)| text.starts_with("A-long-"))
                .unwrap()
                .1;
            let edited = painter
                .bounds
                .iter()
                .find(|(text, _)| text == "Edited")
                .unwrap()
                .1;
            assert!(title.x >= titles[3].x + titles[3].width);
            assert!(edited.x + edited.width <= titles[4].x);
        }
        state.show_bar_menu(BarMenu::File, Point { x: 120.0, y: 46.0 }, true);
        render(&state, 800.0, 360.0);
        assert!(state.menu_scroll.max_offset() > 0.0);
        state.menu_scroll.set(state.menu_scroll.max_offset());
        assert!(
            render(&state, 800.0, 360.0)
                .labels
                .iter()
                .any(|text| text == "Settings…")
        );
    }

    #[test]
    fn subtitle_destinations_and_burn_option_are_contextual() {
        let state = UiState::new();
        assert!(state.add_title());
        let cues = crate::subtitles::parse("1\n00:00:00,000 --> 00:00:01,000\nHello").unwrap();
        let mut ids = Vec::new();
        for _ in 0..2 {
            let mut track =
                crate::subtitles::track(state.editor.borrow().project(), &cues).unwrap();
            ids.push((track.id, track.clips[0].id));
            if ids.len() == 2 {
                track.clips.clear();
            }
            let mut tracks = state.editor.borrow().project().tracks().to_vec();
            tracks.push(track);
            assert!(state.edit(Command::ReplaceTracks(tracks)));
        }
        {
            let editor = state.editor.borrow();
            let tracks = editor.project().tracks();
            let subtitle = &tracks[4].clips[0];
            assert_eq!(
                compatible_destinations(tracks, ids[0].0, subtitle),
                vec![(ids[1].0, false)]
            );
            let regular = tracks
                .iter()
                .find(|track| !track.subtitles && !track.clips.is_empty())
                .unwrap();
            assert!(
                compatible_destinations(tracks, regular.id, &regular.clips[0])
                    .iter()
                    .all(|(id, _)| !ids.iter().any(|(subtitle, _)| subtitle == id))
            );
        }
        assert!(!state.edit(Command::Move {
            track_id: ids[0].0,
            target_track_id: 0,
            clip_id: ids[0].1,
            to_frame: 0
        }));
        assert!(state.edit(Command::Move {
            track_id: ids[0].0,
            target_track_id: ids[1].0,
            clip_id: ids[0].1,
            to_frame: 0
        }));
        state.export_dialog();
        state.export_page.set(ExportPage::Video);
        state.project_path.set(Some("/project.ocus".into()));
        state
            .status
            .set("Edit rejected: check range, track and clip".into());
        assert!(state.export_form().unwrap().burn_subtitles);
        for (width, height) in [(420.0, 480.0), (680.0, 480.0), (1280.0, 760.0)] {
            let painter = render(&state, width, height);
            let (_, rect) = painter
                .bounds
                .iter()
                .find(|(text, _)| text == "Burn in subtitles")
                .unwrap();
            assert!(rect.x >= 0.0 && rect.x + rect.width <= width);
            for (_, rect) in painter.bounds.iter().filter(|(text, _)| text == "Cancel") {
                assert!(
                    rect.y + rect.height <= height.min((height + 520.0) / 2.0) - 12.0,
                    "{width}: {rect:?}"
                );
            }
            let bounds = |label: &str| {
                painter
                    .bounds
                    .iter()
                    .rev()
                    .find(|(text, _)| text.starts_with(label))
                    .unwrap()
                    .1
            };
            let hint = bounds("Export also saves");
            let status = bounds("Edit rejected");
            let cancel = bounds("Cancel");
            assert!(hint.y + hint.height <= status.y);
            assert!(status.y + status.height <= cancel.y || status.x + status.width <= cancel.x);
        }
        state.toggle_burn_subtitles();
        assert!(!state.export_form().unwrap().burn_subtitles);
        for container in [Container::Gif, Container::Wav] {
            state.change_export_codecs(Some(container), None, None);
            assert!(
                !render(&state, 1280.0, 900.0)
                    .labels
                    .iter()
                    .any(|text| text == "Burn in subtitles")
            );
        }
        let plain = UiState::new();
        assert!(plain.add_title());
        plain.export_dialog();
        plain.export_page.set(ExportPage::Video);
        assert!(
            !render(&plain, 1280.0, 900.0)
                .labels
                .iter()
                .any(|text| text == "Burn in subtitles")
        );
    }

    #[test]
    fn subtitle_inspector_style_controls_fit_narrow_panels() {
        let state = UiState::new();
        let cues = crate::subtitles::parse(
            "1\n00:00:00,000 --> 00:00:01,000\nHello\n\n2\n00:00:01,000 --> 00:00:02,000\nWorld",
        )
        .unwrap();
        let track = crate::subtitles::track(state.editor.borrow().project(), &cues).unwrap();
        let selected = (track.id, track.clips[0].id);
        let mut tracks = state.editor.borrow().project().tracks().to_vec();
        tracks.push(track);
        assert!(state.edit(Command::ReplaceTracks(tracks)));
        state.select_clip(Some(selected));
        state.title_background.set_value("#102030B3");
        apply_title_background(&state);
        change_title(&state, |title| {
            title.font_size = 52.0;
            title.y = 0.9;
        });
        for width in [264.0, 288.0] {
            let mut painter = TestPainter::default();
            creamui::reactive::with_context_scope(|| {
                creamui::reactive::provide_context(creamui::theme::ThemeProvider::new(
                    creamui::Theme::dark(),
                ));
                Renderer::new().render(
                    inspector(&state, state.editor.borrow().project(), width, false),
                    Size {
                        width,
                        height: 2000.0,
                    },
                    &mut painter,
                );
            });
            for label in [
                "SUBTITLE STYLE",
                "Font family",
                "Font size (px)",
                "Outline",
                "Background",
                "Background (#RRGGBBAA)",
                "Bottom",
                "Top",
                "Middle",
            ] {
                let (_, rect) = painter
                    .bounds
                    .iter()
                    .find(|(text, _)| text == label)
                    .unwrap_or_else(|| panic!("missing {label}"));
                assert!(
                    rect.x >= 0.0 && rect.x + rect.width <= width + 0.1,
                    "{label}: {rect:?}"
                );
            }
        }
        let editor = state.editor.borrow();
        for clip in &editor.project().tracks().last().unwrap().clips {
            let ClipContent::Title(title) = &clip.content else {
                unreachable!()
            };
            assert_eq!(title.background, Some([16, 32, 48, 179]));
            assert_eq!(title.font_size, 52.0);
        }
        let title = Title {
            text: "Background".into(),
            font_size: 180.0,
            ..Title::default()
        };
        let frame = Rect {
            x: 0.0,
            y: 0.0,
            width: 1920.0,
            height: 1080.0,
        };
        let plain = title_box(&title, frame, (1920, 1080));
        let background = title_box(
            &Title {
                background: Some([0, 0, 0, 179]),
                ..title
            },
            frame,
            (1920, 1080),
        );
        assert!(background.width > plain.width && background.height > plain.height);
    }

    use super::*;
    use crate::media::preview::{StoryboardKey, storyboard};
    use crate::ui::state::clip_storyboard_key;
    use crate::ui::timeline_view::RULER_HEIGHT;
    use creamui::core::{Painter, Point, Rect, Renderer, RgbaImage, TextAlign, Widget};
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn gallery_search_groups_and_click_apply_to_the_selected_clip() {
        for category in [EffectCategory::Video, EffectCategory::Audio] {
            let all = gallery_effects("", category);
            assert!(all.contains(&EffectKind::Reverse));
            assert!(all.contains(&EffectKind::Speed));
        }
        assert_eq!(
            gallery_effects(" SePiA ", EffectCategory::Video),
            [EffectKind::Sepia]
        );
        assert!(gallery_effects("sepia", EffectCategory::Audio).is_empty());
        assert!(gallery_effects("no such effect", EffectCategory::Video).is_empty());
        assert!(
            gallery_effects("noise", EffectCategory::Audio).contains(&EffectKind::NoiseReduction)
        );
        let state = UiState::new();
        assert!(state.edit(Command::ImportMedia(Media {
            id: 1,
            path: "scene.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(10_000_000),
            has_audio: false,
        })));
        state.insert_media(1);
        let before = state.editor.borrow().project().clone();
        state.effect_query.set_value("sepia");
        let mut painter = TestPainter::default();
        let scene = creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(creamui::ThemeProvider::new(creamui::Theme::dark()));
            Renderer::new().render(
                effects_gallery(&state, 288.0),
                Size {
                    width: 288.0,
                    height: 896.0,
                },
                &mut painter,
            )
        });
        assert!(painter.labels.iter().any(|label| label == "Video effects"));
        assert!(painter.labels.iter().any(|label| label == "Audio effects"));
        assert!(!painter.labels.iter().any(|label| label == "Reverse"));
        let rect = painter
            .bounds
            .iter()
            .find(|(label, _)| label == "Sepia")
            .unwrap()
            .1;
        let pointer = Point {
            x: rect.x + 4.0,
            y: rect.y + 4.0,
        };
        let index = scene.drag_hit_test(pointer).unwrap();
        assert_eq!(scene.draggable_at(index).unwrap().0.width, 260.0);
        let help = state.tooltip_controller((EffectKind::Sepia.description(), 0));
        help.focus(true);
        let (area, start) = scene.drag_start_at(pointer).unwrap();
        start(
            Point {
                x: pointer.x - area.x,
                y: pointer.y - area.y,
            },
            area,
        );
        assert!(!help.visible());
        state.end_effect_drag();
        scene
            .hit_test(Point {
                x: rect.x + 4.0,
                y: rect.y + 4.0,
            })
            .unwrap()();
        assert_eq!(
            selected_clip(&state).unwrap().1.effect_stack,
            [EffectKind::Sepia]
        );
        state.undo();
        assert_eq!(state.editor.borrow().project(), &before);
    }

    #[test]
    fn gallery_tabs_remain_selected_across_layouts_and_effect_chips_select_controls() {
        let state = UiState::new();
        let draw = |width| {
            let mut painter = TestPainter::default();
            let scene = creamui::reactive::with_context_scope(|| {
                creamui::reactive::provide_context(creamui::ThemeProvider::new(
                    creamui::Theme::dark(),
                ));
                let size = Size {
                    width,
                    height: 896.0,
                };
                Renderer::new().render(build(&state, size), size, &mut painter)
            });
            (scene, painter)
        };
        let (scene, painter) = draw(1280.0);
        let rect = painter
            .bounds
            .iter()
            .find(|(label, _)| label == "Effects")
            .unwrap()
            .1;
        scene
            .hit_test(Point {
                x: rect.x + 3.0,
                y: rect.y + 3.0,
            })
            .unwrap()();
        assert!(state.library_tab.peek() == LibraryTab::Effects);
        assert!(state.sidebar.peek() == Sidebar::Effects);
        for width in [1000.0, 1280.0] {
            let (_, painter) = draw(width);
            assert!(painter.labels.iter().any(|label| label == "Video effects"));
        }
        assert!(state.edit(Command::ImportMedia(Media {
            id: 1,
            path: "scene.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(10_000_000),
            has_audio: false,
        })));
        state.insert_media(1);
        assert!(!draw(1280.0).1.labels.iter().any(|label| label == "fx"));
        for kind in [EffectKind::Sepia, EffectKind::ColorCorrection] {
            state.click_gallery_effect(kind);
        }
        state.select_clip(None);
        state.library_tab.set(LibraryTab::Media);
        let (_, painter) = draw(1280.0);
        assert!(!painter.labels.iter().any(|label| label == "fx"));
        let (scene, painter) = draw(1280.0);
        for label in ["Sepia", "Color correction"] {
            assert!(painter.labels.iter().any(|text| text == label), "{label}");
        }
        let chip = painter
            .bounds
            .iter()
            .find(|(label, _)| label == "Sepia")
            .unwrap()
            .1;
        scene
            .hit_test(Point {
                x: chip.x + chip.width / 2.0,
                y: chip.y + chip.height / 2.0,
            })
            .unwrap()();
        assert!(matches!(
            state.selected_detail.peek(),
            Some((_, ClipDetail::Effect(EffectKind::Sepia)))
        ));
        assert!(draw(1280.0).1.labels.iter().any(|label| label == "Amount"));
        state.select_clip(state.primary_clip());
        assert!(state.selected_detail.peek().is_none());
        assert!(!draw(1280.0).1.labels.iter().any(|label| label == "Amount"));
        state.undo();
        state.undo();
        assert!(!draw(1280.0).1.labels.iter().any(|label| label == "Sepia"));
    }

    #[test]
    fn gallery_applies_effects_and_selected_effect_inspector_removes_with_one_undo() {
        let state = UiState::new();
        assert!(state.edit(Command::ImportMedia(Media {
            id: 1,
            path: "scene.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(10_000_000),
            has_audio: true
        })));
        state.insert_media(1);
        let target = state.primary_clip().unwrap();
        for kind in [
            EffectKind::Sepia,
            EffectKind::ColorCorrection,
            EffectKind::Reverse,
        ] {
            state.click_gallery_effect(kind);
        }
        let applied = state.editor.borrow().project().clone();
        assert_eq!(
            selected_clip(&state).unwrap().1.effect_stack,
            [
                EffectKind::Sepia,
                EffectKind::ColorCorrection,
                EffectKind::Reverse
            ]
        );
        let labels = render(&state, 1280.0, 896.0).labels;
        assert!(
            !labels
                .iter()
                .any(|label| label == "Add effect" || label == "Amount" || label == "Contrast")
        );
        for kind in [
            EffectKind::Sepia,
            EffectKind::ColorCorrection,
            EffectKind::Reverse,
        ] {
            state.select_detail(target, ClipDetail::Effect(kind));
            let labels = render(&state, 1280.0, 896.0).labels;
            assert!(labels.iter().any(|label| label == "Remove"));
            assert!(!labels.iter().any(|label| label == "Add effect"));
            assert!(labels.iter().any(|label| label == kind.label()));
        }
        state.select_detail(target, ClipDetail::Effect(EffectKind::Sepia));
        let mut painter = TestPainter::default();
        let scene = creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(creamui::ThemeProvider::new(creamui::Theme::dark()));
            Renderer::new().render(
                inspector(&state, &applied, 288.0, false),
                Size {
                    width: 288.0,
                    height: 896.0,
                },
                &mut painter,
            )
        });
        let rect = painter
            .bounds
            .iter()
            .find(|(label, _)| label == "Remove")
            .unwrap()
            .1;
        scene
            .hit_test(Point {
                x: rect.x + rect.width / 2.0,
                y: rect.y + rect.height / 2.0,
            })
            .unwrap()();
        assert_eq!(
            selected_clip(&state).unwrap().1.effect_stack,
            [EffectKind::ColorCorrection, EffectKind::Reverse]
        );
        assert!(state.selected_detail.peek().is_none());
        state.undo();
        assert_eq!(state.editor.borrow().project(), &applied);
        state.redo();
        assert!(
            !selected_clip(&state)
                .unwrap()
                .1
                .effect_stack
                .contains(&EffectKind::Sepia)
        );
    }

    #[test]
    fn transition_markers_select_editors_and_remove_clears_detail() {
        let state = UiState::new();
        state.timeline_rect.set(Rect {
            width: 1128.0,
            height: 400.0,
            ..Rect::default()
        });
        assert!(state.add_title());
        let target = state.primary_clip().unwrap();
        set_transition_kind(&state, false, Some(TransitionKind::Slide));
        let project = state.editor.borrow().project().clone();
        let mut timeline = view(&state);
        timeline.span = 300;
        let mut painter = TestPainter::default();
        let scene = creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(creamui::ThemeProvider::new(creamui::Theme::dark()));
            Renderer::new().render(
                Box::new(timeline),
                Size {
                    width: 1128.0,
                    height: 400.0,
                },
                &mut painter,
            )
        });
        let marker = painter
            .bounds
            .iter()
            .find(|(label, _)| label == "↘")
            .unwrap()
            .1;
        scene
            .hit_test(Point {
                x: marker.x + marker.width / 2.0,
                y: marker.y + marker.height / 2.0,
            })
            .unwrap()();
        assert!(state.selected_detail.peek() == Some((target, ClipDetail::Transition(false))));
        let mut painter = TestPainter::default();
        let scene = creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(creamui::ThemeProvider::new(creamui::Theme::dark()));
            Renderer::new().render(
                inspector(&state, &project, 288.0, false),
                Size {
                    width: 288.0,
                    height: 896.0,
                },
                &mut painter,
            )
        });
        for label in ["Incoming transition", "From", "Remove"] {
            assert!(painter.labels.iter().any(|text| text == label));
        }
        let remove = painter
            .bounds
            .iter()
            .find(|(label, _)| label == "Remove")
            .unwrap()
            .1;
        scene
            .hit_test(Point {
                x: remove.x + remove.width / 2.0,
                y: remove.y + remove.height / 2.0,
            })
            .unwrap()();
        assert!(state.selected_detail.peek().is_none());
        assert!(selected_clip(&state).unwrap().1.transition.is_none());
        state.undo();
        assert_eq!(state.editor.borrow().project(), &project);
        state.select_clip(Some(target));
        let mut painter = TestPainter::default();
        creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(creamui::ThemeProvider::new(creamui::Theme::dark()));
            Renderer::new().render(
                inspector(&state, &project, 288.0, false),
                Size {
                    width: 288.0,
                    height: 3000.0,
                },
                &mut painter,
            )
        });
        assert!(
            !painter
                .labels
                .iter()
                .any(|label| label == "From" || label == "Remove" || label == "Add effect")
        );
    }

    #[test]
    fn gallery_rejects_incompatible_and_applied_effects() {
        for (media_kind, accepted, rejected) in [
            (
                MediaKind::Image,
                EffectKind::Sepia,
                EffectKind::NoiseReduction,
            ),
            (
                MediaKind::Audio,
                EffectKind::NoiseReduction,
                EffectKind::Sepia,
            ),
        ] {
            let state = UiState::new();
            assert!(state.edit(Command::ImportMedia(Media {
                id: 1,
                path: "source".into(),
                kind: media_kind,
                duration_us: Some(1_000_000),
                has_audio: media_kind == MediaKind::Audio
            })));
            state.insert_media(1);
            let before = state.editor.borrow().project().clone();
            state.click_gallery_effect(rejected);
            assert_eq!(state.editor.borrow().project(), &before);
            assert_eq!(state.status.peek(), "Select a clip or drag onto one");
            state.click_gallery_effect(accepted);
            let applied = state.editor.borrow().project().clone();
            state.click_gallery_effect(accepted);
            assert_eq!(state.editor.borrow().project(), &applied);
        }
    }

    #[test]
    fn shader_and_lut_menu_use_picker_validation_before_applying() {
        use super::super::state::load_effect_file;
        let dir = tempfile::tempdir().unwrap();
        for (kind, name, contents) in [
            (
                EffectKind::CustomShader,
                "effect.wgsl",
                "fn effect(uv: vec2<f32>) -> vec4<f32> { return sample(uv); }",
            ),
            (
                EffectKind::ColorLut,
                "effect.cube",
                "LUT_1D_SIZE 2\n0 0 0\n1 1 1\n",
            ),
        ] {
            let state = UiState::new();
            assert!(state.edit(Command::ImportMedia(Media {
                id: 1,
                path: "scene.mp4".into(),
                kind: MediaKind::Video,
                duration_us: Some(1_000_000),
                has_audio: true
            })));
            state.insert_media(1);
            let target = state.primary_clip().unwrap();
            state.click_gallery_effect(kind);
            assert!(selected_clip(&state).unwrap().1.effect_stack.is_empty());
            let path = dir.path().join(name);
            std::fs::write(&path, contents).unwrap();
            let revision = state.revision.peek();
            let picked = std::cell::Cell::new(false);
            let result = load_effect_file(kind, || {
                picked.set(true);
                Some(path.clone())
            });
            assert!(picked.get());
            assert_eq!(result.as_ref().unwrap(), &Some(path.clone()));
            state.finish_effect_file(revision, target.0, target.1, kind, Ok(None));
            assert!(selected_clip(&state).unwrap().1.effect_stack.is_empty());
            state.finish_effect_file(revision + 1, target.0, target.1, kind, result.clone());
            assert!(selected_clip(&state).unwrap().1.effect_stack.is_empty());
            std::fs::write(&path, "invalid").unwrap();
            let error = load_effect_file(kind, || Some(path.clone()));
            assert!(error.is_err());
            state.finish_effect_file(revision, target.0, target.1, kind, error);
            assert!(selected_clip(&state).unwrap().1.effect_stack.is_empty());
            state.finish_effect_file(revision, target.0, target.1, kind, result);
            assert_eq!(selected_clip(&state).unwrap().1.effect_stack, [kind]);
            let labels = render(&state, 1440.0, 2600.0).labels;
            assert!(labels.iter().any(|label| label == kind.label()));
            state.undo();
            assert!(selected_clip(&state).unwrap().1.effect_stack.is_empty());
        }
    }

    fn apply_selected_effect(state: &UiState, kind: EffectKind) {
        let target = state.primary_clip().unwrap();
        state.click_gallery_effect(kind);
        state.select_detail(target, ClipDetail::Effect(kind));
        assert!(selected_clip(state).unwrap().1.effect_stack.contains(&kind));
    }

    fn destination_clip(start_frame: u64, duration_frames: u64) -> Clip {
        Clip {
            effect_stack: Vec::new(),
            playback: crate::timeline::Playback::Forward,
            effect: None,
            mask: None,
            lut: None,
            chroma_key: None,
            id: 10,
            start_frame,
            source_start_frame: 0,
            duration_frames,
            content: ClipContent::Title(Title::default()),
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
        }
    }

    #[test]
    fn destinations_include_all_compatible_tracks_except_current() {
        let tracks = [
            TrackKind::Video,
            TrackKind::Audio,
            TrackKind::Video,
            TrackKind::Video,
        ]
        .into_iter()
        .enumerate()
        .map(|(id, kind)| Track {
            id: id as u64,
            kind,
            subtitles: false,
            name: String::new(),
            locked: false,
            hidden: false,
            clips: Vec::new(),
        })
        .collect::<Vec<_>>();
        assert_eq!(
            compatible_destinations(&tracks, 0, &destination_clip(10, 20)),
            vec![(2, false), (3, false)]
        );
        assert_eq!(
            compatible_destinations(&tracks, 1, &destination_clip(10, 20)),
            vec![]
        );
        assert_eq!(
            compatible_destinations(&tracks, 99, &destination_clip(10, 20)),
            vec![]
        );
        assert_eq!(
            compatible_destinations(&[], 0, &destination_clip(10, 20)),
            vec![]
        );
    }

    #[test]
    fn destinations_flag_overlaps_but_allow_touching_ranges() {
        let mut tracks = vec![Track {
            id: 0,
            kind: TrackKind::Video,
            subtitles: false,
            name: String::new(),
            locked: false,
            hidden: false,
            clips: Vec::new(),
        }];
        for (index, (start, duration)) in [(0, 10), (30, 10), (9, 2), (29, 2), (15, 5), (0, 40)]
            .into_iter()
            .enumerate()
        {
            tracks.push(Track {
                id: index as u64 + 1,
                kind: TrackKind::Video,
                subtitles: false,
                name: String::new(),
                locked: false,
                hidden: false,
                clips: vec![destination_clip(start, duration)],
            });
        }
        assert_eq!(
            compatible_destinations(&tracks, 0, &destination_clip(10, 20)),
            vec![
                (1, false),
                (2, false),
                (3, true),
                (4, true),
                (5, true),
                (6, true)
            ]
        );
    }

    #[derive(Default)]
    struct TestPainter {
        clips: Vec<Rect>,
        visible_bounds: Vec<(String, Rect)>,
        labels: Vec<String>,
        bounds: Vec<(String, Rect)>,
        images: Vec<Rect>,
        image_ids: Vec<u64>,
        fills: Vec<(Rect, Color)>,
        strokes: Vec<(Rect, Color)>,
        text_colors: Vec<(String, Color)>,
    }

    impl Painter for TestPainter {
        fn fill_rect(&mut self, rect: Rect, color: Color, _: f32) {
            self.fills.push((rect, color));
        }

        fn stroke_rect(&mut self, rect: Rect, color: Color, _: f32, _: f32) {
            self.strokes.push((rect, color));
        }

        fn fill_text(&mut self, rect: Rect, text: &str, color: Color, _: f32, _: TextAlign) {
            self.labels.push(text.to_owned());
            self.bounds.push((text.to_owned(), rect));
            if let Some(rect) = self
                .clips
                .iter()
                .try_fold(rect, |rect, clip| rect.intersect(*clip))
            {
                self.visible_bounds.push((text.to_owned(), rect));
            }
            self.text_colors.push((text.to_owned(), color));
        }

        fn push_clip(&mut self, rect: Rect) {
            self.clips.push(rect);
        }

        fn pop_clip(&mut self) {
            self.clips.pop();
        }

        fn draw_image(&mut self, rect: Rect, image: &RgbaImage, _: Option<Color>) {
            self.images.push(rect);
            self.image_ids.push(image.id());
        }
    }

    fn render(state: &UiState, width: f32, height: f32) -> TestPainter {
        let viewport = Size { width, height };
        let mut painter = TestPainter::default();
        creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(creamui::theme::ThemeProvider::new(
                creamui::Theme::dark(),
            ));
            Renderer::new().render(build(state, viewport), viewport, &mut painter);
        });
        painter
    }

    #[test]
    fn clip_thumbnails_and_painted_filmstrips_follow_displayed_source_frames() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("frames.mkv");
        assert!(
            ProcessCommand::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "nullsrc=size=64x36:rate=30:duration=2,geq=lum=16+N*3:cb=128:cr=128",
                    "-c:v",
                    "ffv1",
                ])
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        let state = UiState::new();
        state.timeline_rect.set(TIMELINE);
        let media = Media {
            id: 1,
            path,
            kind: MediaKind::Video,
            duration_us: Some(2_000_000),
            has_audio: true,
        };
        assert!(state.edit(Command::ImportMedia(media.clone())));
        state.insert_media(1);
        let target = state.primary_clip().unwrap();
        assert!(state.edit(Command::Trim {
            track_id: target.0,
            clip_id: target.1,
            start_frame: 0,
            source_start_frame: 5,
            duration_frames: 40
        }));
        let bin_image = Arc::new(RgbaImage::new(1, 1, vec![0, 0, 0, 255]).unwrap());
        let lut = dir.path().join("invert.cube");
        std::fs::write(&lut, "LUT_1D_SIZE 2\n1 1 1\n0 0 0\n").unwrap();
        assert!(state.edit(Command::SetClipTransform {
            track_id: target.0,
            clip_id: target.1,
            transform: Transform {
                scale: 0.8,
                rotation: 15.0,
                x: 0.1,
                opacity: 0.7,
                crop_left: 0.1,
                ..Default::default()
            }
        }));
        apply_selected_effect(&state, EffectKind::ColorCorrection);
        assert!(state.edit(Command::SetClipColor {
            track_id: target.0,
            clip_id: target.1,
            color: ColorAdjust {
                brightness: 0.1,
                ..Default::default()
            }
        }));
        assert!(state.edit(Command::SetClipLut {
            track_id: target.0,
            clip_id: target.1,
            path: Some(lut.clone())
        }));
        state
            .thumbnails
            .borrow_mut()
            .insert(media.path.clone(), bin_image.clone());
        state.selected_detail.set(None);
        for playback in [Playback::Forward, Playback::Reverse, Playback::Freeze] {
            state.set_playback(target, playback);
            let clip = selected_clip(&state).unwrap().1;
            let key = clip_storyboard_key(&clip, &media, (30, 1)).unwrap();
            assert!(
                state.clip_thumbnail(&clip, &media, (30, 1)).is_none(),
                "do not show source frame zero while loading"
            );
            let frames = storyboard(&key, Arc::new(AtomicBool::new(false)), false).unwrap();
            let expected_frames: Vec<_> = [0, clip.duration_frames / 2, clip.duration_frames - 1]
                .into_iter()
                .map(|offset| clip.source_frame_at(offset).unwrap())
                .collect();
            for (index, frame) in frames.iter().enumerate() {
                let expected = storyboard(
                    &StoryboardKey {
                        playback: Playback::Forward,
                        source_start: expected_frames[index],
                        duration: 1,
                        ..key.clone()
                    },
                    Arc::new(AtomicBool::new(false)),
                    false,
                )
                .unwrap();
                assert_eq!(frame.pixels, expected[0].pixels);
            }
            let images: Vec<_> = frames
                .into_iter()
                .map(|frame| {
                    Arc::new(RgbaImage::new(frame.width, frame.height, frame.pixels).unwrap())
                })
                .collect();
            let first_id = images[0].id();
            state.storyboards.borrow_mut().insert(key, Arc::new(images));
            assert_eq!(
                state.clip_thumbnail(&clip, &media, (30, 1)).unwrap().id(),
                first_id
            );
            let mut inspector_painter = TestPainter::default();
            creamui::reactive::with_context_scope(|| {
                creamui::reactive::provide_context(creamui::ThemeProvider::new(
                    creamui::Theme::dark(),
                ));
                let editor = state.editor.borrow();
                Renderer::new().render(
                    inspector(&state, editor.project(), 300.0, false),
                    Size {
                        width: 300.0,
                        height: 2000.0,
                    },
                    &mut inspector_painter,
                );
            });
            assert_eq!(inspector_painter.image_ids, vec![first_id]);
            let mut timeline = view(&state);
            timeline.span = 40;
            let images = timeline.lanes[0].clips[0].images.as_ref().unwrap();
            let mut painter = TestPainter::default();
            timeline.paint(&mut painter, TIMELINE);
            assert!(painter.image_ids.len() > 3);
            assert!(!painter.image_ids.contains(&bin_image.id()));
            let indices: Vec<_> = painter
                .image_ids
                .iter()
                .map(|id| images.iter().position(|image| image.id() == *id).unwrap())
                .collect();
            assert!(indices.windows(2).all(|pair| pair[0] <= pair[1]));
            if playback == Playback::Freeze {
                assert!(painter.image_ids.iter().all(|id| *id == first_id));
                assert!(
                    inspector_painter
                        .labels
                        .iter()
                        .any(|label| label == "Frozen frames are silent")
                );
                assert_eq!(
                    inspector_painter
                        .labels
                        .iter()
                        .filter(|label| *label == "100%")
                        .count(),
                    0
                );
                for label in ["Mute", "Detach audio to its own track"] {
                    assert!(!inspector_painter.labels.iter().any(|text| text == label));
                }
                assert!(
                    !painter
                        .fills
                        .iter()
                        .any(|(rect, color)| rect.x >= 128.0 && *color == AUDIO.mix(panel(), 0.5))
                );
            }
        }
        let mut clip = selected_clip(&state).unwrap().1;
        clip.playback = Playback::Forward;
        clip.source_start_frame = 0;
        clip.transform = Transform::default();
        clip.color = Default::default();
        clip.lut = None;
        assert_eq!(
            state.clip_thumbnail(&clip, &media, (30, 1)).unwrap().id(),
            bin_image.id()
        );
        clip.color.brightness = 0.1;
        assert!(state.clip_thumbnail(&clip, &media, (30, 1)).is_none());
        clip.color = Default::default();
        clip.transform.opacity = 0.5;
        assert!(state.clip_thumbnail(&clip, &media, (30, 1)).is_none());
        clip.transform = Transform::default();
        clip.lut = Some(lut);
        assert!(state.clip_thumbnail(&clip, &media, (30, 1)).is_none());
        clip.duration_frames = 0;
        assert!(state.clip_thumbnail(&clip, &media, (30, 1)).is_none());
    }

    #[test]
    fn inspector_playback_choices_fit_small_and_large_widths() {
        let state = UiState::new();
        assert!(state.edit(Command::ImportMedia(Media {
            id: 1,
            path: "scene.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(10_000_000),
            has_audio: true,
        })));
        state.insert_media(1);
        let target = state.primary_clip().unwrap();
        for playback in [Playback::Forward, Playback::Reverse, Playback::Freeze] {
            state.set_playback(target, playback);
            for width in [210.0, 360.0] {
                let size = Size {
                    width,
                    height: 4000.0,
                };
                let mut painter = TestPainter::default();
                creamui::reactive::with_context_scope(|| {
                    creamui::reactive::provide_context(creamui::ThemeProvider::new(
                        creamui::Theme::dark(),
                    ));
                    let editor = state.editor.borrow();
                    Renderer::new().render(
                        inspector(&state, editor.project(), width, false),
                        size,
                        &mut painter,
                    );
                });
                for label in ["TIMING", "Length"] {
                    let (_, rect) = painter
                        .bounds
                        .iter()
                        .find(|(text, _)| text == label)
                        .unwrap();
                    assert!(
                        rect.x >= 0.0 && rect.x + rect.width <= width,
                        "{label}: {rect:?}"
                    );
                    assert!(
                        rect.y >= 0.0 && rect.y + rect.height <= size.height,
                        "{label}: {rect:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn timeline_playback_badges_fit_clip_headers_or_hide_when_too_narrow() {
        let state = UiState::new();
        state.timeline_rect.set(TIMELINE);
        assert!(state.add_title());
        let mut timeline = view(&state);
        timeline.lanes[0].clips[0].name = "Clip".into();
        for duration in [1, 12, 150] {
            timeline.lanes[0].clips[0].duration = duration;
            timeline.lanes[0].clips[0].playback = Playback::Forward;
            let mut forward = TestPainter::default();
            timeline.paint(&mut forward, TIMELINE);
            for playback in [Playback::Reverse, Playback::Freeze] {
                timeline.lanes[0].clips[0].playback = playback;
                let mut painter = TestPainter::default();
                timeline.paint(&mut painter, TIMELINE);
                let added: Vec<_> = painter
                    .fills
                    .iter()
                    .filter(|fill| !forward.fills.contains(fill))
                    .collect();
                if duration == 1 {
                    assert!(added.is_empty());
                } else {
                    assert!(!added.is_empty());
                    let x = at(0.0);
                    let end = at(duration as f32);
                    for (rect, _) in added {
                        assert!(rect.x >= x && rect.x + rect.width <= end, "{rect:?}");
                        assert!(
                            rect.y >= RULER_HEIGHT && rect.y + rect.height <= RULER_HEIGHT + 20.0,
                            "{rect:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn inspector_color_controls_for_video_media_only() {
        let state = UiState::new();
        assert!(state.edit(Command::ImportMedia(Media {
            id: 1,
            path: "/color.png".into(),
            kind: MediaKind::Image,
            duration_us: None,
            has_audio: false
        })));
        let mut clip = destination_clip(0, 30);
        clip.content = ClipContent::Media { media_id: 1 };
        let id = clip.id;
        assert!(state.edit(Command::InsertClip {
            track_id: 0,
            clip: Box::new(clip)
        }));
        state.select_clip(Some((0, id)));
        apply_selected_effect(&state, EffectKind::ColorCorrection);
        let painter = render(&state, 1280.0, 2600.0);
        for label in [
            "Color correction",
            "Brightness",
            "Contrast",
            "Saturation",
            "Reset color",
            "Color affects playback and export.",
        ] {
            assert!(painter.labels.iter().any(|text| text == label), "{label}");
        }
        set_clip_color(&state, |color| {
            color.brightness = 0.2;
            color.contrast = 1.3;
            color.saturation = 0.0;
        });
        let painter = render(&state, 1280.0, 2600.0);
        for label in ["20", "130", "0"] {
            assert!(painter.labels.iter().any(|text| text == label), "{label}");
        }
        state.select_clip(None);
        assert!(
            !render(&state, 1280.0, 900.0)
                .labels
                .iter()
                .any(|text| text == "Reset color")
        );
        assert!(state.add_title());
        assert!(
            !render(&state, 1280.0, 900.0)
                .labels
                .iter()
                .any(|text| text == "Reset color")
        );
    }

    #[test]
    fn effect_amount_drag_keeps_capture_while_preview_loads() {
        use crate::timeline::{Effect, EffectShader};
        let state = UiState::new();
        assert!(state.edit(Command::ImportMedia(Media {
            id: 1,
            path: "/effect.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(1_000_000),
            has_audio: false,
        })));
        let mut clip = destination_clip(0, 30);
        clip.content = ClipContent::Media { media_id: 1 };
        clip.effect = Some(Effect {
            shader: EffectShader::Builtin("invert".into()),
            amount: 1.0,
        });
        let id = clip.id;
        assert!(state.edit(Command::InsertClip {
            track_id: 0,
            clip: Box::new(clip)
        }));
        state.select_clip(Some((0, id)));
        apply_selected_effect(&state, EffectKind::Invert);
        state.missing_media.set(HashSet::new());
        state.preview_available.set(true);
        let viewport = Size {
            width: 1280.0,
            height: 736.0,
        };
        let mut renderer = Renderer::new();
        let mut draw = || {
            creamui::reactive::with_context_scope(|| {
                creamui::reactive::provide_context(creamui::theme::ThemeProvider::new(
                    creamui::Theme::dark(),
                ));
                renderer.render(
                    build(&state, viewport),
                    viewport,
                    &mut TestPainter::default(),
                )
            })
        };
        let (scene, index, rect) = (0..100)
            .find_map(|step| {
                state.inspector_scroll.set(step as f32 * 40.0);
                let scene = draw();
                let (index, rect) = (0..)
                    .map_while(|index| scene.draggable_at(index).map(|(rect, _)| (index, rect)))
                    .find(|(_, rect)| {
                        rect.width == 220.0
                            && rect.height == 28.0
                            && rect.x > 900.0
                            && rect.y > 100.0
                            && rect.y < 380.0
                    })?;
                Some((scene, index, rect))
            })
            .expect("scrolled Effect slider is visible");
        let offset = state.inspector_scroll.peek();
        let max_offset = state.inspector_scroll.max_offset();
        assert_eq!(offset, 0.0);
        let pointer = Point {
            x: rect.x + rect.width - 6.0,
            y: rect.y + 14.0,
        };
        let local = Point {
            x: pointer.x - rect.x,
            y: pointer.y - rect.y,
        };
        scene.drag_start_at(pointer).unwrap().1(local, rect);
        scene.draggable_at(index).unwrap().1(local, rect);
        let end = scene.drag_end_at(index).unwrap();
        for amount in [0.95, 0.8, 0.6, 0.3] {
            for preview in [PreviewState::Loading, PreviewState::Ready] {
                state.program.preview.set(preview);
                let scene = draw();
                assert_eq!(state.inspector_scroll.peek(), offset);
                assert_eq!(state.inspector_scroll.max_offset(), max_offset);
                let (current, drag) = scene.draggable_at(index).unwrap();
                drag(
                    Point {
                        x: rect.x + 6.0 + amount * (rect.width - 12.0) - current.x,
                        y: pointer.y - current.y,
                    },
                    current,
                );
                assert_eq!(
                    state.inspector_scroll.peek(),
                    offset,
                    "drag must not reach the scrollbar"
                );
                assert_eq!(current, rect, "captured slider bounds must stay fixed");
                let (target, draft) = state.effect_drag.peek().unwrap();
                assert_eq!(target, (0, id));
                assert!((draft - amount).abs() < 0.00001);
                assert_eq!(selected_clip(&state).unwrap().1.effect.unwrap().amount, 1.0);
            }
        }
        end();
        assert!((selected_clip(&state).unwrap().1.effect.unwrap().amount - 0.3).abs() < 0.00001);
        state.undo();
        assert_eq!(selected_clip(&state).unwrap().1.effect.unwrap().amount, 1.0);
    }

    #[test]
    fn effect_amount_drag_survives_rebuild_and_undoes_once() {
        use crate::timeline::{Effect, EffectShader};
        let state = UiState::new();
        assert!(state.edit(Command::ImportMedia(Media {
            id: 1,
            path: "/effect.png".into(),
            kind: MediaKind::Image,
            duration_us: None,
            has_audio: false
        })));
        let mut clip = destination_clip(0, 30);
        clip.content = ClipContent::Media { media_id: 1 };
        clip.effect = Some(Effect {
            shader: EffectShader::Builtin("invert".into()),
            amount: 1.0,
        });
        let id = clip.id;
        assert!(state.edit(Command::InsertClip {
            track_id: 0,
            clip: Box::new(clip)
        }));
        state.select_clip(Some((0, id)));
        apply_selected_effect(&state, EffectKind::Invert);
        let rect = Rect {
            x: 100.0,
            y: 200.0,
            width: 220.0,
            height: 28.0,
        };
        let slider = || EffectAmount {
            state: state.clone(),
            clip: (0, id),
            amount: 1.0,
            enabled: true,
        };
        slider().on_drag_start().unwrap()(Point { x: 58.0, y: 14.0 }, rect);
        assert_eq!(state.effect_drag.peek(), Some(((0, id), 0.25)));
        slider().on_drag().unwrap()(Point { x: 110.0, y: 14.0 }, rect);
        assert_eq!(selected_clip(&state).unwrap().1.effect.unwrap().amount, 1.0);
        slider().on_drag_end().unwrap()();
        assert!(state.effect_drag.peek().is_none());
        assert_eq!(selected_clip(&state).unwrap().1.effect.unwrap().amount, 0.5);
        state.undo();
        assert_eq!(selected_clip(&state).unwrap().1.effect.unwrap().amount, 1.0);
        state.redo();
        assert_eq!(selected_clip(&state).unwrap().1.effect.unwrap().amount, 0.5);
        let painter = render(&state, 1280.0, 2600.0);
        for label in ["Invert", "Amount", "Remove"] {
            assert!(painter.labels.iter().any(|text| text == label), "{label}");
        }
        slider().on_drag_start().unwrap()(Point { x: 58.0, y: 14.0 }, rect);
        state.select_clip(None);
        slider().on_drag_end().unwrap()();
        assert!(state.effect_drag.peek().is_none());
        assert!(
            !render(&state, 1280.0, 900.0)
                .labels
                .iter()
                .any(|text| text == "Amount")
        );
        assert!(state.add_title());
        assert!(
            !render(&state, 1280.0, 2600.0)
                .labels
                .iter()
                .any(|text| text == "Amount")
        );
    }

    fn numeric_test_state() -> UiState {
        let state = UiState::new();
        assert!(state.edit(Command::ImportMedia(Media {
            id: 1,
            path: "image.png".into(),
            kind: MediaKind::Image,
            duration_us: None,
            has_audio: false,
        })));
        state.insert_media(1);
        state
    }

    fn scale_row(state: &UiState) -> BoxedWidget {
        let transform = selected_clip(state).unwrap().1.transform;
        transform_rows(state, transform, 280.0).remove(1)
    }

    #[test]
    fn numeric_picture_drafts_feed_program_plan_and_escape_reverts() {
        creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(creamui::ThemeProvider::new(creamui::Theme::dark()));
            for property in [
                None,
                Some(ColorProperty::Contrast),
                Some(ColorProperty::Saturation),
            ] {
                let state = numeric_test_state();
                if property.is_some() {
                    apply_selected_effect(&state, EffectKind::ColorCorrection);
                }
                let before = state.editor.borrow().project().clone();
                let revision = state.revision.peek();
                let mut row = if let Some(property) = property {
                    color_control(
                        &state,
                        280.0,
                        property,
                        selected_clip(&state).unwrap().1.color,
                    )
                } else {
                    scale_row(&state)
                };
                let slider = row.children().remove(1);
                let rect = Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 120.0,
                    height: 24.0,
                };
                slider.on_drag_start().unwrap()(Point { x: 80.0, y: 12.0 }, rect);
                slider.on_drag().unwrap()(Point { x: 80.0, y: 12.0 }, rect);
                let label = property.map_or("Scale", ColorProperty::label);
                let value: f32 = state.numeric_inputs.borrow()
                    [&(state.primary_clip().unwrap(), label)]
                    .controller
                    .peek()
                    .parse::<f32>()
                    .unwrap()
                    / 100.0;
                let (plan, _) = state.program_plan();
                let source = &plan.sources[0];
                assert!(
                    (property.map_or(source.transform.scale, |property| property
                        .get(source.color))
                        - value)
                        .abs()
                        < 0.0001
                );
                assert_ne!(value, 1.0);
                assert_eq!(state.editor.borrow().project(), &before);
                assert_eq!(state.revision.peek(), revision);
                state.run(Action::Escape);
                slider.on_drag().unwrap()(Point { x: 100.0, y: 12.0 }, rect);
                slider.on_drag_end().unwrap()();
                assert_eq!(
                    state.primary_clip(),
                    Some((0, before.tracks()[0].clips[0].id))
                );
                assert_eq!(state.editor.borrow().project(), &before);
                assert_eq!(state.revision.peek(), revision);
                let (plan, _) = state.program_plan();
                let source = &plan.sources[0];
                assert_eq!(
                    property.map_or(source.transform.scale, |property| property
                        .get(source.color)),
                    property.map_or(before.tracks()[0].clips[0].transform.scale, |property| {
                        property.get(before.tracks()[0].clips[0].color)
                    })
                );
                slider.on_drag_start().unwrap()(Point { x: 80.0, y: 12.0 }, rect);
                slider.on_drag().unwrap()(Point { x: 80.0, y: 12.0 }, rect);
                slider.on_drag_end().unwrap()();
                assert_eq!(state.revision.peek(), revision + 1);
                state.undo();
                assert_eq!(state.editor.borrow().project(), &before);
            }
        });
    }

    #[test]
    fn title_size_draft_changes_program_pixels_without_editing() {
        let state = UiState::new();
        assert!(state.add_title());
        let before = state.editor.borrow().project().clone();
        let pixels = || {
            let plan = state.program_plan().0.visual.unwrap();
            crate::media::composite::Compositor::new(plan, 320, 180, (30, 1))
                .frame(
                    0,
                    &std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                )
                .unwrap()
                .unwrap()
                .pixels
        };
        let original = pixels();
        creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(creamui::ThemeProvider::new(creamui::Theme::dark()));
            let mut painter = TestPainter::default();
            let scene = Renderer::new().render(
                inspector(&state, &before, 320.0, false),
                Size {
                    width: 320.0,
                    height: 2400.0,
                },
                &mut painter,
            );
            let label = painter
                .bounds
                .iter()
                .find(|(label, _)| label == "Font size (px)")
                .unwrap()
                .1;
            let point = Point {
                x: label.x + 120.0,
                y: label.y + label.height / 2.0,
            };
            let index = scene.drag_hit_test(point).unwrap();
            let (rect, start) = scene.drag_start_at(point).unwrap();
            let at = Point {
                x: rect.width * 0.8,
                y: rect.height / 2.0,
            };
            start(at, rect);
            scene.draggable_at(index).unwrap().1(at, rect);
            assert_ne!(pixels(), original);
            assert_eq!(state.editor.borrow().project(), &before);
            state.run(Action::Escape);
            scene.drag_end_at(index).unwrap()();
            assert_eq!(pixels(), original);
            assert_eq!(state.editor.borrow().project(), &before);
        });
    }

    #[test]
    fn numeric_enter_and_remount_blur_commit_once_without_rejection_status() {
        creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(creamui::ThemeProvider::new(creamui::Theme::dark()));
            for contrast in [false, true] {
                let state = numeric_test_state();
                if contrast {
                    apply_selected_effect(&state, EffectKind::ColorCorrection);
                }
                let before = state.editor.borrow().project().clone();
                let revision = state.revision.peek();
                let row = || {
                    let clip = selected_clip(&state).unwrap().1;
                    if contrast {
                        color_control(&state, 280.0, ColorProperty::Contrast, clip.color)
                    } else {
                        transform_rows(&state, clip.transform, 280.0).remove(4)
                    }
                };
                let mut renderer = Renderer::new();
                let mut painter = TestPainter::default();
                let size = Size {
                    width: 280.0,
                    height: 24.0,
                };
                let scene = renderer.render(row(), size, &mut painter);
                let index = (0..8)
                    .find(|index| scene.focus_accepts_text_input(*index))
                    .unwrap();
                let label = if contrast { "Contrast" } else { "Rotation" };
                state.numeric_inputs.borrow()[&(state.primary_clip().unwrap(), label)]
                    .controller
                    .set_value(if contrast { "70" } else { "45" });
                scene.on_key_at(index).unwrap()(creamui::core::KeyInput {
                    key: creamui::core::Key::Enter,
                    modifiers: Default::default(),
                });
                assert!(
                    !state.status.peek().starts_with("Edit rejected"),
                    "{}",
                    state.status.peek()
                );
                scene.on_focus_change_at(index).unwrap()(false);
                renderer.render(row(), size, &mut painter);
                scene.on_focus_change_at(index).unwrap()(false);
                assert!(
                    !state.status.peek().starts_with("Edit rejected"),
                    "{label}: {}",
                    state.status.peek()
                );
                assert_eq!(state.revision.peek(), revision + 1);
                let clip = selected_clip(&state).unwrap().1;
                assert_eq!(
                    if contrast {
                        clip.color.contrast
                    } else {
                        clip.transform.rotation
                    },
                    if contrast { 0.7 } else { 45.0 }
                );
                let mut no_op = row();
                let slider = no_op.children().remove(1);
                let rect = Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 120.0,
                    height: 24.0,
                };
                slider.on_drag_start().unwrap()(Point { x: 0.0, y: 0.0 }, rect);
                slider.on_drag_end().unwrap()();
                assert!(
                    !state.status.peek().starts_with("Edit rejected"),
                    "{label}: {}",
                    state.status.peek()
                );
                assert_eq!(state.revision.peek(), revision + 1);
                let applied = state.editor.borrow().project().clone();
                state.undo();
                assert_eq!(state.editor.borrow().project(), &before);
                state.redo();
                assert_eq!(state.editor.borrow().project(), &applied);
            }
        });
    }

    #[test]
    fn numeric_enter_removes_the_focused_input_identity_for_global_undo() {
        let state = numeric_test_state();
        let before = state.editor.borrow().project().clone();
        creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(creamui::ThemeProvider::new(creamui::Theme::dark()));
            let mut renderer = Renderer::new();
            let mut painter = TestPainter::default();
            let size = Size {
                width: 280.0,
                height: 24.0,
            };
            let scene = renderer.render(scale_row(&state), size, &mut painter);
            let index = (0..8)
                .find(|index| scene.focus_accepts_text_input(*index))
                .unwrap();
            let focused = scene.focus_id_at(index).unwrap();
            state.numeric_inputs.borrow()[&(state.primary_clip().unwrap(), "Scale")]
                .controller
                .set_value("160");
            scene.on_key_at(index).unwrap()(creamui::core::KeyInput {
                key: creamui::core::Key::Enter,
                modifiers: Default::default(),
            });
            assert_eq!(selected_clip(&state).unwrap().1.transform.scale, 1.6);
            let next =
                renderer.render_focused(scale_row(&state), size, &mut painter, Some(index), true);
            assert!(next.focus_index(focused).is_none());
            scene.on_focus_change_at(index).unwrap()(false);
            state.run(Action::Undo);
            assert_eq!(state.editor.borrow().project(), &before);
        });
    }

    #[test]
    fn numeric_slider_keeps_drafts_across_rebuilds_and_commits_one_undo_entry() {
        let state = numeric_test_state();
        let before = state.editor.borrow().project().clone();
        let revision = state.revision.peek();
        creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(creamui::ThemeProvider::new(creamui::Theme::dark()));
            let rect = Rect {
                x: 0.0,
                y: 0.0,
                width: 120.0,
                height: 28.0,
            };
            let mut row = scale_row(&state);
            let slider = row.children().remove(1);
            slider.on_drag_start().unwrap()(Point { x: 40.0, y: 14.0 }, rect);
            slider.on_drag().unwrap()(Point { x: 40.0, y: 14.0 }, rect);
            let mut rebuilt = scale_row(&state);
            let slider = rebuilt.children().remove(1);
            slider.on_drag().unwrap()(Point { x: 80.0, y: 14.0 }, rect);
            assert_eq!(state.editor.borrow().project(), &before);
            assert_eq!(state.revision.peek(), revision);
            assert_ne!(
                state.numeric_inputs.borrow()[&(state.primary_clip().unwrap(), "Scale")]
                    .controller
                    .peek(),
                "100"
            );
            slider.on_drag_end().unwrap()();
            slider.on_drag_end().unwrap()();
        });
        assert_eq!(state.revision.peek(), revision + 1);
        let applied = state.editor.borrow().project().clone();
        assert_ne!(applied, before);
        state.undo();
        assert_eq!(state.editor.borrow().project(), &before);
        state.redo();
        assert_eq!(state.editor.borrow().project(), &applied);
    }

    #[test]
    fn numeric_fields_commit_once_on_enter_clamp_reject_and_reset() {
        let state = numeric_test_state();
        let before = state.editor.borrow().project().clone();
        let revision = state.revision.peek();
        submit_numeric(&state, "Scale", "125.5", || scale_row(&state));
        assert_eq!(state.revision.peek(), revision + 1);
        assert_eq!(selected_clip(&state).unwrap().1.transform.scale, 1.255);
        let applied = state.editor.borrow().project().clone();
        state.undo();
        assert_eq!(state.editor.borrow().project(), &before);
        state.redo();
        assert_eq!(state.editor.borrow().project(), &applied);
        let revision = state.revision.peek();
        for invalid in ["", "invalid", "NaN", "inf"] {
            submit_numeric(&state, "Scale", invalid, || scale_row(&state));
            assert_eq!(state.revision.peek(), revision);
            assert_eq!(selected_clip(&state).unwrap().1.transform.scale, 1.255);
        }
        for (typed, expected) in [("999", 4.0), ("-10", 0.1)] {
            submit_numeric(&state, "Scale", typed, || scale_row(&state));
            assert_eq!(selected_clip(&state).unwrap().1.transform.scale, expected);
        }
        creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(creamui::ThemeProvider::new(creamui::Theme::dark()));
            for part in [0, 1] {
                submit_numeric(&state, "Scale", "200", || scale_row(&state));
                let mut row = scale_row(&state);
                row.children()[part].on_double_click_at().unwrap()(Point { x: 20.0, y: 14.0 });
                assert_eq!(selected_clip(&state).unwrap().1.transform.scale, 1.0);
                state.undo();
                assert_eq!(selected_clip(&state).unwrap().1.transform.scale, 2.0);
            }
        });
    }

    #[test]
    fn numeric_controls_ignore_empty_busy_locked_and_stale_selections() {
        let state = numeric_test_state();
        creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(creamui::ThemeProvider::new(creamui::Theme::dark()));
            let before = state.editor.borrow().project().clone();
            let mut row = scale_row(&state);
            let slider = row.children().remove(1);
            let rect = Rect {
                x: 0.0,
                y: 0.0,
                width: 120.0,
                height: 28.0,
            };
            slider.on_drag_start().unwrap()(Point { x: 80.0, y: 14.0 }, rect);
            slider.on_drag().unwrap()(Point { x: 80.0, y: 14.0 }, rect);
            state.select_clip(None);
            slider.on_drag_end().unwrap()();
            assert_eq!(state.editor.borrow().project(), &before);
            let mut empty = timing_steps(&state, false);
            assert!(numeric_key(empty.as_mut()).is_none());
            state.select_clip(Some((0, 1)));
            state.busy.set(true);
            submit_numeric(&state, "Scale", "200", || scale_row(&state));
            assert_eq!(state.editor.borrow().project(), &before);
            state.busy.set(false);
            let mut row = scale_row(&state);
            let reset = row.children()[0].on_double_click_at().unwrap();
            assert!(state.edit(Command::SetTrackLocked {
                track_id: 0,
                locked: true
            }));
            let locked = state.editor.borrow().project().clone();
            reset(Point { x: 0.0, y: 0.0 });
            assert_eq!(state.editor.borrow().project(), &locked);
        });
    }

    #[test]
    fn numeric_rows_keep_fields_inside_a_narrow_inspector() {
        let state = numeric_test_state();
        creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(creamui::ThemeProvider::new(creamui::Theme::dark()));
            let mut painter = TestPainter::default();
            Renderer::new().render(
                scale_row(&state),
                Size {
                    width: 220.0,
                    height: 24.0,
                },
                &mut painter,
            );
            for (label, rect) in &painter.visible_bounds {
                assert!(
                    rect.x >= 0.0 && rect.x + rect.width <= 220.0,
                    "{label}: {rect:?}"
                );
            }
            assert!(painter.labels.iter().any(|label| label == "100"));
            for label in ["Scale", "100", "%"] {
                assert!(
                    painter.visible_bounds.iter().any(|(text, rect)| {
                        text == label && rect.y >= 0.0 && rect.y + rect.height <= 24.0
                    }),
                    "{label}"
                );
            }
            let mut row = scale_row(&state);
            let mut children = row.children();
            assert_eq!(children.len(), 3);
            let field = children.last_mut().unwrap();
            assert_eq!(field.style().layout.size.width, Dimension::Length(56.0));
            assert_eq!(field.style().layout.size.height, Dimension::Length(24.0));
            assert_eq!(
                field.children()[0].style().layout.size.height,
                Dimension::Length(22.0)
            );
            assert!(
                !painter
                    .labels
                    .iter()
                    .any(|label| label == "+" || label == "−")
            );
        });
    }

    #[test]
    fn color_fields_keep_bounds_steps_and_fine_precision() {
        let state = UiState::new();
        assert!(state.edit(Command::ImportMedia(Media {
            id: 1,
            path: "image.png".into(),
            kind: MediaKind::Image,
            duration_us: None,
            has_audio: false,
        })));
        state.insert_media(1);
        apply_selected_effect(&state, EffectKind::ColorCorrection);
        for property in [
            ColorProperty::Brightness,
            ColorProperty::Contrast,
            ColorProperty::Saturation,
        ] {
            for (value, expected) in [
                ("-1000", property.range().0),
                ("1000", property.range().1),
                ("12.3", 0.123),
            ] {
                let color = selected_clip(&state).unwrap().1.color;
                submit_numeric(&state, property.label(), value, || {
                    color_control(&state, 280.0, property, color)
                });
                assert!(
                    (property.get(selected_clip(&state).unwrap().1.color) - expected).abs()
                        < 0.0001
                );
            }
            let color = selected_clip(&state).unwrap().1.color;
            submit_numeric(&state, property.label(), "0", || {
                color_control(&state, 280.0, property, color)
            });
            creamui::reactive::with_context_scope(|| {
                creamui::reactive::provide_context(creamui::ThemeProvider::new(
                    creamui::Theme::dark(),
                ));
                let color = selected_clip(&state).unwrap().1.color;
                let mut row = color_control(&state, 280.0, property, color);
                numeric_key(row.as_mut()).unwrap()(creamui::core::KeyInput {
                    key: creamui::core::Key::Up,
                    modifiers: Default::default(),
                });
            });
            assert_eq!(
                property.get(selected_clip(&state).unwrap().1.color),
                1.0 / property.steps_per_unit()
            );
        }
    }

    #[test]
    fn program_monitor_distinguishes_audio_from_a_gap() {
        let state = UiState::new();
        let painter = render(&state, 1280.0, 760.0);
        assert!(
            painter
                .labels
                .iter()
                .any(|label| label == "Start your edit")
        );
        assert!(state.edit(Command::ImportMedia(Media {
            id: 1,
            path: "/tmp/audio.wav".into(),
            kind: MediaKind::Audio,
            duration_us: Some(3_000_000),
            has_audio: true,
        })));
        state.insert_media(1);
        state.focus_frame(0);
        let painter = render(&state, 1280.0, 760.0);
        assert!(painter.labels.iter().any(|label| label == "Audio only"));
        set_clip_audio(&state, |clip| (clip.volume, true));
        let painter = render(&state, 1280.0, 760.0);
        assert!(painter.labels.iter().any(|label| label == "Audio only"));
        assert!(
            !painter
                .labels
                .iter()
                .any(|label| label == "No clip at this frame")
        );
        state.focus_frame(90);
        let painter = render(&state, 1280.0, 760.0);
        assert!(
            painter
                .labels
                .iter()
                .any(|label| label == "No clip at this frame")
        );
        assert!(!painter.labels.iter().any(|label| label == "Audio only"));
    }

    #[test]
    fn empty_export_explains_how_to_enable_it() {
        let state = UiState::new();
        render(&state, 1280.0, 760.0);
        state
            .tooltip_controller(("Add clips to the timeline to export", 0))
            .focus(true);
        let painter = render(&state, 1280.0, 760.0);
        assert!(
            painter
                .labels
                .iter()
                .any(|label| label == "Add clips to the timeline to export")
        );
    }

    #[test]
    fn export_dialog_wraps_full_error_reasons() {
        let state = UiState::new();
        let reason = "Cannot export: relink the missing media used in your timeline first. The source file could not be found at its original location. Open the media bin, select the missing source, and use Relink media to choose its new location before exporting again.";
        state.export_error.set(Some(reason.into()));
        state.status.set(reason.into());
        creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(creamui::ThemeProvider::new(creamui::Theme::dark()));
            let viewport = Size {
                width: 760.0,
                height: 760.0,
            };
            let mut painter = TestPainter::default();
            Renderer::new().render(
                export_modal(&state, state.editor.borrow().project(), viewport),
                viewport,
                &mut painter,
            );
            let (_, rect) = painter
                .bounds
                .iter()
                .find(|(label, _)| label == reason)
                .expect("full export error");
            assert!(rect.height > 11.0 * 1.4, "{rect:?}");
        });
    }

    #[test]
    fn bin_thumbnail_without_a_preview_has_an_explicit_fallback() {
        let state = UiState::new();
        assert!(state.edit(Command::ImportMedia(Media {
            id: 1,
            path: "/tmp/video.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(2_000_000),
            has_audio: false,
        })));
        let painter = render(&state, 1280.0, 760.0);
        assert!(painter.labels.iter().any(|label| label == "No preview"));
        let (_, placeholder) = painter
            .bounds
            .iter()
            .find(|(label, _)| label == "No preview")
            .unwrap();
        let (_, duration) = painter
            .bounds
            .iter()
            .find(|(label, _)| label == "0:02")
            .unwrap();
        assert!(
            duration.y + duration.height <= placeholder.y
                || placeholder.y + placeholder.height <= duration.y
        );
        for view in [MediaView::Grid, MediaView::List] {
            state.media_view.set(view);
            state
                .thumbnail_loading
                .set(HashSet::from(["/tmp/video.mp4".into()]));
            let painter = render(&state, 1280.0, 760.0);
            assert!(painter.labels.iter().any(|label| label == "Loading…"));
            assert!(!painter.labels.iter().any(|label| label == "No preview"));
            state.thumbnail_loading.set(HashSet::new());
            let painter = render(&state, 1280.0, 760.0);
            assert!(painter.labels.iter().any(|label| label == "No preview"));
        }
    }

    #[test]
    fn timeline_shows_a_failed_storyboard_fallback() {
        let state = UiState::new();
        let path = std::path::PathBuf::from("/tmp/video.mp4");
        assert!(state.edit(Command::ImportMedia(Media {
            id: 1,
            path: path.clone(),
            kind: MediaKind::Video,
            duration_us: Some(2_000_000),
            has_audio: false,
        })));
        state.thumbnails.borrow_mut().insert(
            path.clone(),
            Arc::new(RgbaImage::new(1, 1, vec![255; 4]).unwrap()),
        );
        state.insert_media(1);
        let editor = state.editor.borrow();
        let project = editor.project();
        let clip = project
            .tracks()
            .iter()
            .filter(|track| track.kind == TrackKind::Video)
            .flat_map(|track| &track.clips)
            .next()
            .unwrap();
        let key = StoryboardKey {
            transform_bits: StoryboardKey::transform_bits(crate::timeline::Transform::default()),
            effect: None,
            mask: None,
            lut: None,
            chroma_key: None,
            color_bits: [0.0f32, 1.0, 1.0].map(f32::to_bits),
            playback: Playback::Forward,
            path,
            kind: MediaKind::Video,
            source_start: clip.source_start_frame,
            duration: clip.duration_frames,
            fps: (
                project.settings().frame_rate.numerator,
                project.settings().frame_rate.denominator,
            ),
        };
        drop(editor);
        state
            .storyboards
            .borrow_mut()
            .insert(key.clone(), Arc::new(Vec::new()));
        let painter = render(&state, 1280.0, 760.0);
        assert!(painter.labels.iter().any(|label| label == "No preview"));
        state.storyboards.borrow_mut().insert(
            key,
            Arc::new(vec![Arc::new(RgbaImage::new(1, 1, vec![255; 4]).unwrap())]),
        );
        let painter = render(&state, 1280.0, 760.0);
        assert!(!painter.labels.iter().any(|label| label == "No preview"));
    }

    #[test]
    fn monitors_show_loading_and_retryable_errors() {
        let state = UiState::new();
        for monitor in [&state.source, &state.program] {
            monitor.preview.set(PreviewState::Loading);
            let painter = render(&state, 1280.0, 760.0);
            assert!(
                painter
                    .labels
                    .iter()
                    .any(|label| label == "Loading preview…")
            );
            monitor.preview.set(PreviewState::Ready);
            let painter = render(&state, 1280.0, 760.0);
            assert!(
                !painter
                    .labels
                    .iter()
                    .any(|label| label == "Loading preview…")
            );
            monitor
                .preview
                .set(PreviewState::Failed("Cannot decode frame".into()));
            let painter = render(&state, 1280.0, 760.0);
            assert!(painter.labels.iter().any(|label| label == "Preview failed"));
            assert!(
                painter
                    .labels
                    .iter()
                    .any(|label| label == "Cannot decode frame")
            );
            assert!(painter.labels.iter().any(|label| label == "Retry"));
            creamui::reactive::with_context_scope(|| {
                creamui::reactive::provide_context(creamui::ThemeProvider::new(
                    creamui::Theme::dark(),
                ));
                let viewport = Size {
                    width: 1280.0,
                    height: 760.0,
                };
                let mut painter = TestPainter::default();
                let scene = Renderer::new().render(build(&state, viewport), viewport, &mut painter);
                let (_, rect) = painter
                    .bounds
                    .iter()
                    .find(|(label, _)| label == "Retry")
                    .unwrap();
                scene
                    .hit_test(Point {
                        x: rect.x + rect.width / 2.0,
                        y: rect.y + rect.height / 2.0,
                    })
                    .unwrap()();
            });
            assert_eq!(monitor.preview.peek(), PreviewState::Ready);
            let painter = render(&state, 1280.0, 760.0);
            assert!(!painter.labels.iter().any(|label| label == "Preview failed"));
        }
    }

    #[test]
    fn preview_error_reason_wraps_in_a_narrow_monitor() {
        let state = UiState::new();
        let reason = "The selected video could not be decoded because its codec is unavailable on this system. Choose another source or retry the preview.";
        state
            .source
            .preview
            .set(PreviewState::Failed(reason.into()));
        creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(creamui::ThemeProvider::new(creamui::Theme::dark()));
            let viewport = Size {
                width: 240.0,
                height: 400.0,
            };
            let mut painter = TestPainter::default();
            Renderer::new().render(
                preview_stage(&state, PlaybackMonitor::Source).unwrap(),
                viewport,
                &mut painter,
            );
            let (_, rect) = painter
                .bounds
                .iter()
                .find(|(label, _)| label == reason)
                .unwrap();
            assert!(rect.width <= viewport.width);
            assert!(rect.height > 22.0);
        });
    }

    #[test]
    fn source_monitor_explains_missing_bin_and_clip_sources() {
        let state = UiState::new();
        assert!(state.edit(Command::ImportMedia(Media {
            id: 1,
            path: "/tmp/lost.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(2_000_000),
            has_audio: false,
        })));
        state.missing_media.set(HashSet::from([1]));
        state.select_media(1);
        let painter = render(&state, 1280.0, 760.0);
        assert!(
            painter
                .labels
                .iter()
                .any(|label| label == "lost.mp4 is missing.")
        );
        assert!(painter.labels.iter().any(|label| label == "Relink media…"));
        assert!(
            !painter
                .labels
                .iter()
                .any(|label| label == "No source selected")
        );
        state.missing_media.set(HashSet::new());
        state.insert_media(1);
        state.missing_media.set(HashSet::from([1]));
        let painter = render(&state, 1280.0, 760.0);
        assert!(
            painter
                .labels
                .iter()
                .any(|label| label == "lost.mp4 is missing.")
        );
    }

    #[test]
    fn program_monitor_marks_only_frames_whose_media_is_missing() {
        let state = UiState::new();
        for (id, name) in [(1, "/tmp/lost.mp4"), (2, "/tmp/kept.mp4")] {
            assert!(state.edit(Command::ImportMedia(Media {
                id,
                path: name.into(),
                kind: MediaKind::Video,
                duration_us: Some(2_000_000),
                has_audio: false,
            })));
            state.insert_media(id);
        }
        state.missing_media.set(HashSet::from([1]));
        state.select_clips(Vec::new());
        state.focus_frame(70);
        let painter = render(&state, 1280.0, 760.0);
        assert!(
            painter
                .labels
                .iter()
                .any(|label| label == "Video preview unavailable")
        );
        assert!(!painter.labels.iter().any(|label| label == "Missing source"));
        state.focus_frame(10);
        let painter = render(&state, 1280.0, 760.0);
        assert!(
            painter
                .labels
                .iter()
                .any(|label| label == "lost.mp4 is missing at this frame.")
        );
    }

    #[test]
    fn icon_help_survives_rebuilds_without_changing_clicks_or_disabled_state() {
        let state = UiState::new();
        let viewport = Size {
            width: 320.0,
            height: 160.0,
        };
        state.tooltip_viewport.set(viewport);
        creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(creamui::ThemeProvider::new(creamui::Theme::dark()));
            let mut renderer = Renderer::new();
            let button = |enabled| {
                icon_button(
                    Glyph::Undo,
                    Help::new("Undo").shortcut("Ctrl+Z"),
                    enabled,
                    &state,
                    |s| s.status.set("Clicked".into()),
                )
            };
            let mut painter = TestPainter::default();
            let scene = renderer.render(button(true), viewport, &mut painter);
            assert!(!painter.labels.iter().any(|label| label == "Undo"));
            let point = Point { x: 14.0, y: 14.0 };
            let index = scene.focus_hit_test(point).unwrap();
            let focus = scene.on_focus_change_at(index).unwrap();
            focus(true);
            let mut painter = TestPainter::default();
            let scene = renderer.render(button(true), viewport, &mut painter);
            assert!(painter.labels.iter().any(|label| label == "Undo"));
            assert!(painter.labels.iter().any(|label| label == "Ctrl+Z"));
            assert_eq!(scene.focus_rect_at(index).unwrap().width, 28.0);
            scene.hit_test(point).unwrap()();
            assert_eq!(state.status.peek(), "Clicked");
            focus(false);
            let mut painter = TestPainter::default();
            let scene = renderer.render(button(false), viewport, &mut painter);
            assert!(!painter.labels.iter().any(|label| label == "Undo"));
            assert!(scene.hit_test(point).is_none());
            scene
                .on_focus_change_at(scene.focus_hit_test(point).unwrap())
                .unwrap()(true);
            let mut painter = TestPainter::default();
            let scene = renderer.render(button(false), viewport, &mut painter);
            assert!(painter.labels.iter().any(|label| label == "Undo"));
            assert!(scene.hit_test(point).is_none());
        });
    }

    #[test]
    fn track_toggle_hover_help_survives_window_rebuilds() {
        for (width, height) in [(1280.0, 760.0), (2560.0, 1856.0)] {
            let state = UiState::new();
            for (track_id, hidden, locked, label) in [
                (0, false, false, "Lock track"),
                (0, false, true, "Unlock track"),
                (0, false, false, "Hide track"),
                (0, true, false, "Show track"),
                (2, false, false, "Mute track"),
                (2, true, false, "Unmute track"),
            ] {
                state.edit(Command::SetTrackHidden { track_id, hidden });
                state.edit(Command::SetTrackLocked { track_id, locked });
                render(&state, width, height);
                let help = state.tooltip_controller((label, track_id));
                creamui::reactive::with_context_scope(|| {
                    creamui::reactive::provide_context(creamui::ThemeProvider::new(
                        creamui::Theme::dark(),
                    ));
                    let viewport = Size { width, height };
                    let mut painter = TestPainter::default();
                    let scene =
                        Renderer::new().render(build(&state, viewport), viewport, &mut painter);
                    let caption = if track_id == 0 { "V1" } else { "A1" };
                    let header = painter
                        .bounds
                        .iter()
                        .find(|(text, _)| text == caption)
                        .unwrap()
                        .1;
                    let point = Point {
                        x: state.timeline_rect.get().x
                            + if matches!(label, "Lock track" | "Unlock track") {
                                111.0
                            } else {
                                137.0
                            },
                        y: header.y + 9.0,
                    };
                    scene.hover_hit_test(point).unwrap().1(true);
                });
                std::thread::sleep(std::time::Duration::from_millis(400));
                render(&state, width, height);
                let painter = render(&state, width, height);
                assert!(help.visible(), "{label} at {width}×{height}");
                assert!(painter.labels.iter().any(|text| text == label));
                help.hover(false);
            }
        }
    }

    #[test]
    fn hovered_help_replaces_an_adjacent_focused_hint() {
        let state = UiState::new();
        render(&state, 1280.0, 760.0);
        let zoom_in = state.tooltip_controller(("Zoom in", 0));
        let zoom_out = state.tooltip_controller(("Zoom out", 0));
        zoom_in.focus(true);
        zoom_out.hover(true);
        std::thread::sleep(std::time::Duration::from_millis(400));
        render(&state, 1280.0, 760.0);
        let painter = render(&state, 1280.0, 760.0);
        assert!(zoom_out.visible());
        assert!(!zoom_in.visible());
        assert!(painter.labels.iter().any(|label| label == "Zoom out"));
        assert!(!painter.labels.iter().any(|label| label == "Zoom in"));
    }

    #[test]
    fn repeated_media_buttons_have_independent_help_and_keep_their_size() {
        let state = UiState::new();
        let viewport = Size {
            width: 320.0,
            height: 160.0,
        };
        state.tooltip_viewport.set(viewport);
        creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(creamui::ThemeProvider::new(creamui::Theme::dark()));
            let buttons = || {
                Box::new(
                    Flex::row()
                        .gap(8.0)
                        .child(sized_icon_button(
                            Glyph::Plus,
                            Help::new("Insert at timeline end").instance(1),
                            24.0,
                            true,
                            false,
                            &state,
                            |_| {},
                        ))
                        .child(sized_icon_button(
                            Glyph::Plus,
                            Help::new("Insert at timeline end").instance(2),
                            24.0,
                            true,
                            false,
                            &state,
                            |_| {},
                        )),
                ) as BoxedWidget
            };
            let mut renderer = Renderer::new();
            let scene = renderer.render(buttons(), viewport, &mut TestPainter::default());
            assert_eq!(scene.focus_rect_at(0).unwrap().width, 24.0);
            assert_eq!(scene.focus_rect_at(1).unwrap().x, 32.0);
            scene.on_focus_change_at(0).unwrap()(true);
            let mut painter = TestPainter::default();
            renderer.render(buttons(), viewport, &mut painter);
            assert_eq!(
                painter
                    .labels
                    .iter()
                    .filter(|label| *label == "Insert at timeline end")
                    .count(),
                1
            );
        });
    }

    #[test]
    fn source_transport_does_not_advertise_sequence_keyboard_shortcuts() {
        let state = UiState::new();
        let shortcut_count = |painter: &TestPainter, shortcut| {
            painter
                .labels
                .iter()
                .filter(|label| *label == shortcut)
                .count()
        };
        let baseline = render(&state, 1280.0, 760.0);
        let source = state.tooltip_controller(("Play / pause source", 0));
        source.focus(true);
        let painter = render(&state, 1280.0, 760.0);
        assert!(
            painter
                .labels
                .iter()
                .any(|label| label == "Play / pause source")
        );
        assert_eq!(
            shortcut_count(&painter, "Space"),
            shortcut_count(&baseline, "Space") + 1
        );
        assert_eq!(
            shortcut_count(&painter, "Space / K"),
            shortcut_count(&baseline, "Space / K")
        );
        source.focus(false);
        state
            .tooltip_controller(("Play / pause sequence", 0))
            .focus(true);
        let painter = render(&state, 1280.0, 760.0);
        assert_eq!(
            shortcut_count(&painter, "Space / K"),
            shortcut_count(&baseline, "Space / K") + 1
        );
    }

    #[test]
    fn title_added_over_footage_appears_in_program_monitor() {
        let state = UiState::new();
        assert!(state.edit(Command::ImportMedia(Media {
            id: 1,
            path: "/tmp/footage.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(3_000_000),
            has_audio: false,
        })));
        state.insert_media(1);
        state.focus_frame(0);
        assert!(state.add_title());
        let project = state.editor.borrow();
        let title = &project.project().tracks()[1].clips[0];
        assert_eq!(title.start_frame, 0);
        drop(project);
        state.select_clip(None);
        let painter = render(&state, 1280.0, 760.0);
        assert!(
            painter
                .bounds
                .iter()
                .any(|(label, rect)| { label == "Title 2" && rect.y < 450.0 })
        );
        state.focus_frame(90);
        let painter = render(&state, 1280.0, 760.0);
        assert!(
            !painter
                .bounds
                .iter()
                .any(|(label, rect)| label == "Title 2" && rect.y < 450.0)
        );
    }

    #[test]
    fn program_keeps_the_unavailable_video_notice_under_a_title() {
        let state = UiState::new();
        assert!(state.edit(Command::ImportMedia(Media {
            id: 1,
            path: "/tmp/footage.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(3_000_000),
            has_audio: false,
        })));
        state.insert_media(1);
        state.focus_frame(0);
        state.select_clip(None);
        let notices = |painter: &TestPainter| {
            painter
                .labels
                .iter()
                .filter(|label| *label == "Video preview unavailable")
                .count()
        };
        let without_title = notices(&render(&state, 1280.0, 760.0));
        assert!(without_title > 0);
        assert!(state.add_title());
        state.select_clip(None);
        let painter = render(&state, 1280.0, 760.0);
        assert!(painter.labels.iter().any(|label| label == "Title 2"));
        assert_eq!(notices(&painter), without_title);
    }

    #[test]
    fn a_title_can_play_without_a_video_device() {
        let state = UiState::new();
        assert!(state.add_title());
        state.play();
        assert!(state.program.playing.peek());
        state.pause();
    }

    #[test]
    fn inspector_does_not_show_internal_preview_diagnostics() {
        let state = UiState::new();
        let painter = render(&state, 1280.0, 1600.0);
        assert!(!painter.labels.iter().any(|label| {
            label.contains("GPU video preview ready") || label.contains("audio pending")
        }));
        assert!(state.add_title());
        let painter = render(&state, 1280.0, 1600.0);
        assert!(!painter.labels.iter().any(|label| label == "Source in"));
    }

    #[test]
    fn titles_use_transparent_text_over_the_program_frame() {
        let stage = Rect {
            x: 0.0,
            y: 0.0,
            width: 640.0,
            height: 360.0,
        };
        let title = Title {
            text: "Over footage".into(),
            font_family: "Sans".into(),
            font_size: 72.0,
            color: [245, 210, 80, 160],
            x: 0.25,
            y: 0.7,
            ..Default::default()
        };
        let mut painter = TestPainter::default();
        ProgramCanvas {
            slot: GpuFrameSlot::new(),
            picture: false,
            picture_unavailable: false,
            titles: vec![((0, 1), title)],
            selected: None,
            state: UiState::new(),
            aspect: 16.0 / 9.0,
            resolution: (1920, 1080),
        }
        .paint(&mut painter, stage);
        assert!(painter.fills.is_empty());
        assert!(painter.labels.iter().all(|label| label == "Over footage"));
        let (_, rect) = painter.bounds.last().unwrap();
        assert!(rect.y > stage.height / 2.0);
        assert_eq!(
            painter.text_colors.last().unwrap().1,
            Color::rgba(245, 210, 80, 160)
        );
    }

    #[test]
    fn feather_drag_keeps_inspector_scroll_and_drag_target() {
        let state = UiState::new();
        assert!(state.edit(Command::ImportMedia(Media {
            id: 1,
            path: "/mask.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(2_000_000),
            has_audio: false,
        })));
        state.insert_media(1);
        apply_selected_effect(&state, EffectKind::Mask);
        set_clip_mask(&state, Some(Mask::default()));
        state.preview_available.set(true);
        creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(creamui::ThemeProvider::new(creamui::Theme::dark()));
            let viewport = Size {
                width: 1600.0,
                height: 1160.0,
            };
            let mut renderer = Renderer::new();
            let mut painter = TestPainter::default();
            let tall = Size {
                height: 5000.0,
                ..viewport
            };
            renderer.render(build(&state, tall), tall, &mut painter);
            let feather = painter
                .bounds
                .iter()
                .find(|(label, _)| label == "Feather")
                .unwrap()
                .1;
            renderer.render(
                build(&state, viewport),
                viewport,
                &mut TestPainter::default(),
            );
            state.inspector_scroll.set(feather.y - 610.0);
            let mut painter = TestPainter::default();
            let scene = renderer.render(build(&state, viewport), viewport, &mut painter);
            let feather = painter
                .bounds
                .iter()
                .find(|(label, _)| label == "Feather")
                .unwrap()
                .1;
            let pointer = Point {
                x: feather.x + 110.0,
                y: feather.y + feather.height / 2.0,
            };
            let index = scene.drag_hit_test(pointer).unwrap();
            let (rect, start) = scene.drag_start_at(pointer).unwrap();
            start(
                Point {
                    x: pointer.x - rect.x,
                    y: pointer.y - rect.y,
                },
                rect,
            );
            let offset = state.inspector_scroll.offset();
            state
                .tooltip_controller((
                    "Soften the edge as a percentage of the smaller mask dimension.",
                    0,
                ))
                .focus(true);
            for preview in [
                PreviewState::Loading,
                PreviewState::Ready,
                PreviewState::Failed("Decode failed".into()),
                PreviewState::Ready,
            ] {
                state.program.preview.set(preview);
                let mut painter = TestPainter::default();
                let scene = renderer.render(build(&state, viewport), viewport, &mut painter);
                let (rect, drag) = scene.draggable_at(index).unwrap();
                drag(
                    Point {
                        x: pointer.x - rect.x,
                        y: pointer.y - rect.y,
                    },
                    rect,
                );
                assert_eq!(state.inspector_scroll.offset(), offset);
                assert_eq!(state.mask_drag.peek().unwrap().2, 4);
            }
        });
    }

    #[test]
    fn mask_slider_drag_survives_rebuilt_controls_and_undoes_once() {
        let state = UiState::new();
        assert!(state.edit(Command::ImportMedia(Media {
            id: 1,
            path: "/mask.png".into(),
            kind: MediaKind::Image,
            duration_us: None,
            has_audio: false
        })));
        let clip: Clip = serde_json::from_value(serde_json::json!({
            "id": 1, "start_frame": 0, "source_start_frame": 0,
            "duration_frames": 30, "content": {"media": {"media_id": 1}},
            "volume": 1.0, "muted": false
        }))
        .unwrap();
        assert!(state.edit(Command::InsertClip {
            track_id: 0,
            clip: Box::new(clip),
        }));
        state.select_clip(Some((0, 1)));
        set_clip_mask(&state, Some(Mask::default()));
        let slider = MaskSlider {
            state: state.clone(),
            field: 2,
            value: 0.6,
            width: 112.0,
        };
        let rect = Rect {
            x: 50.0,
            y: 0.0,
            width: 112.0,
            height: 24.0,
        };
        slider.on_drag_start().unwrap()(Point { x: 66.0, y: 12.0 }, rect);
        slider.on_drag().unwrap()(Point { x: 76.0, y: 12.0 }, rect);
        let rebuilt = MaskSlider {
            state: state.clone(),
            field: 3,
            value: 0.6,
            width: 112.0,
        };
        rebuilt.on_drag().unwrap()(Point { x: 86.0, y: 12.0 }, rect);
        assert_eq!(selected_clip(&state).unwrap().1.mask.unwrap().width, 0.6);
        slider.on_drag_end().unwrap()();
        let mask = selected_clip(&state).unwrap().1.mask.unwrap();
        assert_eq!(mask.width, 0.8);
        assert_eq!(mask.height, 0.6);
        assert!(state.editor.borrow_mut().undo());
        assert_eq!(
            selected_clip(&state).unwrap().1.mask.as_deref().copied(),
            Some(Mask::default())
        );
        let revision = state.revision.peek();
        slider.on_drag_start().unwrap()(Point { x: 66.0, y: 12.0 }, rect);
        slider.on_drag().unwrap()(Point { x: 86.0, y: 12.0 }, rect);
        state.run(Action::Escape);
        slider.on_drag().unwrap()(Point { x: 96.0, y: 12.0 }, rect);
        slider.on_drag_end().unwrap()();
        assert!(state.mask_drag.peek().is_none());
        assert_eq!(state.revision.peek(), revision);
        assert_eq!(
            selected_clip(&state).unwrap().1.mask.as_deref().copied(),
            Some(Mask::default())
        );
        slider.on_drag_start().unwrap()(Point { x: 66.0, y: 12.0 }, rect);
        state.select_clip(None);
        slider.on_drag_end().unwrap()();
        assert_eq!(state.revision.peek(), revision);
        set_clip_mask(&state, None);
        assert!(state.mask_drag.get().is_none());
    }

    #[test]
    fn preview_title_drag_moves_and_resizes_with_one_undoable_edit() {
        let state = UiState::new();
        assert!(state.add_title());
        let title = match &state.editor.borrow().project().tracks()[0].clips[0].content {
            ClipContent::Title(title) => title.clone(),
            _ => unreachable!(),
        };
        let canvas = ProgramCanvas {
            slot: GpuFrameSlot::new(),
            picture: false,
            picture_unavailable: false,
            titles: vec![((0, 1), title.clone())],
            selected: Some((0, 1)),
            state: state.clone(),
            aspect: 16.0 / 9.0,
            resolution: (1920, 1080),
        };
        let rect = Rect {
            x: 0.0,
            y: 0.0,
            width: 640.0,
            height: 360.0,
        };
        let mut painter = TestPainter::default();
        creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(creamui::theme::ThemeProvider::new(
                creamui::Theme::dark(),
            ));
            canvas.paint(&mut painter, rect);
        });
        assert!(
            painter
                .strokes
                .iter()
                .any(|(drawn, _)| *drawn == canvas.selected_title_box(rect).unwrap())
        );
        let program_x = |painter: &TestPainter| {
            painter
                .bounds
                .iter()
                .find(|(label, rect)| {
                    label == "Title 1"
                        && (700.0..900.0).contains(&rect.x)
                        && (150.0..400.0).contains(&rect.y)
                })
                .unwrap()
                .1
                .x
        };
        let before = program_x(&render(&state, 1280.0, 760.0));
        let at = |x, y| Point { x, y };
        let position = at(320.0, 180.0);
        canvas.on_drag_start().unwrap()(position, rect);
        canvas.on_drag().unwrap()(at(384.0, 216.0), rect);
        assert_eq!(state.title_preview.peek().as_ref().unwrap().1.x, 0.6);
        assert!(program_x(&render(&state, 1280.0, 760.0)) > before);
        assert_eq!(title.x, 0.5);
        canvas.on_drag_end().unwrap()();
        let current = || match &state.editor.borrow().project().tracks()[0].clips[0].content {
            ClipContent::Title(title) => title.clone(),
            _ => unreachable!(),
        };
        assert_eq!((current().x, current().y), (0.6, 0.6));
        state.undo();
        assert_eq!((current().x, current().y), (0.5, 0.5));
        state.redo();
        assert_eq!((current().x, current().y), (0.6, 0.6));

        let resized = current();
        let canvas = ProgramCanvas {
            titles: vec![((0, 1), resized.clone())],
            ..canvas
        };
        let box_rect = canvas.selected_title_box(rect).unwrap();
        let handle = at(box_rect.x + box_rect.width, box_rect.y + box_rect.height);
        canvas.on_drag_start().unwrap()(handle, rect);
        canvas.on_drag().unwrap()(at(handle.x + 32.0, handle.y), rect);
        canvas.on_drag_end().unwrap()();
        assert!(current().font_size > resized.font_size);
        state.undo();
        assert_eq!(current().font_size, resized.font_size);
        assert_eq!(state.title_size.peek(), resized.font_size.to_string());
        canvas.on_drag_start().unwrap()(handle, rect);
        canvas.on_drag().unwrap()(at(handle.x + 32.0, handle.y), rect);
        state.run(Action::Escape);
        canvas.on_drag_end().unwrap()();
        assert!(state.title_preview.peek().is_none());
        assert_eq!(state.selected.peek(), vec![(0, 1)]);
        assert_eq!(current().font_size, resized.font_size);
        canvas.on_drag_start().unwrap()(handle, rect);
        canvas.on_drag().unwrap()(at(handle.x, handle.y + 18.0), rect);
        canvas.on_drag_end().unwrap()();
        assert!(current().font_size > resized.font_size);
    }

    #[test]
    fn preview_title_selection_follows_text_at_frame_edges() {
        let state = UiState::new();
        let frame = Rect {
            x: 40.0,
            y: 100.0,
            width: 640.0,
            height: 360.0,
        };
        let canvas_rect = Rect {
            y: 40.0,
            height: 480.0,
            ..frame
        };
        for (x, y) in [(0.0, 0.5), (1.0, 0.5), (0.5, 0.0), (0.5, 1.0), (1.0, 1.0)] {
            let canvas = ProgramCanvas {
                slot: GpuFrameSlot::new(),
                picture: false,
                picture_unavailable: false,
                titles: vec![(
                    (0, 1),
                    Title {
                        text: "Title at frame edge".into(),
                        font_size: 192.0,
                        x,
                        y,
                        ..Title::default()
                    },
                )],
                selected: Some((0, 1)),
                state: state.clone(),
                aspect: 16.0 / 9.0,
                resolution: (1920, 1080),
            };
            let mut painter = TestPainter::default();
            creamui::reactive::with_context_scope(|| {
                creamui::reactive::provide_context(creamui::theme::ThemeProvider::new(
                    creamui::Theme::dark(),
                ));
                canvas.paint(&mut painter, canvas_rect);
            });
            let (_, text) = painter.bounds[0];
            let (selection, _) = painter.strokes[0];
            assert_eq!(
                selection,
                Rect {
                    x: text.x - 8.0,
                    y: text.y - 5.0,
                    width: text.width + 16.0,
                    height: text.height + 10.0,
                }
            );

            let drag = canvas.on_drag_start().unwrap();
            let drag_at = |x, y| {
                drag(
                    Point {
                        x: x - canvas_rect.x,
                        y: y - canvas_rect.y,
                    },
                    canvas_rect,
                )
            };
            let visible = text.intersect(frame).unwrap();
            drag_at(
                visible.x + visible.width / 2.0,
                visible.y + visible.height / 2.0,
            );
            assert!(!state.title_gesture.borrow_mut().take().unwrap().resize);

            let (handle, _) = painter.fills[0];
            assert_eq!(handle.intersect(frame), Some(handle));
            drag_at(
                handle.x + handle.width / 2.0,
                handle.y + handle.height / 2.0,
            );
            assert!(state.title_gesture.borrow_mut().take().unwrap().resize);

            if x == 0.0 {
                drag_at(text.x + text.width + 24.0, text.y + text.height / 2.0);
                assert!(state.title_gesture.borrow().is_none());
            }
            if y == 0.0 {
                drag_at(text.x + text.width / 2.0, frame.y - 8.0);
                assert!(state.title_gesture.borrow().is_none());
            }
        }
    }

    #[test]
    fn applying_title_template_is_one_undoable_edit() {
        let state = UiState::new();
        assert!(state.add_title());
        change_title(&state, |title| {
            title.text = "<b>Hello</b> **world**".into();
            title.parse_html = true;
            title.parse_markdown = true;
        });
        let original = selected_clip(&state).unwrap().1.content;
        for (_, animation) in crate::timeline::TITLE_TEMPLATES {
            change_title(&state, |title| title.apply_template(animation));
            let applied = selected_clip(&state).unwrap().1.content;
            let ClipContent::Title(title) = &applied else {
                panic!()
            };
            assert_eq!(title.animation, animation);
            assert_eq!(title.text, "<b>Hello</b> **world**");
            assert!(title.parse_html && title.parse_markdown);
            state.undo();
            assert_eq!(selected_clip(&state).unwrap().1.content, original);
            state.redo();
            assert_eq!(selected_clip(&state).unwrap().1.content, applied);
            state.undo();
        }
    }

    #[test]
    fn inspector_edits_styled_title_and_program_renders_parsed_runs() {
        let state = UiState::new();
        assert!(state.add_title());
        state
            .title_text
            .set_value("<b>Hello</b> **world** <color=red>red</color>");
        set_title_text(&state);
        state.title_font.set_value("Sans");
        state.title_size.set_value("92");
        state.title_color.set_value("#55AAFFFF");
        apply_title_style(&state);
        change_title(&state, |title| {
            title.bold = false;
            title.italic = true;
            title.outline = true;
            title.parse_html = true;
            title.parse_markdown = true;
        });
        let painter = render(&state, 1280.0, 1600.0);
        for label in [
            "Font family",
            "Font size (px)",
            "Color (#RRGGBB or #RRGGBBAA)",
            "Bold",
            "Italic",
            "Outline",
            "Parse HTML tags",
            "Parse Markdown",
        ] {
            assert!(painter.labels.iter().any(|value| value == label), "{label}");
        }
        assert!(
            painter
                .text_colors
                .iter()
                .any(|(text, color)| text == "red" && *color == Color::rgb(255, 0, 0))
        );
        assert!(painter.labels.iter().any(|label| label == "Hello"));
        assert!(painter.labels.iter().any(|label| label == "world"));
        state.title_picker.set_open(true);
        let popup = render(&state, 1280.0, 1600.0);
        assert!(popup.labels.iter().any(|label| label == "Done"));
        state.title_picker.set_open(false);
        state.undo();
        assert!(!state
            .editor
            .borrow()
            .project()
            .tracks()[0]
            .clips
            .iter()
            .any(|clip| matches!(&clip.content, ClipContent::Title(title) if title.parse_markdown)));
        assert_eq!(state.title_size.peek(), "92");
    }

    #[test]
    fn title_inputs_submit_one_undoable_edit_and_reject_invalid_values() {
        creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(creamui::theme::ThemeProvider::new(
                creamui::Theme::dark(),
            ));
            let state = UiState::new();
            assert!(state.add_title());
            for (controller, value, invalid, apply) in [
                (
                    &state.title_duration,
                    "5",
                    "",
                    set_title_duration as fn(&UiState),
                ),
                (
                    &state.title_font,
                    "Serif",
                    "",
                    apply_title_style as fn(&UiState),
                ),
                (
                    &state.title_size,
                    "180",
                    "-1",
                    apply_title_style as fn(&UiState),
                ),
                (
                    &state.title_color,
                    "#FF0000",
                    "invalid",
                    apply_title_style as fn(&UiState),
                ),
            ] {
                let before = selected_clip(&state).unwrap().1;
                controller.set_value(value);
                let field = title_input(controller, 180.0, &state, apply);
                let submit = field.on_key().unwrap();
                let enter = creamui::core::KeyInput {
                    key: creamui::core::Key::Enter,
                    modifiers: Default::default(),
                };
                submit(enter);
                assert_ne!(selected_clip(&state).unwrap().1, before);
                controller.set_value(invalid);
                submit(enter);
                state.undo();
                assert_eq!(selected_clip(&state).unwrap().1, before);
            }
            state.undo();
            assert!(selected_clip(&state).is_none());
            state.title_size.set_value("180");
            let field = title_input(&state.title_size, 180.0, &state, apply_title_style);
            field.on_key().unwrap()(creamui::core::KeyInput {
                key: creamui::core::Key::Enter,
                modifiers: Default::default(),
            });
            assert!(selected_clip(&state).is_none());
        });
    }

    #[test]
    fn title_duration_follows_length_fields() {
        let state = UiState::new();
        assert!(state.add_title());
        adjust_timing(&state, false, true, true);
        assert_eq!(selected_clip(&state).unwrap().1.duration_frames, 120);
        assert_eq!(state.title_duration.peek(), "4");
        set_title_duration(&state);
        assert_eq!(selected_clip(&state).unwrap().1.duration_frames, 120);
        state.undo();
        assert_eq!(state.title_duration.peek(), "3");
        state.redo();
        assert_eq!(state.title_duration.peek(), "4");
        adjust_timing(&state, false, false, true);
        assert_eq!(state.title_duration.peek(), (121.0_f64 / 30.0).to_string());
        adjust_timing(&state, false, false, false);
        assert_eq!(state.title_duration.peek(), "4");
        adjust_timing(&state, false, true, false);
        assert_eq!(state.title_duration.peek(), "3");
    }

    #[test]
    fn title_duration_preserves_user_draft_during_external_edits() {
        let state = UiState::new();
        assert!(state.add_title());
        for draft in ["5.5", "", "invalid"] {
            state.title_duration.set_value(draft);
            adjust_timing(&state, false, true, true);
            assert_eq!(state.title_duration.peek(), draft);
            state.undo();
            assert_eq!(state.title_duration.peek(), draft);
            state.redo();
            assert_eq!(state.title_duration.peek(), draft);
        }
    }

    #[test]
    fn title_duration_is_editable_in_seconds_and_undoable() {
        let state = UiState::new();
        assert!(state.add_title());
        assert_eq!(state.title_duration.peek(), "3");
        let painter = render(&state, 1280.0, 1600.0);
        assert!(
            painter
                .labels
                .iter()
                .any(|label| label == "Duration (seconds)")
        );
        state.title_duration.set_value("5.5");
        set_title_duration(&state);
        assert_eq!(selected_clip(&state).unwrap().1.duration_frames, 165);
        state.title_duration.set_value("-1");
        set_title_duration(&state);
        assert_eq!(selected_clip(&state).unwrap().1.duration_frames, 165);
        state.undo();
        assert_eq!(selected_clip(&state).unwrap().1.duration_frames, 90);
        assert_eq!(state.title_duration.peek(), "-1");
    }

    #[test]
    fn invalid_color_text_cannot_crash_inspector_or_change_title() {
        let state = UiState::new();
        assert!(state.add_title());
        state.title_color.set_value("#aé12345");
        let painter = render(&state, 1280.0, 900.0);
        assert!(painter.labels.iter().any(|label| label == "Title clip"));
        apply_title_style(&state);
        assert!(state.status.peek().contains("color"));
        let editor = state.editor.borrow();
        let title = &editor.project().tracks()[0].clips[0].content;
        assert!(matches!(title, ClipContent::Title(title) if title.color == [255; 4]));
    }

    #[test]
    fn settings_show_creamui_presets_and_custom_theme_changes_palette() {
        let state = UiState::new();
        state.set_theme_mode(ThemeMode::Dark);
        state.set_accent("orange");
        assert_eq!(
            state.settings.peek().theme().colors.accent,
            creamui::Color::rgb(224, 165, 46)
        );
        state.open_settings();
        assert!(state.settings_modal.peek());
        creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(creamui::ThemeProvider::new(creamui::Theme::dark()));
            let mut painter = TestPainter::default();
            let viewport = Size {
                width: 1280.0,
                height: 760.0,
            };
            let scene = Renderer::new().render(build(&state, viewport), viewport, &mut painter);
            scene.hit_test(Point { x: 650.0, y: 215.0 }).unwrap()();
            assert!(state.settings_modal.peek());
            scene.hit_test(Point { x: 8.0, y: 8.0 }).unwrap()();
            assert!(!state.settings_modal.peek());
        });
        state.open_settings();
        let painter = render(&state, 1280.0, 760.0);
        assert!(
            painter
                .fills
                .iter()
                .any(|(rect, _)| rect.width >= 640.0 && rect.height >= 460.0)
        );
        for label in [
            "Settings",
            "Appearance",
            "Accent color",
            "System",
            "Midnight",
        ] {
            assert!(painter.labels.iter().any(|item| item == label), "{label}");
        }
        assert!(
            !painter
                .labels
                .iter()
                .any(|item| item == "Changes are saved automatically")
        );
        creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(creamui::ThemeProvider::new(creamui::Theme::dark()));
            let viewport = Size {
                width: 1280.0,
                height: 760.0,
            };
            let mut painter = TestPainter::default();
            let scene = Renderer::new().render(build(&state, viewport), viewport, &mut painter);
            let light = painter
                .bounds
                .iter()
                .rev()
                .find(|(label, rect)| label == "Light" && rect.x > 360.0)
                .unwrap()
                .1;
            scene
                .hit_test(Point {
                    x: light.x - 14.0,
                    y: light.y + light.height / 2.0,
                })
                .expect("theme row must be clickable beyond its text")();
            assert!(state.settings.peek().theme == ThemeMode::Light);
            assert!(state.settings_modal.peek());
        });
        state.run(Action::AddTitle);
        assert!(state.editor.borrow().project().tracks()[0].clips.is_empty());
        let painter = render(&state, 1280.0, 760.0);
        for preset in creamui::ThemeDefinition::builtin_default().accent_presets {
            assert!(painter.labels.contains(&preset.name));
        }
        creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(creamui::ThemeProvider::new(creamui::Theme::dark()));
            let viewport = Size {
                width: 1280.0,
                height: 760.0,
            };
            let mut painter = TestPainter::default();
            let scene = Renderer::new().render(build(&state, viewport), viewport, &mut painter);
            let purple = painter
                .bounds
                .iter()
                .rev()
                .find(|(label, rect)| label == "Purple" && rect.x > 360.0)
                .unwrap()
                .1;
            scene
                .hit_test(Point {
                    x: purple.x - 16.0,
                    y: purple.y + purple.height / 2.0,
                })
                .expect("accent row must be clickable beyond its text")();
            assert_eq!(state.settings.peek().accent.as_deref(), Some("purple"));
            assert!(state.settings_modal.peek());
        });
        let small = render(&state, 420.0, 220.0);
        assert!(small.labels.iter().any(|label| label == "Settings"));
        assert!(state.settings_scroll.max_offset() > 0.0);
        state
            .settings_scroll
            .set(state.settings_scroll.max_offset());
        let painter = render(&state, 420.0, 220.0);
        assert!(
            creamui::ThemeDefinition::builtin_default()
                .accent_presets
                .iter()
                .any(|preset| painter.labels.contains(&preset.name))
        );
        state.run(Action::Escape);
        assert!(!state.settings_modal.peek());
        state.open_settings();
        assert_eq!(state.settings_scroll.offset(), 0.0);
        state.close_settings();
        state.set_theme_mode(ThemeMode::System);
        assert!(state.settings.peek().accent.is_none());
    }

    #[test]
    fn settings_about_shows_build_metadata_and_clickable_project_links() {
        let state = UiState::new();
        state.open_settings();
        let viewport = Size {
            width: 1280.0,
            height: 760.0,
        };
        creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(creamui::ThemeProvider::new(creamui::Theme::dark()));
            let mut painter = TestPainter::default();
            let scene = Renderer::new().render(build(&state, viewport), viewport, &mut painter);
            let about = painter
                .bounds
                .iter()
                .find(|(label, _)| label == "About")
                .unwrap()
                .1;
            scene
                .hit_test(Point {
                    x: about.x + 2.0,
                    y: about.y + about.height / 2.0,
                })
                .expect("About navigation must be clickable")();
            assert!(state.settings_modal.peek());
            assert!(state.settings_page.peek() == SettingsPage::About);
        });
        let painter = render(&state, 1280.0, 760.0);
        for label in [
            "Ocus",
            "Version",
            env!("CARGO_PKG_VERSION"),
            "Commit",
            env!("OCUS_COMMIT"),
            "Author",
            env!("CARGO_PKG_AUTHORS"),
            "Source code",
            "Report an issue",
            "Releases",
        ] {
            assert!(painter.labels.iter().any(|item| item == label), "{label}");
        }
        assert!(painter.text_colors.iter().any(|(label, color)| {
            label == "Source code" && *color == Color::rgb(91, 173, 255)
        }));
        creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(
                creamui::ThemeProvider::new(creamui::Theme::light()),
            );
            let mut painter = TestPainter::default();
            let scene = Renderer::new().render(build(&state, viewport), viewport, &mut painter);
            assert!(painter.text_colors.iter().any(|(label, color)| {
                label == "Source code" && *color == Color::rgb(16, 94, 190)
            }));
            let appearance = painter
                .bounds
                .iter()
                .find(|(label, rect)| label == "Appearance" && rect.x < 450.0)
                .unwrap()
                .1;
            scene
                .hit_test(Point {
                    x: appearance.x + 2.0,
                    y: appearance.y + appearance.height / 2.0,
                })
                .expect("Appearance navigation must be clickable")();
            assert!(state.settings_page.peek() == SettingsPage::Appearance);
        });
        state.settings_page.set(SettingsPage::About);
        let small = render(&state, 420.0, 260.0);
        assert!(small.labels.iter().any(|label| label == "Ocus"));
        assert!(state.settings_scroll.max_offset() > 0.0);
        state
            .settings_scroll
            .set(state.settings_scroll.max_offset());
        let small = render(&state, 420.0, 260.0);
        assert!(small.labels.iter().any(|label| label == "Releases"));
        state.open_settings();
        assert!(state.settings_page.peek() == SettingsPage::Appearance);
        assert_eq!(state.settings_scroll.offset(), 0.0);
    }

    #[test]
    fn proxy_preference_is_only_in_performance_and_remains_clickable() {
        let state = UiState::new();
        state.open_settings();
        let appearance = render(&state, 1280.0, 760.0);
        assert!(
            !appearance
                .labels
                .iter()
                .any(|label| label == "Use proxies for preview")
        );
        state.settings_page.set(SettingsPage::Performance);
        let enabled = state.settings.peek().use_proxies;
        creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(creamui::ThemeProvider::new(creamui::Theme::dark()));
            let viewport = Size {
                width: 1280.0,
                height: 760.0,
            };
            let mut painter = TestPainter::default();
            let scene = Renderer::new().render(build(&state, viewport), viewport, &mut painter);
            assert!(
                painter
                    .labels
                    .iter()
                    .any(|label| label == "Hardware acceleration")
            );
            assert!(
                painter
                    .labels
                    .iter()
                    .any(|label| label == "Edit 4K footage smoothly with lightweight copies.")
            );
            assert!(
                painter
                    .labels
                    .iter()
                    .any(|label| label == "Export always uses the originals.")
            );
            let label = painter
                .bounds
                .iter()
                .find(|(label, _)| label == "Use proxies for preview")
                .unwrap()
                .1;
            scene
                .hit_test(Point {
                    x: label.x - 16.0,
                    y: label.y + label.height / 2.0,
                })
                .expect("proxy checkbox must be clickable")();
        });
        assert_eq!(state.settings.peek().use_proxies, !enabled);
        state.toggle_proxies();
        assert_eq!(state.settings.peek().use_proxies, enabled);
    }

    #[test]
    fn performance_settings_toggle_hardware_acceleration() {
        let state = UiState::new();
        state.open_settings();
        let viewport = Size {
            width: 1280.0,
            height: 760.0,
        };
        creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(creamui::ThemeProvider::new(creamui::Theme::dark()));
            let mut painter = TestPainter::default();
            let scene = Renderer::new().render(build(&state, viewport), viewport, &mut painter);
            let performance = painter
                .bounds
                .iter()
                .find(|(label, _)| label == "Performance")
                .unwrap()
                .1;
            scene
                .hit_test(Point {
                    x: performance.x + 2.0,
                    y: performance.y + performance.height / 2.0,
                })
                .expect("Performance navigation must be clickable")();
            assert!(state.settings_page.peek() == SettingsPage::Performance);
        });
        let enabled = state.settings.peek().hardware_acceleration;
        creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(creamui::ThemeProvider::new(creamui::Theme::dark()));
            let mut painter = TestPainter::default();
            let scene = Renderer::new().render(build(&state, viewport), viewport, &mut painter);
            let backends = crate::media::hardware::Backend::PLATFORM
                .iter()
                .map(|backend| backend.name())
                .collect::<Vec<_>>()
                .join(" or ");
            assert!(painter.labels.iter().any(|label| label.contains(&backends)));
            let label = painter
                .bounds
                .iter()
                .find(|(label, _)| label == "Hardware acceleration")
                .unwrap()
                .1;
            scene
                .hit_test(Point {
                    x: label.x - 16.0,
                    y: label.y + label.height / 2.0,
                })
                .expect("hardware acceleration checkbox must be clickable")();
        });
        assert_eq!(state.settings.peek().hardware_acceleration, !enabled);
        assert_eq!(crate::media::hardware::enabled(), !enabled);
        state.toggle_hardware_acceleration();
        assert_eq!(state.settings.peek().hardware_acceleration, enabled);
        assert_eq!(crate::media::hardware::enabled(), enabled);
    }

    #[test]
    fn editor_palette_tracks_the_window_theme() {
        for mode in [ThemeMode::Light, ThemeMode::Dark, ThemeMode::Midnight] {
            creamui::reactive::with_context_scope(|| {
                let settings = crate::settings::Settings {
                    theme: mode,
                    accent: None,
                    ..Default::default()
                };
                let theme = settings.theme();
                creamui::reactive::provide_context(creamui::ThemeProvider::new(theme));
                assert_eq!(
                    background(),
                    Color {
                        a: 255,
                        ..theme.colors.surface
                    }
                );
                assert_eq!(accent(), creamui::Color::rgb(224, 165, 46));
                assert_eq!(text_color(), theme.colors.text_primary);
            });
        }
    }

    fn x_of(painter: &TestPainter, label: &str) -> Option<f32> {
        painter
            .bounds
            .iter()
            .find(|(text, _)| text == label)
            .map(|(_, rect)| rect.x)
    }

    #[test]
    fn media_bin_switches_views_without_pushing_the_timeline_out_of_the_window() {
        let state = UiState::new();
        state.timeline_expanded.set(true);
        assert!(state.edit(Command::ImportMedia(Media {
            id: 1,
            path: "/tmp/footage.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(2_000_000),
            has_audio: false,
        })));
        for view in [MediaView::Grid, MediaView::List] {
            state.media_view.set(view);
            let painter = render(&state, 1280.0, 760.0);
            let last_track = painter
                .bounds
                .iter()
                .find(|(label, _)| label == "A2")
                .unwrap()
                .1;
            assert!(last_track.y + last_track.height < 760.0 - 20.0);
            assert!(painter.labels.iter().any(|text| text == "footage.mp4"));
        }
        assert!(
            render(&state, 1280.0, 760.0)
                .labels
                .iter()
                .any(|text| text == "Video · 0:02")
        );
        state.media_query.set_value("missing");
        let painter = render(&state, 1280.0, 760.0);
        assert!(
            painter
                .labels
                .iter()
                .any(|text| text == "No matching media")
        );
    }

    fn view(state: &UiState) -> TimelineView {
        let editor = state.editor.borrow();
        TimelineView {
            lanes: timeline_lanes(state, editor.project(), (30, 1)),
            start: 0,
            span: 300,
            end: 300,
            fps: (30, 1),
            playhead: 0,
            markers: editor
                .project()
                .markers()
                .iter()
                .map(|marker| marker.frame)
                .collect(),
            selected: state.selected.peek(),
            ghosts: Vec::new(),
            dragging: false,
            marquee: None,
            scroll: 0.0,
            state: state.clone(),
        }
    }

    #[test]
    fn locked_lanes_refuse_pointer_and_marquee_selection() {
        let state = UiState::new();
        state.timeline_rect.set(TIMELINE);
        assert!(state.add_title());
        assert!(state.edit(Command::SetTrackLocked {
            track_id: 0,
            locked: true
        }));
        assert!(state.selected.peek().is_empty());
        let timeline = view(&state);
        press(&timeline, at(30.0), 50.0);
        drag_to(&timeline, at(45.0), 50.0);
        release(&timeline);
        assert!(state.selected.peek().is_empty());
        state.select_all();
        assert!(state.selected.peek().is_empty());
        assert!(state.editor.borrow().project().tracks()[0].locked);
        assert_eq!(
            state.editor.borrow().project().tracks()[0].clips[0].duration_frames,
            90
        );
    }

    #[test]
    fn header_double_click_uses_local_coordinates_to_open_rename() {
        let state = UiState::new();
        state.timeline_rect.set(Rect {
            x: 200.0,
            y: 400.0,
            ..TIMELINE
        });
        let timeline = view(&state);
        let double_click = timeline.on_double_click_at().unwrap();
        double_click(Point { x: 110.0, y: 45.0 });
        assert!(state.rename_track.peek().is_none());
        double_click(Point { x: 20.0, y: 45.0 });
        assert_eq!(state.rename_track.peek(), Some(0));
        state.track_name.set_value("x".repeat(33));
        state.apply_track_rename();
        assert_eq!(state.rename_track.peek(), Some(0));
        assert!(state.editor.borrow().project().tracks()[0].name.is_empty());
        state.track_name.set_value(" Picture ");
        state.apply_track_rename();
        assert!(state.rename_track.peek().is_none());
        assert_eq!(state.editor.borrow().project().tracks()[0].name, "Picture");
        state.undo();
        assert!(state.editor.borrow().project().tracks()[0].name.is_empty());
    }

    #[test]
    fn track_headers_ellipsize_names_and_keep_controls_in_bounds() {
        let state = UiState::new();
        assert!(state.edit(Command::RenameTrack {
            track_id: 0,
            name: "A very long but valid track name".into()
        }));
        for width in [680.0, 1000.0, 1280.0] {
            let painter = render(&state, width, 760.0);
            let (label, name) = painter
                .bounds
                .iter()
                .find(|(label, _)| label.starts_with("A very"))
                .unwrap();
            assert!(label.ends_with('…'));
            assert!(run_width(label, 12.0, "system-ui", true) <= name.width);
            assert!(name.width <= super::super::timeline_view::HEADER_WIDTH - 100.0);
            assert!(painter.labels.iter().any(|label| label == "BASE"));
            creamui::reactive::with_context_scope(|| {
                creamui::reactive::provide_context(creamui::theme::ThemeProvider::new(
                    creamui::Theme::dark(),
                ));
                let mut timeline = view(&state);
                let children = timeline.children();
                assert_eq!(children.len(), 4);
                for child in children {
                    let layout = child.style().layout;
                    assert_eq!(layout.size.width, Dimension::Length(50.0));
                    let creamui::core::layout::LengthPercentageAuto::Length(x) = layout.inset.left
                    else {
                        panic!("header controls need an absolute x position");
                    };
                    assert!(x >= name.x);
                    assert!(x + 50.0 <= super::super::timeline_view::HEADER_WIDTH - 30.0);
                }
            });
        }
        assert!(state.edit(Command::RenameTrack {
            track_id: 0,
            name: "W".repeat(32)
        }));
        let painter = render(&state, 680.0, 760.0);
        let (label, name) = painter
            .bounds
            .iter()
            .find(|(label, _)| label.starts_with("WW"))
            .unwrap();
        assert!(label.ends_with('…'));
        assert!(run_width(label, 12.0, "system-ui", true) <= name.width);
    }

    const TIMELINE: Rect = Rect {
        x: 0.0,
        y: 0.0,
        width: 1184.0,
        height: 240.0,
    };

    /// The timeline x for `frame` with a 1000 px wide, 300 frame view.
    fn at(frame: f32) -> f32 {
        super::super::timeline_view::HEADER_WIDTH + frame * 1000.0 / 300.0
    }

    fn press(view: &TimelineView, x: f32, y: f32) {
        view.on_drag_start().unwrap()(Point { x, y }, TIMELINE);
        view.on_drag().unwrap()(Point { x, y }, TIMELINE);
    }

    fn drag_to(view: &TimelineView, x: f32, y: f32) {
        view.on_drag().unwrap()(Point { x, y }, TIMELINE);
    }

    fn release(view: &TimelineView) {
        view.on_drag_end().unwrap()();
    }

    #[test]
    fn clicking_a_clip_edge_without_trimming_preserves_status_and_history() {
        for subtitles in [false, true] {
            let state = UiState::new();
            assert!(state.add_title());
            if subtitles {
                let mut tracks = state.editor.borrow().project().tracks().to_vec();
                tracks[0].subtitles = true;
                assert!(state.edit(Command::ReplaceTracks(tracks)));
            }
            let before = state.editor.borrow().project().clone();
            let status = state.status.peek();
            let timeline = view(&state);
            press(&timeline, at(0.0), 50.0);
            release(&timeline);
            assert_eq!(state.status.peek(), status);
            assert_eq!(state.editor.borrow().project(), &before);
            assert!(state.trim_draft.peek().is_none());
            state.undo();
            assert_ne!(state.editor.borrow().project(), &before);
        }
    }

    #[test]
    fn trim_drag_previews_then_commits_once_and_escape_discards() {
        let state = UiState::new();
        state.run(Action::AddTitle);
        let original = selected_clip(&state).unwrap().1;
        let timeline = view(&state);
        press(&timeline, at(1.0), 50.0);
        assert!(matches!(
            *state.gesture.borrow(),
            super::super::interaction::Gesture::Trim { left: true, .. }
        ));
        drag_to(&timeline, at(20.0), 50.0);
        drag_to(&timeline, at(30.0), 50.0);
        assert_eq!(selected_clip(&state).unwrap().1, original);
        assert!(state.trim_draft.peek().is_some());
        let preview = view(&state);
        assert_eq!(preview.lanes[0].clips[0].start, 30);
        assert_eq!(
            preview.lanes[0].clips[0].duration,
            original.duration_frames - 30
        );
        let mut painter = TestPainter::default();
        preview.paint(&mut painter, TIMELINE);
        assert!(painter.labels.iter().any(
            |label| label == &format!("{:.1}s", (original.duration_frames - 30) as f64 / 30.0)
        ));
        release(&timeline);
        let trimmed = selected_clip(&state).unwrap().1;
        assert_eq!(trimmed.start_frame, 30);
        assert_eq!(
            trimmed.start_frame + trimmed.duration_frames,
            original.duration_frames
        );
        state.run(Action::Undo);
        assert_eq!(selected_clip(&state).unwrap().1, original);
        press(&timeline, at(1.0), 50.0);
        drag_to(&timeline, at(40.0), 50.0);
        state.run(Action::Escape);
        drag_to(&timeline, at(50.0), 50.0);
        release(&timeline);
        assert_eq!(
            state.editor.borrow().project().tracks()[0].clips[0],
            original
        );
        assert_eq!(state.selected.peek(), vec![(0, original.id)]);
        assert!(state.trim_draft.peek().is_none());
        press(&timeline, at(30.0), 50.0);
        assert!(matches!(
            *state.gesture.borrow(),
            super::super::interaction::Gesture::Clip { .. }
        ));
    }

    #[test]
    fn inspector_length_fields_are_bounded_and_undoable() {
        let state = UiState::new();
        adjust_timing(&state, false, true, true);
        state.run(Action::AddTitle);
        let original = selected_clip(&state).unwrap().1.duration_frames;
        adjust_timing(&state, false, true, true);
        assert_eq!(
            selected_clip(&state).unwrap().1.duration_frames,
            original + 30
        );
        state.run(Action::Undo);
        assert_eq!(selected_clip(&state).unwrap().1.duration_frames, original);
        for _ in 0..10 {
            adjust_timing(&state, false, true, false);
        }
        assert_eq!(selected_clip(&state).unwrap().1.duration_frames, 1);
    }

    #[test]
    fn stabilization_inspector_fits_narrow_panels() {
        let state = UiState::new();
        assert!(state.edit(Command::ImportMedia(Media {
            id: 1,
            path: "shake.mkv".into(),
            kind: MediaKind::Video,
            duration_us: Some(2_000_000),
            has_audio: false
        })));
        let mut clip: Clip = serde_json::from_value(serde_json::json!({
            "id":1,"start_frame":0,"source_start_frame":0,"duration_frames":60,
            "content":{"media":{"media_id":1}},"volume":1.0,"muted":false
        }))
        .unwrap();
        clip.effect_stack.push(EffectKind::Stabilization);
        clip.stabilization = Some(crate::timeline::Stabilization::new(0, vec![[0.0, 0.0]; 60]));
        assert!(state.edit(Command::InsertClip {
            track_id: 0,
            clip: Box::new(clip)
        }));
        state.select_detail((0, 1), ClipDetail::Effect(EffectKind::Stabilization));
        for width in [210.0, 360.0] {
            let mut painter = TestPainter::default();
            creamui::reactive::with_context_scope(|| {
                creamui::reactive::provide_context(creamui::ThemeProvider::new(
                    creamui::Theme::dark(),
                ));
                Renderer::new().render(
                    inspector(&state, state.editor.borrow().project(), width, false),
                    Size {
                        width,
                        height: 4000.0,
                    },
                    &mut painter,
                );
            });
            for label in [
                "Stabilization",
                "Re-analyze",
                "Strength · 50%",
                "Remove",
                "Original video is untouched.",
            ] {
                let (_, rect) = painter
                    .bounds
                    .iter()
                    .find(|(text, _)| text == label)
                    .unwrap_or_else(|| panic!("Missing {label}"));
                assert!(
                    rect.x >= 0.0 && rect.x + rect.width <= width,
                    "{label}: {rect:?}"
                );
            }
        }
    }

    #[test]
    fn transform_inspector_is_media_only_and_fits() {
        let state = UiState::new();
        assert!(state.edit(Command::ImportMedia(Media {
            id: 1,
            path: "image.png".into(),
            kind: MediaKind::Image,
            duration_us: None,
            has_audio: false
        })));
        assert!(state.edit(Command::InsertClip {
            track_id: 0,
            clip: Box::new(Clip {
                effect_stack: Vec::new(),
                speed: 1.0,
                playback: crate::timeline::Playback::Forward,
                noise_reduction: false,
                effect: None,
                mask: None,
                lut: None,
                chroma_key: None,
                transition_out: None,
                id: 1,
                start_frame: 0,
                source_start_frame: 0,
                duration_frames: 30,
                content: ClipContent::Media { media_id: 1 },
                volume: 1.0,
                muted: false,
                transition: None,
                fade_in_frames: 0,
                fade_out_frames: 0,
                linked_audio: false,
                color: Default::default(),
                stabilization: None,
                keyframes: Vec::new(),
                transform: Default::default()
            })
        }));
        state.select_clip(Some((0, 1)));
        toggle_keyframe(&state);
        assert_eq!(selected_clip(&state).unwrap().1.keyframes.len(), 1);
        state.program.position.set(10);
        set_clip_transform(&state, true, |transform| transform.x = 0.5);
        let animated = selected_clip(&state).unwrap().1;
        assert_eq!(animated.keyframes.len(), 2);
        assert_eq!(animated.transform_at(5).x, 0.25);
        state.program.position.set(5);
        set_keyframe_easing(&state, crate::timeline::Easing::EASE_IN);
        assert_eq!(
            selected_clip(&state).unwrap().1.keyframes[0].easing,
            crate::timeline::Easing::EASE_IN
        );
        state.undo();
        assert_eq!(
            selected_clip(&state).unwrap().1.keyframes,
            animated.keyframes
        );
        state.program.position.set(10);
        creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(creamui::theme::ThemeProvider::new(
                creamui::Theme::dark(),
            ));
            state.program.position.set(5);
            let easing_help = [
                "Move at a steady speed to the next keyframe.",
                "Start slowly, then speed up toward the next keyframe.",
                "Start quickly, then slow down toward the next keyframe.",
                "Start slowly, speed up, then slow down at the next keyframe.",
                "Keep this value until the next keyframe.",
            ];
            let draw_easing = || {
                let mut painter = TestPainter::default();
                Renderer::new().render(
                    Box::new(append(
                        Flex::column(),
                        transform_rows(&state, animated.transform_at(5), 280.0),
                    )),
                    Size {
                        width: 300.0,
                        height: 900.0,
                    },
                    &mut painter,
                );
                painter
            };
            draw_easing();
            for help in easing_help {
                assert!(state.tooltips.borrow().contains_key(&(help, 0)));
                state.tooltip_controller((help, 0)).focus(true);
                let painter = draw_easing();
                assert_eq!(
                    painter
                        .labels
                        .iter()
                        .filter(|label| easing_help.contains(&label.as_str()))
                        .count(),
                    1
                );
                assert_eq!(
                    painter
                        .labels
                        .iter()
                        .filter(|label| label.as_str() == help)
                        .count(),
                    1
                );
            }
            state.tooltip_controller((easing_help[4], 0)).focus(false);
            state.program.position.set(10);
            let labels = Rc::new(std::cell::RefCell::new(Vec::new()));
            let renders = Rc::new(std::cell::Cell::new(0));
            let observed = labels.clone();
            let count = renders.clone();
            let reactive_state = state.clone();
            let _effect = creamui::reactive::create_effect(move || {
                reactive_state.begin_tooltips();
                let editor = reactive_state.editor.borrow();
                let widget = inspector(&reactive_state, editor.project(), 300.0, false);
                let mut painter = TestPainter::default();
                Renderer::new().render(
                    widget,
                    Size {
                        width: 300.0,
                        height: 1900.0,
                    },
                    &mut painter,
                );
                *observed.borrow_mut() = painter.labels;
                reactive_state.finish_tooltips();
                count.set(count.get() + 1);
            });
            assert!(
                state
                    .tooltips
                    .borrow()
                    .contains_key(&("Remove keyframe at playhead", 0))
            );
            let initial = renders.get();
            state.program.position.set(5);
            assert_eq!(renders.get(), initial + 1);
            assert!(labels.borrow().iter().any(|label| label == "25"));
            assert!(
                state
                    .tooltips
                    .borrow()
                    .contains_key(&("Add keyframe at playhead", 0))
            );
            assert!(
                !state
                    .tooltips
                    .borrow()
                    .contains_key(&("Remove keyframe at playhead", 0))
            );
            state.seek(10);
            assert!(labels.borrow().iter().any(|label| label == "50"));
            assert!(
                state
                    .tooltips
                    .borrow()
                    .contains_key(&("Remove keyframe at playhead", 0))
            );
            assert!(
                !state
                    .tooltips
                    .borrow()
                    .contains_key(&("Add keyframe at playhead", 0))
            );
            assert_eq!(
                keyframe_offset(
                    &selected_clip(&state).unwrap().1,
                    state.program.position.peek()
                ),
                Some(10)
            );
        });
        assert_eq!(neighbouring_keyframe(&animated, 5, false), Some(0));
        assert_eq!(neighbouring_keyframe(&animated, 5, true), Some(10));
        set_clip_transform(&state, true, |transform| transform.opacity = 0.5);
        assert_eq!(selected_clip(&state).unwrap().1.keyframes.len(), 2);
        set_clip_transform(&state, false, |transform| transform.crop_left = 0.1);
        assert_eq!(selected_clip(&state).unwrap().1.transform.crop_left, 0.1);
        state.program.position.set(30);
        let before = selected_clip(&state).unwrap().1;
        replace_clip_transform(&state, Transform::default());
        let reset = selected_clip(&state).unwrap().1;
        assert!(reset.keyframes.is_empty());
        assert!(reset.transform.is_identity());
        state.run(Action::Undo);
        assert_eq!(selected_clip(&state).unwrap().1, before);
        toggle_keyframe(&state);
        set_clip_transform(&state, true, |transform| transform.scale = 2.0);
        assert_eq!(selected_clip(&state).unwrap().1, before);
        assert_eq!(neighbouring_keyframe(&before, 30, false), None);
        state.program.position.set(10);
        toggle_keyframe(&state);
        assert_eq!(selected_clip(&state).unwrap().1.keyframes.len(), 1);
        state.program.position.set(0);
        toggle_keyframe(&state);
        assert!(selected_clip(&state).unwrap().1.keyframes.is_empty());
        set_clip_transform(&state, false, |transform| transform.crop_left = 0.0);
        for width in [220.0, 280.0, 340.0] {
            creamui::reactive::with_context_scope(|| {
                creamui::reactive::provide_context(creamui::theme::ThemeProvider::new(
                    creamui::Theme::dark(),
                ));
                let project = state.editor.borrow();
                let mut painter = TestPainter::default();
                Renderer::new().render(
                    inspector(&state, project.project(), width, false),
                    Size {
                        width,
                        height: 1600.0,
                    },
                    &mut painter,
                );
                assert!(painter.labels.iter().any(|l| l == "TRANSFORM"));
                for (label, rect) in &painter.bounds {
                    if [
                        "Scale",
                        "Position X",
                        "Position Y",
                        "Rotation",
                        "Crop left",
                        "Crop top",
                        "Crop right",
                        "Crop bottom",
                        "Opacity",
                    ]
                    .contains(&label.as_str())
                    {
                        assert!(
                            rect.x >= 0.0 && rect.x + rect.width <= width,
                            "{label}: {rect:?}"
                        );
                    }
                }
            });
        }
        set_clip_transform(&state, true, |transform| transform.scale = 0.5);
        assert_eq!(selected_clip(&state).unwrap().1.transform.scale, 0.5);
        state.run(Action::Undo);
        assert!(selected_clip(&state).unwrap().1.transform.is_identity());
        state.run(Action::AddTitle);
        assert!(
            !render(&state, 1280.0, 1000.0)
                .labels
                .iter()
                .any(|l| l == "TRANSFORM")
        );
        assert!(
            !render(&state, 1280.0, 1000.0)
                .labels
                .iter()
                .any(|l| l == "PICTURE-IN-PICTURE")
        );
        let title = selected_clip(&state).unwrap();
        assert!(!state.edit(Command::SetClipTransform {
            track_id: title.0,
            clip_id: title.1.id,
            transform: Transform {
                scale: 0.5,
                ..Default::default()
            }
        }));
        for (id, kind, track_id) in [(2, MediaKind::Video, 1), (3, MediaKind::Audio, 2)] {
            assert!(state.edit(Command::ImportMedia(Media {
                id,
                path: format!("source-{id}.mp4").into(),
                kind,
                duration_us: Some(1_000_000),
                has_audio: true
            })));
            assert!(state.edit(Command::InsertClip {
                track_id,
                clip: Box::new(Clip {
                    effect_stack: Vec::new(),
                    speed: 1.0,
                    playback: crate::timeline::Playback::Forward,
                    id: id + 100,
                    noise_reduction: false,
                    effect: None,
                    mask: None,
                    lut: None,
                    chroma_key: None,
                    transition_out: None,
                    start_frame: 400,
                    source_start_frame: 0,
                    duration_frames: 30,
                    content: ClipContent::Media { media_id: id },
                    volume: 1.0,
                    muted: false,
                    transition: None,
                    fade_in_frames: 0,
                    fade_out_frames: 0,
                    linked_audio: false,
                    color: Default::default(),
                    stabilization: None,
                    keyframes: Vec::new(),
                    transform: Default::default()
                })
            }));
            state.select_clip(Some((track_id, id + 100)));
            assert_eq!(
                render(&state, 1280.0, 1600.0)
                    .labels
                    .iter()
                    .any(|l| l == "TRANSFORM"),
                kind == MediaKind::Video
            );
            assert_eq!(
                render(&state, 1280.0, 2300.0)
                    .labels
                    .iter()
                    .any(|l| l == "PICTURE-IN-PICTURE"),
                kind == MediaKind::Video
            );
            if kind == MediaKind::Audio {
                assert!(!state.edit(Command::SetClipTransform {
                    track_id,
                    clip_id: id + 100,
                    transform: Transform {
                        scale: 0.5,
                        ..Default::default()
                    }
                }));
            }
        }
    }

    #[test]
    fn picture_in_picture_presets_compose_in_each_corner_and_are_undoable() {
        use crate::media::composite::{Compositor, VisualPlan};
        use std::sync::atomic::AtomicBool;

        let state = UiState::new();
        let fixture = tempfile::tempdir().unwrap();
        let previous = Transform {
            rotation: 90.0,
            crop_left: 0.25,
            opacity: 0.5,
            ..Transform::default()
        };
        for (track_id, rgb) in [(0, [255, 0, 0]), (1, [0, 255, 0])] {
            let id = track_id + 1;
            let path = fixture.path().join(format!("{id}.ppm"));
            let mut ppm = b"P6\n100 50\n255\n".to_vec();
            ppm.extend(rgb.repeat(100 * 50));
            std::fs::write(&path, ppm).unwrap();
            assert!(state.edit(Command::ImportMedia(Media {
                id,
                path,
                kind: MediaKind::Image,
                duration_us: None,
                has_audio: false,
            })));
            assert!(state.edit(Command::InsertClip {
                track_id,
                clip: Box::new(Clip {
                    stabilization: None,
                    keyframes: Vec::new(),
                    id,
                    content: ClipContent::Media { media_id: id },
                    transform: if track_id == 1 {
                        previous
                    } else {
                        Transform::default()
                    },
                    ..destination_clip(0, 30)
                }),
            }));
        }
        state.select_clip(Some((1, 2)));
        toggle_keyframe(&state);
        let keyed = selected_clip(&state).unwrap().1.keyframes;
        creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(creamui::theme::ThemeProvider::new(
                creamui::Theme::dark(),
            ));
            for (label, x, y) in [
                ("Top left", 4, 2),
                ("Top right", 66, 2),
                ("Bottom left", 4, 33),
                ("Bottom right", 66, 33),
            ] {
                let mut painter = TestPainter::default();
                let scene = Renderer::new().render(
                    inspector(&state, state.editor.borrow().project(), 220.0, false),
                    Size {
                        width: 220.0,
                        height: 2300.0,
                    },
                    &mut painter,
                );
                assert!(painter.labels.iter().any(|l| l == "PICTURE-IN-PICTURE"));
                let (_, button) = painter.bounds.iter().find(|(l, _)| l == label).unwrap();
                assert!(button.x >= 0.0 && button.x + button.width <= 220.0);
                scene
                    .hit_test(Point {
                        x: button.x + button.width / 2.0,
                        y: button.y + button.height / 2.0,
                    })
                    .unwrap()();

                let transform = selected_clip(&state).unwrap().1.transform;
                assert!(selected_clip(&state).unwrap().1.keyframes.is_empty());
                assert_eq!(transform.scale, 0.3);
                let plan = VisualPlan::from_project(state.editor.borrow().project());
                let frame = Compositor::new(plan, 100, 50, (30, 1))
                    .frame(0, &Arc::new(AtomicBool::new(false)))
                    .unwrap()
                    .unwrap();
                for (index, pixel) in frame.pixels.as_chunks::<4>().0.iter().enumerate() {
                    let overlay = (x..x + 30).contains(&(index % 100))
                        && (y..y + 15).contains(&(index / 100));
                    let expected = if overlay {
                        [0, 255, 0, 255]
                    } else {
                        [255, 0, 0, 255]
                    };
                    assert_eq!(*pixel, expected, "{label}, pixel {index}");
                }
                state.run(Action::Undo);
                assert_eq!(selected_clip(&state).unwrap().1.transform, previous);
                assert_eq!(selected_clip(&state).unwrap().1.keyframes, keyed);
                state.run(Action::Redo);
                assert_eq!(selected_clip(&state).unwrap().1.transform, transform);
                state.run(Action::Undo);
            }
        });
    }

    #[test]
    fn out_transition_controls_keep_independent_undo_bounds_and_direction() {
        use crate::timeline::SlideDirection;
        let state = UiState::new();
        set_transition_kind(&state, true, Some(TransitionKind::Slide));
        state.run(Action::AddTitle);
        set_transition_kind(&state, false, Some(TransitionKind::Dissolve));
        set_transition_kind(&state, true, Some(TransitionKind::Slide));
        let clip = selected_clip(&state).unwrap().1;
        assert_eq!(clip.transition_out.as_ref().unwrap().duration_frames, 15);
        set_slide_direction(&state, true, SlideDirection::Up);
        assert_eq!(
            selected_clip(&state)
                .unwrap()
                .1
                .transition_out
                .unwrap()
                .direction,
            SlideDirection::Up
        );
        state.run(Action::Undo);
        assert_eq!(
            selected_clip(&state)
                .unwrap()
                .1
                .transition_out
                .unwrap()
                .direction,
            SlideDirection::Right
        );
        state.run(Action::Redo);
        adjust_transition_duration(&state, true, true);
        assert_eq!(
            selected_clip(&state)
                .unwrap()
                .1
                .transition_out
                .unwrap()
                .duration_frames,
            23
        );
        state.run(Action::Undo);
        assert_eq!(
            selected_clip(&state)
                .unwrap()
                .1
                .transition_out
                .unwrap()
                .duration_frames,
            15
        );
        set_transition_kind(&state, true, Some(TransitionKind::Fade));
        set_transition_kind(&state, true, Some(TransitionKind::Slide));
        assert_eq!(
            selected_clip(&state)
                .unwrap()
                .1
                .transition_out
                .unwrap()
                .direction,
            SlideDirection::Up
        );
        for _ in 0..50 {
            adjust_transition_duration(&state, true, true);
        }
        let clip = selected_clip(&state).unwrap().1;
        assert_eq!(
            clip.transition_out.unwrap().duration_frames,
            clip.duration_frames
        );
        for _ in 0..50 {
            adjust_transition_duration(&state, true, false);
        }
        assert_eq!(
            selected_clip(&state)
                .unwrap()
                .1
                .transition_out
                .unwrap()
                .duration_frames,
            1
        );
        assert_eq!(selected_clip(&state).unwrap().1.transition, clip.transition);
        set_transition_kind(&state, true, None);
        assert!(selected_clip(&state).unwrap().1.transition_out.is_none());
        state.run(Action::Undo);
        assert!(selected_clip(&state).unwrap().1.transition_out.is_some());
        state.busy.set(true);
        let before = selected_clip(&state).unwrap().1;
        set_slide_direction(&state, true, SlideDirection::Left);
        adjust_transition_duration(&state, true, true);
        set_transition_kind(&state, true, None);
        assert_eq!(selected_clip(&state).unwrap().1, before);
    }

    #[test]
    fn transition_rows_are_clickable_and_fit_a_narrow_inspector() {
        let state = UiState::new();
        state.run(Action::AddTitle);
        set_transition_kind(&state, false, Some(TransitionKind::Slide));
        set_transition_kind(&state, true, Some(TransitionKind::Slide));
        creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(creamui::ThemeProvider::new(creamui::Theme::dark()));
            let clip = selected_clip(&state).unwrap().1;
            let mut painter = TestPainter::default();
            let size = Size {
                width: 240.0,
                height: 360.0,
            };
            let scene = Renderer::new().render(
                transition_section(&state, &clip, false),
                size,
                &mut painter,
            );
            for label in ["In", "Out", "From", "Toward", "Duration", "s"] {
                assert!(painter.labels.iter().any(|text| text == label), "{label}");
            }
            for (label, rect) in &painter.visible_bounds {
                assert!(
                    rect.x >= 0.0 && rect.x + rect.width <= 240.5,
                    "{label}: {rect:?}"
                );
            }
            let (_, arrow) = painter
                .bounds
                .iter()
                .rfind(|(label, _)| label == "←")
                .unwrap();
            scene
                .hit_test(Point {
                    x: arrow.x + arrow.width / 2.0,
                    y: arrow.y + arrow.height / 2.0,
                })
                .unwrap()();
            assert_eq!(
                selected_clip(&state)
                    .unwrap()
                    .1
                    .transition_out
                    .unwrap()
                    .direction,
                crate::timeline::SlideDirection::Left
            );
            assert_eq!(
                selected_clip(&state)
                    .unwrap()
                    .1
                    .transition
                    .unwrap()
                    .direction,
                crate::timeline::SlideDirection::Right
            );
            state.run(Action::Undo);
            assert_eq!(
                selected_clip(&state)
                    .unwrap()
                    .1
                    .transition_out
                    .unwrap()
                    .direction,
                crate::timeline::SlideDirection::Right
            );
        });
    }

    #[test]
    fn inspector_transition_choices_and_duration_are_bounded_and_undoable() {
        use crate::timeline::TransitionKind;
        let state = UiState::new();
        set_transition_kind(&state, false, Some(TransitionKind::Fade));
        state.run(Action::AddTitle);
        set_transition_kind(&state, false, Some(TransitionKind::Dissolve));
        assert_eq!(
            selected_clip(&state)
                .unwrap()
                .1
                .transition
                .unwrap()
                .duration_frames,
            15
        );
        adjust_transition_duration(&state, false, true);
        assert_eq!(
            selected_clip(&state)
                .unwrap()
                .1
                .transition
                .unwrap()
                .duration_frames,
            23
        );
        state.run(Action::Undo);
        assert_eq!(
            selected_clip(&state)
                .unwrap()
                .1
                .transition
                .unwrap()
                .duration_frames,
            15
        );
        state.run(Action::Redo);
        set_transition_kind(&state, false, Some(TransitionKind::Slide));
        assert_eq!(
            selected_clip(&state)
                .unwrap()
                .1
                .transition
                .unwrap()
                .duration_frames,
            23
        );
        for _ in 0..50 {
            adjust_transition_duration(&state, false, true);
        }
        let clip = selected_clip(&state).unwrap().1;
        assert_eq!(
            clip.transition.unwrap().duration_frames,
            clip.duration_frames
        );
        for _ in 0..50 {
            adjust_transition_duration(&state, false, false);
        }
        assert_eq!(
            selected_clip(&state)
                .unwrap()
                .1
                .transition
                .unwrap()
                .duration_frames,
            1
        );
        set_transition_kind(&state, false, None);
        assert!(selected_clip(&state).unwrap().1.transition.is_none());
        state.run(Action::Undo);
        assert!(selected_clip(&state).unwrap().1.transition.is_some());
    }

    #[test]
    fn transitions_are_visible_for_video_but_not_audio_and_ignore_busy_edits() {
        let state = UiState::new();
        state.run(Action::AddTitle);
        set_transition_kind(&state, false, Some(TransitionKind::Fade));
        state.select_detail(state.primary_clip().unwrap(), ClipDetail::Transition(false));
        assert!(
            render(&state, 1280.0, 760.0)
                .labels
                .iter()
                .any(|label| label == "Incoming transition")
        );
        set_transition_kind(&state, false, None);
        state.busy.set(true);
        set_transition_kind(&state, false, Some(TransitionKind::Fade));
        assert!(selected_clip(&state).unwrap().1.transition.is_none());
        state.busy.set(false);
        assert!(state.edit(Command::ImportMedia(Media {
            id: 1,
            path: "/sound.wav".into(),
            kind: MediaKind::Audio,
            duration_us: Some(1_000_000),
            has_audio: true
        })));
        state.insert_media(1);
        set_transition_kind(&state, false, Some(TransitionKind::Fade));
        adjust_transition_duration(&state, false, true);
        set_transition_kind(&state, true, Some(TransitionKind::Slide));
        set_slide_direction(&state, true, SlideDirection::Up);
        adjust_transition_duration(&state, true, true);
        assert!(selected_clip(&state).unwrap().1.transition.is_none());
        assert!(selected_clip(&state).unwrap().1.transition_out.is_none());
        assert!(
            !render(&state, 1280.0, 760.0)
                .labels
                .iter()
                .any(|label| label == "TRANSITION")
        );
    }

    #[test]
    fn chroma_slider_commits_final_value_after_scene_rebuilds() {
        let state = UiState::new();
        assert!(state.edit(Command::ImportMedia(Media {
            id: 1,
            path: "/green.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(1_000_000),
            has_audio: false,
        })));
        state.insert_media(1);
        let (track_id, clip) = selected_clip(&state).unwrap();
        let clip_id = clip.id;
        assert!(state.edit(Command::SetClipChromaKey {
            track_id,
            clip_id,
            key: Some(Default::default()),
        }));
        let build_section = |property: usize| {
            let mut painter = TestPainter::default();
            let clip = selected_clip(&state).unwrap().1;
            let scene = creamui::reactive::with_context_scope(|| {
                creamui::reactive::provide_context(creamui::theme::ThemeProvider::new(
                    creamui::Theme::dark(),
                ));
                Renderer::new().render(
                    Box::new(
                        Flex::column()
                            .child(Box::new(
                                RawButton::new(Style::default().size(96.0, 24.0), || {})
                                    .with_drag(|_, _| {})
                                    .disabled(state.chroma_preview.peek().is_some()),
                            ))
                            .child(chroma_section(&state, track_id, &clip, 280.0, false)),
                    ),
                    Size {
                        width: 280.0,
                        height: 1000.0,
                    },
                    &mut painter,
                )
            });
            let label = painter
                .bounds
                .iter()
                .find(|(label, _)| {
                    label.starts_with(["Similarity", "Smoothness", "Spill"][property])
                })
                .unwrap()
                .1;
            let index = (0..)
                .find(|&index| {
                    let Some((rect, _)) = scene.draggable_at(index) else {
                        panic!("missing slider");
                    };
                    rect.y >= label.y + label.height && rect.height == 24.0
                })
                .unwrap();
            (scene, index, painter)
        };
        for (property, press, final_value) in [(0, 8, 92), (0, 20, 60), (1, 92, 36), (2, 50, 80)] {
            let before = selected_clip(&state).unwrap().1.chroma_key.unwrap();
            let (scene, index, _) = build_section(property);
            let end = scene.drag_end_at(index).unwrap();
            let (rect, drag) = scene.draggable_at(index).unwrap();
            let pointer = |value: usize| Point {
                x: 6.0 + (rect.width - 12.0) * value as f32 / 100.0,
                y: 12.0,
            };
            let (start_rect, start) = scene
                .drag_start_at(Point {
                    x: rect.x + 8.0,
                    y: rect.y + 12.0,
                })
                .unwrap();
            start(Point { x: 8.0, y: 12.0 }, start_rect);
            drag(pointer(press), rect);
            let (rebuilt, rebuilt_index, _) = build_section(property);
            assert_eq!(rebuilt_index + 1, index);
            let point = pointer(final_value);
            let absolute = Point {
                x: point.x + rect.x,
                y: point.y + rect.y,
            };
            state.preview_chroma_drag(absolute);
            if let Some((routed_rect, drag)) = rebuilt.draggable_at(index) {
                drag(
                    Point {
                        x: absolute.x - routed_rect.x,
                        y: absolute.y - routed_rect.y,
                    },
                    routed_rect,
                );
            } else {
                assert_eq!(property, 2);
            }
            let (_, _, painted) = build_section(property);
            let mut expected = before;
            match property {
                0 => expected.similarity = final_value as f32 / 100.0,
                1 => expected.smoothness = final_value as f32 / 100.0,
                _ => expected.spill = final_value as f32 / 100.0,
            }
            assert_eq!(state.chroma_preview.peek().unwrap().1, expected);
            assert!(painted.labels.iter().any(|label| label
                == &format!(
                    "{} · {final_value}%",
                    [
                        "Similarity",
                        "Smoothness (soft edge)",
                        "Spill (remove color tint)"
                    ][property]
                )));
            let knob = painted
                .fills
                .iter()
                .find(|(fill, _)| {
                    fill.width == 11.0
                        && fill.height == 11.0
                        && fill.y >= rect.y
                        && fill.y < rect.y + rect.height
                })
                .unwrap()
                .0;
            assert_eq!(
                knob.x,
                (rect.x + 6.0 + (rect.width - 12.0) * final_value as f32 / 100.0 - 5.5).round()
            );
            assert_eq!(selected_clip(&state).unwrap().1.chroma_key.unwrap(), before);
            end();
            assert_eq!(
                selected_clip(&state).unwrap().1.chroma_key.unwrap(),
                expected
            );
            state.run(Action::Undo);
            assert_eq!(selected_clip(&state).unwrap().1.chroma_key.unwrap(), before);
            state.run(Action::Redo);
            assert_eq!(
                selected_clip(&state).unwrap().1.chroma_key.unwrap(),
                expected
            );
        }
    }

    #[test]
    fn chroma_inspector_media_controls_and_slider_commit_once() {
        let state = UiState::new();
        state.run(Action::AddTitle);
        assert!(
            !render(&state, 1280.0, 1100.0)
                .labels
                .iter()
                .any(|label| label == "Chroma key")
        );
        for (id, kind) in [
            (1, MediaKind::Video),
            (2, MediaKind::Image),
            (3, MediaKind::Audio),
        ] {
            assert!(state.edit(Command::ImportMedia(Media {
                id,
                path: format!("/media-{id}").into(),
                kind,
                duration_us: Some(1_000_000),
                has_audio: false,
            })));
            state.insert_media(id);
            if kind != MediaKind::Audio {
                apply_selected_effect(&state, EffectKind::ChromaKey);
            }
            let labels = render(&state, 1280.0, 2600.0).labels;
            assert_eq!(
                labels.iter().any(|label| label == "Green"),
                kind != MediaKind::Audio
            );
            if kind == MediaKind::Audio {
                continue;
            }
            let (track_id, clip) = selected_clip(&state).unwrap();
            let clip_id = clip.id;
            assert_eq!(clip.chroma_key, Some(Default::default()));
            let labels = render(&state, 1280.0, 2600.0).labels;
            assert!(labels.iter().any(|label| label == "Green"));
            assert!(labels.iter().any(|label| label == "Blue"));
            let slider = ChromaSlider {
                state: state.clone(),
                target: (track_id, clip_id),
                property: 0,
                key: Default::default(),
            };
            let rect = Rect {
                x: 0.0,
                y: 0.0,
                width: 96.0,
                height: 24.0,
            };
            slider.on_drag_start().unwrap()(Point { x: 30.0, y: 12.0 }, rect);
            slider.on_drag().unwrap()(Point { x: 30.0, y: 12.0 }, rect);
            slider.on_drag().unwrap()(Point { x: 48.0, y: 12.0 }, rect);
            assert_eq!(
                selected_clip(&state)
                    .unwrap()
                    .1
                    .chroma_key
                    .unwrap()
                    .similarity,
                0.1
            );
            slider.on_drag_end().unwrap()();
            assert_eq!(
                selected_clip(&state)
                    .unwrap()
                    .1
                    .chroma_key
                    .unwrap()
                    .similarity,
                0.5
            );
            state.run(Action::Undo);
            assert_eq!(
                selected_clip(&state)
                    .unwrap()
                    .1
                    .chroma_key
                    .unwrap()
                    .similarity,
                0.1
            );
            slider.on_drag_start().unwrap()(Point { x: 30.0, y: 12.0 }, rect);
            state.busy.set(true);
            state.preview_chroma_drag(Point { x: 80.0, y: 12.0 });
            assert!(state.chroma_preview.peek().is_none());
            state.busy.set(false);
            state.preview_chroma_drag(Point { x: 80.0, y: 12.0 });
            assert!(state.chroma_preview.peek().is_some());
            state.run(Action::Escape);
            assert!(state.chroma_gesture.borrow().is_none());
            state.preview_chroma_drag(Point { x: 60.0, y: 12.0 });
            slider.on_drag_end().unwrap()();
            assert!(state.chroma_preview.peek().is_none());
            assert_eq!(
                selected_clip(&state)
                    .unwrap()
                    .1
                    .chroma_key
                    .unwrap()
                    .similarity,
                0.1
            );
        }
    }

    #[test]
    fn lut_inspector_shows_media_filename_but_not_titles_or_audio() {
        let state = UiState::new();
        state.run(Action::AddTitle);
        assert!(
            !render(&state, 1280.0, 2600.0)
                .labels
                .iter()
                .any(|label| label == "LUT")
        );
        for (id, kind, path) in [
            (1, MediaKind::Video, "/scene.mp4"),
            (2, MediaKind::Audio, "/sound.wav"),
        ] {
            assert!(state.edit(Command::ImportMedia(Media {
                id,
                path: path.into(),
                kind,
                duration_us: Some(1_000_000),
                has_audio: false
            })));
            state.insert_media(id);
            if kind == MediaKind::Video {
                let (track_id, clip) = selected_clip(&state).unwrap();
                assert!(state.edit(Command::ApplyEffect {
                    track_id,
                    clip_id: clip.id,
                    kind: EffectKind::ColorLut,
                    path: Some("/looks/initial.cube".into())
                }));
                state.select_detail(
                    (track_id, clip.id),
                    ClipDetail::Effect(EffectKind::ColorLut),
                );
            }
            let labels = render(&state, 1280.0, 2600.0).labels;
            assert_eq!(
                labels.iter().any(|label| label == "LUT"),
                kind == MediaKind::Video
            );
            if kind == MediaKind::Video {
                assert!(labels.iter().any(|label| label == "Choose .cube file…"));
                let (track_id, clip) = selected_clip(&state).unwrap();
                assert!(state.edit(Command::SetClipLut {
                    track_id,
                    clip_id: clip.id,
                    path: Some("/looks/summer.cube".into())
                }));
                assert!(
                    render(&state, 1280.0, 2600.0)
                        .labels
                        .iter()
                        .any(|label| label == "summer.cube")
                );
            }
        }
    }

    #[test]
    fn inspector_source_steps_preserve_length_and_clamp_to_media() {
        let state = UiState::new();
        assert!(state.edit(Command::ImportMedia(Media {
            id: 1,
            path: "/source.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(3_000_000),
            has_audio: false
        })));
        assert!(state.edit(Command::InsertClip {
            track_id: 0,
            clip: Box::new(Clip {
                effect_stack: Vec::new(),
                playback: crate::timeline::Playback::Forward,
                effect: None,
                mask: None,
                lut: None,
                chroma_key: None,
                id: 1,
                start_frame: 0,
                source_start_frame: 0,
                duration_frames: 60,
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
        state.select_clip(Some((0, 1)));
        adjust_timing(&state, true, true, true);
        adjust_timing(&state, true, true, true);
        let clip = selected_clip(&state).unwrap().1;
        assert_eq!(clip.source_start_frame, 30);
        assert_eq!(clip.duration_frames, 60);
        state.run(Action::Undo);
        assert_eq!(selected_clip(&state).unwrap().1.source_start_frame, 0);
        adjust_timing(&state, true, true, false);
        assert_eq!(selected_clip(&state).unwrap().1.source_start_frame, 0);
    }

    #[test]
    fn timeline_draws_filmstrips_and_scrubs_from_the_ruler() {
        let state = UiState::new();
        let image = Arc::new(RgbaImage::new(16, 9, vec![255; 16 * 9 * 4]).unwrap());
        let view = TimelineView {
            lanes: vec![Lane {
                track_id: 0,
                kind: TrackKind::Video,
                label: "V1".into(),
                locked: false,
                hidden: false,
                base: true,
                removable: false,
                clips: vec![LaneClip {
                    effects: Vec::new(),
                    keyframes: Vec::new(),
                    playback: Playback::Forward,
                    transition_frames: 0,
                    transition_out_frames: 0,
                    clip_id: 7,
                    start: 30,
                    duration: 150,
                    name: "Source".into(),
                    look: ClipLook::Video,
                    images: Some(Arc::new(vec![image.clone(), image.clone(), image])),
                    fallback: None,
                    waveform: None,
                    volume: 1.0,
                    muted: false,
                    speed: 1.0,
                    fade_in_frames: 0,
                    fade_out_frames: 0,
                    linked_audio: false,
                }],
            }],
            ghosts: vec![super::super::timeline_view::Ghost {
                track_id: 0,
                start: 200,
                duration: 30,
                valid: false,
            }],
            ..view(&state)
        };
        let mut painter = TestPainter::default();
        view.paint(&mut painter, TIMELINE);
        assert!(painter.images.len() >= 3);
        assert!(painter.labels.iter().any(|label| label == "Source"));
        assert!(painter.labels.iter().any(|label| label == "00:02"));
        press(&view, at(150.0), 10.0);
        assert_eq!(state.program.position.peek(), 150);
        drag_to(&view, 2000.0, 10.0);
        assert_eq!(state.program.position.peek(), 299);
        release(&view);
        press(&view, 40.0, 60.0);
        drag_to(&view, at(10.0), 60.0);
        assert_eq!(state.program.position.peek(), 299);
    }

    #[test]
    fn timeline_draws_markers_and_ruler_scrubs_snap_to_them() {
        let state = UiState::new();
        let view = TimelineView {
            markers: vec![60, 120],
            ..view(&state)
        };
        let mut painter = TestPainter::default();
        view.paint(&mut painter, TIMELINE);
        assert!(painter.labels.iter().any(|label| label == "1"));
        assert!(painter.labels.iter().any(|label| label == "2"));
        press(&view, at(121.0), 10.0);
        assert_eq!(state.program.position.peek(), 120);
        drag_to(&view, at(90.0), 10.0);
        assert_eq!(state.program.position.peek(), 90);
        drag_to(&view, at(59.0), 10.0);
        assert_eq!(state.program.position.peek(), 60);
        release(&view);
    }

    #[test]
    fn timeline_selects_drags_clips_between_tracks_and_rubber_bands() {
        let state = UiState::new();
        state.timeline_rect.set(TIMELINE);
        assert!(state.add_title());
        assert!(state.add_title());
        state.select_clip(None);
        let timeline = view(&state);
        press(&timeline, at(30.0), 50.0);
        release(&timeline);
        assert_eq!(state.selected.peek(), vec![(0, 1)]);
        assert_eq!(state.program.position.peek(), 30);

        state.modifiers.set(creamui::core::Modifiers {
            ctrl: true,
            ..Default::default()
        });
        press(&timeline, at(120.0), 50.0);
        release(&timeline);
        assert_eq!(state.selected.peek(), vec![(0, 1), (0, 2)]);
        state.modifiers.set(Default::default());

        press(&timeline, at(120.0), 50.0);
        drag_to(&timeline, at(150.0), 110.0);
        assert!(state.drag.peek().is_some());
        assert_eq!(state.drop_plan().unwrap().ghosts.len(), 2);
        release(&timeline);
        assert!(state.drag.peek().is_none());
        let starts = |track: usize| {
            state.editor.borrow().project().tracks()[track]
                .clips
                .iter()
                .map(|clip| clip.start_frame)
                .collect::<Vec<_>>()
        };
        assert_eq!(starts(1), vec![30, 120]);
        assert!(starts(0).is_empty());
        assert_eq!(state.selected.peek().len(), 2);
        state.run(Action::Undo);
        assert_eq!(starts(0), vec![0, 90]);

        state.select_clip(None);
        let timeline = view(&state);
        press(&timeline, at(250.0), 50.0);
        drag_to(&timeline, at(5.0), 60.0);
        assert!(state.marquee.peek().is_some());
        assert_eq!(state.selected.peek(), vec![(0, 1), (0, 2)]);
        release(&timeline);
        assert!(state.marquee.peek().is_none());
        state.run(Action::Delete);
        assert!(starts(0).is_empty());
        state.run(Action::Undo);
        assert_eq!(starts(0), vec![0, 90]);

        assert!(state.edit(Command::ImportMedia(Media {
            id: 9,
            path: "/tmp/shot.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(1_000_000),
            has_audio: false,
        })));
        state.begin_media_drag(9, Point { x: 10.0, y: 10.0 });
        state.drag_pointer(Point {
            x: at(200.0),
            y: 120.0,
        });
        let plan = state.drop_plan().unwrap();
        assert!(plan.valid);
        assert_eq!(plan.ghosts[0].track_id, 1);
        state.end_media_drag();
        assert_eq!(starts(1), vec![200]);
        assert_eq!(state.selected.peek().len(), 1);

        state.begin_media_drag(9, Point { x: 10.0, y: 10.0 });
        state.drag_pointer(Point {
            x: at(30.0),
            y: 50.0,
        });
        assert!(!state.drop_plan().unwrap().valid);
        state.run(Action::Escape);
        assert!(state.drag.peek().is_none());
        state.end_media_drag();
        assert_eq!(starts(0), vec![0, 90]);
    }

    #[test]
    fn frozen_clips_hide_speed_presets_and_reject_speed_edits() {
        let state = UiState::new();
        assert!(state.edit(Command::ImportMedia(Media {
            id: 7,
            path: "video.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(4_000_000),
            has_audio: true,
        })));
        state.insert_media(7);
        let (track_id, clip) = selected_clip(&state).unwrap();
        assert!(state.edit(Command::SetClipSpeed {
            track_id,
            clip_id: clip.id,
            speed: 2.0
        }));
        assert!(state.edit(Command::SetPlayback {
            track_id,
            clip_id: clip.id,
            playback: crate::timeline::Playback::Freeze,
        }));
        let frozen = state.editor.borrow().project().clone();
        assert_eq!(selected_clip(&state).unwrap().1.speed, 1.0);
        let labels = render(&state, 1440.0, 2400.0).labels;
        assert!(!labels.iter().any(|label| label.starts_with("SPEED ·")));
        for speed in [0.25, 0.5, 1.0, 1.5, 2.0, 4.0, f64::NAN] {
            assert!(
                !state
                    .editor
                    .borrow()
                    .can_set_clip_speed(track_id, clip.id, speed)
            );
        }
        assert!(!state.edit(Command::SetClipSpeed {
            track_id,
            clip_id: clip.id,
            speed: 2.0
        }));
        assert_eq!(state.editor.borrow().project(), &frozen);
        assert!(state.editor.borrow_mut().undo());
        assert_eq!(selected_clip(&state).unwrap().1.speed, 2.0);
    }

    #[test]
    fn short_tail_disables_unavailable_speed_presets() {
        let state = UiState::new();
        assert!(state.edit(Command::ImportMedia(Media {
            id: 7,
            path: "video.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(4_000_000),
            has_audio: false,
        })));
        state.insert_media(7);
        let (track_id, clip) = selected_clip(&state).unwrap();
        assert!(state.edit(Command::Split {
            track_id,
            clip_id: clip.id,
            at_frame: 117,
            new_id: 99,
        }));
        state.select_clip(Some((track_id, 99)));
        apply_selected_effect(&state, EffectKind::Speed);
        assert!(state.edit(Command::SetClipSpeed {
            track_id,
            clip_id: 99,
            speed: 1.0
        }));
        let before = state.editor.borrow().project().clone();
        let status = state.status.peek();
        creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(creamui::ThemeProvider::new(creamui::Theme::dark()));
            let mut painter = TestPainter::default();
            let scene = Renderer::new().render(
                inspector(&state, &before, 300.0, false),
                Size {
                    width: 300.0,
                    height: 4000.0,
                },
                &mut painter,
            );
            let point = |label| {
                let rect = painter
                    .bounds
                    .iter()
                    .find(|(text, _)| text == label)
                    .unwrap()
                    .1;
                Point {
                    x: rect.x + rect.width / 2.0,
                    y: rect.y + rect.height / 2.0,
                }
            };
            assert!(scene.hit_test(point("4x")).is_none());
            assert!(
                painter
                    .labels
                    .iter()
                    .any(|text| text == "Not enough source left for some speeds.")
            );
            assert_eq!(state.editor.borrow().project(), &before);
            assert_eq!(state.status.peek(), status);
            for label in ["0.25x", "0.5x", "1.5x", "2x"] {
                assert!(scene.hit_test(point(label)).is_some(), "{label}");
            }
            scene.hit_test(point("2x")).unwrap()();
        });
        let clip = selected_clip(&state).unwrap().1;
        assert_eq!((clip.speed, clip.duration_frames), (2.0, 1));
        assert!(state.editor.borrow_mut().undo());
        assert_eq!(state.editor.borrow().project(), &before);
    }

    #[test]
    fn speed_presets_apply_to_media_but_not_titles_or_stills() {
        let state = UiState::new();
        assert!(
            !render(&state, 1440.0, 2400.0)
                .labels
                .iter()
                .any(|label| label.starts_with("SPEED ·"))
        );
        assert!(state.add_title());
        assert!(
            !render(&state, 1440.0, 2400.0)
                .labels
                .iter()
                .any(|label| label.starts_with("SPEED ·"))
        );
        for (id, kind, path) in [
            (7, MediaKind::Video, "video.mp4"),
            (8, MediaKind::Image, "still.png"),
        ] {
            assert!(state.edit(Command::ImportMedia(Media {
                id,
                path: path.into(),
                kind,
                duration_us: Some(4_000_000),
                has_audio: false
            })));
            state.insert_media(id);
            if kind == MediaKind::Video {
                apply_selected_effect(&state, EffectKind::Speed);
                let (track_id, clip) = selected_clip(&state).unwrap();
                assert!(state.edit(Command::SetClipSpeed {
                    track_id,
                    clip_id: clip.id,
                    speed: 1.0
                }));
            }
            let labels = render(&state, 1440.0, 2400.0).labels;
            if kind == MediaKind::Image {
                assert!(!labels.iter().any(|label| label.starts_with("SPEED ·")));
            } else {
                for preset in ["0.25x", "0.5x", "1x", "1.5x", "2x", "4x"] {
                    assert!(labels.iter().any(|label| label == preset), "{preset}");
                }
                let (track_id, clip) = selected_clip(&state).unwrap();
                assert!(state.edit(Command::SetClipSpeed {
                    track_id,
                    clip_id: clip.id,
                    speed: 2.0
                }));
                let labels = render(&state, 1440.0, 2400.0).labels;
                assert!(labels.iter().any(|label| label == "SPEED · 2X"));
                assert!(labels.iter().any(|label| label == "2x"));
                adjust_timing(&state, true, false, true);
                assert_eq!(selected_clip(&state).unwrap().1.source_start_frame, 0);
                adjust_timing(&state, false, true, false);
                assert_eq!(selected_clip(&state).unwrap().1.duration_frames, 30);
                adjust_timing(&state, true, true, true);
                let clip = selected_clip(&state).unwrap().1;
                assert_eq!((clip.source_start_frame, clip.source_span()), (30, 60));
            }
        }
    }

    #[test]
    fn editor_renders_both_monitors_and_edits_titles() {
        let state = UiState::new();
        for width in [1440.0, 900.0] {
            let painter = render(&state, width, 900.0);
            for label in [
                "OCUS",
                "Media",
                "Source",
                "Program",
                "00:00",
                "No source selected",
            ] {
                assert!(painter.labels.iter().any(|text| text == label), "{label}");
            }
            for label in ["V1", "V2", "A1", "A2"] {
                assert!(painter.labels.iter().any(|text| text == label));
            }
            for label in ["BASE", "ABOVE"] {
                assert!(painter.labels.iter().any(|text| text == label));
            }
        }
        assert!(state.add_title());
        let painter = render(&state, 1440.0, 2400.0);
        assert!(painter.labels.iter().any(|text| text == "Title 1"));
        assert!(painter.labels.iter().any(|text| text == "Title clip"));
        assert!(!painter.labels.iter().any(|text| text == "Noise reduction"));
        state.title_text.set_value("Opening scene");
        set_title_text(&state);
        assert_eq!(
            selected_clip(&state).and_then(|(_, clip)| match clip.content {
                ClipContent::Title(title) => Some(title.text),
                _ => None,
            }),
            Some("Opening scene".into())
        );
        state.undo();
        assert_eq!(
            selected_clip(&state).and_then(|(_, clip)| match clip.content {
                ClipContent::Title(title) => Some(title.text),
                _ => None,
            }),
            Some("Title 1".into())
        );

        assert!(state.edit(Command::ImportMedia(Media {
            id: 7,
            path: "footage/tone.wav".into(),
            kind: MediaKind::Audio,
            duration_us: Some(1_000_000),
            has_audio: true,
        })));
        state.insert_media(7);
        let painter = render(&state, 1440.0, 2400.0);
        assert!(
            painter
                .labels
                .iter()
                .any(|label| label.contains("tone.wav"))
        );
        assert!(painter.labels.iter().any(|label| label == "100"));
        assert!(painter.labels.iter().any(|label| label == "Audio source"));
        apply_selected_effect(&state, EffectKind::Fades);
        let (track_id, clip) = selected_clip(&state).unwrap();
        assert!(state.edit(Command::SetClipFades {
            track_id,
            clip_id: clip.id,
            fade_in_frames: 0,
            fade_out_frames: 0
        }));
        let painter = render(&state, 1440.0, 2400.0);
        assert!(painter.labels.iter().any(|label| label == "Fade in"));
        assert!(painter.labels.iter().any(|label| label == "Fade out"));
        assert!(painter.labels.iter().any(|label| label == "0.000"));
        for (label, rect) in &painter.bounds {
            if label == "Fade in" || label == "Fade out" {
                assert!(rect.x >= 0.0 && rect.x + rect.width <= 1440.0);
                assert!(rect.y >= 0.0 && rect.y + rect.height <= 2400.0);
            }
        }
        change_clip_fade(&state, true, true);
        change_clip_fade(&state, false, true);
        change_clip_fade(&state, true, true);
        let (_, clip) = selected_clip(&state).unwrap();
        assert_eq!(
            clip.fade_in_frames + clip.fade_out_frames,
            clip.duration_frames
        );
        let painter = render(&state, 1440.0, 2400.0);
        assert!(painter.labels.iter().any(|label| label == "0.500"));

        assert!(!selected_clip(&state).unwrap().1.noise_reduction);
        apply_selected_effect(&state, EffectKind::NoiseReduction);
        assert!(selected_clip(&state).unwrap().1.noise_reduction);
        state.undo();
        assert!(!selected_clip(&state).unwrap().1.noise_reduction);
        state.redo();
        assert!(selected_clip(&state).unwrap().1.noise_reduction);
        set_clip_audio(&state, |clip| (clip.volume - 0.5, true));
        assert!(selected_clip(&state).unwrap().1.noise_reduction);
        let painter = render(&state, 1440.0, 2400.0);
        assert!(painter.labels.iter().any(|label| label == "50"));
        assert!(painter.labels.iter().any(|label| label == "Unmute"));
        let (track_id, clip) = selected_clip(&state).unwrap();
        assert!(state.edit(Command::SetTrackLocked {
            track_id,
            locked: true,
        }));
        assert!(!state.edit(Command::SetClipNoiseReduction {
            track_id,
            clip_id: clip.id,
            enabled: false,
        }));
    }

    #[test]
    fn wheel_axes_scroll_tracks_or_time_and_ctrl_zooms_at_the_pointer() {
        let state = UiState::new();
        state.timeline_rect.set(TIMELINE);
        assert!(state.edit(Command::ImportMedia(Media {
            id: 1,
            path: "/tmp/long.mp4".into(),
            kind: MediaKind::Video,
            duration_us: Some(60_000_000),
            has_audio: true,
        })));
        state.insert_media(1);
        for _ in 0..4 {
            state.add_track(TrackKind::Audio);
        }
        let wheel = |state: &UiState, x: f32, y: f32, ctrl: bool| {
            let view = TimelineView {
                start: state.timeline_start.peek(),
                scroll: state.track_scroll.peek(),
                ..view(state)
            };
            view.on_wheel().unwrap()(creamui::WheelInput {
                delta: Point { x, y },
                modifiers: creamui::core::Modifiers {
                    ctrl,
                    ..Default::default()
                },
            });
        };
        wheel(&state, 0.0, 60.0, false);
        assert_eq!(state.timeline_start.peek(), 0);
        assert_eq!(state.track_scroll.peek(), 60.0);
        wheel(&state, 100.0, 0.0, false);
        assert_eq!(state.timeline_start.peek(), 30);
        assert_eq!(state.track_scroll.peek(), 60.0);
        wheel(&state, -1.0, 0.0, false);
        assert_eq!(state.timeline_start.peek(), 29);
        wheel(&state, 0.0, -500.0, false);
        assert_eq!(state.track_scroll.peek(), 0.0);
        assert_eq!(state.timeline_start.peek(), 29);

        state.pointer.set(Point {
            x: at(150.0),
            y: 60.0,
        });
        let zoom = state.timeline_zoom.peek();
        wheel(&state, 0.0, -10.0, true);
        assert_eq!(state.timeline_zoom.peek(), zoom);
        wheel(&state, 0.0, -20.0, true);
        assert_eq!(state.timeline_zoom.peek(), zoom - 1);
        assert_eq!(state.visible_frames(), 240);
        assert_eq!(state.timeline_start.peek(), 179 - 120);
    }

    #[test]
    fn export_is_disabled_until_the_timeline_has_content() {
        let state = UiState::new();
        let painter = render(&state, 1440.0, 900.0);
        assert!(painter.labels.iter().any(|label| label == "Export"));
        assert!(!painter.labels.iter().any(|label| label.contains("silent")));
        state.run(Action::Export);
        assert_eq!(state.status.peek(), "Ready · Project not saved");
    }

    #[test]
    fn export_destination_is_visible_and_changeable_on_every_page() {
        let state = UiState::new();
        assert!(state.add_title());
        state.export_dialog();
        let path = std::env::temp_dir().join("Movies/export.mp4");
        state
            .export_path
            .set_value(path.to_string_lossy().into_owned());
        for page in [
            ExportPage::Presets,
            ExportPage::Video,
            ExportPage::Audio,
            ExportPage::Output,
        ] {
            state.export_page.set(page);
            for width in [420.0, 680.0, 1280.0] {
                let painter = render(&state, width, 480.0);
                assert!(painter.labels.iter().any(|label| label == "Save to"));
                assert!(painter.labels.iter().any(|label| label == "Change…"));
                assert!(
                    painter
                        .labels
                        .iter()
                        .any(|label| label.starts_with(path.parent().unwrap().to_str().unwrap()))
                );
                for label in ["Save to", "Change…", "Export"] {
                    let (_, bounds) = painter
                        .bounds
                        .iter()
                        .rev()
                        .find(|(text, _)| text == label)
                        .unwrap();
                    assert!(bounds.x >= 0.0 && bounds.x + bounds.width <= width);
                    assert!(bounds.y >= 0.0 && bounds.y + bounds.height <= 480.0);
                }
            }
        }
    }

    #[test]
    fn export_dialog_says_export_also_saves_a_saved_project() {
        const HINT: &str = "Export also saves your project";
        let state = UiState::new();
        assert!(state.add_title());
        state.export_dialog();
        assert!(
            !render(&state, 1280.0, 480.0)
                .labels
                .iter()
                .any(|label| label == HINT)
        );
        state
            .project_path
            .set(Some(std::env::temp_dir().join("Movies/project.ocus")));
        for width in [420.0, 680.0, 1280.0] {
            let painter = render(&state, width, 480.0);
            let (_, bounds) = painter
                .bounds
                .iter()
                .rev()
                .find(|(text, _)| text == HINT)
                .unwrap();
            assert!(bounds.x >= 0.0 && bounds.x + bounds.width <= width);
            assert!(bounds.y >= 0.0 && bounds.y + bounds.height <= 480.0);
        }
    }

    #[test]
    fn export_modal_shows_presets_and_custom_fields_at_small_window_sizes() {
        let state = UiState::new();
        assert!(state.add_title());
        state.export_dialog();
        assert!(state.export_modal.peek());
        let painter = render(&state, 680.0, 480.0);
        for label in [
            "Export timeline",
            "Full HD 1080p",
            "HD 720p",
            "Portrait 9:16",
            "Video",
            "Audio",
            "Destination",
            "Export",
        ] {
            assert!(
                painter.labels.iter().any(|item| item == label),
                "missing {label}: {:?}",
                painter.labels
            );
        }
        let button = painter
            .bounds
            .iter()
            .rev()
            .find(|(label, _)| label == "Export")
            .unwrap()
            .1;
        assert!(button.y >= 0.0 && button.y + button.height <= 480.0);
        let narrow = render(&state, 420.0, 480.0);
        assert!(narrow.labels.iter().any(|label| label == "Presets"));
        assert!(narrow.labels.iter().any(|label| label == "Export"));
        assert!(state.export_scroll.max_offset() > 0.0);
        state.export_scroll.set(state.export_scroll.max_offset());
        let painter = render(&state, 680.0, 480.0);
        assert!(painter.labels.iter().any(|label| label == "Save preset"));
        state.export_page.set(ExportPage::Video);
        state.export_scroll.set(0.0);
        let painter = render(&state, 680.0, 480.0);
        assert!(painter.labels.iter().any(|label| label == "Video codec"));
        state
            .export_scroll
            .set(state.export_scroll.max_offset() / 2.0);
        let painter = render(&state, 680.0, 480.0);
        assert!(painter.labels.iter().any(|label| label == "Width (pixels)"));
        state.export_page.set(ExportPage::Audio);
        state.export_scroll.set(0.0);
        let painter = render(&state, 680.0, 480.0);
        assert!(painter.labels.iter().any(|label| label == "Audio codec"));
        state.export_scroll.set(state.export_scroll.max_offset());
        let painter = render(&state, 680.0, 480.0);
        assert!(painter.labels.iter().any(|label| label == "Audio (kbps)"));
        state.export_page.set(ExportPage::Output);
        state.export_scroll.set(0.0);
        let painter = render(&state, 680.0, 480.0);
        assert!(painter.labels.iter().any(|label| label == "Browse…"));
        state.select_export_preset("Portrait 9:16");
        assert_eq!(state.export_form().unwrap().width, 1080);
        assert_eq!(state.export_form().unwrap().height, 1920);
        state.change_export_codecs(Some(Container::WebM), None, None);
        assert_eq!(state.export_form().unwrap().video_codec, VideoCodec::Vp9);
        assert_eq!(state.export_form().unwrap().audio_codec, AudioCodec::Opus);
        assert!(state.export_path.peek().ends_with(".webm"));
        state.set_export_quality_mode(true);
        state.export_quality.set_value("34");
        state.set_export_speed(EncodingSpeed::Balanced);
        assert_eq!(state.export_form().unwrap().video_quality, Some(34));
        assert_eq!(state.export_form().unwrap().speed, EncodingSpeed::Balanced);
        state.export_quality.set_value("invalid");
        state.set_export_quality_mode(false);
        assert_eq!(state.export_form().unwrap().video_quality, None);
        state.export_width.set_value("oops");
        assert!(state.export_form().is_err());
        state.change_export_codecs(Some(Container::Gif), None, None);
        assert_eq!(state.export_form().unwrap().container, Container::Gif);
        assert!(state.export_path.peek().ends_with(".gif"));
        assert!(state.export_form().unwrap().width <= 640);
        state.export_audio_bitrate.set_value("unused");
        assert!(state.export_form().is_ok());
        state.export_page.set(ExportPage::Audio);
        let painter = render(&state, 680.0, 480.0);
        assert!(painter.labels.iter().any(|label| label == "No audio"));
        assert!(!painter.labels.iter().any(|label| label == "Audio (kbps)"));
        state.select_export_preset("Audio only (WAV)");
        assert!(state.export_path.peek().ends_with(".wav"));
        state.export_width.set_value("unused");
        state.export_audio_bitrate.set_value("unused");
        assert_eq!(state.export_form().unwrap().container, Container::Wav);
        let painter = render(&state, 680.0, 480.0);
        assert!(
            painter
                .labels
                .iter()
                .any(|label| label == "Uncompressed audio")
        );
        state.export_page.set(ExportPage::Video);
        let painter = render(&state, 680.0, 480.0);
        assert!(painter.labels.iter().any(|label| label == "WAV"));
        assert!(!painter.labels.iter().any(|label| label == "Width (pixels)"));
        state.run(Action::Escape);
        assert!(!state.export_modal.peek());

        let mut library = crate::export::presets::PresetLibrary::default();
        library
            .insert(
                "My vertical".into(),
                crate::export::presets::builtins(state.editor.borrow().project())[3]
                    .settings
                    .clone(),
            )
            .unwrap();
        let other_project = UiState::new();
        other_project.export_library.set(library);
        assert!(other_project.add_title());
        other_project.export_dialog();
        other_project.select_export_preset("My vertical");
        assert_eq!(other_project.export_form().unwrap().height, 1920);
        render(&other_project, 680.0, 480.0);
        other_project
            .export_scroll
            .set(other_project.export_scroll.max_offset());
        assert!(
            render(&other_project, 680.0, 480.0)
                .labels
                .iter()
                .any(|label| label == "My vertical")
        );
    }

    #[test]
    fn export_queue_shows_progress_and_lets_busy_users_add_or_remove_jobs() {
        use crate::export::queue::Request;
        use crate::project::export_settings::LastExport;

        let state = UiState::new();
        assert!(state.add_title());
        let exported = std::env::temp_dir().join("active.mp4");
        state
            .project_path
            .set(Some(exported.with_extension("ocus")));
        let mut queue = Queue::default();
        for path in [&exported, &exported.with_file_name("waiting.mp4")] {
            queue
                .enqueue(Request {
                    project: state.editor.borrow().project().clone(),
                    output: LastExport {
                        path: path.clone(),
                        settings: state.export_draft.peek(),
                    },
                    total: 90,
                    auto_number: false,
                })
                .unwrap();
        }
        let active = queue.start_next().unwrap();
        state.export_queue.set(queue);
        state.busy.set(true);
        state.open_export_queue();

        for (width, height) in [(420.0, 480.0), (1280.0, 760.0)] {
            let painter = render(&state, width, height);
            for label in [
                "Export queue",
                "active.mp4",
                "waiting.mp4",
                "Remove",
                "Clear finished",
                "1 active · 1 waiting",
            ] {
                let (_, bounds) = painter
                    .bounds
                    .iter()
                    .find(|(text, _)| text == label)
                    .unwrap_or_else(|| panic!("missing {label}"));
                assert!(
                    bounds.x >= 0.0 && bounds.x + bounds.width <= width,
                    "{label}: {bounds:?}"
                );
                assert!(
                    bounds.y >= 0.0 && bounds.y + bounds.height <= height,
                    "{label}: {bounds:?}"
                );
            }
        }

        let viewport = Size {
            width: 420.0,
            height: 480.0,
        };
        creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(creamui::ThemeProvider::new(creamui::Theme::dark()));
            let click = |label: &str| {
                let mut painter = TestPainter::default();
                let scene = Renderer::new().render(build(&state, viewport), viewport, &mut painter);
                let rect = painter
                    .bounds
                    .iter()
                    .rev()
                    .find(|(text, _)| text == label)
                    .unwrap()
                    .1;
                scene
                    .hit_test(Point {
                        x: rect.x + rect.width / 2.0,
                        y: rect.y + rect.height / 2.0,
                    })
                    .expect(label)();
            };
            click("Remove");
            assert!(matches!(
                state.export_queue.peek().jobs()[1].status,
                ExportStatus::Cancelled
            ));
            assert!(matches!(
                state.export_queue.peek().active().unwrap().status,
                ExportStatus::Encoding {
                    cancelling: false,
                    ..
                }
            ));
            click("Add export…");
            assert!(state.export_modal.peek());
            assert_eq!(
                std::path::PathBuf::from(state.export_path.peek()),
                exported.with_file_name("export.mp4")
            );
            assert!(
                render(&state, viewport.width, viewport.height)
                    .labels
                    .iter()
                    .any(|label| label == "Add to queue")
            );
            state.run(Action::Escape);
            assert!(!state.export_modal.peek());
            assert!(state.export_queue_modal.peek());
            click("Cancel");
            assert!(matches!(
                state.export_queue.peek().active().unwrap().status,
                ExportStatus::Encoding {
                    cancelling: true,
                    ..
                }
            ));
            state.run(Action::Escape);
            assert!(!state.export_queue_modal.peek());
            assert_eq!(state.export_queue.peek().active().unwrap().id, active.id);
        });
    }

    #[test]
    fn export_dialog_keeps_inside_clicks_and_switches_pages() {
        let state = UiState::new();
        assert!(state.add_title());
        state.export_dialog();
        let viewport = Size {
            width: 1280.0,
            height: 760.0,
        };
        creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(creamui::ThemeProvider::new(creamui::Theme::dark()));
            let mut painter = TestPainter::default();
            let scene = Renderer::new().render(build(&state, viewport), viewport, &mut painter);
            scene
                .hit_test(Point { x: 650.0, y: 140.0 })
                .expect("modal header must capture pointer")();
            assert!(state.export_modal.peek());
            let video = painter
                .bounds
                .iter()
                .rev()
                .find(|(label, rect)| {
                    label == "Video" && rect.x > 450.0 && rect.x < 650.0 && rect.y < 250.0
                })
                .unwrap()
                .1;
            scene
                .hit_test(Point {
                    x: video.x + 2.0,
                    y: video.y + video.height / 2.0,
                })
                .expect("video tab must be clickable")();
            assert!(state.export_modal.peek());
            assert!(state.export_page.peek() == ExportPage::Video);
            let mut painter = TestPainter::default();
            let scene = Renderer::new().render(build(&state, viewport), viewport, &mut painter);
            let webm = painter
                .bounds
                .iter()
                .rev()
                .find(|(label, rect)| label == "WebM" && rect.x > 400.0)
                .unwrap()
                .1;
            scene
                .hit_test(Point {
                    x: webm.x - 18.0,
                    y: webm.y + webm.height / 2.0,
                })
                .expect("format option must be clickable beyond its text")();
            assert!(state.export_modal.peek());
            assert!(state.export_draft.peek().container == Container::WebM);
            state.export_width.set_value("not a width");
            assert!(state.export_form().is_err());
            let mut painter = TestPainter::default();
            let scene = Renderer::new().render(build(&state, viewport), viewport, &mut painter);
            scene
                .hit_test(Point { x: 650.0, y: 140.0 })
                .expect("empty header must capture pointer")();
            assert!(state.export_modal.peek());
            scene
                .hit_test(Point { x: 8.0, y: 8.0 })
                .expect("backdrop must close modal")();
            assert!(!state.export_modal.peek());
        });
    }

    #[test]
    fn frame_rates_drop_redundant_decimals() {
        assert_eq!(frame_rate_label((30, 1)), "30 fps");
        assert_eq!(frame_rate_label((30_000, 1_001)), "29.97 fps");
        assert_eq!(frame_rate_label((25, 2)), "12.5 fps");
        assert_eq!(seconds(375, (30, 1)), "12.5s");
    }

    #[test]
    fn timeline_places_clips_by_start_frame_and_follows_later_footage() {
        let state = UiState::new();
        assert!(state.add_title());
        assert!(state.add_title());
        assert!(state.edit(Command::Move {
            track_id: 0,
            target_track_id: 0,
            clip_id: 2,
            to_frame: 150,
        }));
        state.select_clip(None);
        let painter = render(&state, 900.0, 760.0);
        let first = x_of(&painter, "Title 1").unwrap();
        let second = x_of(&painter, "Title 2").unwrap();
        assert!(second - first > 300.0);

        assert!(state.add_title());
        assert!(state.add_title());
        assert_eq!(state.timeline_start.peek(), 300);
        let painter = render(&state, 900.0, 760.0);
        assert!(painter.labels.iter().any(|label| label == "Title 4"));
        assert!(!painter.labels.iter().any(|label| label == "Title 1"));
    }
}
