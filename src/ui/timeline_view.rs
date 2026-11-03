use super::UiState;
use super::controls::{
    ACCENT, AUDIO, BORDER, FAINT, Glyph, IMAGE, MUTED, PANEL, PANEL_RAISED, PLAYHEAD, STAGE,
    SURFACE, TEXT, TITLE, VIDEO, draw_glyph, fit,
};
use super::edits::ClipRef;
use super::interaction::Gesture;
use super::state::Storyboard;
use crate::timeline::TrackKind;
use creamui::core::layout::{Dimension, Style};
use creamui::core::{Modifiers, Painter, Point, Rect, RgbaImage, TextAlign, Widget};
use creamui::{Color, WheelInput};
use std::rc::Rc;
use std::sync::Arc;

pub(super) const HEADER_WIDTH: f32 = 128.0;
pub(super) const RULER_HEIGHT: f32 = 28.0;
const LANE_GAP: f32 = 3.0;
const CLIP_HEADER: f32 = 17.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ClipLook {
    Video,
    Image,
    Audio,
    Title,
}

impl ClipLook {
    fn color(self) -> Color {
        match self {
            ClipLook::Video => VIDEO,
            ClipLook::Image => IMAGE,
            ClipLook::Audio => AUDIO,
            ClipLook::Title => TITLE,
        }
    }
}

pub(super) struct LaneClip {
    pub clip_id: u64,
    pub start: u64,
    pub duration: u64,
    pub name: String,
    pub look: ClipLook,
    pub images: Option<Storyboard>,
    pub fallback: Option<Arc<RgbaImage>>,
    pub volume: f32,
    pub muted: bool,
}

pub(super) struct Lane {
    pub track_id: u64,
    pub kind: TrackKind,
    pub label: String,
    pub clips: Vec<LaneClip>,
}

/// The sequence ruler, track headers and clip lanes, painted as one surface
/// so the playhead and gridlines can cross every track.
pub(super) struct TimelineView {
    pub lanes: Vec<Lane>,
    pub start: u64,
    pub span: u64,
    pub end: u64,
    pub fps: (u32, u32),
    pub playhead: u64,
    pub selected: Vec<ClipRef>,
    /// Where a drag would place clips, and whether that drop is allowed.
    pub ghosts: Vec<Ghost>,
    /// Selected clips are being dragged, so they are drawn faded in place.
    pub dragging: bool,
    /// Rubber-band selection rectangle, relative to this widget.
    pub marquee: Option<Rect>,
    pub state: UiState,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Ghost {
    pub track_id: u64,
    pub start: u64,
    pub duration: u64,
    pub valid: bool,
}

/// Lane and time layout shared by painting, hit-testing and drop targeting.
#[derive(Clone)]
pub(super) struct Geometry {
    left: f32,
    width: f32,
    start: u64,
    span: u64,
    lanes: Vec<Rect>,
}

impl Geometry {
    pub(super) fn new(rect: Rect, kinds: &[TrackKind], start: u64, span: u64) -> Self {
        let weight = |kind: &TrackKind| if *kind == TrackKind::Video { 1.3 } else { 1.0 };
        let total: f32 = kinds.iter().map(weight).sum::<f32>().max(1.0);
        let available =
            (rect.height - RULER_HEIGHT - LANE_GAP * (kinds.len() as f32 + 1.0)).max(0.0);
        let mut y = rect.y + RULER_HEIGHT + LANE_GAP;
        let lanes = kinds
            .iter()
            .map(|kind| {
                let height = (available * weight(kind) / total).clamp(24.0, 84.0);
                let lane = Rect {
                    x: rect.x,
                    y,
                    width: rect.width,
                    height,
                };
                y += height + LANE_GAP;
                lane
            })
            .collect();
        Self {
            left: rect.x + HEADER_WIDTH,
            width: (rect.width - HEADER_WIDTH).max(1.0),
            start,
            span: span.max(1),
            lanes,
        }
    }

    fn x(&self, frame: u64) -> f32 {
        let offset = frame as f64 - self.start as f64;
        self.left + (offset * f64::from(self.width) / self.span as f64) as f32
    }

    fn frame(&self, x: f32) -> u64 {
        let progress = ((x - self.left) / self.width).clamp(0.0, 1.0);
        self.start
            .saturating_add((f64::from(progress) * self.span as f64).floor() as u64)
    }

    /// The (possibly negative) frame under `x`, without clamping to the view.
    pub(super) fn frame_at(&self, x: f32) -> f64 {
        self.start as f64 + f64::from(x - self.left) * self.span as f64 / f64::from(self.width)
    }

    pub(super) fn pixels_per_frame(&self) -> f32 {
        self.width / self.span as f32
    }

    fn right(&self) -> f32 {
        self.left + self.width
    }

    /// The lane under `y`, if any.
    pub(super) fn lane_at(&self, y: f32) -> Option<usize> {
        self.lanes
            .iter()
            .position(|lane| y >= lane.y && y < lane.y + lane.height + LANE_GAP)
    }

    /// The lane whose center is closest to `y`.
    pub(super) fn nearest_lane(&self, y: f32, accept: impl Fn(usize) -> bool) -> Option<usize> {
        (0..self.lanes.len())
            .filter(|index| accept(*index))
            .min_by(|a, b| {
                let distance = |index: usize| {
                    let lane = self.lanes[index];
                    (lane.y + lane.height / 2.0 - y).abs()
                };
                distance(*a).total_cmp(&distance(*b))
            })
    }

    pub(super) fn in_lanes(&self, point: Point) -> bool {
        point.x >= self.left && point.x <= self.right() && self.lane_at(point.y).is_some()
    }
}

/// Chooses a labelled ruler interval at least ~84 px wide, in seconds, and
/// how many minor ticks divide it.
fn tick_spacing(pixels_per_second: f64) -> (f64, u32) {
    [
        1.0, 2.0, 5.0, 10.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1800.0,
    ]
    .into_iter()
    .find(|seconds| seconds * pixels_per_second >= 84.0)
    .map_or((3600.0, 4), |seconds| {
        (
            seconds,
            if seconds == 1.0 || (seconds as u64).is_multiple_of(5) {
                5
            } else {
                4
            },
        )
    })
}

/// Aligns a thin line to the physical pixel grid so it stays crisp.
fn snap(painter: &dyn Painter, value: f32) -> f32 {
    let scale = painter.scale_factor().max(1.0);
    (value * scale).round() / scale
}

fn ruler_label(seconds: f64) -> String {
    let total = seconds.round() as u64;
    if total >= 3600 {
        format!("{}:{:02}:{:02}", total / 3600, total / 60 % 60, total % 60)
    } else {
        format!("{:02}:{:02}", total / 60, total % 60)
    }
}

type HitLane = (u64, Vec<(u64, u64, u64)>);

/// The clip under `point`: `None` outside the lanes, `Some(None)` on a gap.
fn hit(geometry: &Geometry, lanes: &[HitLane], point: Point) -> Option<Option<ClipRef>> {
    if point.x < geometry.left {
        return None;
    }
    let ((track_id, clips), _) = lanes
        .iter()
        .zip(&geometry.lanes)
        .find(|(_, rect)| point.y >= rect.y && point.y < rect.y + rect.height)?;
    let frame = geometry.frame(point.x);
    Some(
        clips
            .iter()
            .find(|(_, start, duration)| frame >= *start && frame - start < *duration)
            .map(|(clip_id, _, _)| (*track_id, *clip_id)),
    )
}

/// Clips overlapping `area` (in the geometry's coordinates).
fn clips_in(geometry: &Geometry, lanes: &[HitLane], area: Rect) -> Vec<ClipRef> {
    let (first, last) = (geometry.frame(area.x), geometry.frame(area.x + area.width));
    lanes
        .iter()
        .zip(&geometry.lanes)
        .filter(|(_, lane)| lane.y < area.y + area.height && area.y < lane.y + lane.height)
        .flat_map(|((track_id, clips), _)| {
            clips
                .iter()
                .filter(move |(_, start, duration)| *start <= last && first < start + duration)
                .map(move |(clip_id, _, _)| (*track_id, *clip_id))
        })
        .collect()
}

fn local(rect: Rect) -> Rect {
    Rect {
        x: 0.0,
        y: 0.0,
        ..rect
    }
}

impl TimelineView {
    fn geometry(&self, rect: Rect) -> Geometry {
        let kinds: Vec<_> = self.lanes.iter().map(|lane| lane.kind).collect();
        Geometry::new(rect, &kinds, self.start, self.span)
    }

    fn hit_lanes(&self) -> Vec<HitLane> {
        self.lanes
            .iter()
            .map(|lane| {
                (
                    lane.track_id,
                    lane.clips
                        .iter()
                        .map(|clip| (clip.clip_id, clip.start, clip.duration))
                        .collect(),
                )
            })
            .collect()
    }

    fn paint_ruler(&self, painter: &mut dyn Painter, rect: Rect, geometry: &Geometry) {
        let ruler = Rect {
            x: geometry.left,
            y: rect.y,
            width: geometry.width,
            height: RULER_HEIGHT,
        };
        painter.fill_rect(ruler, PANEL_RAISED, 0.0);
        painter.fill_text(
            Rect {
                x: rect.x + 12.0,
                y: rect.y,
                width: HEADER_WIDTH - 12.0,
                height: RULER_HEIGHT,
            },
            "TRACKS",
            FAINT,
            10.0,
            TextAlign::Start,
        );
        let fps = f64::from(self.fps.0) / f64::from(self.fps.1.max(1));
        let pixels_per_second = f64::from(geometry.width) * fps / self.span.max(1) as f64;
        let (major, divisions) = tick_spacing(pixels_per_second);
        let minor = major / f64::from(divisions);
        let first = (self.start as f64 / fps / minor).floor() as u64;
        let last = ((self.start + self.span) as f64 / fps / minor).ceil() as u64;
        let bottom = rect.y + RULER_HEIGHT;
        let lanes_bottom = geometry
            .lanes
            .last()
            .map_or(bottom, |lane| lane.y + lane.height);
        for index in first..=last {
            let seconds = index as f64 * minor;
            let x = snap(painter, geometry.x((seconds * fps).round() as u64));
            if x < geometry.left - 0.5 || x > geometry.right() + 0.5 {
                continue;
            }
            let is_major = index % u64::from(divisions) == 0;
            painter.fill_rect(
                Rect {
                    x,
                    y: bottom - if is_major { 9.0 } else { 4.0 },
                    width: 1.0,
                    height: if is_major { 9.0 } else { 4.0 },
                },
                if is_major { MUTED } else { FAINT },
                0.0,
            );
            if is_major {
                painter.fill_rect(
                    Rect {
                        x,
                        y: bottom,
                        width: 1.0,
                        height: lanes_bottom - bottom,
                    },
                    Color::rgba(255, 255, 255, 9),
                    0.0,
                );
                if x + 44.0 <= geometry.right() + 4.0 {
                    painter.fill_text(
                        Rect {
                            x: x + 4.0,
                            y: rect.y + 3.0,
                            width: 48.0,
                            height: 14.0,
                        },
                        &ruler_label(seconds),
                        MUTED,
                        10.0,
                        TextAlign::Start,
                    );
                }
            }
        }
        painter.fill_rect(
            Rect {
                x: rect.x,
                y: bottom,
                width: rect.width,
                height: 1.0,
            },
            BORDER,
            0.0,
        );
    }

    fn paint_header(&self, painter: &mut dyn Painter, lane: &Lane, rect: Rect) {
        let color = if lane.kind == TrackKind::Video {
            VIDEO
        } else {
            AUDIO
        };
        let header = Rect {
            width: HEADER_WIDTH - 6.0,
            ..rect
        };
        painter.fill_rect(header, PANEL_RAISED, 5.0);
        painter.fill_rect(
            Rect {
                width: 3.0,
                ..header
            },
            color,
            1.5,
        );
        let icon = Rect {
            x: header.x + 12.0,
            y: header.y + (header.height - 15.0) / 2.0,
            width: 15.0,
            height: 15.0,
        };
        draw_glyph(
            painter,
            if lane.kind == TrackKind::Video {
                Glyph::Film
            } else {
                Glyph::Volume
            },
            icon,
            color.mix(MUTED, 0.4),
        );
        painter.fill_text_weight(
            Rect {
                x: icon.x + 22.0,
                y: header.y + header.height / 2.0 - 14.0,
                width: 40.0,
                height: 15.0,
            },
            &lane.label,
            TEXT,
            12.0,
            TextAlign::Start,
            true,
            false,
        );
        painter.fill_text(
            Rect {
                x: icon.x + 22.0,
                y: header.y + header.height / 2.0,
                width: HEADER_WIDTH - 50.0,
                height: 13.0,
            },
            &match lane.clips.len() {
                0 => "Empty".to_owned(),
                1 => "1 clip".to_owned(),
                count => format!("{count} clips"),
            },
            FAINT,
            10.0,
            TextAlign::Start,
        );
    }

    fn paint_clip(
        &self,
        painter: &mut dyn Painter,
        clip: &LaneClip,
        lane: Rect,
        geometry: &Geometry,
        selected: bool,
    ) {
        let full = Rect {
            x: geometry.x(clip.start),
            y: lane.y + 2.0,
            width: geometry.x(clip.start.saturating_add(clip.duration)) - geometry.x(clip.start),
            height: lane.height - 4.0,
        };
        let left = full.x.max(geometry.left);
        let right = (full.x + full.width).min(geometry.right());
        if right - left < 1.0 {
            return;
        }
        let body = Rect {
            x: left,
            width: (right - left).max(2.0),
            ..full
        };
        let base = clip.look.color();
        let tone = if clip.muted { 0.8 } else { 0.66 };
        painter.push_clip_rounded(body, 4.0);
        painter.fill_rect(
            body,
            base.mix(PANEL, if selected { tone - 0.12 } else { tone }),
            4.0,
        );
        let content = Rect {
            y: body.y + CLIP_HEADER,
            height: (body.height - CLIP_HEADER).max(0.0),
            ..body
        };
        let images = clip
            .images
            .as_deref()
            .filter(|images| !images.is_empty())
            .map(Vec::as_slice)
            .or_else(|| clip.fallback.as_ref().map(std::slice::from_ref));
        match clip.look {
            ClipLook::Video | ClipLook::Image if content.height > 6.0 => {
                if let Some(images) = images {
                    let aspect = images[0].width() as f32 / images[0].height().max(1) as f32;
                    let tile = (content.height * aspect).max(8.0);
                    let first = ((body.x - full.x) / tile).floor().max(0.0) as usize;
                    let mut index = first;
                    loop {
                        let x = snap(painter, full.x + index as f32 * tile);
                        if x >= right {
                            break;
                        }
                        let center = (index as f32 + 0.5) * tile / full.width.max(1.0);
                        let image = &images
                            [((center * images.len() as f32) as usize).min(images.len() - 1)];
                        painter.draw_image(
                            Rect {
                                x,
                                width: tile,
                                ..content
                            },
                            image,
                            None,
                        );
                        if index > first {
                            painter.fill_rect(
                                Rect {
                                    x,
                                    width: 1.0,
                                    ..content
                                },
                                Color::rgba(0, 0, 0, 120),
                                0.0,
                            );
                        }
                        index += 1;
                    }
                }
            }
            ClipLook::Audio if content.height > 6.0 => {
                let level = (clip.volume / 2.0).clamp(0.0, 1.0);
                let y = snap(painter, content.y + content.height * (1.0 - level));
                painter.fill_rect(
                    Rect {
                        y,
                        height: content.y + content.height - y,
                        ..content
                    },
                    base.mix(PANEL, if clip.muted { 0.85 } else { 0.5 }),
                    0.0,
                );
                painter.fill_rect(
                    Rect {
                        y: y - 1.0,
                        height: 2.0,
                        ..content
                    },
                    if clip.muted { MUTED } else { base },
                    0.0,
                );
                if clip.muted && content.height >= 16.0 && body.width >= 60.0 {
                    draw_glyph(
                        painter,
                        Glyph::Mute,
                        Rect {
                            x: content.x + 6.0,
                            y: content.y + (content.height - 14.0) / 2.0,
                            width: 14.0,
                            height: 14.0,
                        },
                        TEXT,
                    );
                }
            }
            ClipLook::Title if content.height > 14.0 && body.width >= 24.0 => {
                let side = content.height.min(22.0) - 4.0;
                draw_glyph(
                    painter,
                    Glyph::Title,
                    Rect {
                        x: body.x + 6.0,
                        y: content.y + (content.height - side) / 2.0,
                        width: side,
                        height: side,
                    },
                    base.mix(TEXT, 0.5),
                );
            }
            _ => {}
        }
        painter.fill_rect(
            Rect {
                height: CLIP_HEADER.min(body.height),
                ..body
            },
            base.mix(STAGE, if selected { 0.2 } else { 0.42 }),
            0.0,
        );
        let label_space = body.width - 12.0;
        let duration = format!(
            "{:.1}s",
            clip.duration as f64 * f64::from(self.fps.1) / f64::from(self.fps.0.max(1))
        );
        let show_duration = label_space >= 120.0;
        painter.fill_text(
            Rect {
                x: body.x + 6.0,
                y: body.y + 1.0,
                width: label_space.max(0.0),
                height: CLIP_HEADER - 2.0,
            },
            &fit(
                &clip.name,
                label_space - if show_duration { 44.0 } else { 0.0 },
                10.0,
            ),
            TEXT,
            10.0,
            TextAlign::Start,
        );
        if show_duration {
            painter.fill_text(
                Rect {
                    x: body.x + body.width - 50.0,
                    y: body.y + 1.0,
                    width: 44.0,
                    height: CLIP_HEADER - 2.0,
                },
                &duration,
                TEXT.mix(base, 0.4),
                10.0,
                TextAlign::End,
            );
        }
        painter.pop_clip();
        painter.stroke_rect(
            body,
            if selected {
                ACCENT
            } else {
                base.mix(PANEL, 0.3)
            },
            if selected { 2.0 } else { 1.0 },
            4.0,
        );
    }

    fn paint_ghosts(&self, painter: &mut dyn Painter, geometry: &Geometry) {
        for ghost in &self.ghosts {
            let Some(lane) = self
                .lanes
                .iter()
                .position(|lane| lane.track_id == ghost.track_id)
                .map(|index| geometry.lanes[index])
            else {
                continue;
            };
            let x = geometry.x(ghost.start).max(geometry.left);
            let right = geometry
                .x(ghost.start.saturating_add(ghost.duration))
                .min(geometry.right());
            if right <= x {
                continue;
            }
            let color = if ghost.valid { ACCENT } else { PLAYHEAD };
            let body = Rect {
                x,
                y: lane.y + 2.0,
                width: right - x,
                height: lane.height - 4.0,
            };
            painter.fill_rect(body, Color::rgba(color.r, color.g, color.b, 46), 4.0);
            painter.stroke_rect(body, color, 2.0, 4.0);
        }
        if let Some(ghost) = self.ghosts.iter().min_by_key(|ghost| ghost.start)
            && ghost.start >= self.start
            && let Some(first) = geometry.lanes.first()
            && let Some(last) = geometry.lanes.last()
        {
            let x = snap(painter, geometry.x(ghost.start));
            painter.fill_rect(
                Rect {
                    x,
                    y: first.y,
                    width: 1.0,
                    height: last.y + last.height - first.y,
                },
                Color::rgba(255, 255, 255, 90),
                0.0,
            );
        }
    }

    fn paint_playhead(&self, painter: &mut dyn Painter, rect: Rect, geometry: &Geometry) {
        if self.playhead < self.start || self.playhead >= self.start.saturating_add(self.span) {
            return;
        }
        let x = snap(painter, geometry.x(self.playhead));
        let bottom = geometry
            .lanes
            .last()
            .map_or(rect.y + rect.height, |lane| lane.y + lane.height);
        painter.fill_rect(
            Rect {
                x: x - 1.0,
                y: rect.y + 12.0,
                width: 2.0,
                height: bottom - rect.y - 12.0,
            },
            PLAYHEAD,
            0.0,
        );
        painter.fill_rect(
            Rect {
                x: x - 6.0,
                y: rect.y + 2.0,
                width: 12.0,
                height: 12.0,
            },
            PLAYHEAD,
            2.5,
        );
        let step = 1.0 / painter.scale_factor().max(1.0);
        let mut y = 0.0;
        while y < 6.0 {
            let half = 6.0 - y;
            painter.fill_rect(
                Rect {
                    x: x - half,
                    y: rect.y + 14.0 + y,
                    width: half * 2.0,
                    height: step,
                },
                PLAYHEAD,
                0.0,
            );
            y += step;
        }
    }
}

impl Widget for TimelineView {
    fn style(&self) -> creamui::Style {
        Style {
            size: creamui::core::layout::Size {
                width: Dimension::Percent(1.0),
                height: Dimension::Percent(1.0),
            },
            flex_grow: 1.0,
            min_size: creamui::core::layout::Size {
                width: Dimension::Length(0.0),
                height: Dimension::Length(0.0),
            },
            ..Default::default()
        }
        .into()
    }

    fn paint(&self, painter: &mut dyn Painter, rect: Rect) {
        let geometry = self.geometry(rect);
        painter.push_clip(rect);
        for (lane, lane_rect) in self.lanes.iter().zip(&geometry.lanes) {
            painter.fill_rect(
                Rect {
                    x: geometry.left,
                    width: geometry.width,
                    ..*lane_rect
                },
                if lane.kind == TrackKind::Video {
                    Color::rgb(17, 19, 24)
                } else {
                    Color::rgb(15, 18, 20)
                },
                3.0,
            );
        }
        let end_x = geometry.x(self.end).max(geometry.left);
        if end_x < geometry.right()
            && let (Some(first), Some(last)) = (geometry.lanes.first(), geometry.lanes.last())
        {
            painter.fill_rect(
                Rect {
                    x: end_x,
                    y: first.y,
                    width: geometry.right() - end_x,
                    height: last.y + last.height - first.y,
                },
                Color::rgba(0, 0, 0, 70),
                0.0,
            );
        }
        self.paint_ruler(painter, rect, &geometry);
        for (lane, lane_rect) in self.lanes.iter().zip(&geometry.lanes) {
            self.paint_header(painter, lane, *lane_rect);
            if lane.clips.is_empty() && lane_rect.height >= 20.0 {
                painter.fill_text(
                    Rect {
                        x: geometry.left + 12.0,
                        width: geometry.width - 24.0,
                        ..*lane_rect
                    },
                    if lane.kind == TrackKind::Video {
                        "Add video, stills or titles"
                    } else {
                        "Audio from sources lands here"
                    },
                    Color::rgb(58, 64, 74),
                    11.0,
                    TextAlign::Start,
                );
            }
            for clip in &lane.clips {
                let selected = self.selected.contains(&(lane.track_id, clip.clip_id));
                self.paint_clip(painter, clip, *lane_rect, &geometry, selected);
                if selected && self.dragging {
                    let x = geometry.x(clip.start).max(geometry.left);
                    let right = geometry
                        .x(clip.start.saturating_add(clip.duration))
                        .min(geometry.right());
                    if right > x {
                        painter.fill_rect(
                            Rect {
                                x,
                                y: lane_rect.y + 2.0,
                                width: right - x,
                                height: lane_rect.height - 4.0,
                            },
                            Color::rgba(10, 12, 15, 160),
                            4.0,
                        );
                    }
                }
            }
        }
        self.paint_ghosts(painter, &geometry);
        if let Some(marquee) = self.marquee {
            let area = Rect {
                x: rect.x + marquee.x,
                y: rect.y + marquee.y,
                ..marquee
            };
            painter.fill_rect(area, Color::rgba(92, 208, 190, 28), 2.0);
            painter.stroke_rect(area, ACCENT, 1.0, 2.0);
        }
        self.paint_playhead(painter, rect, &geometry);
        painter.fill_rect(
            Rect {
                x: snap(painter, geometry.left) - 1.0,
                y: rect.y,
                width: 1.0,
                height: rect.height,
            },
            SURFACE,
            0.0,
        );
        painter.pop_clip();
    }

    fn reports_layout(&self) -> bool {
        true
    }

    fn on_layout(&self, rect: Rect) -> bool {
        self.state.timeline_rect.set(rect);
        false
    }

    fn on_pointer_move(&self) -> Option<Rc<dyn Fn(Point, Modifiers)>> {
        let state = self.state.clone();
        Some(Rc::new(move |_, modifiers| state.modifiers.set(modifiers)))
    }

    fn on_drag_start(&self) -> Option<Rc<dyn Fn(Point, Rect)>> {
        let state = self.state.clone();
        let kinds: Vec<_> = self.lanes.iter().map(|lane| lane.kind).collect();
        let lanes = self.hit_lanes();
        let (start, span) = (self.start, self.span);
        Some(Rc::new(move |position, rect| {
            let geometry = Geometry::new(local(rect), &kinds, start, span);
            let window = Point {
                x: rect.x + position.x,
                y: rect.y + position.y,
            };
            let gesture = if position.x < geometry.left {
                Gesture::Idle
            } else if position.y < RULER_HEIGHT {
                Gesture::Scrub
            } else {
                match hit(&geometry, &lanes, position) {
                    Some(Some(clip)) => {
                        let modifiers = state.modifiers.get();
                        if modifiers.ctrl {
                            state.toggle_clip(clip);
                        } else if modifiers.shift {
                            state.extend_selection(clip);
                        } else if state.selected.peek().contains(&clip) {
                            let mut selection = state.selected.peek();
                            selection.retain(|item| *item != clip);
                            selection.push(clip);
                            state.select_clips(selection);
                        } else {
                            state.select_clip(Some(clip));
                        }
                        if state.selected.peek().contains(&clip) {
                            Gesture::Clip {
                                anchor: clip,
                                grab: geometry.frame_at(position.x),
                                origin: window,
                                click: (!modifiers.ctrl && !modifiers.shift)
                                    .then(|| geometry.frame(position.x)),
                            }
                        } else {
                            Gesture::Idle
                        }
                    }
                    Some(None) => {
                        let modifiers = state.modifiers.get();
                        Gesture::Marquee {
                            origin: position,
                            base: if modifiers.ctrl || modifiers.shift {
                                state.selected.peek()
                            } else {
                                Vec::new()
                            },
                            frame: geometry.frame(position.x),
                        }
                    }
                    None => Gesture::Idle,
                }
            };
            *state.gesture.borrow_mut() = gesture;
        }))
    }

    fn on_drag(&self) -> Option<Rc<dyn Fn(Point, Rect)>> {
        let state = self.state.clone();
        let kinds: Vec<_> = self.lanes.iter().map(|lane| lane.kind).collect();
        let lanes = self.hit_lanes();
        let (start, span, end) = (self.start, self.span, self.end);
        Some(Rc::new(move |position, rect| {
            let geometry = Geometry::new(local(rect), &kinds, start, span);
            let gesture = state.gesture.borrow().clone();
            match gesture {
                Gesture::Scrub if end > 0 => {
                    let frame = geometry.frame(position.x).min(end - 1);
                    if state.program.position.peek() != frame {
                        if state.program.playing.peek() {
                            state.pause();
                        }
                        state.focus_frame(frame);
                    }
                }
                Gesture::Clip { .. } => {
                    state.drag_pointer(Point {
                        x: rect.x + position.x,
                        y: rect.y + position.y,
                    });
                    if state.drag.peek().is_some() {
                        if position.x > rect.width - 24.0 {
                            state.scroll_timeline(1);
                        } else if position.x < geometry.left + 24.0 {
                            state.scroll_timeline(-1);
                        }
                    }
                }
                Gesture::Marquee { origin, base, .. } => {
                    if state.marquee.peek().is_none()
                        && (position.x - origin.x).hypot(position.y - origin.y) < 4.0
                    {
                        return;
                    }
                    let x = origin.x.min(position.x).max(geometry.left);
                    let y = origin.y.min(position.y).max(RULER_HEIGHT);
                    let area = Rect {
                        x,
                        y,
                        width: (origin.x.max(position.x).min(rect.width) - x).max(0.0),
                        height: (origin.y.max(position.y).min(rect.height) - y).max(0.0),
                    };
                    state.marquee.set(Some(area));
                    let mut selection = base;
                    selection.extend(clips_in(&geometry, &lanes, area));
                    state.select_clips(selection);
                }
                _ => {}
            }
        }))
    }

    fn on_drag_end(&self) -> Option<Rc<dyn Fn()>> {
        let state = self.state.clone();
        Some(Rc::new(move || {
            let gesture = std::mem::take(&mut *state.gesture.borrow_mut());
            match gesture {
                Gesture::Clip { click, .. } => {
                    if state.drag.peek().is_some() {
                        state.finish_drag();
                    } else if let Some(frame) = click {
                        state.seek(frame);
                    }
                }
                Gesture::Marquee { base, frame, .. } => {
                    if state.marquee.peek().is_some() {
                        state.marquee.set(None);
                    } else if base.is_empty() {
                        state.select_clip(None);
                        state.seek(frame);
                    }
                }
                _ => {}
            }
        }))
    }

    fn on_wheel(&self) -> Option<Rc<dyn Fn(WheelInput)>> {
        let state = self.state.clone();
        Some(Rc::new(move |input| {
            let amount = if input.delta.x.abs() > input.delta.y.abs() {
                input.delta.x
            } else {
                input.delta.y
            };
            if amount == 0.0 {
                return;
            }
            if input.modifiers.ctrl {
                let zoom = state.timeline_zoom.peek();
                state.zoom_timeline(if amount > 0.0 {
                    zoom + 1
                } else {
                    zoom.saturating_sub(1)
                });
            } else {
                let steps = (amount / 40.0).round() as i64;
                state.scroll_timeline(if steps == 0 {
                    amount.signum() as i64
                } else {
                    steps
                });
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ruler_ticks_stay_readable_at_every_zoom() {
        assert_eq!(tick_spacing(300.0), (1.0, 5));
        assert_eq!(tick_spacing(30.0), (5.0, 5));
        assert_eq!(tick_spacing(6.0), (15.0, 5));
        assert_eq!(tick_spacing(0.01), (3600.0, 4));
        assert_eq!(ruler_label(75.0), "01:15");
        assert_eq!(ruler_label(3_725.0), "1:02:05");
    }
}
