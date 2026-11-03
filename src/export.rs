use crate::media::MediaKind;
use crate::project::Project;
use crate::timeline::{ClipContent, TrackKind};
use ffmpeg_next as ffmpeg;
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

static NEXT_EXPORT: AtomicU64 = AtomicU64::new(0);

#[derive(Default)]
pub struct ExportControl {
    cancelled: AtomicBool,
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
    let mut tracks = project
        .tracks()
        .iter()
        .filter(|track| !track.clips.is_empty());
    let Some(track) = tracks.next() else {
        return false;
    };
    if track.kind != TrackKind::Video || tracks.next().is_some() {
        return false;
    }
    let mut end = 0;
    track.clips.iter().all(|clip| {
        let valid = clip.start_frame == end
            && matches!(clip.content, ClipContent::Media { media_id }
                if project.media().iter().any(|media| media.id == media_id
                    && matches!(media.kind, MediaKind::Video | MediaKind::Image)
                    && !media.has_audio));
        end = clip.start_frame.saturating_add(clip.duration_frames);
        valid
    })
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
        return Err(
            "Export currently supports one contiguous video track without titles or audio".into(),
        );
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
        for clip in project.tracks().iter().flat_map(|track| &track.clips) {
            let ClipContent::Media { media_id } = clip.content else {
                continue;
            };
            let source = &project
                .media()
                .iter()
                .find(|media| media.id == media_id)
                .unwrap()
                .path;
            if destination == fs::canonicalize(source).map_err(|error| error.to_string())? {
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
    let result = render_to(project, &temporary, control)
        .and_then(|()| fs::rename(&temporary, path).map_err(|error| error.to_string()));
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
    };
    for clip in project.tracks().iter().flat_map(|track| &track.clips) {
        let ClipContent::Media { media_id } = clip.content else {
            unreachable!();
        };
        let source = project
            .media()
            .iter()
            .find(|media| media.id == media_id)
            .ok_or("Source missing")?;
        render_clip(
            clip,
            &source.path,
            settings.width,
            settings.height,
            fps,
            &mut muxer,
        )?;
    }
    muxer
        .encoder
        .send_eof()
        .map_err(|error| error.to_string())?;
    muxer.write_packets()?;
    muxer
        .output
        .write_trailer()
        .map_err(|error| error.to_string())
}

fn render_clip(
    clip: &crate::timeline::Clip,
    path: &Path,
    width: u32,
    height: u32,
    fps: ffmpeg::Rational,
    muxer: &mut Muxer,
) -> Result<(), String> {
    let cancel = muxer.control.clone();
    let mut input = ffmpeg::format::input_with_interrupt(path, move || {
        cancel.cancelled.load(Ordering::Relaxed)
    })
    .map_err(|error| error.to_string())?;
    let stream = input
        .streams()
        .best(ffmpeg::media::Type::Video)
        .ok_or("Source has no video")?;
    let index = stream.index();
    let mut decoder = ffmpeg::codec::context::Context::from_parameters(stream.parameters())
        .and_then(|context| context.decoder().video())
        .map_err(|error| error.to_string())?;
    let source_base = stream.time_base();
    let mut scaler = ffmpeg::software::scaling::context::Context::get(
        decoder.format(),
        decoder.width(),
        decoder.height(),
        ffmpeg::format::Pixel::YUV420P,
        width,
        height,
        ffmpeg::software::scaling::flag::Flags::BILINEAR,
    )
    .map_err(|error| error.to_string())?;
    let mut previous: Option<ffmpeg::frame::Video> = None;
    let mut first_pts = None;
    let mut next = 0;
    for (stream, packet) in input.packets() {
        muxer.control.check()?;
        if stream.index() != index {
            continue;
        }
        decoder
            .send_packet(&packet)
            .map_err(|error| error.to_string())?;
        drain_frames(
            &mut decoder,
            &mut scaler,
            &mut previous,
            &mut next,
            clip,
            (source_base, fps, &mut first_pts),
            muxer,
        )?;
        if next >= clip.duration_frames {
            break;
        }
    }
    if next < clip.duration_frames {
        decoder.send_eof().map_err(|error| error.to_string())?;
        drain_frames(
            &mut decoder,
            &mut scaler,
            &mut previous,
            &mut next,
            clip,
            (source_base, fps, &mut first_pts),
            muxer,
        )?;
    }
    let last = previous.ok_or("Source has no decodable video frames")?;
    while next < clip.duration_frames {
        muxer.write_frame(&last, clip.start_frame + next)?;
        next += 1;
    }
    Ok(())
}

struct Muxer {
    encoder: ffmpeg::encoder::video::Encoder,
    output: ffmpeg::format::context::Output,
    stream_index: usize,
    output_base: ffmpeg::Rational,
    control: Arc<ExportControl>,
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

fn drain_frames(
    decoder: &mut ffmpeg::decoder::Video,
    scaler: &mut ffmpeg::software::scaling::context::Context,
    previous: &mut Option<ffmpeg::frame::Video>,
    next: &mut u64,
    clip: &crate::timeline::Clip,
    (source_base, fps, first_pts): (ffmpeg::Rational, ffmpeg::Rational, &mut Option<i64>),
    muxer: &mut Muxer,
) -> Result<(), String> {
    let mut decoded = ffmpeg::frame::Video::empty();
    while decoder.receive_frame(&mut decoded).is_ok() {
        let source_frame = decoded
            .timestamp()
            .and_then(|pts| {
                let origin = *first_pts.get_or_insert(pts);
                let value = (i128::from(pts) - i128::from(origin))
                    * i128::from(source_base.numerator())
                    * i128::from(fps.numerator())
                    / (i128::from(source_base.denominator()) * i128::from(fps.denominator()));
                u64::try_from(value).ok()
            })
            .unwrap_or(clip.source_start_frame + *next);
        while *next < clip.duration_frames
            && clip.source_start_frame + *next < source_frame
            && previous.is_some()
        {
            muxer.write_frame(previous.as_ref().unwrap(), clip.start_frame + *next)?;
            *next += 1;
        }
        let mut scaled = ffmpeg::frame::Video::empty();
        scaler
            .run(&decoded, &mut scaled)
            .map_err(|error| error.to_string())?;
        *previous = Some(scaled);
        if *next < clip.duration_frames && source_frame >= clip.source_start_frame {
            muxer.write_frame(previous.as_ref().unwrap(), clip.start_frame + *next)?;
            *next += 1;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::media::{Media, MediaKind};
    use crate::project::{Command, Editor, Project, format};
    use crate::timeline::{Clip, ClipContent};
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
}
