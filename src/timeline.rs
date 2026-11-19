use serde::{Deserialize, Serialize};

#[derive(Default, Clone, Copy, Debug, Hash, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Playback {
    #[default]
    Forward,
    Reverse,
    Freeze,
}

impl Playback {
    pub fn source_frame_at(self, start: u64, duration: u64, offset: u64) -> Option<u64> {
        if offset >= duration {
            return None;
        }
        start.checked_add(match self {
            Self::Forward => offset,
            Self::Reverse => duration - 1 - offset,
            Self::Freeze => 0,
        })
    }

    fn is_default(&self) -> bool {
        *self == Self::Forward
    }
}

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
    pub content: ClipContent,
    pub volume: f32,
    pub muted: bool,
    /// A video clip that also carries its source's audio. The audio is cut,
    /// moved and trimmed with the picture; `volume` and `muted` apply to it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub linked_audio: bool,
    #[serde(default, skip_serializing_if = "Playback::is_default")]
    pub playback: Playback,
}

impl Clip {
    pub fn source_frame_at(&self, offset: u64) -> Option<u64> {
        self.playback
            .source_frame_at(self.source_start_frame, self.duration_frames, offset)
    }

    pub fn source_start_for_range(&self, offset: u64, duration: u64) -> Option<u64> {
        self.source_start_frame.checked_add(match self.playback {
            Playback::Forward => offset,
            Playback::Reverse => self
                .duration_frames
                .checked_sub(offset.checked_add(duration)?)?,
            Playback::Freeze => 0,
        })
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

#[cfg(test)]
mod playback_tests {
    use super::*;

    #[test]
    fn mapping_and_backward_compatible_json() {
        let json = r#"{"id":1,"start_frame":0,"source_start_frame":10,"duration_frames":5,"content":{"media":{"media_id":1}},"volume":1.0,"muted":false}"#;
        let mut clip: Clip = serde_json::from_str(json).unwrap();
        assert_eq!(clip.playback, Playback::Forward);
        assert!(!serde_json::to_string(&clip).unwrap().contains("playback"));
        for (playback, expected) in [
            (Playback::Forward, [10, 11, 12, 13, 14]),
            (Playback::Reverse, [14, 13, 12, 11, 10]),
            (Playback::Freeze, [10; 5]),
        ] {
            clip.playback = playback;
            assert_eq!(
                (0..5)
                    .map(|k| clip.source_frame_at(k).unwrap())
                    .collect::<Vec<_>>(),
                expected
            );
            assert_eq!(clip.source_frame_at(5), None);
            assert_eq!(
                serde_json::from_str::<Clip>(&serde_json::to_string(&clip).unwrap()).unwrap(),
                clip
            );
        }
        clip.duration_frames = 0;
        assert_eq!(clip.source_frame_at(0), None);
        clip.duration_frames = 2;
        clip.source_start_frame = u64::MAX;
        clip.playback = Playback::Forward;
        assert_eq!(clip.source_frame_at(1), None);
    }
}
