use super::MediaKind;
use super::audio::Clock;
use super::composite::{Compositor, VisualPlan};
use ffmpeg_next as ffmpeg;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct FrameSource {
    pub clip_id: u64,
    pub path: PathBuf,
    pub kind: MediaKind,
    pub start: u64,
    pub source_start: u64,
    pub duration: u64,
}

pub struct FrameImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
pub struct StoryboardKey {
    pub path: PathBuf,
    pub kind: MediaKind,
    pub source_start: u64,
    pub duration: u64,
    pub fps: (u32, u32),
}

pub fn storyboard(
    key: &StoryboardKey,
    cancelled: Arc<AtomicBool>,
) -> Result<Vec<FrameImage>, String> {
    ffmpeg::init().map_err(|error| error.to_string())?;
    if key.duration == 0 || key.fps.0 == 0 || key.fps.1 == 0 || key.kind == MediaKind::Audio {
        return Err("Invalid storyboard source".into());
    }
    let offsets = if key.kind == MediaKind::Image {
        vec![0]
    } else {
        vec![0, key.duration / 2, key.duration - 1]
    };
    let source = FrameSource {
        clip_id: 0,
        path: key.path.clone(),
        kind: key.kind,
        start: 0,
        source_start: key.source_start,
        duration: key.duration,
    };
    let mut frames = Vec::new();
    for offset in offsets {
        if cancelled.load(Ordering::Relaxed) {
            break;
        }
        let target = key
            .source_start
            .checked_add(offset)
            .ok_or("Storyboard frame range overflows")?;
        let mut reader = FrameReader::open(&source, target, key.fps, (160, 90), cancelled.clone())?;
        let mut image = reader.frame_at(target, &cancelled)?;
        for pixel in image.pixels.chunks_exact_mut(4) {
            for channel in 0..3 {
                pixel[channel] =
                    ((u16::from(pixel[channel]) * u16::from(pixel[3]) + 127) / 255) as u8;
            }
        }
        frames.push(image);
    }
    Ok(frames)
}

pub struct PreviewPlan {
    pub sources: Vec<FrameSource>,
    pub visual: Option<VisualPlan>,
    pub end: u64,
    pub fps: (u32, u32),
    pub resolution: (u32, u32),
}

pub(crate) struct VideoLayers {
    fps: (u32, u32),
    output: (u32, u32),
    readers: HashMap<u64, FrameReader>,
}

impl VideoLayers {
    pub(crate) fn new(fps: (u32, u32), output: (u32, u32)) -> Self {
        Self {
            fps,
            output,
            readers: HashMap::new(),
        }
    }

    pub(crate) fn frame_at(
        &mut self,
        sources: &[FrameSource],
        position: u64,
        cancelled: &Arc<AtomicBool>,
    ) -> Result<Option<FrameImage>, String> {
        let mut active = Vec::new();
        let mut pixels = vec![0; self.output.0 as usize * self.output.1 as usize * 4];
        for pixel in pixels.chunks_exact_mut(4) {
            pixel[3] = 255;
        }
        for source in sources.iter().rev() {
            if source.start > position || position - source.start >= source.duration {
                continue;
            }
            active.push(source.clip_id);
            let image = self.layer_at(source, position, cancelled)?;
            for (dst, src) in pixels.chunks_exact_mut(4).zip(image.pixels.chunks_exact(4)) {
                blend(dst, src);
            }
        }
        self.retain(&active);
        Ok((!active.is_empty()).then_some(FrameImage {
            width: self.output.0,
            height: self.output.1,
            pixels,
        }))
    }

    pub(crate) fn layer_at(
        &mut self,
        source: &FrameSource,
        position: u64,
        cancelled: &Arc<AtomicBool>,
    ) -> Result<FrameImage, String> {
        let target = source.source_start + position - source.start;
        if !self.readers.contains_key(&source.clip_id) {
            self.readers.insert(
                source.clip_id,
                FrameReader::open(source, target, self.fps, self.output, cancelled.clone())?,
            );
        }
        self.readers
            .get_mut(&source.clip_id)
            .unwrap()
            .frame_at(target, cancelled)
    }

    pub(crate) fn retain(&mut self, active: &[u64]) {
        self.readers.retain(|id, _| active.contains(id));
    }
}

pub(crate) fn blend(dst: &mut [u8], src: &[u8]) {
    let alpha = u32::from(src[3]);
    for channel in 0..3 {
        dst[channel] =
            ((u32::from(src[channel]) * alpha + u32::from(dst[channel]) * (255 - alpha) + 127)
                / 255) as u8;
    }
}

/// How far video may run ahead of a stalled audio clock before taking over.
const MAX_AUDIO_LAG: Duration = Duration::from_millis(250);

pub enum PreviewEvent {
    Frame(u64, Option<FrameImage>),
    Finished,
    Failed(String),
}

pub fn decode_sequence(
    plan: &PreviewPlan,
    start: u64,
    playing: bool,
    cancelled: Arc<AtomicBool>,
    clock: Option<Clock>,
    mut emit: impl FnMut(PreviewEvent),
) -> Result<(), String> {
    ffmpeg::init().map_err(|error| error.to_string())?;
    let PreviewPlan {
        sources,
        visual,
        end,
        fps,
        resolution,
    } = plan;
    let fps = *fps;
    let resolution = *resolution;
    if fps.0 == 0 || fps.1 == 0 || resolution.0 == 0 || resolution.1 == 0 {
        return Err("Invalid preview settings".into());
    }
    let scale = (960.0 / resolution.0 as f32)
        .min(540.0 / resolution.1 as f32)
        .min(1.0);
    let output = (
        (resolution.0 as f32 * scale).round().max(1.0) as u32,
        (resolution.1 as f32 * scale).round().max(1.0) as u32,
    );
    let started = Instant::now();
    let mut layers = VideoLayers::new(fps, output);
    let mut compositor = visual
        .clone()
        .map(|plan| Compositor::new(plan, output.0, output.1, fps));
    let mut position = start;
    let elapsed = || {
        let wall = started.elapsed();
        match &clock {
            Some(clock) => wall.saturating_sub(MAX_AUDIO_LAG).max(clock.elapsed()),
            None => wall,
        }
    };
    while position < *end && !cancelled.load(Ordering::Relaxed) {
        if playing {
            let due = elapsed().as_nanos().saturating_mul(u128::from(fps.0))
                / (u128::from(fps.1) * 1_000_000_000);
            let due = start
                .saturating_add(u64::try_from(due).unwrap_or(u64::MAX))
                .min(end.saturating_sub(1));
            position = position.max(due);
            let nanos = u128::from(position - start) * u128::from(fps.1) * 1_000_000_000
                / u128::from(fps.0);
            let deadline = Duration::from_nanos(nanos.min(u128::from(u64::MAX)) as u64);
            loop {
                // Audio output is the master clock, but a stalled device must
                // not freeze the picture for more than MAX_AUDIO_LAG.
                let current = elapsed();
                if current >= deadline {
                    break;
                }
                if cancelled.load(Ordering::Relaxed) {
                    return Ok(());
                }
                std::thread::sleep((deadline - current).min(Duration::from_millis(10)));
            }
        }
        let image = match &mut compositor {
            Some(compositor) => compositor.frame(position, &cancelled)?,
            None => layers.frame_at(sources, position, &cancelled)?,
        };
        if cancelled.load(Ordering::Relaxed) {
            break;
        }
        emit(PreviewEvent::Frame(position, image));
        if !playing {
            break;
        }
        position += 1;
    }
    if playing && !cancelled.load(Ordering::Relaxed) {
        emit(PreviewEvent::Finished);
    }
    Ok(())
}

struct FrameReader {
    input: ffmpeg::format::context::Input,
    decoder: ffmpeg::decoder::Video,
    scaler: ffmpeg::software::scaling::context::Context,
    stream_index: usize,
    time_base: ffmpeg::Rational,
    first_pts: i64,
    fps: (u32, u32),
    output: (u32, u32),
    fallback: u64,
    current: Option<(u64, ffmpeg::frame::Video)>,
    future: Option<(u64, ffmpeg::frame::Video)>,
    eof: bool,
}

impl FrameReader {
    fn open(
        source: &FrameSource,
        target: u64,
        fps: (u32, u32),
        output: (u32, u32),
        cancelled: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        let interrupted = cancelled.clone();
        let mut input = ffmpeg::format::input_with_interrupt(&source.path, move || {
            interrupted.load(Ordering::Relaxed)
        })
        .map_err(|error| format!("Cannot open preview source: {error}"))?;
        // An interrupted probe can leave codec parameters incomplete, and
        // swscale aborts the process on an unknown pixel format.
        if cancelled.load(Ordering::Relaxed) {
            return Err("Preview cancelled".into());
        }
        let stream = input
            .streams()
            .best(ffmpeg::media::Type::Video)
            .ok_or("Preview source has no video")?;
        let stream_index = stream.index();
        let time_base = stream.time_base();
        let first_pts = stream.start_time();
        let mut decoder = ffmpeg::codec::context::Context::from_parameters(stream.parameters())
            .and_then(|context| context.decoder().video())
            .map_err(|error| error.to_string())?;
        if decoder.format() == ffmpeg::format::Pixel::None || decoder.width() == 0 {
            return Err("Preview source has an unknown pixel format".into());
        }
        let scaler = ffmpeg::software::scaling::context::Context::get(
            decoder.format(),
            decoder.width(),
            decoder.height(),
            ffmpeg::format::Pixel::RGBA,
            output.0,
            output.1,
            ffmpeg::software::scaling::flag::Flags::BILINEAR,
        )
        .map_err(|error| error.to_string())?;
        if source.kind == MediaKind::Video && target > 0 {
            let micros = u128::from(target) * u128::from(fps.1) * 1_000_000 / u128::from(fps.0);
            if let Ok(timestamp) = i64::try_from(micros)
                && input.seek(timestamp, ..timestamp).is_ok()
            {
                decoder.flush();
            }
        }
        Ok(Self {
            input,
            decoder,
            scaler,
            stream_index,
            time_base,
            first_pts,
            fps,
            output,
            fallback: 0,
            current: None,
            future: None,
            eof: false,
        })
    }

    fn frame_at(&mut self, target: u64, cancelled: &AtomicBool) -> Result<FrameImage, String> {
        if self.future.is_none() && self.current.is_none() {
            self.future = self.next_decoded(cancelled)?;
        }
        while self
            .future
            .as_ref()
            .is_some_and(|(position, _)| *position <= target)
        {
            self.current = self.future.take();
            self.future = self.next_decoded(cancelled)?;
        }
        let frame = self
            .current
            .as_ref()
            .or(self.future.as_ref())
            .ok_or("Preview source has no decodable video frames")?;
        let mut scaled = ffmpeg::frame::Video::empty();
        self.scaler
            .run(&frame.1, &mut scaled)
            .map_err(|error| error.to_string())?;
        let row_size = self.output.0 as usize * 4;
        let mut pixels = Vec::with_capacity(row_size * self.output.1 as usize);
        for row in 0..self.output.1 as usize {
            let start = row * scaled.stride(0);
            pixels.extend_from_slice(&scaled.data(0)[start..start + row_size]);
        }
        Ok(FrameImage {
            width: self.output.0,
            height: self.output.1,
            pixels,
        })
    }

    fn next_decoded(
        &mut self,
        cancelled: &AtomicBool,
    ) -> Result<Option<(u64, ffmpeg::frame::Video)>, String> {
        loop {
            if cancelled.load(Ordering::Relaxed) {
                return Ok(None);
            }
            let mut decoded = ffmpeg::frame::Video::empty();
            if self.decoder.receive_frame(&mut decoded).is_ok() {
                let fallback = self.fallback;
                self.fallback = self.fallback.saturating_add(1);
                let position = decoded
                    .timestamp()
                    .and_then(|pts| {
                        let start = if self.first_pts == i64::MIN {
                            0
                        } else {
                            self.first_pts
                        };
                        let base = i128::from(pts) - i128::from(start);
                        let denominator =
                            i128::from(self.time_base.denominator()) * i128::from(self.fps.1);
                        (base >= 0 && denominator > 0)
                            .then(|| {
                                u64::try_from(
                                    base * i128::from(self.time_base.numerator())
                                        * i128::from(self.fps.0)
                                        / denominator,
                                )
                                .ok()
                            })
                            .flatten()
                    })
                    .unwrap_or(fallback);
                return Ok(Some((position, decoded)));
            }
            if self.eof {
                return Ok(None);
            }
            let packet = self.input.packets().find_map(|(stream, packet)| {
                (stream.index() == self.stream_index).then_some(packet)
            });
            if let Some(packet) = packet {
                self.decoder
                    .send_packet(&packet)
                    .map_err(|error| error.to_string())?;
            } else {
                self.decoder.send_eof().map_err(|error| error.to_string())?;
                self.eof = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn slow_frame_delivery_does_not_slow_timeline_playback() {
        let plan = PreviewPlan {
            sources: Vec::new(),
            visual: None,
            end: 30,
            fps: (30, 1),
            resolution: (2, 2),
        };
        let started = Instant::now();
        let mut frames = Vec::new();
        decode_sequence(
            &plan,
            0,
            true,
            Arc::new(AtomicBool::new(false)),
            None,
            |event| {
                if let PreviewEvent::Frame(position, _) = event {
                    frames.push(position);
                    std::thread::sleep(Duration::from_millis(80));
                }
            },
        )
        .unwrap();
        assert_eq!(frames.first(), Some(&0));
        assert_eq!(frames.last(), Some(&29));
        assert!(started.elapsed() < Duration::from_secs(2), "{frames:?}");
    }

    #[test]
    fn seek_and_play_use_source_timestamps() {
        let path = std::env::temp_dir().join(format!("ocus-preview-{}.y4m", std::process::id()));
        let mut source = b"YUV4MPEG2 W2 H2 F30:1 Ip A1:1 C420jpeg\n".to_vec();
        for shade in [80, 80, 80, 180, 180, 180] {
            source.extend(b"FRAME\n");
            source.extend([shade, shade, shade, shade, 128, 128]);
        }
        fs::write(&path, source).unwrap();
        let clips = [FrameSource {
            clip_id: 1,
            path: path.clone(),
            kind: MediaKind::Video,
            start: 0,
            source_start: 0,
            duration: 6,
        }];
        let plan = PreviewPlan {
            sources: clips.to_vec(),
            visual: None,
            end: 6,
            fps: (30, 1),
            resolution: (2, 2),
        };
        let mut result = Vec::new();
        decode_sequence(
            &plan,
            4,
            false,
            Arc::new(AtomicBool::new(false)),
            None,
            |event| {
                if let PreviewEvent::Frame(position, Some(image)) = event {
                    result.push((position, image.pixels[0]));
                }
            },
        )
        .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, 4);
        assert!(result[0].1 > 140, "{result:?}");
        result.clear();
        decode_sequence(
            &plan,
            0,
            true,
            Arc::new(AtomicBool::new(false)),
            None,
            |event| {
                if let PreviewEvent::Frame(position, Some(image)) = event {
                    result.push((position, image.pixels[0]));
                }
            },
        )
        .unwrap();
        assert!(result.first().is_some_and(|frame| frame.1 < 120));
        assert!(
            result
                .last()
                .is_some_and(|frame| frame.0 == 5 && frame.1 > 140),
            "{result:?}"
        );
        let mut late_frames = Vec::new();
        decode_sequence(
            &plan,
            0,
            true,
            Arc::new(AtomicBool::new(false)),
            None,
            |event| {
                if let PreviewEvent::Frame(position, Some(image)) = event {
                    late_frames.push((position, image.pixels[0]));
                    if position == 0 {
                        std::thread::sleep(Duration::from_millis(80));
                    }
                }
            },
        )
        .unwrap();
        assert!(late_frames.len() < 6, "{late_frames:?}");
        assert!(
            late_frames
                .last()
                .is_some_and(|frame| frame.0 == 5 && frame.1 > 140)
        );
        let storyboard_frames = storyboard(
            &StoryboardKey {
                path: path.clone(),
                kind: MediaKind::Video,
                source_start: 2,
                duration: 3,
                fps: (30, 1),
            },
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        assert_eq!(storyboard_frames.len(), 3);
        assert!(storyboard_frames[0].pixels[0] < 120);
        assert!(storyboard_frames[2].pixels[0] > 140);
        let cut = PreviewPlan {
            sources: vec![FrameSource {
                clip_id: 2,
                path: path.clone(),
                kind: MediaKind::Video,
                start: 3,
                source_start: 2,
                duration: 2,
            }],
            visual: None,
            end: 5,
            fps: (30, 1),
            resolution: (2, 2),
        };
        let mut frames = Vec::new();
        for position in 2..5 {
            decode_sequence(
                &cut,
                position,
                false,
                Arc::new(AtomicBool::new(false)),
                None,
                |event| {
                    if let PreviewEvent::Frame(_, image) = event {
                        frames.push(image.map(|image| image.pixels[0]));
                    }
                },
            )
            .unwrap();
        }
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0], None);
        assert!(frames[1].is_some_and(|shade| shade < 120));
        assert!(frames[2].is_some_and(|shade| shade > 140));
        let cancelled = Arc::new(AtomicBool::new(true));
        assert!(FrameReader::open(&clips[0], 0, (30, 1), (2, 2), cancelled.clone()).is_err());
        decode_sequence(&plan, 0, true, cancelled, None, |_| {
            panic!("cancelled preview emitted a frame")
        })
        .unwrap();
        fs::remove_file(path).unwrap();
    }
}
