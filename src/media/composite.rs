use super::lut::{CompiledLut, Lut};
use super::preview::{FrameImage, FrameSource, VideoLayers, blend};
use crate::project::Project;
use crate::timeline::{
    ClipContent, ColorAdjust, Playback, SlideDirection, Title, TrackKind, Transform, Transition,
    TransitionKind,
};
use crate::ui::{title_fonts, title_markup};
use creamui::fonts::{FontWeight, HorizontalAlign, LayoutSettings, cached_layout, resolve};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use swash::scale::{Render, ScaleContext, Source, image::Image};
use swash::zeno::{Format, Join, Stroke};

pub(super) fn adjust_color(image: Arc<FrameImage>, color: ColorAdjust) -> Arc<FrameImage> {
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
        Self::build(project, missing, true)
    }

    pub fn for_export(project: &Project, burn_subtitles: bool) -> Self {
        Self::build(project, &HashSet::new(), burn_subtitles)
    }

    fn build(project: &Project, missing: &HashSet<u64>, subtitles: bool) -> Self {
        let tracks = project
            .tracks()
            .iter()
            .filter(|track| {
                track.kind == TrackKind::Video && !track.hidden && (subtitles || !track.subtitles)
            })
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
                                playback: clip.playback,
                                lut: clip.lut.clone(),
                                chroma_key: clip.chroma_key,
                                color: clip.color,
                                clip_id: clip.id,
                                path: media.path.clone(),
                                kind: media.kind,
                                start: clip.start_frame,
                                source_start: clip.source_start_frame,
                                duration: clip.duration_frames,
                                keyframes: clip.keyframes.clone(),
                                transform: clip.transform,
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

    pub fn replace_chroma_key(&mut self, clip_id: u64, key: crate::timeline::ChromaKey) {
        for clip in self.tracks.iter_mut().flatten() {
            if let VisualClip::Media(source) = clip
                && source.clip_id == clip_id
            {
                source.chroma_key = Some(key);
                return;
            }
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
    luts: HashMap<std::path::PathBuf, CompiledLut>,
    width: u32,
    height: u32,
    plan: VisualPlan,
    layers: VideoLayers,
    glyphs: Glyphs,
}

enum Layer<'a> {
    Media(Arc<FrameImage>, Transform),
    Title(&'a Title),
}

impl Compositor {
    pub(crate) fn use_proxies(&mut self, enabled: bool) {
        self.layers.use_proxies = enabled;
    }

    pub fn new(plan: VisualPlan, width: u32, height: u32, fps: (u32, u32)) -> Self {
        Self {
            luts: HashMap::new(),
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
                            apply_chroma_key(
                                self.layers.layer_at(source, position, cancelled)?,
                                source.chroma_key,
                            ),
                            source.color,
                        );
                        let image = apply_lut(&mut self.luts, source, image)?;
                        let transform = source.transform_at(position);
                        let opaque = transform.is_identity()
                            && image.width == self.width
                            && image.height == self.height
                            && image.pixels.len() == pixels.len()
                            && image
                                .pixels
                                .as_chunks::<4>()
                                .0
                                .iter()
                                .all(|pixel| pixel[3] == 255);
                        pending.push(Layer::Media(image, transform));
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
                Layer::Media(image, transform) => {
                    if !transform.is_identity() {
                        draw_transformed(&mut pixels, self.width, self.height, &image, transform);
                        continue;
                    }
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
            picture: (0, 0, self.width, self.height),
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
                let position = position.min(
                    source
                        .start
                        .saturating_add(source.duration)
                        .saturating_sub(1),
                );
                let image = adjust_color(
                    apply_chroma_key(
                        self.layers.layer_at(source, position, cancelled)?,
                        source.chroma_key,
                    ),
                    source.color,
                );
                let image = apply_lut(&mut self.luts, source, image)?;
                let transform = source.transform_at(position);
                if !transform.is_identity() {
                    draw_transformed(pixels, self.width, self.height, &image, transform);
                    return Ok(());
                }
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
                let (_, start, duration) = clip.timing();
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
                    .cloned()
                    .map(|mut previous| {
                        if let VisualClip::Media(source) = &mut previous {
                            let handle =
                                duration.min(transition.as_ref().unwrap().0.duration_frames);
                            let handle = if source.playback == Playback::Reverse {
                                let handle = handle.min(source.source_start);
                                source.source_start -= handle;
                                handle
                            } else {
                                handle
                            };
                            source.duration = source.duration.saturating_add(handle);
                        }
                        previous
                    });
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
            picture: (0, 0, self.width, self.height),
            width: self.width,
            height: self.height,
            pixels,
        }))
    }
}

pub(super) fn draw_transformed(
    destination: &mut [u8],
    width: u32,
    height: u32,
    image: &FrameImage,
    transform: Transform,
) {
    let (left, top, picture_width, picture_height) = image.picture;
    let center_x = left as f32 + picture_width as f32 / 2.0;
    let center_y = top as f32 + picture_height as f32 / 2.0;
    let crop_left = left as f32 + picture_width as f32 * transform.crop_left;
    let crop_right = left as f32 + picture_width as f32 * (1.0 - transform.crop_right);
    let crop_top = top as f32 + picture_height as f32 * transform.crop_top;
    let crop_bottom = top as f32 + picture_height as f32 * (1.0 - transform.crop_bottom);
    let (sine, cosine) = transform.rotation.to_radians().sin_cos();
    let offset_x = center_x + transform.x * width as f32;
    let offset_y = center_y + transform.y * height as f32;
    let mut bounds = [
        f32::INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NEG_INFINITY,
    ];
    for (x, y) in [
        (crop_left, crop_top),
        (crop_right, crop_top),
        (crop_left, crop_bottom),
        (crop_right, crop_bottom),
    ] {
        let x = (x - center_x) * transform.scale;
        let y = (y - center_y) * transform.scale;
        let (x, y) = (
            offset_x + cosine * x - sine * y,
            offset_y + sine * x + cosine * y,
        );
        bounds[0] = bounds[0].min(x);
        bounds[1] = bounds[1].min(y);
        bounds[2] = bounds[2].max(x);
        bounds[3] = bounds[3].max(y);
    }
    let integer_translation = transform.scale == 1.0
        && transform.rotation == 0.0
        && (transform.x * width as f32).fract() == 0.0
        && (transform.y * height as f32).fract() == 0.0;
    let source_pixels = image.pixels.as_chunks::<4>().0;
    let x_bounds =
        (bounds[0].floor().max(0.0) as u32)..(bounds[2].ceil().min(width as f32).max(0.0) as u32);
    for y in
        (bounds[1].floor().max(0.0) as u32)..(bounds[3].ceil().min(height as f32).max(0.0) as u32)
    {
        let delta_y = y as f32 + 0.5 - offset_y;
        let source_x = |x: u32| {
            let delta_x = x as f32 + 0.5 - offset_x;
            center_x + (cosine * delta_x + sine * delta_y) / transform.scale
        };
        let source_y = |x: u32| {
            let delta_x = x as f32 + 0.5 - offset_x;
            center_y + (-sine * delta_x + cosine * delta_y) / transform.scale
        };
        let mut span = x_bounds.clone();
        clip_span(&mut span, crop_left, crop_right, cosine >= 0.0, source_x);
        clip_span(&mut span, crop_top, crop_bottom, sine <= 0.0, source_y);
        if span.is_empty() {
            continue;
        }
        let row_start = (y as usize * width as usize + span.start as usize) * 4;
        let row_end = (y as usize * width as usize + span.end as usize) * 4;
        let destination_row = destination[row_start..row_end].as_chunks_mut::<4>().0;
        if integer_translation {
            let source_start = source_y(span.start) as usize * image.width as usize
                + source_x(span.start) as usize;
            for (dest, pixel) in destination_row
                .iter_mut()
                .zip(&source_pixels[source_start..source_start + span.len()])
            {
                let mut rgba = *pixel;
                rgba[3] = (pixel[3] as f32 * transform.opacity).round() as u8;
                blend(dest, &rgba);
            }
            continue;
        }
        for (x, dest) in span.zip(destination_row) {
            let sample_x =
                (source_x(x) - 0.5).clamp(left as f32, (left + picture_width - 1) as f32);
            let sample_y = (source_y(x) - 0.5).clamp(top as f32, (top + picture_height - 1) as f32);
            let sample_left = sample_x as u32;
            let sample_top = sample_y as u32;
            let sample_right = (sample_left + 1).min(left + picture_width - 1);
            let sample_bottom = (sample_top + 1).min(top + picture_height - 1);
            let taps = [
                source_pixels[(sample_top * image.width + sample_left) as usize],
                source_pixels[(sample_bottom * image.width + sample_left) as usize],
                source_pixels[(sample_top * image.width + sample_right) as usize],
                source_pixels[(sample_bottom * image.width + sample_right) as usize],
            ];
            let fraction_x = sample_x - sample_left as f32;
            let fraction_y = sample_y - sample_top as f32;
            let mut rgba = [0; 4];
            if taps.iter().all(|pixel| pixel[3] == 255) {
                // Q12 fractions give Q24 weights; even 255 * 2^24 fits u32.
                let weight_x = (fraction_x * 4096.0 + 0.5) as u32;
                let weight_y = (fraction_y * 4096.0 + 0.5) as u32;
                let weights = [
                    (4096 - weight_x) * (4096 - weight_y),
                    (4096 - weight_x) * weight_y,
                    weight_x * (4096 - weight_y),
                    weight_x * weight_y,
                ];
                for channel in 0..3 {
                    rgba[channel] = ((u32::from(taps[0][channel]) * weights[0]
                        + u32::from(taps[1][channel]) * weights[1]
                        + u32::from(taps[2][channel]) * weights[2]
                        + u32::from(taps[3][channel]) * weights[3]
                        + (1 << 23))
                        >> 24) as u8;
                }
                let alpha = 255.0 * (1.0 - fraction_x) * (1.0 - fraction_y)
                    + 255.0 * (1.0 - fraction_x) * fraction_y
                    + 255.0 * fraction_x * (1.0 - fraction_y)
                    + 255.0 * fraction_x * fraction_y;
                rgba[3] = (alpha * transform.opacity).round() as u8;
            } else {
                let mut sample = [0.0; 4];
                for (pixel, weight_x, weight_y) in [
                    (taps[0], 1.0 - fraction_x, 1.0 - fraction_y),
                    (taps[1], 1.0 - fraction_x, fraction_y),
                    (taps[2], fraction_x, 1.0 - fraction_y),
                    (taps[3], fraction_x, fraction_y),
                ] {
                    let source_alpha = pixel[3] as f32 / 255.0;
                    for (channel, value) in sample.iter_mut().enumerate() {
                        *value += pixel[channel] as f32
                            * weight_x
                            * weight_y
                            * if channel == 3 { 1.0 } else { source_alpha };
                    }
                }
                let alpha = sample[3];
                if alpha > 0.0 {
                    for channel in 0..3 {
                        rgba[channel] = (sample[channel] * 255.0 / alpha).round() as u8;
                    }
                }
                rgba[3] = (alpha * transform.opacity).round() as u8;
            }
            blend(dest, &rgba);
        }
    }
}

// Each inverse-mapped axis is monotonic along a destination row. Searching with
// the sampler's f32 expression preserves its inclusive/exclusive crop edges.
fn clip_span(
    span: &mut std::ops::Range<u32>,
    minimum: f32,
    maximum: f32,
    increasing: bool,
    coordinate: impl Fn(u32) -> f32,
) {
    let partition = |mut start: u32, mut end: u32, threshold: f32| {
        while start < end {
            let middle = start + (end - start) / 2;
            let value = coordinate(middle);
            if if increasing {
                value < threshold
            } else {
                value >= threshold
            } {
                start = middle + 1;
            } else {
                end = middle;
            }
        }
        start
    };
    let (first, last) = if increasing {
        (minimum, maximum)
    } else {
        (maximum, minimum)
    };
    span.start = partition(span.start, span.end, first);
    span.end = partition(span.start, span.end, last);
}

pub(super) fn apply_chroma_key(
    image: Arc<FrameImage>,
    key: Option<crate::timeline::ChromaKey>,
) -> Arc<FrameImage> {
    let Some(key) = key else {
        return image;
    };
    let uv = |color: [u8; 3]| {
        let [r, g, b] = color.map(|v| f32::from(v) / 255.0);
        [
            -0.168736 * r - 0.331264 * g + 0.5 * b,
            0.5 * r - 0.418688 * g - 0.081312 * b,
        ]
    };
    let target = uv(key.color);
    let dominant = (0..3).max_by_key(|&i| key.color[i]).unwrap();
    let mut image = Arc::unwrap_or_clone(image);
    for pixel in image.pixels.as_chunks_mut::<4>().0 {
        let value = uv([pixel[0], pixel[1], pixel[2]]);
        let distance = ((value[0] - target[0]).powi(2) + (value[1] - target[1]).powi(2)).sqrt();
        let t = if key.smoothness == 0.0 {
            if distance <= key.similarity { 0.0 } else { 1.0 }
        } else {
            ((distance - key.similarity) / key.smoothness).clamp(0.0, 1.0)
        };
        let alpha = t * t * (3.0 - 2.0 * t);
        pixel[3] = (f32::from(pixel[3]) * alpha).round() as u8;
        let others =
            (f32::from(pixel[(dominant + 1) % 3]) + f32::from(pixel[(dominant + 2) % 3])) * 0.5;
        let channel = f32::from(pixel[dominant]);
        pixel[dominant] = (channel - (channel - others).max(0.0) * key.spill).round() as u8;
    }
    Arc::new(image)
}

pub(super) fn apply_lut(
    cache: &mut HashMap<std::path::PathBuf, CompiledLut>,
    source: &FrameSource,
    image: Arc<FrameImage>,
) -> Result<Arc<FrameImage>, String> {
    let Some(path) = &source.lut else {
        return Ok(image);
    };
    if !cache.contains_key(path) {
        let lut = Lut::load(path)?.compile();
        cache.insert(path.clone(), lut);
    }
    Ok(cache[path].apply(image))
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

pub(crate) fn title_background_padding(size: f32) -> (f32, f32) {
    (size * 0.2, size * 0.08)
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
            if let Some(color) = title.background
                && !line.is_empty()
            {
                let line_width = layouts.iter().map(|(_, _, width)| width).sum::<f32>();
                let (pad_x, pad_y) = title_background_padding(size);
                let top = start_y + line_index as f32 * line_height;
                for py in ((top + pad_y).floor() as i32).max(0)
                    ..((top + line_height - pad_y).ceil() as i32).min(height as i32)
                {
                    for px in ((x - pad_x).floor() as i32).max(0)
                        ..((x + line_width + pad_x).ceil() as i32).min(width as i32)
                    {
                        let offset = py as usize * canvas.stride + px as usize * 4;
                        blend(&mut canvas.pixels[offset..offset + 4], &color);
                    }
                }
            }
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
    fn chroma_key_alpha_softness_spill_and_disabled() {
        let image = |rgb: [u8; 3]| {
            Arc::new(FrameImage {
                picture: (0, 0, 1, 1),
                width: 1,
                height: 1,
                pixels: vec![rgb[0], rgb[1], rgb[2], 200],
            })
        };
        let key = crate::timeline::ChromaKey::default();
        assert_eq!(apply_chroma_key(image([0, 255, 0]), Some(key)).pixels[3], 0);
        assert_eq!(
            apply_chroma_key(image([255, 0, 0]), Some(key)).pixels[3],
            200
        );
        let soft = apply_chroma_key(image([40, 225, 40]), Some(key));
        assert!(
            soft.pixels[3] > 0 && soft.pixels[3] < 200,
            "{:?}",
            soft.pixels
        );
        assert!(soft.pixels[1] < 225);
        let original = image([0, 255, 0]);
        assert!(Arc::ptr_eq(
            &original,
            &apply_chroma_key(original.clone(), None)
        ));
        let hard = crate::timeline::ChromaKey {
            smoothness: 0.0,
            ..key
        };
        assert_eq!(
            apply_chroma_key(image([0, 255, 0]), Some(hard)).pixels[3],
            0
        );
    }

    #[test]
    fn chroma_key_reveals_lower_track_normally_and_during_transitions() {
        use crate::timeline::{ChromaKey, Transition, TransitionKind};
        let dir = tempfile::tempdir().unwrap();
        let source = |id, rgb: [u8; 3], key| {
            let path = dir.path().join(format!("{id}.tga"));
            let mut tga = vec![0; 18];
            tga[2] = 2;
            tga[12..14].copy_from_slice(&4u16.to_le_bytes());
            tga[14..16].copy_from_slice(&2u16.to_le_bytes());
            tga[16] = 32;
            tga[17] = 40;
            tga.extend([rgb[2], rgb[1], rgb[0], 255].repeat(8));
            fs::write(&path, tga).unwrap();
            VisualClip::Media(FrameSource {
                speed: 1.0,
                playback: Default::default(),
                lut: None,
                chroma_key: key,
                clip_id: id,
                path,
                color: Default::default(),
                kind: MediaKind::Image,
                start: 0,
                source_start: 0,
                duration: 20,
                keyframes: Vec::new(),
                transform: Transform::default(),
            })
        };
        for transition in [
            None,
            Some(TransitionKind::Fade),
            Some(TransitionKind::Dissolve),
            Some(TransitionKind::Slide),
        ] {
            let plan = VisualPlan {
                transitions_out: HashMap::new(),
                resolution: (4, 2),
                transitions: transition
                    .map(|kind| {
                        HashMap::from([(
                            2,
                            Transition {
                                kind,
                                duration_frames: 10,
                                direction: Default::default(),
                            },
                        )])
                    })
                    .unwrap_or_default(),
                tracks: vec![
                    vec![source(1, [240, 0, 0], None)],
                    vec![source(2, [0, 255, 0], Some(ChromaKey::default()))],
                ],
            };
            let mut compositor = Compositor::new(plan, 4, 2, (30, 1));
            let image = compositor
                .frame(4, &Arc::new(AtomicBool::new(false)))
                .unwrap()
                .unwrap();
            for pixel in image.pixels.as_chunks::<4>().0 {
                assert_eq!(*pixel, [240, 0, 0, 255], "{transition:?}");
            }
        }
    }

    #[test]
    fn proxy_crop_uses_the_fitted_picture() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("wide.mp4");
        assert!(
            std::process::Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-y",
                    "-f",
                    "lavfi",
                    "-i",
                    "color=red:s=2048x1024:r=30:d=0.1",
                    "-c:v",
                    "mpeg4",
                    "-threads",
                    "2"
                ])
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        let cancelled = Arc::new(AtomicBool::new(false));
        assert!(crate::media::proxy::generate(&path, &cancelled).unwrap());
        let source = FrameSource {
            keyframes: Vec::new(),
            speed: 1.0,
            playback: crate::timeline::Playback::Forward,
            clip_id: 1,
            path,
            kind: MediaKind::Video,
            start: 0,
            source_start: 0,
            duration: 3,
            color: Default::default(),
            lut: None,
            chroma_key: None,
            transform: Transform {
                crop_left: 0.25,
                ..Default::default()
            },
        };
        for use_proxies in [false, true] {
            let mut layers = VideoLayers::new((30, 1), (32, 32));
            layers.use_proxies = use_proxies;
            let image = layers.layer_at(&source, 0, &cancelled).unwrap();
            assert_eq!(image.picture, (0, 8, 32, 16));
            assert!(image.pixels[(16 * 32 + 16) * 4] > 100);
            let fitted = image;
            let plan = VisualPlan {
                tracks: vec![vec![VisualClip::Media(source.clone())]],
                resolution: (32, 32),
                transitions: HashMap::new(),
                transitions_out: HashMap::new(),
            };
            let mut compositor = Compositor::new(plan, 32, 32, (30, 1));
            compositor.use_proxies(use_proxies);
            let image = compositor.frame(0, &cancelled).unwrap().unwrap();
            for (index, pixel) in image.pixels.as_chunks::<4>().0.iter().enumerate() {
                let (x, y) = (index % 32, index / 32);
                if x < 8 || !(8..24).contains(&y) {
                    assert_eq!(pixel, &[0, 0, 0, 255]);
                } else {
                    assert_eq!(pixel, &fitted.pixels[index * 4..index * 4 + 4]);
                }
            }
        }
    }

    #[test]
    fn color_adjust_maps_pixels() {
        let image = Arc::new(FrameImage {
            picture: (0, 0, 1, 1),
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
                playback: Playback::Forward,
                color,
                speed: 1.0,
                lut: None,
                chroma_key: None,
                clip_id: 1,
                path: path.clone(),
                kind: crate::media::MediaKind::Image,
                start: 0,
                source_start: 0,
                duration: 30,
                keyframes: Vec::new(),
                transform: Transform::default(),
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
    #[test]
    fn color_adjustment_precedes_lut_in_normal_and_transition_frames() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("color.tga");
        let mut tga = vec![0; 18];
        tga[2] = 2;
        tga[12..14].copy_from_slice(&2u16.to_le_bytes());
        tga[14..16].copy_from_slice(&2u16.to_le_bytes());
        tga[16] = 32;
        tga[17] = 40;
        tga.extend([200, 150, 100, 255].repeat(4));
        fs::write(&path, tga).unwrap();
        let lut_path = dir.path().join("invert.cube");
        fs::write(&lut_path, "LUT_1D_SIZE 3\n1 1 1\n0.75 0.75 0.75\n0 0 0\n").unwrap();
        let color = ColorAdjust {
            saturation: 0.0,
            ..Default::default()
        };
        let image = Arc::new(FrameImage {
            width: 2,
            height: 2,
            pixels: [100, 150, 200, 255].repeat(4),
            picture: (0, 0, 2, 2),
        });
        let lut = Lut::load(&lut_path).unwrap().compile();
        let expected = [168, 168, 168, 255].repeat(4);
        assert_eq!(
            lut.apply(adjust_color(image.clone(), color)).pixels,
            expected
        );
        assert_ne!(adjust_color(lut.apply(image), color).pixels, expected);
        let plan = VisualPlan {
            resolution: (2, 2),
            transitions: HashMap::new(),
            transitions_out: HashMap::new(),
            tracks: vec![vec![VisualClip::Media(FrameSource {
                playback: crate::timeline::Playback::Forward,
                color,
                lut: Some(lut_path),
                chroma_key: None,
                speed: 1.0,
                clip_id: 1,
                path,
                kind: MediaKind::Image,
                start: 0,
                source_start: 0,
                duration: 10,
                keyframes: Vec::new(),
                transform: Transform::default(),
            })]],
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut compositor = Compositor::new(plan, 2, 2, (30, 1));
        assert_eq!(
            compositor.frame(0, &cancelled).unwrap().unwrap().pixels,
            expected
        );
        for kind in [
            TransitionKind::Fade,
            TransitionKind::Dissolve,
            TransitionKind::Slide,
        ] {
            compositor.plan.transitions.insert(
                1,
                Transition {
                    kind,
                    duration_frames: 3,
                    direction: Default::default(),
                },
            );
            let expected = if kind == TransitionKind::Slide {
                [[0, 0, 0, 255], [168, 168, 168, 255]].concat().repeat(2)
            } else {
                [126, 126, 126, 255].repeat(4)
            };
            assert_eq!(
                compositor.frame(2, &cancelled).unwrap().unwrap().pixels,
                expected,
                "{kind:?}"
            );
        }
    }

    static NEXT_TEST: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn keyframed_opacity_and_position_change_composed_frames() {
        use crate::timeline::Keyframe;
        ffmpeg_next::init().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("animated.tga");
        let mut tga = vec![0; 18];
        tga[2] = 2;
        tga[12..14].copy_from_slice(&4u16.to_le_bytes());
        tga[14..16].copy_from_slice(&2u16.to_le_bytes());
        tga[16] = 32;
        tga[17] = 40;
        tga.extend([0, 0, 200, 255].repeat(8));
        fs::write(&path, tga).unwrap();
        let source = FrameSource {
            playback: crate::timeline::Playback::Forward,
            path,
            clip_id: 1,
            kind: MediaKind::Image,
            start: 0,
            source_start: 20,
            speed: 1.0,
            duration: 11,
            color: Default::default(),
            lut: None,
            chroma_key: None,
            transform: Transform::default(),
            keyframes: vec![
                Keyframe::new(0, Transform::default()),
                Keyframe::new(
                    10,
                    Transform {
                        x: 0.5,
                        opacity: 0.5,
                        ..Default::default()
                    },
                ),
            ],
        };
        let plan = VisualPlan {
            resolution: (4, 2),
            tracks: vec![vec![VisualClip::Media(source)]],
            transitions: HashMap::new(),
            transitions_out: HashMap::new(),
        };
        let mut compositor = Compositor::new(plan, 4, 2, (30, 1));
        let cancelled = Arc::new(AtomicBool::new(false));
        let first = compositor.frame(0, &cancelled).unwrap().unwrap();
        let last = compositor.frame(10, &cancelled).unwrap().unwrap();
        assert_eq!(&first.pixels[..4], &[200, 0, 0, 255]);
        assert_eq!(&last.pixels[..4], &[0, 0, 0, 255]);
        assert_eq!(&last.pixels[12..16], &[100, 0, 0, 255]);
    }

    #[test]
    fn title_background_is_optional_alpha_blended_and_resolution_scaled() {
        let render = |background, width: u32| {
            let height = width * 9 / 16;
            let mut pixels = vec![0; width as usize * height as usize * 4];
            for pixel in pixels.as_chunks_mut::<4>().0 {
                *pixel = [100, 100, 100, 255];
            }
            Glyphs::default().draw(
                &Title {
                    text: "Subtitle".into(),
                    font_size: 48.0,
                    background,
                    ..Title::default()
                },
                width,
                height,
                640,
                width as usize * 4,
                &mut pixels,
            );
            pixels
        };
        let plain = render(None, 640);
        let transparent = render(Some([0, 0, 0, 0]), 640);
        assert_eq!(plain, transparent);
        let mut areas = Vec::new();
        for width in [320, 640, 1280] {
            let pixels = render(Some([0, 0, 0, 128]), width);
            let area = pixels
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|pixel| **pixel == [50, 50, 50, 255])
                .count();
            assert!(area > 0);
            areas.push(area as f64 / f64::from(width * width));
        }
        assert!(
            areas.iter().all(|area| (area / areas[1] - 1.0).abs() < 0.1),
            "{areas:?}"
        );
        let opaque = render(Some([0, 0, 0, 255]), 640);
        assert!(opaque.as_chunks::<4>().0.contains(&[0, 0, 0, 255]));
        assert!(!plain.as_chunks::<4>().0.contains(&[0, 0, 0, 255]));
    }

    #[test]
    fn transformed_sampler_matches_original() {
        let mut seed = 0x9e37_79b9_u32;
        let mut random = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        let mut transforms = vec![
            Transform::default(),
            Transform {
                scale: 0.5,
                x: 0.2,
                rotation: 15.0,
                crop_left: 0.1,
                opacity: 0.8,
                ..Default::default()
            },
            Transform {
                x: -0.25,
                y: 0.25,
                crop_left: 0.13,
                crop_bottom: 0.2,
                opacity: 0.5,
                ..Default::default()
            },
            Transform {
                rotation: 360.0,
                ..Default::default()
            },
            Transform {
                rotation: -360.0,
                ..Default::default()
            },
            Transform {
                rotation: 90.0,
                ..Default::default()
            },
            Transform {
                rotation: -90.0,
                ..Default::default()
            },
            Transform {
                rotation: 180.0,
                ..Default::default()
            },
            Transform {
                rotation: 15.0,
                opacity: 0.5,
                ..Default::default()
            },
            Transform {
                scale: 0.1,
                rotation: -37.0,
                ..Default::default()
            },
            Transform {
                scale: 4.0,
                rotation: 137.0,
                ..Default::default()
            },
            Transform {
                x: 1.0,
                y: -1.0,
                ..Default::default()
            },
            Transform {
                crop_left: 0.999,
                crop_top: 0.999,
                ..Default::default()
            },
            Transform {
                opacity: 0.0,
                ..Default::default()
            },
        ];
        for _ in 0..32 {
            transforms.push(Transform {
                scale: 0.1 + (random() % 3901) as f32 / 1000.0,
                x: (random() % 2001) as f32 / 1000.0 - 1.0,
                y: (random() % 2001) as f32 / 1000.0 - 1.0,
                rotation: (random() % 72001) as f32 / 100.0 - 360.0,
                crop_left: (random() % 400) as f32 / 1000.0,
                crop_right: (random() % 400) as f32 / 1000.0,
                crop_top: (random() % 400) as f32 / 1000.0,
                crop_bottom: (random() % 400) as f32 / 1000.0,
                opacity: (random() % 1001) as f32 / 1000.0,
            });
        }
        for (width, height, picture) in [
            (1, 1, (0, 0, 1, 1)),
            (16, 12, (7, 5, 1, 1)),
            (97, 61, (7, 5, 83, 51)),
            (1920, 1080, (0, 0, 1920, 1080)),
        ] {
            for alpha_mode in 0..3 {
                if width == 1920 && alpha_mode != 0 {
                    continue;
                }
                let mut image = FrameImage {
                    picture,
                    width,
                    height,
                    pixels: (0..width * height * 4).map(|_| random() as u8).collect(),
                };
                for (index, pixel) in image.pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                    let (x, y) = (index as u32 % width, index as u32 / width);
                    pixel[3] = if x < picture.0
                        || x >= picture.0 + picture.2
                        || y < picture.1
                        || y >= picture.1 + picture.3
                    {
                        0
                    } else {
                        match alpha_mode {
                            0 => 255,
                            1 => 0,
                            _ => pixel[3],
                        }
                    };
                }
                let destination: Vec<u8> =
                    (0..width * height * 4).map(|_| random() as u8).collect();
                let count = if width == 1920 { 9 } else { transforms.len() };
                for transform in &transforms[..count] {
                    assert!(transform.valid());
                    let mut expected = destination.clone();
                    let mut actual = destination.clone();
                    reference_draw_transformed(&mut expected, width, height, &image, *transform);
                    draw_transformed(&mut actual, width, height, &image, *transform);
                    for (index, (actual, expected)) in actual.iter().zip(&expected).enumerate() {
                        assert!(
                            actual.abs_diff(*expected) <= 1,
                            "{width}x{height}, alpha mode {alpha_mode}, {transform:?}, byte {index}: {actual} != {expected}"
                        );
                    }
                }
            }
        }
    }

    // Keep the original scalar sampler as an independent pixel-equivalence oracle.
    fn reference_draw_transformed(
        destination: &mut [u8],
        width: u32,
        height: u32,
        image: &FrameImage,
        transform: Transform,
    ) {
        let (left, top, picture_width, picture_height) = image.picture;
        let center_x = left as f32 + picture_width as f32 / 2.0;
        let center_y = top as f32 + picture_height as f32 / 2.0;
        let crop_left = left as f32 + picture_width as f32 * transform.crop_left;
        let crop_right = left as f32 + picture_width as f32 * (1.0 - transform.crop_right);
        let crop_top = top as f32 + picture_height as f32 * transform.crop_top;
        let crop_bottom = top as f32 + picture_height as f32 * (1.0 - transform.crop_bottom);
        let (sine, cosine) = transform.rotation.to_radians().sin_cos();
        let offset_x = center_x + transform.x * width as f32;
        let offset_y = center_y + transform.y * height as f32;
        let mut bounds = [
            f32::INFINITY,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NEG_INFINITY,
        ];
        for (x, y) in [
            (crop_left, crop_top),
            (crop_right, crop_top),
            (crop_left, crop_bottom),
            (crop_right, crop_bottom),
        ] {
            let x = (x - center_x) * transform.scale;
            let y = (y - center_y) * transform.scale;
            let (x, y) = (
                offset_x + cosine * x - sine * y,
                offset_y + sine * x + cosine * y,
            );
            bounds[0] = bounds[0].min(x);
            bounds[1] = bounds[1].min(y);
            bounds[2] = bounds[2].max(x);
            bounds[3] = bounds[3].max(y);
        }
        for y in (bounds[1].floor().max(0.0) as u32)
            ..(bounds[3].ceil().min(height as f32).max(0.0) as u32)
        {
            for x in (bounds[0].floor().max(0.0) as u32)
                ..(bounds[2].ceil().min(width as f32).max(0.0) as u32)
            {
                let delta_x = x as f32 + 0.5 - offset_x;
                let delta_y = y as f32 + 0.5 - offset_y;
                let source_x = center_x + (cosine * delta_x + sine * delta_y) / transform.scale;
                let source_y = center_y + (-sine * delta_x + cosine * delta_y) / transform.scale;
                if source_x < crop_left
                    || source_x >= crop_right
                    || source_y < crop_top
                    || source_y >= crop_bottom
                {
                    continue;
                }
                let sample_x =
                    (source_x - 0.5).clamp(left as f32, (left + picture_width - 1) as f32);
                let sample_y =
                    (source_y - 0.5).clamp(top as f32, (top + picture_height - 1) as f32);
                let sample_left = sample_x.floor() as u32;
                let sample_top = sample_y.floor() as u32;
                let mut sample = [0.0; 4];
                for (pixel_x, weight_x) in [
                    (sample_left, 1.0 - (sample_x - sample_left as f32)),
                    (
                        (sample_left + 1).min(left + picture_width - 1),
                        sample_x - sample_left as f32,
                    ),
                ] {
                    for (pixel_y, weight_y) in [
                        (sample_top, 1.0 - (sample_y - sample_top as f32)),
                        (
                            (sample_top + 1).min(top + picture_height - 1),
                            sample_y - sample_top as f32,
                        ),
                    ] {
                        let pixel_offset = ((pixel_y * image.width + pixel_x) * 4) as usize;
                        let source_alpha = image.pixels[pixel_offset + 3] as f32 / 255.0;
                        for (channel, value) in sample.iter_mut().enumerate() {
                            *value += image.pixels[pixel_offset + channel] as f32
                                * weight_x
                                * weight_y
                                * if channel == 3 { 1.0 } else { source_alpha };
                        }
                    }
                }
                let alpha = sample[3];
                let mut rgba = [0; 4];
                if alpha > 0.0 {
                    for channel in 0..3 {
                        rgba[channel] = (sample[channel] * 255.0 / alpha).round() as u8;
                    }
                }
                rgba[3] = (alpha * transform.opacity).round() as u8;
                let pixel_offset = ((y * width + x) * 4) as usize;
                blend(&mut destination[pixel_offset..pixel_offset + 4], &rgba);
            }
        }
    }

    #[test]
    #[ignore = "release-mode transform benchmark; requires ffmpeg"]
    fn benchmark_clip_transform() {
        use std::time::Instant;

        let root =
            std::env::temp_dir().join(format!("ocus-transform-bench-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let make_source = |clip_id, name: &str, filter: &str| {
            let path = root.join(name);
            assert!(
                std::process::Command::new("ffmpeg")
                    .args([
                        "-v",
                        "error",
                        "-y",
                        "-f",
                        "lavfi",
                        "-i",
                        filter,
                        "-c:v",
                        "libx264",
                        "-preset",
                        "ultrafast",
                        "-crf",
                        "18",
                        "-threads",
                        "2"
                    ])
                    .arg(&path)
                    .status()
                    .unwrap()
                    .success()
            );
            FrameSource {
                speed: 1.0,
                playback: crate::timeline::Playback::Forward,
                clip_id,
                lut: None,
                chroma_key: None,
                path,
                kind: MediaKind::Video,
                start: 0,
                source_start: 0,
                duration: 90,
                color: Default::default(),
                keyframes: Vec::new(),
                transform: Transform::default(),
            }
        };
        let lower = make_source(1, "lower.mp4", "testsrc2=size=1280x720:rate=30:duration=3");
        let upper = make_source(2, "upper.mp4", "smptebars=size=1280x720:rate=30:duration=3");
        let transform = Transform {
            scale: 0.5,
            x: 0.2,
            rotation: 15.0,
            crop_left: 0.1,
            opacity: 0.8,
            ..Default::default()
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        fn median(label: &str, mut run: impl FnMut() -> f64) {
            let mut timings: Vec<_> = (0..5).map(|_| run()).collect();
            timings.sort_by(f64::total_cmp);
            println!("{label}: median_ms={:.3} runs={timings:?}", timings[2]);
        }
        for (label, source) in [("decode_lower", &lower), ("decode_upper", &upper)] {
            median(
                label,
                Box::new(|| {
                    let mut layers = VideoLayers::new((30, 1), (1920, 1080));
                    layers.layer_at(source, 0, &cancelled).unwrap();
                    let started = Instant::now();
                    for position in 1..61 {
                        std::hint::black_box(
                            layers.layer_at(source, position, &cancelled).unwrap(),
                        );
                    }
                    started.elapsed().as_secs_f64() * 1000.0 / 60.0
                }),
            );
        }
        let image = VideoLayers::new((30, 1), (1920, 1080))
            .layer_at(&upper, 0, &cancelled)
            .unwrap();
        median(
            "draw_transformed",
            Box::new(|| {
                let mut pixels = [0, 0, 0, 255].repeat(1920 * 1080);
                let started = Instant::now();
                for _ in 0..60 {
                    draw_transformed(
                        std::hint::black_box(&mut pixels),
                        1920,
                        1080,
                        &image,
                        transform,
                    );
                }
                std::hint::black_box(pixels);
                started.elapsed().as_secs_f64() * 1000.0 / 60.0
            }),
        );
        for (label, transform) in [
            ("frame_identity", Transform::default()),
            ("frame_transformed", transform),
        ] {
            median(
                label,
                Box::new(|| {
                    let mut upper = upper.clone();
                    upper.transform = transform;
                    let plan = VisualPlan {
                        transitions_out: HashMap::new(),
                        tracks: vec![
                            vec![VisualClip::Media(lower.clone())],
                            vec![VisualClip::Media(upper)],
                        ],
                        resolution: (1280, 720),
                        transitions: HashMap::new(),
                    };
                    let mut compositor = Compositor::new(plan, 1920, 1080, (30, 1));
                    compositor.frame(0, &cancelled).unwrap();
                    let started = Instant::now();
                    for position in 1..61 {
                        std::hint::black_box(compositor.frame(position, &cancelled).unwrap());
                    }
                    started.elapsed().as_secs_f64() * 1000.0 / 60.0
                }),
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn color_and_lut_are_applied_before_transform_in_normal_and_transition_frames() {
        ffmpeg_next::init().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gradient.tga");
        let mut tga = vec![0; 18];
        tga[2] = 2;
        tga[12..14].copy_from_slice(&4u16.to_le_bytes());
        tga[14..16].copy_from_slice(&2u16.to_le_bytes());
        tga[16] = 32;
        tga[17] = 40;
        for _ in 0..2 {
            for value in [10, 60, 130, 240] {
                tga.extend([value, value, value, 255]);
            }
        }
        fs::write(&path, tga).unwrap();
        let lut_path = dir.path().join("invert.cube");
        fs::write(&lut_path, "LUT_1D_SIZE 2\n1 1 1\n0 0 0\n").unwrap();
        let source = FrameSource {
            keyframes: Vec::new(),
            speed: 1.0,
            playback: crate::timeline::Playback::Forward,
            lut: Some(lut_path.clone()),
            chroma_key: None,
            clip_id: 1,
            path,
            kind: MediaKind::Image,
            start: 0,
            source_start: 0,
            duration: 5,
            color: ColorAdjust {
                brightness: 0.2,
                contrast: 2.0,
                saturation: 1.0,
            },
            transform: Transform {
                scale: 0.75,
                rotation: 15.0,
                opacity: 0.8,
                ..Default::default()
            },
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        let image = VideoLayers::new((30, 1), (4, 2))
            .layer_at(&source, 0, &cancelled)
            .unwrap();
        let graded = adjust_color(image.clone(), source.color);
        let with_lut = Lut::load(&lut_path)
            .unwrap()
            .compile()
            .apply(graded.clone());
        assert_ne!(graded.pixels, with_lut.pixels);
        let mut expected = [0, 0, 0, 255].repeat(8);
        draw_transformed(&mut expected, 4, 2, &with_lut, source.transform);
        let mut ungraded = [0, 0, 0, 255].repeat(8);
        draw_transformed(&mut ungraded, 4, 2, &image, source.transform);
        assert_ne!(expected, ungraded);
        for transition in [None, Some(false), Some(true)] {
            let effect = Transition {
                kind: TransitionKind::Fade,
                duration_frames: 3,
                direction: Default::default(),
            };
            let plan = VisualPlan {
                resolution: (4, 2),
                transitions: if transition == Some(false) {
                    HashMap::from([(1, effect.clone())])
                } else {
                    HashMap::new()
                },
                transitions_out: if transition == Some(true) {
                    HashMap::from([(1, effect)])
                } else {
                    HashMap::new()
                },
                tracks: vec![vec![VisualClip::Media(source.clone())]],
            };
            let actual = Compositor::new(plan, 4, 2, (30, 1))
                .frame(if transition == Some(true) { 2 } else { 0 }, &cancelled)
                .unwrap()
                .unwrap();
            let mut expected = expected.clone();
            if let Some(outgoing) = transition {
                for pixel in expected.as_chunks_mut::<4>().0 {
                    for channel in &mut pixel[..3] {
                        *channel = (f64::from(*channel) * if outgoing { 0.75 } else { 0.25 })
                            .round() as u8;
                    }
                }
            }
            assert_eq!(actual.pixels, expected, "transition={transition:?}");
        }
    }

    #[test]
    fn lut_colors_normal_transition_and_errors() {
        ffmpeg_next::init().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("image.tga");
        let mut tga = vec![0; 18];
        tga[2] = 2;
        tga[12..14].copy_from_slice(&2u16.to_le_bytes());
        tga[14..16].copy_from_slice(&2u16.to_le_bytes());
        tga[16] = 32;
        tga[17] = 40;
        tga.extend([200, 64, 128, 255].repeat(4));
        fs::write(&path, tga).unwrap();
        let lut_path = dir.path().join("invert.cube");
        fs::write(&lut_path, "LUT_1D_SIZE 2\n1 1 1\n0 0 0\n").unwrap();
        let source = FrameSource {
            playback: crate::timeline::Playback::Forward,
            lut: None,
            chroma_key: None,
            speed: 1.0,
            color: Default::default(),
            clip_id: 1,
            path,
            kind: MediaKind::Image,
            start: 0,
            source_start: 0,
            duration: 10,
            keyframes: Vec::new(),
            transform: Transform::default(),
        };
        let plan = VisualPlan {
            resolution: (2, 2),
            transitions: HashMap::new(),
            transitions_out: HashMap::new(),
            tracks: vec![vec![VisualClip::Media(source.clone())]],
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut compositor = Compositor::new(plan, 2, 2, (30, 1));
        assert_eq!(
            &compositor.frame(0, &cancelled).unwrap().unwrap().pixels[..4],
            &[128, 64, 200, 255]
        );
        let image = Arc::new(FrameImage {
            width: 1,
            height: 1,
            pixels: vec![128, 64, 200, 137],
            picture: (0, 0, 1, 1),
        });
        assert!(Arc::ptr_eq(
            &image,
            &apply_lut(&mut compositor.luts, &source, image.clone()).unwrap()
        ));
        let VisualClip::Media(source) = &mut compositor.plan.tracks[0][0] else {
            panic!()
        };
        source.lut = Some(lut_path.clone());
        assert_eq!(
            &compositor.frame(0, &cancelled).unwrap().unwrap().pixels[..4],
            &[127, 191, 55, 255]
        );
        let graded = compositor.luts[&lut_path].apply(image);
        assert_eq!(graded.pixels, [127, 191, 55, 137]);
        compositor.plan.transitions.insert(
            1,
            Transition {
                kind: TransitionKind::Dissolve,
                duration_frames: 3,
                direction: Default::default(),
            },
        );
        assert_eq!(
            &compositor.frame(0, &cancelled).unwrap().unwrap().pixels[..4],
            &[32, 48, 14, 255]
        );
        assert_eq!(compositor.luts.len(), 1);
        let VisualClip::Media(source) = &mut compositor.plan.tracks[0][0] else {
            panic!()
        };
        source.lut = Some(dir.path().join("missing.cube"));
        assert!(
            compositor
                .frame(0, &cancelled)
                .err()
                .unwrap()
                .contains("missing.cube")
        );
        let invalid = dir.path().join("invalid.cube");
        fs::write(&invalid, "not a LUT").unwrap();
        let VisualClip::Media(source) = &mut compositor.plan.tracks[0][0] else {
            panic!()
        };
        source.lut = Some(invalid);
        compositor.plan.transitions.clear();
        assert!(
            compositor
                .frame(0, &cancelled)
                .err()
                .unwrap()
                .contains("invalid.cube")
        );
    }

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
                playback: Playback::Forward,
                color: Default::default(),
                speed: 1.0,
                lut: None,
                chroma_key: None,
                clip_id: id,
                path,
                kind: MediaKind::Image,
                start,
                source_start: 0,
                duration,
                keyframes: Vec::new(),
                transform: Transform::default(),
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
        value["tracks"][0]["clips"][0]["transform"] = serde_json::to_value(Transform {
            scale: 0.5,
            rotation: 15.0,
            ..Default::default()
        })
        .unwrap();
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
        let video = dir.path().join("out.nut");
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
                    playback: crate::timeline::Playback::Forward,
                    lut: None,
                    chroma_key: None,
                    clip_id: 1,
                    path: video,
                    color: Default::default(),
                    kind: MediaKind::Video,
                    start: 0,
                    source_start: 0,
                    duration: 2,
                    keyframes: Vec::new(),
                    transform: Transform::default(),
                }),
                VisualClip::Media(FrameSource {
                    speed: 1.0,
                    playback: crate::timeline::Playback::Forward,
                    lut: None,
                    chroma_key: None,
                    clip_id: 2,
                    path: still,
                    color: Default::default(),
                    kind: MediaKind::Image,
                    start: 2,
                    source_start: 0,
                    duration: 5,
                    keyframes: Vec::new(),
                    transform: Transform::default(),
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
        for speed in [0.5, 2.0] {
            let mut pip = plan.clone();
            pip.tracks[0].truncate(1);
            pip.transitions.clear();
            let VisualClip::Media(source) = &mut pip.tracks[0][0] else {
                unreachable!()
            };
            source.speed = speed;
            source.duration = (4.0_f64 / speed) as u64;
            source.playback = Playback::Reverse;
            source.transform = Transform {
                scale: 0.5,
                x: 0.5,
                y: 0.5,
                ..Default::default()
            };
            let duration = source.duration;
            let transform = source.transform;
            let mut compositor = Compositor::new(pip, 4, 2, (30, 1));
            for offset in 0..duration {
                let target = 3 - (offset as f64 * speed).floor() as usize;
                let [r, g, b] = colors[target];
                let image = FrameImage {
                    picture: (0, 0, 4, 2),
                    width: 4,
                    height: 2,
                    pixels: [r, g, b, 255].repeat(8),
                };
                let mut expected = [0, 0, 0, 255].repeat(8);
                draw_transformed(&mut expected, 4, 2, &image, transform);
                assert_eq!(
                    compositor
                        .frame(offset, &cancelled)
                        .unwrap()
                        .unwrap()
                        .pixels,
                    expected,
                    "reverse PiP {speed}x offset {offset}"
                );
            }
        }
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
            source.transform.scale = 0.5;
            let transformed = source.clone();
            let mut reference = VideoLayers::new((30, 1), (4, 2));
            let mut transformed_plan = sped.clone();
            transformed_plan.tracks[0].truncate(1);
            transformed_plan.transitions.clear();
            let mut scaled = Compositor::new(transformed_plan, 4, 2, (30, 1));
            for position in 0..transformed.duration {
                let image = reference
                    .layer_at(&transformed, position, &cancelled)
                    .unwrap();
                let mut expected = [0, 0, 0, 255].repeat(8);
                draw_transformed(&mut expected, 4, 2, &image, transformed.transform);
                assert_eq!(
                    scaled.frame(position, &cancelled).unwrap().unwrap().pixels,
                    expected,
                    "scaled {speed}x frame {position}"
                );
            }
            let VisualClip::Media(source) = &mut sped.tracks[0][0] else {
                unreachable!()
            };
            source.transform = Transform::default();
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
        for (playback, color) in [
            (Playback::Reverse, [240, 0, 0]),
            (Playback::Freeze, [0, 240, 0]),
        ] {
            let mut plan = plan.clone();
            let VisualClip::Media(source) = &mut plan.tracks[0][0] else {
                unreachable!()
            };
            source.playback = playback;
            source.source_start = 1;
            let mut compositor = Compositor::new(plan.clone(), 4, 2, (30, 1));
            for position in 2..5 {
                let factor = (5 - position) as u8;
                let expected = [
                    color[0] / 4 * factor,
                    color[1] / 4 * factor,
                    color[2] / 4 * factor,
                    255,
                ];
                assert_eq!(
                    &compositor
                        .frame(position, &cancelled)
                        .unwrap()
                        .unwrap()
                        .pixels[..4],
                    &expected,
                    "{playback:?} at {position}"
                );
            }
            assert_eq!(
                &Compositor::new(plan.clone(), 4, 2, (30, 1))
                    .frame(4, &cancelled)
                    .unwrap()
                    .unwrap()
                    .pixels[..4],
                &[color[0] / 4, color[1] / 4, color[2] / 4, 255]
            );
            plan.tracks[0].truncate(1);
            plan.transitions.clear();
            let VisualClip::Media(source) = &mut plan.tracks[0][0] else {
                unreachable!()
            };
            source.source_start = 0;
            source.duration = 4;
            source.color = ColorAdjust {
                brightness: 0.1,
                contrast: 0.6,
                saturation: 0.4,
            };
            let lut = dir.path().join("invert.cube");
            fs::write(&lut, "LUT_1D_SIZE 2\n1 1 1\n0 0 0\n").unwrap();
            source.lut = Some(lut);
            let mut reference = Compositor::new(plan.clone(), 4, 2, (30, 1));
            let frames: Vec<_> = (0..4)
                .map(|position| reference.frame(position, &cancelled).unwrap().unwrap())
                .collect();
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
                    let mut plan = plan.clone();
                    let transition = Transition {
                        kind,
                        direction,
                        duration_frames: 1,
                    };
                    plan.transitions.insert(1, transition.clone());
                    plan.transitions_out.insert(1, transition);
                    let mut compositor = Compositor::new(plan, 4, 2, (30, 1));
                    for (position, reference) in frames.iter().enumerate() {
                        let boundary = position == 0 || position == 3;
                        let (dx, dy) = if boundary && kind == TransitionKind::Slide {
                            match direction {
                                SlideDirection::Left => (-2, 0),
                                SlideDirection::Right => (2, 0),
                                SlideDirection::Up => (0, -1),
                                SlideDirection::Down => (0, 1),
                            }
                        } else {
                            (0, 0)
                        };
                        let frame = compositor
                            .frame(position as u64, &cancelled)
                            .unwrap()
                            .unwrap();
                        for y in 0..2i32 {
                            for x in 0..4i32 {
                                let pixel = (y * 4 + x) as usize * 4;
                                let mut expected = [0, 0, 0, 255];
                                if (0..4).contains(&(x - dx)) && (0..2).contains(&(y - dy)) {
                                    let source = ((y - dy) * 4 + x - dx) as usize * 4;
                                    expected.copy_from_slice(&reference.pixels[source..source + 4]);
                                    if boundary && kind != TransitionKind::Slide {
                                        for channel in &mut expected[..3] {
                                            *channel = (f32::from(*channel) * 0.5).round() as u8;
                                        }
                                    }
                                }
                                assert_eq!(
                                    &frame.pixels[pixel..pixel + 4],
                                    &expected,
                                    "{playback:?} {kind:?} {direction:?} at {position}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn transitions_preserve_incoming_and_outgoing_transforms() {
        ffmpeg_next::init().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let source = |clip_id, color: [u8; 4], start, transform| {
            let path = dir.path().join(format!("{clip_id}.tga"));
            let mut tga = vec![0; 18];
            tga[2] = 2;
            tga[12..14].copy_from_slice(&4u16.to_le_bytes());
            tga[14..16].copy_from_slice(&2u16.to_le_bytes());
            tga[16] = 32;
            tga[17] = 40;
            tga.extend([color[2], color[1], color[0], color[3]].repeat(8));
            fs::write(&path, tga).unwrap();
            VisualClip::Media(FrameSource {
                keyframes: Vec::new(),
                speed: 1.0,
                playback: Playback::Forward,
                clip_id,
                lut: None,
                chroma_key: None,
                path,
                kind: MediaKind::Image,
                start,
                source_start: 0,
                duration: 5,
                transform,
                color: Default::default(),
            })
        };
        let mut outgoing = source(
            1,
            [240, 0, 0, 255],
            0,
            Transform {
                crop_left: 0.5,
                ..Default::default()
            },
        );
        if let VisualClip::Media(source) = &mut outgoing {
            source.duration = 2;
        }
        let incoming = source(
            2,
            [0, 0, 240, 255],
            2,
            Transform {
                crop_right: 0.5,
                ..Default::default()
            },
        );
        let plan = VisualPlan {
            transitions_out: HashMap::new(),
            resolution: (4, 2),
            transitions: HashMap::from([(
                2,
                Transition {
                    kind: TransitionKind::Dissolve,
                    duration_frames: 3,
                    direction: Default::default(),
                },
            )]),
            tracks: vec![vec![outgoing, incoming]],
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut compositor = Compositor::new(plan, 4, 2, (30, 1));
        let frame = compositor.frame(2, &cancelled).unwrap().unwrap();
        assert_eq!(frame.picture, (0, 0, 4, 2));
        for row in frame.pixels.as_chunks::<16>().0 {
            assert_eq!(
                row,
                &[0, 0, 60, 255, 0, 0, 60, 255, 180, 0, 0, 255, 180, 0, 0, 255]
            );
        }
        let frame = compositor.frame(5, &cancelled).unwrap().unwrap();
        for row in frame.pixels.as_chunks::<16>().0 {
            assert_eq!(
                row,
                &[0, 0, 240, 255, 0, 0, 240, 255, 0, 0, 0, 255, 0, 0, 0, 255]
            );
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
                playback: crate::timeline::Playback::Forward,
                lut: None,
                chroma_key: None,
                clip_id: id,
                path,
                color: Default::default(),
                kind: MediaKind::Image,
                start,
                source_start: 0,
                duration,
                keyframes: Vec::new(),
                transform: Transform::default(),
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
                playback: crate::timeline::Playback::Forward,
                lut: None,
                chroma_key: None,
                color: Default::default(),
                clip_id: 1,
                path,
                kind: MediaKind::Image,
                start: 0,
                source_start: 0,
                duration: 300,
                keyframes: Vec::new(),
                transform: Transform::default(),
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
                    playback: crate::timeline::Playback::Forward,
                    lut: None,
                    chroma_key: None,
                    color: Default::default(),
                    clip_id: 1,
                    path,
                    kind: MediaKind::Video,
                    start: 0,
                    source_start: 0,
                    duration: frames,
                    keyframes: Vec::new(),
                    transform: Transform::default(),
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
                    if !source.transform.is_identity() {
                        draw_transformed(&mut pixels, width, height, &image, source.transform);
                        continue;
                    }
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
                playback: crate::timeline::Playback::Forward,
                lut: None,
                chroma_key: None,
                color: Default::default(),
                clip_id: id,
                path,
                kind: MediaKind::Image,
                start: 0,
                source_start: 0,
                duration: 3,
                keyframes: Vec::new(),
                transform: Transform::default(),
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
        let transformed = |transform| {
            let mut clip = source(1, opaque.clone());
            if let VisualClip::Media(source) = &mut clip {
                source.transform = transform;
            }
            (vec![vec![source(0, opaque.clone())], vec![clip]], 2)
        };
        let stacked: Vec<_> = (0..10).map(|id| vec![source(id, opaque.clone())]).collect();
        let mut titled = stacked.clone();
        titled.push(vec![title.clone()]);
        let scenarios = [
            transformed(Transform {
                scale: 0.5,
                ..Default::default()
            }),
            transformed(Transform {
                x: 0.2,
                ..Default::default()
            }),
            transformed(Transform {
                rotation: 90.0,
                ..Default::default()
            }),
            transformed(Transform {
                crop_left: 0.2,
                ..Default::default()
            }),
            transformed(Transform {
                opacity: 0.5,
                ..Default::default()
            }),
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
                playback: crate::timeline::Playback::Forward,
                lut: None,
                chroma_key: None,
                color: Default::default(),
                clip_id: id,
                path,
                kind: MediaKind::Image,
                start: 0,
                source_start: 0,
                duration: 3,
                keyframes: Vec::new(),
                transform: Transform::default(),
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
                use_proxies: false,
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
    #[test]
    fn scale_offset_rotation_crop_and_opacity() {
        let image = FrameImage {
            picture: (0, 0, 4, 4),
            width: 4,
            height: 4,
            pixels: (0..16)
                .flat_map(|i| {
                    if i < 8 {
                        [255, 0, 0, 255]
                    } else {
                        [0, 255, 0, 255]
                    }
                })
                .collect(),
        };
        let render = |t| {
            let mut dst = [0, 0, 255, 255].repeat(16);
            draw_transformed(&mut dst, 4, 4, &image, t);
            dst
        };
        let scaled = render(Transform {
            scale: 0.5,
            ..Default::default()
        });
        assert_eq!(&scaled[..4], &[0, 0, 255, 255]);
        assert_eq!(&scaled[20..24], &[255, 0, 0, 255]);
        let moved = render(Transform {
            x: 0.5,
            ..Default::default()
        });
        assert_eq!(&moved[..4], &[0, 0, 255, 255]);
        assert_eq!(&moved[8..12], &[255, 0, 0, 255]);
        let rotated = render(Transform {
            rotation: 90.0,
            ..Default::default()
        });
        assert_eq!(&rotated[..4], &[0, 255, 0, 255]);
        assert_eq!(&rotated[12..16], &[255, 0, 0, 255]);
        let cropped = render(Transform {
            crop_left: 0.5,
            ..Default::default()
        });
        assert_eq!(&cropped[..4], &[0, 0, 255, 255]);
        assert_eq!(&cropped[8..12], &[255, 0, 0, 255]);
        let faded = render(Transform {
            opacity: 0.5,
            ..Default::default()
        });
        assert_eq!(&faded[..4], &[128, 0, 127, 255]);
        assert_eq!(render(Transform::default()), image.pixels);
        let mut black = [0, 0, 0, 255].repeat(16);
        draw_transformed(
            &mut black,
            4,
            4,
            &image,
            Transform {
                scale: 0.5,
                ..Default::default()
            },
        );
        assert_eq!(&black[..4], &[0, 0, 0, 255]);
        assert_eq!(
            render(Transform {
                opacity: 0.0,
                ..Default::default()
            }),
            [0, 0, 255, 255].repeat(16)
        );
        let fitted = FrameImage {
            width: 4,
            height: 4,
            picture: (1, 0, 2, 4),
            pixels: (0..16)
                .flat_map(|i| {
                    if i % 4 == 1 || i % 4 == 2 {
                        [255, 0, 0, 255]
                    } else {
                        [0, 0, 0, 0]
                    }
                })
                .collect(),
        };
        let mut dst = [0, 0, 255, 255].repeat(16);
        draw_transformed(
            &mut dst,
            4,
            4,
            &fitted,
            Transform {
                crop_left: 0.5,
                ..Default::default()
            },
        );
        assert_eq!(&dst[4..8], &[0, 0, 255, 255]);
        assert_eq!(&dst[8..12], &[255, 0, 0, 255]);
    }
}
