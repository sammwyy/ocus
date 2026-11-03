use super::{Sidebar, UiState};
use crate::export::ExportControl;
use crate::media::MediaKind;
use crate::project::Command;
use crate::timeline::{Clip, ClipContent, Title, TrackKind};
use creamui::core::layout::{Dimension, Style};
use creamui::core::{Painter, Point, Rect, RgbaImage, TextAlign, Widget};
use creamui::widgets::layout::{Align, Flex};
use creamui::widgets::{
    AppTitlebar, Button, ButtonSize, ButtonState, ButtonVariant, RawScrollView, RawText,
    TextController, TextInput,
};
use creamui::{BoxedWidget, Color, Size, Styled};
use std::sync::Arc;

pub(crate) const BACKGROUND: Color = Color::rgb(10, 13, 17);
const PANEL: Color = Color::rgb(22, 27, 33);
const SURFACE: Color = Color::rgb(33, 40, 48);
const STAGE: Color = Color::rgb(6, 9, 13);
const TEXT: Color = Color::rgb(235, 240, 242);
const MUTED: Color = Color::rgb(139, 153, 164);
const ACCENT: Color = Color::rgb(114, 208, 193);
const AUDIO: Color = Color::rgb(226, 167, 110);

struct TimelineRuler {
    start: u64,
    span: u64,
    fps: f64,
    playhead: u64,
}

impl Widget for TimelineRuler {
    fn style(&self) -> creamui::Style {
        Style {
            size: creamui::core::layout::Size {
                width: Dimension::Percent(1.0),
                height: Dimension::Length(25.0),
            },
            ..Default::default()
        }
        .into()
    }

    fn paint(&self, painter: &mut dyn Painter, rect: Rect) {
        painter.fill_rect(rect, SURFACE, 4.0);
        let left = rect.x + 100.0;
        let width = (rect.width - 112.0).max(1.0);
        for second in 0..=10 {
            let x = left + width * second as f32 / 10.0;
            painter.stroke_line(
                Point {
                    x,
                    y: rect.y + 17.0,
                },
                Point {
                    x,
                    y: rect.y + if second % 2 == 0 { 24.0 } else { 21.0 },
                },
                MUTED,
                1.0,
            );
            if second % 2 == 0 {
                let time = (self.start as f64 / self.fps + second as f64).floor() as u64;
                painter.fill_text(
                    Rect {
                        x: if second == 10 { x - 40.0 } else { x + 3.0 },
                        y: rect.y,
                        width: 40.0,
                        height: 16.0,
                    },
                    &format!("{:02}:{:02}", time / 60, time % 60),
                    MUTED,
                    10.0,
                    TextAlign::Start,
                );
            }
        }
        if self.playhead >= self.start && self.playhead < self.start.saturating_add(self.span) {
            let x = left + width * (self.playhead - self.start) as f32 / self.span as f32;
            painter.fill_rect(
                Rect {
                    x: x - 1.0,
                    y: rect.y + 15.0,
                    width: 2.0,
                    height: 10.0,
                },
                ACCENT,
                1.0,
            );
        }
    }
}

struct ExportMeter {
    control: Arc<ExportControl>,
    total: u64,
}

struct MediaArtwork {
    image: Option<Arc<RgbaImage>>,
    kind: MediaKind,
}

impl Widget for MediaArtwork {
    fn style(&self) -> creamui::Style {
        Style {
            size: creamui::core::layout::Size {
                width: Dimension::Percent(1.0),
                height: Dimension::Length(92.0),
            },
            ..Default::default()
        }
        .into()
    }

    fn paint(&self, painter: &mut dyn Painter, rect: Rect) {
        painter.fill_rect(rect, STAGE, 6.0);
        if let Some(image) = &self.image {
            let scale =
                (rect.width / image.width() as f32).min(rect.height / image.height() as f32);
            let width = image.width() as f32 * scale;
            let height = image.height() as f32 * scale;
            painter.draw_image(
                Rect {
                    x: rect.x + (rect.width - width) / 2.0,
                    y: rect.y + (rect.height - height) / 2.0,
                    width,
                    height,
                },
                image,
                None,
            );
        } else {
            painter.fill_text(
                rect,
                match self.kind {
                    MediaKind::Video => "VIDEO SOURCE",
                    MediaKind::Audio => "AUDIO SOURCE",
                    MediaKind::Image => "STILL IMAGE",
                },
                MUTED,
                11.0,
                TextAlign::Center,
            );
        }
    }
}

impl Widget for ExportMeter {
    fn style(&self) -> creamui::Style {
        Style {
            size: creamui::core::layout::Size {
                width: Dimension::Percent(1.0),
                height: Dimension::Length(5.0),
            },
            ..Default::default()
        }
        .into()
    }

    fn paint(&self, painter: &mut dyn Painter, rect: Rect) {
        let _ = painter.animation_time();
        painter.fill_rect(rect, SURFACE, 2.5);
        painter.fill_rect(
            Rect {
                width: rect.width
                    * (self.control.frames_done() as f32 / self.total.max(1) as f32).min(1.0),
                ..rect
            },
            ACCENT,
            2.5,
        );
    }
}

fn text(value: impl Into<String>, color: Color, size: f32) -> BoxedWidget {
    Box::new(RawText::new(value, color, size))
}

fn heading(value: impl Into<String>, color: Color, size: f32) -> BoxedWidget {
    Box::new(
        RawText::unstyled(value).with_style(
            creamui::Style::new()
                .color(color)
                .font_size(size)
                .bold(true),
        ),
    )
}

fn panel(title: &str, subtitle: &str, content: BoxedWidget) -> Flex {
    Flex::column()
        .gap(10.0)
        .padding(12.0)
        .background(PANEL)
        .corner_radius(10.0)
        .child(Box::new(
            Flex::row()
                .gap(8.0)
                .align(Align::Center)
                .child(heading(title, TEXT, 13.0))
                .child(text(subtitle, MUTED, 10.0)),
        ))
        .child(content)
}

fn add_title(state: &UiState) -> bool {
    let editor = state.editor.borrow();
    let project = editor.project();
    let Some(track) = project.tracks().iter().find(|t| t.kind == TrackKind::Video) else {
        return false;
    };
    let track_id = track.id;
    let Some(start_frame) = track
        .clips
        .iter()
        .map(|clip| clip.start_frame.checked_add(clip.duration_frames))
        .try_fold(0_u64, |latest, end| Some(latest.max(end?)))
    else {
        return false;
    };
    let Some(id) = project
        .tracks()
        .iter()
        .flat_map(|track| &track.clips)
        .map(|clip| clip.id)
        .max()
        .unwrap_or(0)
        .checked_add(1)
    else {
        return false;
    };
    let command = Command::InsertClip {
        track_id,
        clip: Clip {
            id,
            start_frame,
            source_start_frame: 0,
            duration_frames: (project.settings().frame_rate.numerator as u64).saturating_mul(3)
                / project.settings().frame_rate.denominator as u64,
            content: ClipContent::Title(Title {
                text: format!("Title {id}"),
                font_family: "Sans".into(),
                font_size: 64.0,
                color: [255, 255, 255, 255],
                x: 0.5,
                y: 0.5,
            }),
            volume: 1.0,
            muted: false,
        },
    };
    drop(editor);
    if state.edit(command) {
        state.selected.set(Some((track_id, id)));
        state.sidebar.set(Sidebar::Inspector);
        state.title_text.set_value(format!("Title {id}"));
        state.focus_frame(start_frame);
        true
    } else {
        false
    }
}

fn action(
    label: &str,
    enabled: bool,
    state: &UiState,
    operation: impl Fn(&UiState) + 'static,
) -> BoxedWidget {
    styled_action(label, ButtonVariant::Tertiary, enabled, state, operation)
}

fn styled_action(
    label: &str,
    variant: ButtonVariant,
    enabled: bool,
    state: &UiState,
    operation: impl Fn(&UiState) + 'static,
) -> BoxedWidget {
    let state = state.clone();
    Box::new(
        Button::styled(
            variant,
            ButtonSize::Sm,
            label,
            ButtonState::Normal,
            move || operation(&state),
        )
        .disabled(!enabled),
    )
}

fn input(controller: &TextController) -> BoxedWidget {
    Box::new(TextInput::controlled(controller).width(190.0))
}

fn selected_clip(state: &UiState) -> Option<(u64, Clip)> {
    let (track_id, clip_id) = state.selected.get()?;
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
    state.edit(edit(&clip, track_id, next_id));
}

fn set_title_text(state: &UiState) {
    let Some((track_id, selected)) = selected_clip(state) else {
        return;
    };
    let mut tracks = state.editor.borrow().project().tracks().to_vec();
    if let Some(clip) = tracks
        .iter_mut()
        .find(|track| track.id == track_id)
        .and_then(|track| track.clips.iter_mut().find(|clip| clip.id == selected.id))
        && let ClipContent::Title(title) = &mut clip.content
    {
        title.text = state.title_text.peek();
        state.edit(Command::ReplaceTracks(tracks));
    }
}

pub(crate) fn build(state: &UiState, viewport: Size) -> BoxedWidget {
    state.revision.get();
    state.ui_revision.get();
    let editor = state.editor.borrow();
    let project = editor.project();
    let compact = viewport.width < 1200.0;
    let busy = state.busy.get();
    let current_path = state.project_path.get();
    let dirty = state.dirty.get();
    let project_label = current_path
        .as_ref()
        .and_then(|path| path.file_name())
        .map_or_else(
            || project.name().to_owned(),
            |name| name.to_string_lossy().into_owned(),
        );
    let titlebar = AppTitlebar::new(viewport.width)
        .height(46.0)
        .padding(16.0)
        .center_margin(48.0)
        .foreground(TEXT)
        .background(PANEL)
        .start_boxed(Box::new(
            Flex::row()
                .gap(9.0)
                .align(Align::Center)
                .child(heading("OCUS", ACCENT, 16.0))
                .child(text("EDIT  /  COMPOSE", MUTED, 10.0)),
        ))
        .center_boxed(text(
            format!("{project_label}{}", if dirty { "  •  Unsaved" } else { "" }),
            TEXT,
            12.0,
        ));

    let files = project
        .media()
        .iter()
        .map(|media| {
            let name = media.path.file_name().map_or_else(
                || media.path.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            );
            Box::new(
                Flex::column()
                    .gap(7.0)
                    .padding(10.0)
                    .background(SURFACE)
                    .corner_radius(6.0)
                    .child(Box::new(MediaArtwork {
                        image: state.thumbnails.borrow().get(&media.path).cloned(),
                        kind: media.kind,
                    }))
                    .child(text(name.chars().take(24).collect::<String>(), TEXT, 12.0))
                    .child(text(
                        format!(
                            "{:?} · {}",
                            media.kind,
                            media.duration_us.map_or_else(
                                || "still image · 3s default".into(),
                                |duration| format!("{:.1}s", duration as f64 / 1_000_000.0)
                            )
                        ),
                        MUTED,
                        10.0,
                    ))
                    .child(action(
                        "Add to timeline",
                        media.duration_us.is_some() || media.kind == crate::media::MediaKind::Image,
                        state,
                        {
                            let id = media.id;
                            move |state| state.insert_media(id)
                        },
                    )),
            ) as BoxedWidget
        })
        .collect();
    let media_content = Flex::column()
        .gap(10.0)
        .grow(1.0)
        .child(styled_action(
            "Import media",
            ButtonVariant::Primary,
            !busy,
            state,
            UiState::import_dialog,
        ))
        .child(text(
            format!("{} sources in this project", project.media().len()),
            MUTED,
            11.0,
        ))
        .child(Box::new(
            RawScrollView::controlled(
                Style {
                    size: creamui::core::layout::Size {
                        width: Dimension::Percent(1.0),
                        height: Dimension::Percent(1.0),
                    },
                    flex_grow: 1.0,
                    ..Default::default()
                },
                state.media_scroll.clone(),
            )
            .child(Box::new(if project.media().is_empty() {
                Flex::column()
                    .gap(7.0)
                    .padding(12.0)
                    .background(SURFACE)
                    .corner_radius(6.0)
                    .child(text("Your media bin is empty", TEXT, 12.0))
                    .child(text(
                        "Import video, audio or still images to begin.",
                        MUTED,
                        11.0,
                    ))
            } else {
                Flex::column().gap(6.0).with_children(files)
            })),
        ));
    let media = panel(
        "Media library",
        "SOURCE FILES",
        Box::new(media_content.child(text(
            "Sources stay on disk; Ocus never copies them.",
            MUTED,
            10.0,
        ))),
    )
    .width(if compact { 248.0 } else { 264.0 });
    let frame = state.playhead.get();
    let fps = project.settings().frame_rate.numerator as f64
        / project.settings().frame_rate.denominator as f64;
    let page_span = (u64::from(project.settings().frame_rate.numerator) * 10
        / u64::from(project.settings().frame_rate.denominator))
    .max(1);
    let timeline_end = project
        .tracks()
        .iter()
        .flat_map(|track| &track.clips)
        .filter_map(|clip| clip.start_frame.checked_add(clip.duration_frames))
        .max()
        .unwrap_or(0);
    let title_at_frame = project
        .tracks()
        .iter()
        .filter(|track| track.kind == TrackKind::Video)
        .flat_map(|track| &track.clips)
        .find_map(|clip| {
            (clip.start_frame <= frame
                && clip
                    .start_frame
                    .checked_add(clip.duration_frames)
                    .is_some_and(|end| frame < end))
            .then_some(&clip.content)
            .and_then(|content| match content {
                ClipContent::Title(title) => Some(title.text.as_str()),
                _ => None,
            })
        });
    let media_at_frame = project.tracks().iter().any(|track| {
        track.kind == TrackKind::Video
            && track.clips.iter().any(|clip| {
                clip.start_frame <= frame
                    && clip
                        .start_frame
                        .checked_add(clip.duration_frames)
                        .is_some_and(|end| frame < end)
                    && matches!(clip.content, ClipContent::Media { .. })
            })
    });
    let empty_timeline = project.tracks().iter().all(|track| track.clips.is_empty());
    let first_media = project.media().first();
    let preview = panel(
        "Program monitor",
        "TEXT PREVIEW",
        Box::new(
            Flex::column()
                .gap(8.0)
                .grow(1.0)
                .child(Box::new(
                    Flex::column()
                        .grow(1.0)
                        .align(Align::Center)
                        .gap(12.0)
                        .padding(22.0)
                        .background(STAGE)
                        .corner_radius(8.0)
                        .child(text(
                            if media_at_frame {
                                "SOURCE PREVIEW"
                            } else {
                                "OCUS  /  MONITOR"
                            },
                            MUTED,
                            10.0,
                        ))
                        .child(text(
                            title_at_frame.unwrap_or(if media_at_frame {
                                "Video preview is not available"
                            } else if empty_timeline && first_media.is_some() {
                                "Footage is ready"
                            } else if empty_timeline {
                                "Start your edit"
                            } else {
                                "No clip at this frame"
                            }),
                            TEXT,
                            23.0,
                        ))
                        .child(text(
                            if empty_timeline {
                                if first_media.is_some() {
                                    "Add a source to the timeline to start editing."
                                } else {
                                    "Import footage, then add it to the timeline."
                                }
                            } else {
                                "Titles are text-only. Video playback is not available yet."
                            },
                            MUTED,
                            11.0,
                        ))
                        .child(
                            if let Some(media) = first_media.filter(|_| empty_timeline) {
                                styled_action(
                                    "Add first source",
                                    ButtonVariant::Primary,
                                    !busy
                                        && (media.duration_us.is_some()
                                            || media.kind == MediaKind::Image),
                                    state,
                                    {
                                        let id = media.id;
                                        move |state| state.insert_media(id)
                                    },
                                )
                            } else if empty_timeline {
                                styled_action(
                                    "Import media",
                                    ButtonVariant::Primary,
                                    !busy,
                                    state,
                                    UiState::import_dialog,
                                )
                            } else {
                                Box::new(Flex::row().height(0.0))
                            },
                        ),
                ))
                .child(Box::new(
                    Flex::row()
                        .gap(8.0)
                        .align(Align::Center)
                        .child(action("Previous frame", frame > 0, state, |s| {
                            s.focus_frame(s.playhead.peek().saturating_sub(1));
                        }))
                        .child(text(format!("Frame {frame}"), ACCENT, 12.0))
                        .child(action(
                            "Next frame",
                            frame.saturating_add(1) < timeline_end,
                            state,
                            |s| {
                                s.focus_frame(s.playhead.peek().saturating_add(1));
                            },
                        ))
                        .child(text(
                            format!(
                                "{} × {}",
                                project.settings().width,
                                project.settings().height
                            ),
                            MUTED,
                            11.0,
                        )),
                )),
        ),
    )
    .grow(1.0);
    let selected = selected_clip(state);
    let inspector_content: BoxedWidget = if let Some((track_id, clip)) = selected {
        let clip_id = clip.id;
        let mut content = Flex::column()
            .gap(5.0)
            .child(text(
                format!("CLIP {clip_id} · FRAME {}", clip.start_frame),
                ACCENT,
                11.0,
            ))
            .child(text(
                format!("Duration: {} frames", clip.duration_frames),
                MUTED,
                11.0,
            ));
        if let ClipContent::Media { media_id } = clip.content
            && let Some(media) = project.media().iter().find(|media| media.id == media_id)
        {
            content = content.child(text(
                media
                    .path
                    .file_name()
                    .map_or_else(
                        || "Media source".into(),
                        |name| name.to_string_lossy().into_owned(),
                    )
                    .chars()
                    .take(28)
                    .collect::<String>(),
                TEXT,
                11.0,
            ));
        }
        if matches!(clip.content, ClipContent::Title(_)) {
            content = content.child(input(&state.title_text)).child(action(
                "Apply title",
                true,
                state,
                set_title_text,
            ));
        }
        if project
            .tracks()
            .iter()
            .any(|track| track.id == track_id && track.kind == TrackKind::Audio)
            && matches!(clip.content, ClipContent::Media { .. })
        {
            content = content
                .child(text(
                    format!(
                        "Audio: {}%{}",
                        (clip.volume * 100.0).round() as u32,
                        if clip.muted { " · muted" } else { "" }
                    ),
                    AUDIO,
                    11.0,
                ))
                .child(Box::new(
                    Flex::row()
                        .gap(5.0)
                        .child(action("−10%", clip.volume > 0.0, state, move |state| {
                            if let Some((_, clip)) = selected_clip(state) {
                                state.edit(Command::SetClipAudio {
                                    track_id,
                                    clip_id,
                                    volume: ((clip.volume * 10.0).round() - 1.0).max(0.0) / 10.0,
                                    muted: clip.muted,
                                });
                            }
                        }))
                        .child(action("+10%", clip.volume < 2.0, state, move |state| {
                            if let Some((_, clip)) = selected_clip(state) {
                                state.edit(Command::SetClipAudio {
                                    track_id,
                                    clip_id,
                                    volume: ((clip.volume * 10.0).round() + 1.0).min(20.0) / 10.0,
                                    muted: clip.muted,
                                });
                            }
                        })),
                ))
                .child(action(
                    if clip.muted { "Unmute" } else { "Mute" },
                    true,
                    state,
                    move |state| {
                        if let Some((_, clip)) = selected_clip(state) {
                            state.edit(Command::SetClipAudio {
                                track_id,
                                clip_id,
                                volume: clip.volume,
                                muted: !clip.muted,
                            });
                        }
                    },
                ))
                .child(text("Settings saved; audio playback pending", MUTED, 10.0));
        }
        content = content
            .child(action("Move to other track", true, state, |s| {
                edit_selection(s, |clip, track_id, _| {
                    let editor = s.editor.borrow();
                    let tracks = editor.project().tracks();
                    let kind = tracks
                        .iter()
                        .find(|track| track.id == track_id)
                        .map_or(TrackKind::Video, |track| track.kind);
                    let target_track_id = tracks
                        .iter()
                        .find(|track| track.id != track_id && track.kind == kind)
                        .map_or(track_id, |track| track.id);
                    Command::Move {
                        track_id,
                        target_track_id,
                        clip_id: clip.id,
                        to_frame: clip.start_frame,
                    }
                });
            }))
            .child(action(
                "Split at playhead",
                state.playhead.get() > clip.start_frame
                    && state.playhead.peek()
                        < clip.start_frame.saturating_add(clip.duration_frames),
                state,
                |s| {
                    edit_selection(s, |clip, track_id, next_id| Command::Split {
                        track_id,
                        clip_id: clip.id,
                        at_frame: s.playhead.peek(),
                        new_id: next_id,
                    });
                },
            ))
            .child(Box::new(
                Flex::row()
                    .gap(5.0)
                    .child(action("← Move", clip.start_frame > 0, state, |s| {
                        edit_selection(s, |clip, track_id, _| Command::Move {
                            track_id,
                            target_track_id: track_id,
                            clip_id: clip.id,
                            to_frame: clip.start_frame.saturating_sub(1),
                        });
                    }))
                    .child(action("Move →", true, state, |s| {
                        edit_selection(s, |clip, track_id, _| Command::Move {
                            track_id,
                            target_track_id: track_id,
                            clip_id: clip.id,
                            to_frame: clip.start_frame.saturating_add(1),
                        });
                    })),
            ))
            .child(Box::new(
                Flex::row()
                    .gap(5.0)
                    .child(action("Trim −", clip.duration_frames > 1, state, |s| {
                        edit_selection(s, |clip, track_id, _| Command::Trim {
                            track_id,
                            clip_id: clip.id,
                            start_frame: clip.start_frame,
                            source_start_frame: clip.source_start_frame,
                            duration_frames: clip.duration_frames.saturating_sub(1),
                        });
                    }))
                    .child(action("Trim +", true, state, |s| {
                        edit_selection(s, |clip, track_id, _| Command::Trim {
                            track_id,
                            clip_id: clip.id,
                            start_frame: clip.start_frame,
                            source_start_frame: clip.source_start_frame,
                            duration_frames: clip.duration_frames.saturating_add(1),
                        });
                    })),
            ))
            .child(action("Delete clip", true, state, move |s| {
                if s.edit(Command::RemoveClip { track_id, clip_id }) {
                    s.selected.set(None);
                }
            }))
            .child(action("Ripple delete", true, state, move |s| {
                if s.edit(Command::RippleDelete { track_id, clip_id }) {
                    s.selected.set(None);
                }
            }));
        Box::new(content)
    } else {
        Box::new(
            Flex::column()
                .gap(12.0)
                .padding(8.0)
                .child(heading(
                    if empty_timeline && first_media.is_none() {
                        "Prepare your sequence"
                    } else if empty_timeline {
                        "Build your timeline"
                    } else {
                        "Choose a clip"
                    },
                    TEXT,
                    13.0,
                ))
                .child(text(
                    if empty_timeline && first_media.is_none() {
                        "1  Import video, audio or stills."
                    } else if empty_timeline {
                        "1  Your media is ready."
                    } else {
                        "Select a clip on the timeline."
                    },
                    MUTED,
                    11.0,
                ))
                .child(text(
                    if empty_timeline {
                        "2  Add a source to the timeline."
                    } else {
                        "Use the inspector to edit it."
                    },
                    MUTED,
                    11.0,
                ))
                .child(text(
                    "Preview of video frames is not available yet.",
                    MUTED,
                    10.0,
                ))
                .child(text(
                    format!(
                        "{} × {}  ·  {} / {} fps",
                        project.settings().width,
                        project.settings().height,
                        project.settings().frame_rate.numerator,
                        project.settings().frame_rate.denominator
                    ),
                    MUTED,
                    10.0,
                )),
        )
    };
    let inspector = panel(
        "Inspector",
        "SELECTED CLIP",
        Box::new(
            RawScrollView::controlled(
                Style {
                    size: creamui::core::layout::Size {
                        width: Dimension::Percent(1.0),
                        height: Dimension::Percent(1.0),
                    },
                    flex_grow: 1.0,
                    ..Default::default()
                },
                state.inspector_scroll.clone(),
            )
            .child(inspector_content),
        ),
    )
    .width(if compact { 248.0 } else { 276.0 });

    let last_page = timeline_end.saturating_sub(1) / page_span;
    let page = state.timeline_page.get().min(last_page);
    let visible_start = page.saturating_mul(page_span);
    let visible_end = visible_start.saturating_add(page_span);
    let track_width = (viewport.width - 160.0).max(260.0);
    let px_per_frame = track_width / page_span as f32;
    let mut video = 0;
    let mut audio = 0;
    let tracks: Vec<BoxedWidget> = project
        .tracks()
        .iter()
        .map(|track| {
            let (label, color) = match track.kind {
                TrackKind::Video => {
                    video += 1;
                    (format!("V{video}   VIDEO"), ACCENT)
                }
                TrackKind::Audio => {
                    audio += 1;
                    (format!("A{audio}   AUDIO"), AUDIO)
                }
            };
            let mut cursor = visible_start;
            let mut clips: Vec<BoxedWidget> = Vec::new();
            for clip in &track.clips {
                let end = clip.start_frame.saturating_add(clip.duration_frames);
                if end <= visible_start || clip.start_frame >= visible_end {
                    continue;
                }
                let start = clip.start_frame.max(visible_start);
                let end = end.min(visible_end);
                if start > cursor {
                    clips.push(Box::new(
                        Flex::row().width((start - cursor) as f32 * px_per_frame),
                    ));
                }
                let name = match &clip.content {
                    ClipContent::Title(title) => title.text.clone(),
                    ClipContent::Media { media_id } => project
                        .media()
                        .iter()
                        .find(|media| media.id == *media_id)
                        .and_then(|media| media.path.file_name())
                        .map_or_else(
                            || format!("Media {media_id}"),
                            |name| name.to_string_lossy().into_owned(),
                        ),
                };
                let clip_id = clip.id;
                let track_id = track.id;
                let start_frame = start;
                let title = match &clip.content {
                    ClipContent::Title(title) => title.text.clone(),
                    _ => String::new(),
                };
                let state = state.clone();
                let width = (end - start) as f32 * px_per_frame;
                let label = if width >= 220.0 {
                    format!(
                        "{} · {:.1}s",
                        name.chars().take(16).collect::<String>(),
                        clip.duration_frames as f64 / fps
                    )
                } else if width >= 120.0 {
                    name.chars().take(12).collect()
                } else if width >= 70.0 {
                    format!("{}…", name.chars().take(5).collect::<String>())
                } else {
                    String::new()
                };
                clips.push(Box::new(
                    Button::styled(
                        if state.selected.peek() == Some((track_id, clip_id)) {
                            ButtonVariant::Primary
                        } else {
                            ButtonVariant::Secondary
                        },
                        ButtonSize::Sm,
                        label,
                        ButtonState::Normal,
                        move || {
                            state.selected.set(Some((track_id, clip_id)));
                            state.sidebar.set(Sidebar::Inspector);
                            state.title_text.set_value(title.clone());
                            state.focus_frame(start_frame);
                        },
                    )
                    .width(width),
                ) as BoxedWidget);
                cursor = end;
            }
            Box::new(
                Flex::row()
                    .height(46.0)
                    .gap(8.0)
                    .align(Align::Center)
                    .background(SURFACE)
                    .corner_radius(5.0)
                    .child(Box::new(
                        Flex::row()
                            .width(92.0)
                            .padding(9.0)
                            .child(text(label, color, 11.0)),
                    ))
                    .child(Box::new(Flex::row().with_children(if clips.is_empty() {
                        vec![text("No clips in this range", MUTED, 11.0)]
                    } else {
                        clips
                    }))),
            ) as BoxedWidget
        })
        .collect();
    let timeline = Flex::column()
        .height((viewport.height * 0.41).clamp(240.0, 345.0))
        .gap(8.0)
        .padding(12.0)
        .background(PANEL)
        .corner_radius(10.0)
        .child(Box::new(
            Flex::row()
                .gap(10.0)
                .align(Align::Center)
                .child(heading("Timeline", TEXT, 13.0))
                .child(text(
                    format!(
                        "{} / {} fps",
                        project.settings().frame_rate.numerator,
                        project.settings().frame_rate.denominator
                    ),
                    MUTED,
                    11.0,
                ))
                .child(text(
                    format!("{} tracks", project.tracks().len()),
                    MUTED,
                    11.0,
                ))
                .child(action("Earlier", page > 0, state, |state| {
                    state
                        .timeline_page
                        .update(|page| *page = page.saturating_sub(1));
                }))
                .child(text(
                    format!(
                        "{:02}:{:02} – {:02}:{:02}",
                        (visible_start as f64 / fps / 60.0) as u64,
                        (visible_start as f64 / fps) as u64 % 60,
                        (visible_end as f64 / fps / 60.0) as u64,
                        (visible_end as f64 / fps) as u64 % 60
                    ),
                    ACCENT,
                    11.0,
                ))
                .child(action("Later", page < last_page, state, |state| {
                    state
                        .timeline_page
                        .update(|page| *page = page.saturating_add(1));
                })),
        ))
        .child(Box::new(TimelineRuler {
            start: visible_start,
            span: page_span,
            fps,
            playhead: frame,
        }))
        .child(Box::new(
            RawScrollView::controlled(
                Style {
                    size: creamui::core::layout::Size {
                        width: Dimension::Percent(1.0),
                        height: Dimension::Percent(1.0),
                    },
                    flex_grow: 1.0,
                    ..Default::default()
                },
                state.timeline_scroll.clone(),
            )
            .child(Box::new(Flex::column().gap(5.0).with_children(tracks))),
        ));

    let workspace: BoxedWidget = if compact {
        let active = state.sidebar.get();
        let sidebar = Box::new(
            Flex::column()
                .width(248.0)
                .gap(6.0)
                .child(Box::new(
                    Flex::row()
                        .gap(6.0)
                        .child(styled_action(
                            "Media",
                            if active == Sidebar::Media {
                                ButtonVariant::Primary
                            } else {
                                ButtonVariant::Tertiary
                            },
                            true,
                            state,
                            |state| state.sidebar.set(Sidebar::Media),
                        ))
                        .child(styled_action(
                            "Inspector",
                            if active == Sidebar::Inspector {
                                ButtonVariant::Primary
                            } else {
                                ButtonVariant::Tertiary
                            },
                            true,
                            state,
                            |state| state.sidebar.set(Sidebar::Inspector),
                        )),
                ))
                .child(if active == Sidebar::Media {
                    Box::new(media) as BoxedWidget
                } else {
                    Box::new(inspector) as BoxedWidget
                }),
        ) as BoxedWidget;
        Box::new(
            Flex::row()
                .gap(8.0)
                .grow(1.0)
                .child(sidebar)
                .child(Box::new(preview)),
        )
    } else {
        Box::new(
            Flex::row()
                .gap(8.0)
                .grow(1.0)
                .child(Box::new(media))
                .child(Box::new(preview))
                .child(Box::new(inspector)),
        )
    };

    let file_actions: BoxedWidget = Box::new(
        Flex::row()
            .gap(6.0)
            .align(Align::Center)
            .child(action("New", !busy, state, UiState::new_project))
            .child(action("Open project", !busy, state, UiState::open_dialog))
            .child(action("Save", !busy, state, UiState::save))
            .child(action("Save as", !busy, state, UiState::save_as_dialog)),
    );
    let edit_actions: BoxedWidget = Box::new(
        Flex::row()
            .gap(6.0)
            .align(Align::Center)
            .child(styled_action(
                "Import media",
                ButtonVariant::Secondary,
                !busy,
                state,
                UiState::import_dialog,
            ))
            .child(action("Add title", !busy, state, |state| {
                add_title(state);
            }))
            .child(action(
                "Undo",
                !busy && editor.can_undo(),
                state,
                UiState::undo,
            ))
            .child(action(
                "Redo",
                !busy && editor.can_redo(),
                state,
                UiState::redo,
            ))
            .child(if state.exporting.get() {
                styled_action(
                    "Cancel export",
                    ButtonVariant::Secondary,
                    true,
                    state,
                    UiState::cancel_export,
                )
            } else {
                styled_action(
                    "Export silent MP4",
                    ButtonVariant::Primary,
                    !busy,
                    state,
                    UiState::export_dialog,
                )
            }),
    );
    let toolbar: BoxedWidget = if viewport.width < 850.0 {
        Box::new(
            Flex::column()
                .gap(5.0)
                .padding(8.0)
                .background(PANEL)
                .corner_radius(8.0)
                .child(file_actions)
                .child(edit_actions),
        )
    } else {
        Box::new(
            Flex::row()
                .gap(16.0)
                .align(Align::Center)
                .padding(8.0)
                .background(PANEL)
                .corner_radius(8.0)
                .child(file_actions)
                .child(edit_actions),
        )
    };
    let mut footer = Flex::column().gap(4.0).child(Box::new(
        Flex::row()
            .height(22.0)
            .gap(12.0)
            .child(text(
                if busy {
                    "WORKING"
                } else if dirty {
                    "UNSAVED"
                } else {
                    "READY"
                },
                ACCENT,
                10.0,
            ))
            .child(text(state.status.get(), MUTED, 11.0)),
    ));
    if state.exporting.get()
        && let Some(control) = state.export_control()
    {
        footer = footer.child(Box::new(ExportMeter {
            control,
            total: state.export_total.get(),
        }));
    }

    Box::new(
        Flex::column()
            .size(viewport.width, viewport.height)
            .gap(8.0)
            .background(BACKGROUND)
            .child(Box::new(titlebar))
            .child(Box::new(
                Flex::column()
                    .gap(8.0)
                    .padding_xy(10.0, 2.0)
                    .grow(1.0)
                    .child(toolbar)
                    .child(workspace)
                    .child(Box::new(timeline))
                    .child(Box::new(footer)),
            )),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use creamui::core::{Painter, Rect, Renderer, TextAlign};

    #[derive(Default)]
    struct TestPainter {
        labels: Vec<String>,
        positions: Vec<(String, f32)>,
    }

    impl Painter for TestPainter {
        fn fill_rect(&mut self, _: Rect, _: Color, _: f32) {}

        fn stroke_rect(&mut self, _: Rect, _: Color, _: f32, _: f32) {}

        fn fill_text(&mut self, rect: Rect, text: &str, _: Color, _: f32, _: TextAlign) {
            self.labels.push(text.to_owned());
            self.positions.push((text.to_owned(), rect.x));
        }
    }

    #[test]
    fn editor_renders_empty_and_populated_tracks() {
        let state = UiState::new();
        for width in [1440.0, 900.0] {
            let viewport = Size {
                width,
                height: 900.0,
            };
            let mut painter = TestPainter::default();
            creamui::reactive::with_context_scope(|| {
                creamui::reactive::provide_context(creamui::theme::ThemeProvider::new(
                    creamui::Theme::dark(),
                ));
                Renderer::new().render(build(&state, viewport), viewport, &mut painter);
            });
            for label in [
                "OCUS",
                "Media library",
                "Program monitor",
                "Timeline",
                "00:00",
            ] {
                assert!(painter.labels.iter().any(|text| text == label), "{label}");
            }
            for label in ["V1", "V2", "A1", "A2"] {
                assert!(painter.labels.iter().any(|text| text.starts_with(label)));
            }
        }
        assert!(add_title(&state));
        let viewport = Size {
            width: 1440.0,
            height: 900.0,
        };
        let mut painter = TestPainter::default();
        creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(creamui::theme::ThemeProvider::new(
                creamui::Theme::dark(),
            ));
            Renderer::new().render(build(&state, viewport), viewport, &mut painter);
        });
        assert!(painter.labels.iter().any(|text| text == "Title 1"));
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

        assert!(state.edit(Command::ImportMedia(crate::media::Media {
            id: 7,
            path: "footage/tone.wav".into(),
            kind: crate::media::MediaKind::Audio,
            duration_us: Some(1_000_000),
            has_audio: true,
        })));
        state.insert_media(7);
        let mut painter = TestPainter::default();
        creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(creamui::theme::ThemeProvider::new(
                creamui::Theme::dark(),
            ));
            Renderer::new().render(build(&state, viewport), viewport, &mut painter);
        });
        assert!(
            painter
                .labels
                .iter()
                .any(|label| label.contains("tone.wav"))
        );
        assert!(
            painter
                .labels
                .iter()
                .any(|label| label == "Add to timeline")
        );
    }

    #[test]
    fn timeline_places_clips_by_start_frame_and_pages_later_footage() {
        let state = UiState::new();
        assert!(add_title(&state));
        assert!(add_title(&state));
        assert!(state.edit(Command::Move {
            track_id: 0,
            target_track_id: 0,
            clip_id: 2,
            to_frame: 150,
        }));
        let viewport = Size {
            width: 900.0,
            height: 760.0,
        };
        let mut painter = TestPainter::default();
        creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(creamui::theme::ThemeProvider::new(
                creamui::Theme::dark(),
            ));
            Renderer::new().render(build(&state, viewport), viewport, &mut painter);
        });
        let first = painter
            .positions
            .iter()
            .find(|(label, _)| label.starts_with("Title 1 ·"))
            .unwrap()
            .1;
        let second = painter
            .positions
            .iter()
            .find(|(label, _)| label.starts_with("Title 2 ·"))
            .unwrap()
            .1;
        assert!(second - first > 300.0);

        assert!(add_title(&state));
        assert!(add_title(&state));
        assert_eq!(state.timeline_page.peek(), 1);
        let mut painter = TestPainter::default();
        creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(creamui::theme::ThemeProvider::new(
                creamui::Theme::dark(),
            ));
            Renderer::new().render(build(&state, viewport), viewport, &mut painter);
        });
        assert!(
            painter
                .labels
                .iter()
                .any(|label| label.starts_with("Title 4 ·"))
        );
        assert!(
            !painter
                .labels
                .iter()
                .any(|label| label.starts_with("Title 1 ·"))
        );
    }
}
