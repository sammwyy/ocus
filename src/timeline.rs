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
    pub content: ClipContent,
    pub volume: f32,
    pub muted: bool,
    /// A video clip that also carries its source's audio. The audio is cut,
    /// moved and trimmed with the picture; `volume` and `muted` apply to it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub linked_audio: bool,
    #[serde(default, skip_serializing_if = "Transform::is_identity")]
    pub transform: Transform,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transform {
    pub scale: f32,
    pub x: f32,
    pub y: f32,
    pub rotation: f32,
    pub crop_left: f32,
    pub crop_top: f32,
    pub crop_right: f32,
    pub crop_bottom: f32,
    pub opacity: f32,
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            scale: 1.0,
            x: 0.0,
            y: 0.0,
            rotation: 0.0,
            crop_left: 0.0,
            crop_top: 0.0,
            crop_right: 0.0,
            crop_bottom: 0.0,
            opacity: 1.0,
        }
    }
}

impl Transform {
    pub fn is_identity(&self) -> bool {
        *self == Self::default()
    }

    pub fn valid(&self) -> bool {
        [
            (self.scale, 0.1, 4.0),
            (self.x, -1.0, 1.0),
            (self.y, -1.0, 1.0),
            (self.rotation, -360.0, 360.0),
            (self.crop_left, 0.0, 1.0),
            (self.crop_top, 0.0, 1.0),
            (self.crop_right, 0.0, 1.0),
            (self.crop_bottom, 0.0, 1.0),
            (self.opacity, 0.0, 1.0),
        ]
        .into_iter()
        .all(|(v, min, max)| v.is_finite() && (min..=max).contains(&v))
            && self.crop_left + self.crop_right < 1.0
            && self.crop_top + self.crop_bottom < 1.0
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
mod tests {
    use super::*;

    #[test]
    fn identity_is_backward_compatible() {
        let json = r#"{"id":1,"start_frame":0,"source_start_frame":0,"duration_frames":10,"content":{"media":{"media_id":1}},"volume":1.0,"muted":false}"#;
        let clip: Clip = serde_json::from_str(json).unwrap();
        assert!(clip.transform.is_identity());
        assert!(!serde_json::to_string(&clip).unwrap().contains("transform"));
        for invalid in [
            Transform {
                scale: f32::NAN,
                ..Default::default()
            },
            Transform {
                crop_left: 0.5,
                crop_right: 0.5,
                ..Default::default()
            },
            Transform {
                opacity: 1.1,
                ..Default::default()
            },
        ] {
            assert!(!invalid.valid());
        }
    }
}
