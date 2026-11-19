use super::preview::{FrameImage, FrameSource, VideoLayers, blend};
use crate::project::Project;
use crate::timeline::{
    ClipContent, ColorAdjust, SlideDirection, Title, TrackKind, Transition, TransitionKind,
};
use crate::ui::{title_fonts, title_markup};
use creamui::fonts::{FontWeight, HorizontalAlign, LayoutSettings, cached_layout, resolve};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use swash::scale::{Render, ScaleContext, Source, image::Image};
use swash::zeno::{Format, Join, Stroke};

fn adjust_color(image: Arc<FrameImage>, color: ColorAdjust) -> Arc<FrameImage> {
    if color.neutral() {
        return image;
    }
    let table: [f32; 256] = std::array::from_fn(|value| {
        (((value as f32 / 255.0 - 0.5) * color.contrast + 0.5 + color.brightness) * 255.0)
            .clamp(0.0, 255.0)
    });
    let mut adjusted = (*image).clone();
    for pixel in adjusted.pixels.as_chunks_mut::<4>().0 {
        let rgb = [
            table[pixel[0] as usize],
            table[pixel[1] as usize],
            table[pixel[2] as usize],
        ];
        let luma = rgb[0] * 0.2126 + rgb[1] * 0.7152 + rgb[2] * 0.0722;
        for channel in 0..3 {
            pixel[channel] = (luma + (rgb[channel] - luma) * color.saturation)
                .round()
                .clamp(0.0, 255.0) as u8;
        }
    }
    Arc::new(adjusted)
}

#[derive(Clone)]
pub struct VisualPlan {
    transitions: HashMap<u64, Transition>,
    transitions_out: HashMap<u64, Transition>,
    tracks: Vec<Vec<VisualClip>>,
    resolution: (u32, u32),
}

#[derive(Clone)]
enum VisualClip {
    Media(FrameSource),
    Title {
        clip_id: u64,
        start: u64,
        duration: u64,
        title: Title,
    },
}

impl VisualClip {
    fn timing(&self) -> (u64, u64, u64) {
        match self {
            Self::Media(source) => (source.clip_id, source.start, source.duration),
            Self::Title {
                clip_id,
                start,
                duration,
                ..
            } => (*clip_id, *start, *duration),
        }
    }
}

impl VisualPlan {
    pub fn from_project(project: &Project) -> Self {
        Self::without_media(project, &HashSet::new())
    }

    /// The project's visuals with clips of the `missing` media left out.
    pub fn without_media(project: &Project, missing: &HashSet<u64>) -> Self {
        let tracks = project
            .tracks()
            .iter()
            .filter(|track| track.kind == TrackKind::Video && !track.hidden)
            .map(|track| {
                track
                    .clips
                    .iter()
                    .filter_map(|clip| match &clip.content {
                        ClipContent::Media { media_id } if !missing.contains(media_id) => {
                            let media =
                                project.media().iter().find(|media| media.id == *media_id)?;
                            Some(VisualClip::Media(FrameSource {
                                speed: clip.speed,
                                color: clip.color,
                                clip_id: clip.id,
                                path: media.path.clone(),
                                kind: media.kind,
                                start: clip.start_frame,
                                source_start: clip.source_start_frame,
                                duration: clip.duration_frames,
                            }))
                        }
                        ClipContent::Media { .. } => None,
                        ClipContent::Title(title) => Some(VisualClip::Title {
                            clip_id: clip.id,
                            start: clip.start_frame,
                            duration: clip.duration_frames,
                            title: title.clone(),
                        }),
                    })
                    .collect()
            })
            .collect();
        Self {
            transitions_out: project
                .tracks()
                .iter()
                .flat_map(|track| &track.clips)
                .filter_map(|clip| {
                    clip.transition_out
                        .clone()
                        .map(|transition| (clip.id, transition))
                })
                .collect(),
            transitions: project
                .tracks()
                .iter()
                .flat_map(|track| &track.clips)
                .filter_map(|clip| {
                    clip.transition
                        .clone()
                        .map(|transition| (clip.id, transition))
                })
                .collect(),
            tracks,
            resolution: (project.settings().width, project.settings().height),
        }
    }

    fn transition_at(&self, clip: &VisualClip, position: u64) -> Option<(&Transition, f64, bool)> {
        let (id, start, duration) = clip.timing();
        let elapsed = position.checked_sub(start)?;
        if elapsed >= duration {
            return None;
        }
        let incoming = self.transitions.get(&id);
        let outgoing = self.transitions_out.get(&id);
        let mut in_length =
            incoming.map_or(0, |transition| transition.duration_frames.min(duration));
        let mut out_length =
            outgoing.map_or(0, |transition| transition.duration_frames.min(duration));
        if in_length > duration - out_length {
            in_length = in_length.min(duration / 2);
            out_length = out_length.min(duration / 2);
        }
        if elapsed < in_length {
            Some((
                incoming?,
                (elapsed + 1) as f64 / (in_length as f64 + 1.0),
                false,
            ))
        } else if out_length > 0 && elapsed >= duration - out_length {
            Some((
                outgoing?,
                (elapsed - (duration - out_length) + 1) as f64 / (out_length as f64 + 1.0),
                true,
            ))
        } else {
            None
        }
    }

    pub fn replace_title(&mut self, clip_id: u64, title: Title) {
        for track in &mut self.tracks {
            for clip in track {
                if let VisualClip::Title {
                    clip_id: id,
                    title: current,
                    ..
                } = clip
                    && *id == clip_id
                {
                    *current = title;
                    return;
                }
            }
        }
    }
}

pub struct Compositor {
    width: u32,
    height: u32,
    plan: VisualPlan,
    layers: VideoLayers,
    glyphs: Glyphs,
}

enum Layer<'a> {
    Media(Arc<FrameImage>),
    Title(&'a Title),
}

impl Compositor {
    pub fn new(plan: VisualPlan, width: u32, height: u32, fps: (u32, u32)) -> Self {
        Self {
            width,
            height,
            plan,
            layers: VideoLayers::new(fps, (width, height)),
            glyphs: Glyphs::default(),
        }
    }

    pub fn frame(
        &mut self,
        position: u64,
        cancelled: &Arc<AtomicBool>,
    ) -> Result<Option<FrameImage>, String> {
        if (!self.plan.transitions.is_empty() || !self.plan.transitions_out.is_empty())
            && self
                .plan
                .tracks
                .iter()
                .flatten()
                .any(|clip| self.plan.transition_at(clip, position).is_some())
        {
            return self.transition_frame(position, cancelled);
        }
        let mut pixels = vec![0; self.width as usize * self.height as usize * 4];
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel.copy_from_slice(&[0, 0, 0, 255]);
        }
        let mut active = Vec::new();
        let mut visible = false;
        let mut pending = Vec::new();
        'layers: for track in self.plan.tracks.iter().rev() {
            for clip in track.iter().rev() {
                match clip {
                    VisualClip::Media(source)
                        if source.start <= position
                            && position - source.start < source.duration =>
                    {
                        visible = true;
                        active.push(source.clip_id);
                        let image = adjust_color(
                            self.layers.layer_at(source, position, cancelled)?,
                            source.color,
                        );
                        let opaque = image.width == self.width
                            && image.height == self.height
                            && image.pixels.len() == pixels.len()
                            && image
                                .pixels
                                .as_chunks::<4>()
                                .0
                                .iter()
                                .all(|pixel| pixel[3] == 255);
                        pending.push(Layer::Media(image));
                        if opaque {
                            break 'layers;
                        }
                    }
                    VisualClip::Title {
                        clip_id: _,
                        start,
                        duration,
                        title,
                    } if *start <= position && position - *start < *duration => {
                        visible = true;
                        pending.push(Layer::Title(title));
                    }
                    _ => {}
                }
            }
        }
        for layer in pending.into_iter().rev() {
            match layer {
                Layer::Media(image) => {
                    for (dest, src) in pixels
                        .as_chunks_mut::<4>()
                        .0
                        .iter_mut()
                        .zip(image.pixels.as_chunks::<4>().0)
                    {
                        blend(dest, src);
                    }
                }
                Layer::Title(title) => {
                    self.glyphs.draw(
                        title,
                        self.width,
                        self.height,
                        self.plan.resolution.0,
                        self.width as usize * 4,
                        &mut pixels,
                    );
                }
            }
        }
        self.layers.retain(&active);
        Ok(visible.then_some(FrameImage {
            width: self.width,
            height: self.height,
            pixels,
        }))
    }

    fn draw_clip(
        &mut self,
        clip: &VisualClip,
        position: u64,
        pixels: &mut [u8],
        active: &mut Vec<u64>,
        cancelled: &Arc<AtomicBool>,
    ) -> Result<(), String> {
        match clip {
            VisualClip::Media(source) => {
                active.push(source.clip_id);
                let image = adjust_color(
                    self.layers.layer_at(source, position, cancelled)?,
                    source.color,
                );
                for (dest, src) in pixels
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .zip(image.pixels.as_chunks::<4>().0)
                {
                    blend(dest, src);
                }
            }
            VisualClip::Title { title, .. } => self.glyphs.draw(
                title,
                self.width,
                self.height,
                self.plan.resolution.0,
                self.width as usize * 4,
                pixels,
            ),
        }
        Ok(())
    }

    fn transition_frame(
        &mut self,
        position: u64,
        cancelled: &Arc<AtomicBool>,
    ) -> Result<Option<FrameImage>, String> {
        let black = || [0, 0, 0, 255].repeat(self.width as usize * self.height as usize);
        let mut pixels = black();
        let mut active = Vec::new();
        let clips: Vec<_> = self
            .plan
            .tracks
            .iter()
            .filter_map(|track| {
                let clip = track.iter().find(|clip| {
                    let (_, start, duration) = clip.timing();
                    position >= start && position - start < duration
                })?;
                let (_, start, _) = clip.timing();
                let transition = self
                    .plan
                    .transition_at(clip, position)
                    .map(|(transition, t, out)| (transition.clone(), t, out));
                let previous = transition
                    .as_ref()
                    .filter(|(_, _, out)| !out)
                    .and_then(|_| {
                        track.iter().find(|other| {
                            let (_, other_start, other_duration) = other.timing();
                            other_start.checked_add(other_duration) == Some(start)
                        })
                    })
                    .cloned();
                Some((clip.clone(), transition, previous))
            })
            .collect();
        for (clip, transition, previous) in clips {
            let Some((transition, progress, out)) = transition else {
                self.draw_clip(&clip, position, &mut pixels, &mut active, cancelled)?;
                continue;
            };
            let mut incoming = pixels.clone();
            let dip_through_black =
                !out && transition.kind == TransitionKind::Fade && previous.is_some();
            if let Some(previous) = previous {
                self.draw_clip(&previous, position, &mut pixels, &mut active, cancelled)?;
            }
            let t = if out { 1.0 - progress } else { progress };
            let distance = 1.0 - t;
            let (dx, dy) = match transition.direction {
                SlideDirection::Left => (-((distance * f64::from(self.width)).round() as i64), 0),
                SlideDirection::Right => ((distance * f64::from(self.width)).round() as i64, 0),
                SlideDirection::Up => (0, -((distance * f64::from(self.height)).round() as i64)),
                SlideDirection::Down => (0, (distance * f64::from(self.height)).round() as i64),
            };
            if transition.kind == TransitionKind::Slide {
                copy_shifted(&pixels, &mut incoming, self.width, self.height, -dx, -dy);
            }
            self.draw_clip(&clip, position, &mut incoming, &mut active, cancelled)?;
            match transition.kind {
                TransitionKind::Dissolve | TransitionKind::Fade => {
                    for (dest, src) in pixels
                        .as_chunks_mut::<4>()
                        .0
                        .iter_mut()
                        .zip(incoming.as_chunks::<4>().0)
                    {
                        for channel in 0..3 {
                            let value = if !dip_through_black {
                                f64::from(dest[channel]) * (1.0 - t) + f64::from(src[channel]) * t
                            } else if t < 0.5 {
                                f64::from(dest[channel]) * (1.0 - 2.0 * t)
                            } else {
                                f64::from(src[channel]) * (2.0 * t - 1.0)
                            };
                            dest[channel] = value.round() as u8;
                        }
                    }
                }
                TransitionKind::Slide => {
                    copy_shifted(&incoming, &mut pixels, self.width, self.height, dx, dy);
                }
            }
        }
        self.layers.retain(&active);
        Ok(Some(FrameImage {
            width: self.width,
            height: self.height,
            pixels,
        }))
    }
}

fn copy_shifted(source: &[u8], dest: &mut [u8], width: u32, height: u32, dx: i64, dy: i64) {
    let columns = i64::from(width) - dx.abs();
    let rows = i64::from(height) - dy.abs();
    if columns <= 0 || rows <= 0 {
        return;
    }
    let stride = width as usize * 4;
    let source_x = (-dx).max(0) as usize * 4;
    let dest_x = dx.max(0) as usize * 4;
    let source_y = (-dy).max(0) as usize;
    let dest_y = dy.max(0) as usize;
    let length = columns as usize * 4;
    for row in 0..rows as usize {
        let from = (source_y + row) * stride + source_x;
        let to = (dest_y + row) * stride + dest_x;
        dest[to..to + length].copy_from_slice(&source[from..from + length]);
    }
}

#[derive(Default)]
struct Glyphs {
    scaler: ScaleContext,
    cache: HashMap<(u64, u16, u32, bool), GlyphMasks>,
}

struct GlyphMasks {
    fill: Option<Image>,
    outline: Option<Image>,
}

pub(crate) fn title_outline_radius(scaled_font_size: f32) -> f32 {
    scaled_font_size * 0.02
}

impl Glyphs {
    fn draw(
        &mut self,
        title: &Title,
        width: u32,
        height: u32,
        native_width: u32,
        stride: usize,
        pixels: &mut [u8],
    ) {
        let mut canvas = Canvas {
            width,
            height,
            stride,
            pixels,
        };
        let size = (title.font_size * width as f32 / native_width.max(1) as f32)
            .max(1.0)
            .min(height as f32 * 2.0);
        let mut lines = vec![Vec::new()];
        for run in title_markup::parse(title) {
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
        let line_height = size * 1.35;
        let start_y = height as f32 * title.y - line_height * lines.len() as f32 / 2.0;
        let family = title_fonts::family_stack(&title.font_family);
        for (line_index, line) in lines.iter().enumerate() {
            let layouts: Vec<_> = line
                .iter()
                .map(|run| {
                    let face = resolve(
                        &family,
                        if run.bold {
                            FontWeight::Bold
                        } else {
                            FontWeight::Regular
                        },
                    );
                    let layout = cached_layout(
                        &face,
                        &run.text,
                        size,
                        &LayoutSettings {
                            horizontal_align: HorizontalAlign::Left,
                            ..Default::default()
                        },
                    );
                    let run_width = layout.lines.first().map_or(0.0, |line| line.width);
                    (face, layout, run_width)
                })
                .collect();
            let mut x = width as f32 * title.x
                - layouts.iter().map(|(_, _, width)| width).sum::<f32>() / 2.0;
            for (run, (face, layout, run_width)) in line.iter().zip(layouts) {
                for glyph in &layout.glyphs {
                    let face = if glyph.face == 0 {
                        &face
                    } else {
                        &layout.fallback_faces[glyph.face - 1]
                    };
                    let key = (face.id(), glyph.id, size.to_bits(), title.outline);
                    if self.cache.len() >= 4096 && !self.cache.contains_key(&key) {
                        self.cache.clear();
                    }
                    let masks = self.cache.entry(key).or_insert_with(|| {
                        let mut scaler = self
                            .scaler
                            .builder(face.font_ref())
                            .size(size)
                            .hint(false)
                            .build();
                        let mut render = Render::new(&[Source::Outline]);
                        render.format(Format::Alpha);
                        let fill = render.render(&mut scaler, glyph.id);
                        let outline = if title.outline && fill.is_some() {
                            let mut stroke = Stroke::new(2.0 * title_outline_radius(size));
                            stroke.join(Join::Round);
                            render.style(stroke).render(&mut scaler, glyph.id)
                        } else {
                            None
                        };
                        GlyphMasks { fill, outline }
                    });
                    let Some(fill) = &masks.fill else {
                        continue;
                    };
                    let gx = (x + glyph.x).round() as i32;
                    let gy = (start_y + line_index as f32 * line_height + glyph.y).round() as i32;
                    // Use the same shear origin for both masks, even when the outline is taller.
                    let italic_bottom = run
                        .italic
                        .then(|| gy - fill.placement.top + fill.placement.height as i32 - 1);
                    if let Some(outline) = &masks.outline {
                        paint(
                            outline,
                            (gx, gy),
                            &mut canvas,
                            [0, 0, 0, run.color.a],
                            italic_bottom,
                        );
                    }
                    paint(
                        fill,
                        (gx, gy),
                        &mut canvas,
                        [run.color.r, run.color.g, run.color.b, run.color.a],
                        italic_bottom,
                    );
                }
                x += run_width;
            }
        }
    }
}

struct Canvas<'a> {
    width: u32,
    height: u32,
    stride: usize,
    pixels: &'a mut [u8],
}

fn paint(
    mask: &Image,
    origin: (i32, i32),
    canvas: &mut Canvas<'_>,
    color: [u8; 4],
    italic_bottom: Option<i32>,
) {
    let (x, y) = (
        origin.0 + mask.placement.left,
        origin.1 - mask.placement.top,
    );
    let width = mask.placement.width;
    for row in 0..mask.placement.height {
        let py = y + row as i32;
        if py < 0 || py >= canvas.height as i32 {
            continue;
        }
        let shear = italic_bottom.map_or(0, |bottom| ((bottom - py) as f32 * 0.22).round() as i32);
        for col in 0..width {
            let px = x + col as i32 + shear;
            if px < 0 || px >= canvas.width as i32 {
                continue;
            }
            let coverage = u32::from(mask.data[(row * width + col) as usize]);
            let offset = py as usize * canvas.stride + px as usize * 4;
            let mut rgba = color;
            rgba[3] = ((u32::from(color[3]) * coverage + 127) / 255) as u8;
            blend(&mut canvas.pixels[offset..offset + 4], &rgba);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::MediaKind;
    use crate::media::preview::{PreviewEvent, PreviewPlan, decode_sequence};
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    #[test]
    fn color_adjust_maps_pixels() {
        let image = Arc::new(FrameImage {
            width: 1,
            height: 1,
            pixels: vec![100, 150, 200, 73],
        });
        assert!(Arc::ptr_eq(
            &image,
            &adjust_color(image.clone(), ColorAdjust::default())
        ));
        let grey = adjust_color(
            image.clone(),
            ColorAdjust {
                saturation: 0.0,
                ..Default::default()
            },
        );
        assert_eq!(grey.pixels, vec![143, 143, 143, 73]);
        let bright = adjust_color(
            image.clone(),
            ColorAdjust {
                brightness: 0.2,
                ..Default::default()
            },
        );
        assert_eq!(bright.pixels, vec![151, 201, 251, 73]);
        let flat = adjust_color(
            image,
            ColorAdjust {
                contrast: 0.0,
                ..Default::default()
            },
        );
        assert_eq!(flat.pixels, vec![128, 128, 128, 73]);
    }

    #[test]
    fn adjusted_clip_changes_composed_frame() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("color.tga");
        let mut tga = vec![0; 18];
        tga[2] = 2;
        tga[12..14].copy_from_slice(&2u16.to_le_bytes());
        tga[14..16].copy_from_slice(&2u16.to_le_bytes());
        tga[16] = 32;
        tga[17] = 40;
        tga.extend([200, 150, 100, 255].repeat(4));
        std::fs::write(&path, tga).unwrap();
        let plan = |color| VisualPlan {
            resolution: (2, 2),
            transitions: HashMap::new(),
            transitions_out: HashMap::new(),
            tracks: vec![vec![VisualClip::Media(FrameSource {
                color,
                speed: 1.0,
                clip_id: 1,
                path: path.clone(),
                kind: crate::media::MediaKind::Image,
                start: 0,
                source_start: 0,
                duration: 30,
            })]],
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        let raw = Compositor::new(plan(Default::default()), 2, 2, (30, 1))
            .frame(0, &cancelled)
            .unwrap()
            .unwrap();
        let grey = Compositor::new(
            plan(ColorAdjust {
                saturation: 0.0,
                ..Default::default()
            }),
            2,
            2,
            (30, 1),
        )
        .frame(0, &cancelled)
        .unwrap()
        .unwrap();
        assert_ne!(raw.pixels, grey.pixels);
        assert_eq!(grey.pixels, [143, 143, 143, 255].repeat(4));
        for kind in [
            TransitionKind::Fade,
            TransitionKind::Dissolve,
            TransitionKind::Slide,
        ] {
            let mut transition_plan = plan(ColorAdjust {
                saturation: 0.0,
                ..Default::default()
            });
            transition_plan.transitions.insert(
                1,
                Transition {
                    kind,
                    duration_frames: 2,
                    direction: Default::default(),
                },
            );
            let frame = Compositor::new(transition_plan, 2, 2, (30, 1))
                .frame(0, &cancelled)
                .unwrap()
                .unwrap();
            assert!(
                frame.pixels.as_chunks::<4>().0.iter().all(|pixel| {
                    pixel[0] == pixel[1] && pixel[1] == pixel[2] && pixel[3] == 255
                })
            );
            assert!(
                frame
                    .pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|pixel| pixel[0] > 0)
            );
        }
    }
    static NEXT_TEST: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn out_transition_pixels_and_directional_slides() {
        use crate::timeline::SlideDirection;
        ffmpeg_next::init().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let source = |id, color: [u8; 4], start, duration| {
            let path = dir.path().join(format!("polish-{id}.tga"));
            let mut tga = vec![0; 18];
            tga[2] = 2;
            tga[12..14].copy_from_slice(&4u16.to_le_bytes());
            tga[14..16].copy_from_slice(&4u16.to_le_bytes());
            tga[16] = 32;
            tga[17] = 40;
            tga.extend([color[2], color[1], color[0], color[3]].repeat(16));
            fs::write(&path, tga).unwrap();
            VisualClip::Media(FrameSource {
                color: Default::default(),
                speed: 1.0,
                clip_id: id,
                path,
                kind: MediaKind::Image,
                start,
                source_start: 0,
                duration,
            })
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        for lower in [false, true] {
            for out in [false, true] {
                for kind in [
                    TransitionKind::Fade,
                    TransitionKind::Dissolve,
                    TransitionKind::Slide,
                ] {
                    for direction in [
                        SlideDirection::Left,
                        SlideDirection::Right,
                        SlideDirection::Up,
                        SlideDirection::Down,
                    ] {
                        for alpha in [0, 128, 255] {
                            let mut plan = VisualPlan {
                                transitions: HashMap::new(),
                                transitions_out: HashMap::new(),
                                tracks: if lower {
                                    vec![vec![source(1, [240, 0, 0, 255], 0, 20)]]
                                } else {
                                    vec![]
                                },
                                resolution: (4, 4),
                            };
                            plan.tracks.push(vec![source(2, [0, 0, 240, alpha], 5, 5)]);
                            let transition = Transition {
                                kind,
                                direction,
                                duration_frames: 3,
                            };
                            if out {
                                plan.transitions_out.insert(2, transition);
                            } else {
                                plan.transitions.insert(2, transition);
                            }
                            let mut compositor = Compositor::new(plan, 4, 4, (30, 1));
                            let under = if lower {
                                [240, 0, 0, 255]
                            } else {
                                [0, 0, 0, 255]
                            };
                            let mut fully_visible = under;
                            blend(&mut fully_visible, &[0, 0, 240, alpha]);
                            for phase in 0..3 {
                                let position = if out { 7 + phase } else { 5 + phase };
                                let t = (phase + 1) as f64 / 4.0;
                                let opacity = if out { 1.0 - t } else { t };
                                let frame =
                                    compositor.frame(position, &cancelled).unwrap().unwrap();
                                let offset = if out { phase + 1 } else { 3 - phase } as i32;
                                let (dx, dy) = match direction {
                                    SlideDirection::Left => (-offset, 0),
                                    SlideDirection::Right => (offset, 0),
                                    SlideDirection::Up => (0, -offset),
                                    SlideDirection::Down => (0, offset),
                                };
                                for y in 0..4i32 {
                                    for x in 0..4i32 {
                                        let expected = if kind == TransitionKind::Slide {
                                            if (0..4).contains(&(x - dx))
                                                && (0..4).contains(&(y - dy))
                                            {
                                                fully_visible
                                            } else {
                                                under
                                            }
                                        } else {
                                            std::array::from_fn(|channel| {
                                                (f64::from(under[channel]) * (1.0 - opacity)
                                                    + f64::from(fully_visible[channel]) * opacity)
                                                    .round()
                                                    as u8
                                            })
                                        };
                                        let i = (y * 4 + x) as usize * 4;
                                        assert_eq!(
                                            &frame.pixels[i..i + 4],
                                            &expected,
                                            "{kind:?} {direction:?} out={out} lower={lower} phase={phase} x={x} y={y}"
                                        );
                                    }
                                }
                            }
                            if out {
                                assert_eq!(
                                    &compositor.frame(6, &cancelled).unwrap().unwrap().pixels[..4],
                                    &fully_visible
                                );
                            } else {
                                assert_eq!(
                                    &compositor.frame(8, &cancelled).unwrap().unwrap().pixels[..4],
                                    &fully_visible
                                );
                            }
                        }
                    }
                }
            }
        }
        let transition = Transition {
            kind: TransitionKind::Dissolve,
            direction: SlideDirection::Right,
            duration_frames: u64::MAX,
        };
        for duration in [1, 2, 3, 4] {
            let plan = VisualPlan {
                transitions: HashMap::from([(2, transition.clone())]),
                transitions_out: HashMap::from([(2, transition.clone())]),
                tracks: vec![vec![source(2, [0, 0, 240, 255], 0, duration)]],
                resolution: (4, 4),
            };
            let mut compositor = Compositor::new(plan.clone(), 4, 4, (30, 1));
            for position in 0..duration {
                let length = duration / 2;
                let opacity = if position < length {
                    (position + 1) as f64 / (length + 1) as f64
                } else if position >= duration - length {
                    (duration - position) as f64 / (length + 1) as f64
                } else {
                    1.0
                };
                assert_eq!(
                    &compositor
                        .frame(position, &cancelled)
                        .unwrap()
                        .unwrap()
                        .pixels[..4],
                    &[0, 0, (240.0 * opacity).round() as u8, 255]
                );
            }
            assert!(compositor.frame(duration, &cancelled).unwrap().is_none());
            let mut out_only = plan;
            out_only.transitions.clear();
            let mut compositor = Compositor::new(out_only, 4, 4, (30, 1));
            for position in 0..duration {
                let opacity = (duration - position) as f64 / (duration + 1) as f64;
                assert_eq!(
                    &compositor
                        .frame(position, &cancelled)
                        .unwrap()
                        .unwrap()
                        .pixels[..4],
                    &[0, 0, (240.0 * opacity).round() as u8, 255]
                );
            }
        }
        let mut legacy = serde_json::to_value(Project::new("Legacy slide")).unwrap();
        legacy["version"] = 1.into();
        legacy["settings"]["width"] = 4.into();
        legacy["settings"]["height"] = 4.into();
        legacy["media"] = serde_json::json!([crate::media::Media {
            id: 1,
            path: dir.path().join("polish-2.tga"),
            kind: MediaKind::Image,
            duration_us: None,
            has_audio: false,
        }]);
        legacy["tracks"][0]["clips"] = serde_json::json!([{
            "id": 2, "start_frame": 5, "source_start_frame": 0,
            "duration_frames": 5, "content": {"media": {"media_id": 1}},
            "volume": 1.0, "muted": false,
            "transition": {"kind": "slide", "duration_frames": 3}
        }]);
        let old_project = crate::project::format::from_json(&legacy.to_string()).unwrap();
        legacy["tracks"][0]["clips"][0]["transition"]["direction"] = "right".into();
        legacy["tracks"][0]["clips"][0]["transition_out"] = serde_json::Value::Null;
        let new_project = crate::project::format::from_json(&legacy.to_string()).unwrap();
        let mut old = Compositor::new(VisualPlan::from_project(&old_project), 4, 4, (30, 1));
        let mut new = Compositor::new(VisualPlan::from_project(&new_project), 4, 4, (30, 1));
        for position in 0..11 {
            assert_eq!(
                old.frame(position, &cancelled)
                    .unwrap()
                    .map(|frame| frame.pixels),
                new.frame(position, &cancelled)
                    .unwrap()
                    .map(|frame| frame.pixels)
            );
        }
        legacy["tracks"][0]["clips"][0]
            .as_object_mut()
            .unwrap()
            .remove("transition");
        legacy["tracks"][0]["clips"][0]["transition_out"] =
            serde_json::json!({"kind": "fade", "duration_frames": 3});
        legacy["media"][0]["path"] = dir
            .path()
            .join("missing.tga")
            .to_string_lossy()
            .into_owned()
            .into();
        let missing = crate::project::format::from_json(&legacy.to_string()).unwrap();
        assert!(
            Compositor::new(VisualPlan::from_project(&missing), 4, 4, (30, 1))
                .frame(8, &cancelled)
                .is_err()
        );
    }

    #[test]
    fn hidden_video_tracks_do_not_contribute_titles_or_decode_missing_media() {
        let mut value = serde_json::to_value(Project::new("Hidden")).unwrap();
        value["media"] = serde_json::json!([{"id":1,"path":"must-not-open.png","kind":"image","has_audio":false}]);
        value["tracks"][0]["clips"] = serde_json::json!([{"id":1,"start_frame":0,"source_start_frame":0,"duration_frames":30,"content":{"media":{"media_id":1}},"volume":1.0,"muted":false}]);
        value["tracks"][1]["clips"] = serde_json::json!([{"id":2,"start_frame":0,"source_start_frame":0,"duration_frames":30,"content":{"title":Title::default()},"volume":1.0,"muted":false}]);
        value["tracks"][0]["hidden"] = true.into();
        value["tracks"][1]["hidden"] = true.into();
        let project: Project = serde_json::from_value(value.clone()).unwrap();
        let plan = VisualPlan::from_project(&project);
        assert!(plan.tracks.is_empty());
        assert!(
            Compositor::new(plan, 160, 90, (30, 1))
                .frame(0, &Arc::new(AtomicBool::new(false)))
                .unwrap()
                .is_none()
        );
        value["tracks"][1]["hidden"] = false.into();
        let project: Project = serde_json::from_value(value).unwrap();
        assert_eq!(VisualPlan::from_project(&project).tracks.len(), 1);
    }

    #[test]
    fn adjacent_video_continues_after_out_and_holds_at_source_eof() {
        ffmpeg_next::init().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let raw = dir.path().join("frames.rgb");
        let video = dir.path().join("out.mkv");
        let colors = [[240, 0, 0], [0, 240, 0], [0, 0, 240], [240, 240, 0]];
        fs::write(
            &raw,
            colors
                .into_iter()
                .flat_map(|color| color.repeat(8))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert!(
            std::process::Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "rawvideo",
                    "-pixel_format",
                    "rgb24",
                    "-video_size",
                    "4x2",
                    "-framerate",
                    "30",
                    "-i",
                ])
                .arg(&raw)
                .args(["-c:v", "ffv1"])
                .arg(&video)
                .status()
                .unwrap()
                .success()
        );
        let still = dir.path().join("black.tga");
        let mut tga = vec![0; 18];
        tga[2] = 2;
        tga[12..14].copy_from_slice(&4u16.to_le_bytes());
        tga[14..16].copy_from_slice(&2u16.to_le_bytes());
        tga[16] = 32;
        tga[17] = 40;
        tga.extend([0, 0, 0, 255].repeat(8));
        fs::write(&still, tga).unwrap();
        let plan = VisualPlan {
            transitions_out: HashMap::new(),
            resolution: (4, 2),
            transitions: HashMap::from([(
                2,
                Transition {
                    direction: Default::default(),
                    kind: TransitionKind::Dissolve,
                    duration_frames: 3,
                },
            )]),
            tracks: vec![vec![
                VisualClip::Media(FrameSource {
                    speed: 1.0,
                    clip_id: 1,
                    path: video,
                    color: Default::default(),
                    kind: MediaKind::Video,
                    start: 0,
                    source_start: 0,
                    duration: 2,
                }),
                VisualClip::Media(FrameSource {
                    speed: 1.0,
                    clip_id: 2,
                    path: still,
                    color: Default::default(),
                    kind: MediaKind::Image,
                    start: 2,
                    source_start: 0,
                    duration: 5,
                }),
            ]],
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut compositor = Compositor::new(plan.clone(), 4, 2, (30, 1));
        for (position, expected) in [
            (2, [0, 0, 180, 255]),
            (3, [120, 120, 0, 255]),
            (4, [60, 60, 0, 255]),
        ] {
            assert_eq!(
                &compositor
                    .frame(position, &cancelled)
                    .unwrap()
                    .unwrap()
                    .pixels[..4],
                &expected
            );
        }
        assert_eq!(
            &Compositor::new(plan.clone(), 4, 2, (30, 1))
                .frame(4, &cancelled)
                .unwrap()
                .unwrap()
                .pixels[..4],
            &[60, 60, 0, 255]
        );
        for (speed, colors) in [
            (0.5, [[0, 180, 0, 255], [0, 120, 0, 255], [0, 0, 60, 255]]),
            (
                2.0,
                [[180, 180, 0, 255], [120, 120, 0, 255], [60, 60, 0, 255]],
            ),
        ] {
            let mut sped = plan.clone();
            let VisualClip::Media(source) = &mut sped.tracks[0][0] else {
                unreachable!()
            };
            source.speed = speed;
            let mut compositor = Compositor::new(sped, 4, 2, (30, 1));
            for (position, expected) in (2..5).zip(colors) {
                assert_eq!(
                    &compositor
                        .frame(position, &cancelled)
                        .unwrap()
                        .unwrap()
                        .pixels[..4],
                    &expected
                );
            }
        }
        for speed in [0.25, 0.5, 1.0, 1.5, 2.0, 4.0] {
            for kind in [
                TransitionKind::Fade,
                TransitionKind::Dissolve,
                TransitionKind::Slide,
            ] {
                for direction in [
                    SlideDirection::Left,
                    SlideDirection::Right,
                    SlideDirection::Up,
                    SlideDirection::Down,
                ] {
                    let mut sped = plan.clone();
                    sped.tracks[0].truncate(1);
                    let VisualClip::Media(source) = &mut sped.tracks[0][0] else {
                        unreachable!()
                    };
                    source.speed = speed;
                    source.duration = 5;
                    sped.transitions.clear();
                    sped.transitions_out.insert(
                        1,
                        Transition {
                            kind,
                            direction,
                            duration_frames: 3,
                        },
                    );
                    let mut compositor = Compositor::new(sped, 4, 2, (30, 1));
                    for position in 0..5 {
                        let color = colors[((position as f64 * speed).floor() as usize).min(3)];
                        let t = if position < 2 {
                            1.0
                        } else {
                            (5 - position) as f64 / 4.0
                        };
                        let (dx, dy) = match direction {
                            SlideDirection::Left => (-((1.0 - t) * 4.0).round() as i32, 0),
                            SlideDirection::Right => (((1.0 - t) * 4.0).round() as i32, 0),
                            SlideDirection::Up => (0, -((1.0 - t) * 2.0).round() as i32),
                            SlideDirection::Down => (0, ((1.0 - t) * 2.0).round() as i32),
                        };
                        let actual = compositor.frame(position, &cancelled).unwrap().unwrap();
                        for y in 0..2 {
                            for x in 0..4 {
                                let rgb = if kind != TransitionKind::Slide {
                                    color.map(|value| (f64::from(value) * t).round() as u8)
                                } else if (0..4).contains(&(x - dx)) && (0..2).contains(&(y - dy)) {
                                    color
                                } else {
                                    [0; 3]
                                };
                                let offset = ((y * 4 + x) * 4) as usize;
                                assert_eq!(
                                    &actual.pixels[offset..offset + 3],
                                    &rgb,
                                    "{speed}x {kind:?} {direction:?} frame {position}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn transition_pixels_cover_boundaries_underneath_and_slide() {
        use crate::timeline::{Transition, TransitionKind};
        ffmpeg_next::init().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let source = |id, color: [u8; 4], start, duration| {
            let path = dir.path().join(format!("{id}.tga"));
            let mut tga = vec![0; 18];
            tga[2] = 2;
            tga[12..14].copy_from_slice(&4u16.to_le_bytes());
            tga[14..16].copy_from_slice(&2u16.to_le_bytes());
            tga[16] = 32;
            tga[17] = 40;
            tga.extend([color[2], color[1], color[0], color[3]].repeat(8));
            fs::write(&path, tga).unwrap();
            VisualClip::Media(FrameSource {
                speed: 1.0,
                clip_id: id,
                path,
                color: Default::default(),
                kind: MediaKind::Image,
                start,
                source_start: 0,
                duration,
            })
        };
        let red = source(1, [240, 0, 0, 255], 0, 20);
        let blue = source(2, [0, 0, 240, 255], 5, 10);
        let cancelled = Arc::new(AtomicBool::new(false));
        for alpha in [0, 128] {
            let overlay_plan = VisualPlan {
                transitions_out: HashMap::new(),
                resolution: (4, 2),
                transitions: HashMap::from([(
                    3,
                    Transition {
                        direction: Default::default(),
                        kind: TransitionKind::Fade,
                        duration_frames: 3,
                    },
                )]),
                tracks: vec![
                    vec![red.clone()],
                    vec![source(3, [0, 0, 240, alpha], 5, 10)],
                ],
            };
            let mut normal_plan = overlay_plan.clone();
            normal_plan.transitions.clear();
            let background = full_blend(&normal_plan, 4);
            let foreground = full_blend(&normal_plan, 5);
            let mut compositor = Compositor::new(overlay_plan, 4, 2, (30, 1));
            for position in 5..=8 {
                let t = f64::from((position - 4).min(4) as u32) / 4.0;
                let expected: Vec<_> = background
                    .iter()
                    .zip(&foreground)
                    .map(|(&under, &over)| {
                        (f64::from(under) * (1.0 - t) + f64::from(over) * t).round() as u8
                    })
                    .collect();
                assert_eq!(
                    compositor
                        .frame(position, &cancelled)
                        .unwrap()
                        .unwrap()
                        .pixels,
                    expected,
                    "overlay alpha={alpha}, position={position}"
                );
            }
        }
        for adjacent in [false, true] {
            let mut underneath = red.clone();
            if adjacent && let VisualClip::Media(source) = &mut underneath {
                source.duration = 5;
            }
            for kind in [
                TransitionKind::Dissolve,
                TransitionKind::Fade,
                TransitionKind::Slide,
            ] {
                let mut plan = VisualPlan {
                    transitions_out: HashMap::new(),
                    resolution: (4, 2),
                    transitions: HashMap::from([(
                        2,
                        Transition {
                            direction: Default::default(),
                            kind,
                            duration_frames: 3,
                        },
                    )]),
                    tracks: if adjacent {
                        vec![vec![underneath.clone(), blue.clone()]]
                    } else {
                        vec![vec![underneath.clone()], vec![blue.clone()]]
                    },
                };
                let mut compositor = Compositor::new(plan.clone(), 4, 2, (30, 1));
                for (position, expected) in match kind {
                    TransitionKind::Dissolve => vec![
                        (5, [180, 0, 60, 255]),
                        (6, [120, 0, 120, 255]),
                        (7, [60, 0, 180, 255]),
                    ],
                    TransitionKind::Fade if adjacent => vec![
                        (5, [120, 0, 0, 255]),
                        (6, [0, 0, 0, 255]),
                        (7, [0, 0, 120, 255]),
                    ],
                    TransitionKind::Fade => vec![
                        (5, [180, 0, 60, 255]),
                        (6, [120, 0, 120, 255]),
                        (7, [60, 0, 180, 255]),
                    ],
                    TransitionKind::Slide => vec![
                        (5, [240, 0, 0, 255]),
                        (6, [240, 0, 0, 255]),
                        (7, [240, 0, 0, 255]),
                    ],
                } {
                    let frame = compositor.frame(position, &cancelled).unwrap().unwrap();
                    assert_eq!(
                        &frame.pixels[..4],
                        &expected,
                        "{kind:?} at {position}, adjacent={adjacent}"
                    );
                    if kind == TransitionKind::Slide {
                        let offset = 8 - position as usize;
                        for x in 0..4 {
                            assert_eq!(
                                &frame.pixels[x * 4..x * 4 + 4],
                                if x < offset {
                                    &[240, 0, 0, 255]
                                } else {
                                    &[0, 0, 240, 255]
                                }
                            );
                        }
                    }
                }
                assert_eq!(
                    &compositor.frame(8, &cancelled).unwrap().unwrap().pixels[..4],
                    &[0, 0, 240, 255]
                );
                plan.transitions.clear();
                let expected = full_blend(&plan, 6);
                assert_eq!(
                    Compositor::new(plan, 4, 2, (30, 1))
                        .frame(6, &cancelled)
                        .unwrap()
                        .unwrap()
                        .pixels,
                    expected
                );
            }
        }
        let plan = VisualPlan {
            transitions_out: HashMap::new(),
            resolution: (4, 2),
            transitions: HashMap::from([(
                2,
                Transition {
                    direction: Default::default(),
                    kind: TransitionKind::Dissolve,
                    duration_frames: 30,
                },
            )]),
            tracks: vec![vec![blue.clone()]],
        };
        let mut compositor = Compositor::new(plan.clone(), 4, 2, (30, 1));
        assert!(compositor.frame(4, &cancelled).unwrap().is_none());
        assert_eq!(
            &compositor.frame(5, &cancelled).unwrap().unwrap().pixels[..4],
            &[0, 0, 22, 255]
        );
        assert_eq!(
            &compositor.frame(14, &cancelled).unwrap().unwrap().pixels[..4],
            &[0, 0, 218, 255]
        );
        assert!(compositor.frame(15, &cancelled).unwrap().is_none());
        let mut missing = plan;
        if let VisualClip::Media(source) = &mut missing.tracks[0][0] {
            source.path = dir.path().join("missing.tga");
        }
        assert!(
            Compositor::new(missing, 4, 2, (30, 1))
                .frame(5, &cancelled)
                .is_err()
        );
    }

    #[test]
    #[ignore = "release-mode still-image benchmark; requires ffmpeg"]
    fn benchmark_still_image() {
        let root = std::env::temp_dir().join(format!("ocus-still-bench-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("still.png");
        assert!(
            std::process::Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-y",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=size=4000x3000",
                    "-frames:v",
                    "1",
                    "-threads",
                    "1"
                ])
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        let plan = VisualPlan {
            transitions_out: HashMap::new(),
            transitions: HashMap::new(),
            resolution: (1920, 1080),
            tracks: vec![vec![VisualClip::Media(FrameSource {
                speed: 1.0,
                color: Default::default(),
                clip_id: 1,
                path,
                kind: MediaKind::Image,
                start: 0,
                source_start: 0,
                duration: 300,
            })]],
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut compositor = Compositor::new(plan, 1920, 1080, (30, 1));
        let started = std::time::Instant::now();
        let mut hash = 0xcbf29ce484222325u64;
        for position in 0..300 {
            let frame = compositor.frame(position, &cancelled).unwrap().unwrap();
            for byte in &frame.pixels {
                hash = (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
            }
            std::hint::black_box(&frame);
        }
        println!(
            "still frames=300 elapsed_ms={} fnv1a={hash:016x}",
            started.elapsed().as_millis()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[ignore = "release-mode held-video-frame benchmark; requires ffmpeg"]
    fn benchmark_video_frame_rates() {
        let root = std::env::temp_dir().join(format!("ocus-video-bench-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        for (rate, fps) in [(5, (30, 1)), (24, (60, 1)), (30, (30, 1))] {
            let path = root.join(format!("{rate}fps.mp4"));
            assert!(
                std::process::Command::new("ffmpeg")
                    .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
                    .arg(format!("testsrc2=size=1920x1080:rate={rate}:duration=10"))
                    .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-threads", "1"])
                    .arg(&path)
                    .status()
                    .unwrap()
                    .success()
            );
            let frames = 10 * u64::from(fps.0 / fps.1);
            let plan = VisualPlan {
                transitions_out: HashMap::new(),
                transitions: HashMap::new(),
                resolution: (1920, 1080),
                tracks: vec![vec![VisualClip::Media(FrameSource {
                    speed: 1.0,
                    color: Default::default(),
                    clip_id: 1,
                    path,
                    kind: MediaKind::Video,
                    start: 0,
                    source_start: 0,
                    duration: frames,
                })]],
            };
            let cancelled = Arc::new(AtomicBool::new(false));
            let mut compositor = Compositor::new(plan, 1920, 1080, fps);
            let mut elapsed = std::time::Duration::ZERO;
            let mut hash = 0xcbf29ce484222325u64;
            for position in 0..frames {
                let started = std::time::Instant::now();
                let frame = compositor.frame(position, &cancelled).unwrap().unwrap();
                elapsed += started.elapsed();
                for byte in &frame.pixels {
                    hash = (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
                }
            }
            println!(
                "video source_fps={rate} sequence_fps={} frames={frames} elapsed_ms={} fnv1a={hash:016x}",
                fps.0,
                elapsed.as_millis()
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    fn full_blend(plan: &VisualPlan, position: u64) -> Vec<u8> {
        let (width, height) = plan.resolution;
        let mut pixels = [0, 0, 0, 255].repeat(width as usize * height as usize);
        let mut layers = VideoLayers::new((30, 1), (width, height));
        let mut glyphs = Glyphs::default();
        let cancelled = Arc::new(AtomicBool::new(false));
        for clip in plan.tracks.iter().flatten() {
            match clip {
                VisualClip::Media(source)
                    if source.start <= position && position - source.start < source.duration =>
                {
                    let image = layers.layer_at(source, position, &cancelled).unwrap();
                    for (dest, src) in pixels
                        .as_chunks_mut::<4>()
                        .0
                        .iter_mut()
                        .zip(image.pixels.as_chunks::<4>().0)
                    {
                        blend(dest, src);
                    }
                }
                VisualClip::Title {
                    start,
                    duration,
                    title,
                    ..
                } if *start <= position && position - *start < *duration => {
                    glyphs.draw(title, width, height, width, width as usize * 4, &mut pixels);
                }
                _ => {}
            }
        }
        pixels
    }

    #[test]
    fn covered_layers_match_full_blend_without_decoding_hidden_media() {
        let dir = std::env::temp_dir().join(format!(
            "ocus-coverage-{}-{}",
            std::process::id(),
            NEXT_TEST.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        let fixture = |name: &str, width: u16, height: u16, alpha| {
            let path = dir.join(name);
            let mut tga = vec![0; 18];
            tga[2] = 2;
            tga[12..14].copy_from_slice(&width.to_le_bytes());
            tga[14..16].copy_from_slice(&height.to_le_bytes());
            tga[16] = 32;
            tga[17] = 40;
            tga.extend(
                [210 - alpha / 2, 70, alpha / 2, alpha].repeat(width as usize * height as usize),
            );
            fs::write(&path, tga).unwrap();
            path
        };
        let opaque = fixture("opaque.tga", 16, 9, 255);
        let fitted = fixture("fitted.tga", 8, 9, 255);
        let alpha = fixture("alpha.tga", 16, 9, 128);
        let transparent = fixture("transparent.tga", 16, 9, 0);
        let source = |id, path| {
            VisualClip::Media(FrameSource {
                speed: 1.0,
                color: Default::default(),
                clip_id: id,
                path,
                kind: MediaKind::Image,
                start: 0,
                source_start: 0,
                duration: 3,
            })
        };
        let title = VisualClip::Title {
            clip_id: 20,
            start: 0,
            duration: 3,
            title: Title {
                text: "X".into(),
                font_size: 20.0,
                ..Title::default()
            },
        };
        let stacked: Vec<_> = (0..10).map(|id| vec![source(id, opaque.clone())]).collect();
        let mut titled = stacked.clone();
        titled.push(vec![title.clone()]);
        let scenarios = [
            (stacked, 1),
            (titled, 1),
            (
                vec![vec![source(0, opaque.clone())], vec![source(1, fitted)]],
                2,
            ),
            (
                vec![vec![source(0, opaque.clone())], vec![source(1, alpha)]],
                2,
            ),
            (
                vec![
                    vec![source(0, opaque.clone())],
                    vec![source(1, transparent)],
                ],
                2,
            ),
            (vec![vec![source(0, opaque.clone())], vec![title]], 1),
        ];
        let cancelled = Arc::new(AtomicBool::new(false));
        for (tracks, decoded) in scenarios {
            let mut plan = VisualPlan {
                transitions_out: HashMap::new(),
                transitions: HashMap::new(),
                tracks,
                resolution: (160, 90),
            };
            let expected = full_blend(&plan, 0);
            let media_count = plan
                .tracks
                .iter()
                .flatten()
                .filter(|clip| matches!(clip, VisualClip::Media(_)))
                .count();
            for clip in plan
                .tracks
                .iter_mut()
                .flatten()
                .filter(|clip| matches!(clip, VisualClip::Media(_)))
                .take(media_count - decoded)
            {
                if let VisualClip::Media(source) = clip {
                    source.path = dir.join("must-not-be-decoded.tga");
                }
            }
            let mut compositor = Compositor::new(plan, 160, 90, (30, 1));
            assert_eq!(
                compositor.frame(0, &cancelled).unwrap().unwrap().pixels,
                expected
            );
            assert!(compositor.frame(3, &cancelled).unwrap().is_none());
        }
        let plan = VisualPlan {
            transitions_out: HashMap::new(),
            transitions: HashMap::new(),
            tracks: vec![
                vec![source(0, dir.join("missing.tga"))],
                vec![source(1, opaque)],
            ],
            resolution: (160, 90),
        };
        let mut compositor = Compositor::new(plan, 160, 90, (30, 1));
        assert!(compositor.frame(0, &cancelled).unwrap().is_some());
        compositor.plan.tracks.pop();
        assert!(compositor.frame(0, &cancelled).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn translucent_video_pixel_blends_over_lower_layer() {
        let mut background = [20, 40, 80, 255];
        crate::media::preview::blend(&mut background, &[220, 120, 0, 128]);
        assert_eq!(background, [120, 80, 40, 255]);
    }

    #[test]
    fn serif_title_matches_the_installed_serif_face() {
        let render = |family: &str| {
            let title = Title {
                text: "Serif".into(),
                font_family: family.into(),
                font_size: 42.0,
                ..Title::default()
            };
            let mut pixels = vec![0; 160 * 90 * 4];
            Glyphs::default().draw(&title, 160, 90, 160, 160 * 4, &mut pixels);
            pixels
        };
        let serif = render("Times New Roman, Georgia, DejaVu Serif, Liberation Serif, Noto Serif");
        assert!(
            render("Serif") == serif,
            "the generic Serif title should render like its concrete serif family"
        );
    }

    #[test]
    fn title_outline_keeps_its_weight_relative_to_the_fill() {
        let mut title = Title {
            text: "Outline".into(),
            font_size: 96.0,
            ..Title::default()
        };
        let mut glyphs = Glyphs::default();
        let mut weights = Vec::new();
        for (width, height) in [(640, 360), (1280, 720), (1920, 1080), (3840, 2160)] {
            let mut pixels = vec![128; width as usize * height as usize * 4];
            for pixel in pixels.as_chunks_mut::<4>().0 {
                pixel[3] = 255;
            }
            title.outline = false;
            glyphs.draw(&title, width, height, 1920, width as usize * 4, &mut pixels);
            let fill: u32 = pixels
                .as_chunks::<4>()
                .0
                .iter()
                .map(|pixel| u32::from(pixel[0] - 128))
                .sum();

            for pixel in pixels.as_chunks_mut::<4>().0 {
                *pixel = [128, 128, 128, 255];
            }
            title.outline = true;
            glyphs.draw(&title, width, height, 1920, width as usize * 4, &mut pixels);
            let outline: u32 = pixels
                .as_chunks::<4>()
                .0
                .iter()
                .map(|pixel| u32::from(128_u8.saturating_sub(pixel[0])))
                .sum();
            assert!(outline > 0 && fill > 0);
            weights.push(outline as f32 / fill as f32);
        }

        let reference = weights[2];
        // At monitor size, subpixel coverage varies more around the antialiased fill.
        assert!(
            weights
                .iter()
                .all(|weight| (0.70..=1.30).contains(&(weight / reference))),
            "outline-to-fill weights at preview, 720p, 1080p and 4K: {weights:?}"
        );
    }

    #[test]
    fn program_preview_and_export_share_lower_track_priority_and_title_occlusion() {
        let dir = std::env::temp_dir().join(format!(
            "ocus-layer-preview-{}-{}",
            std::process::id(),
            NEXT_TEST.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        let red = dir.join("base.ppm");
        let blue = dir.join("overlay.ppm");
        fs::write(
            &red,
            b"P6\n2 2\n255\n\xdc\x0a\x0a\xdc\x0a\x0a\xdc\x0a\x0a\xdc\x0a\x0a",
        )
        .unwrap();
        fs::write(
            &blue,
            b"P6\n2 2\n255\n\x0a\x0a\xdc\x0a\x0a\xdc\x0a\x0a\xdc\x0a\x0a\xdc",
        )
        .unwrap();
        let source = |id, path| {
            VisualClip::Media(FrameSource {
                speed: 1.0,
                color: Default::default(),
                clip_id: id,
                path,
                kind: MediaKind::Image,
                start: 0,
                source_start: 0,
                duration: 3,
            })
        };
        let title = VisualClip::Title {
            clip_id: 3,
            start: 0,
            duration: 3,
            title: Title {
                text: "X".into(),
                font_size: 42.0,
                color: [0, 255, 0, 255],
                ..Title::default()
            },
        };
        let plan = VisualPlan {
            transitions_out: HashMap::new(),
            transitions: HashMap::new(),
            tracks: vec![
                vec![source(1, red.clone())],
                vec![source(2, blue.clone())],
                vec![title.clone()],
            ],
            resolution: (160, 90),
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut compositor = Compositor::new(plan.clone(), 160, 90, (30, 1));
        let image = compositor.frame(0, &cancelled).unwrap().unwrap();
        let picture = 80 * 4;
        assert!(image.pixels[picture + 2] > 180 && image.pixels[picture] < 60);
        assert!(
            image
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[1] > 150 && pixel[2] < 100)
        );
        let mut preview = None;
        decode_sequence(
            &PreviewPlan {
                sources: Vec::new(),
                visual: Some(plan.clone()),
                end: 3,
                fps: (30, 1),
                resolution: (160, 90),
            },
            0,
            false,
            cancelled.clone(),
            None,
            |event| {
                if let PreviewEvent::Frame(_, frame) = event {
                    preview = frame;
                }
            },
        )
        .unwrap();
        assert_eq!(preview.unwrap().pixels, image.pixels);
        let scaled = Compositor::new(plan.clone(), 80, 45, (30, 1))
            .frame(0, &cancelled)
            .unwrap()
            .unwrap();
        let green_count = |pixels: &[u8]| {
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|pixel| pixel[1] > 150 && pixel[2] < 100)
                .count()
        };
        assert!(green_count(&scaled.pixels) > 0);
        assert!(green_count(&scaled.pixels) < green_count(&image.pixels));
        let mut draft = plan.clone();
        draft.replace_title(
            3,
            Title {
                x: 0.8,
                text: "X".into(),
                font_size: 42.0,
                color: [0, 255, 0, 255],
                ..Title::default()
            },
        );
        let moved = Compositor::new(draft, 160, 90, (30, 1))
            .frame(0, &cancelled)
            .unwrap()
            .unwrap();
        let leftmost_green = |pixels: &[u8]| {
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .enumerate()
                .filter(|(_, pixel)| pixel[1] > 150 && pixel[2] < 100)
                .map(|(index, _)| index % 160)
                .min()
                .unwrap()
        };
        assert!(leftmost_green(&moved.pixels) > leftmost_green(&image.pixels));
        let behind = VisualPlan {
            transitions_out: HashMap::new(),
            transitions: HashMap::new(),
            tracks: vec![vec![title], vec![source(2, blue.clone())]],
            resolution: (160, 90),
        };
        let image = Compositor::new(behind, 160, 90, (30, 1))
            .frame(0, &cancelled)
            .unwrap()
            .unwrap();
        assert!(
            !image
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[1] > 150)
        );
        fs::remove_file(red).unwrap();
        fs::remove_file(blue).unwrap();
        fs::remove_dir(dir).unwrap();
    }
}
