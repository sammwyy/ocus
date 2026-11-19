pub mod presets;

use crate::media::audio::{self, AudioPlan, AudioQueue};
use crate::media::composite::{Compositor, VisualPlan};
use crate::media::fitted_dimensions;
use crate::project::Project;
use crate::project::export_settings::{
    AudioCodec, Container, EncodingSpeed, ExportSettings, VideoCodec,
};
use ffmpeg_next as ffmpeg;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;

#[derive(Default)]
pub struct ExportControl {
    cancelled: Arc<AtomicBool>,
    progress: AtomicU64,
}

impl ExportControl {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    pub fn frames_done(&self) -> u64 {
        self.progress.load(Ordering::Relaxed)
    }

    fn check(&self) -> Result<(), String> {
        if self.cancelled.load(Ordering::Relaxed) {
            Err("Export cancelled".into())
        } else {
            Ok(())
        }
    }
}

pub fn can_render(project: &Project) -> bool {
    project.tracks().iter().any(|track| !track.clips.is_empty())
}

pub fn render(project: &Project, path: &Path) -> Result<(), String> {
    render_with_control(project, path, Arc::new(ExportControl::default()))
}

pub fn render_with_control(
    project: &Project,
    path: &Path,
    control: Arc<ExportControl>,
) -> Result<(), String> {
    let video = project.settings();
    let settings = ExportSettings::for_project(video.width, video.height, video.frame_rate.clone());
    render_with_settings(project, path, &settings, control)
}

pub fn render_with_settings(
    project: &Project,
    path: &Path,
    settings: &ExportSettings,
    control: Arc<ExportControl>,
) -> Result<(), String> {
    control.check()?;
    if !can_render(project) {
        return Err("Nothing to export".into());
    }
    project.validate()?;
    settings.validate()?;
    if !path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case(settings.container.extension()))
    {
        return Err(format!(
            "Export filename must end in .{}",
            settings.container.extension()
        ));
    }
    if path.exists() {
        let destination = fs::canonicalize(path).map_err(|error| error.to_string())?;
        for media in project.media() {
            if fs::canonicalize(&media.path).is_ok_and(|source| destination == source) {
                return Err("Export destination cannot be an imported source".into());
            }
        }
        return Err("Export destination already exists. Choose a different filename.".into());
    }
    write_export(path, settings.container.extension(), |temporary| {
        render_to(project, temporary, settings, control.clone())?;
        control.check()
    })
}

fn write_export(
    path: &Path,
    extension: &str,
    encode: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let suffix = format!(".{extension}");
    let mut builder = tempfile::Builder::new();
    builder.prefix(".ocus-export-").suffix(&suffix);
    #[cfg(unix)]
    builder.permissions(std::os::unix::fs::PermissionsExt::from_mode(0o666));
    let temporary = builder
        .tempfile_in(parent)
        .map_err(|error| error.to_string())?
        .into_temp_path();
    encode(&temporary)?;
    temporary
        .persist_noclobber(path)
        .map_err(|error| error.error.to_string())
}

fn render_to(
    project: &Project,
    path: &Path,
    settings: &ExportSettings,
    control: Arc<ExportControl>,
) -> Result<(), String> {
    ffmpeg::init().map_err(|error| error.to_string())?;
    let audio_plan = AudioPlan::from_project(project);
    if settings.container.has_audio() {
        for source in &audio_plan.sources {
            control.check()?;
            let interrupted = control.cancelled.clone();
            let input = ffmpeg::format::input_with_interrupt(&source.path, move || {
                interrupted.load(Ordering::Relaxed)
            })
            .map_err(|error| error.to_string())?;
            if input.streams().best(ffmpeg::media::Type::Audio).is_none() {
                return Err("An audible clip has no audio stream".into());
            }
        }
    }
    let output = ffmpeg::format::output(path).map_err(|error| error.to_string())?;
    let include_audio = settings.container.has_audio()
        && (!audio_plan.sources.is_empty() || !settings.container.has_video());
    let queue = include_audio.then(|| Arc::new(AudioQueue::new()));
    let cancelled = control.cancelled.clone();
    thread::scope(|scope| {
        let worker = queue.as_ref().map(|queue| {
            let queue = queue.clone();
            let cancelled = cancelled.clone();
            let plan = &audio_plan;
            scope.spawn(move || audio::feed(plan, 0, queue, cancelled))
        });
        let result = if settings.container.has_video() {
            render_video(project, output, settings, control, &audio_plan, queue)
        } else {
            queue
                .as_deref()
                .ok_or_else(|| "Audio export queue missing".to_string())
                .and_then(|queue| render_wav(output, settings, &control, &audio_plan, queue))
        };
        if result.is_err() {
            cancelled.store(true, Ordering::Relaxed);
        }
        let audio_result = worker
            .map(|worker| {
                worker
                    .join()
                    .map_err(|_| "Audio export worker panicked".to_string())
                    .and_then(|result| result)
            })
            .transpose();
        result.and(audio_result.map(|_| ()))
    })
}

fn render_video(
    project: &Project,
    mut output: ffmpeg::format::context::Output,
    settings: &ExportSettings,
    control: Arc<ExportControl>,
    audio_plan: &AudioPlan,
    queue: Option<Arc<AudioQueue>>,
) -> Result<(), String> {
    let fps = ffmpeg::Rational(
        i32::try_from(settings.frame_rate.numerator)
            .map_err(|_| "Export frame rate is out of range")?,
        i32::try_from(settings.frame_rate.denominator)
            .map_err(|_| "Export frame rate is out of range")?,
    );
    let time_base = ffmpeg::Rational(fps.denominator(), fps.numerator());
    let gif = settings.container == Container::Gif;
    let (video_id, pixel) = if gif {
        (ffmpeg::codec::Id::GIF, ffmpeg::format::Pixel::PAL8)
    } else {
        (
            match settings.video_codec {
                VideoCodec::H264 => ffmpeg::codec::Id::H264,
                VideoCodec::Hevc => ffmpeg::codec::Id::HEVC,
                VideoCodec::Vp9 => ffmpeg::codec::Id::VP9,
            },
            ffmpeg::format::Pixel::YUV420P,
        )
    };
    let codec = ffmpeg::encoder::find(video_id)
        .ok_or_else(|| format!("{video_id:?} encoder not available"))?;
    let mut encoder = ffmpeg::codec::context::Context::new_with_codec(codec)
        .encoder()
        .video()
        .map_err(|error| error.to_string())?;
    encoder.set_width(settings.width);
    encoder.set_height(settings.height);
    encoder.set_format(pixel);
    encoder.set_time_base(time_base);
    encoder.set_frame_rate(Some(fps));
    if !gif {
        encoder.set_bit_rate(
            settings
                .video_quality
                .map_or(settings.video_bitrate_kbps as usize * 1_000, |_| 0),
        );
    }
    if output
        .format()
        .flags()
        .contains(ffmpeg::format::Flags::GLOBAL_HEADER)
    {
        encoder.set_flags(ffmpeg::codec::Flags::GLOBAL_HEADER);
    }
    let mut options = ffmpeg::Dictionary::new();
    match (gif, settings.video_codec, settings.speed) {
        (true, _, _) => {}
        (false, VideoCodec::H264, speed) => options.set(
            "preset",
            match speed {
                EncodingSpeed::Fast => "veryfast",
                EncodingSpeed::Balanced => "medium",
                EncodingSpeed::Quality => "slow",
            },
        ),
        (false, VideoCodec::Hevc, speed) => options.set(
            "preset",
            match speed {
                EncodingSpeed::Fast => "fast",
                EncodingSpeed::Balanced => "medium",
                EncodingSpeed::Quality => "slow",
            },
        ),
        (false, VideoCodec::Vp9, speed) => {
            options.set(
                "deadline",
                if speed == EncodingSpeed::Fast {
                    "realtime"
                } else {
                    "good"
                },
            );
            options.set(
                "cpu-used",
                match speed {
                    EncodingSpeed::Fast => "6",
                    EncodingSpeed::Balanced => "4",
                    EncodingSpeed::Quality => "1",
                },
            );
        }
    }
    if let Some(quality) = settings.video_quality.filter(|_| !gif) {
        options.set("crf", &quality.to_string());
    }
    let encoder = encoder
        .open_with(options)
        .map_err(|error| error.to_string())?;
    let stream_index = {
        let mut stream = output
            .add_stream(codec)
            .map_err(|error| error.to_string())?;
        stream.set_time_base(time_base);
        stream.set_parameters(&encoder);
        stream.index()
    };
    let end = audio_plan.end;
    let fps_pair = audio_plan.fps;
    let audio = queue
        .as_ref()
        .map(|_| AudioEncoder::new(&mut output, settings))
        .transpose()?;
    output.write_header().map_err(|error| error.to_string())?;
    let output_base = output
        .stream(stream_index)
        .ok_or("Output stream missing")?
        .time_base();
    let mut muxer = Muxer {
        encoder,
        output,
        stream_index,
        output_base,
        control,
        audio,
        queue,
        fps: (
            settings.frame_rate.numerator,
            settings.frame_rate.denominator,
        ),
        audio_end: samples_at(end, fps_pair)?,
    };
    let mut palette = gif.then(|| gif_palette(settings, time_base)).transpose()?;
    let (picture_width, picture_height) = fitted_dimensions(
        project.settings().width,
        project.settings().height,
        settings.width,
        settings.height,
    );
    let mut compositor = Compositor::new(
        VisualPlan::from_project(project),
        picture_width,
        picture_height,
        fps_pair,
    );
    let mut rgba =
        ffmpeg::frame::Video::new(ffmpeg::format::Pixel::RGBA, settings.width, settings.height);
    let mut scaler = ffmpeg::software::scaling::context::Context::get(
        ffmpeg::format::Pixel::RGBA,
        settings.width,
        settings.height,
        if gif {
            ffmpeg::format::Pixel::BGRA
        } else {
            pixel
        },
        settings.width,
        settings.height,
        ffmpeg::software::scaling::flag::Flags::BILINEAR,
    )
    .map_err(|error| error.to_string())?;
    let out_end = output_frames(end, fps_pair, muxer.fps)?;
    for position in 0..out_end {
        muxer.control.check()?;
        let source_position =
            (u128::from(position) * u128::from(fps_pair.0) * u128::from(muxer.fps.1)
                / (u128::from(fps_pair.1) * u128::from(muxer.fps.0)))
            .min(u128::from(end - 1)) as u64;
        let image = compositor.frame(source_position, &muxer.control.cancelled)?;
        for row in 0..settings.height as usize {
            let stride = rgba.stride(0);
            let dest =
                &mut rgba.data_mut(0)[row * stride..row * stride + settings.width as usize * 4];
            for pixel in dest.as_chunks_mut::<4>().0 {
                pixel.copy_from_slice(&[0, 0, 0, 255]);
            }
            let top = (settings.height - picture_height) as usize / 2;
            if let Some(image) = &image
                && row >= top
                && row < top + picture_height as usize
            {
                let left = (settings.width - picture_width) as usize / 2;
                let source = &image.pixels[(row - top) * picture_width as usize * 4
                    ..(row - top + 1) * picture_width as usize * 4];
                dest[left * 4..(left + picture_width as usize) * 4].copy_from_slice(source);
            }
        }
        let mut frame = ffmpeg::frame::Video::empty();
        scaler
            .run(&rgba, &mut frame)
            .map_err(|error| error.to_string())?;
        if let Some(graph) = &mut palette {
            frame.set_pts(Some(
                i64::try_from(position).map_err(|_| "Timeline is too long to export")?,
            ));
            graph
                .get("in")
                .ok_or("GIF palette input missing")?
                .source()
                .add(&frame)
                .map_err(|error| error.to_string())?;
            muxer.write_paletted_frames(graph)?;
        } else {
            muxer.write_frame(frame, position)?;
        }
    }
    if let Some(graph) = &mut palette {
        graph
            .get("in")
            .ok_or("GIF palette input missing")?
            .source()
            .close(i64::try_from(out_end).map_err(|_| "Timeline is too long to export")?)
            .map_err(|error| error.to_string())?;
        muxer.write_paletted_frames(graph)?;
    }
    muxer
        .encoder
        .send_eof()
        .map_err(|error| error.to_string())?;
    muxer.write_packets()?;
    if let Some(mut audio) = muxer.audio.take() {
        audio.finish(&mut muxer.output)?;
    }
    muxer
        .output
        .write_trailer()
        .map_err(|error| error.to_string())
}

fn gif_palette(
    settings: &ExportSettings,
    time_base: ffmpeg::Rational,
) -> Result<ffmpeg::filter::Graph, String> {
    let mut graph = ffmpeg::filter::Graph::new();
    let source = ffmpeg::filter::find("buffer").ok_or("FFmpeg video buffer unavailable")?;
    let sink = ffmpeg::filter::find("buffersink").ok_or("FFmpeg video sink unavailable")?;
    let args = format!(
        "video_size={}x{}:pix_fmt=bgra:time_base={}/{}:pixel_aspect=1/1",
        settings.width,
        settings.height,
        time_base.numerator(),
        time_base.denominator()
    );
    graph
        .add(&source, "in", &args)
        .map_err(|error| error.to_string())?;
    graph
        .add(&sink, "out", "")
        .map_err(|error| error.to_string())?;
    graph.output("in", 0)
        .and_then(|parser| parser.input("out", 0))
        .and_then(|parser| parser.parse(
            "split[v][p];[p]palettegen=stats_mode=single:reserve_transparent=0[palette];[v][palette]paletteuse=new=1:dither=bayer"
        ))
        .and_then(|()| graph.validate())
        .map_err(|error| format!("Could not initialize GIF palette: {error}"))?;
    Ok(graph)
}

fn samples_at(frame: u64, fps: (u32, u32)) -> Result<u64, String> {
    u64::try_from(
        u128::from(frame) * u128::from(audio::RATE) * u128::from(fps.1) / u128::from(fps.0),
    )
    .map_err(|_| "Audio timeline too long".into())
}

fn render_wav(
    mut output: ffmpeg::format::context::Output,
    settings: &ExportSettings,
    control: &ExportControl,
    plan: &AudioPlan,
    queue: &AudioQueue,
) -> Result<(), String> {
    let mut audio = AudioEncoder::new(&mut output, settings)?;
    let mut options = ffmpeg::Dictionary::new();
    options.set("rf64", "auto");
    output
        .write_header_with(options)
        .map_err(|error| error.to_string())?;
    for frame in 1..=plan.end {
        audio.advance(samples_at(frame, plan.fps)?, queue, control, &mut output)?;
        control.progress.store(frame, Ordering::Relaxed);
    }
    audio.finish(&mut output)?;
    output.write_trailer().map_err(|error| error.to_string())
}

pub fn output_frames(end: u64, source: (u32, u32), target: (u32, u32)) -> Result<u64, String> {
    let numerator = u128::from(end) * u128::from(source.1) * u128::from(target.0);
    let denominator = u128::from(source.0) * u128::from(target.1);
    if denominator == 0 {
        return Err("Invalid output frame rate".into());
    }
    u64::try_from(numerator.div_ceil(denominator)).map_err(|_| "Output is too long".into())
}

struct Muxer {
    encoder: ffmpeg::encoder::video::Encoder,
    output: ffmpeg::format::context::Output,
    stream_index: usize,
    output_base: ffmpeg::Rational,
    control: Arc<ExportControl>,
    audio: Option<AudioEncoder>,
    queue: Option<Arc<AudioQueue>>,
    fps: (u32, u32),
    audio_end: u64,
}

impl Muxer {
    fn write_paletted_frames(&mut self, graph: &mut ffmpeg::filter::Graph) -> Result<(), String> {
        let mut out = graph.get("out").ok_or("GIF palette output missing")?;
        let mut sink = out.sink();
        loop {
            let mut frame = ffmpeg::frame::Video::empty();
            match sink.frame(&mut frame) {
                Ok(()) => {
                    let position = frame
                        .pts()
                        .and_then(|pts| u64::try_from(pts).ok())
                        .ok_or("GIF frame timestamp missing")?;
                    self.write_frame(frame, position)?;
                }
                Err(
                    ffmpeg::Error::Eof
                    | ffmpeg::Error::Other {
                        errno: ffmpeg::error::EAGAIN,
                    },
                ) => return Ok(()),
                Err(error) => return Err(error.to_string()),
            }
        }
    }

    fn write_frame(
        &mut self,
        mut frame: ffmpeg::frame::Video,
        position: u64,
    ) -> Result<(), String> {
        self.control.check()?;
        frame.set_pts(Some(
            i64::try_from(position).map_err(|_| "Timeline is too long to export")?,
        ));
        self.encoder
            .send_frame(&frame)
            .map_err(|error| error.to_string())?;
        self.write_packets()?;
        if let (Some(audio), Some(queue)) = (&mut self.audio, &self.queue) {
            let samples =
                ((u128::from(position + 1) * u128::from(audio::RATE) * u128::from(self.fps.1))
                    / u128::from(self.fps.0))
                .min(u128::from(self.audio_end)) as u64;
            audio.advance(samples, queue, &self.control, &mut self.output)?;
        }
        self.control.progress.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn write_packets(&mut self) -> Result<(), String> {
        let mut packet = ffmpeg::Packet::empty();
        while self.encoder.receive_packet(&mut packet).is_ok() {
            packet.set_stream(self.stream_index);
            packet.set_duration(1);
            packet.rescale_ts(self.encoder.time_base(), self.output_base);
            packet
                .write_interleaved(&mut self.output)
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}

struct AudioEncoder {
    encoder: ffmpeg::encoder::audio::Encoder,
    index: usize,
    next_sample: u64,
    encoded_samples: u64,
    pending: Vec<f32>,
    chunk_samples: usize,
    pad_final_frame: bool,
}

impl AudioEncoder {
    fn new(
        output: &mut ffmpeg::format::context::Output,
        settings: &ExportSettings,
    ) -> Result<Self, String> {
        use ffmpeg::codec::Id;
        use ffmpeg::format::sample::Type::{Packed, Planar};
        let (id, sample_type) = if settings.container == Container::Wav {
            (Id::PCM_F32LE, Packed)
        } else {
            match settings.audio_codec {
                AudioCodec::Aac => (Id::AAC, Planar),
                AudioCodec::Opus => (Id::OPUS, Packed),
            }
        };
        let codec =
            ffmpeg::encoder::find(id).ok_or_else(|| format!("{id:?} encoder not available"))?;
        let formats = codec.audio().map_err(|error| error.to_string())?;
        let format = ffmpeg::format::Sample::F32(sample_type);
        if !formats
            .formats()
            .is_some_and(|mut formats| formats.any(|f| f == format))
            || formats
                .rates()
                .is_some_and(|mut rates| !rates.any(|r| r == audio::RATE as i32))
        {
            return Err(format!("{id:?} encoder cannot accept 48 kHz float stereo"));
        }
        let time_base = ffmpeg::Rational(1, audio::RATE as i32);
        let mut encoder = ffmpeg::codec::context::Context::new_with_codec(codec)
            .encoder()
            .audio()
            .map_err(|error| error.to_string())?;
        encoder.set_rate(audio::RATE as i32);
        encoder.set_channel_layout(ffmpeg::ChannelLayout::STEREO);
        encoder.set_format(format);
        encoder.set_time_base(time_base);
        if id != Id::PCM_F32LE {
            encoder.set_bit_rate(settings.audio_bitrate_kbps as usize * 1_000);
        }
        if output
            .format()
            .flags()
            .contains(ffmpeg::format::Flags::GLOBAL_HEADER)
        {
            encoder.set_flags(ffmpeg::codec::Flags::GLOBAL_HEADER);
        }
        let encoder = encoder.open().map_err(|error| error.to_string())?;
        let mut stream = output
            .add_stream(codec)
            .map_err(|error| error.to_string())?;
        stream.set_time_base(time_base);
        stream.set_parameters(&encoder);
        let frame_size = encoder.frame_size() as usize;
        let chunk_samples = if frame_size == 0 { 1024 } else { frame_size };
        Ok(Self {
            encoder,
            index: stream.index(),
            next_sample: 0,
            encoded_samples: 0,
            pending: Vec::with_capacity(chunk_samples * 2),
            chunk_samples,
            pad_final_frame: frame_size != 0,
        })
    }

    fn advance(
        &mut self,
        until: u64,
        queue: &AudioQueue,
        control: &ExportControl,
        output: &mut ffmpeg::format::context::Output,
    ) -> Result<(), String> {
        while self.next_sample < until {
            control.check()?;
            if let Some(left) = queue.pop() {
                let right = loop {
                    control.check()?;
                    if let Some(right) = queue.pop() {
                        break right;
                    }
                    if queue.is_done() {
                        return Err("Incomplete stereo audio".into());
                    }
                    thread::yield_now();
                };
                self.pending.extend([left, right]);
                self.next_sample += 1;
                if self.pending.len() == self.chunk_samples * 2 {
                    self.encode(output)?;
                }
            } else if queue.is_done() {
                return Err("Audio ended before the timeline".into());
            } else {
                thread::yield_now();
            }
        }
        Ok(())
    }

    fn encode(&mut self, output: &mut ffmpeg::format::context::Output) -> Result<(), String> {
        let count = self.pending.len() / 2;
        let mut frame =
            ffmpeg::frame::Audio::new(self.encoder.format(), count, ffmpeg::ChannelLayout::STEREO);
        frame.set_rate(audio::RATE);
        frame.set_pts(Some(
            i64::try_from(self.encoded_samples).map_err(|_| "Audio timeline too long")?,
        ));
        if self.encoder.format().is_planar() {
            for channel in 0..2 {
                for (dst, sample) in frame
                    .plane_mut::<f32>(channel)
                    .iter_mut()
                    .zip(self.pending.as_chunks::<2>().0)
                {
                    *dst = sample[channel];
                }
            }
        } else {
            for (dest, sample) in frame
                .plane_mut::<(f32, f32)>(0)
                .iter_mut()
                .zip(self.pending.as_chunks::<2>().0)
            {
                *dest = (sample[0], sample[1]);
            }
        }
        self.encoder
            .send_frame(&frame)
            .map_err(|error| error.to_string())?;
        self.encoded_samples += count as u64;
        self.pending.clear();
        self.write_packets(output)
    }

    fn finish(&mut self, output: &mut ffmpeg::format::context::Output) -> Result<(), String> {
        if !self.pending.is_empty() {
            if self.pad_final_frame {
                self.pending.resize(self.chunk_samples * 2, 0.0);
            }
            self.encode(output)?;
        }
        self.encoder.send_eof().map_err(|error| error.to_string())?;
        self.write_packets(output)
    }

    fn write_packets(
        &mut self,
        output: &mut ffmpeg::format::context::Output,
    ) -> Result<(), String> {
        let base = output
            .stream(self.index)
            .ok_or("Audio stream missing")?
            .time_base();
        let mut packet = ffmpeg::Packet::empty();
        while self.encoder.receive_packet(&mut packet).is_ok() {
            packet.set_stream(self.index);
            packet.rescale_ts(self.encoder.time_base(), base);
            packet
                .write_interleaved(output)
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{ExportControl, render_with_settings};
    use crate::media::{Media, MediaKind};
    use crate::project::{Command, Editor, Project, format};
    use crate::timeline::{Clip, ClipContent, Title, Track, TrackKind};
    use ffmpeg_next as ffmpeg;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST_DIR: AtomicU64 = AtomicU64::new(0);

    fn export_test_base() -> PathBuf {
        std::env::var_os("OCUS_TEST_EXPORT_DIR").map_or_else(std::env::temp_dir, PathBuf::from)
    }

    #[test]
    fn export_uses_full_resolution_original_even_when_proxy_is_ready() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("original.mp4");
        assert!(
            std::process::Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-y",
                    "-f",
                    "lavfi",
                    "-i",
                    "color=red:s=2048x1152:r=30:d=0.1",
                    "-c:v",
                    "mpeg4",
                    "-threads",
                    "2"
                ])
                .arg(&source)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            crate::media::proxy::generate(&source, &std::sync::atomic::AtomicBool::new(false))
                .unwrap()
        );
        let proxy = crate::media::proxy::ready(&source).unwrap();
        assert!(
            std::process::Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-y",
                    "-f",
                    "lavfi",
                    "-i",
                    "color=blue:s=1280x720:r=30:d=0.1",
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
        let mut document: serde_json::Value =
            serde_json::from_str(&format::to_json(&Project::new("Original export")).unwrap())
                .unwrap();
        document["settings"]["width"] = 2048.into();
        document["settings"]["height"] = 1152.into();
        let mut editor = Editor::new(format::from_json(&document.to_string()).unwrap());
        assert!(editor.apply(Command::ImportMedia(Media {
            id: 1,
            path: source,
            kind: MediaKind::Video,
            duration_us: Some(100_000),
            has_audio: false
        })));
        assert!(editor.apply(Command::InsertClip {
            track_id: 0,
            clip: Clip {
                color: Default::default(),
                id: 1,
                start_frame: 0,
                source_start_frame: 0,
                duration_frames: 3,
                content: ClipContent::Media { media_id: 1 },
                volume: 1.0,
                muted: false,
                transition: None,
                fade_in_frames: 0,
                fade_out_frames: 0,
                linked_audio: false,
            }
        }));
        let target = directory.path().join("export.mp4");
        super::render(editor.project(), &target).unwrap();
        let input = ffmpeg::format::input(&target).unwrap();
        let video = input.streams().best(ffmpeg::media::Type::Video).unwrap();
        let decoder = ffmpeg::codec::context::Context::from_parameters(video.parameters())
            .unwrap()
            .decoder()
            .video()
            .unwrap();
        assert_eq!((decoder.width(), decoder.height()), (2048, 1152));
        let output = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(&target)
            .args([
                "-frames:v",
                "1",
                "-vf",
                "scale=1:1",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "rgb24",
                "-",
            ])
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(
            output.stdout[0] > 200 && output.stdout[2] < 40,
            "export used proxy instead of red original: {:?}",
            output.stdout
        );
        fs::remove_file(proxy).unwrap();
    }

    #[test]
    #[ignore = "manual real-export audio measurement; set OCUS_DOWNMIX_EVIDENCE"]
    fn exports_center_only_surround_audio() {
        let dir = std::path::PathBuf::from(std::env::var("OCUS_DOWNMIX_EVIDENCE").unwrap());
        fs::create_dir_all(&dir).unwrap();
        let source = dir.join("center.wav");
        assert!(
            std::process::Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-y",
                    "-f",
                    "lavfi",
                    "-i",
                    "aevalsrc=0|0|0.5*sin(2*PI*440*t)|0|0|0:s=48000:d=1:c=5.1",
                    "-c:a",
                    "pcm_f32le"
                ])
                .arg(&source)
                .status()
                .unwrap()
                .success()
        );
        let mut document: serde_json::Value =
            serde_json::from_str(&format::to_json(&Project::new("Downmix")).unwrap()).unwrap();
        document["settings"]["width"] = 160.into();
        document["settings"]["height"] = 90.into();
        let mut editor = Editor::new(format::from_json(&document.to_string()).unwrap());
        assert!(editor.apply(Command::ImportMedia(Media {
            id: 1,
            path: source,
            kind: MediaKind::Audio,
            duration_us: Some(1_000_000),
            has_audio: true,
        })));
        let track_id = editor
            .project()
            .tracks()
            .iter()
            .find(|track| track.kind == TrackKind::Audio)
            .unwrap()
            .id;
        assert!(editor.apply(Command::InsertClip {
            track_id,
            clip: Clip {
                id: 1,
                start_frame: 0,
                source_start_frame: 0,
                duration_frames: 30,
                content: ClipContent::Media { media_id: 1 },
                volume: 1.0,
                muted: false,
                color: Default::default(),
                transition: None,
                fade_in_frames: 0,
                fade_out_frames: 0,
                linked_audio: false,
            }
        }));
        let target = dir.join("export.mp4");
        super::render(editor.project(), &target).unwrap();
        let output = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-i"])
            .arg(&target)
            .args(["-vn", "-af", "volumedetect", "-f", "null", "-"])
            .output()
            .unwrap();
        assert!(output.status.success());
        println!("{}", String::from_utf8_lossy(&output.stderr));
    }

    #[test]
    fn output_frame_mapping_and_aspect_fit_are_bounded() {
        assert_eq!(super::output_frames(7, (30, 1), (24, 1)), Ok(6));
        assert_eq!(super::output_frames(7, (30, 1), (60, 1)), Ok(14));
        assert_eq!(
            super::fitted_dimensions(1920, 1080, 1080, 1920),
            (1080, 606)
        );
        assert_eq!(super::fitted_dimensions(1920, 1080, 1280, 720), (1280, 720));
        assert_eq!(
            super::fitted_dimensions(1080, 1920, 1920, 1080),
            (606, 1080)
        );
    }

    #[test]
    fn existing_destination_is_rejected_before_encoding() {
        let dir = std::env::temp_dir().join(format!(
            "ocus-export-collision-{}-{}",
            std::process::id(),
            NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        let target = dir.join("export.mp4");
        fs::write(&target, b"existing export").unwrap();
        let mut document: serde_json::Value =
            serde_json::from_str(&format::to_json(&Project::new("Collision")).unwrap()).unwrap();
        document["settings"]["width"] = 160.into();
        document["settings"]["height"] = 90.into();
        let mut editor = Editor::new(format::from_json(&document.to_string()).unwrap());
        assert!(editor.apply(Command::InsertClip {
            track_id: 0,
            clip: Clip {
                id: 1,
                start_frame: 0,
                source_start_frame: 0,
                duration_frames: 2,
                content: ClipContent::Title(Title::default()),
                volume: 1.0,
                muted: false,
                color: Default::default(),
                transition: None,
                fade_in_frames: 0,
                fade_out_frames: 0,
                linked_audio: false,
            },
        }));
        let control = Arc::new(ExportControl::default());
        let result = super::render_with_control(editor.project(), &target, control.clone());
        assert!(result.is_err());
        assert_eq!(fs::read(&target).unwrap(), b"existing export");
        assert_eq!(
            control.frames_done(),
            0,
            "must not encode an occupied destination"
        );
        assert_eq!(
            result.unwrap_err(),
            "Export destination already exists. Choose a different filename."
        );
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn destination_created_during_encoding_is_not_overwritten() {
        let dir = tempfile::tempdir_in(export_test_base()).unwrap();
        let target = dir.path().join("export.mp4");
        let result = super::write_export(&target, "mp4", |temporary| {
            fs::write(temporary, b"new export").unwrap();
            fs::write(&target, b"existing export").unwrap();
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(fs::read(&target).unwrap(), b"existing export");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn exports_a_video_clip_as_playable_h264_mp4() {
        let dir = export_test_base().join(format!("ocus-export-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source.y4m");
        let target = dir.join("result.mp4");
        let mut frames = b"YUV4MPEG2 W2 H2 F30:1 Ip A1:1 C420jpeg\n".to_vec();
        for frame in 0..30 {
            frames.extend(b"FRAME\n");
            let shade = if frame < 15 { 80 } else { 180 };
            frames.extend([shade, shade, shade, shade, 128, 128]);
        }
        fs::write(&source, frames).unwrap();
        let mut document: serde_json::Value =
            serde_json::from_str(&format::to_json(&Project::new("Export")).unwrap()).unwrap();
        document["settings"]["width"] = 160.into();
        document["settings"]["height"] = 90.into();
        let mut editor = Editor::new(format::from_json(&document.to_string()).unwrap());
        assert!(editor.apply(Command::ImportMedia(Media {
            id: 1,
            path: source.clone(),
            kind: MediaKind::Video,
            duration_us: Some(1_000_000),
            has_audio: false,
        })));
        assert!(editor.apply(Command::InsertClip {
            track_id: 0,
            clip: Clip {
                id: 1,
                start_frame: 0,
                source_start_frame: 0,
                duration_frames: 30,
                content: ClipContent::Media { media_id: 1 },
                volume: 1.0,
                muted: false,
                color: Default::default(),
                transition: None,
                fade_in_frames: 0,
                fade_out_frames: 0,
                linked_audio: false,
            },
        }));
        let progress = std::sync::Arc::new(super::ExportControl::default());
        super::render_with_control(editor.project(), &target, progress.clone()).unwrap_or_else(
            |error| {
                panic!(
                    "Export failed after {} frames: {error}",
                    progress.frames_done()
                )
            },
        );
        assert_eq!(progress.frames_done(), 30);
        let cancelled = std::sync::Arc::new(super::ExportControl::default());
        cancelled.cancel();
        let skipped = dir.join("cancelled.mp4");
        assert!(
            super::render_with_control(editor.project(), &skipped, cancelled)
                .unwrap_err()
                .contains("cancelled")
        );
        assert!(!skipped.exists());
        let mut missing_sound: serde_json::Value =
            serde_json::from_str(&format::to_json(editor.project()).unwrap()).unwrap();
        missing_sound["media"][0]["has_audio"] = true.into();
        missing_sound["tracks"][0]["clips"][0]["linked_audio"] = true.into();
        let missing_sound = format::from_json(&missing_sound.to_string()).unwrap();
        let incomplete = dir.join("missing-sound.mp4");
        assert!(
            super::render(&missing_sound, &incomplete)
                .unwrap_err()
                .contains("no audio stream")
        );
        assert!(!incomplete.exists());
        let output = ffmpeg::format::input(&target).unwrap();
        let video = output.streams().best(ffmpeg::media::Type::Video).unwrap();
        assert_eq!(video.parameters().id(), ffmpeg::codec::Id::H264);
        assert!(output.duration() > 0);
        assert!(editor.apply(Command::Split {
            track_id: 0,
            clip_id: 1,
            at_frame: 15,
            new_id: 2,
        }));
        let split = dir.join("split.mp4");
        super::render(editor.project(), &split).unwrap();
        let output = ffmpeg::format::input(&split).unwrap();
        assert_eq!(output.duration(), 1_000_000);
        let video = output.streams().best(ffmpeg::media::Type::Video).unwrap();
        let index = video.index();
        let mut decoder = ffmpeg::codec::context::Context::from_parameters(video.parameters())
            .unwrap()
            .decoder()
            .video()
            .unwrap();
        let mut output = output;
        let mut shades = Vec::new();
        for (stream, packet) in output.packets() {
            if stream.index() != index {
                continue;
            }
            decoder.send_packet(&packet).unwrap();
            let mut decoded = ffmpeg::frame::Video::empty();
            while decoder.receive_frame(&mut decoded).is_ok() {
                shades.push(decoded.data(0)[decoded.stride(0) * 45 + 80]);
            }
        }
        decoder.send_eof().unwrap();
        let mut decoded = ffmpeg::frame::Video::empty();
        while decoder.receive_frame(&mut decoded).is_ok() {
            shades.push(decoded.data(0)[decoded.stride(0) * 45 + 80]);
        }
        assert_eq!(shades.len(), 30);
        assert!(shades[0] < 120 && shades[29] > 140);
        let audio_source = dir.join("bed.wav");
        let samples = vec![6_000_i16; 72_000 * 2];
        let mut wav = b"RIFF".to_vec();
        wav.extend(((samples.len() * 2 + 36) as u32).to_le_bytes());
        wav.extend(b"WAVEfmt ");
        wav.extend(16_u32.to_le_bytes());
        wav.extend(1_u16.to_le_bytes());
        wav.extend(2_u16.to_le_bytes());
        wav.extend(48_000_u32.to_le_bytes());
        wav.extend(192_000_u32.to_le_bytes());
        wav.extend(4_u16.to_le_bytes());
        wav.extend(16_u16.to_le_bytes());
        wav.extend(b"data");
        wav.extend(((samples.len() * 2) as u32).to_le_bytes());
        for sample in samples {
            wav.extend(sample.to_le_bytes());
        }
        fs::write(&audio_source, wav).unwrap();
        assert!(editor.apply(Command::ImportMedia(Media {
            id: 2,
            path: audio_source.clone(),
            kind: MediaKind::Audio,
            duration_us: Some(1_500_000),
            has_audio: true,
        })));
        assert!(editor.apply(Command::Move {
            track_id: 0,
            target_track_id: 0,
            clip_id: 2,
            to_frame: 30,
        }));
        assert!(editor.apply(Command::InsertClip {
            track_id: 2,
            clip: Clip {
                id: 3,
                start_frame: 0,
                source_start_frame: 0,
                duration_frames: 45,
                content: ClipContent::Media { media_id: 2 },
                volume: 0.5,
                muted: false,
                color: Default::default(),
                transition: None,
                fade_in_frames: 0,
                fade_out_frames: 0,
                linked_audio: false,
            },
        }));
        let with_audio = dir.join("audio.mp4");
        assert!(super::can_render(editor.project()));
        let control = std::sync::Arc::new(super::ExportControl::default());
        super::render_with_control(editor.project(), &with_audio, control.clone()).unwrap();
        assert_eq!(control.frames_done(), 45);
        let output = ffmpeg::format::input(&with_audio).unwrap();
        assert!((1_500_000..1_530_000).contains(&output.duration()));
        let sound = output.streams().best(ffmpeg::media::Type::Audio).unwrap();
        assert_eq!(sound.parameters().id(), ffmpeg::codec::Id::AAC);
        let sound_index = sound.index();
        let mut decoder = ffmpeg::codec::context::Context::from_parameters(sound.parameters())
            .unwrap()
            .decoder()
            .audio()
            .unwrap();
        let mut output = output;
        let mut loud = false;
        for (stream, packet) in output.packets() {
            if stream.index() == sound_index {
                decoder.send_packet(&packet).unwrap();
                let mut frame = ffmpeg::frame::Audio::empty();
                while decoder.receive_frame(&mut frame).is_ok() {
                    loud |= frame
                        .plane::<f32>(0)
                        .iter()
                        .any(|sample| sample.abs() > 0.03);
                }
            }
        }
        assert!(loud);
        let mut audio_only = editor.project().tracks().to_vec();
        audio_only[0].clips.clear();
        assert!(editor.apply(Command::ReplaceTracks(audio_only)));
        assert!(super::can_render(editor.project()));
        let audio_only_target = dir.join("audio-only.mp4");
        super::render(editor.project(), &audio_only_target).unwrap();
        let output = ffmpeg::format::input(&audio_only_target).unwrap();
        assert!(output.streams().best(ffmpeg::media::Type::Audio).is_some());
        assert!(output.streams().best(ffmpeg::media::Type::Video).is_some());
        let lower_source = dir.join("lower.ppm");
        fs::write(
            &lower_source,
            b"P6\n2 2\n255\n\xff\x00\x00\xff\x00\x00\xff\x00\x00\xff\x00\x00",
        )
        .unwrap();
        assert!(editor.apply(Command::ImportMedia(Media {
            id: 3,
            path: lower_source.clone(),
            kind: MediaKind::Image,
            duration_us: None,
            has_audio: false,
        })));
        let mut layered = editor.project().tracks().to_vec();
        layered[0].clips = vec![Clip {
            id: 4,
            start_frame: 15,
            source_start_frame: 15,
            duration_frames: 15,
            content: ClipContent::Media { media_id: 1 },
            volume: 1.0,
            muted: false,
            color: Default::default(),
            transition: None,
            fade_in_frames: 0,
            fade_out_frames: 0,
            linked_audio: false,
        }];
        layered[1].clips = vec![Clip {
            id: 5,
            start_frame: 0,
            source_start_frame: 0,
            duration_frames: 20,
            content: ClipContent::Media { media_id: 3 },
            volume: 1.0,
            muted: false,
            color: Default::default(),
            transition: None,
            fade_in_frames: 0,
            fade_out_frames: 0,
            linked_audio: false,
        }];
        layered.push(Track {
            id: 4,
            kind: TrackKind::Video,
            name: String::new(),
            locked: false,
            hidden: false,
            clips: vec![Clip {
                id: 6,
                start_frame: 0,
                source_start_frame: 0,
                duration_frames: 30,
                content: ClipContent::Title(Title {
                    text: "<color=#00ff00>X</color>".into(),
                    font_size: 42.0,
                    parse_html: true,
                    color: [255, 255, 255, 255],
                    ..Title::default()
                }),
                volume: 1.0,
                muted: false,
                color: Default::default(),
                transition: None,
                fade_in_frames: 0,
                fade_out_frames: 0,
                linked_audio: false,
            }],
        });
        assert!(editor.apply(Command::ReplaceTracks(layered)));
        assert!(super::can_render(editor.project()));
        let layered_target = dir.join("layered.mp4");
        super::render(editor.project(), &layered_target).unwrap();
        let output = ffmpeg::format::input(&layered_target).unwrap();
        let stream = output.streams().best(ffmpeg::media::Type::Video).unwrap();
        let index = stream.index();
        let mut decoder = ffmpeg::codec::context::Context::from_parameters(stream.parameters())
            .unwrap()
            .decoder()
            .video()
            .unwrap();
        let mut output = output;
        let mut shades = Vec::new();
        let mut title_pixels = 0;
        for (stream, packet) in output.packets() {
            if stream.index() != index {
                continue;
            }
            decoder.send_packet(&packet).unwrap();
            let mut frame = ffmpeg::frame::Video::empty();
            while decoder.receive_frame(&mut frame).is_ok() {
                shades.push(frame.data(0)[frame.stride(0) * 10 + 80]);
                if shades.len() == 1 {
                    title_pixels = frame.data(0).iter().filter(|&&value| value > 120).count();
                }
            }
        }
        decoder.send_eof().unwrap();
        let mut frame = ffmpeg::frame::Video::empty();
        while decoder.receive_frame(&mut frame).is_ok() {
            shades.push(frame.data(0)[frame.stride(0) * 10 + 80]);
        }
        assert_eq!(shades.len(), 45);
        assert!(
            shades[0] < 100 && shades[15] < 100 && shades[21] > 120 && shades[30] < 30,
            "{:?}",
            &shades[..31]
        );
        assert!(title_pixels > 10, "Title did not appear in encoded video");
        assert!(editor.undo());
        let same_source = dir.join("imported.mp4");
        fs::write(&same_source, b"original source content").unwrap();
        let mut changed: serde_json::Value =
            serde_json::from_str(&format::to_json(editor.project()).unwrap()).unwrap();
        changed["media"][0]["path"] = same_source.to_string_lossy().to_string().into();
        let changed = format::from_json(&changed.to_string()).unwrap();
        assert!(
            super::render(&changed, &same_source)
                .unwrap_err()
                .contains("imported source")
        );
        assert_eq!(fs::read(&same_source).unwrap(), b"original source content");
        fs::remove_file(same_source).unwrap();
        fs::remove_file(source).unwrap();
        fs::remove_file(target).unwrap();
        fs::remove_file(split).unwrap();
        fs::remove_file(audio_source).unwrap();
        fs::remove_file(with_audio).unwrap();
        fs::remove_file(audio_only_target).unwrap();
        fs::remove_file(lower_source).unwrap();
        fs::remove_file(layered_target).unwrap();
        fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn still_image_exports_as_a_timed_video() {
        let dir = std::env::temp_dir().join(format!(
            "ocus-image-export-test-{}-{}",
            std::process::id(),
            NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        let source = dir.join("still.ppm");
        let target = dir.join("still.mp4");
        fs::write(
            &source,
            b"P6\n2 2\n255\n\xff\x00\x00\xff\x00\x00\xff\x00\x00\xff\x00\x00",
        )
        .unwrap();
        let mut document: serde_json::Value =
            serde_json::from_str(&format::to_json(&Project::new("Still")).unwrap()).unwrap();
        document["settings"]["width"] = 160.into();
        document["settings"]["height"] = 90.into();
        let mut editor = Editor::new(format::from_json(&document.to_string()).unwrap());
        assert!(editor.apply(Command::ImportMedia(Media {
            id: 1,
            path: source.clone(),
            kind: MediaKind::Image,
            duration_us: None,
            has_audio: false,
        })));
        assert!(editor.apply(Command::InsertClip {
            track_id: 0,
            clip: Clip {
                id: 1,
                start_frame: 0,
                source_start_frame: 0,
                duration_frames: 30,
                content: ClipContent::Media { media_id: 1 },
                volume: 1.0,
                muted: false,
                color: Default::default(),
                transition: None,
                fade_in_frames: 0,
                fade_out_frames: 0,
                linked_audio: false,
            },
        }));
        super::render(editor.project(), &target).unwrap();
        assert_eq!(
            ffmpeg::format::input(&target).unwrap().duration(),
            1_000_000
        );
        let portrait = dir.join("portrait.mp4");
        let settings = crate::project::export_settings::ExportSettings::for_project(
            96,
            160,
            editor.project().settings().frame_rate.clone(),
        );
        render_with_settings(
            editor.project(),
            &portrait,
            &settings,
            Arc::new(ExportControl::default()),
        )
        .unwrap();
        let mut output = ffmpeg::format::input(&portrait).unwrap();
        let stream = output.streams().best(ffmpeg::media::Type::Video).unwrap();
        let video_index = stream.index();
        let mut decoder = ffmpeg::codec::context::Context::from_parameters(stream.parameters())
            .unwrap()
            .decoder()
            .video()
            .unwrap();
        let mut frame = ffmpeg::frame::Video::empty();
        let mut decoded = false;
        for (stream, packet) in output.packets() {
            if stream.index() == video_index {
                decoder.send_packet(&packet).unwrap();
                if decoder.receive_frame(&mut frame).is_ok() {
                    decoded = true;
                    break;
                }
            }
        }
        assert!(decoded);
        let luma = frame.data(0);
        assert!(luma[frame.stride(0) * 10 + 48] < 30);
        assert!(luma[frame.stride(0) * 80 + 48] > 50);
        fs::remove_file(source).unwrap();
        fs::remove_file(target).unwrap();
        fs::remove_file(portrait).unwrap();
        fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn title_only_sequence_exports_visible_text_over_black() {
        let dir = std::env::temp_dir().join(format!(
            "ocus-title-export-{}-{}",
            std::process::id(),
            NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        let target = dir.join("title.mp4");
        let mut document: serde_json::Value =
            serde_json::from_str(&format::to_json(&Project::new("Title")).unwrap()).unwrap();
        document["settings"]["width"] = 160.into();
        document["settings"]["height"] = 90.into();
        let mut editor = Editor::new(format::from_json(&document.to_string()).unwrap());
        assert!(editor.apply(Command::InsertClip {
            track_id: 0,
            clip: Clip {
                id: 1,
                start_frame: 0,
                source_start_frame: 0,
                duration_frames: 15,
                content: ClipContent::Title(Title {
                    text: "**Hi**".into(),
                    parse_markdown: true,
                    font_size: 42.0,
                    outline: true,
                    ..Title::default()
                }),
                volume: 1.0,
                muted: false,
                color: Default::default(),
                transition: None,
                fade_in_frames: 0,
                fade_out_frames: 0,
                linked_audio: false,
            },
        }));
        super::render(editor.project(), &target).unwrap();
        let output = ffmpeg::format::input(&target).unwrap();
        let video = output.streams().best(ffmpeg::media::Type::Video).unwrap();
        let index = video.index();
        let mut decoder = ffmpeg::codec::context::Context::from_parameters(video.parameters())
            .unwrap()
            .decoder()
            .video()
            .unwrap();
        let mut output = output;
        let mut bright = false;
        for (stream, packet) in output.packets() {
            if stream.index() != index {
                continue;
            }
            decoder.send_packet(&packet).unwrap();
            let mut frame = ffmpeg::frame::Video::empty();
            while decoder.receive_frame(&mut frame).is_ok() {
                bright |= frame.data(0).iter().any(|&pixel| pixel > 180);
            }
        }
        assert!(bright);
        let target_gif = dir.join("title.gif");
        let mut settings = super::ExportSettings::for_project(
            160,
            90,
            crate::project::FrameRate {
                numerator: 15,
                denominator: 1,
            },
        );
        settings.container = crate::project::export_settings::Container::Gif;
        let control = Arc::new(ExportControl::default());
        render_with_settings(editor.project(), &target_gif, &settings, control.clone()).unwrap();
        let mut gif = ffmpeg::format::input(&target_gif).unwrap();
        assert_eq!(gif.streams().len(), 1);
        let stream = gif.streams().best(ffmpeg::media::Type::Video).unwrap();
        assert_eq!(stream.parameters().id(), ffmpeg::codec::Id::GIF);
        let mut decoder = ffmpeg::codec::context::Context::from_parameters(stream.parameters())
            .unwrap()
            .decoder()
            .video()
            .unwrap();
        assert_eq!((decoder.width(), decoder.height()), (160, 90));
        let mut frames = 0;
        let mut bright = false;
        for (_, packet) in gif.packets() {
            decoder.send_packet(&packet).unwrap();
            let mut frame = ffmpeg::frame::Video::empty();
            while decoder.receive_frame(&mut frame).is_ok() {
                frames += 1;
                bright |= frame.data(0).as_chunks::<4>().0.iter().any(|p| p[0] > 180);
            }
        }
        assert_eq!(frames, 8);
        assert_eq!(control.frames_done(), 8);
        assert!(bright);
        fs::remove_file(target_gif).unwrap();
        fs::remove_file(target).unwrap();
        fs::remove_dir(dir).unwrap();
    }

    fn decoded_stereo(path: &std::path::Path) -> Vec<(f32, f32)> {
        let mut input = ffmpeg::format::input(path).unwrap();
        let stream = input.streams().best(ffmpeg::media::Type::Audio).unwrap();
        let index = stream.index();
        let mut decoder = ffmpeg::codec::context::Context::from_parameters(stream.parameters())
            .unwrap()
            .decoder()
            .audio()
            .unwrap();
        let mut samples = Vec::new();
        let mut collect = |decoder: &mut ffmpeg::decoder::Audio| {
            let mut frame = ffmpeg::frame::Audio::empty();
            while decoder.receive_frame(&mut frame).is_ok() {
                assert_eq!(frame.channels(), 2);
                if frame.is_planar() {
                    let (left, right) = (frame.plane::<f32>(0), frame.plane::<f32>(1));
                    samples.extend(left.iter().copied().zip(right.iter().copied()));
                } else {
                    samples.extend_from_slice(frame.plane::<(f32, f32)>(0));
                }
            }
        };
        for (packet_stream, packet) in input.packets() {
            if packet_stream.index() == index {
                decoder.send_packet(&packet).unwrap();
                collect(&mut decoder);
            }
        }
        decoder.send_eof().unwrap();
        collect(&mut decoder);
        samples
    }

    #[test]
    fn opus_export_keeps_every_stereo_sample_like_aac() {
        use crate::project::export_settings::{AudioCodec, Container, ExportSettings, VideoCodec};
        ffmpeg::init().unwrap();
        if ffmpeg::encoder::find(ffmpeg::codec::Id::VP9).is_none()
            || ffmpeg::encoder::find(ffmpeg::codec::Id::OPUS).is_none()
        {
            return;
        }
        let dir = export_test_base().join(format!(
            "ocus-opus-stereo-{}-{}",
            std::process::id(),
            NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        let source = dir.join("tone.wav");
        let rate = 48_000_u32;
        let mut wav = b"RIFF".to_vec();
        wav.extend((rate * 4 + 36).to_le_bytes());
        wav.extend(b"WAVEfmt ");
        wav.extend(16_u32.to_le_bytes());
        wav.extend(1_u16.to_le_bytes());
        wav.extend(2_u16.to_le_bytes());
        wav.extend(rate.to_le_bytes());
        wav.extend((rate * 4).to_le_bytes());
        wav.extend(4_u16.to_le_bytes());
        wav.extend(16_u16.to_le_bytes());
        wav.extend(b"data");
        wav.extend((rate * 4).to_le_bytes());
        for n in 0..rate {
            let t = f64::from(n) / f64::from(rate);
            for hz in [440.0, 660.0] {
                let sample = 8_000.0 * (std::f64::consts::TAU * hz * t).sin();
                wav.extend((sample.round() as i16).to_le_bytes());
            }
        }
        fs::write(&source, wav).unwrap();
        let mut editor = Editor::new(Project::new("Opus stereo"));
        assert!(editor.apply(Command::ImportMedia(Media {
            id: 1,
            path: source,
            kind: MediaKind::Audio,
            duration_us: Some(1_000_000),
            has_audio: true,
        })));
        let track_id = editor
            .project()
            .tracks()
            .iter()
            .find(|track| track.kind == TrackKind::Audio)
            .unwrap()
            .id;
        assert!(editor.apply(Command::InsertClip {
            track_id,
            clip: Clip {
                id: 1,
                start_frame: 0,
                source_start_frame: 0,
                duration_frames: 30,
                content: ClipContent::Media { media_id: 1 },
                volume: 1.0,
                muted: false,
                color: Default::default(),
                transition: None,
                fade_in_frames: 0,
                fade_out_frames: 0,
                linked_audio: false,
            },
        }));
        let mut settings = ExportSettings::for_project(
            96,
            54,
            crate::project::FrameRate {
                numerator: 24,
                denominator: 1,
            },
        );
        let expected_rms = 8_000.0 / 32_768.0 / std::f32::consts::SQRT_2;
        for (container, video_codec, audio_codec, name) in [
            (
                Container::Matroska,
                VideoCodec::H264,
                AudioCodec::Aac,
                "aac.mkv",
            ),
            (
                Container::WebM,
                VideoCodec::Vp9,
                AudioCodec::Opus,
                "opus.webm",
            ),
            (Container::Wav, VideoCodec::H264, AudioCodec::Aac, "mix.wav"),
        ] {
            settings.container = container;
            settings.video_codec = video_codec;
            settings.audio_codec = audio_codec;
            let target = dir.join(name);
            render_with_settings(
                editor.project(),
                &target,
                &settings,
                Arc::new(ExportControl::default()),
            )
            .unwrap();
            let samples = decoded_stereo(&target);
            if container == Container::Wav {
                let output = ffmpeg::format::input(&target).unwrap();
                assert_eq!(output.streams().len(), 1);
                let stream = output.streams().best(ffmpeg::media::Type::Audio).unwrap();
                assert_eq!(stream.parameters().id(), ffmpeg::codec::Id::PCM_F32LE);
                assert_eq!(samples.len(), 48_000);
            }
            assert!(samples.len() >= 47_000, "{name}: {} samples", samples.len());
            for (start, window) in samples[4_800..43_200]
                .as_chunks::<480>()
                .0
                .iter()
                .enumerate()
            {
                let rms = |channel: fn(&(f32, f32)) -> f32| {
                    (window.iter().map(|s| channel(s).powi(2)).sum::<f32>() / window.len() as f32)
                        .sqrt()
                };
                for (label, rms) in [("left", rms(|s| s.0)), ("right", rms(|s| s.1))] {
                    assert!(
                        (rms / expected_rms - 1.0).abs() < 0.2,
                        "{name} {label} RMS {rms:.4} vs {expected_rms:.4} in window {start}"
                    );
                }
            }
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn custom_frame_rate_container_and_opus_audio_export_correctly() {
        use crate::project::export_settings::{
            AudioCodec, Container, EncodingSpeed, ExportSettings, VideoCodec,
        };
        ffmpeg::init().unwrap();
        if ffmpeg::encoder::find(ffmpeg::codec::Id::VP9).is_none()
            || ffmpeg::encoder::find(ffmpeg::codec::Id::OPUS).is_none()
        {
            return;
        }
        let dir = std::env::temp_dir().join(format!(
            "ocus-custom-export-{}-{}",
            std::process::id(),
            NEXT_TEST_DIR.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        let wav_path = dir.join("audio.wav");
        let mut wav = b"RIFF".to_vec();
        wav.extend((48_000 + 36_u32).to_le_bytes());
        wav.extend(b"WAVEfmt ");
        wav.extend(16_u32.to_le_bytes());
        wav.extend(1_u16.to_le_bytes());
        wav.extend(2_u16.to_le_bytes());
        wav.extend(48_000_u32.to_le_bytes());
        wav.extend(192_000_u32.to_le_bytes());
        wav.extend(4_u16.to_le_bytes());
        wav.extend(16_u16.to_le_bytes());
        wav.extend(b"data");
        wav.extend(48_000_u32.to_le_bytes());
        for _ in 0..12_000 {
            wav.extend(5_000_i16.to_le_bytes());
            wav.extend((-5_000_i16).to_le_bytes());
        }
        fs::write(&wav_path, wav).unwrap();
        let mut editor = Editor::new(Project::new("Custom"));
        assert!(editor.apply(Command::ImportMedia(Media {
            id: 1,
            path: wav_path.clone(),
            kind: MediaKind::Audio,
            duration_us: Some(250_000),
            has_audio: true,
        })));
        assert!(editor.apply(Command::InsertClip {
            track_id: 2,
            clip: Clip {
                id: 1,
                start_frame: 0,
                source_start_frame: 0,
                duration_frames: 7,
                content: ClipContent::Media { media_id: 1 },
                volume: 1.0,
                muted: false,
                color: Default::default(),
                transition: None,
                fade_in_frames: 0,
                fade_out_frames: 0,
                linked_audio: false,
            },
        }));
        let mut settings = ExportSettings::for_project(
            96,
            54,
            crate::project::FrameRate {
                numerator: 24,
                denominator: 1,
            },
        );
        settings.container = Container::WebM;
        settings.video_codec = VideoCodec::Vp9;
        settings.audio_codec = AudioCodec::Opus;
        settings.video_quality = Some(32);
        settings.speed = EncodingSpeed::Balanced;
        let output = dir.join("output.webm");
        let control = Arc::new(ExportControl::default());
        render_with_settings(editor.project(), &output, &settings, control.clone()).unwrap();
        assert_eq!(control.frames_done(), 6);
        let input = ffmpeg::format::input(&output).unwrap();
        let video = input.streams().best(ffmpeg::media::Type::Video).unwrap();
        assert_eq!(video.parameters().id(), ffmpeg::codec::Id::VP9);
        let decoder = ffmpeg::codec::context::Context::from_parameters(video.parameters())
            .unwrap()
            .decoder()
            .video()
            .unwrap();
        assert_eq!((decoder.width(), decoder.height()), (96, 54));
        assert_eq!(
            input
                .streams()
                .best(ffmpeg::media::Type::Audio)
                .unwrap()
                .parameters()
                .id(),
            ffmpeg::codec::Id::OPUS
        );
        let audio = input.streams().best(ffmpeg::media::Type::Audio).unwrap();
        let audio_index = audio.index();
        let mut decoder = ffmpeg::codec::context::Context::from_parameters(audio.parameters())
            .unwrap()
            .decoder()
            .audio()
            .unwrap();
        let mut decoded_sound = false;
        let mut input = input;
        for (stream, packet) in input.packets() {
            if stream.index() != audio_index {
                continue;
            }
            decoder.send_packet(&packet).unwrap();
            let mut frame = ffmpeg::frame::Audio::empty();
            while decoder.receive_frame(&mut frame).is_ok() {
                decoded_sound |= frame.data(0).iter().any(|sample| *sample != 0);
            }
        }
        assert!(decoded_sound);
        let original = fs::read(&output).unwrap();
        assert!(
            render_with_settings(
                editor.project(),
                &output,
                &settings,
                Arc::new(ExportControl::default())
            )
            .is_err()
        );
        assert_eq!(fs::read(&output).unwrap(), original);
        settings.container = Container::Matroska;
        settings.video_codec = VideoCodec::H264;
        settings.audio_codec = AudioCodec::Aac;
        settings.video_quality = None;
        let mkv = dir.join("output.mkv");
        render_with_settings(
            editor.project(),
            &mkv,
            &settings,
            Arc::new(ExportControl::default()),
        )
        .unwrap();
        let input = ffmpeg::format::input(&mkv).unwrap();
        assert_eq!(
            input
                .streams()
                .best(ffmpeg::media::Type::Audio)
                .unwrap()
                .parameters()
                .id(),
            ffmpeg::codec::Id::AAC
        );
        assert_eq!(
            input
                .streams()
                .best(ffmpeg::media::Type::Video)
                .unwrap()
                .parameters()
                .id(),
            ffmpeg::codec::Id::H264
        );
        settings.frame_rate = crate::project::FrameRate {
            numerator: 60,
            denominator: 1,
        };
        let high_fps = dir.join("high-fps.mkv");
        let high_fps_control = Arc::new(ExportControl::default());
        render_with_settings(
            editor.project(),
            &high_fps,
            &settings,
            high_fps_control.clone(),
        )
        .unwrap();
        assert_eq!(high_fps_control.frames_done(), 14);
        assert!(
            ffmpeg::format::input(&high_fps)
                .unwrap()
                .streams()
                .best(ffmpeg::media::Type::Audio)
                .is_some()
        );
        settings.frame_rate = crate::project::FrameRate {
            numerator: 24,
            denominator: 1,
        };
        if ffmpeg::encoder::find(ffmpeg::codec::Id::HEVC).is_some() {
            settings.container = Container::Mp4;
            settings.video_codec = VideoCodec::Hevc;
            settings.video_quality = Some(24);
            let hevc = dir.join("output-hevc.mp4");
            render_with_settings(
                editor.project(),
                &hevc,
                &settings,
                Arc::new(ExportControl::default()),
            )
            .unwrap();
            assert_eq!(
                ffmpeg::format::input(&hevc)
                    .unwrap()
                    .streams()
                    .best(ffmpeg::media::Type::Video)
                    .unwrap()
                    .parameters()
                    .id(),
                ffmpeg::codec::Id::HEVC
            );
            fs::remove_file(hevc).unwrap();
        }
        fs::remove_file(wav_path).unwrap();
        fs::remove_file(output).unwrap();
        fs::remove_file(mkv).unwrap();
        fs::remove_file(high_fps).unwrap();
        fs::remove_dir(dir).unwrap();
    }
}
