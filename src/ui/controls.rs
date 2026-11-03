use super::UiState;
use crate::export::ExportControl;
use crate::media::MediaKind;
use creamui::core::layout::{Dimension, Style};
use creamui::core::{
    CursorIcon, FileDropTarget, Painter, Point, Rect, RgbaImage, TextAlign, Widget,
};
use creamui::widgets::layout::{Align, Justify, StyleExt};
use creamui::widgets::{Icon, RawButton, RawText, Symbol};
use creamui::{BoxedWidget, Color, StateStyle, Styled};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

pub(crate) const BACKGROUND: Color = Color::rgb(11, 12, 15);
pub(super) const PANEL: Color = Color::rgb(20, 22, 27);
pub(super) const PANEL_RAISED: Color = Color::rgb(26, 29, 35);
pub(super) const SURFACE: Color = Color::rgb(34, 38, 45);
pub(super) const SURFACE_HOVER: Color = Color::rgb(46, 51, 60);
pub(super) const BORDER: Color = Color::rgb(38, 42, 50);
pub(super) const STAGE: Color = Color::rgb(3, 4, 6);
pub(super) const TEXT: Color = Color::rgb(232, 235, 239);
pub(super) const MUTED: Color = Color::rgb(139, 146, 157);
pub(super) const FAINT: Color = Color::rgb(84, 91, 102);
pub(super) const ACCENT: Color = Color::rgb(92, 208, 190);
pub(super) const ACCENT_TEXT: Color = Color::rgb(5, 28, 25);
pub(super) const PLAYHEAD: Color = Color::rgb(255, 94, 94);
pub(super) const VIDEO: Color = Color::rgb(84, 146, 232);
pub(super) const AUDIO: Color = Color::rgb(70, 194, 132);
pub(super) const TITLE: Color = Color::rgb(172, 130, 242);
pub(super) const IMAGE: Color = Color::rgb(234, 172, 88);
const CLEAR: Color = Color::rgba(0, 0, 0, 0);

pub(super) fn kind_color(kind: MediaKind) -> Color {
    match kind {
        MediaKind::Video => VIDEO,
        MediaKind::Audio => AUDIO,
        MediaKind::Image => IMAGE,
    }
}

pub(super) fn kind_glyph(kind: MediaKind) -> Glyph {
    match kind {
        MediaKind::Video => Glyph::Film,
        MediaKind::Audio => Glyph::Music,
        MediaKind::Image => Glyph::Picture,
    }
}

pub(super) fn text(value: impl Into<String>, color: Color, size: f32) -> BoxedWidget {
    Box::new(RawText::new(value, color, size))
}

pub(super) fn heading(value: impl Into<String>, color: Color, size: f32) -> BoxedWidget {
    Box::new(
        RawText::unstyled(value).with_style(
            creamui::Style::new()
                .color(color)
                .font_size(size)
                .bold(true),
        ),
    )
}

/// Shortens `value` with an ellipsis so it fits roughly `width` pixels.
pub(super) fn fit(value: &str, width: f32, size: f32) -> String {
    let limit = (width / (size * 0.6)).floor().max(0.0) as usize;
    if value.chars().count() <= limit {
        value.to_owned()
    } else if limit <= 1 {
        String::new()
    } else {
        let mut short: String = value.chars().take(limit - 1).collect();
        short.push('…');
        short
    }
}

pub(super) fn timecode(frame: u64, fps: (u32, u32)) -> String {
    let milliseconds = u128::from(frame) * u128::from(fps.1) * 1000 / u128::from(fps.0.max(1));
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        milliseconds / 3_600_000,
        (milliseconds / 60_000) % 60,
        (milliseconds / 1_000) % 60,
        milliseconds % 1_000
    )
}

pub(super) fn short_duration(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as u64;
    if total >= 3600 {
        format!("{}:{:02}:{:02}", total / 3600, total / 60 % 60, total % 60)
    } else {
        format!("{}:{:02}", total / 60, total % 60)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Glyph {
    Play,
    Pause,
    StepBack,
    StepForward,
    ToStart,
    ToEnd,
    Split,
    Trash,
    Ripple,
    Title,
    Undo,
    Redo,
    Plus,
    Minus,
    ZoomIn,
    ZoomOut,
    List,
    Grid,
    Import,
    Export,
    Film,
    Music,
    Picture,
    Volume,
    Mute,
    ChevronUp,
    ChevronDown,
    ChevronLeft,
    ChevronRight,
    Search,
    Folder,
    Save,
    Document,
    Locate,
}

struct Pen<'a> {
    painter: &'a mut dyn Painter,
    rect: Rect,
    unit: f32,
    color: Color,
}

impl Pen<'_> {
    fn point(&self, (x, y): (f32, f32)) -> Point {
        Point {
            x: self.rect.x + x * self.unit,
            y: self.rect.y + y * self.unit,
        }
    }

    fn line(&mut self, points: &[(f32, f32)]) {
        for pair in points.windows(2) {
            let (from, to) = (self.point(pair[0]), self.point(pair[1]));
            self.painter
                .stroke_line(from, to, self.color, 1.8 * self.unit);
        }
    }

    fn arc(&mut self, center: (f32, f32), radius: f32, from: f32, to: f32) {
        let points: Vec<_> = (0..=16)
            .map(|step| {
                let angle = (from + (to - from) * step as f32 / 16.0).to_radians();
                (
                    center.0 + radius * angle.cos(),
                    center.1 + radius * angle.sin(),
                )
            })
            .collect();
        self.line(&points);
    }

    fn bar(&mut self, x: f32, y: f32, width: f32, height: f32) {
        let origin = self.point((x, y));
        self.painter.fill_rect(
            Rect {
                x: origin.x,
                y: origin.y,
                width: width * self.unit,
                height: height * self.unit,
            },
            self.color,
            0.7 * self.unit,
        );
    }

    fn outline(&mut self, x: f32, y: f32, width: f32, height: f32) {
        self.line(&[
            (x, y),
            (x + width, y),
            (x + width, y + height),
            (x, y + height),
            (x, y),
        ]);
    }

    /// Fills a convex polygon with physical-pixel scanlines.
    fn fill(&mut self, points: &[(f32, f32)]) {
        let points: Vec<_> = points.iter().map(|point| self.point(*point)).collect();
        let top = points.iter().map(|p| p.y).fold(f32::MAX, f32::min);
        let bottom = points.iter().map(|p| p.y).fold(f32::MIN, f32::max);
        let step = 1.0 / self.painter.scale_factor().max(1.0);
        let mut y = top;
        while y < bottom {
            let middle = y + step / 2.0;
            let (mut left, mut right) = (f32::MAX, f32::MIN);
            for (index, a) in points.iter().enumerate() {
                let b = points[(index + 1) % points.len()];
                if (a.y <= middle) != (b.y <= middle) {
                    let x = a.x + (middle - a.y) / (b.y - a.y) * (b.x - a.x);
                    left = left.min(x);
                    right = right.max(x);
                }
            }
            if right > left {
                self.painter.fill_rect(
                    Rect {
                        x: left,
                        y,
                        width: right - left,
                        height: step,
                    },
                    self.color,
                    0.0,
                );
            }
            y += step;
        }
    }

    fn disc(&mut self, center: (f32, f32), radius: f32) {
        let points: Vec<_> = (0..14)
            .map(|step| {
                let angle = step as f32 / 14.0 * std::f32::consts::TAU;
                (
                    center.0 + radius * angle.cos(),
                    center.1 + radius * angle.sin(),
                )
            })
            .collect();
        self.fill(&points);
    }
}

pub(super) fn draw_glyph(painter: &mut dyn Painter, glyph: Glyph, rect: Rect, color: Color) {
    let symbol = match glyph {
        Glyph::Picture => Some(Symbol::Image),
        Glyph::Search => Some(Symbol::Search),
        Glyph::Folder => Some(Symbol::Folder),
        _ => None,
    };
    if let Some(symbol) = symbol {
        Icon::draw(symbol, painter, rect, color);
        return;
    }
    let side = rect.width.min(rect.height);
    let mut pen = Pen {
        unit: side / 24.0,
        rect: Rect {
            x: rect.x + (rect.width - side) / 2.0,
            y: rect.y + (rect.height - side) / 2.0,
            width: side,
            height: side,
        },
        painter,
        color,
    };
    match glyph {
        Glyph::Play => pen.fill(&[(7.0, 4.5), (19.5, 12.0), (7.0, 19.5)]),
        Glyph::Pause => {
            pen.bar(6.0, 5.0, 4.0, 14.0);
            pen.bar(14.0, 5.0, 4.0, 14.0);
        }
        Glyph::StepBack => {
            pen.bar(5.0, 6.0, 2.4, 12.0);
            pen.fill(&[(19.0, 6.0), (19.0, 18.0), (9.0, 12.0)]);
        }
        Glyph::StepForward => {
            pen.bar(16.6, 6.0, 2.4, 12.0);
            pen.fill(&[(5.0, 6.0), (5.0, 18.0), (15.0, 12.0)]);
        }
        Glyph::ToStart => {
            pen.bar(4.0, 6.0, 2.4, 12.0);
            pen.fill(&[(13.0, 6.0), (13.0, 18.0), (6.8, 12.0)]);
            pen.fill(&[(20.0, 6.0), (20.0, 18.0), (13.8, 12.0)]);
        }
        Glyph::ToEnd => {
            pen.bar(17.6, 6.0, 2.4, 12.0);
            pen.fill(&[(4.0, 6.0), (4.0, 18.0), (10.2, 12.0)]);
            pen.fill(&[(11.0, 6.0), (11.0, 18.0), (17.2, 12.0)]);
        }
        Glyph::Split => {
            pen.arc((7.0, 17.0), 2.8, 0.0, 360.0);
            pen.arc((17.0, 17.0), 2.8, 0.0, 360.0);
            pen.line(&[(8.8, 14.9), (17.0, 3.5)]);
            pen.line(&[(15.2, 14.9), (7.0, 3.5)]);
        }
        Glyph::Trash => {
            pen.line(&[(4.0, 6.5), (20.0, 6.5)]);
            pen.line(&[(9.5, 6.5), (9.5, 4.0), (14.5, 4.0), (14.5, 6.5)]);
            pen.line(&[(6.5, 6.5), (7.5, 20.0), (16.5, 20.0), (17.5, 6.5)]);
            pen.line(&[(10.5, 10.0), (10.5, 16.5)]);
            pen.line(&[(13.5, 10.0), (13.5, 16.5)]);
        }
        Glyph::Ripple => {
            pen.outline(2.5, 7.0, 6.0, 10.0);
            pen.bar(15.5, 7.0, 6.0, 10.0);
            pen.line(&[(14.0, 12.0), (10.0, 12.0)]);
            pen.line(&[(12.0, 9.8), (10.0, 12.0), (12.0, 14.2)]);
        }
        Glyph::Title => {
            pen.line(&[(5.0, 8.0), (5.0, 5.0), (19.0, 5.0), (19.0, 8.0)]);
            pen.line(&[(12.0, 5.0), (12.0, 19.5)]);
            pen.line(&[(9.0, 19.5), (15.0, 19.5)]);
        }
        Glyph::Undo | Glyph::Redo => {
            let flip = |(x, y): (f32, f32)| {
                if glyph == Glyph::Redo {
                    (24.0 - x, y)
                } else {
                    (x, y)
                }
            };
            let head: Vec<_> = [(9.0, 4.5), (5.0, 8.5), (9.0, 12.5)]
                .into_iter()
                .map(flip)
                .collect();
            pen.line(&head);
            let mut path = vec![flip((5.0, 8.5)), flip((14.0, 8.5))];
            for step in 0..=12 {
                let angle = (-90.0 + 180.0 * step as f32 / 12.0_f32).to_radians();
                path.push(flip((14.0 + 5.5 * angle.cos(), 14.0 + 5.5 * angle.sin())));
            }
            path.push(flip((8.0, 19.5)));
            pen.line(&path);
        }
        Glyph::Plus => {
            pen.line(&[(12.0, 5.0), (12.0, 19.0)]);
            pen.line(&[(5.0, 12.0), (19.0, 12.0)]);
        }
        Glyph::Minus => pen.line(&[(5.0, 12.0), (19.0, 12.0)]),
        Glyph::ZoomIn | Glyph::ZoomOut => {
            pen.arc((10.5, 10.5), 6.0, 0.0, 360.0);
            pen.line(&[(15.0, 15.0), (20.5, 20.5)]);
            pen.line(&[(7.8, 10.5), (13.2, 10.5)]);
            if glyph == Glyph::ZoomIn {
                pen.line(&[(10.5, 7.8), (10.5, 13.2)]);
            }
        }
        Glyph::List => {
            for y in [5.0, 10.5, 16.0] {
                pen.bar(3.5, y, 3.0, 3.0);
                pen.line(&[(10.0, y + 1.5), (20.5, y + 1.5)]);
            }
        }
        Glyph::Grid => {
            for (x, y) in [(4.0, 4.0), (13.5, 4.0), (4.0, 13.5), (13.5, 13.5)] {
                pen.outline(x, y, 6.5, 6.5);
            }
        }
        Glyph::Import | Glyph::Export => {
            pen.line(&[(12.0, 3.5), (12.0, 14.5)]);
            if glyph == Glyph::Import {
                pen.line(&[(7.5, 10.0), (12.0, 14.5), (16.5, 10.0)]);
            } else {
                pen.line(&[(7.5, 8.0), (12.0, 3.5), (16.5, 8.0)]);
            }
            pen.line(&[(4.0, 15.0), (4.0, 20.0), (20.0, 20.0), (20.0, 15.0)]);
        }
        Glyph::Film => {
            pen.outline(3.0, 5.0, 18.0, 14.0);
            pen.line(&[(7.0, 5.0), (7.0, 19.0)]);
            pen.line(&[(17.0, 5.0), (17.0, 19.0)]);
            for y in [7.5, 11.2, 14.9] {
                pen.bar(4.3, y, 1.5, 1.7);
                pen.bar(18.2, y, 1.5, 1.7);
            }
        }
        Glyph::Music => {
            pen.line(&[(9.0, 17.0), (9.0, 5.5), (19.0, 3.5), (19.0, 15.0)]);
            pen.disc((6.6, 17.2), 2.6);
            pen.disc((16.6, 15.2), 2.6);
        }
        Glyph::Volume | Glyph::Mute => {
            pen.bar(3.0, 9.0, 4.8, 6.0);
            pen.fill(&[(7.5, 9.0), (12.5, 4.5), (12.5, 19.5), (7.5, 15.0)]);
            if glyph == Glyph::Volume {
                pen.arc((12.5, 12.0), 4.0, -50.0, 50.0);
                pen.arc((12.5, 12.0), 7.5, -50.0, 50.0);
            } else {
                pen.line(&[(15.5, 9.0), (21.0, 15.0)]);
                pen.line(&[(21.0, 9.0), (15.5, 15.0)]);
            }
        }
        Glyph::ChevronUp => pen.line(&[(6.0, 15.0), (12.0, 9.0), (18.0, 15.0)]),
        Glyph::ChevronDown => pen.line(&[(6.0, 9.0), (12.0, 15.0), (18.0, 9.0)]),
        Glyph::ChevronLeft => pen.line(&[(15.0, 6.0), (9.0, 12.0), (15.0, 18.0)]),
        Glyph::ChevronRight => pen.line(&[(9.0, 6.0), (15.0, 12.0), (9.0, 18.0)]),
        Glyph::Save => {
            pen.line(&[
                (4.0, 4.0),
                (17.0, 4.0),
                (20.0, 7.0),
                (20.0, 20.0),
                (4.0, 20.0),
                (4.0, 4.0),
            ]);
            pen.line(&[(8.0, 4.0), (8.0, 8.5), (15.0, 8.5), (15.0, 4.0)]);
            pen.outline(7.5, 13.0, 9.0, 7.0);
        }
        Glyph::Document => {
            pen.line(&[
                (6.0, 3.0),
                (14.0, 3.0),
                (19.0, 8.0),
                (19.0, 21.0),
                (6.0, 21.0),
                (6.0, 3.0),
            ]);
            pen.line(&[(14.0, 3.0), (14.0, 8.0), (19.0, 8.0)]);
            pen.line(&[(12.5, 11.0), (12.5, 17.0)]);
            pen.line(&[(9.5, 14.0), (15.5, 14.0)]);
        }
        Glyph::Locate => {
            pen.arc((12.0, 12.0), 6.5, 0.0, 360.0);
            pen.disc((12.0, 12.0), 2.2);
            pen.line(&[(12.0, 2.5), (12.0, 5.5)]);
            pen.line(&[(12.0, 18.5), (12.0, 21.5)]);
            pen.line(&[(2.5, 12.0), (5.5, 12.0)]);
            pen.line(&[(18.5, 12.0), (21.5, 12.0)]);
        }
        Glyph::Picture | Glyph::Search | Glyph::Folder => {}
    }
}

pub(super) struct GlyphView {
    pub glyph: Glyph,
    pub color: Color,
    pub size: f32,
}

impl Widget for GlyphView {
    fn style(&self) -> creamui::Style {
        let mut style = Style::default().size(self.size, self.size);
        style.flex_shrink = 0.0;
        style.into()
    }

    fn paint(&self, painter: &mut dyn Painter, rect: Rect) {
        draw_glyph(painter, self.glyph, rect, self.color);
    }
}

pub(super) fn glyph(glyph: Glyph, color: Color, size: f32) -> BoxedWidget {
    Box::new(GlyphView { glyph, color, size })
}

struct Look {
    background: Color,
    hover: Color,
    height: f32,
    width: Option<f32>,
    padding: f32,
    radius: f32,
}

fn pressable(
    look: Look,
    enabled: bool,
    children: Vec<BoxedWidget>,
    state: &UiState,
    operation: impl Fn(&UiState) + 'static,
) -> BoxedWidget {
    let state = state.clone();
    let mut style = Style::default()
        .flex_row()
        .align(Align::Center)
        .justify(Justify::Center)
        .gap(6.0)
        .height(look.height)
        .padding_xy(look.padding, 0.0);
    if let Some(width) = look.width {
        style = style.width(width);
    }
    style.flex_shrink = 0.0;
    let hover = if enabled { look.hover } else { look.background };
    Box::new(
        RawButton::new(style, move || operation(&state))
            .with_children(children)
            .disabled(!enabled)
            .background(look.background)
            .corner_radius(look.radius)
            .hover_style(StateStyle::new().background(hover))
            .pressed_style(StateStyle::new().background(hover.mix(BACKGROUND, 0.35)))
            .focus_style(StateStyle::new().outline(ACCENT, 2.0))
            .disabled_style(StateStyle::new().background(look.background)),
    )
}

/// A square, borderless button showing only a glyph.
pub(super) fn icon_button(
    icon: Glyph,
    enabled: bool,
    state: &UiState,
    operation: impl Fn(&UiState) + 'static,
) -> BoxedWidget {
    sized_icon_button(icon, 28.0, enabled, false, state, operation)
}

pub(super) fn sized_icon_button(
    icon: Glyph,
    size: f32,
    enabled: bool,
    active: bool,
    state: &UiState,
    operation: impl Fn(&UiState) + 'static,
) -> BoxedWidget {
    let color = if !enabled {
        FAINT
    } else if active {
        ACCENT
    } else {
        TEXT.mix(MUTED, 0.35)
    };
    pressable(
        Look {
            background: if active { SURFACE } else { CLEAR },
            hover: SURFACE_HOVER,
            height: size,
            width: Some(size),
            padding: 0.0,
            radius: 6.0,
        },
        enabled,
        vec![glyph(icon, color, (size * 0.6).round())],
        state,
        operation,
    )
}

/// The round accent play/pause control of a monitor transport.
pub(super) fn play_button(
    playing: bool,
    enabled: bool,
    state: &UiState,
    operation: impl Fn(&UiState) + 'static,
) -> BoxedWidget {
    pressable(
        Look {
            background: if enabled { ACCENT } else { SURFACE },
            hover: ACCENT.mix(TEXT, 0.25),
            height: 34.0,
            width: Some(34.0),
            padding: 0.0,
            radius: 17.0,
        },
        enabled,
        vec![glyph(
            if playing { Glyph::Pause } else { Glyph::Play },
            if enabled { ACCENT_TEXT } else { FAINT },
            16.0,
        )],
        state,
        operation,
    )
}

/// A compact, borderless glyph-and-label control for toolbars.
pub(super) fn tool_button(
    icon: Option<Glyph>,
    label: &str,
    enabled: bool,
    state: &UiState,
    operation: impl Fn(&UiState) + 'static,
) -> BoxedWidget {
    let mut children = Vec::new();
    if let Some(icon) = icon {
        children.push(glyph(icon, if enabled { MUTED } else { FAINT }, 15.0));
    }
    children.push(text(label, if enabled { TEXT } else { FAINT }, 12.0));
    pressable(
        Look {
            background: CLEAR,
            hover: SURFACE_HOVER,
            height: 28.0,
            width: None,
            padding: 9.0,
            radius: 6.0,
        },
        enabled,
        children,
        state,
        operation,
    )
}

/// A filled call-to-action button.
pub(super) fn accent_button(
    icon: Option<Glyph>,
    label: &str,
    enabled: bool,
    state: &UiState,
    operation: impl Fn(&UiState) + 'static,
) -> BoxedWidget {
    let color = if enabled { ACCENT_TEXT } else { FAINT };
    let mut children = Vec::new();
    if let Some(icon) = icon {
        children.push(glyph(icon, color, 15.0));
    }
    children.push(heading(label, color, 12.0));
    pressable(
        Look {
            background: if enabled { ACCENT } else { SURFACE },
            hover: ACCENT.mix(TEXT, 0.2),
            height: 30.0,
            width: None,
            padding: 12.0,
            radius: 7.0,
        },
        enabled,
        children,
        state,
        operation,
    )
}

/// A neutral raised button for secondary actions.
pub(super) fn secondary_button(
    icon: Option<Glyph>,
    label: &str,
    enabled: bool,
    state: &UiState,
    operation: impl Fn(&UiState) + 'static,
) -> BoxedWidget {
    let color = if enabled { TEXT } else { FAINT };
    let mut children = Vec::new();
    if let Some(icon) = icon {
        children.push(glyph(icon, if enabled { MUTED } else { FAINT }, 14.0));
    }
    children.push(text(label, color, 12.0));
    pressable(
        Look {
            background: SURFACE,
            hover: SURFACE_HOVER,
            height: 28.0,
            width: None,
            padding: 10.0,
            radius: 6.0,
        },
        enabled,
        children,
        state,
        operation,
    )
}

/// One option of a segmented filter.
pub(super) fn chip(
    label: &str,
    active: bool,
    state: &UiState,
    operation: impl Fn(&UiState) + 'static,
) -> BoxedWidget {
    pressable(
        Look {
            background: if active { SURFACE_HOVER } else { CLEAR },
            hover: if active { SURFACE_HOVER } else { SURFACE },
            height: 24.0,
            width: None,
            padding: 9.0,
            radius: 5.0,
        },
        true,
        vec![text(label, if active { TEXT } else { MUTED }, 11.0)],
        state,
        operation,
    )
}

/// A clickable container, used for selectable media bin items.
pub(super) fn selectable(
    style: Style,
    selected: bool,
    children: Vec<BoxedWidget>,
    state: &UiState,
    operation: impl Fn(&UiState) + 'static,
) -> BoxedWidget {
    let state = state.clone();
    let background = if selected { SURFACE } else { PANEL_RAISED };
    Box::new(
        RawButton::new(style, move || operation(&state))
            .with_children(children)
            .background(background)
            .border(if selected { ACCENT } else { BORDER }, 1.0)
            .corner_radius(8.0)
            .hover_style(StateStyle::new().background(SURFACE))
            .focus_style(StateStyle::new().outline(ACCENT, 2.0)),
    )
}

/// A media thumbnail letterboxed on a dark stage with an optional badge.
pub(super) struct Thumb {
    pub image: Option<Arc<RgbaImage>>,
    pub kind: MediaKind,
    pub width: Option<f32>,
    pub height: f32,
    pub badge: Option<String>,
}

impl Widget for Thumb {
    fn style(&self) -> creamui::Style {
        let mut style = Style::default();
        style.size.width = self
            .width
            .map_or(Dimension::Percent(1.0), Dimension::Length);
        style.size.height = Dimension::Length(self.height);
        style.flex_shrink = 0.0;
        style.into()
    }

    fn paint(&self, painter: &mut dyn Painter, rect: Rect) {
        painter.push_clip_rounded(rect, 5.0);
        painter.fill_rect(rect, STAGE, 5.0);
        let color = kind_color(self.kind);
        if let Some(image) = &self.image {
            let scale =
                (rect.width / image.width() as f32).min(rect.height / image.height() as f32);
            let (width, height) = (image.width() as f32 * scale, image.height() as f32 * scale);
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
            painter.fill_linear_gradient(
                rect,
                color.mix(STAGE, 0.78),
                color.mix(STAGE, 0.92),
                135.0,
                5.0,
            );
            let side = (rect.height * 0.42).min(28.0);
            draw_glyph(
                painter,
                kind_glyph(self.kind),
                Rect {
                    x: rect.x + (rect.width - side) / 2.0,
                    y: rect.y + (rect.height - side) / 2.0,
                    width: side,
                    height: side,
                },
                color.mix(STAGE, 0.25),
            );
        }
        painter.fill_rect(
            Rect {
                y: rect.y + rect.height - 2.0,
                height: 2.0,
                ..rect
            },
            color,
            0.0,
        );
        if let Some(badge) = self.badge.as_deref().filter(|_| rect.width >= 60.0) {
            let width = badge.chars().count() as f32 * 6.0 + 10.0;
            let pill = Rect {
                x: rect.x + rect.width - width - 4.0,
                y: rect.y + rect.height - 20.0,
                width,
                height: 15.0,
            };
            painter.fill_rect(pill, Color::rgba(0, 0, 0, 190), 4.0);
            painter.fill_text(pill, badge, TEXT, 10.0, TextAlign::Center);
        }
        painter.pop_clip();
    }
}

/// A horizontal position bar for a monitor; dragging calls `seek`.
pub(super) struct Scrubber {
    pub position: u64,
    pub end: u64,
    pub color: Color,
    pub seek: Rc<dyn Fn(u64)>,
}

impl Widget for Scrubber {
    fn style(&self) -> creamui::Style {
        let mut style = Style::default().height(18.0);
        style.size.width = Dimension::Percent(1.0);
        style.into()
    }

    fn paint(&self, painter: &mut dyn Painter, rect: Rect) {
        let hovered = painter.hovered(rect);
        let thickness = if hovered { 5.0 } else { 4.0 };
        let bar = Rect {
            x: rect.x,
            y: rect.y + (rect.height - thickness) / 2.0,
            width: rect.width,
            height: thickness,
        };
        painter.fill_rect(bar, SURFACE, thickness / 2.0);
        if self.end == 0 {
            return;
        }
        let progress = (self.position as f32 / self.end.saturating_sub(1).max(1) as f32).min(1.0);
        painter.fill_rect(
            Rect {
                width: bar.width * progress,
                ..bar
            },
            self.color,
            thickness / 2.0,
        );
        let x = rect.x + rect.width * progress;
        let knob = if hovered { 14.0 } else { 12.0 };
        painter.fill_rect(
            Rect {
                x: x - knob / 2.0,
                y: rect.y + (rect.height - knob) / 2.0,
                width: knob,
                height: knob,
            },
            TEXT,
            knob / 2.0,
        );
    }

    fn on_drag(&self) -> Option<Rc<dyn Fn(Point, Rect)>> {
        let (end, seek) = (self.end, self.seek.clone());
        Some(Rc::new(move |position, rect| {
            if end == 0 {
                return;
            }
            let progress = (position.x / rect.width.max(1.0)).clamp(0.0, 1.0);
            seek(((progress * (end - 1) as f32).round() as u64).min(end - 1));
        }))
    }

    fn cursor_icon(&self) -> Option<CursorIcon> {
        Some(CursorIcon::Pointer)
    }
}

pub(super) struct ExportMeter {
    pub control: Arc<ExportControl>,
    pub total: u64,
}

impl Widget for ExportMeter {
    fn style(&self) -> creamui::Style {
        Style::default().size(140.0, 5.0).into()
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

/// A container that imports files dropped onto it from the desktop.
pub(super) struct DropZone {
    pub style: creamui::Style,
    pub children: Vec<BoxedWidget>,
    pub on_drop: Rc<dyn Fn(Vec<PathBuf>)>,
}

impl Widget for DropZone {
    fn style(&self) -> creamui::Style {
        self.style.clone()
    }

    fn paint(&self, _painter: &mut dyn Painter, _rect: Rect) {}

    fn children(&mut self) -> Vec<BoxedWidget> {
        std::mem::take(&mut self.children)
    }

    fn clips_children(&self) -> bool {
        true
    }

    fn clip_corner_radius(&self) -> f32 {
        self.style.paint.corner_radius.unwrap_or(0.0)
    }

    fn file_drop_target(&self) -> Option<FileDropTarget> {
        let on_drop = self.on_drop.clone();
        Some(FileDropTarget {
            accepts: Rc::new(|_| true),
            on_drop: Rc::new(move |paths| on_drop(paths)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_shorten_to_fit_and_durations_read_naturally() {
        assert_eq!(
            fit("interview_take2.mov", 200.0, 11.0),
            "interview_take2.mov"
        );
        assert_eq!(fit("interview_take2.mov", 60.0, 11.0), "intervie…");
        assert_eq!(fit("clip", 4.0, 11.0), "");
        assert_eq!(short_duration(64.4), "1:04");
        assert_eq!(short_duration(3_725.0), "1:02:05");
        assert_eq!(timecode(30, (30_000, 1_001)), "00:00:01.001");
    }
}
