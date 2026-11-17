use std::path::Path;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use ffmpeg_next as ffmpeg;
use serde::{Deserialize, Serialize};

pub mod audio;
pub mod composite;
pub mod preview;

#[derive(Clone, Copy)]
pub(crate) struct Rotation(u32);

impl Rotation {
    fn from_stream(stream: &ffmpeg::Stream<'_>) -> Self {
        let angle = stream
            .side_data()
            .find(|data| data.kind() == ffmpeg::codec::packet::side_data::Type::DisplayMatrix)
            .and_then(|data| {
                let bytes: &[u8; 36] = data.data().try_into().ok()?;
                let matrix: [i32; 9] = std::array::from_fn(|i| {
                    i32::from_ne_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap())
                });
                // FFmpeg expects an aligned matrix of nine native-endian i32 values.
                Some(unsafe { ffmpeg::ffi::av_display_rotation_get(matrix.as_ptr()) })
            })
            .or_else(|| {
                stream
                    .metadata()
                    .get("rotate")?
                    .parse::<f64>()
                    .ok()
                    .map(|angle| -angle)
            })
            .unwrap_or(0.0);
        Self(if angle.is_finite() {
            ((angle / 90.0).round() as i32).rem_euclid(4) as u32
        } else {
            0
        })
    }

    fn dimensions(self, width: u32, height: u32) -> (u32, u32) {
        if self.0 % 2 == 1 {
            (height, width)
        } else {
            (width, height)
        }
    }

    fn orient(self, frame: &ffmpeg::frame::Video) -> Thumbnail {
        let (width, height) = self.dimensions(frame.width(), frame.height());
        let mut pixels = vec![0; width as usize * height as usize * 4];
        if self.0 == 0 {
            let row_size = width as usize * 4;
            for (y, row) in pixels.chunks_exact_mut(row_size).enumerate() {
                let start = y * frame.stride(0);
                row.copy_from_slice(&frame.data(0)[start..start + row_size]);
            }
            return Thumbnail {
                width,
                height,
                pixels,
            };
        }
        for y in 0..frame.height() {
            for x in 0..frame.width() {
                let (dx, dy) = match self.0 {
                    1 => (y, frame.width() - 1 - x),
                    2 => (frame.width() - 1 - x, frame.height() - 1 - y),
                    3 => (frame.height() - 1 - y, x),
                    _ => (x, y),
                };
                let source = y as usize * frame.stride(0) + x as usize * 4;
                let destination = (dy as usize * width as usize + dx as usize) * 4;
                pixels[destination..destination + 4]
                    .copy_from_slice(&frame.data(0)[source..source + 4]);
            }
        }
        Thumbnail {
            width,
            height,
            pixels,
        }
    }
}

pub(crate) fn fitted_dimensions(
    source_width: u32,
    source_height: u32,
    width: u32,
    height: u32,
) -> (u32, u32) {
    if u64::from(width) * u64::from(source_height) <= u64::from(height) * u64::from(source_width) {
        let fitted = (u64::from(width) * u64::from(source_height) / u64::from(source_width)) as u32;
        (width, (fitted.max(2) & !1).min(height))
    } else {
        let fitted =
            (u64::from(height) * u64::from(source_width) / u64::from(source_height)) as u32;
        ((fitted.max(2) & !1).min(width), height)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Media {
    pub id: u64,
    pub path: PathBuf,
    pub kind: MediaKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_us: Option<u64>,
    #[serde(default)]
    pub has_audio: bool,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Video,
    Audio,
    Image,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MediaProbe {
    pub kind: MediaKind,
    pub duration_us: Option<u64>,
    pub has_audio: bool,
}

pub struct Thumbnail {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

pub fn thumbnail(path: &Path) -> Result<Thumbnail, String> {
    ffmpeg::init().map_err(|error| error.to_string())?;
    let started = Instant::now();
    let mut input = ffmpeg::format::input_with_interrupt(path, move || {
        started.elapsed() > Duration::from_secs(10)
    })
    .map_err(|error| format!("Cannot read thumbnail: {error}"))?;
    let stream = input
        .streams()
        .best(ffmpeg::media::Type::Video)
        .ok_or("Source has no video frames")?;
    let index = stream.index();
    let rotation = Rotation::from_stream(&stream);
    let mut decoder = ffmpeg::codec::context::Context::from_parameters(stream.parameters())
        .and_then(|context| context.decoder().video())
        .map_err(|error| error.to_string())?;
    if decoder.format() == ffmpeg::format::Pixel::None || decoder.width() == 0 {
        return Err("Source has an unknown pixel format".into());
    }
    let display = rotation.dimensions(decoder.width(), decoder.height());
    let scale = (208.0 / display.0 as f32)
        .min(116.0 / display.1 as f32)
        .min(1.0);
    let width = (decoder.width() as f32 * scale).round().max(1.0) as u32;
    let height = (decoder.height() as f32 * scale).round().max(1.0) as u32;
    let mut scaler = ffmpeg::software::scaling::context::Context::get(
        decoder.format(),
        decoder.width(),
        decoder.height(),
        ffmpeg::format::Pixel::RGBA,
        width,
        height,
        ffmpeg::software::scaling::flag::Flags::BILINEAR,
    )
    .map_err(|error| error.to_string())?;
    for (stream, packet) in input.packets() {
        if stream.index() != index {
            continue;
        }
        decoder
            .send_packet(&packet)
            .map_err(|error| error.to_string())?;
        let mut decoded = ffmpeg::frame::Video::empty();
        if decoder.receive_frame(&mut decoded).is_ok() {
            return scale_thumbnail(&mut scaler, &decoded, rotation);
        }
    }
    decoder.send_eof().map_err(|error| error.to_string())?;
    let mut decoded = ffmpeg::frame::Video::empty();
    decoder
        .receive_frame(&mut decoded)
        .map_err(|_| "Source has no decodable video frame")?;
    scale_thumbnail(&mut scaler, &decoded, rotation)
}

fn scale_thumbnail(
    scaler: &mut ffmpeg::software::scaling::context::Context,
    decoded: &ffmpeg::frame::Video,
    rotation: Rotation,
) -> Result<Thumbnail, String> {
    let mut frame = ffmpeg::frame::Video::empty();
    scaler
        .run(decoded, &mut frame)
        .map_err(|error| error.to_string())?;
    let mut thumbnail = rotation.orient(&frame);
    for pixel in thumbnail.pixels.as_chunks_mut::<4>().0 {
        for channel in 0..3 {
            pixel[channel] = ((u16::from(pixel[channel]) * u16::from(pixel[3]) + 127) / 255) as u8;
        }
    }
    Ok(thumbnail)
}

pub fn probe(path: &Path) -> Result<MediaProbe, String> {
    if !path.is_file() {
        return Err("Source is not a regular file".into());
    }
    if path.to_str().is_none_or(|name| name.contains('\0')) {
        return Err("Source path is not supported by the decoder".into());
    }
    ffmpeg::init().map_err(|error| format!("Cannot initialize FFmpeg: {error}"))?;
    ffmpeg::log::set_level(ffmpeg::log::Level::Quiet);
    let started = Instant::now();
    let input = ffmpeg::format::input_with_interrupt(path, move || {
        started.elapsed() > Duration::from_secs(10)
    })
    .map_err(|error| format!("Cannot probe source: {error}"))?;
    let video = input.streams().best(ffmpeg::media::Type::Video);
    let audio = input.streams().best(ffmpeg::media::Type::Audio);
    let still = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            ["png", "jpg", "jpeg", "webp", "bmp", "ppm", "tif", "tiff"]
                .iter()
                .any(|known| extension.eq_ignore_ascii_case(known))
        });
    let (kind, stream) = if let Some(video) = video.as_ref() {
        (
            if still {
                MediaKind::Image
            } else {
                MediaKind::Video
            },
            video,
        )
    } else if let Some(audio) = audio.as_ref() {
        (MediaKind::Audio, audio)
    } else {
        return Err("Source contains no supported video or audio stream".into());
    };
    let duration_us = if kind == MediaKind::Image {
        None
    } else {
        u64::try_from(input.duration())
            .ok()
            .filter(|duration| *duration > 0)
            .or_else(|| {
                let time_base = stream.time_base();
                let numerator = i128::from(stream.duration())
                    * i128::from(time_base.numerator())
                    * i128::from(ffmpeg::ffi::AV_TIME_BASE);
                let denominator = i128::from(time_base.denominator());
                (numerator > 0 && denominator > 0)
                    .then(|| u64::try_from(numerator / denominator).ok())
                    .flatten()
                    .filter(|duration| *duration > 0)
            })
    };
    if duration_us.is_none() && kind != MediaKind::Image {
        return Err("Source has no measurable duration".into());
    }
    Ok(MediaProbe {
        kind,
        duration_us,
        has_audio: audio.is_some(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn legacy_rotation_metadata_is_clockwise() {
        ffmpeg::init().unwrap();
        let path =
            std::env::temp_dir().join(format!("ocus-rotation-tags-{}.mp4", std::process::id()));
        let mut output = ffmpeg::format::output(&path).unwrap();
        let mut stream = output
            .add_stream(ffmpeg::encoder::find(ffmpeg::codec::Id::H264))
            .unwrap();
        assert_eq!(Rotation::from_stream(&stream).0, 0);
        for (value, turns) in [
            ("90", 3),
            ("180", 2),
            ("270", 1),
            ("-90", 1),
            ("invalid", 0),
            ("NaN", 0),
        ] {
            let mut metadata = ffmpeg::Dictionary::new();
            metadata.set("rotate", value);
            stream.set_metadata(metadata);
            assert_eq!(Rotation::from_stream(&stream).0, turns, "{value}");
        }
        drop(output);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn probes_video_audio_and_still_images_from_contents() {
        let root = std::env::temp_dir().join(format!(
            "ocus-probe-{}-{}",
            std::process::id(),
            NEXT_TEST.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let wave = root.join("tone.wav");
        let mut bytes = Vec::new();
        bytes.extend(b"RIFF");
        bytes.extend((96000 + 36_u32).to_le_bytes());
        bytes.extend(b"WAVEfmt ");
        bytes.extend(16_u32.to_le_bytes());
        bytes.extend(1_u16.to_le_bytes());
        bytes.extend(1_u16.to_le_bytes());
        bytes.extend(48000_u32.to_le_bytes());
        bytes.extend(96000_u32.to_le_bytes());
        bytes.extend(2_u16.to_le_bytes());
        bytes.extend(16_u16.to_le_bytes());
        bytes.extend(b"data");
        bytes.extend(96000_u32.to_le_bytes());
        bytes.resize(bytes.len() + 96000, 0);
        fs::write(&wave, bytes).unwrap();
        let info = probe(&wave).unwrap();
        assert_eq!(info.kind, MediaKind::Audio);
        assert_eq!(info.duration_us, Some(1_000_000));
        assert!(info.has_audio);

        let video = root.join("frames.y4m");
        let mut frames = b"YUV4MPEG2 W2 H2 F30:1 Ip A1:1 C420jpeg\n".to_vec();
        for _ in 0..30 {
            frames.extend(b"FRAME\n");
            frames.extend([0, 0, 0, 0, 128, 128]);
        }
        fs::write(&video, frames).unwrap();
        let info = probe(&video).unwrap();
        assert_eq!(info.kind, MediaKind::Video);
        assert!(info.duration_us.is_some_and(|duration| duration > 0));
        assert!(!info.has_audio);
        let thumb = thumbnail(&video).unwrap();
        assert_eq!(
            thumb.pixels.len(),
            thumb.width as usize * thumb.height as usize * 4
        );

        let image = root.join("still.ppm");
        fs::write(&image, b"P6\n1 1\n255\n\xff\x00\x00").unwrap();
        let info = probe(&image).unwrap();
        assert_eq!(info.kind, MediaKind::Image);
        assert_eq!(info.duration_us, None);
        assert!(!info.has_audio);
        let thumb = thumbnail(&image).unwrap();
        assert_eq!(&thumb.pixels[0..4], &[255, 0, 0, 255]);
        let invalid = root.join("bad.mp4");
        fs::write(&invalid, b"not a video").unwrap();
        assert!(probe(&invalid).is_err());

        for file in [wave, video, image, invalid] {
            fs::remove_file(file).unwrap();
        }
        fs::remove_dir(root).unwrap();
    }
}
