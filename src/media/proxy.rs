use super::cache;
use ffmpeg_next as ffmpeg;
use std::collections::VecDeque;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

const PARAMETERS: &[u8] = b"proxy:v2:mjpeg:1280x720:bt601:origin";
const CAP: u64 = 512 * 1024 * 1024;

fn entry(root: &Path, source: &Path) -> Option<PathBuf> {
    Some(
        root.join("proxies")
            .join(format!("{}.nut", cache::key(source, PARAMETERS)?)),
    )
}

pub fn ready(source: &Path) -> Option<PathBuf> {
    ready_in(&cache::directory()?, source)
}

fn ready_in(root: &Path, source: &Path) -> Option<PathBuf> {
    let path = entry(root, source)?;
    let file = fs::File::open(&path).ok()?;
    if file.metadata().ok()?.len() == 0 {
        return None;
    }
    let _ = file.set_modified(SystemTime::now());
    Some(path)
}

pub fn generate(source: &Path, cancelled: &AtomicBool) -> Result<bool, String> {
    let root = cache::directory().ok_or("Media cache is unavailable")?;
    generate_in(&root, source, cancelled)
}

fn generate_in(root: &Path, source: &Path, cancelled: &AtomicBool) -> Result<bool, String> {
    check_cancelled(cancelled)?;
    if ready_in(root, source).is_some() {
        return Ok(true);
    }
    ffmpeg::init().map_err(|error| error.to_string())?;
    let mut input =
        ffmpeg::format::input_with_interrupt(source, || cancelled.load(Ordering::Relaxed))
            .map_err(|error| error.to_string())?;
    check_cancelled(cancelled)?;
    let stream = input
        .streams()
        .best(ffmpeg::media::Type::Video)
        .ok_or("No video")?;
    let mut decoder = ffmpeg::codec::context::Context::from_parameters(stream.parameters())
        .and_then(|context| context.decoder().video())
        .map_err(|error| error.to_string())?;
    let stream_index = stream.index();
    let time_base = stream.time_base();
    let fps = stream.avg_frame_rate();
    let origin = super::format_origin(&input, time_base);
    let rotation = super::Rotation::from_stream(&stream);
    let square = super::display_dimensions(&decoder);
    let (width, height) = rotation.dimensions(square.0, square.1);
    if width <= 1920 && height <= 1080 {
        return Ok(false);
    }
    let destination = entry(root, source).ok_or("Cannot identify proxy source")?;
    let directory = destination.parent().unwrap();
    fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    let temporary = tempfile::Builder::new()
        .prefix(".proxy-")
        .suffix(".nut")
        .tempfile_in(directory)
        .map_err(|error| error.to_string())?;
    let mut writer = ProxyWriter::new(&decoder, rotation, fps, time_base, temporary.path())?;
    loop {
        check_cancelled(cancelled)?;
        let mut packet = ffmpeg::Packet::empty();
        match packet.read(&mut input) {
            Ok(()) if packet.stream() == stream_index => {
                decoder
                    .send_packet(&packet)
                    .map_err(|error| error.to_string())?;
                writer.receive_frames(&mut decoder, origin, cancelled)?;
            }
            Ok(()) => {}
            Err(ffmpeg::Error::Eof) => break,
            Err(error) => return Err(error.to_string()),
        }
    }
    decoder.send_eof().map_err(|error| error.to_string())?;
    writer.receive_frames(&mut decoder, origin, cancelled)?;
    writer.finish(cancelled)?;
    check_cancelled(cancelled)?;
    if entry(root, source).as_ref() != Some(&destination) {
        return Err("Source changed during proxy generation".into());
    }
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    temporary
        .persist(&destination)
        .map_err(|error| error.to_string())?;
    cache::evict(root, CAP).map_err(|error| error.to_string())?;
    Ok(destination.is_file())
}

fn check_cancelled(cancelled: &AtomicBool) -> Result<(), String> {
    if cancelled.load(Ordering::Relaxed) {
        Err("Proxy cancelled".into())
    } else {
        Ok(())
    }
}

struct ProxyWriter {
    encoder: ffmpeg::encoder::video::Encoder,
    output: ffmpeg::format::context::Output,
    output_base: ffmpeg::Rational,
    to_rgba: ffmpeg::software::scaling::Context,
    to_yuv: ffmpeg::software::scaling::Context,
    rotation: super::Rotation,
    rgba: ffmpeg::frame::Video,
    frames: u64,
    durations: VecDeque<i64>,
}

impl ProxyWriter {
    fn new(
        decoder: &ffmpeg::decoder::Video,
        rotation: super::Rotation,
        fps: ffmpeg::Rational,
        time_base: ffmpeg::Rational,
        path: &Path,
    ) -> Result<Self, String> {
        if decoder.format() == ffmpeg::format::Pixel::None || decoder.width() == 0 {
            return Err("Proxy source has an unknown pixel format".into());
        }
        let square = super::display_dimensions(decoder);
        let display = rotation.dimensions(square.0, square.1);
        let (width, height) = super::fitted_dimensions(display.0, display.1, 1280, 720);
        let coded = rotation.dimensions(width, height);
        let to_rgba = super::rgba_scaler(decoder, coded.0, coded.1)?;
        let to_yuv = ffmpeg::software::scaling::Context::get(
            ffmpeg::format::Pixel::RGBA,
            width,
            height,
            ffmpeg::format::Pixel::YUVJ420P,
            width,
            height,
            ffmpeg::software::scaling::Flags::BILINEAR,
        )
        .map_err(|error| error.to_string())?;
        let mut output =
            ffmpeg::format::output_as(path, "nut").map_err(|error| error.to_string())?;
        let codec = ffmpeg::encoder::find(ffmpeg::codec::Id::MJPEG)
            .ok_or("FFmpeg MJPEG encoder is unavailable")?;
        let mut encoder = ffmpeg::codec::context::Context::new_with_codec(codec)
            .encoder()
            .video()
            .map_err(|error| error.to_string())?;
        encoder.set_width(width);
        encoder.set_height(height);
        encoder.set_format(ffmpeg::format::Pixel::YUVJ420P);
        encoder.set_time_base(time_base);
        encoder.set_aspect_ratio((1, 1));
        if fps.numerator() > 0 && fps.denominator() > 0 {
            encoder.set_frame_rate(Some(fps));
        }
        encoder.set_colorspace(ffmpeg::color::Space::SMPTE170M);
        encoder.set_color_range(ffmpeg::color::Range::JPEG);
        encoder.set_qmin(3);
        encoder.set_qmax(3);
        encoder.set_threading(ffmpeg::codec::threading::Config::count(2));
        if output
            .format()
            .flags()
            .contains(ffmpeg::format::Flags::GLOBAL_HEADER)
        {
            encoder.set_flags(ffmpeg::codec::Flags::GLOBAL_HEADER);
        }
        let encoder = encoder.open().map_err(|error| error.to_string())?;
        {
            let mut stream = output
                .add_stream(codec)
                .map_err(|error| error.to_string())?;
            stream.set_time_base(time_base);
            if fps.numerator() > 0 && fps.denominator() > 0 {
                stream.set_rate(fps);
                stream.set_avg_frame_rate(fps);
            }
            stream.set_parameters(&encoder);
        }
        output.write_header().map_err(|error| error.to_string())?;
        let output_base = output.stream(0).ok_or("Proxy stream missing")?.time_base();
        Ok(Self {
            encoder,
            output,
            output_base,
            to_rgba,
            to_yuv,
            rotation,
            rgba: ffmpeg::frame::Video::new(ffmpeg::format::Pixel::RGBA, width, height),
            frames: 0,
            durations: VecDeque::new(),
        })
    }

    fn receive_frames(
        &mut self,
        decoder: &mut ffmpeg::decoder::Video,
        origin: i64,
        cancelled: &AtomicBool,
    ) -> Result<(), String> {
        loop {
            check_cancelled(cancelled)?;
            let mut decoded = ffmpeg::frame::Video::empty();
            match decoder.receive_frame(&mut decoded) {
                Ok(()) => {}
                Err(
                    ffmpeg::Error::Eof
                    | ffmpeg::Error::Other {
                        errno: ffmpeg::error::EAGAIN,
                    },
                ) => return Ok(()),
                Err(error) => return Err(error.to_string()),
            }
            let pts = decoded
                .timestamp()
                .ok_or("Proxy source frame has no timestamp")?
                .checked_sub(origin)
                .ok_or("Proxy timestamp overflows")?;
            if pts < 0 {
                continue;
            }
            let mut scaled = ffmpeg::frame::Video::empty();
            self.to_rgba
                .run(&decoded, &mut scaled)
                .map_err(|error| error.to_string())?;
            let image = self.rotation.orient(&scaled);
            let stride = self.rgba.stride(0);
            for (y, row) in image
                .pixels
                .chunks_exact(image.width as usize * 4)
                .enumerate()
            {
                self.rgba.data_mut(0)[y * stride..y * stride + row.len()].copy_from_slice(row);
            }
            let mut frame = ffmpeg::frame::Video::empty();
            self.to_yuv
                .run(&self.rgba, &mut frame)
                .map_err(|error| error.to_string())?;
            frame.set_pts(Some(pts));
            frame.set_color_space(ffmpeg::color::Space::SMPTE170M);
            frame.set_color_range(ffmpeg::color::Range::JPEG);
            check_cancelled(cancelled)?;
            self.encoder
                .send_frame(&frame)
                .map_err(|error| error.to_string())?;
            self.durations.push_back(decoded.packet().duration);
            self.write_packets(cancelled)?;
            self.frames += 1;
        }
    }

    fn write_packets(&mut self, cancelled: &AtomicBool) -> Result<(), String> {
        loop {
            check_cancelled(cancelled)?;
            let mut packet = ffmpeg::Packet::empty();
            match self.encoder.receive_packet(&mut packet) {
                Ok(()) => {
                    packet.set_stream(0);
                    packet.set_duration(
                        self.durations
                            .pop_front()
                            .ok_or("Proxy frame duration missing")?,
                    );
                    packet.rescale_ts(self.encoder.time_base(), self.output_base);
                    packet
                        .write_interleaved(&mut self.output)
                        .map_err(|error| error.to_string())?;
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

    fn finish(mut self, cancelled: &AtomicBool) -> Result<(), String> {
        if self.frames == 0 {
            return Err("Proxy source has no decodable frames".into());
        }
        check_cancelled(cancelled)?;
        self.encoder.send_eof().map_err(|error| error.to_string())?;
        self.write_packets(cancelled)?;
        self.output
            .write_trailer()
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ffmpeg::Rescale;
    use std::process::Command;
    use std::time::Duration;

    fn fixture(path: &Path, duration: &str, size: &str) {
        assert!(
            Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-y",
                    "-f",
                    "lavfi",
                    "-i",
                    &format!("testsrc2=s={size}:r=30:d={duration}"),
                    "-c:v",
                    "mpeg4",
                    "-threads",
                    "2"
                ])
                .arg(path)
                .status()
                .unwrap()
                .success()
        );
    }

    #[test]
    fn generation_bakes_orientation_and_preserves_fractional_timestamps_and_colors() {
        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().join("base.mp4");
        let source = directory.path().join("rotated.mp4");
        let filter = concat!(
            "color=red:s=2048x1152:r=30000/1001:d=0.2,",
            "drawbox=x=0:y=0:w=1024:h=576:color=blue:t=fill,setsar=2,",
            "scale=in_color_matrix=bt601:out_color_matrix=bt709",
        );
        let status = Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                filter,
                "-colorspace",
                "bt709",
                "-c:v",
                "mpeg4",
                "-threads",
                "2",
            ])
            .arg(&base)
            .status()
            .unwrap();
        assert!(status.success());
        assert!(
            Command::new("ffmpeg")
                .args(["-v", "error", "-y", "-i"])
                .arg(&base)
                .args(["-c", "copy", "-metadata:s:v", "rotate=90"])
                .arg(&source)
                .status()
                .unwrap()
                .success()
        );
        assert!(generate_in(directory.path(), &source, &AtomicBool::new(false)).unwrap());
        let proxy = ready_in(directory.path(), &source).unwrap();
        let mut original_input = ffmpeg::format::input(&source).unwrap();
        let stream = original_input
            .streams()
            .best(ffmpeg::media::Type::Video)
            .unwrap();
        let time_base = stream.time_base();
        let origin = super::super::format_origin(&original_input, time_base);
        let original: Vec<_> = original_input
            .packets()
            .map(|(_, packet)| (packet.pts().unwrap() - origin, packet.duration()))
            .collect();
        let mut proxy_input = ffmpeg::format::input(&proxy).unwrap();
        assert_eq!(proxy_input.nb_streams(), 1);
        let stream = proxy_input.stream(0).unwrap();
        let proxy_base = stream.time_base();
        assert_eq!(stream.avg_frame_rate(), ffmpeg::Rational(30000, 1001));
        let decoder = ffmpeg::codec::context::Context::from_parameters(stream.parameters())
            .unwrap()
            .decoder()
            .video()
            .unwrap();
        assert_eq!((decoder.width(), decoder.height()), (202, 720));
        assert_eq!(decoder.aspect_ratio(), ffmpeg::Rational(1, 1));
        assert_eq!(decoder.color_range(), ffmpeg::color::Range::JPEG);
        assert_eq!(
            super::super::Rotation::from_stream(&stream).dimensions(202, 720),
            (202, 720)
        );
        let encoded: Vec<_> = proxy_input
            .packets()
            .map(|(_, packet)| {
                (
                    packet.pts().unwrap().rescale(proxy_base, time_base),
                    packet.duration().rescale(proxy_base, time_base),
                )
            })
            .collect();
        assert_eq!(original, encoded);
        let original = super::super::thumbnail(&source).unwrap();
        let encoded = super::super::thumbnail(&proxy).unwrap();
        assert_eq!(
            (original.width, original.height),
            (encoded.width, encoded.height)
        );
        let error: usize = original
            .pixels
            .iter()
            .zip(&encoded.pixels)
            .map(|(a, b)| a.abs_diff(*b) as usize)
            .sum();
        assert!(
            error < original.pixels.len() * 6,
            "mean color error: {}",
            error as f64 / original.pixels.len() as f64
        );
    }

    #[test]
    fn cache_identity_and_atomic_generation() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("cache");
        let source = directory.path().join("source.mp4");
        fixture(&source, "0.1", "2048x1152");
        let key = entry(&root, &source).unwrap();
        assert!(ready_in(&root, &source).is_none());
        assert!(generate_in(&root, &source, &AtomicBool::new(true)).is_err());
        assert!(!key.exists());
        assert!(generate_in(&root, &source, &AtomicBool::new(false)).unwrap());
        let contents = fs::read(&key).unwrap();
        assert!(generate_in(&root, &source, &AtomicBool::new(false)).unwrap());
        assert_eq!(contents, fs::read(&key).unwrap());
        fs::File::options()
            .write(true)
            .open(&source)
            .unwrap()
            .set_modified(SystemTime::now() + Duration::from_secs(2))
            .unwrap();
        assert_ne!(entry(&root, &source).unwrap(), key);
        assert!(ready_in(&root, &source).is_none());
        fs::write(&source, b"changed").unwrap();
        assert_ne!(entry(&root, &source).unwrap(), key);
        assert!(ready_in(&root, &source).is_none());
        assert!(generate_in(&root, &source, &AtomicBool::new(false)).is_err());
        assert_eq!(fs::read_dir(root.join("proxies")).unwrap().count(), 1);
    }

    #[test]
    fn cancellation_removes_in_progress_file_and_small_media_is_skipped() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.mp4");
        fixture(&source, "4", "2048x1152");
        let root = directory.path().join("cache");
        let cancelled = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| generate_in(&root, &source, &cancelled));
            let started = std::time::Instant::now();
            while !root.join("proxies").is_dir() {
                assert!(started.elapsed() < Duration::from_secs(10));
                std::thread::sleep(Duration::from_millis(1));
            }
            cancelled.store(true, Ordering::Relaxed);
            assert!(worker.join().unwrap().is_err());
        });
        assert_eq!(fs::read_dir(root.join("proxies")).unwrap().count(), 0);
        fixture(&source, "0.1", "320x180");
        assert!(!generate_in(&root, &source, &AtomicBool::new(false)).unwrap());
        assert_eq!(fs::read_dir(root.join("proxies")).unwrap().count(), 0);
    }
}
