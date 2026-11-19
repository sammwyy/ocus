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
    let mut context = ffmpeg::codec::context::Context::from_parameters(stream.parameters())
        .map_err(|error| error.to_string())?;
    let codec = ffmpeg::decoder::find(context.id()).ok_or("Video decoder unavailable")?;
    let threads = std::thread::available_parallelism().map_or(2, usize::from);
    context.set_threading(threading(codec, threads.saturating_sub(2).clamp(1, 8)));
    let mut decoder = context
        .decoder()
        .video()
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

fn threading(codec: ffmpeg::Codec, count: usize) -> ffmpeg::codec::threading::Config {
    use ffmpeg::codec::{
        Capabilities,
        threading::{Config, Type},
    };
    let capabilities = codec.capabilities();
    Config {
        kind: if capabilities.contains(Capabilities::FRAME_THREADS) {
            Type::Frame
        } else if capabilities.contains(Capabilities::SLICE_THREADS) {
            Type::Slice
        } else {
            Type::None
        },
        count,
        ..Config::default()
    }
}

fn yuv_scaler(
    decoder: &ffmpeg::decoder::Video,
    width: u32,
    height: u32,
) -> Result<ffmpeg::software::scaling::Context, String> {
    use ffmpeg::{
        color::{Range, Space},
        ffi,
    };
    let mut scaler = ffmpeg::software::scaling::Context::get(
        decoder.format(),
        decoder.width(),
        decoder.height(),
        ffmpeg::format::Pixel::YUVJ420P,
        width,
        height,
        ffmpeg::software::scaling::Flags::BILINEAR,
    )
    .map_err(|error| error.to_string())?;
    let matrix = match decoder.color_space() {
        Space::BT709 => ffi::SWS_CS_ITU709,
        Space::FCC => ffi::SWS_CS_FCC,
        Space::SMPTE240M => ffi::SWS_CS_SMPTE240M,
        Space::BT2020NCL => ffi::SWS_CS_BT2020,
        _ => ffi::SWS_CS_DEFAULT,
    };
    unsafe {
        let context = scaler.as_mut_ptr();
        let (mut input_table, mut output_table) = (std::ptr::null_mut(), std::ptr::null_mut());
        let (mut input_range, mut output_range) = (0, 0);
        let (mut brightness, mut contrast, mut saturation) = (0, 0, 0);
        if ffi::sws_getColorspaceDetails(
            context,
            &mut input_table,
            &mut input_range,
            &mut output_table,
            &mut output_range,
            &mut brightness,
            &mut contrast,
            &mut saturation,
        ) < 0
        {
            return Err("Cannot read proxy scaler colorspace".into());
        }
        let input_range = match decoder.color_range() {
            Range::JPEG => 1,
            Range::MPEG => 0,
            Range::Unspecified => input_range,
        };
        if ffi::sws_setColorspaceDetails(
            context,
            ffi::sws_getCoefficients(matrix),
            input_range,
            ffi::sws_getCoefficients(ffi::SWS_CS_ITU601),
            1,
            brightness,
            contrast,
            saturation,
        ) < 0
        {
            return Err("Cannot configure proxy scaler colorspace".into());
        }
    }
    Ok(scaler)
}

fn rotate_yuv(source: &ffmpeg::frame::Video, rotation: super::Rotation) -> ffmpeg::frame::Video {
    let (width, height) = rotation.dimensions(source.width(), source.height());
    let mut target = ffmpeg::frame::Video::new(ffmpeg::format::Pixel::YUVJ420P, width, height);
    for plane in 0..3 {
        let width = source.plane_width(plane) as usize;
        let height = source.plane_height(plane) as usize;
        let stride = target.stride(plane);
        for y in 0..height {
            for x in 0..width {
                let (dx, dy) = match rotation.0 {
                    1 => (y, width - 1 - x),
                    2 => (width - 1 - x, height - 1 - y),
                    3 => (height - 1 - y, x),
                    _ => (x, y),
                };
                target.data_mut(plane)[dy * stride + dx] =
                    source.data(plane)[y * source.stride(plane) + x];
            }
        }
    }
    target
}

struct ProxyWriter {
    encoder: ffmpeg::encoder::video::Encoder,
    output: ffmpeg::format::context::Output,
    output_base: ffmpeg::Rational,
    scaler: ffmpeg::software::scaling::Context,
    rotation: super::Rotation,
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
        let scaler = yuv_scaler(decoder, coded.0, coded.1)?;
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
        encoder.set_threading(threading(codec, 2));
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
            scaler,
            rotation,
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
            let mut frame = ffmpeg::frame::Video::empty();
            self.scaler
                .run(&decoded, &mut frame)
                .map_err(|error| error.to_string())?;
            if self.rotation.0 != 0 {
                frame = rotate_yuv(&frame, self.rotation);
            }
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

    #[test]
    fn planar_rotation_preserves_luma_and_both_chroma_planes() {
        ffmpeg::init().unwrap();
        let mut source = ffmpeg::frame::Video::new(ffmpeg::format::Pixel::YUVJ420P, 8, 4);
        for plane in 0..3 {
            let width = source.plane_width(plane) as usize;
            let height = source.plane_height(plane) as usize;
            let stride = source.stride(plane);
            for y in 0..height {
                for x in 0..width {
                    source.data_mut(plane)[y * stride + x] = (plane * 60 + y * width + x) as u8;
                }
            }
        }
        let chroma = [
            [0, 1, 2, 3, 4, 5, 6, 7],
            [3, 7, 2, 6, 1, 5, 0, 4],
            [7, 6, 5, 4, 3, 2, 1, 0],
            [4, 0, 5, 1, 6, 2, 7, 3],
        ];
        let corners = [
            [0, 7, 24, 31],
            [7, 31, 0, 24],
            [31, 24, 7, 0],
            [24, 0, 31, 7],
        ];
        for turns in 0..4 {
            let target = rotate_yuv(&source, super::super::Rotation(turns as u32));
            let width = target.width() as usize;
            let height = target.height() as usize;
            let stride = target.stride(0);
            assert_eq!(
                [
                    target.data(0)[0],
                    target.data(0)[width - 1],
                    target.data(0)[(height - 1) * stride],
                    target.data(0)[(height - 1) * stride + width - 1]
                ],
                corners[turns]
            );
            for plane in 1..3 {
                let pixels: Vec<_> = (0..target.plane_height(plane) as usize)
                    .flat_map(|y| {
                        let start = y * target.stride(plane);
                        target.data(plane)[start..start + target.plane_width(plane) as usize]
                            .iter()
                            .copied()
                    })
                    .collect();
                let expected: Vec<_> = chroma[turns]
                    .iter()
                    .map(|value| value + plane as u8 * 60)
                    .collect();
                assert_eq!(pixels, expected);
            }
        }
    }

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
