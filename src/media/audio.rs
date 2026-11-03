use ffmpeg_next as ffmpeg;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// The sample format every clip is converted to before mixing.
pub const RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;

/// Samples held ahead of playback; caps decode memory and restart latency.
const CAPACITY: usize = RATE as usize / 4 * CHANNELS;
const CHUNK: usize = 2_048;

#[derive(Clone)]
pub struct AudioSource {
    pub path: PathBuf,
    pub start: u64,
    pub source_start: u64,
    pub duration: u64,
    pub volume: f32,
}

pub struct AudioPlan {
    pub sources: Vec<AudioSource>,
    pub end: u64,
    pub fps: (u32, u32),
}

/// Stereo samples the output device has consumed. Video pacing follows this
/// instead of the wall clock so picture and sound stay aligned.
#[derive(Clone)]
pub struct Clock {
    played: Arc<AtomicU64>,
}

impl Clock {
    pub fn elapsed(&self) -> Duration {
        let played = self.played.load(Ordering::Relaxed) as f64;
        Duration::from_secs_f64(played / (f64::from(RATE) * CHANNELS as f64))
    }
}

/// Bounded stereo buffer the decode worker fills and the device drains.
pub struct AudioQueue {
    samples: Mutex<VecDeque<f32>>,
    space: Condvar,
    played: Arc<AtomicU64>,
    done: AtomicBool,
}

impl AudioQueue {
    pub fn new() -> Self {
        Self {
            samples: Mutex::new(VecDeque::new()),
            space: Condvar::new(),
            played: Arc::new(AtomicU64::new(0)),
            done: AtomicBool::new(false),
        }
    }

    /// Blocks while the buffer is full; returns false once cancelled.
    pub fn push(&self, chunk: &[f32], cancelled: &AtomicBool) -> bool {
        let mut samples = self.samples.lock().unwrap();
        while samples.len() + chunk.len() > CAPACITY {
            if cancelled.load(Ordering::Relaxed) || self.done.load(Ordering::Relaxed) {
                return false;
            }
            let (guard, _) = self
                .space
                .wait_timeout(samples, Duration::from_millis(20))
                .unwrap();
            samples = guard;
        }
        samples.extend(chunk.iter().copied());
        true
    }

    /// Takes a decoded sample; real samples count toward the clock.
    pub fn pop(&self) -> Option<f32> {
        let value = self.samples.lock().unwrap().pop_front();
        if value.is_some() {
            self.note_played();
        }
        value
    }

    /// Advances the clock by one output sample.
    pub fn note_played(&self) {
        self.played.fetch_add(1, Ordering::Relaxed);
    }

    pub fn finish(&self) {
        self.done.store(true, Ordering::Relaxed);
        self.space.notify_all();
    }

    pub fn is_done(&self) -> bool {
        self.done.load(Ordering::Relaxed)
    }

    pub fn clock(&self) -> Clock {
        Clock {
            played: self.played.clone(),
        }
    }
}

impl Default for AudioQueue {
    fn default() -> Self {
        Self::new()
    }
}

/// Decodes every source overlapping `start`, mixes them to 48 kHz stereo and
/// feeds the queue a little ahead of playback until the sequence ends. Gaps
/// and exhausted sources contribute silence so output stays timeline-aligned.
pub fn feed(
    plan: &AudioPlan,
    start: u64,
    queue: Arc<AudioQueue>,
    cancelled: Arc<AtomicBool>,
) -> Result<(), String> {
    ffmpeg::init().map_err(|error| error.to_string())?;
    let (numerator, denominator) = plan.fps;
    if numerator == 0 || denominator == 0 || start >= plan.end {
        return Ok(());
    }
    let to_samples = |frame: u64| -> u64 {
        u64::try_from(
            u128::from(frame) * u128::from(RATE) * u128::from(denominator) / u128::from(numerator),
        )
        .unwrap_or(u64::MAX)
    };
    struct Slot {
        reader: AudioReader,
        begin: u64,
        end: u64,
        volume: f32,
    }
    let mut slots = Vec::new();
    let mut first_error = None;
    for source in &plan.sources {
        let from = source.start.max(start);
        let until = source.start.saturating_add(source.duration).min(plan.end);
        if from >= until {
            continue;
        }
        let offset = source.source_start + from - source.start;
        match AudioReader::open(&source.path, offset, plan.fps, cancelled.clone()) {
            Ok(Some(reader)) => slots.push(Slot {
                reader,
                begin: to_samples(from),
                end: to_samples(until),
                volume: source.volume,
            }),
            Ok(None) => {}
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
    }
    let end = to_samples(plan.end);
    let mut position = to_samples(start);
    while position < end && !cancelled.load(Ordering::Relaxed) {
        let count = (end - position).min(CHUNK as u64) as usize;
        let mut mix = vec![0.0; count * CHANNELS];
        for slot in &mut slots {
            let from = position.max(slot.begin);
            let until = (position + count as u64).min(slot.end);
            if from >= until {
                continue;
            }
            let mut sound = Vec::with_capacity((until - from) as usize * CHANNELS);
            match slot
                .reader
                .read(&mut sound, (until - from) as usize, &cancelled)
            {
                Ok(_) => {
                    for (index, frame) in sound.chunks_exact(CHANNELS).enumerate() {
                        let offset = (from - position) as usize * CHANNELS + index * CHANNELS;
                        mix[offset] += frame[0] * slot.volume;
                        mix[offset + 1] += frame[1] * slot.volume;
                    }
                }
                Err(error) => {
                    first_error.get_or_insert(error);
                    slot.end = from;
                }
            }
        }
        for sample in &mut mix {
            *sample = sample.clamp(-1.0, 1.0);
        }
        if !queue.push(&mix, &cancelled) {
            break;
        }
        position += count as u64;
    }
    queue.finish();
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// One source's audio stream, resampled to the output format and emitting
/// stereo samples in order for the part of the clip being played.
struct AudioReader {
    input: ffmpeg::format::context::Input,
    decoder: ffmpeg::decoder::Audio,
    stream_index: usize,
    time_base: ffmpeg::Rational,
    first_pts: i64,
    skip_until: u64,
    pending: VecDeque<f32>,
    position: f64,
    ratio: f64,
    eof: bool,
    exhausted: bool,
}

impl AudioReader {
    fn open(
        path: &Path,
        offset: u64,
        fps: (u32, u32),
        cancelled: Arc<AtomicBool>,
    ) -> Result<Option<Self>, String> {
        let interrupted = cancelled.clone();
        let mut input =
            ffmpeg::format::input_with_interrupt(path, move || interrupted.load(Ordering::Relaxed))
                .map_err(|error| format!("Cannot open audio source: {error}"))?;
        // See FrameReader::open: an interrupted probe leaves unusable state.
        if cancelled.load(Ordering::Relaxed) {
            return Err("Preview cancelled".into());
        }
        let Some(stream) = input.streams().best(ffmpeg::media::Type::Audio) else {
            return Ok(None);
        };
        let stream_index = stream.index();
        let time_base = stream.time_base();
        let first_pts = stream.start_time();
        let mut decoder = ffmpeg::codec::context::Context::from_parameters(stream.parameters())
            .and_then(|context| context.decoder().audio())
            .map_err(|error| error.to_string())?;
        let rate = decoder.rate();
        if rate == 0 || decoder.format() == ffmpeg::format::Sample::None {
            return Err("Audio source has an unsupported format".into());
        }
        if offset > 0 {
            let micros = u128::from(offset) * u128::from(fps.1) * 1_000_000 / u128::from(fps.0);
            if let Ok(timestamp) = i64::try_from(micros)
                && input.seek(timestamp, ..timestamp).is_ok()
            {
                decoder.flush();
            }
        }
        Ok(Some(Self {
            input,
            decoder,
            stream_index,
            time_base,
            first_pts,
            skip_until: u64::try_from(
                u128::from(offset) * u128::from(fps.1) * u128::from(rate) / u128::from(fps.0),
            )
            .unwrap_or(u64::MAX),
            pending: VecDeque::new(),
            position: 0.0,
            ratio: f64::from(rate) / f64::from(RATE),
            eof: false,
            exhausted: false,
        }))
    }

    /// Appends up to `frames` stereo samples, fewer at the end of the stream.
    fn read(
        &mut self,
        out: &mut Vec<f32>,
        frames: usize,
        cancelled: &AtomicBool,
    ) -> Result<usize, String> {
        let mut read = 0;
        while read < frames {
            let index = self.position as usize;
            let available = self.pending.len() / CHANNELS;
            if index < available && (index + 1 < available || self.exhausted) {
                let fraction = (self.position - index as f64) as f32;
                let (left, right) = if index + 1 < available {
                    (
                        self.pending[index * CHANNELS]
                            + (self.pending[index * CHANNELS + 2] - self.pending[index * CHANNELS])
                                * fraction,
                        self.pending[index * CHANNELS + 1]
                            + (self.pending[index * CHANNELS + 3]
                                - self.pending[index * CHANNELS + 1])
                                * fraction,
                    )
                } else {
                    (
                        self.pending[index * CHANNELS],
                        self.pending[index * CHANNELS + 1],
                    )
                };
                out.push(left);
                out.push(right);
                read += 1;
                self.position += self.ratio;
                let used = self.position as usize;
                if used > 0 {
                    self.pending.drain(..used * CHANNELS);
                    self.position -= used as f64;
                }
            } else if !self.pull(cancelled)? {
                break;
            }
        }
        Ok(read)
    }

    /// Decodes the next input frame into `pending`; false once drained.
    fn pull(&mut self, cancelled: &AtomicBool) -> Result<bool, String> {
        loop {
            if cancelled.load(Ordering::Relaxed) || self.exhausted {
                return Ok(false);
            }
            let mut decoded = ffmpeg::frame::Audio::empty();
            if self.decoder.receive_frame(&mut decoded).is_ok() {
                // Some decoders leave the frame layout unset; the codec
                // context knows it, and packed buffers keep one interleaved
                // element per channel as a last resort.
                let mut channels = decoded.channels();
                if channels == 0 {
                    channels = self.decoder.channels();
                }
                if channels == 0 && decoded.is_packed() && decoded.planes() > 0 {
                    let width = decoded.format().bytes().max(1) * decoded.samples().max(1);
                    channels = u16::try_from(decoded.data(0).len() / width)
                        .unwrap_or(1)
                        .max(1);
                }
                let mut stereo = stereo_samples(&decoded, channels);
                if let Some(start) = decoded.timestamp().map(|pts| self.sample_index(pts)) {
                    let frames = stereo.len() / CHANNELS;
                    let skip = self.skip_until.saturating_sub(start).min(frames as u64) as usize;
                    stereo.drain(..skip * CHANNELS);
                }
                self.pending.extend(stereo);
                return Ok(true);
            }
            if self.eof {
                self.exhausted = true;
                return Ok(false);
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

    /// Source-sample index of a packet timestamp, relative to stream start.
    fn sample_index(&self, pts: i64) -> u64 {
        let start = if self.first_pts == i64::MIN {
            0
        } else {
            self.first_pts
        };
        let base = i128::from(pts) - i128::from(start);
        let denominator = i128::from(self.time_base.denominator());
        if base <= 0 || denominator <= 0 {
            return 0;
        }
        u64::try_from(
            base * i128::from(self.time_base.numerator()) * i128::from(self.decoder.rate())
                / denominator,
        )
        .unwrap_or(u64::MAX)
    }
}

/// Interleaved stereo f32 at the frame's own rate; mono duplicates, wider
/// layouts keep their first two channels.
fn stereo_samples(frame: &ffmpeg::frame::Audio, channels: u16) -> Vec<f32> {
    let format = frame.format();
    let width = format.bytes().max(1);
    let planar = frame.is_planar();
    let planes = frame.planes();
    if planes == 0 {
        return Vec::new();
    }
    let channels = usize::from(channels).max(1);
    let at = |channel: usize, sample: usize| -> f32 {
        let data = frame.data(if planar { channel.min(planes - 1) } else { 0 });
        // FFmpeg only fills linesize[0] for audio; planes beyond the first
        // report zero length, so rebuild the slice by sample count.
        let data = if planar && data.len() < frame.samples() * width {
            unsafe { std::slice::from_raw_parts(data.as_ptr(), frame.samples() * width) }
        } else {
            data
        };
        let index = (if planar {
            sample
        } else {
            sample * channels + channel
        }) * width;
        // Layout and plane sizes can disagree; a short read plays silence.
        let Some(raw) = data.get(index..index + width) else {
            return 0.0;
        };
        match format {
            ffmpeg::format::Sample::U8(_) => (f32::from(raw[0]) - 128.0) / 128.0,
            ffmpeg::format::Sample::I16(_) => {
                f32::from(i16::from_ne_bytes(raw.try_into().unwrap())) / 32768.0
            }
            ffmpeg::format::Sample::I32(_) => {
                i32::from_ne_bytes(raw.try_into().unwrap()) as f32 / 2_147_483_648.0
            }
            ffmpeg::format::Sample::I64(_) => {
                i64::from_ne_bytes(raw.try_into().unwrap()) as f32 / 9.223_372e18
            }
            ffmpeg::format::Sample::F64(_) => f64::from_ne_bytes(raw.try_into().unwrap()) as f32,
            _ => f32::from_ne_bytes(raw.try_into().unwrap()),
        }
    };
    let mut out = Vec::with_capacity(frame.samples() * CHANNELS);
    for sample in 0..frame.samples() {
        out.push(at(0, sample));
        out.push(at(1.min(channels - 1), sample));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_wav(path: &Path, rate: u32, channels: u16, samples: &[i16]) {
        let mut bytes = Vec::new();
        bytes.extend(b"RIFF");
        bytes.extend(((36 + samples.len() * 2) as u32).to_le_bytes());
        bytes.extend(b"WAVEfmt ");
        bytes.extend(16_u32.to_le_bytes());
        bytes.extend(1_u16.to_le_bytes());
        bytes.extend(channels.to_le_bytes());
        bytes.extend(rate.to_le_bytes());
        bytes.extend((rate * u32::from(channels) * 2).to_le_bytes());
        bytes.extend((channels * 2).to_le_bytes());
        bytes.extend(16_u16.to_le_bytes());
        bytes.extend(b"data");
        bytes.extend(((samples.len() * 2) as u32).to_le_bytes());
        for sample in samples {
            bytes.extend(sample.to_le_bytes());
        }
        fs::write(path, bytes).unwrap();
    }

    fn drain(queue: &AudioQueue) -> (usize, usize) {
        let mut heard = 0;
        let mut loud = 0;
        loop {
            match queue.pop() {
                Some(sample) => {
                    heard += 1;
                    assert!(sample.abs() <= 1.0);
                    if sample.abs() > 0.1 {
                        loud += 1;
                    }
                }
                None if queue.is_done() => break,
                None => std::thread::yield_now(),
            }
        }
        (heard, loud)
    }

    #[test]
    fn decodes_resamples_and_mixes_clip_audio_on_the_output_clock() {
        let root = std::env::temp_dir().join(format!("ocus-audio-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let mono = root.join("tone-24k.wav");
        let samples: Vec<i16> = (0..12_000_i32)
            .map(|index| (index * 37 % 24_000 - 12_000) as i16)
            .collect();
        write_wav(&mono, 24_000, 1, &samples);
        let stereo = root.join("bed-48k.wav");
        write_wav(&stereo, 48_000, 2, &vec![8_000; 48_000]);
        let silent = root.join("frames.y4m");
        let mut video = b"YUV4MPEG2 W2 H2 F30:1 Ip A1:1 C420jpeg\n".to_vec();
        video.extend(b"FRAME\n");
        video.extend([0, 0, 0, 0, 128, 128]);
        fs::write(&silent, video).unwrap();

        let queue = Arc::new(AudioQueue::new());
        let clock = queue.clock();
        let plan = AudioPlan {
            sources: vec![
                AudioSource {
                    path: mono.clone(),
                    start: 0,
                    source_start: 0,
                    duration: 15,
                    volume: 1.0,
                },
                AudioSource {
                    path: stereo.clone(),
                    start: 5,
                    source_start: 6,
                    duration: 10,
                    volume: 0.5,
                },
                AudioSource {
                    path: silent.clone(),
                    start: 0,
                    source_start: 0,
                    duration: 15,
                    volume: 1.0,
                },
            ],
            end: 15,
            fps: (30, 1),
        };
        let worker = {
            let queue = queue.clone();
            std::thread::spawn(move || feed(&plan, 0, queue, Arc::new(AtomicBool::new(false))))
        };
        let (heard, loud) = drain(&queue);
        worker.join().unwrap().unwrap();
        assert_eq!(heard, 48_000);
        assert!(loud > 20_000, "{loud}");
        assert!((clock.elapsed().as_secs_f64() - 0.5).abs() < 0.01);
        for file in [mono, stereo, silent] {
            fs::remove_file(file).unwrap();
        }
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn keeps_left_and_right_separate_through_the_pipeline() {
        let path = std::env::temp_dir().join(format!("ocus-stereo-{}.wav", std::process::id()));
        let mut interleaved = Vec::new();
        for _ in 0..4_800 {
            interleaved.push(16_000_i16);
            interleaved.push(-16_000_i16);
        }
        write_wav(&path, 48_000, 2, &interleaved);
        let queue = Arc::new(AudioQueue::new());
        let plan = AudioPlan {
            sources: vec![AudioSource {
                path: path.clone(),
                start: 0,
                source_start: 0,
                duration: 10,
                volume: 1.0,
            }],
            end: 10,
            fps: (30, 1),
        };
        let worker = {
            let queue = queue.clone();
            std::thread::spawn(move || feed(&plan, 0, queue, Arc::new(AtomicBool::new(false))))
        };
        let mut buffered = None;
        let mut real = 0;
        let mut split = 0;
        loop {
            let left = buffered.take().or_else(|| queue.pop());
            match left {
                None if queue.is_done() => break,
                None => std::thread::yield_now(),
                Some(left) => match queue.pop() {
                    Some(right) => {
                        if left.abs() > 0.1 || right.abs() > 0.1 {
                            real += 1;
                        }
                        if left > 0.1 && right < -0.1 {
                            split += 1;
                        }
                    }
                    None => {
                        buffered = Some(left);
                        if queue.is_done() {
                            break;
                        }
                    }
                },
            }
        }
        worker.join().unwrap().unwrap();
        assert!(real > 4_500, "{real}");
        assert_eq!(split, real);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn cancelling_stops_feeding_the_queue() {
        let queue = Arc::new(AudioQueue::new());
        let cancelled = Arc::new(AtomicBool::new(true));
        let plan = AudioPlan {
            sources: vec![AudioSource {
                path: PathBuf::from("missing.wav"),
                start: 0,
                source_start: 0,
                duration: 15,
                volume: 1.0,
            }],
            end: 15,
            fps: (30, 1),
        };
        assert!(feed(&plan, 0, queue, cancelled).is_err());
    }
}
