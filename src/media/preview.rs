use super::MediaKind;
use super::audio::Clock;
use super::composite::{Compositor, VisualPlan};
use crate::timeline::ColorAdjust;
use ffmpeg_next as ffmpeg;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct FrameSource {
    pub lut: Option<PathBuf>,
    pub color: ColorAdjust,
    pub clip_id: u64,
    pub path: PathBuf,
    pub kind: MediaKind,
    pub start: u64,
    pub source_start: u64,
    pub duration: u64,
    pub speed: f64,
}

impl FrameSource {
    fn source_frame(&self, position: u64) -> u64 {
        self.source_start + ((position - self.start) as f64 * self.speed).floor() as u64
    }
}

#[derive(Clone)]
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
    use_proxies: bool,
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
        speed: 1.0,
        lut: None,
        color: Default::default(),
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
        let mut reader = FrameReader::open_selected(
            &source,
            target,
            key.fps,
            (160, 90),
            cancelled.clone(),
            use_proxies,
        )?;
        let mut image = Arc::unwrap_or_clone(reader.frame_at(target, &cancelled)?);
        for pixel in image.pixels.as_chunks_mut::<4>().0 {
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
    pub use_proxies: bool,
    pub sources: Vec<FrameSource>,
    pub visual: Option<VisualPlan>,
    pub end: u64,
    pub fps: (u32, u32),
    pub resolution: (u32, u32),
}

pub(crate) struct VideoLayers {
    pub(crate) use_proxies: bool,
    fps: (u32, u32),
    output: (u32, u32),
    readers: HashMap<u64, FrameReader>,
}

impl VideoLayers {
    pub(crate) fn new(fps: (u32, u32), output: (u32, u32)) -> Self {
        Self {
            use_proxies: false,
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
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel[3] = 255;
        }
        for source in sources.iter().rev() {
            if source.start > position || position - source.start >= source.duration {
                continue;
            }
            active.push(source.clip_id);
            let image = self.layer_at(source, position, cancelled)?;
            for (dst, src) in pixels
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(image.pixels.as_chunks::<4>().0)
            {
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
    ) -> Result<Arc<FrameImage>, String> {
        let target = source.source_frame(position);
        if !self.readers.contains_key(&source.clip_id) {
            self.readers.insert(
                source.clip_id,
                FrameReader::open_selected(
                    source,
                    target,
                    self.fps,
                    self.output,
                    cancelled.clone(),
                    self.use_proxies,
                )?,
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
        use_proxies,
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
    layers.use_proxies = *use_proxies;
    let mut compositor = visual
        .clone()
        .map(|plan| Compositor::new(plan, output.0, output.1, fps));
    if let Some(compositor) = &mut compositor {
        compositor.use_proxies(*use_proxies);
    }
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
    rotation: super::Rotation,
    stream_index: usize,
    time_base: ffmpeg::Rational,
    origin_pts: i64,
    fps: (u32, u32),
    output: (u32, u32),
    fallback: u64,
    current: Option<(u64, ffmpeg::frame::Video)>,
    future: Option<(u64, ffmpeg::frame::Video)>,
    eof: bool,
    still: bool,
    fitted: Option<Arc<FrameImage>>,
}

impl FrameReader {
    #[cfg(test)]
    fn open(
        source: &FrameSource,
        target: u64,
        fps: (u32, u32),
        output: (u32, u32),
        cancelled: Arc<AtomicBool>,
    ) -> Result<Self, String> {
        Self::open_selected(source, target, fps, output, cancelled, false)
    }

    fn open_selected(
        source: &FrameSource,
        target: u64,
        fps: (u32, u32),
        output: (u32, u32),
        cancelled: Arc<AtomicBool>,
        use_proxies: bool,
    ) -> Result<Self, String> {
        let proxy = (use_proxies && source.kind == MediaKind::Video)
            .then(|| super::proxy::ready(&source.path))
            .flatten();
        if let Some(path) = proxy {
            let mut selected = source.clone();
            selected.path = path.clone();
            match Self::open_path(&selected, target, fps, output, cancelled.clone(), true) {
                Ok(reader) => return Ok(reader),
                Err(_) if !cancelled.load(Ordering::Relaxed) => {
                    let _ = std::fs::remove_file(path);
                }
                Err(error) => return Err(error),
            }
        }
        Self::open_path(source, target, fps, output, cancelled, false)
    }

    fn open_path(
        source: &FrameSource,
        target: u64,
        fps: (u32, u32),
        output: (u32, u32),
        cancelled: Arc<AtomicBool>,
        proxy: bool,
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
        let origin_pts = if proxy {
            0
        } else {
            super::format_origin(&input, time_base)
        };
        let rotation = super::Rotation::from_stream(&stream);
        let mut decoder = super::hardware::video_decoder(stream.parameters())?;
        if decoder.format() == ffmpeg::format::Pixel::None || decoder.width() == 0 {
            return Err("Preview source has an unknown pixel format".into());
        }
        let square = super::display_dimensions(&decoder);
        let display = rotation.dimensions(square.0, square.1);
        let picture = super::fitted_dimensions(display.0, display.1, output.0, output.1);
        let coded = rotation.dimensions(picture.0, picture.1);
        let scaler = super::rgba_scaler(&decoder, decoder.format(), coded.0, coded.1)?;
        if source.kind == MediaKind::Video && target > 0 {
            let micros = u128::from(target) * u128::from(fps.1) * 1_000_000 / u128::from(fps.0);
            let timestamp = if proxy {
                i64::try_from(micros).ok()
            } else {
                super::seek_timestamp(&input, micros)
            };
            if let Some(timestamp) = timestamp
                && input.seek(timestamp, ..timestamp).is_ok()
            {
                decoder.flush();
            }
        }
        Ok(Self {
            input,
            decoder,
            scaler,
            rotation,
            stream_index,
            time_base,
            origin_pts,
            fps,
            output,
            fallback: 0,
            current: None,
            future: None,
            eof: false,
            still: source.kind == MediaKind::Image,
            fitted: None,
        })
    }

    fn frame_at(&mut self, target: u64, cancelled: &AtomicBool) -> Result<Arc<FrameImage>, String> {
        if self.still
            && let Some(image) = &self.fitted
        {
            return Ok(image.clone());
        }
        if self.future.is_none() && self.current.is_none() {
            self.future = self.next_decoded(cancelled)?;
        }
        while self
            .future
            .as_ref()
            .is_some_and(|(position, _)| *position <= target)
        {
            self.current = self.future.take();
            self.fitted = None;
            self.future = self.next_decoded(cancelled)?;
        }
        if let Some(image) = &self.fitted {
            return Ok(image.clone());
        }
        let frame = self
            .current
            .as_ref()
            .or(self.future.as_ref())
            .ok_or("Preview source has no decodable video frames")?;
        let downloaded = super::hardware::download(&frame.1)?;
        let frame = downloaded.as_ref().unwrap_or(&frame.1);
        if frame.format() != self.scaler.input().format {
            let output = self.scaler.output();
            self.scaler =
                super::rgba_scaler(&self.decoder, frame.format(), output.width, output.height)?;
        }
        let mut scaled = ffmpeg::frame::Video::empty();
        self.scaler
            .run(frame, &mut scaled)
            .map_err(|error| error.to_string())?;
        let scaled = self.rotation.orient(&scaled);
        let row_size = scaled.width as usize * 4;
        let stride = self.output.0 as usize * 4;
        let left = (self.output.0 - scaled.width) as usize / 2;
        let top = (self.output.1 - scaled.height) as usize / 2;
        let mut pixels = vec![0; stride * self.output.1 as usize];
        for row in 0..scaled.height as usize {
            let start = row * row_size;
            let destination = (top + row) * stride + left * 4;
            pixels[destination..destination + row_size]
                .copy_from_slice(&scaled.pixels[start..start + row_size]);
        }
        let image = Arc::new(FrameImage {
            width: self.output.0,
            height: self.output.1,
            pixels,
        });
        self.fitted = Some(image.clone());
        Ok(image)
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
                if decoded.timestamp().is_some_and(|pts| pts < self.origin_pts) {
                    continue;
                }
                let fallback = self.fallback;
                self.fallback = self.fallback.saturating_add(1);
                let position = decoded
                    .timestamp()
                    .and_then(|pts| {
                        let base = i128::from(pts) - i128::from(self.origin_pts);
                        let denominator =
                            i128::from(self.time_base.denominator()) * i128::from(self.fps.1);
                        (base >= 0 && denominator > 0)
                            .then(|| {
                                // Allow half a container tick for timestamp rounding.
                                u64::try_from(
                                    (base
                                        * i128::from(self.time_base.numerator())
                                        * i128::from(self.fps.0)
                                        + i128::from(self.time_base.numerator())
                                            * i128::from(self.fps.0)
                                            / 2)
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
    use std::process::Command;

    fn proxy_source(path: PathBuf, duration: u64) -> FrameSource {
        FrameSource {
            speed: 1.0,
            lut: None,
            color: ColorAdjust::default(),
            clip_id: 1,
            path,
            kind: MediaKind::Video,
            start: 0,
            source_start: 0,
            duration,
        }
    }

    #[test]
    fn proxy_frames_align_with_original_and_setting_selects_original() {
        let directory = tempfile::tempdir().unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        for late in [false, true] {
            let path = directory.path().join(format!("numbered-{late}.mp4"));
            let mut command = Command::new("ffmpeg");
            command.args(["-v", "error", "-y"]);
            if late {
                command.args(["-itsoffset", "0.5"]);
            }
            command.args([
                "-f",
                "lavfi",
                "-i",
                "nullsrc=s=2048x1152:r=30:d=1,geq=lum='32+N*6':cb=128:cr=128",
            ]);
            if late {
                command.args([
                    "-f",
                    "lavfi",
                    "-i",
                    "anullsrc=r=48000:cl=stereo",
                    "-t",
                    "1.5",
                    "-c:a",
                    "aac",
                    "-output_ts_offset",
                    "30",
                ]);
            }
            assert!(
                command
                    .args(["-c:v", "mpeg4", "-threads", "2", "-vsync", "0"])
                    .arg(&path)
                    .status()
                    .unwrap()
                    .success()
            );
            assert!(super::super::proxy::generate(&path, &cancelled).unwrap());
            let proxy = super::super::proxy::ready(&path).unwrap();
            let source = proxy_source(path, if late { 45 } else { 30 });
            for speed in [0.25, 0.5, 1.0, 1.5, 2.0, 4.0] {
                let sped = FrameSource {
                    speed,
                    start: 10,
                    source_start: 3,
                    duration: ((source.duration - 3) as f64 / speed).floor() as u64,
                    ..source.clone()
                };
                let mut originals = VideoLayers::new((30, 1), (160, 90));
                let mut proxies = VideoLayers::new((30, 1), (160, 90));
                proxies.use_proxies = true;
                for position in sped.start..sped.start + sped.duration {
                    let expected = originals.layer_at(&sped, position, &cancelled).unwrap();
                    let actual = proxies.layer_at(&sped, position, &cancelled).unwrap();
                    assert!(
                        expected.pixels[0].abs_diff(actual.pixels[0]) <= 3,
                        "late={late}, {speed}x at {position}"
                    );
                }
            }
            let mut sequential_original = FrameReader::open_selected(
                &source,
                0,
                (30, 1),
                (160, 90),
                cancelled.clone(),
                false,
            )
            .unwrap();
            let mut sequential_proxy =
                FrameReader::open_selected(&source, 0, (30, 1), (160, 90), cancelled.clone(), true)
                    .unwrap();
            for target in 0..source.duration {
                let original = FrameReader::open_selected(
                    &source,
                    target,
                    (30, 1),
                    (160, 90),
                    cancelled.clone(),
                    false,
                )
                .unwrap()
                .frame_at(target, &cancelled)
                .unwrap();
                let preview = FrameReader::open_selected(
                    &source,
                    target,
                    (30, 1),
                    (160, 90),
                    cancelled.clone(),
                    true,
                )
                .unwrap()
                .frame_at(target, &cancelled)
                .unwrap();
                assert!(
                    original.pixels[0].abs_diff(preview.pixels[0]) <= 3,
                    "late={late}, frame={target}: original={} proxy={}",
                    original.pixels[0],
                    preview.pixels[0]
                );
                assert_eq!(
                    original.pixels[0],
                    sequential_original
                        .frame_at(target, &cancelled)
                        .unwrap()
                        .pixels[0]
                );
                assert_eq!(
                    preview.pixels[0],
                    sequential_proxy
                        .frame_at(target, &cancelled)
                        .unwrap()
                        .pixels[0]
                );
            }
            // A deliberately different cache entry makes source selection observable.
            assert!(
                Command::new("ffmpeg")
                    .args([
                        "-v",
                        "error",
                        "-y",
                        "-f",
                        "lavfi",
                        "-i",
                        "color=white:s=1280x720:r=30:d=2",
                        "-an",
                        "-c:v",
                        "mjpeg",
                        "-threads",
                        "2",
                        "-f",
                        "nut"
                    ])
                    .arg(&proxy)
                    .status()
                    .unwrap()
                    .success()
            );
            for (enabled, white) in [(false, false), (true, true)] {
                let image = FrameReader::open_selected(
                    &source,
                    0,
                    (30, 1),
                    (160, 90),
                    cancelled.clone(),
                    enabled,
                )
                .unwrap()
                .frame_at(0, &cancelled)
                .unwrap();
                assert_eq!(image.pixels[0] > 240, white);
            }
            fs::write(&proxy, b"corrupt").unwrap();
            let image =
                FrameReader::open_selected(&source, 0, (30, 1), (160, 90), cancelled.clone(), true)
                    .unwrap()
                    .frame_at(0, &cancelled)
                    .unwrap();
            assert!(image.pixels[0] < 100);
            assert!(!proxy.exists());
        }
    }

    #[test]
    #[ignore = "release preview throughput benchmark; set OCUS_PROXY_BENCH_DIR"]
    fn proxy_decode_benchmark() {
        let directory = PathBuf::from(std::env::var("OCUS_PROXY_BENCH_DIR").unwrap());
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("4k-h264.mp4");
        if !path.exists() {
            assert!(
                Command::new("ffmpeg")
                    .args([
                        "-v",
                        "error",
                        "-y",
                        "-f",
                        "lavfi",
                        "-i",
                        "testsrc2=s=3840x2160:r=30:d=10",
                        "-c:v",
                        "libx264",
                        "-preset",
                        "fast",
                        "-crf",
                        "20",
                        "-threads",
                        "2",
                        "-pix_fmt",
                        "yuv420p"
                    ])
                    .arg(&path)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        if let Some(proxy) = super::super::proxy::ready(&path) {
            fs::remove_file(proxy).unwrap();
        }
        let started = Instant::now();
        assert!(super::super::proxy::generate(&path, &cancelled).unwrap());
        println!("Proxy generation: {:.3} s", started.elapsed().as_secs_f64());
        let source = proxy_source(path, 300);
        for enabled in [false, true] {
            let mut runs = Vec::new();
            for _ in 0..5 {
                let started = Instant::now();
                let mut reader = FrameReader::open_selected(
                    &source,
                    0,
                    (30, 1),
                    (960, 540),
                    cancelled.clone(),
                    enabled,
                )
                .unwrap();
                for target in 0..300 {
                    std::hint::black_box(reader.frame_at(target, &cancelled).unwrap());
                }
                runs.push(started.elapsed());
            }
            runs.sort();
            let seconds = runs[2].as_secs_f64();
            println!(
                "{}: {:.2} fps, {:.3} ms/frame (median of 5, 300 frames, 960x540)",
                if enabled { "Proxy" } else { "Original" },
                300.0 / seconds,
                seconds * 1000.0 / 300.0
            );
        }
    }

    #[test]
    fn speed_maps_timeline_offsets_to_source_frames() {
        for speed in [0.25, 0.5, 1.0, 1.5, 2.0, 4.0] {
            let source = super::FrameSource {
                color: Default::default(),
                lut: None,
                clip_id: 1,
                path: "video.mp4".into(),
                kind: super::MediaKind::Video,
                start: 10,
                source_start: 30,
                duration: 60,
                speed,
            };
            for offset in [0, 1, 3, 17, 59] {
                assert_eq!(
                    source.source_frame(10 + offset),
                    30 + (offset as f64 * speed).floor() as u64
                );
            }
        }
    }

    #[test]
    fn decoded_colors_match_ffmpeg() {
        ffmpeg::init().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("red.mkv");
        for (matrix, range, tags) in [
            ("bt601", "limited", &[][..]),
            (
                "bt709",
                "limited",
                &[
                    "-colorspace",
                    "bt709",
                    "-color_primaries",
                    "bt709",
                    "-color_trc",
                    "bt709",
                ][..],
            ),
            (
                "bt709",
                "full",
                &["-colorspace", "bt709", "-color_range", "pc"][..],
            ),
        ] {
            assert!(
                Command::new("ffmpeg")
                    .args([
                        "-v",
                        "error",
                        "-y",
                        "-f",
                        "lavfi",
                        "-i",
                        "color=red:size=64x64:rate=30:duration=1",
                        "-vf",
                    ])
                    .arg(format!(
                        "scale=in_color_matrix=bt601:out_color_matrix={matrix}:out_range={range}"
                    ))
                    .args(tags)
                    .args(["-c:v", "ffv1"])
                    .arg(&path)
                    .status()
                    .unwrap()
                    .success()
            );
            let reference = Command::new("ffmpeg")
                .args(["-v", "error", "-i"])
                .arg(&path)
                .arg("-vf")
                .arg(format!("scale=in_color_matrix={matrix}:in_range={range}"))
                .args(["-frames:v", "1", "-pix_fmt", "rgba", "-f", "rawvideo", "-"])
                .output()
                .unwrap();
            assert!(reference.status.success());
            let source = FrameSource {
                speed: 1.0,
                lut: None,
                color: Default::default(),
                clip_id: 1,
                path: path.clone(),
                kind: MediaKind::Video,
                start: 0,
                source_start: 0,
                duration: 30,
            };
            let cancelled = Arc::new(AtomicBool::new(false));
            let frame = FrameReader::open(&source, 0, (30, 1), (64, 64), cancelled.clone())
                .unwrap()
                .frame_at(0, &cancelled)
                .unwrap();
            let thumbnail = crate::media::thumbnail(&path).unwrap();
            let expected = &reference.stdout[..4];
            for (name, actual) in [
                ("preview", &frame.pixels[..4]),
                ("thumbnail", &thumbnail.pixels[..4]),
            ] {
                assert!(
                    actual
                        .iter()
                        .zip(expected)
                        .all(|(a, b)| a.abs_diff(*b) <= 2),
                    "{name} {matrix} {range}: got {actual:?}, FFmpeg reference {expected:?}"
                );
            }
        }
    }

    #[test]
    #[ignore = "release-mode seek benchmark; requires OCUS_SEEK_FIXTURES"]
    fn benchmark_seek_startup() {
        ffmpeg::init().unwrap();
        let root = PathBuf::from(std::env::var("OCUS_SEEK_FIXTURES").unwrap());
        for name in ["late", "zero"] {
            let source = FrameSource {
                lut: None,
                color: Default::default(),
                clip_id: 1,
                path: root.join(format!("{name}.mp4")),
                speed: 1.0,
                kind: MediaKind::Video,
                start: 0,
                source_start: 0,
                duration: 1800,
            };
            for target in [0, 300, 901, 1500, 1790] {
                let mut runs = Vec::new();
                let mut hashes = Vec::new();
                for _ in 0..3 {
                    let cancelled = Arc::new(AtomicBool::new(false));
                    let started = Instant::now();
                    let mut reader =
                        FrameReader::open(&source, target, (30, 1), (1280, 720), cancelled.clone())
                            .unwrap();
                    let image = reader.frame_at(target, &cancelled).unwrap();
                    runs.push(started.elapsed());
                    hashes.push(
                        image
                            .pixels
                            .iter()
                            .fold(0xcbf29ce484222325_u64, |hash, byte| {
                                (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
                            }),
                    );
                }
                assert!(hashes.iter().all(|hash| *hash == hashes[0]));
                runs.sort();
                println!(
                    "{name} target={target} median_ms={:.3} hash={:016x}",
                    runs[1].as_secs_f64() * 1000.0,
                    hashes[0]
                );
            }
        }
    }

    #[test]
    fn still_frames_reuse_fitted_pixels() {
        ffmpeg::init().unwrap();
        let path =
            std::env::temp_dir().join(format!("ocus-still-cache-{}.ppm", std::process::id()));
        let mut data = b"P6\n8 6\n255\n".to_vec();
        data.extend((0..8 * 6 * 3).map(|value| value as u8));
        fs::write(&path, data).unwrap();
        let source = FrameSource {
            speed: 1.0,
            lut: None,
            color: Default::default(),
            clip_id: 1,
            path: path.clone(),
            kind: MediaKind::Image,
            start: 0,
            source_start: 0,
            duration: 300,
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut reader =
            FrameReader::open(&source, 0, (30, 1), (16, 9), cancelled.clone()).unwrap();
        let first = reader.frame_at(0, &cancelled).unwrap();
        let repeated = reader.frame_at(299, &cancelled).unwrap();
        assert!(Arc::ptr_eq(&first, &repeated));
        let fresh = FrameReader::open(&source, 0, (30, 1), (16, 9), cancelled.clone())
            .unwrap()
            .frame_at(0, &cancelled)
            .unwrap();
        assert_eq!(first.pixels, fresh.pixels);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn held_video_frames_reuse_fitted_pixels() {
        ffmpeg::init().unwrap();
        let root = std::env::temp_dir().join(format!("ocus-held-frames-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        for (rate, fps) in [(5, (30, 1)), (24, (60, 1)), (30, (30, 1))] {
            let path = root.join(format!("{rate}fps.mp4"));
            assert!(
                Command::new("ffmpeg")
                    .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
                    .arg(format!(
                        "nullsrc=size=64x36:rate={rate}:duration=1,geq=lum=16+N*7:cb=128:cr=128"
                    ))
                    .args(["-pix_fmt", "yuv420p"])
                    .arg(&path)
                    .status()
                    .unwrap()
                    .success()
            );
            let frames = u64::from(fps.0 / fps.1);
            let source = FrameSource {
                speed: 1.0,
                lut: None,
                color: Default::default(),
                clip_id: 1,
                path,
                kind: MediaKind::Video,
                start: 0,
                source_start: 0,
                duration: frames,
            };
            let cancelled = Arc::new(AtomicBool::new(false));
            let mut reader =
                FrameReader::open(&source, 0, fps, (32, 18), cancelled.clone()).unwrap();
            let mut previous: Option<Arc<FrameImage>> = None;
            for position in 0..frames {
                let image = reader.frame_at(position, &cancelled).unwrap();
                let fresh = FrameReader::open(&source, 0, fps, (32, 18), cancelled.clone())
                    .unwrap()
                    .frame_at(position, &cancelled)
                    .unwrap();
                assert_eq!(image.pixels, fresh.pixels, "{rate} fps at frame {position}");
                if let Some(last) = &previous {
                    assert_eq!(
                        last.pixels == image.pixels,
                        Arc::ptr_eq(last, &image),
                        "{rate} fps reuse at frame {position}"
                    );
                }
                previous = Some(image);
            }
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn anamorphic_frames_and_thumbnails_use_display_aspect() {
        let root = std::env::temp_dir().join(format!("ocus-anamorphic-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("wide.mp4");
        assert!(
            Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-y",
                    "-f",
                    "lavfi",
                    "-i",
                    "color=white:size=720x576:rate=1",
                    "-vf",
                    "setsar=64/45",
                    "-t",
                    "1",
                    "-pix_fmt",
                    "yuv420p"
                ])
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        let source = FrameSource {
            speed: 1.0,
            lut: None,
            color: Default::default(),
            clip_id: 1,
            path: path.clone(),
            kind: MediaKind::Video,
            start: 0,
            source_start: 0,
            duration: 1,
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        let image = FrameReader::open(&source, 0, (1, 1), (1280, 720), cancelled.clone())
            .unwrap()
            .frame_at(0, &cancelled)
            .unwrap();
        assert!(
            image
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .all(|pixel| pixel[0] > 240),
            "anamorphic picture must fill 1280x720 without pillarboxing"
        );
        let thumbnail = crate::media::thumbnail(&path).unwrap();
        assert_eq!((thumbnail.width, thumbnail.height), (206, 116));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rotation_metadata_orients_frames_and_thumbnails() {
        ffmpeg::init().unwrap();
        let root = std::env::temp_dir().join(format!("ocus-rotation-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let base = root.join("base.mp4");
        assert!(
            Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-y",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc=size=320x180:rate=1",
                    "-t",
                    "1",
                    "-pix_fmt",
                    "yuv420p"
                ])
                .arg(&base)
                .status()
                .unwrap()
                .success()
        );
        for angle in [0, 90, 180, 270] {
            let path = root.join(format!("{angle}.mp4"));
            assert!(
                Command::new("ffmpeg")
                    .args(["-v", "error", "-y", "-i"])
                    .arg(&base)
                    .args(["-c", "copy", "-metadata:s:v", &format!("rotate={angle}")])
                    .arg(&path)
                    .status()
                    .unwrap()
                    .success()
            );
            let output = if angle % 180 == 0 {
                (320, 180)
            } else {
                (180, 320)
            };
            let source = FrameSource {
                speed: 1.0,
                lut: None,
                color: Default::default(),
                clip_id: 1,
                path: path.clone(),
                kind: MediaKind::Video,
                start: 0,
                source_start: 0,
                duration: 1,
            };
            let cancelled = Arc::new(AtomicBool::new(false));
            let image = FrameReader::open(&source, 0, (1, 1), output, cancelled.clone())
                .unwrap()
                .frame_at(0, &cancelled)
                .unwrap();
            let reference = Command::new("ffmpeg")
                .args(["-v", "error", "-i"])
                .arg(&path)
                .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgba", "-"])
                .output()
                .unwrap();
            assert!(reference.status.success());
            assert_eq!(image.pixels.len(), reference.stdout.len());
            let difference: usize = image
                .pixels
                .iter()
                .zip(&reference.stdout)
                .map(|(a, b)| a.abs_diff(*b) as usize)
                .sum();
            assert!(
                difference < image.pixels.len() * 2,
                "rotation {angle}: mean pixel error {}",
                difference as f64 / image.pixels.len() as f64
            );
            let thumbnail = crate::media::thumbnail(&path).unwrap();
            assert_eq!(
                thumbnail.width > thumbnail.height,
                angle % 180 == 0,
                "rotation {angle}"
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn sources_are_fitted_and_centered() {
        ffmpeg::init().unwrap();
        let path = std::env::temp_dir().join(format!("ocus-fit-{}.ppm", std::process::id()));
        for (width, height, output, picture) in [
            (720, 1280, (1920, 1080), (606, 1080)),
            (80, 60, (160, 90), (120, 90)),
            (160, 90, (80, 120), (80, 44)),
            (80, 60, (80, 60), (80, 60)),
            (80, 60, (1, 1), (1, 1)),
        ] {
            let mut data = format!("P6\n{width} {height}\n255\n").into_bytes();
            data.extend(vec![255; width * height * 3]);
            fs::write(&path, data).unwrap();
            let source = FrameSource {
                speed: 1.0,
                lut: None,
                color: Default::default(),
                clip_id: 1,
                path: path.clone(),
                kind: MediaKind::Image,
                start: 0,
                source_start: 0,
                duration: 1,
            };
            let cancelled = Arc::new(AtomicBool::new(false));
            let image = VideoLayers::new((30, 1), output)
                .frame_at(&[source], 0, &cancelled)
                .unwrap()
                .unwrap();
            assert_eq!((image.width, image.height), output);
            let left = (output.0 - picture.0) / 2;
            let top = (output.1 - picture.1) / 2;
            for y in 0..output.1 {
                for x in 0..output.0 {
                    let offset = ((y * output.0 + x) * 4) as usize;
                    let shade = if (left..left + picture.0).contains(&x)
                        && (top..top + picture.1).contains(&y)
                    {
                        255
                    } else {
                        0
                    };
                    assert_eq!(
                        &image.pixels[offset..offset + 4],
                        &[shade, shade, shade, 255],
                        "at ({x}, {y}) for {width}x{height}"
                    );
                }
            }
        }
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn slow_frame_delivery_does_not_slow_timeline_playback() {
        let plan = PreviewPlan {
            use_proxies: false,
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
    fn late_video_keeps_its_container_offset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("late.mp4");
        let generated = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-itsoffset", "0.5", "-f", "lavfi", "-i",
                "color=c=black:s=32x32:r=30:d=3.5,drawbox=color=white:t=fill:enable='between(t,1,1.099)'",
                "-f", "lavfi", "-i", "aevalsrc=if(between(t\\,1.5\\,1.6)\\,0.8*sin(2*PI*1000*t)\\,0):s=48000:d=4",
                "-c:v", "mpeg4", "-vsync", "0", "-c:a", "aac", "-output_ts_offset", "30"])
            .arg(&path).status();
        let status = match generated {
            Ok(status) => status,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => panic!("{error}"),
        };
        assert!(status.success());
        ffmpeg::init().unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let source = FrameSource {
            speed: 1.0,
            lut: None,
            color: Default::default(),
            clip_id: 1,
            path,
            kind: MediaKind::Video,
            start: 0,
            source_start: 0,
            duration: 120,
        };
        let mut reader =
            FrameReader::open(&source, 0, (30, 1), (32, 32), cancelled.clone()).unwrap();
        let flashes: Vec<_> = (0..120)
            .filter(|&position| reader.frame_at(position, &cancelled).unwrap().pixels[0] > 200)
            .collect();
        assert_eq!(flashes, [45, 46, 47]);
        for (target, white) in [(30, false), (45, true)] {
            let mut reader =
                FrameReader::open(&source, target, (30, 1), (32, 32), cancelled.clone()).unwrap();
            assert_eq!(
                reader.frame_at(target, &cancelled).unwrap().pixels[0] > 200,
                white
            );
        }
    }

    #[test]
    fn millisecond_timestamps_do_not_skip_source_frames_at_changed_speeds() {
        let directory = tempfile::tempdir().unwrap();
        let original = directory.path().join("original.y4m");
        let quantized = directory.path().join("quantized.mkv");
        let mut bytes = b"YUV4MPEG2 W2 H2 F30:1 Ip A1:1 C420jpeg\n".to_vec();
        for shade in (32..224).step_by(8) {
            bytes.extend(b"FRAME\n");
            bytes.extend([shade, shade, shade, shade, 128, 128]);
        }
        fs::write(&original, bytes).unwrap();
        assert!(
            Command::new("ffmpeg")
                .args(["-v", "error", "-i"])
                .arg(&original)
                .args(["-c:v", "ffv1"])
                .arg(&quantized)
                .status()
                .unwrap()
                .success()
        );
        let cancelled = Arc::new(AtomicBool::new(false));
        for speed in [0.25, 0.5, 1.0, 1.5, 2.0, 4.0] {
            let source = FrameSource {
                color: Default::default(),
                lut: None,
                clip_id: 1,
                path: quantized.clone(),
                kind: MediaKind::Video,
                start: 0,
                source_start: 0,
                duration: (24.0_f64 / speed) as u64,
                speed,
            };
            let mut reference = FrameReader::open(
                &FrameSource {
                    path: original.clone(),
                    ..source.clone()
                },
                0,
                (30, 1),
                (2, 2),
                cancelled.clone(),
            )
            .unwrap();
            let mut layers = VideoLayers::new((30, 1), (2, 2));
            for position in 0..source.duration {
                let expected = reference
                    .frame_at(source.source_frame(position), &cancelled)
                    .unwrap();
                let actual = layers.layer_at(&source, position, &cancelled).unwrap();
                assert_eq!(actual.pixels, expected.pixels, "{speed}x at {position}");
            }
        }
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
            speed: 1.0,
            lut: None,
            color: Default::default(),
            clip_id: 1,
            path: path.clone(),
            kind: MediaKind::Video,
            start: 0,
            source_start: 0,
            duration: 6,
        }];
        let plan = PreviewPlan {
            use_proxies: false,
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
            false,
        )
        .unwrap();
        assert_eq!(storyboard_frames.len(), 3);
        let center = (45 * 160 + 80) * 4;
        assert!(storyboard_frames[0].pixels[center] < 120);
        assert!(storyboard_frames[2].pixels[center] > 140);
        let cut = PreviewPlan {
            use_proxies: false,
            sources: vec![FrameSource {
                speed: 1.0,
                lut: None,
                color: Default::default(),
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
