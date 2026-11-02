use super::UiState;
use crate::project::Command;
use crate::timeline::{Clip, ClipContent, Title, TrackKind};
use creamui::core::layout::{Dimension, Style};
use creamui::widgets::layout::{Align, Flex};
use creamui::widgets::{
    AppTitlebar, Button, ButtonSize, ButtonState, ButtonVariant, RawScrollView, RawText,
    TextController, TextInput,
};
use creamui::{BoxedWidget, Color, Size, Styled};

pub(crate) const BACKGROUND: Color = Color::rgb(14, 18, 26);
const PANEL: Color = Color::rgb(24, 29, 40);
const SURFACE: Color = Color::rgb(32, 39, 53);
const STAGE: Color = Color::rgb(9, 13, 21);
const TEXT: Color = Color::rgb(239, 243, 250);
const MUTED: Color = Color::rgb(144, 157, 177);
const ACCENT: Color = Color::rgb(115, 169, 248);
const AUDIO: Color = Color::rgb(103, 204, 181);

fn text(value: impl Into<String>, color: Color, size: f32) -> BoxedWidget {
    Box::new(RawText::new(value, color, size))
}

fn panel(title: &str, subtitle: &str, content: BoxedWidget) -> Flex {
    Flex::column()
        .gap(8.0)
        .padding(10.0)
        .background(PANEL)
        .corner_radius(6.0)
        .child(Box::new(
            Flex::row()
                .gap(10.0)
                .child(text(title, TEXT, 12.0))
                .child(text(subtitle, MUTED, 11.0)),
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
        state.title_text.set_value(format!("Title {id}"));
        state.playhead.set(start_frame);
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
    let state = state.clone();
    Box::new(
        Button::styled(
            ButtonVariant::Secondary,
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
    let editor = state.editor.borrow();
    let project = editor.project();
    let compact = viewport.width < 1000.0;
    let busy = state.busy.get();
    let titlebar = AppTitlebar::new(viewport.width)
        .height(42.0)
        .padding(10.0)
        .center_margin(24.0)
        .foreground(TEXT)
        .background(PANEL)
        .start_boxed(text("OCUS", ACCENT, 15.0))
        .center_boxed(text(project.name(), TEXT, 12.0));

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
                    .gap(3.0)
                    .padding(7.0)
                    .background(SURFACE)
                    .child(text(name, TEXT, 12.0))
                    .child(text(
                        format!("{:?} · reference only", media.kind),
                        MUTED,
                        10.0,
                    )),
            ) as BoxedWidget
        })
        .collect();
    let media = panel(
        "MEDIA",
        "REFERENCES",
        Box::new(
            Flex::column()
                .gap(6.0)
                .child(input(&state.media_path))
                .child(action("Add reference", !busy, state, UiState::import))
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
                    .child(Box::new(Flex::column().gap(4.0).with_children(files))),
                ))
                .child(text("Sources are not decoded yet", MUTED, 10.0)),
        ),
    )
    .width(if compact { 205.0 } else { 240.0 });
    let frame = state.playhead.get();
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
    let preview = panel(
        "PROGRAM MONITOR",
        "PREVIEW  /  16:9",
        Box::new(
            Flex::column()
                .gap(5.0)
                .grow(1.0)
                .child(Box::new(
                    Flex::column()
                        .grow(1.0)
                        .align(Align::Center)
                        .gap(6.0)
                        .padding(8.0)
                        .background(STAGE)
                        .corner_radius(4.0)
                        .child(text(
                            title_at_frame.unwrap_or("No frame to display"),
                            TEXT,
                            22.0,
                        ))
                        .child(text("Title text only · No video renderer", MUTED, 11.0)),
                ))
                .child(Box::new(
                    Flex::row()
                        .gap(5.0)
                        .align(Align::Center)
                        .child(action("◀", frame > 0, state, |s| {
                            s.playhead.update(|f| *f = f.saturating_sub(1))
                        }))
                        .child(text(format!("Frame {frame}"), TEXT, 12.0))
                        .child(action("▶", true, state, |s| {
                            s.playhead.update(|f| *f = f.saturating_add(1))
                        }))
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
        if matches!(clip.content, ClipContent::Title(_)) {
            content = content.child(input(&state.title_text)).child(action(
                "Apply title",
                true,
                state,
                set_title_text,
            ));
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
            .child(action("Split at playhead", true, state, |s| {
                edit_selection(s, |clip, track_id, next_id| Command::Split {
                    track_id,
                    clip_id: clip.id,
                    at_frame: s.playhead.peek(),
                    new_id: next_id,
                });
            }))
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
                s.edit(Command::RemoveClip { track_id, clip_id });
                s.selected.set(None);
            }))
            .child(action("Ripple delete", true, state, move |s| {
                s.edit(Command::RippleDelete { track_id, clip_id });
                s.selected.set(None);
            }));
        Box::new(content)
    } else {
        text("Select a clip in the timeline", MUTED, 11.0)
    };
    let inspector =
        panel("CLIP", "EDIT", inspector_content).width(if compact { 205.0 } else { 238.0 });

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
            let clips: Vec<BoxedWidget> = track
                .clips
                .iter()
                .map(|clip| {
                    let name = match &clip.content {
                        ClipContent::Title(title) => title.text.clone(),
                        ClipContent::Media { media_id } => format!("Media {media_id}"),
                    };
                    let clip_id = clip.id;
                    let track_id = track.id;
                    let start_frame = clip.start_frame;
                    let title = match &clip.content {
                        ClipContent::Title(title) => title.text.clone(),
                        _ => String::new(),
                    };
                    let state = state.clone();
                    Box::new(Button::styled(
                        if state.selected.peek() == Some((track_id, clip_id)) {
                            ButtonVariant::Primary
                        } else {
                            ButtonVariant::Secondary
                        },
                        ButtonSize::Sm,
                        format!(
                            "{name}  ·  {}–{}",
                            clip.start_frame,
                            clip.start_frame + clip.duration_frames
                        ),
                        ButtonState::Normal,
                        move || {
                            state.selected.set(Some((track_id, clip_id)));
                            state.title_text.set_value(title.clone());
                            state.playhead.set(start_frame);
                        },
                    )) as BoxedWidget
                })
                .collect();
            Box::new(
                Flex::row()
                    .height(36.0)
                    .gap(4.0)
                    .align(Align::Center)
                    .background(STAGE)
                    .child(Box::new(
                        Flex::row()
                            .width(90.0)
                            .padding(5.0)
                            .child(text(label, color, 11.0)),
                    ))
                    .child(Box::new(Flex::row().gap(6.0).with_children(
                        if clips.is_empty() {
                            vec![text("Empty track", MUTED, 11.0)]
                        } else {
                            clips
                        },
                    ))),
            ) as BoxedWidget
        })
        .collect();
    let timeline = Flex::column()
        .gap(5.0)
        .padding(8.0)
        .background(PANEL)
        .corner_radius(6.0)
        .child(Box::new(
            Flex::row()
                .gap(6.0)
                .align(Align::Center)
                .child(text("TIMELINE", TEXT, 12.0))
                .child(text(
                    format!(
                        "{} / {} FPS",
                        project.settings().frame_rate.numerator,
                        project.settings().frame_rate.denominator
                    ),
                    MUTED,
                    11.0,
                ))
                .child(action("＋ Title", true, state, |s| {
                    add_title(s);
                }))
                .child(action("Undo", editor.can_undo(), state, UiState::undo))
                .child(action("Redo", editor.can_redo(), state, UiState::redo)),
        ))
        .child(Box::new(Flex::column().gap(3.0).with_children(tracks)));

    let mut workspace = Flex::row().gap(4.0).grow(1.0);
    if !compact {
        workspace = workspace.child(Box::new(media));
    }
    workspace = workspace
        .child(Box::new(preview))
        .child(Box::new(inspector));

    Box::new(
        Flex::column()
            .size(viewport.width, viewport.height)
            .gap(4.0)
            .background(BACKGROUND)
            .child(Box::new(titlebar))
            .child(Box::new(
                Flex::column()
                    .gap(4.0)
                    .padding_xy(4.0, 0.0)
                    .grow(1.0)
                    .child(Box::new(
                        Flex::row()
                            .gap(6.0)
                            .align(Align::Center)
                            .padding(5.0)
                            .background(PANEL)
                            .child(text("PROJECT", ACCENT, 11.0))
                            .child(input(&state.project_path))
                            .child(action("Open", !busy, state, UiState::load))
                            .child(action("Save", !busy, state, UiState::save))
                            .child(text("Enter a .ocus path to open or save", MUTED, 11.0)),
                    ))
                    .child(if compact {
                        Box::new(
                            Flex::row()
                                .gap(6.0)
                                .align(Align::Center)
                                .padding(4.0)
                                .background(PANEL)
                                .child(text("MEDIA", ACCENT, 11.0))
                                .child(input(&state.media_path))
                                .child(action("Add reference", !busy, state, UiState::import))
                                .child(text(
                                    format!("{} sources", project.media().len()),
                                    MUTED,
                                    11.0,
                                )),
                        ) as BoxedWidget
                    } else {
                        Box::new(Flex::row().height(0.0)) as BoxedWidget
                    })
                    .child(Box::new(workspace))
                    .child(Box::new(timeline))
                    .child(Box::new(Flex::row().height(20.0).child(text(
                        state.status.get(),
                        MUTED,
                        11.0,
                    )))),
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
    }

    impl Painter for TestPainter {
        fn fill_rect(&mut self, _: Rect, _: Color, _: f32) {}

        fn stroke_rect(&mut self, _: Rect, _: Color, _: f32, _: f32) {}

        fn fill_text(&mut self, _: Rect, text: &str, _: Color, _: f32, _: TextAlign) {
            self.labels.push(text.to_owned());
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
            for label in ["OCUS", "MEDIA", "PROGRAM MONITOR", "TIMELINE"] {
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
    }
}
