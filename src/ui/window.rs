use super::controls::{
    AUDIO, DragOverlay, ExportMeter, Glyph, Help, IMAGE, Layer, PLAYHEAD, STAGE, Scrubber, TITLE,
    Thumb, VIDEO, ZoomSlider, accent, accent_button, background, border, chip, faint, fit, glyph,
    heading, icon_button, kind_color, list_row, media_item, menu_row, muted, panel, panel_raised,
    play_button, secondary_button, segment, short_duration, sized_icon_button, surface,
    surface_hover, text, text_color, timecode, tool_button,
};
use super::edits::ClipRef;
use super::interaction::{Action, DragPayload, MenuState};
use super::state::{
    ExportPage, MediaView, PlaybackMonitor, SequenceContent, SettingsPage, SourceRange,
    TitleGesture, ZOOM_SECONDS, media_frames,
};
use super::timeline_view::{ClipLook, Lane, LaneClip, TimelineView};
use super::waveform::WaveformSlice;
use super::{MediaFilter, Sidebar, UiState};
use super::{title_fonts, title_markup};
use crate::media::audio::carries_sound;
use crate::media::composite::title_outline_radius;
use crate::media::preview::StoryboardKey;
use crate::media::{Media, MediaKind};
use crate::project::export_settings::{AudioCodec, Container, EncodingSpeed, VideoCodec};
use crate::project::{Command, Project};
use crate::settings::ThemeMode;
use crate::timeline::{Clip, ClipContent, Title, Track, TrackKind};
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
        let progress =
            (self.position as f64 / self.waveform.duration.max(1) as f64).clamp(0.0, 1.0);
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

fn adjust_timing(state: &UiState, source: bool, seconds: bool, increase: bool) {
    let Some((track_id, clip)) = selected_clip(state) else {
        return;
    };
    let fps = state.frame_rate();
    let step = if seconds {
        state.frames_per_second_step()
    } else {
        1
    };
    let adjust = |value: u64| {
        if increase {
            value.saturating_add(step)
        } else {
            value.saturating_sub(step)
        }
    };
    let editor = state.editor.borrow();
    let project = editor.project();
    let command = if source {
        let maximum = match clip.content {
            ClipContent::Media { media_id } => project
                .media()
                .iter()
                .find(|media| media.id == media_id)
                .filter(|media| media.kind != MediaKind::Image)
                .and_then(|media| media_frames(media, fps))
                .map(|frames| frames.saturating_sub(clip.duration_frames))
                .unwrap_or(u64::MAX - clip.duration_frames),
            _ => return,
        };
        Some(Command::Trim {
            track_id,
            clip_id: clip.id,
            start_frame: clip.start_frame,
            source_start_frame: adjust(clip.source_start_frame).min(maximum),
            duration_frames: clip.duration_frames,
        })
    } else {
        crate::project::edit::trim_edge(
            project,
            track_id,
            clip.id,
            false,
            clip.start_frame
                .saturating_add(adjust(clip.duration_frames).max(1)),
        )
    };
    drop(editor);
    if let Some(command) = command {
        state.edit(command);
    }
}

fn timing_steps(state: &UiState, source: bool) -> BoxedWidget {
    let mut row = Flex::row().gap(4.0);
    for (label, seconds, increase) in [
        ("−1 s", true, false),
        ("−1 f", false, false),
        ("+1 f", false, true),
        ("+1 s", true, true),
    ] {
        row = row.child(secondary_button(None, label, true, state, move |state| {
            adjust_timing(state, source, seconds, increase)
        }));
    }
    Box::new(row)
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

fn change_clip_fade(state: &UiState, incoming: bool, increase: bool) {
    if let Some((track_id, clip)) = selected_clip(state) {
        let fps = state.frame_rate();
        let step = (f64::from(fps.0) / f64::from(fps.1) / 2.0).round().max(1.0) as u64;
        let (current, other) = if incoming {
            (clip.fade_in_frames, clip.fade_out_frames)
        } else {
            (clip.fade_out_frames, clip.fade_in_frames)
        };
        let value = if increase {
            current
                .saturating_add(step)
                .min(clip.duration_frames.saturating_sub(other))
        } else {
            current.saturating_sub(step)
        };
        state.edit(Command::SetClipFades {
            track_id,
            clip_id: clip.id,
            fade_in_frames: if incoming { value } else { other },
            fade_out_frames: if incoming { other } else { value },
        });
    }
}

fn fade_row(state: &UiState, clip: &Clip, incoming: bool) -> Box<dyn Widget> {
    let (label, value, other) = if incoming {
        ("Fade in", clip.fade_in_frames, clip.fade_out_frames)
    } else {
        ("Fade out", clip.fade_out_frames, clip.fade_in_frames)
    };
    let fps = state.frame_rate();
    let length = if value == 0 {
        "Off".into()
    } else {
        format!(
            "{:.1} s",
            value as f64 * f64::from(fps.1) / f64::from(fps.0)
        )
    };
    Box::new(
        Flex::row()
            .gap(6.0)
            .align(Align::Center)
            .child(text(label, text_color(), 11.0))
            .child(spacer())
            .child(text(length, text_color(), 11.0))
            .child(sized_icon_button(
                Glyph::Minus,
                Help::new(if incoming {
                    "Decrease fade in"
                } else {
                    "Decrease fade out"
                }),
                24.0,
                value > 0,
                false,
                state,
                move |s| change_clip_fade(s, incoming, false),
            ))
            .child(sized_icon_button(
                Glyph::Plus,
                Help::new(if incoming {
                    "Increase fade in"
                } else {
                    "Increase fade out"
                }),
                24.0,
                value < clip.duration_frames.saturating_sub(other),
                false,
                state,
                move |s| change_clip_fade(s, incoming, true),
            )),
    )
}

fn toggle_clip_noise_reduction(state: &UiState) {
    if let Some((track_id, clip)) = selected_clip(state) {
        state.edit(Command::SetClipNoiseReduction {
            track_id,
            clip_id: clip.id,
            enabled: !clip.noise_reduction,
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
    let box_width = (longest.max(line_width) + 16.0).max(32.0);
    let box_height = lines as f32 * size * 1.35 + 10.0;
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
    fn selected_title_box(&self, bounds: Rect) -> Option<Rect> {
        let selected = self.selected?;
        let title = &self.titles.iter().find(|(key, _)| *key == selected)?.1;
        Some(title_box(title, bounds, self.resolution))
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
        for (_, title) in &self.titles {
            if self.picture {
                continue;
            }
            let size = (title.font_size * scale).max(1.0).min(height * 2.0);
            let family = title_fonts::family_stack(&title.font_family);
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
                    .map(|run| run_width(&run.text, size, &family, run.bold))
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
                                .child(text(
                                    if missing {
                                        "Missing · Relink to restore".into()
                                    } else {
                                        media_details(media)
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
                        Help::new("Grid view"),
                        26.0,
                        true,
                        view == MediaView::Grid,
                        state,
                        |s| s.media_view.set(MediaView::Grid),
                    ),
                    sized_icon_button(
                        Glyph::List,
                        Help::new("List view"),
                        26.0,
                        true,
                        view == MediaView::List,
                        state,
                        |s| s.media_view.set(MediaView::List),
                    ),
                    sized_icon_button(
                        Glyph::Import,
                        Help::new("Import media").shortcut("Ctrl+I"),
                        26.0,
                        !busy,
                        false,
                        state,
                        |s| s.import_dialog(),
                    ),
                ],
            ),
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
        .filter(|track| track.id != track_id && track.kind == source.kind)
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
            timing.push(timing_steps(state, true));
        }
        timing.push(text("Length", muted(), 11.0));
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
                    text("Font family", muted(), 11.0),
                    Box::new(title_input(
                        &state.title_font,
                        inner,
                        state,
                        apply_title_style,
                    )),
                    text("Font size (px)", muted(), 11.0),
                    Box::new(title_input(
                        &state.title_size,
                        inner,
                        state,
                        apply_title_style,
                    )),
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
            let audio_state = state.clone();
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
                                Help::new("Decrease clip volume"),
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
                                Help::new("Increase clip volume"),
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
                    Box::new(
                        Flex::row()
                            .gap(8.0)
                            .align(Align::Center)
                            .child(Box::new(Checkbox::new(clip.noise_reduction, move || {
                                toggle_clip_noise_reduction(&audio_state);
                            })))
                            .child(text("Noise reduction", text_color(), 11.0)),
                    ),
                    text("Best for speech with background noise.", faint(), 10.0),
                    fade_row(state, &clip, true),
                    fade_row(state, &clip, false),
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
                        "Volume, mute and fades affect playback and export.",
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
                            Help::new("Shorten clip by one frame"),
                            26.0,
                            clip.duration_frames > 1,
                            false,
                            state,
                            |s| adjust_timing(s, false, false, false),
                        ))
                        .child(sized_icon_button(
                            Glyph::Plus,
                            Help::new("Lengthen clip by one frame"),
                            26.0,
                            true,
                            false,
                            state,
                            |s| adjust_timing(s, false, false, true),
                        ))
                        .child(text("1 frame", faint(), 10.0)),
                ),
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
    let draft = state.trim_draft.get();
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
                    LaneClip {
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
                        waveform: media
                            .filter(|media| {
                                !state.missing_media.get().contains(&media.id)
                                    && carries_sound(track.kind, clip, media)
                            })
                            .and_then(|media| state.waveform(&media.path))
                            .map(|peaks| WaveformSlice {
                                peaks,
                                start: clip.source_start_frame,
                                duration: clip.duration_frames,
                                fps,
                            }),
                        volume: clip.volume,
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
    state.tooltip_viewport.set(viewport);
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
            icon_button(
                icon,
                match icon {
                    Glyph::Document => Help::new("New project").shortcut("Ctrl+N"),
                    Glyph::Folder => Help::new("Open project").shortcut("Ctrl+O"),
                    _ => Help::new("Save project").shortcut("Ctrl+S"),
                },
                !busy,
                state,
                operation,
            )
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
                    Help::new("Undo").shortcut("Ctrl+Z"),
                    !busy && editor.can_undo(),
                    state,
                    UiState::undo,
                ))
                .child(icon_button(
                    Glyph::Redo,
                    Help::new("Redo").shortcut("Ctrl+Shift+Z / Ctrl+Y"),
                    !busy && editor.can_redo(),
                    state,
                    UiState::redo,
                ))
                .child(Box::new(Flex::row().width(6.0)))
                .child(icon_button(
                    Glyph::Settings,
                    Help::new("Settings"),
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
                        if project.last_export().is_some() {
                            "Export again"
                        } else {
                            "Export"
                        },
                        !busy && state.sequence_end() > 0,
                        state,
                        UiState::quick_export,
                    )
                })
                .child(
                    if project.last_export().is_some() && !state.exporting.get() {
                        secondary_button(None, "Options…", !busy, state, UiState::export_dialog)
                    } else {
                        Box::new(Flex::row().width(0.0)) as BoxedWidget
                    },
                ),
        ));

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
        program_stage,
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
    let source_position = state.source.position.get();
    let source_stage: BoxedWidget = match (&range, &selected_title) {
        (Some(range), _) if range.kind == MediaKind::Audio => {
            if let Some(peaks) = state.waveform(&range.path) {
                Box::new(SourceWaveform {
                    waveform: WaveformSlice {
                        peaks,
                        start: range.source_start,
                        duration: range.duration,
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

    let content = Box::new(
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
            })
            .child(if state.export_modal.get() {
                export_modal(state, project, viewport)
            } else {
                Box::new(Flex::row().size(0.0, 0.0)) as BoxedWidget
            })
            .child(if state.settings_modal.get() {
                settings_modal(state, viewport)
            } else {
                Box::new(Flex::row().size(0.0, 0.0)) as BoxedWidget
            }),
    );
    state.finish_tooltips();
    content
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
                "Export",
                !state.busy.get(),
                state,
                UiState::submit_export,
            )),
    );
    let status = state.status.get();
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
                .child(text(fit(&status, inner - 200.0, 11.0), muted(), 11.0))
                .child(footer_actions),
        )
    };
    let mut footer = Flex::column().width(inner).gap(8.0).child(Box::new(
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
                                .width(width - 40.0)
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
                                            if page == SettingsPage::Appearance {
                                                content
                                            } else {
                                                about_settings(state, content_width)
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

    fn destination_clip(start_frame: u64, duration_frames: u64) -> Clip {
        Clip {
            id: 10,
            start_frame,
            source_start_frame: 0,
            duration_frames,
            content: ClipContent::Title(Title::default()),
            volume: 1.0,
            muted: false,
            fade_in_frames: 0,
            fade_out_frames: 0,
            noise_reduction: false,
            linked_audio: false,
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
            clips: Vec::new(),
        }];
        for (index, (start, duration)) in [(0, 10), (30, 10), (9, 2), (29, 2), (15, 5), (0, 40)]
            .into_iter()
            .enumerate()
        {
            tracks.push(Track {
                id: index as u64 + 1,
                kind: TrackKind::Video,
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
    fn hovered_help_replaces_an_adjacent_focused_hint() {
        let state = UiState::new();
        render(&state, 1280.0, 760.0);
        let undo = state.tooltip_controller(("Undo", 0));
        let redo = state.tooltip_controller(("Redo", 0));
        undo.focus(true);
        redo.hover(true);
        std::thread::sleep(std::time::Duration::from_millis(400));
        render(&state, 1280.0, 760.0);
        let painter = render(&state, 1280.0, 760.0);
        assert!(redo.visible());
        assert!(!undo.visible());
        assert!(painter.labels.iter().any(|label| label == "Redo"));
        assert!(!painter.labels.iter().any(|label| label == "Undo"));
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
    fn title_duration_follows_length_buttons() {
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
    fn inspector_length_steps_are_bounded_and_undoable() {
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
            clip: Clip {
                id: 1,
                start_frame: 0,
                source_start_frame: 0,
                duration_frames: 60,
                content: ClipContent::Media { media_id: 1 },
                volume: 1.0,
                muted: false,
                fade_in_frames: 0,
                fade_out_frames: 0,
                noise_reduction: false,
                linked_audio: false,
            }
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
                removable: false,
                clips: vec![LaneClip {
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
        let painter = render(&state, 1440.0, 900.0);
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
        let painter = render(&state, 1440.0, 900.0);
        assert!(
            painter
                .labels
                .iter()
                .any(|label| label.contains("tone.wav"))
        );
        assert!(painter.labels.iter().any(|label| label == "100%"));
        assert!(painter.labels.iter().any(|label| label == "Audio source"));
        let painter = render(&state, 1440.0, 1600.0);
        assert!(painter.labels.iter().any(|label| label == "Fade in"));
        assert!(painter.labels.iter().any(|label| label == "Fade out"));
        assert!(painter.labels.iter().any(|label| label == "Off"));
        for (label, rect) in &painter.bounds {
            if label == "Fade in" || label == "Fade out" {
                assert!(rect.x >= 0.0 && rect.x + rect.width <= 1440.0);
                assert!(rect.y >= 0.0 && rect.y + rect.height <= 1600.0);
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
        let painter = render(&state, 1440.0, 1600.0);
        assert!(painter.labels.iter().any(|label| label == "0.5 s"));
        assert!(
            painter
                .labels
                .iter()
                .any(|label| label == "Noise reduction")
        );
        assert!(!selected_clip(&state).unwrap().1.noise_reduction);
        toggle_clip_noise_reduction(&state);
        assert!(selected_clip(&state).unwrap().1.noise_reduction);
        state.undo();
        assert!(!selected_clip(&state).unwrap().1.noise_reduction);
        state.redo();
        assert!(selected_clip(&state).unwrap().1.noise_reduction);
        set_clip_audio(&state, |clip| (clip.volume - 0.5, true));
        assert!(selected_clip(&state).unwrap().1.noise_reduction);
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
