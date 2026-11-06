use super::FrameRate;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Container {
    Mp4,
    Matroska,
    WebM,
}

impl Container {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Matroska => "mkv",
            Self::WebM => "webm",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoCodec {
    H264,
    Hevc,
    Vp9,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioCodec {
    Aac,
    Opus,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EncodingSpeed {
    #[default]
    Fast,
    Balanced,
    Quality,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExportSettings {
    pub container: Container,
    pub video_codec: VideoCodec,
    pub audio_codec: AudioCodec,
    pub width: u32,
    pub height: u32,
    pub frame_rate: FrameRate,
    pub video_bitrate_kbps: u32,
    pub audio_bitrate_kbps: u32,
    #[serde(default)]
    pub video_quality: Option<u8>,
    #[serde(default)]
    pub speed: EncodingSpeed,
}

impl ExportSettings {
    pub fn for_project(width: u32, height: u32, frame_rate: FrameRate) -> Self {
        Self {
            container: Container::Mp4,
            video_codec: VideoCodec::H264,
            audio_codec: AudioCodec::Aac,
            width,
            height,
            frame_rate,
            video_bitrate_kbps: 8_000,
            audio_bitrate_kbps: 192,
            video_quality: None,
            speed: EncodingSpeed::Fast,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.width < 2
            || self.height < 2
            || self.width > 8192
            || self.height > 8192
            || !self.width.is_multiple_of(2)
            || !self.height.is_multiple_of(2)
        {
            return Err("Output width and height must be even and between 2 and 8192".into());
        }
        let rate = &self.frame_rate;
        if rate.denominator == 0
            || rate.numerator < rate.denominator
            || rate.denominator > 1001
            || u64::from(rate.numerator) > 120 * u64::from(rate.denominator)
        {
            return Err("Output frame rate must be between 1 and 120 fps".into());
        }
        if !(100..=200_000).contains(&self.video_bitrate_kbps)
            || !(32..=512).contains(&self.audio_bitrate_kbps)
        {
            return Err("Video bitrate must be 100–200000 kbps; audio 32–512 kbps".into());
        }
        if self.video_quality.is_some_and(|quality| {
            quality
                > if self.video_codec == VideoCodec::Vp9 {
                    63
                } else {
                    51
                }
        }) {
            return Err("Video quality is out of range for this codec".into());
        }
        match (self.container, self.video_codec, self.audio_codec) {
            (Container::Mp4, VideoCodec::H264 | VideoCodec::Hevc, AudioCodec::Aac)
            | (Container::Matroska, _, _)
            | (Container::WebM, VideoCodec::Vp9, AudioCodec::Opus) => Ok(()),
            _ => Err("Codec combination is not supported by this container".into()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LastExport {
    pub settings: ExportSettings,
    pub path: PathBuf,
}

impl LastExport {
    pub fn validate(&self) -> Result<(), String> {
        self.settings.validate()?;
        if self.path.as_os_str().is_empty()
            || !self
                .path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case(self.settings.container.extension()))
        {
            return Err("Last export path must match its container".into());
        }
        Ok(())
    }
}
