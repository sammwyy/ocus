use crate::media::MediaKind;
use crate::media::audio::{self, AudioPlan, AudioQueue, AudioSource};
use crate::media::composite::{Compositor, VisualPlan};
use crate::project::Project;
use crate::timeline::{ClipContent, TrackKind};
use ffmpeg_next as ffmpeg;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;

static NEXT_EXPORT: AtomicU64 = AtomicU64::new(0);

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
    control.check()?;
    if !can_render(project) {
        return Err("Nothing to export".into());
    }
    project.validate()?;
    if !path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("mp4"))
    {
        return Err("Export filename must end in .mp4".into());
    }
    if path.exists() {
        let destination = fs::canonicalize(path).map_err(|error| error.to_string())?;
        for media in project.media() {
            if fs::canonicalize(&media.path).is_ok_and(|source| destination == source) {
                return Err("Export destination cannot be an imported source".into());
            }
        }
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .ok_or("Missing export filename")?
        .to_string_lossy();
    let mut temporary: Option<PathBuf> = None;
    for _ in 0..8 {
        let candidate = parent.join(format!(
            ".{name}.{}.{}.mp4",
            std::process::id(),
            NEXT_EXPORT.fetch_add(1, Ordering::Relaxed)
        ));
        match OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&candidate)
        {
            Ok(file) => {
                drop(file);
                temporary = Some(candidate);
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    let temporary = temporary.ok_or("Cannot create temporary export file")?;
    let result = render_to(project, &temporary, control.clone()).and_then(|()| {
        control.check()?;
        fs::rename(&temporary, path).map_err(|error| error.to_string())
    });
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn render_to(project: &Project, path: &Path, control: Arc<ExportControl>) -> Result<(), String> {
    ffmpeg::init().map_err(|error| error.to_string())?;
    let settings = project.settings();
    if !settings.width.is_multiple_of(2)
        || !settings.height.is_multiple_of(2)
        || settings.width > 8192
        || settings.height > 8192
    {
        return Err("H.264 export requires even dimensions no greater than 8192".into());
    }
    let fps = ffmpeg::Rational(
        i32::try_from(settings.frame_rate.numerator)
            .map_err(|_| "Export frame rate is out of range")?,
        i32::try_from(settings.frame_rate.denominator)
            .map_err(|_| "Export frame rate is out of range")?,
    );
    let time_base = ffmpeg::Rational(fps.denominator(), fps.numerator());
    let codec =
        ffmpeg::encoder::find(ffmpeg::codec::Id::H264).ok_or("H.264 encoder not available")?;
    let mut output = ffmpeg::format::output(path).map_err(|error| error.to_string())?;
    let mut encoder = ffmpeg::codec::context::Context::new_with_codec(codec)
        .encoder()
        .video()
        .map_err(|error| error.to_string())?;
    encoder.set_width(settings.width);
    encoder.set_height(settings.height);
    encoder.set_format(ffmpeg::format::Pixel::YUV420P);
    encoder.set_time_base(time_base);
    encoder.set_frame_rate(Some(fps));
    if output
        .format()
        .flags()
        .contains(ffmpeg::format::Flags::GLOBAL_HEADER)
    {
        encoder.set_flags(ffmpeg::codec::Flags::GLOBAL_HEADER);
    }
    let mut options = ffmpeg::Dictionary::new();
    options.set("preset", "veryfast");
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
    let end = project
        .tracks()
        .iter()
        .flat_map(|track| &track.clips)
        .map(|clip| clip.start_frame + clip.duration_frames)
        .max()
        .ok_or("Nothing to export")?;
    let fps_pair = (
        settings.frame_rate.numerator,
        settings.frame_rate.denominator,
    );
    let sources: Vec<_> = project
        .tracks()
        .iter()
        .flat_map(|track| {
            track.clips.iter().filter_map(|clip| {
                let ClipContent::Media { media_id } = clip.content else {
                    return None;
                };
                let media = project.media().iter().find(|media| media.id == media_id)?;
                let audible = !clip.muted
                    && match track.kind {
                        TrackKind::Video => clip.linked_audio && media.has_audio,
                        TrackKind::Audio => media.kind == MediaKind::Audio || media.has_audio,
                    };
                audible.then(|| AudioSource {
                    path: media.path.clone(),
                    start: clip.start_frame,
                    source_start: clip.source_start_frame,
                    duration: clip.duration_frames,
                    volume: clip.volume,
                })
            })
        })
        .collect();
    for source in &sources {
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
    let audio_encoder = if sources.is_empty() {
        None
    } else {
        let codec =
            ffmpeg::encoder::find(ffmpeg::codec::Id::AAC).ok_or("AAC encoder not available")?;
        let formats = codec.audio().map_err(|error| error.to_string())?;
        let format = ffmpeg::format::Sample::F32(ffmpeg::format::sample::Type::Planar);
        if !formats
            .formats()
            .is_some_and(|mut formats| formats.any(|f| f == format))
            || !formats
                .rates()
                .is_some_and(|mut rates| rates.any(|r| r == audio::RATE as i32))
        {
            return Err("AAC encoder cannot accept 48 kHz planar float stereo".into());
        }
        let mut encoder = ffmpeg::codec::context::Context::new_with_codec(codec)
            .encoder()
            .audio()
            .map_err(|error| error.to_string())?;
        encoder.set_rate(audio::RATE as i32);
        encoder.set_channel_layout(ffmpeg::ChannelLayout::STEREO);
        encoder.set_format(format);
        encoder.set_time_base(ffmpeg::Rational(1, audio::RATE as i32));
        encoder.set_bit_rate(192_000);
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
        stream.set_time_base(ffmpeg::Rational(1, audio::RATE as i32));
        stream.set_parameters(&encoder);
        Some((encoder, stream.index()))
    };
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
        audio: audio_encoder.map(|(encoder, index)| AudioEncoder {
            encoder,
            index,
            next_sample: 0,
            encoded_samples: 0,
            pending: Vec::new(),
        }),
        queue: None,
        fps: fps_pair,
    };
    let queue = (!sources.is_empty()).then(|| Arc::new(AudioQueue::new()));
    muxer.queue = queue.clone();
    let cancelled = Arc::new(AtomicBool::new(false));
    thread::scope(|scope| {
        let worker = queue.as_ref().map(|queue| {
            let queue = queue.clone();
            let cancelled = cancelled.clone();
            scope.spawn(move || {
                audio::feed(
                    &AudioPlan {
                        sources,
                        end,
                        fps: fps_pair,
                    },
                    0,
                    queue,
                    cancelled,
                )
            })
        });
        let result = (|| {
            let mut compositor = Compositor::new(
                VisualPlan::from_project(project),
                settings.width,
                settings.height,
                fps_pair,
            );
            let mut rgba = ffmpeg::frame::Video::new(
                ffmpeg::format::Pixel::RGBA,
                settings.width,
                settings.height,
            );
            let mut scaler = ffmpeg::software::scaling::context::Context::get(
                ffmpeg::format::Pixel::RGBA,
                settings.width,
                settings.height,
                ffmpeg::format::Pixel::YUV420P,
                settings.width,
                settings.height,
                ffmpeg::software::scaling::flag::Flags::BILINEAR,
            )
            .map_err(|error| error.to_string())?;
            for position in 0..end {
                muxer.control.check()?;
                let image = compositor.frame(position, &muxer.control.cancelled)?;
                for row in 0..settings.height as usize {
                    let stride = rgba.stride(0);
                    let dest = &mut rgba.data_mut(0)
                        [row * stride..row * stride + settings.width as usize * 4];
                    if let Some(image) = &image {
                        let source = &image.pixels[row * settings.width as usize * 4
                            ..(row + 1) * settings.width as usize * 4];
                        dest.copy_from_slice(source);
                    } else {
                        for pixel in dest.chunks_exact_mut(4) {
                            pixel.copy_from_slice(&[0, 0, 0, 255]);
                        }
                    }
                }
                let mut frame = ffmpeg::frame::Video::empty();
                scaler
                    .run(&rgba, &mut frame)
                    .map_err(|error| error.to_string())?;
                muxer.write_frame(&frame, position)?;
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
        })();
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

struct Muxer {
    encoder: ffmpeg::encoder::video::Encoder,
    output: ffmpeg::format::context::Output,
    stream_index: usize,
    output_base: ffmpeg::Rational,
    control: Arc<ExportControl>,
    audio: Option<AudioEncoder>,
    queue: Option<Arc<AudioQueue>>,
    fps: (u32, u32),
}

impl Muxer {
    fn write_frame(&mut self, frame: &ffmpeg::frame::Video, position: u64) -> Result<(), String> {
        self.control.check()?;
        let mut frame = frame.clone();
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
                    / u128::from(self.fps.0)) as u64;
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
}

impl AudioEncoder {
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
                if self.pending.len() == self.encoder.frame_size() as usize * 2 {
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
        for channel in 0..2 {
            for (dst, sample) in frame
                .plane_mut::<f32>(channel)
                .iter_mut()
                .zip(self.pending.chunks_exact(2))
            {
                *dst = sample[channel];
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
            let count = self.encoder.frame_size() as usize * 2;
            self.pending.resize(count, 0.0);
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
    use crate::media::{Media, MediaKind};
    use crate::project::{Command, Editor, Project, format};
    use crate::timeline::{Clip, ClipContent, Title, Track, TrackKind};
    use ffmpeg_next as ffmpeg;
    use std::fs;
    use std::sync::atomic::Ordering;

    #[test]
    fn exports_a_video_clip_as_playable_h264_mp4() {
        let dir = std::env::temp_dir().join(format!("ocus-export-test-{}", std::process::id()));
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
                linked_audio: false,
            },
        }));
        let progress = std::sync::Arc::new(super::ExportControl::default());
        super::render_with_control(editor.project(), &target, progress.clone()).unwrap();
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
                shades.push(decoded.data(0)[0]);
            }
        }
        decoder.send_eof().unwrap();
        let mut decoded = ffmpeg::frame::Video::empty();
        while decoder.receive_frame(&mut decoded).is_ok() {
            shades.push(decoded.data(0)[0]);
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
            linked_audio: false,
        }];
        layered.push(Track {
            id: 4,
            kind: TrackKind::Video,
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
                shades.push(frame.data(0)[frame.stride(0) * 10 + 10]);
                if shades.len() == 1 {
                    title_pixels = frame.data(0).iter().filter(|&&value| value > 120).count();
                }
            }
        }
        decoder.send_eof().unwrap();
        let mut frame = ffmpeg::frame::Video::empty();
        while decoder.receive_frame(&mut frame).is_ok() {
            shades.push(frame.data(0)[frame.stride(0) * 10 + 10]);
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
            super::NEXT_EXPORT.fetch_add(1, Ordering::Relaxed)
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
                linked_audio: false,
            },
        }));
        super::render(editor.project(), &target).unwrap();
        assert_eq!(
            ffmpeg::format::input(&target).unwrap().duration(),
            1_000_000
        );
        fs::remove_file(source).unwrap();
        fs::remove_file(target).unwrap();
        fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn title_only_sequence_exports_visible_text_over_black() {
        let dir = std::env::temp_dir().join(format!(
            "ocus-title-export-{}-{}",
            std::process::id(),
            super::NEXT_EXPORT.fetch_add(1, Ordering::Relaxed)
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
        fs::remove_file(target).unwrap();
        fs::remove_dir(dir).unwrap();
    }
}
