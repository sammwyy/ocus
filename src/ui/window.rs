use super::controls::{
    ACCENT, AUDIO, BACKGROUND, BORDER, DragOverlay, ExportMeter, FAINT, Glyph, IMAGE, MUTED, PANEL,
    PANEL_RAISED, PLAYHEAD, STAGE, SURFACE, Scrubber, TEXT, TITLE, Thumb, VIDEO, accent_button,
    chip, fit, glyph, heading, icon_button, kind_color, media_item, play_button, secondary_button,
    short_duration, sized_icon_button, text, timecode, tool_button,
};
use super::interaction::DragPayload;
use super::state::{MediaView, SourceRange, ZOOM_SECONDS, media_frames};
use super::timeline_view::{ClipLook, Lane, LaneClip, TimelineView};
use super::{MediaFilter, Sidebar, UiState};
use crate::media::preview::StoryboardKey;
use crate::media::{Media, MediaKind};
use crate::project::{Command, Project};
use crate::timeline::{Clip, ClipContent, TrackKind};
use creamui::core::layout::{Dimension, Style};
use creamui::widgets::layout::Flex;
use creamui::widgets::layout::{Align, Grid, Justify, StyleExt};
use creamui::widgets::{AppTitlebar, RawScrollView, TextController, TextInput};
use creamui::{BoxedWidget, Color, GpuFrameSlot, GpuViewport, Size, Styled};
use std::rc::Rc;

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

fn frame_rate_label(fps: (u32, u32)) -> String {
    let rate = format!("{:.3}", f64::from(fps.0) / f64::from(fps.1.max(1)));
    format!("{} fps", rate.trim_end_matches('0').trim_end_matches('.'))
}

fn track_labels(project: &Project) -> Vec<(u64, String)> {
    let (mut video, mut audio) = (0, 0);
    project
        .tracks()
        .iter()
        .map(|track| {
            let label = if track.kind == TrackKind::Video {
                video += 1;
                format!("V{video}")
            } else {
                audio += 1;
                format!("A{audio}")
            };
            (track.id, label)
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
    Box::new(Flex::row().size(1.0, 18.0).background(BORDER))
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
            .child(heading(title, TEXT, 13.0))
            .child(detail)
            .child(spacer()),
        actions,
    ))
}

fn section(title: &str, rows: Vec<BoxedWidget>) -> BoxedWidget {
    Box::new(append(
        Flex::column()
            .gap(7.0)
            .child(Box::new(
                Flex::row().height(1.0).full_width().background(BORDER),
            ))
            .child(heading(title.to_uppercase(), FAINT, 10.0)),
        rows,
    ))
}

fn property(label: &str, value: impl Into<String>) -> BoxedWidget {
    Box::new(
        Flex::row()
            .gap(8.0)
            .align(Align::Center)
            .child(Box::new(
                Flex::row().width(86.0).child(text(label, MUTED, 11.0)),
            ))
            .child(text(value, TEXT, 11.0)),
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
        .child(glyph(icon, FAINT, 30.0))
        .child(heading(title, TEXT.mix(MUTED, 0.3), 14.0))
        .child(text(detail, MUTED, 11.0));
    if let Some(action) = action {
        content = content
            .child(Box::new(Flex::row().height(4.0)))
            .child(action);
    }
    Box::new(content)
}

fn title_card(title: &str) -> BoxedWidget {
    Box::new(
        Flex::column()
            .gap(6.0)
            .align(Align::Center)
            .child(heading(title, TEXT, 26.0))
            .child(text("Title overlay preview", FAINT, 10.0)),
    )
}

fn monitor_viewport(slot: &GpuFrameSlot, aspect: f32) -> BoxedWidget {
    Box::new(GpuViewport::new(fill_style(), slot.clone()).aspect_ratio(aspect))
}

struct Transport {
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
        position,
        end,
        fps,
        playing,
        can_play,
        color,
        compact,
    } = info;
    let last = end.saturating_sub(1);
    let jump = |icon, enabled, target: u64| {
        let seek = seek.clone();
        icon_button(icon, enabled, state, move |state| seek(state, target))
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
                        (!compact).then(|| jump(Glyph::ToStart, position > 0, 0)),
                        Some(jump(
                            Glyph::StepBack,
                            position > 0,
                            position.saturating_sub(1),
                        )),
                        Some(Box::new(Flex::row().width(4.0))),
                        Some(play_button(playing, can_play || playing, state, toggle)),
                        Some(Box::new(Flex::row().width(4.0))),
                        Some(jump(
                            Glyph::StepForward,
                            position < last,
                            (position + 1).min(last),
                        )),
                        (!compact).then(|| jump(Glyph::ToEnd, end > 0 && position < last, last)),
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
                            MUTED,
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
            .background(PANEL)
            .corner_radius(10.0)
            .child(Box::new(append(
                Flex::row()
                    .height(28.0)
                    .gap(8.0)
                    .align(Align::Center)
                    .child(dot(accent, 7.0))
                    .child(heading(title, TEXT, 13.0))
                    .child(text(detail, MUTED, 11.0))
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
                    .border(BORDER, 1.0)
                    .child(stage),
            )),
        controls,
    ))
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
            let name = media_name(media);
            let image = state.thumbnails.borrow().get(&media.path).cloned();
            let insertable = !busy && media_frames(media, fps).is_some();
            let insert = sized_icon_button(Glyph::Plus, 24.0, insertable, false, state, move |s| {
                s.insert_media(id)
            });
            let badge = media
                .duration_us
                .map(|duration| short_duration(duration as f64 / 1_000_000.0));
            if view == MediaView::Grid {
                let tile = (inner - 8.0) / 2.0;
                media_item(
                    Style::default().flex_column().gap(6.0).padding_all(5.0),
                    selected == Some(id),
                    vec![
                        Box::new(Thumb {
                            image,
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
                                        .child(text(fit(&name, tile - 40.0, 11.0), TEXT, 11.0))
                                        .child(text(
                                            match media.kind {
                                                MediaKind::Video if media.has_audio => {
                                                    "Video + audio"
                                                }
                                                MediaKind::Video => "Video",
                                                MediaKind::Audio => "Audio",
                                                MediaKind::Image => "Still",
                                            },
                                            kind_color(media.kind).mix(MUTED, 0.5),
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
                            kind: media.kind,
                            width: Some(64.0),
                            height: 36.0,
                            badge: None,
                        }),
                        Box::new(
                            Flex::column()
                                .gap(3.0)
                                .grow(1.0)
                                .child(text(fit(&name, inner - 120.0, 12.0), TEXT, 12.0))
                                .child(text(media_details(media), MUTED, 10.0)),
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
    let filters = [
        ("All", MediaFilter::All),
        ("Video", MediaFilter::Video),
        ("Audio", MediaFilter::Audio),
        ("Stills", MediaFilter::Image),
    ]
    .into_iter()
    .map(|(label, option)| {
        chip(label, filter == option, state, move |s| {
            s.media_filter.set(option)
        })
    })
    .collect();
    Box::new(append(
        Flex::column()
            .gap(8.0)
            .padding(10.0)
            .width(width)
            .background(PANEL)
            .corner_radius(10.0),
        vec![
            panel_header(
                "Media",
                text(
                    if count == project.media().len() {
                        format!("{count}")
                    } else {
                        format!("{count} of {}", project.media().len())
                    },
                    FAINT,
                    11.0,
                ),
                vec![
                    sized_icon_button(
                        Glyph::Grid,
                        26.0,
                        true,
                        view == MediaView::Grid,
                        state,
                        |s| s.media_view.set(MediaView::Grid),
                    ),
                    sized_icon_button(
                        Glyph::List,
                        26.0,
                        true,
                        view == MediaView::List,
                        state,
                        |s| s.media_view.set(MediaView::List),
                    ),
                    sized_icon_button(Glyph::Import, 26.0, !busy, false, state, |s| {
                        s.import_dialog()
                    }),
                ],
            ),
            Box::new(
                TextInput::controlled(&state.media_query)
                    .width(inner)
                    .height(30.0)
                    .placeholder("Search media"),
            ),
            Box::new(
                Flex::row()
                    .gap(2.0)
                    .padding(2.0)
                    .background(BACKGROUND)
                    .corner_radius(7.0)
                    .with_children(filters),
            ),
            Box::new(
                RawScrollView::controlled(fill_style(), state.media_scroll.clone()).child(content),
            ),
            text("Drag into the timeline · drop files anywhere", FAINT, 10.0),
        ],
    ))
}

fn input(controller: &TextController, width: f32) -> BoxedWidget {
    Box::new(TextInput::controlled(controller).width(width).height(30.0))
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
    if count > 1 {
        rows.push(Box::new(
            Flex::column()
                .gap(8.0)
                .padding(12.0)
                .background(PANEL_RAISED)
                .border(ACCENT, 1.0)
                .corner_radius(8.0)
                .child(heading(format!("{count} clips selected"), TEXT, 13.0))
                .child(text("Drag any of them to move them together.", MUTED, 11.0))
                .child(Box::new(
                    Flex::row()
                        .gap(6.0)
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
                ))
                .child(text(
                    "The last clip you picked is shown below.",
                    FAINT,
                    10.0,
                )),
        ));
    }
    if let Some((track_id, clip)) = selected_clip(state) {
        let track_kind = project
            .tracks()
            .iter()
            .find(|track| track.id == track_id)
            .map_or(TrackKind::Video, |track| track.kind);
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
            _ => ("Missing source".into(), MUTED),
        };
        subtitle = match (&clip.content, track_kind) {
            (ClipContent::Title(_), _) => "Title clip",
            (_, TrackKind::Audio) => "Audio clip",
            _ if media.is_some_and(|media| media.kind == MediaKind::Image) => "Still clip",
            _ => "Video clip",
        };
        let preview: BoxedWidget = match (&clip.content, media) {
            (ClipContent::Title(title), _) => Box::new(
                Flex::column()
                    .height(96.0)
                    .full_width()
                    .align(Align::Center)
                    .justify(Justify::Center)
                    .background(STAGE)
                    .corner_radius(6.0)
                    .child(heading(fit(&title.text, inner - 20.0, 18.0), TEXT, 18.0)),
            ),
            (_, Some(media)) => Box::new(Thumb {
                image: (track_kind == TrackKind::Video)
                    .then(|| state.thumbnails.borrow().get(&media.path).cloned())
                    .flatten(),
                kind: if track_kind == TrackKind::Audio {
                    MediaKind::Audio
                } else {
                    media.kind
                },
                width: None,
                height: 110.0,
                badge: None,
            }),
            _ => Box::new(Flex::row().height(0.0)),
        };
        rows.push(preview);
        rows.push(Box::new(
            Flex::row()
                .gap(7.0)
                .align(Align::Center)
                .child(dot(color, 8.0))
                .child(heading(fit(&name, inner - 20.0, 13.0), TEXT, 13.0)),
        ));
        rows.push(section(
            "Timing",
            vec![
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
                property("Source in", timecode(clip.source_start_frame, fps)),
            ],
        ));
        if matches!(clip.content, ClipContent::Title(_)) {
            rows.push(section(
                "Title",
                vec![
                    input(&state.title_text, inner),
                    inline(secondary_button(
                        Some(Glyph::Title),
                        "Apply text",
                        true,
                        state,
                        set_title_text,
                    )),
                ],
            ));
        }
        if track_kind == TrackKind::Audio && media.is_some() {
            let volume = clip.volume;
            rows.push(section(
                "Audio",
                vec![
                    Box::new(
                        Flex::row()
                            .gap(6.0)
                            .align(Align::Center)
                            .child(glyph(
                                if clip.muted {
                                    Glyph::Mute
                                } else {
                                    Glyph::Volume
                                },
                                if clip.muted { MUTED } else { AUDIO },
                                16.0,
                            ))
                            .child(Box::new(
                                Flex::row()
                                    .width(inner - 136.0)
                                    .height(6.0)
                                    .background(SURFACE)
                                    .corner_radius(3.0)
                                    .child(Box::new(
                                        Flex::row()
                                            .width(((inner - 136.0) * volume / 2.0).max(0.0))
                                            .height(6.0)
                                            .background(if clip.muted { FAINT } else { AUDIO })
                                            .corner_radius(3.0),
                                    )),
                            ))
                            .child(Box::new(Flex::row().width(36.0).child(text(
                                format!("{}%", (volume * 100.0).round() as u32),
                                TEXT,
                                11.0,
                            ))))
                            .child(spacer())
                            .child(sized_icon_button(
                                Glyph::Minus,
                                24.0,
                                volume > 0.0,
                                false,
                                state,
                                |s| {
                                    set_clip_audio(s, |clip| {
                                        (
                                            ((clip.volume * 10.0).round() - 1.0).max(0.0) / 10.0,
                                            clip.muted,
                                        )
                                    })
                                },
                            ))
                            .child(sized_icon_button(
                                Glyph::Plus,
                                24.0,
                                volume < 2.0,
                                false,
                                state,
                                |s| {
                                    set_clip_audio(s, |clip| {
                                        (
                                            ((clip.volume * 10.0).round() + 1.0).min(20.0) / 10.0,
                                            clip.muted,
                                        )
                                    })
                                },
                            )),
                    ),
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
                    text(
                        "Saved with the project; audio playback is pending.",
                        FAINT,
                        10.0,
                    ),
                ],
            ));
        }
        let other_track = project
            .tracks()
            .iter()
            .find(|track| track.id != track_id && track.kind == track_kind)
            .map(|track| track.id);
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
                        .child(Box::new(
                            Flex::row().width(86.0).child(text("Position", MUTED, 11.0)),
                        ))
                        .child(sized_icon_button(
                            Glyph::ChevronLeft,
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
                        .child(text("1 frame", FAINT, 10.0)),
                ),
                Box::new(
                    Flex::row()
                        .gap(4.0)
                        .align(Align::Center)
                        .child(Box::new(
                            Flex::row().width(86.0).child(text("Length", MUTED, 11.0)),
                        ))
                        .child(sized_icon_button(
                            Glyph::Minus,
                            26.0,
                            clip.duration_frames > 1,
                            false,
                            state,
                            |s| {
                                edit_selection(s, |clip, track_id, _| Command::Trim {
                                    track_id,
                                    clip_id: clip.id,
                                    start_frame: clip.start_frame,
                                    source_start_frame: clip.source_start_frame,
                                    duration_frames: clip.duration_frames.saturating_sub(1),
                                })
                            },
                        ))
                        .child(sized_icon_button(
                            Glyph::Plus,
                            26.0,
                            true,
                            false,
                            state,
                            |s| {
                                edit_selection(s, |clip, track_id, _| Command::Trim {
                                    track_id,
                                    clip_id: clip.id,
                                    start_frame: clip.start_frame,
                                    source_start_frame: clip.source_start_frame,
                                    duration_frames: clip.duration_frames.saturating_add(1),
                                })
                            },
                        ))
                        .child(text("1 frame", FAINT, 10.0)),
                ),
                inline(secondary_button(
                    Some(Glyph::ChevronDown),
                    &format!(
                        "Move to {}",
                        other_track.map_or_else(|| "other track".into(), label_of)
                    ),
                    other_track.is_some(),
                    state,
                    move |s| {
                        edit_selection(s, |clip, track_id, _| Command::Move {
                            track_id,
                            target_track_id: other_track.unwrap_or(track_id),
                            clip_id: clip.id,
                            to_frame: clip.start_frame,
                        })
                    },
                )),
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
                    TEXT,
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
                .background(PANEL_RAISED)
                .corner_radius(8.0)
                .child(heading("Nothing selected", TEXT, 13.0))
                .child(text(
                    "Select a clip on the timeline to edit it,",
                    MUTED,
                    11.0,
                ))
                .child(text("or a file in the media bin to view it.", MUTED, 11.0)),
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
        rows.push(section(
            "Preview",
            vec![text(
                if state.preview_available.get() {
                    "GPU video preview ready · audio pending"
                } else {
                    "GPU rendering is required for video preview"
                },
                MUTED,
                11.0,
            )],
        ));
        rows.push(section(
            "Shortcuts",
            [
                ("Space", "Play / pause"),
                ("← →", "Step a frame (Shift: 1 s)"),
                ("Ctrl Z / Ctrl Shift Z", "Undo / redo"),
                ("Delete", "Remove (Shift: ripple)"),
                ("Ctrl C / X / V / D", "Copy, cut, paste, duplicate"),
                ("S · Ctrl K", "Split at playhead"),
                ("Ctrl A · Esc", "Select all · clear"),
                ("Alt ← →", "Nudge selection"),
                ("Ctrl + wheel · + −", "Zoom timeline"),
            ]
            .into_iter()
            .map(|(keys, action)| {
                Box::new(
                    Flex::row()
                        .gap(8.0)
                        .align(Align::Center)
                        .child(Box::new(
                            Flex::row().width(112.0).child(text(keys, TEXT, 10.0)),
                        ))
                        .child(text(action, MUTED, 10.0)),
                ) as BoxedWidget
            })
            .collect(),
        ));
    }
    Box::new(
        Flex::column()
            .width(width)
            .gap(8.0)
            .padding(12.0)
            .background(PANEL)
            .corner_radius(10.0)
            .child(panel_header(
                "Inspector",
                text(subtitle, FAINT, 11.0),
                Vec::new(),
            ))
            .child(Box::new(
                RawScrollView::controlled(fill_style(), state.inspector_scroll.clone())
                    .child(Box::new(Flex::column().gap(10.0).with_children(rows))),
            )),
    )
}

fn timeline_lanes(state: &UiState, project: &Project, fps: (u32, u32)) -> Vec<Lane> {
    let labels = track_labels(project);
    project
        .tracks()
        .iter()
        .zip(&labels)
        .map(|(track, (_, label))| Lane {
            track_id: track.id,
            kind: track.kind,
            label: label.clone(),
            clips: track
                .clips
                .iter()
                .map(|clip| {
                    let media = match clip.content {
                        ClipContent::Media { media_id } => {
                            project.media().iter().find(|media| media.id == media_id)
                        }
                        ClipContent::Title(_) => None,
                    };
                    let video = track.kind == TrackKind::Video;
                    LaneClip {
                        clip_id: clip.id,
                        start: clip.start_frame,
                        duration: clip.duration_frames,
                        name: match (&clip.content, media) {
                            (ClipContent::Title(title), _) => title.text.clone(),
                            (_, Some(media)) => media_name(media),
                            _ => "Missing source".into(),
                        },
                        look: match (&clip.content, media) {
                            (ClipContent::Title(_), _) => ClipLook::Title,
                            _ if !video => ClipLook::Audio,
                            (_, Some(media)) if media.kind == MediaKind::Image => ClipLook::Image,
                            _ => ClipLook::Video,
                        },
                        images: media.filter(|_| video).and_then(|media| {
                            state
                                .storyboards
                                .borrow()
                                .get(&StoryboardKey {
                                    path: media.path.clone(),
                                    kind: media.kind,
                                    source_start: clip.source_start_frame,
                                    duration: clip.duration_frames,
                                    fps,
                                })
                                .cloned()
                        }),
                        fallback: media
                            .filter(|_| video)
                            .and_then(|media| state.thumbnails.borrow().get(&media.path).cloned()),
                        volume: clip.volume,
                        muted: clip.muted,
                    }
                })
                .collect(),
        })
        .collect()
}

pub(crate) fn build(state: &UiState, viewport: Size) -> BoxedWidget {
    state.revision.get();
    state.ui_revision.get();
    let editor = state.editor.borrow();
    let project = editor.project();
    let compact = viewport.width < 1200.0;
    let narrow = viewport.width < 1000.0;
    let busy = state.busy.get();
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

    let file_button = |icon: Glyph, label: &str, operation: fn(&UiState)| {
        if narrow {
            icon_button(icon, !busy, state, operation)
        } else {
            tool_button(Some(icon), label, !busy, state, operation)
        }
    };
    let titlebar = AppTitlebar::new(viewport.width)
        .height(46.0)
        .padding(12.0)
        .center_margin(32.0)
        .foreground(TEXT)
        .background(PANEL)
        .start_boxed(Box::new(
            Flex::row()
                .gap(4.0)
                .align(Align::Center)
                .child(Box::new(
                    Flex::row()
                        .size(26.0, 26.0)
                        .align(Align::Center)
                        .justify(Justify::Center)
                        .background(ACCENT)
                        .corner_radius(7.0)
                        .child(glyph(Glyph::Film, Color::rgb(5, 28, 25), 16.0)),
                ))
                .child(Box::new(Flex::row().width(4.0)))
                .child(heading("OCUS", TEXT, 15.0))
                .child(Box::new(Flex::row().width(10.0)))
                .child(divider())
                .child(Box::new(Flex::row().width(6.0)))
                .child(file_button(Glyph::Document, "New", UiState::new_project))
                .child(file_button(Glyph::Folder, "Open", UiState::open_dialog))
                .child(file_button(Glyph::Save, "Save", UiState::save))
                .child(if narrow {
                    Box::new(Flex::row().width(0.0)) as BoxedWidget
                } else {
                    tool_button(None, "Save as…", !busy, state, UiState::save_as_dialog)
                }),
        ))
        .center_boxed(Box::new(
            Flex::row()
                .gap(8.0)
                .align(Align::Center)
                .child(text(fit(&project_label, 220.0, 12.0), TEXT, 12.0))
                .child(if dirty {
                    Box::new(
                        Flex::row()
                            .gap(5.0)
                            .align(Align::Center)
                            .padding_xy(7.0, 2.0)
                            .background(SURFACE)
                            .corner_radius(9.0)
                            .child(dot(IMAGE, 6.0))
                            .child(text("Edited", MUTED, 10.0)),
                    ) as BoxedWidget
                } else {
                    Box::new(Flex::row().width(0.0))
                }),
        ))
        .end_boxed(Box::new(
            Flex::row()
                .gap(4.0)
                .align(Align::Center)
                .child(icon_button(
                    Glyph::Undo,
                    !busy && editor.can_undo(),
                    state,
                    UiState::undo,
                ))
                .child(icon_button(
                    Glyph::Redo,
                    !busy && editor.can_redo(),
                    state,
                    UiState::redo,
                ))
                .child(Box::new(Flex::row().width(6.0)))
                .child(secondary_button(
                    Some(Glyph::Import),
                    "Import",
                    !busy,
                    state,
                    UiState::import_dialog,
                ))
                .child(if state.exporting.get() {
                    secondary_button(None, "Cancel export", true, state, UiState::cancel_export)
                } else {
                    accent_button(
                        Some(Glyph::Export),
                        if narrow {
                            "Export"
                        } else {
                            "Export silent MP4"
                        },
                        !busy,
                        state,
                        UiState::export_dialog,
                    )
                }),
        ));

    let frame = state.program.position.get();
    let timeline_end = state.sequence_end();
    let clip_at = |frame: u64| {
        project
            .tracks()
            .iter()
            .filter(|track| track.kind == TrackKind::Video)
            .flat_map(|track| &track.clips)
            .find(|clip| {
                clip.start_frame <= frame
                    && clip
                        .start_frame
                        .checked_add(clip.duration_frames)
                        .is_some_and(|end| frame < end)
            })
    };
    let program_clip = clip_at(frame);
    let empty_timeline = project.tracks().iter().all(|track| track.clips.is_empty());
    let first_media = project.media().first();
    let program_stage: BoxedWidget = match program_clip.map(|clip| &clip.content) {
        Some(ClipContent::Media { .. }) if gpu_preview => {
            monitor_viewport(&state.program.slot, aspect)
        }
        Some(ClipContent::Title(title)) => title_card(&title.text),
        Some(ClipContent::Media { .. }) => stage_message(
            Glyph::Film,
            "GPU preview unavailable",
            "A GPU rendering backend is required to show video.",
            None,
        ),
        None if empty_timeline => stage_message(
            Glyph::Film,
            if first_media.is_some() {
                "Footage is ready"
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
        ),
        None => stage_message(
            Glyph::Film,
            "No clip at this frame",
            "Move the playhead over a video, still or title clip.",
            None,
        ),
    };
    let has_video = project.tracks().iter().any(|track| {
        track.kind == TrackKind::Video
            && track
                .clips
                .iter()
                .any(|clip| matches!(clip.content, ClipContent::Media { .. }))
    });
    let program_playing = state.program.playing.get();
    let program = monitor(
        "Program",
        PLAYHEAD,
        "Sequence".into(),
        vec![text(
            format!("{}p · {}", project.settings().height, frame_rate_label(fps)),
            FAINT,
            10.0,
        )],
        program_stage,
        transport(
            state,
            Transport {
                position: frame,
                end: timeline_end,
                fps,
                playing: program_playing,
                can_play: gpu_preview && has_video,
                color: ACCENT,
                compact: narrow,
            },
            |state, target| {
                if state.program.position.peek() != target {
                    if state.program.playing.peek() {
                        state.pause();
                    }
                    state.focus_frame(target);
                }
            },
            |state| {
                if state.program.playing.peek() {
                    state.pause();
                } else {
                    state.play();
                }
            },
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
    let source_stage: BoxedWidget = match (&range, &selected_title) {
        (Some(range), _) if range.kind == MediaKind::Audio => stage_message(
            Glyph::Music,
            "Audio source",
            "Waveforms and audio playback are not available yet.",
            None,
        ),
        (Some(_), _) if gpu_preview => monitor_viewport(&state.source.slot, aspect),
        (Some(_), _) => stage_message(
            Glyph::Film,
            "GPU preview unavailable",
            "A GPU rendering backend is required to show video.",
            None,
        ),
        (None, Some(title)) => title_card(title),
        (None, None) => stage_message(
            Glyph::Locate,
            "No source selected",
            "Pick a clip on the timeline or a file in the bin.",
            None,
        ),
    };
    let source_end = range.as_ref().map_or(0, |range| range.duration);
    let source_actions: Vec<BoxedWidget> = match (primary, selected_media) {
        (Some(_), _) => selected_clip(state)
            .filter(|_| range.is_some())
            .map(|(_, clip)| {
                let start = clip.start_frame;
                tool_button(Some(Glyph::Locate), "Match frame", true, state, move |s| {
                    s.pause_source();
                    s.focus_frame(start.saturating_add(s.source.position.peek()));
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
                position: state.source.position.get(),
                end: source_end,
                fps,
                playing: state.source.playing.get(),
                can_play: gpu_preview && range.as_ref().is_some_and(|r| r.kind != MediaKind::Audio),
                color: VIDEO,
                compact: narrow,
            },
            |state, target| {
                if state.source.position.peek() != target {
                    if state.source.playing.peek() {
                        state.pause_source();
                    }
                    state.focus_source_frame(target);
                }
            },
            |state| {
                if state.source.playing.peek() {
                    state.pause_source();
                } else {
                    state.play_source();
                }
            },
        ),
    );

    let lanes = timeline_lanes(state, project, fps);
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
        .background(PANEL)
        .corner_radius(10.0)
        .child(Box::new(
            Flex::row()
                .height(32.0)
                .gap(4.0)
                .align(Align::Center)
                .child(Box::new(Flex::row().width(4.0)))
                .child(heading(timecode(frame, fps), ACCENT, 15.0))
                .child(Box::new(Flex::row().width(6.0)))
                .child(text(
                    format!("of {}", timecode(timeline_end, fps)),
                    FAINT,
                    11.0,
                ))
                .child(Box::new(Flex::row().width(10.0)))
                .child(divider())
                .child(tool_button(
                    Some(Glyph::Split),
                    "Split",
                    splittable,
                    state,
                    UiState::split_at_playhead,
                ))
                .child(tool_button(
                    Some(Glyph::Trash),
                    "Delete",
                    selected,
                    state,
                    |s| s.delete_selection(false),
                ))
                .child(tool_button(
                    Some(Glyph::Ripple),
                    "Ripple",
                    selected,
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
                .child(spacer())
                .child(icon_button(
                    Glyph::ChevronLeft,
                    state.timeline_start.get() > 0,
                    state,
                    |s| s.scroll_timeline(-8),
                ))
                .child(icon_button(
                    Glyph::ChevronRight,
                    state.timeline_start.get().saturating_add(span) < timeline_end,
                    state,
                    |s| s.scroll_timeline(8),
                ))
                .child(divider())
                .child(icon_button(
                    Glyph::ZoomOut,
                    zoom + 1 < ZOOM_SECONDS.len(),
                    state,
                    |s| s.zoom_timeline(s.timeline_zoom.peek() + 1),
                ))
                .child(Box::new(
                    Flex::row().width(44.0).justify(Justify::Center).child(text(
                        short_duration(ZOOM_SECONDS[zoom.min(ZOOM_SECONDS.len() - 1)] as f64),
                        MUTED,
                        11.0,
                    )),
                ))
                .child(icon_button(Glyph::ZoomIn, zoom > 0, state, |s| {
                    s.zoom_timeline(s.timeline_zoom.peek().saturating_sub(1))
                }))
                .child(divider())
                .child(icon_button(
                    if expanded_timeline {
                        Glyph::ChevronDown
                    } else {
                        Glyph::ChevronUp
                    },
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
            selected: selection.clone(),
            ghosts: plan
                .as_ref()
                .map_or_else(Vec::new, |plan| plan.ghosts.clone()),
            dragging: matches!(
                drag.map(|drag| drag.payload),
                Some(DragPayload::Clips { .. })
            ),
            marquee,
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
                .child(Box::new(
                    Flex::column()
                        .width(side_width)
                        .gap(6.0)
                        .child(Box::new(
                            Flex::row()
                                .gap(2.0)
                                .padding(2.0)
                                .background(PANEL)
                                .corner_radius(8.0)
                                .child(chip("Media", active == Sidebar::Media, state, |s| {
                                    s.sidebar.set(Sidebar::Media)
                                }))
                                .child(chip(
                                    "Inspector",
                                    active == Sidebar::Inspector,
                                    state,
                                    |s| s.sidebar.set(Sidebar::Inspector),
                                )),
                        ))
                        .child(Box::new(Flex::column().grow(1.0).child(
                            if active == Sidebar::Media {
                                media_bin(state, project, side_width, busy)
                            } else {
                                inspector(state, project, side_width, busy)
                            },
                        ))),
                ))
                .child(monitors),
        )
    } else {
        Box::new(
            Flex::row()
                .gap(6.0)
                .height(workspace_height)
                .child(media_bin(state, project, side_width, busy))
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
                ACCENT
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
            MUTED,
            10.0,
        ))
        .child(text(state.status.get(), MUTED, 11.0))
        .child(spacer());
    if state.exporting.get()
        && let Some(control) = state.export_control()
    {
        status = status
            .child(text("Exporting", MUTED, 10.0))
            .child(Box::new(ExportMeter {
                control,
                total: state.export_total.get(),
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
        FAINT,
        10.0,
    ));

    Box::new(
        Flex::column()
            .size(viewport.width, viewport.height)
            .gap(6.0)
            .background(BACKGROUND)
            .child(Box::new(titlebar))
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
            }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::interaction::Action;
    use creamui::core::{Painter, Point, Rect, Renderer, RgbaImage, TextAlign, Widget};
    use std::sync::Arc;

    #[derive(Default)]
    struct TestPainter {
        labels: Vec<String>,
        bounds: Vec<(String, Rect)>,
        images: Vec<Rect>,
    }

    impl Painter for TestPainter {
        fn fill_rect(&mut self, _: Rect, _: Color, _: f32) {}

        fn stroke_rect(&mut self, _: Rect, _: Color, _: f32, _: f32) {}

        fn fill_text(&mut self, rect: Rect, text: &str, _: Color, _: f32, _: TextAlign) {
            self.labels.push(text.to_owned());
            self.bounds.push((text.to_owned(), rect));
        }

        fn draw_image(&mut self, rect: Rect, _: &RgbaImage, _: Option<Color>) {
            self.images.push(rect);
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
            selected: state.selected.peek(),
            ghosts: Vec::new(),
            dragging: false,
            marquee: None,
            state: state.clone(),
        }
    }

    const TIMELINE: Rect = Rect {
        x: 0.0,
        y: 0.0,
        width: 1128.0,
        height: 240.0,
    };

    /// The timeline x for `frame` with a 1000 px wide, 300 frame view.
    fn at(frame: f32) -> f32 {
        128.0 + frame * 1000.0 / 300.0
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
    fn timeline_draws_filmstrips_and_scrubs_from_the_ruler() {
        let state = UiState::new();
        let image = Arc::new(RgbaImage::new(16, 9, vec![255; 16 * 9 * 4]).unwrap());
        let view = TimelineView {
            lanes: vec![Lane {
                track_id: 0,
                kind: TrackKind::Video,
                label: "V1".into(),
                clips: vec![LaneClip {
                    clip_id: 7,
                    start: 30,
                    duration: 150,
                    name: "Source".into(),
                    look: ClipLook::Video,
                    images: Some(Arc::new(vec![image.clone(), image.clone(), image])),
                    fallback: None,
                    volume: 1.0,
                    muted: false,
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
        }
        assert!(state.add_title());
        let painter = render(&state, 1440.0, 900.0);
        assert!(painter.labels.iter().any(|text| text == "Title 1"));
        assert!(painter.labels.iter().any(|text| text == "Title clip"));
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
        let painter = render(&state, 1440.0, 900.0);
        assert!(
            painter
                .labels
                .iter()
                .any(|label| label.contains("tone.wav"))
        );
        assert!(painter.labels.iter().any(|label| label == "100%"));
        assert!(painter.labels.iter().any(|label| label == "Audio source"));
        set_clip_audio(&state, |clip| (clip.volume - 0.5, true));
        let painter = render(&state, 1440.0, 900.0);
        assert!(painter.labels.iter().any(|label| label == "50%"));
        assert!(painter.labels.iter().any(|label| label == "Unmute"));
    }

    #[test]
    fn frame_rates_drop_redundant_decimals() {
        assert_eq!(frame_rate_label((30, 1)), "30 fps");
        assert_eq!(frame_rate_label((30_000, 1_001)), "29.97 fps");
        assert_eq!(frame_rate_label((25, 2)), "12.5 fps");
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
