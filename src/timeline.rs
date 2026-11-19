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
    pub fn source_frame_at_speed(
        self,
        start: u64,
        duration: u64,
        offset: u64,
        speed: f64,
    ) -> Option<u64> {
        if offset >= duration {
            return None;
        }
        if self == Self::Freeze {
            return Some(start);
        }
        self.source_frame_at(
            start,
            (duration as f64 * speed).ceil() as u64,
            (offset as f64 * speed).floor() as u64,
        )
    }

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
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub subtitles: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub locked: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
    pub clips: Vec<Clip>,
}

impl Track {
    pub fn valid_name(name: &str) -> bool {
        name == name.trim() && name.chars().count() <= 32 && !name.chars().any(char::is_control)
    }

    pub fn display_name(&self, positional: &str) -> String {
        if self.name.is_empty() {
            positional.to_owned()
        } else {
            self.name.clone()
        }
    }
}

/// A point in the sequence the editor can jump and snap to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    pub frame: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    Video,
    Audio,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lut: Option<std::path::PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chroma_key: Option<crate::timeline::ChromaKey>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transition: Option<Transition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transition_out: Option<Transition>,
    pub id: u64,
    pub start_frame: u64,
    pub source_start_frame: u64,
    pub duration_frames: u64,
    #[serde(default = "default_speed", skip_serializing_if = "normal_speed")]
    pub speed: f64,
    pub content: ClipContent,
    pub volume: f32,
    #[serde(default, skip_serializing_if = "zero")]
    pub fade_in_frames: u64,
    #[serde(default, skip_serializing_if = "zero")]
    pub fade_out_frames: u64,
    pub muted: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub noise_reduction: bool,
    /// A video clip that also carries its source's audio. The audio is cut,
    /// moved and trimmed with the picture; `volume` and `muted` apply to it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub linked_audio: bool,
    #[serde(default, skip_serializing_if = "Playback::is_default")]
    pub playback: Playback,
    #[serde(default, skip_serializing_if = "ColorAdjust::neutral")]
    pub color: ColorAdjust,
    #[serde(default, skip_serializing_if = "Transform::is_identity")]
    pub transform: Transform,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keyframes: Vec<Keyframe>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct ChromaKey {
    pub color: [u8; 3],
    pub similarity: f32,
    pub smoothness: f32,
    pub spill: f32,
}

impl Default for ChromaKey {
    fn default() -> Self {
        Self {
            color: [0, 255, 0],
            similarity: 0.1,
            smoothness: 0.1,
            spill: 0.5,
        }
    }
}

impl ChromaKey {
    fn bits(self) -> ([u8; 3], [u32; 3]) {
        (
            self.color,
            [self.similarity, self.smoothness, self.spill].map(f32::to_bits),
        )
    }
    pub fn valid(self) -> bool {
        [self.similarity, self.smoothness, self.spill]
            .into_iter()
            .all(|value| value.is_finite() && (0.0..=1.0).contains(&value))
    }
}

impl PartialEq for ChromaKey {
    fn eq(&self, other: &Self) -> bool {
        self.bits() == other.bits()
    }
}
impl Eq for ChromaKey {}
impl std::hash::Hash for ChromaKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::hash::Hash::hash(&self.bits(), state);
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum Easing {
    #[default]
    Linear,
    Hold,
    Curve {
        x1: f64,
        y1: f64,
        x2: f64,
        y2: f64,
    },
}

impl Easing {
    pub const EASE_IN: Self = Self::Curve {
        x1: 0.42,
        y1: 0.0,
        x2: 1.0,
        y2: 1.0,
    };
    pub const EASE_OUT: Self = Self::Curve {
        x1: 0.0,
        y1: 0.0,
        x2: 0.58,
        y2: 1.0,
    };
    pub const EASE_IN_OUT: Self = Self::Curve {
        x1: 0.42,
        y1: 0.0,
        x2: 0.58,
        y2: 1.0,
    };

    pub fn is_linear(&self) -> bool {
        *self == Self::Linear
    }

    pub fn valid(self) -> bool {
        match self {
            Self::Curve { x1, y1, x2, y2 } => {
                x1 <= x2
                    && y1 <= y2
                    && [x1, y1, x2, y2]
                        .iter()
                        .all(|v| v.is_finite() && (0.0..=1.0).contains(v))
            }
            _ => true,
        }
    }

    fn coordinate(s: f64, a: f64, b: f64) -> f64 {
        3.0 * (1.0 - s).powi(2) * s * a + 3.0 * (1.0 - s) * s * s * b + s.powi(3)
    }

    fn parameter(t: f64, x1: f64, x2: f64) -> f64 {
        let (mut low, mut high) = (0.0, 1.0);
        for _ in 0..60 {
            let middle = (low + high) / 2.0;
            if Self::coordinate(middle, x1, x2) < t {
                low = middle;
            } else {
                high = middle;
            }
        }
        (low + high) / 2.0
    }

    pub fn evaluate(self, t: f64) -> f64 {
        if t <= 0.0 {
            return 0.0;
        }
        if t >= 1.0 {
            return 1.0;
        }
        match self {
            Self::Linear => t,
            Self::Hold => 0.0,
            Self::Curve { x1, y1, x2, y2 } => Self::coordinate(Self::parameter(t, x1, x2), y1, y2),
        }
    }

    pub fn split(self, t: f64) -> (Self, Self) {
        if t <= 0.0 {
            return (Self::Linear, self);
        }
        if t >= 1.0 {
            return (self, Self::Linear);
        }
        let Self::Curve { x1, y1, x2, y2 } = self else {
            return (self, self);
        };
        let s = Self::parameter(t, x1, x2);
        let mix = |a: [f64; 2], b: [f64; 2]| [a[0] + (b[0] - a[0]) * s, a[1] + (b[1] - a[1]) * s];
        let a = mix([0.0, 0.0], [x1, y1]);
        let b = mix([x1, y1], [x2, y2]);
        let c = mix([x2, y2], [1.0, 1.0]);
        let d = mix(a, b);
        let e = mix(b, c);
        let p = mix(d, e);
        let normalized = |a: [f64; 2], b: [f64; 2], start: [f64; 2], end: [f64; 2]| {
            let w = end[0] - start[0];
            let h = end[1] - start[1];
            if h == 0.0 || w == 0.0 {
                return Self::Linear;
            }
            Self::Curve {
                x1: (a[0] - start[0]) / w,
                y1: (a[1] - start[1]) / h,
                x2: (b[0] - start[0]) / w,
                y2: (b[1] - start[1]) / h,
            }
        };
        let pieces = (
            normalized(a, d, [0.0, 0.0], p),
            normalized(e, c, p, [1.0, 1.0]),
        );
        debug_assert!(pieces.0.valid() && pieces.1.valid());
        pieces
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Keyframe {
    /// Offset from the clip's timeline start, independent of media playback mode.
    pub frame: u64,
    pub scale: f32,
    pub x: f32,
    pub y: f32,
    pub rotation: f32,
    pub opacity: f32,
    #[serde(default, skip_serializing_if = "Easing::is_linear")]
    pub easing: Easing,
}

impl Keyframe {
    pub fn new(frame: u64, transform: Transform) -> Self {
        Self {
            frame,
            scale: transform.scale,
            x: transform.x,
            y: transform.y,
            rotation: transform.rotation,
            opacity: transform.opacity,
            easing: Easing::Linear,
        }
    }

    pub fn valid(&self) -> bool {
        self.apply(Transform::default()).valid() && self.easing.valid()
    }

    fn apply(&self, base: Transform) -> Transform {
        Transform {
            scale: self.scale,
            x: self.x,
            y: self.y,
            rotation: self.rotation,
            opacity: self.opacity,
            ..base
        }
    }
}

pub fn valid_keyframes(keyframes: &[Keyframe]) -> bool {
    keyframes.iter().all(Keyframe::valid)
        && keyframes
            .windows(2)
            .all(|pair| pair[0].frame < pair[1].frame)
}

pub fn evaluate_transform(base: Transform, keyframes: &[Keyframe], frame: u64) -> Transform {
    let Some(first) = keyframes.first() else {
        return base;
    };
    if frame <= first.frame {
        return first.apply(base);
    }
    for pair in keyframes.windows(2) {
        let [left, right] = pair else { unreachable!() };
        if frame <= right.frame {
            let t = left
                .easing
                .evaluate((frame - left.frame) as f64 / (right.frame - left.frame) as f64)
                as f32;
            let lerp = |a: f32, b: f32| a + (b - a) * t;
            return Transform {
                scale: lerp(left.scale, right.scale),
                x: lerp(left.x, right.x),
                y: lerp(left.y, right.y),
                rotation: lerp(left.rotation, right.rotation),
                opacity: lerp(left.opacity, right.opacity),
                ..base
            };
        }
    }
    keyframes.last().unwrap().apply(base)
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
        if self.playback == Playback::Freeze {
            1
        } else {
            (self.duration_frames as f64 * self.speed).ceil() as u64
        }
    }

    pub fn timeline_span(&self, source_frames: u64) -> u64 {
        (source_frames as f64 / self.speed).floor() as u64
    }

    pub fn valid_speed(&self) -> bool {
        self.speed.is_finite() && (0.25..=4.0).contains(&self.speed)
    }
}

impl Clip {
    pub fn keyframes_for_range(&self, offset: i128) -> Vec<Keyframe> {
        if self.keyframes.is_empty() {
            return Vec::new();
        }
        let mut keys = Vec::new();
        if offset > 0 {
            let mut boundary = Keyframe::new(
                0,
                evaluate_transform(self.transform, &self.keyframes, offset as u64),
            );
            if let Some(left) = self
                .keyframes
                .iter()
                .rfind(|key| i128::from(key.frame) <= offset)
            {
                boundary.easing = left.easing;
                if let Some(right) = self
                    .keyframes
                    .iter()
                    .find(|key| i128::from(key.frame) > offset)
                {
                    let t = (offset - i128::from(left.frame)) as f64
                        / (right.frame - left.frame) as f64;
                    boundary.easing = left.easing.split(t).1;
                }
            }
            keys.push(boundary);
        }
        keys.extend(self.keyframes.iter().filter_map(|key| {
            let frame = i128::from(key.frame) - offset;
            (frame >= 0 && frame <= i128::from(u64::MAX) && (offset <= 0 || frame > 0)).then_some({
                Keyframe {
                    frame: frame as u64,
                    ..*key
                }
            })
        }));
        keys
    }

    pub fn source_frame_at(&self, offset: u64) -> Option<u64> {
        self.playback.source_frame_at_speed(
            self.source_start_frame,
            self.duration_frames,
            offset,
            self.speed,
        )
    }

    pub fn source_start_for_range(&self, offset: u64, duration: u64) -> Option<u64> {
        if offset.checked_add(duration)? > self.duration_frames {
            return None;
        }
        self.source_start_frame.checked_add(match self.playback {
            Playback::Forward => self.source_offset(offset),
            Playback::Reverse => self
                .source_span()
                .checked_sub(self.source_offset(offset))?
                .checked_sub((duration as f64 * self.speed).ceil() as u64)?,
            Playback::Freeze => 0,
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transition {
    pub kind: TransitionKind,
    pub duration_frames: u64,
    #[serde(default, skip_serializing_if = "SlideDirection::is_default")]
    pub direction: SlideDirection,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlideDirection {
    Left,
    #[default]
    Right,
    Up,
    Down,
}

impl SlideDirection {
    fn is_default(&self) -> bool {
        *self == Self::Right
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionKind {
    Fade,
    Dissolve,
    Slide,
}

fn zero(value: &u64) -> bool {
    *value == 0
}

impl Clip {
    pub fn transform_at(&self, timeline_frame: u64) -> Transform {
        evaluate_transform(
            self.transform,
            &self.keyframes,
            timeline_frame.saturating_sub(self.start_frame),
        )
    }

    pub(crate) fn clamp_fades(&mut self) {
        self.fade_in_frames = self.fade_in_frames.min(self.duration_frames);
        self.fade_out_frames = self
            .fade_out_frames
            .min(self.duration_frames - self.fade_in_frames);
    }

    pub(crate) fn valid_fades(&self) -> bool {
        self.fade_in_frames <= self.duration_frames
            && self.fade_out_frames <= self.duration_frames - self.fade_in_frames
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipContent {
    Media { media_id: u64 },
    Title(Title),
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ColorAdjust {
    pub brightness: f32,
    pub contrast: f32,
    pub saturation: f32,
}

impl Default for ColorAdjust {
    fn default() -> Self {
        Self {
            brightness: 0.0,
            contrast: 1.0,
            saturation: 1.0,
        }
    }
}

impl ColorAdjust {
    pub fn neutral(&self) -> bool {
        *self == Self::default()
    }

    pub fn valid(&self) -> bool {
        self.brightness.is_finite()
            && (-1.0..=1.0).contains(&self.brightness)
            && self.contrast.is_finite()
            && (0.0..=2.0).contains(&self.contrast)
            && self.saturation.is_finite()
            && (0.0..=2.0).contains(&self.saturation)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Title {
    pub text: String,
    pub font_family: String,
    pub font_size: f32,
    pub color: [u8; 4],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background: Option<[u8; 4]>,
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
            background: None,
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

#[cfg(test)]
mod tests {
    #[test]
    fn easing_math_and_compatibility() {
        use super::*;
        for easing in [
            Easing::Linear,
            Easing::Hold,
            Easing::EASE_IN,
            Easing::EASE_OUT,
            Easing::EASE_IN_OUT,
        ] {
            assert_eq!(easing.evaluate(0.0), 0.0);
            assert_eq!(easing.evaluate(1.0), 1.0);
            for i in 0..100 {
                assert!(
                    easing.evaluate(i as f64 / 100.0) <= easing.evaluate((i + 1) as f64 / 100.0)
                );
            }
        }
        assert!((Easing::EASE_IN.evaluate(0.5) - 0.3153568).abs() < 1e-6);
        assert!((Easing::EASE_OUT.evaluate(0.5) - 0.6846432).abs() < 1e-6);
        assert!((Easing::EASE_IN_OUT.evaluate(0.5) - 0.5).abs() < 1e-12);
        let old = r#"{"frame":0,"scale":1.0,"x":0.0,"y":0.0,"rotation":0.0,"opacity":1.0}"#;
        let key: Keyframe = serde_json::from_str(old).unwrap();
        assert_eq!(serde_json::to_string(&key).unwrap(), old);
        for easing in [
            Easing::Curve {
                x1: 1.0 / 3.0,
                y1: 1.0,
                x2: 2.0 / 3.0,
                y2: 0.0,
            },
            Easing::Curve {
                x1: 0.8,
                y1: 0.0,
                x2: 0.2,
                y2: 1.0,
            },
        ] {
            assert!(!valid_keyframes(&[Keyframe { easing, ..key }]));
        }
        for easing in [
            Easing::Hold,
            Easing::EASE_IN,
            Easing::EASE_OUT,
            Easing::EASE_IN_OUT,
        ] {
            let eased = Keyframe { easing, ..key };
            assert_eq!(
                serde_json::from_str::<Keyframe>(&serde_json::to_string(&eased).unwrap()).unwrap(),
                eased
            );
        }
        for value in [-0.01, 1.01, f64::NAN, f64::INFINITY] {
            for easing in [
                Easing::Curve {
                    x1: value,
                    y1: 0.0,
                    x2: 1.0,
                    y2: 1.0,
                },
                Easing::Curve {
                    x1: 0.0,
                    y1: value,
                    x2: 1.0,
                    y2: 1.0,
                },
                Easing::Curve {
                    x1: 0.0,
                    y1: 0.0,
                    x2: value,
                    y2: 1.0,
                },
                Easing::Curve {
                    x1: 0.0,
                    y1: 0.0,
                    x2: 1.0,
                    y2: value,
                },
            ] {
                assert!(!valid_keyframes(&[Keyframe { easing, ..key }]));
            }
        }
        let hold = Keyframe {
            easing: Easing::Hold,
            ..key
        };
        let end = Keyframe::new(
            60,
            Transform {
                x: 1.0,
                ..Default::default()
            },
        );
        assert_eq!(
            evaluate_transform(Default::default(), &[hold, end], 59).x,
            0.0
        );
        assert_eq!(
            evaluate_transform(Default::default(), &[hold, end], 60).x,
            1.0
        );
        assert_eq!(
            evaluate_transform(Default::default(), &[], 10),
            Transform::default()
        );
    }

    #[test]
    fn subdivided_ranges_preserve_every_property_at_every_frame() {
        use super::*;
        let mut clip: Clip = serde_json::from_value(serde_json::json!({
            "id":1,"start_frame":0,"source_start_frame":0,"duration_frames":61,
            "content":{"media":{"media_id":1}},"volume":1.0,"muted":false
        }))
        .unwrap();
        let mut easings = vec![
            Easing::Linear,
            Easing::Hold,
            Easing::EASE_IN,
            Easing::EASE_OUT,
            Easing::EASE_IN_OUT,
        ];
        let mut seed = 0x5eed_u64;
        let mut random = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            (seed >> 32) as f64 / u32::MAX as f64
        };
        for _ in 0..16 {
            let (a, b, c, d) = (random(), random(), random(), random());
            easings.push(Easing::Curve {
                x1: a.min(b),
                x2: a.max(b),
                y1: c.min(d),
                y2: c.max(d),
            });
        }
        for easing in easings {
            clip.keyframes = vec![
                Keyframe {
                    easing,
                    ..Keyframe::new(0, Transform::default())
                },
                Keyframe::new(
                    60,
                    Transform {
                        x: 0.8,
                        y: -0.5,
                        scale: 2.0,
                        rotation: 90.0,
                        opacity: 0.2,
                        ..Default::default()
                    },
                ),
            ];
            for split in [1, 7, 15, 30, 49, 59] {
                let mut right = clip.clone();
                right.start_frame = split;
                right.keyframes = clip.keyframes_for_range(i128::from(split));
                let mut left = clip.clone();
                left.duration_frames = split;
                let (first, second) = easing.split(split as f64 / 60.0);
                assert!(first.valid() && second.valid());
                let boundary = Keyframe::new(split, clip.transform_at(split));
                left.keyframes[0].easing = first;
                left.keyframes[1] = boundary;
                for frame in 0..=60 {
                    let expected = clip.transform_at(frame);
                    let actual = if frame < split {
                        left.transform_at(frame)
                    } else {
                        right.transform_at(frame)
                    };
                    for (a, b) in [
                        (actual.x, expected.x),
                        (actual.y, expected.y),
                        (actual.scale, expected.scale),
                        (actual.rotation, expected.rotation),
                        (actual.opacity, expected.opacity),
                    ] {
                        assert!(
                            (a - b).abs() < 2e-5,
                            "{easing:?} split {split} frame {frame}: {a} != {b}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn animation_advances_for_reverse_and_freeze_and_survives_ranges() {
        use super::*;
        for playback in [Playback::Forward, Playback::Reverse, Playback::Freeze] {
            let mut clip: Clip = serde_json::from_value(serde_json::json!({
                "id":1,"start_frame":100,"source_start_frame":50,"duration_frames":30,
                "content":{"media":{"media_id":1}},"volume":1.0,"muted":false
            }))
            .unwrap();
            clip.playback = playback;
            clip.keyframes = vec![
                Keyframe::new(0, Transform::default()),
                Keyframe::new(
                    20,
                    Transform {
                        x: 1.0,
                        opacity: 0.0,
                        ..Default::default()
                    },
                ),
            ];
            let middle = clip.transform_at(110);
            assert_eq!((middle.x, middle.opacity), (0.5, 0.5));
            let mut right = clip.clone();
            right.keyframes = clip.keyframes_for_range(10);
            right.source_start_frame = clip.source_start_for_range(10, 20).unwrap();
            right.start_frame = 110;
            right.duration_frames = 20;
            assert_eq!(right.transform_at(110), middle);
            assert_eq!(right.transform_at(115), clip.transform_at(115));
            right.keyframes = right.keyframes_for_range(-5);
            right.start_frame = 105;
            assert_eq!(right.transform_at(115), clip.transform_at(115));
        }
    }
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
