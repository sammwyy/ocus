use super::controls::{
    AUDIO, DragOverlay, ExportMeter, Glyph, IMAGE, Layer, PLAYHEAD, STAGE, Scrubber, TITLE, Thumb,
    VIDEO, ZoomSlider, accent, accent_button, background, border, chip, faint, fit, glyph, heading,
    icon_button, kind_color, list_row, media_item, menu_row, muted, panel, panel_raised,
    play_button, secondary_button, segment, short_duration, sized_icon_button, surface, text,
    text_color, timecode, tool_button,
};
use super::edits::ClipRef;
use super::interaction::{Action, DragPayload, MenuState};
use super::state::{MediaView, SourceRange, TitleGesture, ZOOM_SECONDS, audible, media_frames};
use super::timeline_view::{ClipLook, Lane, LaneClip, TimelineView};
use super::title_markup;
use super::{MediaFilter, Sidebar, UiState};
use crate::media::preview::StoryboardKey;
use crate::media::{Media, MediaKind};
use crate::project::{Command, Project};
use crate::settings::ThemeMode;
use crate::timeline::{Clip, ClipContent, Title, TrackKind};
use creamui::core::layout::{Dimension, Style};
use creamui::core::{Painter, Point, Rect, TextAlign, Widget};
use creamui::fonts::{FontWeight, HorizontalAlign, LayoutSettings, cached_layout, resolve};
use creamui::widgets::layout::Flex;
use creamui::widgets::layout::{Align, Grid, Justify, StyleExt};
use creamui::widgets::{
    AppTitlebar, Checkbox, ColorPicker, RawScrollView, TextArea, TextController, TextInput,
};
use creamui::{BoxedWidget, Color, GpuFrameSlot, GpuViewport, ImageData, Size, Styled};
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

impl Widget for Logo {
    fn style(&self) -> creamui::Style {
        Style::default().size(26.0, 26.0).into()
    }

    fn paint(&self, painter: &mut dyn Painter, rect: Rect) {
        painter.draw_image(rect, logo().image(), None);
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
    state.edit(edit(&clip, track_id, next_id));
}

fn set_title_text(state: &UiState) {
    change_title(state, |title| title.text = state.title_text.peek());
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
        state.edit(Command::SetTitle {
            track_id,
            clip_id: selected.id,
            title,
        });
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

fn program_layers(project: &Project, frame: u64) -> (bool, Vec<(ClipRef, Title)>) {
    let mut picture = false;
    let mut titles = Vec::new();
    for track in project
        .tracks()
        .iter()
        .filter(|track| track.kind == TrackKind::Video)
    {
        for clip in &track.clips {
            if clip.start_frame > frame || frame - clip.start_frame >= clip.duration_frames {
                continue;
            }
            match &clip.content {
                ClipContent::Title(title) => titles.push(((track.id, clip.id), title.clone())),
                ClipContent::Media { media_id } => {
                    picture |= project
                        .media()
                        .iter()
                        .any(|media| media.id == *media_id && media.kind != MediaKind::Audio);
                }
            }
        }
    }
    (picture, titles)
}

struct ProgramCanvas {
    slot: GpuFrameSlot,
    picture: bool,
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

fn title_box(title: &Title, rect: Rect, aspect: f32, resolution: (u32, u32)) -> Rect {
    let width = rect.width.min(rect.height * aspect);
    let height = rect.height.min(rect.width / aspect);
    let origin_x = rect.x + (rect.width - width) / 2.0;
    let origin_y = rect.y + (rect.height - height) / 2.0;
    let size = (title.font_size * width / resolution.0.max(1) as f32)
        .max(1.0)
        .min(height * 2.0);
    let mut longest = 0.0_f32;
    let mut line_width = 0.0;
    let mut lines = 1;
    for run in title_markup::parse(title) {
        for (index, part) in run.text.split('\n').enumerate() {
            if index > 0 {
                longest = longest.max(line_width);
                line_width = 0.0;
                lines += 1;
            }
            line_width += run_width(part, size, &title.font_family, run.bold);
        }
    }
    let box_width = (longest.max(line_width) + 16.0).clamp(32.0, width.max(32.0));
    let box_height = (lines as f32 * size * 1.35 + 10.0).min(height);
    Rect {
        x: (origin_x + width * title.x - box_width / 2.0)
            .clamp(origin_x, origin_x + (width - box_width).max(0.0)),
        y: (origin_y + height * title.y - box_height / 2.0)
            .clamp(origin_y, origin_y + (height - box_height).max(0.0)),
        width: box_width,
        height: box_height,
    }
}

impl ProgramCanvas {
    fn title_box(&self, rect: Rect) -> Option<Rect> {
        if rect.width.min(rect.height * self.aspect) <= 16.0 || rect.height <= 0.0 {
            return None;
        }
        let selected = self.selected?;
        let title = &self.titles.iter().find(|(key, _)| *key == selected)?.1;
        Some(title_box(title, rect, self.aspect, self.resolution))
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
        let width = rect.width.min(rect.height * self.aspect);
        let height = rect.height.min(rect.width / self.aspect);
        let bounds = Rect {
            x: rect.x + (rect.width - width) / 2.0,
            y: rect.y + (rect.height - height) / 2.0,
            width,
            height,
        };
        if width <= 16.0 || height <= 0.0 {
            return;
        }
        let scale = width / self.resolution.0.max(1) as f32;
        painter.push_clip_rounded(bounds, 0.0);
        for (_, title) in &self.titles {
            let size = (title.font_size * scale).max(1.0).min(height * 2.0);
            let runs = title_markup::parse(title);
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
                    .map(|run| run_width(&run.text, size, &title.font_family, run.bold))
                    .collect();
                let line_width: f32 = widths.iter().sum();
                let mut x = bounds.x + width * title.x - line_width / 2.0;
                for (run, text_width) in line.iter().zip(widths) {
                    let line_rect = Rect {
                        x,
                        y: start_y + index as f32 * line_height,
                        width: text_width.max(1.0),
                        height: line_height,
                    };
                    if !run.text.trim().is_empty() && run.color.a > 0 {
                        if title.outline {
                            for (dx, dy) in [(-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0)] {
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
                                    Some(&title.font_family),
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
                            Some(&title.font_family),
                            run.bold,
                            run.italic,
                        );
                    }
                    x += text_width;
                }
            }
        }
        painter.pop_clip();
        if let Some(box_rect) = self.title_box(rect) {
            painter.stroke_rect(box_rect, accent(), 1.5, 0.0);
            painter.fill_rect(
                Rect {
                    x: box_rect.x + box_rect.width - 5.0,
                    y: box_rect.y + box_rect.height - 5.0,
                    width: 10.0,
                    height: 10.0,
                },
                accent(),
                2.0,
            );
        }
    }

    fn on_drag_start(&self) -> Option<Rc<dyn Fn(Point, Rect)>> {
        let selected = self.selected?;
        let title = self
            .titles
            .iter()
            .find(|(key, _)| *key == selected)?
            .1
            .clone();
        let state = self.state.clone();
        let aspect = self.aspect;
        let resolution = self.resolution;
        Some(Rc::new(move |pointer, rect| {
            if rect.width.min(rect.height * aspect) <= 16.0 || rect.height <= 0.0 {
                return;
            }
            let box_rect = title_box(&title, rect, aspect, resolution);
            let position = Point {
                x: pointer.x + rect.x,
                y: pointer.y + rect.y,
            };
            if position.x < box_rect.x - 5.0
                || position.x > box_rect.x + box_rect.width + 5.0
                || position.y < box_rect.y - 5.0
                || position.y > box_rect.y + box_rect.height + 5.0
            {
                return;
            }
            state.pause();
            *state.title_gesture.borrow_mut() = Some(TitleGesture {
                clip: selected,
                title: title.clone(),
                pointer,
                resize: position.x > box_rect.x + box_rect.width - 12.0
                    && position.y > box_rect.y + box_rect.height - 12.0,
            });
        }))
    }

    fn on_drag(&self) -> Option<Rc<dyn Fn(Point, Rect)>> {
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
        }))
    }

    fn on_drag_end(&self) -> Option<Rc<dyn Fn()>> {
        let state = self.state.clone();
        Some(Rc::new(move || {
            let Some(gesture) = state.title_gesture.borrow_mut().take() else {
                return;
            };
            let draft = state.title_preview.peek();
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
            }
        }))
    }
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
                                .child(text(media_details(media), muted(), 10.0)),
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
            panel_header(
                "Media",
                text(
                    if count == project.media().len() {
                        format!("{count}")
                    } else {
                        format!("{count} of {}", project.media().len())
                    },
                    faint(),
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
            _ => ("Missing source".into(), muted()),
        };
        subtitle = match (&clip.content, track_kind) {
            (ClipContent::Title(_), _) => "Title clip",
            (_, TrackKind::Audio) => "Audio clip",
            _ if media.is_some_and(|media| media.kind == MediaKind::Image) => "Still clip",
            _ if clip.linked_audio => "Video + linked audio",
            _ => "Video clip",
        };
        if let Some(media) = media {
            rows.push(Box::new(Thumb {
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
            timing.push(property(
                "Source in",
                timecode(clip.source_start_frame, fps),
            ));
        }
        rows.push(section("Timing", timing));
        if let ClipContent::Title(title) = &clip.content {
            rows.push(section(
                "Title",
                vec![
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
                    text("Font family", muted(), 11.0),
                    input(&state.title_font, inner),
                    text("Font size (px)", muted(), 11.0),
                    input(&state.title_size, inner),
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
                    input(&state.title_color, inner),
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
                    title_toggle(state, "Parse HTML tags", title.parse_html, |title| {
                        title.parse_html = !title.parse_html
                    }),
                    title_toggle(state, "Parse Markdown", title.parse_markdown, |title| {
                        title.parse_markdown = !title.parse_markdown
                    }),
                ],
            ));
        }
        if (track_kind == TrackKind::Audio || clip.linked_audio) && media.is_some() {
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
                                if clip.muted { muted() } else { AUDIO },
                                16.0,
                            ))
                            .child(Box::new(
                                Flex::row()
                                    .width(inner - 136.0)
                                    .height(6.0)
                                    .background(surface())
                                    .corner_radius(3.0)
                                    .child(Box::new(
                                        Flex::row()
                                            .width(((inner - 136.0) * volume / 2.0).max(0.0))
                                            .height(6.0)
                                            .background(if clip.muted { faint() } else { AUDIO })
                                            .corner_radius(3.0),
                                    )),
                            ))
                            .child(Box::new(Flex::row().width(36.0).child(text(
                                format!("{}%", (volume * 100.0).round() as u32),
                                text_color(),
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
                    text("Volume and mute affect playback.", faint(), 10.0),
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
                        .child(Box::new(Flex::row().width(86.0).child(text(
                            "Position",
                            muted(),
                            11.0,
                        ))))
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
                        .child(text("1 frame", faint(), 10.0)),
                ),
                Box::new(
                    Flex::row()
                        .gap(4.0)
                        .align(Align::Center)
                        .child(Box::new(Flex::row().width(86.0).child(text(
                            "Length",
                            muted(),
                            11.0,
                        ))))
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
                        .child(text("1 frame", faint(), 10.0)),
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
                        .child(Box::new(Flex::row().width(112.0).child(text(
                            keys,
                            text_color(),
                            10.0,
                        ))))
                        .child(text(action, muted(), 10.0)),
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
            .background(panel())
            .corner_radius(10.0)
            .child(panel_header(
                "Inspector",
                text(subtitle, faint(), 11.0),
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
            removable: project
                .tracks()
                .iter()
                .filter(|other| other.kind == track.kind)
                .count()
                > 1,
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
                        linked_audio: clip.linked_audio,
                    }
                })
                .collect(),
        })
        .collect()
}

pub(crate) fn build(state: &UiState, viewport: Size) -> BoxedWidget {
    state.sync_system_theme();
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
                .child(text(fit(&project_label, 220.0, 12.0), text_color(), 12.0))
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
                .child(icon_button(
                    Glyph::Settings,
                    true,
                    state,
                    UiState::open_settings,
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
                        "Export",
                        !busy && state.sequence_end() > 0,
                        state,
                        UiState::export_dialog,
                    )
                }),
        ));

    let frame = state.program.position.get();
    let timeline_end = state.sequence_end();
    let (picture, mut titles) = program_layers(project, frame);
    if let Some((key, draft)) = state.title_preview.get()
        && let Some((_, title)) = titles.iter_mut().find(|(clip, _)| *clip == key)
    {
        *title = draft;
    }
    let empty_timeline = project.tracks().iter().all(|track| track.clips.is_empty());
    let first_media = project.media().first();
    let program_stage: BoxedWidget = if picture && !gpu_preview && titles.is_empty() {
        stage_message(
            Glyph::Film,
            "Video preview unavailable",
            "This display cannot show video preview.",
            None,
        )
    } else if picture || !titles.is_empty() {
        Box::new(ProgramCanvas {
            slot: state.program.slot.clone(),
            picture: picture && gpu_preview,
            titles,
            selected: state.selected.get().last().copied(),
            state: state.clone(),
            aspect,
            resolution: (project.settings().width, project.settings().height),
        })
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
    let mut has_picture = false;
    let mut playable = false;
    for track in project.tracks() {
        for clip in &track.clips {
            if track.kind == TrackKind::Video && matches!(clip.content, ClipContent::Title(_)) {
                playable = true;
            }
            let ClipContent::Media { media_id } = clip.content else {
                continue;
            };
            let Some(media) = project.media().iter().find(|media| media.id == media_id) else {
                continue;
            };
            if track.kind == TrackKind::Video && media.kind != MediaKind::Audio {
                has_picture = true;
                playable = true;
            } else if audible(track.kind, clip, media) {
                playable = true;
            }
        }
    }
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
        program_stage,
        transport(
            state,
            Transport {
                position: frame,
                end: timeline_end,
                fps,
                playing: program_playing,
                can_play: playable && (gpu_preview || !has_picture),
                color: accent(),
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
            "Audio plays through the default output device.",
            None,
        ),
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
                can_play: range
                    .as_ref()
                    .is_some_and(|r| gpu_preview || r.kind == MediaKind::Audio),
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
                .child(Box::new(ZoomSlider {
                    level: ZOOM_SECONDS.len() - 1 - zoom.min(ZOOM_SECONDS.len() - 1),
                    levels: ZOOM_SECONDS.len(),
                    set: {
                        let state = state.clone();
                        Rc::new(move |level| state.zoom_timeline(ZOOM_SECONDS.len() - 1 - level))
                    },
                }))
                .child(icon_button(Glyph::ZoomIn, zoom > 0, state, |s| {
                    s.zoom_timeline(s.timeline_zoom.peek().saturating_sub(1))
                }))
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
                .child(Box::new(
                    Flex::column()
                        .width(side_width)
                        .gap(6.0)
                        .child(Box::new(
                            Flex::row()
                                .gap(2.0)
                                .padding(2.0)
                                .background(panel())
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
        .child(text(state.status.get(), muted(), 11.0))
        .child(spacer());
    if state.exporting.get()
        && let Some(control) = state.export_control()
    {
        status = status
            .child(text("Exporting", muted(), 10.0))
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
        faint(),
        10.0,
    ));

    Box::new(
        Flex::column()
            .size(viewport.width, viewport.height)
            .gap(6.0)
            .background(background())
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
            })
            .child(match state.menu.get() {
                Some(menu) => context_menu(state, menu, viewport),
                None => Box::new(Flex::row().size(0.0, 0.0)),
            }),
    )
}

pub(crate) fn build_settings(state: &UiState, viewport: Size) -> BoxedWidget {
    state.sync_system_theme();
    let settings = state.settings.get();
    let theme = creamui::use_theme();
    let colors = theme.colors;
    let options = [
        ("System", ThemeMode::System),
        ("Light", ThemeMode::Light),
        ("Dark", ThemeMode::Dark),
        ("Midnight", ThemeMode::Midnight),
    ]
    .into_iter()
    .map(|(label, mode)| {
        chip(label, settings.theme == mode, state, move |state| {
            state.set_theme_mode(mode)
        })
    })
    .collect::<Vec<_>>();
    Box::new(
        Flex::column()
            .size(viewport.width, viewport.height)
            .padding(24.0)
            .gap(16.0)
            .background(colors.surface)
            .child(heading("Editor settings", colors.text_primary, 20.0))
            .child(text("Appearance", colors.text_secondary, 13.0))
            .child(Box::new(append(Flex::row().gap(6.0), options)))
            .child(text("Accent color", colors.text_secondary, 13.0))
            .child(Box::new(append(
                Flex::column().gap(6.0),
                creamui::ThemeDefinition::builtin_default()
                    .accent_presets
                    .chunks(3)
                    .map(|row| {
                        Box::new(append(
                            Flex::row().gap(6.0),
                            row.iter()
                                .map(|preset| {
                                    let id = preset.id.clone();
                                    if settings.theme == ThemeMode::System {
                                        secondary_button(None, &preset.name, false, state, |_| {})
                                    } else {
                                        chip(
                                            &preset.name,
                                            settings.accent.as_deref().unwrap_or("orange") == id,
                                            state,
                                            move |state| state.set_accent(&id),
                                        )
                                    }
                                })
                                .collect(),
                        )) as BoxedWidget
                    })
                    .collect(),
            )))
            .child(text(
                if settings.theme == ThemeMode::System {
                    "System theme and accent are provided by CreamUI"
                } else {
                    "Changes are saved automatically"
                },
                colors.text_secondary,
                11.0,
            )),
    )
}

/// The open context menu over a full-window layer that closes it when
/// clicked outside.
fn context_menu(state: &UiState, menu: MenuState, viewport: Size) -> BoxedWidget {
    const WIDTH: f32 = 244.0;
    let separators = menu.entries.iter().filter(|e| e.command.is_none()).count() as f32;
    let rows = menu.entries.len() as f32 - separators;
    let height = 12.0 + 26.0 + rows * 28.0 + separators * 9.0;
    let x = menu.anchor.x.min(viewport.width - WIDTH - 8.0).max(8.0);
    let y = if menu.anchor.y + height > viewport.height - 8.0 {
        (menu.anchor.y - height).max(8.0)
    } else {
        menu.anchor.y
    };
    let entries = menu
        .entries
        .iter()
        .map(|entry| match entry.command {
            Some(command) => menu_row(
                entry.icon,
                &entry.label,
                entry.shortcut,
                entry.enabled,
                entry.danger,
                state,
                move |s| s.run_menu(command),
            ),
            None => Box::new(
                Flex::row()
                    .height(9.0)
                    .align(Align::Center)
                    .padding_xy(8.0, 0.0)
                    .child(Box::new(
                        Flex::row().height(1.0).grow(1.0).background(border()),
                    )),
            ) as BoxedWidget,
        })
        .collect();
    let close = state.clone();
    Box::new(Layer {
        style: Layer::at(0.0, 0.0, viewport.width, viewport.height),
        on_click: Some(Rc::new(move || close.close_menu())),
        children: vec![Box::new(Layer {
            style: Layer::at(x, y, WIDTH, height),
            on_click: None,
            children: vec![Box::new(append(
                Flex::column()
                    .size(WIDTH, height)
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
                    .child(Box::new(
                        Flex::row()
                            .height(26.0)
                            .align(Align::Center)
                            .padding_xy(10.0, 0.0)
                            .child(heading(fit(&menu.title, WIDTH - 32.0, 11.0), muted(), 11.0)),
                    )),
                entries,
            ))],
        })],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use creamui::core::{Painter, Point, Rect, Renderer, RgbaImage, TextAlign, Widget};
    use std::sync::Arc;

    #[derive(Default)]
    struct TestPainter {
        labels: Vec<String>,
        bounds: Vec<(String, Rect)>,
        images: Vec<Rect>,
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
            self.text_colors.push((text.to_owned(), color));
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
            picture: true,
            titles: vec![((0, 1), title)],
            selected: None,
            state: UiState::new(),
            aspect: 16.0 / 9.0,
            resolution: (1920, 1080),
        }
        .paint(&mut painter, stage);
        assert_eq!(painter.fills.len(), 1);
        assert_eq!(painter.fills[0].1, Color::rgb(32, 32, 32));
        assert!(painter.labels.iter().all(|label| label == "Over footage"));
        let (_, rect) = painter.bounds.last().unwrap();
        assert!(rect.y > stage.height / 2.0);
        assert_eq!(
            painter.text_colors.last().unwrap().1,
            Color::rgba(245, 210, 80, 160)
        );
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
                .any(|(drawn, _)| *drawn == canvas.title_box(rect).unwrap())
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
        let box_rect = canvas.title_box(rect).unwrap();
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
        let viewport = Size {
            width: 420.0,
            height: 310.0,
        };
        creamui::reactive::with_context_scope(|| {
            creamui::reactive::provide_context(creamui::theme::ThemeProvider::new(
                state.settings.peek().theme(),
            ));
            let mut painter = TestPainter::default();
            Renderer::new().render(build_settings(&state, viewport), viewport, &mut painter);
            for preset in creamui::ThemeDefinition::builtin_default().accent_presets {
                assert!(painter.labels.contains(&preset.name));
            }
        });
        state.set_theme_mode(ThemeMode::System);
        assert!(state.settings.peek().accent.is_none());
    }

    #[test]
    fn editor_palette_tracks_the_window_theme() {
        for mode in [ThemeMode::Light, ThemeMode::Dark, ThemeMode::Midnight] {
            creamui::reactive::with_context_scope(|| {
                let settings = crate::settings::Settings {
                    theme: mode,
                    accent: None,
                };
                let theme = settings.theme();
                creamui::reactive::provide_context(creamui::ThemeProvider::new(theme));
                assert_eq!(background(), theme.colors.surface);
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
            selected: state.selected.peek(),
            ghosts: Vec::new(),
            dragging: false,
            marquee: None,
            scroll: 0.0,
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
                removable: false,
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
