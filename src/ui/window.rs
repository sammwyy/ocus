use crate::project::{Command, Editor};
use crate::timeline::{Clip, ClipContent, Title, TrackKind};
use creamui::widgets::layout::{Align, Flex};
use creamui::widgets::{AppTitlebar, Button, ButtonSize, ButtonState, ButtonVariant, RawText};
use creamui::{BoxedWidget, Color, Signal, Size, Styled};
use std::cell::RefCell;
use std::rc::Rc;

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
        .gap(16.0)
        .padding(18.0)
        .background(PANEL)
        .corner_radius(12.0)
        .child(Box::new(
            Flex::column()
                .gap(4.0)
                .child(text(title, TEXT, 15.0))
                .child(text(subtitle, MUTED, 11.0)),
        ))
        .child(content)
}

fn add_title(editor: &Rc<RefCell<Editor>>) -> bool {
    let mut editor = editor.borrow_mut();
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
    editor.apply(Command::InsertClip {
        track_id,
        clip: Clip {
            id,
            start_frame,
            source_start_frame: 0,
            duration_frames: 90,
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
    })
}

fn action(
    label: &str,
    enabled: bool,
    editor: &Rc<RefCell<Editor>>,
    revision: &Signal<u64>,
    operation: impl Fn(&Rc<RefCell<Editor>>) -> bool + 'static,
) -> BoxedWidget {
    let editor = Rc::clone(editor);
    let revision = revision.clone();
    Box::new(
        Button::styled(
            ButtonVariant::Secondary,
            ButtonSize::Sm,
            label,
            ButtonState::Normal,
            move || {
                if operation(&editor) {
                    revision.update(|value| *value += 1);
                }
            },
        )
        .disabled(!enabled),
    )
}

pub(crate) fn build(
    editor: &Rc<RefCell<Editor>>,
    revision: &Signal<u64>,
    viewport: Size,
) -> BoxedWidget {
    let state = editor.borrow();
    let project = state.project();
    let compact = viewport.width < 1000.0;
    let titlebar = AppTitlebar::new(viewport.width)
        .height(54.0)
        .padding(16.0)
        .center_margin(36.0)
        .foreground(TEXT)
        .background(PANEL)
        .start_boxed(text("OCUS  /  EDIT", ACCENT, 15.0))
        .center_boxed(text(project.name(), TEXT, 14.0));

    let media = panel(
        "LIBRARY",
        "PROJECT ASSETS",
        Box::new(
            Flex::column()
                .gap(14.0)
                .child(text("ALL MEDIA", ACCENT, 11.0))
                .child(Box::new(
                    Flex::column()
                        .gap(8.0)
                        .padding(16.0)
                        .background(SURFACE)
                        .corner_radius(9.0)
                        .child(text("Your media lives here", TEXT, 13.0))
                        .child(text("Import is not available yet", MUTED, 11.0)),
                ))
                .child(text(
                    format!("{} sources in project", project.media().len()),
                    MUTED,
                    11.0,
                )),
        ),
    )
    .width(if compact { 180.0 } else { 248.0 });
    let preview = panel(
        "PROGRAM MONITOR",
        "PREVIEW  /  16:9",
        Box::new(
            Flex::column()
                .gap(10.0)
                .grow(1.0)
                .child(Box::new(
                    Flex::column()
                        .grow(1.0)
                        .align(Align::Center)
                        .gap(12.0)
                        .padding(24.0)
                        .background(STAGE)
                        .corner_radius(9.0)
                        .child(text("◇", ACCENT, 38.0))
                        .child(text("Preview unavailable", TEXT, 16.0))
                        .child(text("Media playback is not implemented", MUTED, 12.0)),
                ))
                .child(Box::new(
                    Flex::row()
                        .gap(12.0)
                        .child(text("NO PLAYBACK", MUTED, 11.0))
                        .child(text(
                            format!(
                                "{} × {}  ·  {} fps",
                                project.settings().width,
                                project.settings().height,
                                project.settings().frame_rate.numerator
                            ),
                            MUTED,
                            11.0,
                        )),
                )),
        ),
    )
    .grow(1.0);
    let inspector = panel(
        "INSPECTOR",
        "PROJECT PROPERTIES",
        Box::new(
            Flex::column()
                .gap(12.0)
                .child(text("SEQUENCE", ACCENT, 11.0))
                .child(text(
                    format!(
                        "{} × {}",
                        project.settings().width,
                        project.settings().height
                    ),
                    TEXT,
                    15.0,
                ))
                .child(text("Select a clip to inspect it", MUTED, 12.0)),
        ),
    )
    .width(220.0);

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
                    Box::new(
                        Flex::row()
                            .padding_xy(12.0, 8.0)
                            .background(SURFACE)
                            .corner_radius(6.0)
                            .child(text(name, color, 12.0)),
                    ) as BoxedWidget
                })
                .collect();
            Box::new(
                Flex::row()
                    .height(42.0)
                    .gap(8.0)
                    .align(Align::Center)
                    .background(STAGE)
                    .child(Box::new(
                        Flex::row()
                            .width(124.0)
                            .padding(10.0)
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
        .gap(10.0)
        .padding(16.0)
        .background(PANEL)
        .corner_radius(12.0)
        .child(Box::new(
            Flex::row()
                .gap(12.0)
                .align(Align::Center)
                .child(text("TIMELINE", TEXT, 14.0))
                .child(text(
                    format!(
                        "{} / {} FPS",
                        project.settings().frame_rate.numerator,
                        project.settings().frame_rate.denominator
                    ),
                    MUTED,
                    11.0,
                ))
                .child(action("＋ Title", true, editor, revision, add_title))
                .child(action(
                    "↶ Undo",
                    state.can_undo(),
                    editor,
                    revision,
                    |editor| editor.borrow_mut().undo(),
                ))
                .child(action(
                    "↷ Redo",
                    state.can_redo(),
                    editor,
                    revision,
                    |editor| editor.borrow_mut().redo(),
                )),
        ))
        .child(text("TRACKS  /  CLIP ORDER", MUTED, 11.0))
        .child(Box::new(Flex::column().gap(3.0).with_children(tracks)));

    let mut workspace = Flex::row()
        .gap(8.0)
        .grow(1.0)
        .child(Box::new(media))
        .child(Box::new(preview));
    if !compact {
        workspace = workspace.child(Box::new(inspector));
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
                    .padding_xy(8.0, 0.0)
                    .grow(1.0)
                    .child(Box::new(workspace))
                    .child(Box::new(timeline))
                    .child(Box::new(
                        Flex::row()
                            .height(26.0)
                            .child(text("OCUS  /  EDIT WORKSPACE", MUTED, 10.0))
                            .child(text("    •    Changes are not saved to disk", MUTED, 10.0)),
                    )),
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
        let editor = Rc::new(RefCell::new(Editor::new(crate::project::Project::new(
            "Untitled",
        ))));
        let revision = Signal::new(0);
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
                Renderer::new().render(build(&editor, &revision, viewport), viewport, &mut painter);
            });
            for label in ["OCUS  /  EDIT", "LIBRARY", "PROGRAM MONITOR", "TIMELINE"] {
                assert!(painter.labels.iter().any(|text| text == label), "{label}");
            }
            for label in ["V1", "V2", "A1", "A2"] {
                assert!(painter.labels.iter().any(|text| text.starts_with(label)));
            }
        }
        assert!(add_title(&editor));
        let viewport = Size {
            width: 1440.0,
            height: 900.0,
        };
        let mut painter = TestPainter::default();
        creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(creamui::theme::ThemeProvider::new(
                creamui::Theme::dark(),
            ));
            Renderer::new().render(build(&editor, &revision, viewport), viewport, &mut painter);
        });
        assert!(painter.labels.iter().any(|text| text == "Title 1"));
    }
}
