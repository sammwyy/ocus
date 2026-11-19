use crate::media::{Media, MediaKind};
use crate::project::Project;
use crate::timeline::{Clip, ClipContent, TrackKind};
use ffmpeg_next as ffmpeg;
use std::collections::{HashSet, VecDeque};
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

#[derive(Clone, Debug, PartialEq)]
pub struct AudioSource {
    pub path: PathBuf,
    pub start: u64,
    pub source_start: u64,
    pub duration: u64,
    pub volume: f32,
}

#[derive(Debug, PartialEq)]
pub struct AudioPlan {
    pub sources: Vec<AudioSource>,
    pub end: u64,
    pub fps: (u32, u32),
}

impl AudioPlan {
    pub fn from_project(project: &Project) -> Self {
        Self::without_media(project, &HashSet::new())
    }

    /// The project's sound with clips of the `missing` media left out.
    pub fn without_media(project: &Project, missing: &HashSet<u64>) -> Self {
        let sources = project
            .tracks()
            .iter()
            .filter(|track| !track.hidden)
            .flat_map(|track| {
                track.clips.iter().filter_map(|clip| {
                    let ClipContent::Media { media_id } = clip.content else {
                        return None;
                    };
                    if missing.contains(&media_id) {
                        return None;
                    }
                    let media = project.media().iter().find(|media| media.id == media_id)?;
                    audible(track.kind, clip, media).then(|| AudioSource {
                        path: media.path.clone(),
                        start: clip.start_frame,
                        source_start: clip.source_start_frame,
                        duration: clip.duration_frames,
                        volume: clip.volume,
                    })
                })
            })
            .collect();
        Self {
            sources,
            end: project
                .tracks()
                .iter()
                .flat_map(|track| &track.clips)
                .filter_map(|clip| clip.start_frame.checked_add(clip.duration_frames))
                .max()
                .unwrap_or(0),
            fps: (
                project.settings().frame_rate.numerator,
                project.settings().frame_rate.denominator,
            ),
        }
    }
}

pub(crate) fn audible(kind: TrackKind, clip: &Clip, media: &Media) -> bool {
    !clip.muted && carries_sound(kind, clip, media)
}

pub(crate) fn carries_sound(kind: TrackKind, clip: &Clip, media: &Media) -> bool {
    match kind {
        TrackKind::Video => clip.linked_audio && media.has_audio,
        TrackKind::Audio => media.kind == MediaKind::Audio || media.has_audio,
    }
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

/// Mixes every source overlapping `start` to 48 kHz stereo and feeds the
/// queue a little ahead of playback until the sequence ends. Each source is
/// opened when its first chunk is mixed and released after its last, so only
/// audible clips hold decoders. Gaps and exhausted sources contribute silence
/// so output stays timeline-aligned.
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
    struct Slot<'a> {
        path: &'a Path,
        offset: u64,
        reader: Option<AudioReader>,
        begin: u64,
        end: u64,
        volume: f32,
    }
    let mut slots: Vec<Slot> = plan
        .sources
        .iter()
        .filter_map(|source| {
            let from = source.start.max(start);
            let until = source.start.saturating_add(source.duration).min(plan.end);
            (from < until).then(|| Slot {
                path: &source.path,
                offset: source.source_start + from - source.start,
                reader: None,
                begin: to_samples(from),
                end: to_samples(until),
                volume: source.volume,
            })
        })
        .collect();
    // Errors raised after a cancel come from the interrupted I/O itself.
    let mut first_error = None;
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
            let reader = match &mut slot.reader {
                Some(reader) => reader,
                None => {
                    match AudioReader::open(slot.path, slot.offset, plan.fps, cancelled.clone()) {
                        Ok(Some(reader)) => slot.reader.insert(reader),
                        Ok(None) => {
                            slot.end = from;
                            continue;
                        }
                        Err(error) => {
                            if !cancelled.load(Ordering::Relaxed) {
                                first_error.get_or_insert(error);
                            }
                            slot.end = from;
                            continue;
                        }
                    }
                }
            };
            let mut sound = Vec::with_capacity((until - from) as usize * CHANNELS);
            match reader.read(&mut sound, (until - from) as usize, &cancelled) {
                Ok(_) => {
                    for (index, frame) in sound.as_chunks::<CHANNELS>().0.iter().enumerate() {
                        let offset = (from - position) as usize * CHANNELS + index * CHANNELS;
                        mix[offset] += frame[0] * slot.volume;
                        mix[offset + 1] += frame[1] * slot.volume;
                    }
                }
                Err(error) => {
                    if !cancelled.load(Ordering::Relaxed) {
                        first_error.get_or_insert(error);
                    }
                    slot.end = from;
                }
            }
        }
        slots.retain(|slot| slot.end > position + count as u64);
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
    origin_pts: i64,
    skip_until: u64,
    pending: VecDeque<f32>,
    initial: bool,
    silence: u64,
    deferred: Option<Vec<f32>>,
    downmixer: Option<ffmpeg::software::resampling::Context>,
    resampler: Option<ffmpeg::software::resampling::Context>,
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
        let origin_pts = super::format_origin(&input, time_base);
        let mut decoder = ffmpeg::codec::context::Context::from_parameters(stream.parameters())
            .and_then(|context| context.decoder().audio())
            .map_err(|error| error.to_string())?;
        let rate = decoder.rate();
        if rate == 0 || decoder.format() == ffmpeg::format::Sample::None {
            return Err("Audio source has an unsupported format".into());
        }
        if offset > 0 {
            let micros = u128::from(offset) * u128::from(fps.1) * 1_000_000 / u128::from(fps.0);
            if let Some(timestamp) = super::seek_timestamp(&input, micros)
                && input.seek(timestamp, ..timestamp).is_ok()
            {
                decoder.flush();
            }
        }
        let downmixer = if decoder.channels() > 2 {
            let mut layout = decoder.channel_layout();
            if layout.is_empty() {
                layout = ffmpeg::ChannelLayout::default(i32::from(decoder.channels()));
            }
            let mut options = ffmpeg::Dictionary::new();
            options.set("rematrix_maxval", "1");
            Some(
                ffmpeg::software::resampling::Context::get_with(
                    decoder.format(),
                    layout,
                    rate,
                    ffmpeg::format::Sample::F32(ffmpeg::format::sample::Type::Packed),
                    ffmpeg::ChannelLayout::STEREO,
                    rate,
                    options,
                )
                .map_err(|error| error.to_string())?,
            )
        } else {
            None
        };
        Ok(Some(Self {
            input,
            decoder,
            stream_index,
            time_base,
            origin_pts,
            skip_until: u64::try_from(
                u128::from(offset) * u128::from(fps.1) * u128::from(rate) / u128::from(fps.0),
            )
            .unwrap_or(u64::MAX),
            pending: VecDeque::new(),
            initial: true,
            silence: 0,
            deferred: None,
            downmixer,
            resampler: if rate == RATE {
                None
            } else {
                Some(
                    ffmpeg::software::resampling::Context::get(
                        ffmpeg::format::Sample::F32(ffmpeg::format::sample::Type::Packed),
                        ffmpeg::ChannelLayout::STEREO,
                        rate,
                        ffmpeg::format::Sample::F32(ffmpeg::format::sample::Type::Packed),
                        ffmpeg::ChannelLayout::STEREO,
                        RATE,
                    )
                    .map_err(|error| error.to_string())?,
                )
            },
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
            let available = self.pending.len() / CHANNELS;
            if available > 0 {
                let count = available.min(frames - read);
                out.extend(self.pending.drain(..count * CHANNELS));
                read += count;
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
            if self.silence > 0 {
                let count = self.silence.min(CHUNK as u64);
                self.silence -= count;
                self.append_native(vec![0.0; count as usize * CHANNELS])?;
                return Ok(true);
            }
            if let Some(stereo) = self.deferred.take() {
                self.append_native(stereo)?;
                return Ok(true);
            }
            let mut decoded = ffmpeg::frame::Audio::empty();
            if self.decoder.receive_frame(&mut decoded).is_ok() {
                let timestamp = decoded.timestamp();
                if let Some(downmixer) = &mut self.downmixer {
                    if decoded.channel_layout().is_empty() {
                        decoded.set_channel_layout(downmixer.input().channel_layout);
                    }
                    let mut output = ffmpeg::frame::Audio::empty();
                    downmixer
                        .run(&decoded, &mut output)
                        .map_err(|error| error.to_string())?;
                    decoded = output;
                }
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
                let start = timestamp.map(|pts| self.sample_index(pts));
                if let Some(start) = start {
                    let frames = stereo.len() / CHANNELS;
                    let skip =
                        (i128::from(self.skip_until) - start).clamp(0, frames as i128) as usize;
                    stereo.drain(..skip * CHANNELS);
                }
                if stereo.is_empty() {
                    continue;
                }
                if self.initial {
                    self.initial = false;
                    self.silence = start.map_or(0, |start| {
                        u64::try_from((start - i128::from(self.skip_until)).max(0))
                            .unwrap_or(u64::MAX)
                    });
                    if self.silence > 0 {
                        self.deferred = Some(stereo);
                        continue;
                    }
                }
                self.append_native(stereo)?;
                return Ok(true);
            }
            if self.eof {
                if let Some(resampler) = &mut self.resampler {
                    let mut output = ffmpeg::frame::Audio::new(
                        resampler.output().format,
                        CHUNK,
                        ffmpeg::ChannelLayout::STEREO,
                    );
                    resampler
                        .flush(&mut output)
                        .map_err(|error| error.to_string())?;
                    if output.samples() > 0 {
                        self.pending.extend(
                            output
                                .plane::<(f32, f32)>(0)
                                .iter()
                                .flat_map(|&(left, right)| [left, right]),
                        );
                        return Ok(true);
                    }
                }
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

    fn append_native(&mut self, stereo: Vec<f32>) -> Result<(), String> {
        if let Some(resampler) = &mut self.resampler {
            let mut input = ffmpeg::frame::Audio::new(
                resampler.input().format,
                stereo.len() / CHANNELS,
                ffmpeg::ChannelLayout::STEREO,
            );
            input.set_rate(resampler.input().rate);
            for (frame, samples) in input
                .plane_mut::<(f32, f32)>(0)
                .iter_mut()
                .zip(stereo.as_chunks::<CHANNELS>().0)
            {
                *frame = (samples[0], samples[1]);
            }
            let mut output = ffmpeg::frame::Audio::empty();
            resampler
                .run(&input, &mut output)
                .map_err(|error| error.to_string())?;
            self.pending.extend(
                output
                    .plane::<(f32, f32)>(0)
                    .iter()
                    .flat_map(|&(left, right)| [left, right]),
            );
        } else {
            self.pending.extend(stereo);
        }
        Ok(())
    }

    /// Source-sample index relative to the container origin.
    fn sample_index(&self, pts: i64) -> i128 {
        let base = i128::from(pts) - i128::from(self.origin_pts);
        let denominator = i128::from(self.time_base.denominator());
        if denominator <= 0 {
            return 0;
        }
        base * i128::from(self.time_base.numerator()) * i128::from(self.decoder.rate())
            / denominator
    }
}

/// Interleaved stereo f32 at the frame's own rate; mono duplicates.
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

    fn mixed_project() -> Project {
        let mut value = serde_json::to_value(Project::new("Mixed audio")).unwrap();
        value["settings"]["frame_rate"] =
            serde_json::json!({"numerator": 30000, "denominator": 1001});
        value["media"] = serde_json::json!([
            {"id":1,"path":"video.mp4","kind":"video","has_audio":true},
            {"id":2,"path":"silent.mp4","kind":"video","has_audio":false},
            {"id":3,"path":"sound.wav","kind":"audio","has_audio":false},
            {"id":4,"path":"image.png","kind":"image","has_audio":false}
        ]);
        let clip = |id, media_id, start, linked, muted, volume| {
            serde_json::json!({
                "id":id,"start_frame":start,"source_start_frame":2,"duration_frames":10,
                "content":{"media":{"media_id":media_id}},"linked_audio":linked,"muted":muted,"volume":volume
            })
        };
        value["tracks"][0]["clips"] = serde_json::json!([
            clip(1, 1, 0, true, false, 0.5),
            clip(2, 1, 10, false, false, 1.0),
            clip(3, 2, 20, true, false, 1.0),
            clip(4, 1, 30, true, true, 1.0),
            clip(5, 4, 40, false, false, 1.0),
            clip(6, 99, 50, true, false, 1.0)
        ]);
        value["tracks"][1]["clips"] = serde_json::json!([clip(7, 1, 5, true, false, 0.25)]);
        value["tracks"][2]["clips"] = serde_json::json!([
            clip(8, 3, 0, false, false, 1.5),
            clip(9, 1, 10, false, false, 0.75),
            clip(10, 2, 20, false, false, 1.0),
            clip(11, 4, 30, false, false, 1.0),
            clip(12, 3, 40, false, true, 1.0)
        ]);
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn hidden_tracks_exclude_linked_video_and_audio_but_keep_the_clock() {
        let project = mixed_project();
        let expected_end = AudioPlan::from_project(&project).end;
        let mut value = serde_json::to_value(&project).unwrap();
        for track in value["tracks"].as_array_mut().unwrap() {
            track["hidden"] = true.into();
        }
        let hidden: Project = serde_json::from_value(value.clone()).unwrap();
        let plan = AudioPlan::from_project(&hidden);
        assert!(plan.sources.is_empty());
        assert_eq!(plan.end, expected_end);
        value["tracks"][1]["hidden"] = false.into();
        let one: Project = serde_json::from_value(value).unwrap();
        let plan = AudioPlan::from_project(&one);
        assert_eq!(plan.sources.len(), 1);
        assert_eq!(plan.sources[0].start, 5);
        assert!(
            AudioPlan::without_media(&one, &HashSet::from([1]))
                .sources
                .is_empty()
        );
    }

    #[test]
    fn timeline_audio_plan_preserves_sources_and_clock() {
        let project = mixed_project();
        let source = |path: &str, start, volume| AudioSource {
            path: path.into(),
            start,
            source_start: 2,
            duration: 10,
            volume,
        };
        let expected = AudioPlan {
            sources: vec![
                source("video.mp4", 0, 0.5),
                source("video.mp4", 5, 0.25),
                source("sound.wav", 0, 1.5),
                source("video.mp4", 10, 0.75),
            ],
            end: 60,
            fps: (30000, 1001),
        };
        assert_eq!(AudioPlan::from_project(&project), expected);
        assert_eq!(
            AudioPlan::without_media(&project, &HashSet::from([1, 99])),
            AudioPlan {
                sources: vec![source("sound.wav", 0, 1.5)],
                end: 60,
                fps: (30000, 1001),
            }
        );
    }

    #[test]
    fn empty_timeline_audio_plan() {
        let project = Project::new("Empty");
        assert_eq!(
            AudioPlan::from_project(&project),
            AudioPlan {
                sources: vec![],
                end: 0,
                fps: (30, 1),
            }
        );
    }

    #[test]
    fn timeline_audio_plan_ignores_overflowing_ends() {
        let mut value = serde_json::to_value(mixed_project()).unwrap();
        value["tracks"][0]["clips"][5]["start_frame"] = u64::MAX.into();
        let project = serde_json::from_value(value).unwrap();
        assert_eq!(AudioPlan::from_project(&project).end, 50);
    }

    #[test]
    fn downmixes_center_channel_through_feed() {
        let root = std::env::temp_dir().join(format!("ocus-downmix-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        for (name, layout, signal) in [
            ("mono", "mono", "0.5*sin(2*PI*440*t)"),
            (
                "stereo",
                "stereo",
                "0.5*sin(2*PI*440*t)|-0.25*sin(2*PI*440*t)",
            ),
            ("center", "5.1", "0|0|0.5*sin(2*PI*440*t)|0|0|0"),
            ("center-7.1", "7.1", "0|0|0.5*sin(2*PI*440*t)|0|0|0|0|0"),
            (
                "surround",
                "5.1(side)",
                "0|0|0|0|0.5*sin(2*PI*440*t)|0.5*sin(2*PI*440*t)",
            ),
            ("lfe", "5.1", "0|0|0|0.5*sin(2*PI*440*t)|0|0"),
            ("full-scale", "5.1", "1|1|1|1|1|1"),
        ] {
            let path = root.join(format!("{name}.wav"));
            assert!(
                std::process::Command::new("ffmpeg")
                    .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
                    .arg(format!("aevalsrc={signal}:s=48000:d=0.2:c={layout}"))
                    .args(["-c:a", "pcm_f32le"])
                    .arg(&path)
                    .status()
                    .unwrap()
                    .success()
            );
            let queue = Arc::new(AudioQueue::new());
            let plan = AudioPlan {
                sources: vec![AudioSource {
                    path,
                    start: 0,
                    source_start: 0,
                    duration: 6,
                    volume: 1.0,
                }],
                end: 6,
                fps: (30, 1),
            };
            feed(&plan, 0, queue.clone(), Arc::new(AtomicBool::new(false))).unwrap();
            let samples: Vec<_> = std::iter::from_fn(|| queue.pop()).collect();
            assert_eq!(samples.len(), 19_200);
            let rms = |channel| {
                (samples
                    .iter()
                    .skip(channel)
                    .step_by(2)
                    .map(|&v| f64::from(v).powi(2))
                    .sum::<f64>()
                    / 9600.0)
                    .sqrt()
            };
            let (left, right) = (rms(0), rms(1));
            println!("{name} ({layout}): RMS L={left:.6} R={right:.6}");
            if name == "lfe" {
                assert_eq!((left, right), (0.0, 0.0));
            } else {
                assert!(left > 0.05 && right > 0.05, "{layout}: silent downmix");
            }
            if name == "full-scale" {
                assert!(samples.iter().all(|&sample| (sample - 1.0).abs() < 0.00001));
            }
            if layout == "stereo" {
                for (index, pair) in samples.as_chunks::<2>().0.iter().enumerate() {
                    let sine =
                        (2.0 * std::f32::consts::PI * 440.0 * index as f32 / RATE as f32).sin();
                    assert!((pair[0] - 0.5 * sine).abs() < 0.0001);
                    assert!((pair[1] + 0.25 * sine).abs() < 0.0001);
                }
            } else {
                assert!((left - right).abs() < 0.00001);
            }
        }
        fs::remove_dir_all(root).unwrap();
    }

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

    fn open_descriptors() -> usize {
        fs::read_dir("/proc/self/fd").unwrap().count()
    }

    #[test]
    #[ignore = "release-mode playback startup benchmark; requires Linux /proc"]
    fn benchmark_playback_startup() {
        let path =
            std::env::temp_dir().join(format!("ocus-startup-bench-{}.wav", std::process::id()));
        write_wav(&path, 44_100, 2, &vec![8_000; 44_100 * 8 * CHANNELS]);
        for clips in [1, 128] {
            let plan = Arc::new(AudioPlan {
                sources: (0..clips)
                    .map(|index| AudioSource {
                        path: path.clone(),
                        start: index * 240,
                        source_start: 0,
                        duration: 240,
                        volume: 1.0,
                    })
                    .collect(),
                end: clips * 240,
                fps: (30, 1),
            });
            let mut runs = Vec::new();
            for _ in 0..5 {
                let baseline = open_descriptors();
                let queue = Arc::new(AudioQueue::new());
                let cancelled = Arc::new(AtomicBool::new(false));
                let started = std::time::Instant::now();
                let worker = {
                    let (plan, queue, cancelled) = (plan.clone(), queue.clone(), cancelled.clone());
                    std::thread::spawn(move || feed(&plan, 0, queue, cancelled))
                };
                while queue.pop().is_none() {
                    std::thread::yield_now();
                }
                let first_sample = started.elapsed();
                let open = open_descriptors() - baseline;
                cancelled.store(true, Ordering::Relaxed);
                worker.join().unwrap().unwrap();
                runs.push((first_sample, open, open_descriptors() - baseline));
            }
            runs.sort();
            let (first_sample, open, after_cancel) = runs[runs.len() / 2];
            println!(
                "clips={clips} median_of={} first_sample_ms={:.1} open_descriptors={open} after_cancel={after_cancel}",
                runs.len(),
                first_sample.as_secs_f64() * 1_000.0
            );
        }
        fs::remove_file(path).unwrap();
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
    fn resamples_trimmed_audio_above_the_output_rate() {
        let path = std::env::temp_dir().join(format!("ocus-176k-{}.wav", std::process::id()));
        let interleaved: Vec<i16> = (0..176_400 * 2_i32)
            .map(|index| (index * 53 % 32_000 - 16_000) as i16)
            .collect();
        write_wav(&path, 176_400, 2, &interleaved);
        let queue = Arc::new(AudioQueue::new());
        let plan = AudioPlan {
            sources: vec![AudioSource {
                path: path.clone(),
                start: 0,
                source_start: 3,
                duration: 6,
                volume: 1.0,
            }],
            end: 6,
            fps: (30, 1),
        };
        // Six frames fit in the queue, so the worker finishes before draining.
        let worker = {
            let queue = queue.clone();
            std::thread::spawn(move || feed(&plan, 0, queue, Arc::new(AtomicBool::new(false))))
        };
        worker.join().unwrap().unwrap();
        let (heard, loud) = drain(&queue);
        assert_eq!(heard, 19_200);
        assert!(loud > 8_000, "{loud}");
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn drains_resampled_audio_once_including_the_tail() {
        ffmpeg::init().unwrap();
        for rate in [24_000, 96_000, 176_400] {
            let path = std::env::temp_dir()
                .join(format!("ocus-audio-tail-{}-{rate}.wav", std::process::id()));
            write_wav(&path, rate, 2, &vec![8_000; rate as usize / 5 * CHANNELS]);
            let cancelled = Arc::new(AtomicBool::new(false));
            let mut reader = AudioReader::open(&path, 0, (30, 1), cancelled.clone())
                .unwrap()
                .unwrap();
            let mut samples = Vec::new();
            assert_eq!(
                reader.read(&mut samples, 10_000, &cancelled).unwrap(),
                9_600
            );
            assert_eq!(samples.len(), 19_200);
            assert!(samples[samples.len() - 1] > 0.1);
            assert_eq!(reader.read(&mut samples, 1, &cancelled).unwrap(), 0);
            let mut trimmed = AudioReader::open(&path, 6, (30, 1), cancelled.clone())
                .unwrap()
                .unwrap();
            assert_eq!(trimmed.read(&mut Vec::new(), 1, &cancelled).unwrap(), 0);
            fs::remove_file(path).unwrap();
        }
    }

    #[test]
    fn resampling_rejects_aliases_and_preserves_the_passband() {
        let mut amplitudes = Vec::new();
        for frequency in [30_000, 1_000] {
            let path = std::env::temp_dir().join(format!(
                "ocus-spectrum-{}-{frequency}.wav",
                std::process::id()
            ));
            let samples: Vec<i16> = (0..19_200)
                .flat_map(|index| {
                    let phase =
                        std::f64::consts::TAU * f64::from(frequency) * f64::from(index) / 96_000.0;
                    [(phase.sin() * 16_384.0) as i16; CHANNELS]
                })
                .collect();
            write_wav(&path, 96_000, 2, &samples);
            let queue = Arc::new(AudioQueue::new());
            let plan = AudioPlan {
                sources: vec![AudioSource {
                    path: path.clone(),
                    start: 0,
                    source_start: 0,
                    duration: 6,
                    volume: 1.0,
                }],
                end: 6,
                fps: (30, 1),
            };
            let worker = {
                let queue = queue.clone();
                std::thread::spawn(move || feed(&plan, 0, queue, Arc::new(AtomicBool::new(false))))
            };
            worker.join().unwrap().unwrap();
            let mut output = Vec::new();
            while let Some(sample) = queue.pop() {
                output.push(sample);
            }
            assert_eq!(output.len(), 19_200);
            // Exclude filter startup and EOF transients; retain whole tone periods.
            let frames = &output.as_chunks::<CHANNELS>().0[480..9_120];
            let bin = if frequency == 30_000 {
                18_000
            } else {
                frequency
            };
            let (real, imaginary) =
                frames
                    .iter()
                    .enumerate()
                    .fold((0.0, 0.0), |(real, imaginary), (index, frame)| {
                        let phase =
                            std::f64::consts::TAU * f64::from(bin) * index as f64 / f64::from(RATE);
                        (
                            real + f64::from(frame[0]) * phase.cos(),
                            imaginary + f64::from(frame[0]) * phase.sin(),
                        )
                    });
            let amplitude = 2.0 * real.hypot(imaginary) / frames.len() as f64;
            println!(
                "{frequency} Hz input: output amplitude {amplitude:.9}, gain {:.2} dB",
                20.0 * (amplitude / 0.5).log10()
            );
            amplitudes.push(amplitude);
            fs::remove_file(path).unwrap();
        }
        assert!(amplitudes[0] < 0.0005, "alias amplitude: {}", amplitudes[0]);
        assert!((amplitudes[1] / 0.5 - 1.0).abs() < 0.01);
    }

    /// The mix every reader opened up front produces, summed in plan order.
    fn eager_mix(plan: &AudioPlan, start: u64) -> Vec<f32> {
        let to_samples =
            |frame: u64| frame * u64::from(RATE) * u64::from(plan.fps.1) / u64::from(plan.fps.0);
        let cancelled = Arc::new(AtomicBool::new(false));
        let first = to_samples(start);
        let mut mix = vec![0.0; (to_samples(plan.end) - first) as usize * CHANNELS];
        for source in &plan.sources {
            let from = source.start.max(start);
            let until = (source.start + source.duration).min(plan.end);
            if from >= until {
                continue;
            }
            let offset = source.source_start + from - source.start;
            let mut reader = AudioReader::open(&source.path, offset, plan.fps, cancelled.clone())
                .unwrap()
                .unwrap();
            let mut sound = Vec::new();
            let frames = (to_samples(until) - to_samples(from)) as usize;
            reader.read(&mut sound, frames, &cancelled).unwrap();
            let base = (to_samples(from) - first) as usize * CHANNELS;
            for (index, sample) in sound.iter().enumerate() {
                mix[base + index] += sample * source.volume;
            }
        }
        mix.iter().map(|sample| sample.clamp(-1.0, 1.0)).collect()
    }

    /// Feeds `plan` from `start`, cancelling once `cancel_after` samples were
    /// heard; returns every sample the queue delivered. Samples after the
    /// cancellation may be cut short and are never played.
    fn play(plan: &Arc<AudioPlan>, start: u64, cancel_after: Option<usize>) -> Vec<f32> {
        let queue = Arc::new(AudioQueue::new());
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker = {
            let (plan, queue, cancelled) = (plan.clone(), queue.clone(), cancelled.clone());
            std::thread::spawn(move || feed(&plan, start, queue, cancelled))
        };
        let mut heard = Vec::new();
        loop {
            match queue.pop() {
                Some(sample) => {
                    heard.push(sample);
                    if cancel_after == Some(heard.len()) {
                        cancelled.store(true, Ordering::Relaxed);
                    }
                }
                None if queue.is_done() => break,
                None => std::thread::yield_now(),
            }
        }
        worker.join().unwrap().unwrap();
        heard
    }

    #[test]
    fn late_audio_keeps_its_container_offset() {
        for rate in [48_000, 44_100] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("late.mp4");
            let generated = std::process::Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-y",
                    "-f",
                    "lavfi",
                    "-i",
                    "color=c=black:s=32x32:r=30:d=4",
                    "-itsoffset",
                    "0.5",
                    "-f",
                    "lavfi",
                    "-i",
                ])
                .arg(format!(
                    "aevalsrc=if(between(t\\,0.5\\,0.6)\\,0.8*sin(2*PI*1000*t)\\,0):s={rate}:d=3.5"
                ))
                .args(["-c:v", "mpeg4", "-c:a", "aac", "-output_ts_offset", "30"])
                .arg(&path)
                .status();
            let status = match generated {
                Ok(status) => status,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
                Err(error) => panic!("{error}"),
            };
            assert!(status.success());
            ffmpeg::init().unwrap();
            let reader = AudioReader::open(&path, 0, (30, 1), Arc::new(AtomicBool::new(false)))
                .unwrap()
                .unwrap();
            assert_eq!(
                reader.sample_index(reader.origin_pts - i64::from(rate)),
                -i128::from(rate)
            );
            let plan = Arc::new(AudioPlan {
                sources: vec![AudioSource {
                    path,
                    start: 0,
                    source_start: 0,
                    duration: 120,
                    volume: 1.0,
                }],
                end: 120,
                fps: (30, 1),
            });
            for start in [0, 9, 15, 30, 45] {
                let samples = play(&plan, start, None);
                assert_eq!(
                    samples.len(),
                    (120 - start) as usize * RATE as usize / 30 * CHANNELS
                );
                if start == 45 {
                    assert!(samples.iter().all(|sample| sample.abs() < 0.03));
                    continue;
                }
                let onset = samples
                    .as_chunks::<CHANNELS>()
                    .0
                    .iter()
                    .position(|frame| frame[0].abs() > 0.03)
                    .unwrap() as f64
                    / f64::from(RATE);
                let expected = 1.0 - start as f64 / 30.0;
                assert!(
                    (onset - expected).abs() <= 0.025,
                    "rate={rate}, start={start}: beep at {onset}, expected {expected}"
                );
            }
        }
    }

    fn mixing_plan(root: &Path) -> Arc<AudioPlan> {
        let ramp = root.join("ramp-44k.wav");
        let ramp_samples: Vec<i16> = (0..44_100 * 2_i32)
            .map(|index| (index * 29 % 30_000 - 15_000) as i16)
            .collect();
        write_wav(&ramp, 44_100, 1, &ramp_samples);
        let stereo = root.join("stereo-48k.wav");
        let stereo_samples: Vec<i16> = (0..48_000 * 2 * 2_i32)
            .map(|index| (index * 41 % 26_000 - 13_000) as i16)
            .collect();
        write_wav(&stereo, 48_000, 2, &stereo_samples);
        let source = |path: &PathBuf, start, source_start, duration, volume| AudioSource {
            path: path.clone(),
            start,
            source_start,
            duration,
            volume,
        };
        // Overlap at 10..20, a gap at 30..40, a second overlap at 45..55.
        Arc::new(AudioPlan {
            sources: vec![
                source(&ramp, 0, 0, 20, 1.0),
                source(&stereo, 10, 5, 20, 0.5),
                source(&ramp, 40, 3, 15, 0.8),
                source(&stereo, 45, 0, 15, 1.0),
            ],
            end: 60,
            fps: (30, 1),
        })
    }

    #[test]
    fn mixes_identically_across_overlaps_gaps_and_seeks() {
        let root = std::env::temp_dir().join(format!("ocus-audio-mix-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let plan = mixing_plan(&root);
        for start in [0, 15, 25, 33, 40, 50] {
            let expected = eager_mix(&plan, start);
            assert!(expected.iter().any(|sample| sample.abs() > 0.1));
            let heard = play(&plan, start, None);
            assert_eq!(heard.len(), expected.len(), "start {start}");
            let mismatch = heard
                .iter()
                .zip(&expected)
                .position(|(heard, expected)| heard.to_bits() != expected.to_bits());
            assert_eq!(mismatch, None, "start {start}");
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cancelling_mid_playback_keeps_the_mixed_prefix() {
        let root = std::env::temp_dir().join(format!("ocus-audio-cancel-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let plan = mixing_plan(&root);
        let expected = eager_mix(&plan, 0);
        // During the first overlap and inside the gap.
        for cancel_after in [60_000, 112_000] {
            let heard = play(&plan, 0, Some(cancel_after));
            assert!(heard.len() >= cancel_after && heard.len() < expected.len());
            let mismatch = heard[..cancel_after]
                .iter()
                .zip(&expected)
                .position(|(heard, expected)| heard.to_bits() != expected.to_bits());
            assert_eq!(mismatch, None, "cancel after {cancel_after}");
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn cancelling_while_a_source_opens_is_not_an_error() {
        let path = std::env::temp_dir().join(format!("ocus-opening-{}.wav", std::process::id()));
        let _ = fs::remove_file(&path);
        let made = std::process::Command::new("mkfifo").arg(&path).status();
        assert!(made.unwrap().success());
        let plan = AudioPlan {
            sources: vec![AudioSource {
                path: path.clone(),
                start: 0,
                source_start: 0,
                duration: 6,
                volume: 1.0,
            }],
            end: 6,
            fps: (30, 1),
        };
        let queue = Arc::new(AudioQueue::new());
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker = {
            let (queue, cancelled) = (queue.clone(), cancelled.clone());
            std::thread::spawn(move || feed(&plan, 0, queue, cancelled))
        };
        // Opening a pipe for writing waits until the feed opens it to read,
        // so the cancel lands while the source is being opened.
        let writer = fs::OpenOptions::new().write(true).open(&path).unwrap();
        cancelled.store(true, Ordering::Relaxed);
        drop(writer);
        assert_eq!(worker.join().unwrap(), Ok(()));
        assert!(queue.is_done());
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
                duration: 6,
                volume: 1.0,
            }],
            end: 6,
            fps: (30, 1),
        };
        assert_eq!(feed(&plan, 0, queue.clone(), cancelled), Ok(()));
        assert!(queue.is_done() && queue.pop().is_none());
        // Six frames fit in the queue, so the uncancelled feed never blocks.
        let reached = feed(
            &plan,
            0,
            Arc::new(AudioQueue::new()),
            Arc::new(AtomicBool::new(false)),
        );
        assert!(reached.is_err());
    }
}
