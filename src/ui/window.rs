use crate::project::Project;
use crate::timeline::TrackKind;
use creamui::widgets::RawText;
use creamui::widgets::layout::{Align, Flex};
use creamui::{BoxedWidget, Color, Size, Styled};

pub(crate) const BACKGROUND: Color = Color::rgb(20, 22, 28);
const PANEL: Color = Color::rgb(31, 34, 42);
const TEXT: Color = Color::rgb(230, 233, 240);
const MUTED: Color = Color::rgb(158, 164, 177);

fn text(value: impl Into<String>, color: Color, size: f32) -> BoxedWidget {
    Box::new(RawText::new(value, color, size))
}

fn panel(title: &str, content: BoxedWidget) -> Flex {
    Flex::column()
        .gap(12.0)
        .padding(16.0)
        .background(PANEL)
        .child(text(title, TEXT, 15.0))
        .child(content)
}

pub(crate) fn build(project: &Project, viewport: Size) -> BoxedWidget {
    let media = panel(
        "Media bin",
        text(
            format!("{} imported files", project.media().len()),
            MUTED,
            13.0,
        ),
    )
    .width(225.0);
    let preview = panel("Preview", text("No frame to display", MUTED, 13.0)).grow(1.0);
    let inspector = panel("Inspector", text("No clip selected", MUTED, 13.0)).width(225.0);

    let mut video_tracks = 0;
    let mut audio_tracks = 0;
    let tracks = project
        .tracks()
        .iter()
        .map(|track| {
            let (label, index) = match track.kind {
                TrackKind::Video => {
                    video_tracks += 1;
                    ("Video", video_tracks)
                }
                TrackKind::Audio => {
                    audio_tracks += 1;
                    ("Audio", audio_tracks)
                }
            };
            text(
                format!("{label} {index} · {} clips", track.clips.len()),
                MUTED,
                13.0,
            )
        })
        .collect();
    let timeline = panel(
        "Timeline",
        Box::new(Flex::column().gap(8.0).with_children(tracks)),
    )
    .height(230.0);

    Box::new(
        Flex::column()
            .size(viewport.width, viewport.height)
            .gap(8.0)
            .padding(8.0)
            .background(BACKGROUND)
            .child(Box::new(
                Flex::row()
                    .height(46.0)
                    .align(Align::Center)
                    .padding(10.0)
                    .background(PANEL)
                    .child(text("Ocus", TEXT, 20.0))
                    .child(text(format!("  /  {}", project.name()), MUTED, 14.0)),
            ))
            .child(Box::new(
                Flex::row()
                    .gap(8.0)
                    .grow(1.0)
                    .child(Box::new(media))
                    .child(Box::new(preview))
                    .child(Box::new(inspector)),
            ))
            .child(Box::new(timeline)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use creamui::core::{Painter, Rect, Renderer, TextAlign};

    #[derive(Default)]
    struct TestPainter {
        labels: Vec<String>,
        panels: usize,
    }

    impl Painter for TestPainter {
        fn fill_rect(&mut self, _: Rect, _: Color, _: f32) {
            self.panels += 1;
        }

        fn stroke_rect(&mut self, _: Rect, _: Color, _: f32, _: f32) {}

        fn fill_text(&mut self, _: Rect, text: &str, _: Color, _: f32, _: TextAlign) {
            self.labels.push(text.to_owned());
        }
    }

    #[test]
    fn initial_screen_renders_the_four_empty_tracks() {
        let viewport = Size {
            width: 1280.0,
            height: 800.0,
        };
        let mut painter = TestPainter::default();
        Renderer::new().render(
            build(&Project::new("Untitled"), viewport),
            viewport,
            &mut painter,
        );
        assert!(painter.panels >= 4);
        for label in ["Ocus", "Media bin", "Preview", "Timeline"] {
            assert!(painter.labels.iter().any(|text| text == label));
        }
        for label in ["Video 1", "Video 2", "Audio 1", "Audio 2"] {
            assert!(painter.labels.iter().any(|text| text.starts_with(label)));
        }
    }
}
