use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub id: u64,
    pub kind: TrackKind,
    pub clips: Vec<Clip>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    Video,
    Audio,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    pub id: u64,
    pub start_frame: u64,
    pub source_start_frame: u64,
    pub duration_frames: u64,
    #[serde(default = "default_speed", skip_serializing_if = "normal_speed")]
    pub speed: f64,
    pub content: ClipContent,
    pub volume: f32,
    pub muted: bool,
    /// A video clip that also carries its source's audio. The audio is cut,
    /// moved and trimmed with the picture; `volume` and `muted` apply to it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub linked_audio: bool,
}

fn default_speed() -> f64 {
    1.0
}

fn normal_speed(speed: &f64) -> bool {
    *speed == 1.0
}

impl Clip {
    pub fn source_offset(&self, timeline_frames: u64) -> u64 {
        (timeline_frames as f64 * self.speed).floor() as u64
    }

    pub fn source_span(&self) -> u64 {
        (self.duration_frames as f64 * self.speed).ceil() as u64
    }

    pub fn timeline_span(&self, source_frames: u64) -> u64 {
        (source_frames as f64 / self.speed).floor() as u64
    }

    pub fn valid_speed(&self) -> bool {
        self.speed.is_finite() && (0.25..=4.0).contains(&self.speed)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipContent {
    Media { media_id: u64 },
    Title(Title),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Title {
    pub text: String,
    pub font_family: String,
    pub font_size: f32,
    pub color: [u8; 4],
    pub x: f32,
    pub y: f32,
    #[serde(default = "default_bold")]
    pub bold: bool,
    #[serde(default)]
    pub italic: bool,
    #[serde(default)]
    pub outline: bool,
    #[serde(default)]
    pub parse_html: bool,
    #[serde(default)]
    pub parse_markdown: bool,
}

fn default_bold() -> bool {
    true
}

impl Default for Title {
    fn default() -> Self {
        Self {
            text: String::new(),
            font_family: "Sans".into(),
            font_size: 64.0,
            color: [255; 4],
            x: 0.5,
            y: 0.5,
            bold: true,
            italic: false,
            outline: false,
            parse_html: false,
            parse_markdown: false,
        }
    }
}

impl Title {
    pub fn valid(&self) -> bool {
        self.font_size.is_finite()
            && self.font_size > 0.0
            && self.x.is_finite()
            && self.y.is_finite()
            && (0.0..=1.0).contains(&self.x)
            && (0.0..=1.0).contains(&self.y)
    }
}
