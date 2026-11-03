use super::MediaKind;
use ffmpeg_next as ffmpeg;
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
    pub end: u64,
    pub fps: (u32, u32),
    pub resolution: (u32, u32),
}

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
    mut emit: impl FnMut(PreviewEvent),
) -> Result<(), String> {
    ffmpeg::init().map_err(|error| error.to_string())?;
    let PreviewPlan {
        sources,
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
    let mut reader: Option<(u64, FrameReader)> = None;
    let mut position = start;
    while position < *end && !cancelled.load(Ordering::Relaxed) {
        if playing {
            let nanos = u128::from(position - start) * u128::from(fps.1) * 1_000_000_000
                / u128::from(fps.0);
            let deadline = Duration::from_nanos(nanos.min(u128::from(u64::MAX)) as u64);
            while started.elapsed() < deadline {
                if cancelled.load(Ordering::Relaxed) {
                    return Ok(());
                }
                std::thread::sleep((deadline - started.elapsed()).min(Duration::from_millis(10)));
            }
        }
        let source = sources
            .iter()
            .find(|source| position >= source.start && position - source.start < source.duration);
        let image = if let Some(source) = source {
            let target = source.source_start + position - source.start;
            if reader.as_ref().is_none_or(|(id, _)| *id != source.clip_id) {
                reader = Some((
                    source.clip_id,
                    FrameReader::open(source, target, fps, output, cancelled.clone())?,
                ));
            }
            Some(reader.as_mut().unwrap().1.frame_at(target, &cancelled)?)
        } else {
            reader = None;
            None
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
            end: 6,
            fps: (30, 1),
            resolution: (2, 2),
        };
        let mut result = Vec::new();
        decode_sequence(&plan, 4, false, Arc::new(AtomicBool::new(false)), |event| {
            if let PreviewEvent::Frame(position, Some(image)) = event {
                result.push((position, image.pixels[0]));
            }
        })
        .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, 4);
        assert!(result[0].1 > 140, "{result:?}");
        result.clear();
        decode_sequence(&plan, 0, true, Arc::new(AtomicBool::new(false)), |event| {
            if let PreviewEvent::Frame(position, Some(image)) = event {
                result.push((position, image.pixels[0]));
            }
        })
        .unwrap();
        assert_eq!(result.len(), 6);
        assert!(result[0].1 < 120 && result[5].1 > 140, "{result:?}");
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
        decode_sequence(&plan, 0, true, cancelled, |_| {
            panic!("cancelled preview emitted a frame")
        })
        .unwrap();
        fs::remove_file(path).unwrap();
    }
}
