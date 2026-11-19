use super::{Clip, ClipContent, EffectShader, Playback, Track, TrackKind};
use crate::media::{Media, MediaKind};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectCategory {
    Video,
    Audio,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectKind {
    Grayscale,
    Sepia,
    Invert,
    Vignette,
    Blur,
    Pixelate,
    ChromaticAberration,
    CustomShader,
    ChromaKey,
    ColorCorrection,
    ColorLut,
    Mask,
    Stabilization,
    FreezeFrame,
    Reverse,
    Speed,
    NoiseReduction,
    Fades,
}

impl EffectKind {
    pub const ALL: [Self; 18] = [
        Self::Grayscale,
        Self::Sepia,
        Self::Invert,
        Self::Vignette,
        Self::Blur,
        Self::Pixelate,
        Self::ChromaticAberration,
        Self::CustomShader,
        Self::ChromaKey,
        Self::ColorCorrection,
        Self::ColorLut,
        Self::Mask,
        Self::Stabilization,
        Self::FreezeFrame,
        Self::Reverse,
        Self::Speed,
        Self::NoiseReduction,
        Self::Fades,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Self::Grayscale => "grayscale",
            Self::Sepia => "sepia",
            Self::Invert => "invert",
            Self::Vignette => "vignette",
            Self::Blur => "blur",
            Self::Pixelate => "pixelate",
            Self::ChromaticAberration => "chromatic_aberration",
            Self::CustomShader => "custom_shader",
            Self::ChromaKey => "chroma_key",
            Self::ColorCorrection => "color_correction",
            Self::ColorLut => "color_lut",
            Self::Mask => "mask",
            Self::Stabilization => "stabilization",
            Self::FreezeFrame => "freeze_frame",
            Self::Reverse => "reverse",
            Self::Speed => "speed",
            Self::NoiseReduction => "noise_reduction",
            Self::Fades => "fades",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Grayscale => "Grayscale",
            Self::Sepia => "Sepia",
            Self::Invert => "Invert",
            Self::Vignette => "Vignette",
            Self::Blur => "Blur",
            Self::Pixelate => "Pixelate",
            Self::ChromaticAberration => "Chromatic aberration",
            Self::CustomShader => "Custom shader",
            Self::ChromaKey => "Chroma key",
            Self::ColorCorrection => "Color correction",
            Self::ColorLut => "Color LUT",
            Self::Mask => "Mask",
            Self::Stabilization => "Stabilization",
            Self::FreezeFrame => "Freeze frame",
            Self::Reverse => "Reverse",
            Self::Speed => "Speed",
            Self::NoiseReduction => "Noise reduction",
            Self::Fades => "Fades",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Grayscale => "Remove color from the image.",
            Self::Sepia => "Apply a warm sepia tone.",
            Self::Invert => "Invert image colors.",
            Self::Vignette => "Darken the image edges.",
            Self::Blur => "Soften image details.",
            Self::Pixelate => "Reduce the image to blocks of color.",
            Self::ChromaticAberration => "Offset the image color channels.",
            Self::CustomShader => "Process the image with a WGSL shader file.",
            Self::ChromaKey => "Make a selected color transparent.",
            Self::ColorCorrection => "Adjust brightness, contrast and saturation.",
            Self::ColorLut => "Transform colors with a cube LUT file.",
            Self::Mask => "Limit visibility to a shaped region.",
            Self::Stabilization => "Analyze and compensate camera motion.",
            Self::FreezeFrame => "Hold a single video frame.",
            Self::Reverse => "Play the source backwards.",
            Self::Speed => "Change playback speed.",
            Self::NoiseReduction => "Reduce background audio noise.",
            Self::Fades => "Fade audio in and out.",
        }
    }

    pub fn categories(self) -> &'static [EffectCategory] {
        use EffectCategory::{Audio, Video};
        match self {
            Self::Reverse | Self::Speed => &[Video, Audio],
            Self::NoiseReduction | Self::Fades => &[Audio],
            _ => &[Video],
        }
    }

    pub fn is_shader(self) -> bool {
        matches!(
            self,
            Self::Grayscale
                | Self::Sepia
                | Self::Invert
                | Self::Vignette
                | Self::Blur
                | Self::Pixelate
                | Self::ChromaticAberration
                | Self::CustomShader
        )
    }
}

#[derive(Clone, Copy)]
pub struct EffectTarget<'a> {
    pub clip: &'a Clip,
    pub track: &'a Track,
    pub media: &'a Media,
}

pub fn can_apply(kind: EffectKind, target: EffectTarget<'_>) -> Result<(), String> {
    if target.track.locked {
        return Err("Track is locked".into());
    }
    if target.clip.effect_stack.contains(&kind) {
        return Err(format!("{} is already applied", kind.label()));
    }
    compatible(kind, target)
}

pub fn applicable_effects(target: EffectTarget<'_>) -> Vec<EffectKind> {
    EffectKind::ALL
        .into_iter()
        .filter(|kind| can_apply(*kind, target).is_ok())
        .collect()
}

pub(crate) fn compatible(kind: EffectKind, target: EffectTarget<'_>) -> Result<(), String> {
    let EffectTarget { clip, track, media } = target;
    if !matches!(clip.content, ClipContent::Media { media_id } if media_id == media.id) {
        return Err("Effect target needs matching source media".into());
    }
    let video =
        track.kind == TrackKind::Video && matches!(media.kind, MediaKind::Video | MediaKind::Image);
    let moving_video = video && media.kind == MediaKind::Video;
    let audio = (track.kind == TrackKind::Audio
        && (media.kind == MediaKind::Audio || media.kind == MediaKind::Video && media.has_audio))
        || (moving_video && clip.linked_audio && media.has_audio);
    let allowed = match kind {
        EffectKind::NoiseReduction | EffectKind::Fades => audio,
        EffectKind::Speed => (moving_video || audio) && clip.playback != Playback::Freeze,
        EffectKind::Reverse => moving_video || audio,
        EffectKind::FreezeFrame | EffectKind::Stabilization => moving_video,
        _ => video,
    };
    if allowed {
        Ok(())
    } else {
        Err(format!(
            "{} is incompatible with this clip and track",
            kind.label()
        ))
    }
}

pub(crate) fn validate_stack(target: EffectTarget<'_>) -> Result<(), String> {
    let stack = &target.clip.effect_stack;
    let mut seen = HashSet::new();
    for &kind in stack {
        if !seen.insert(kind) {
            return Err("Duplicate applied effect".into());
        }
        compatible(kind, target)?;
    }
    if stack.iter().filter(|kind| kind.is_shader()).count() > 1 {
        return Err("Only one shader effect can be applied".into());
    }
    if stack.contains(&EffectKind::Reverse) && stack.contains(&EffectKind::FreezeFrame) {
        return Err("Reverse and Freeze frame are mutually exclusive".into());
    }
    Ok(())
}

impl Clip {
    pub(crate) fn sync_playback_effects(&mut self) {
        self.effect_stack.retain(|kind| {
            !matches!(kind, EffectKind::Reverse | EffectKind::FreezeFrame)
                && !(self.playback == Playback::Freeze && *kind == EffectKind::Speed)
        });
        match self.playback {
            Playback::Reverse => self.effect_stack.push(EffectKind::Reverse),
            Playback::Freeze => self.effect_stack.push(EffectKind::FreezeFrame),
            Playback::Forward => {}
        }
    }

    pub(crate) fn normalize_effect_stack(&mut self) {
        if !self.effect_stack.is_empty() {
            return;
        }
        if let Some(effect) = &self.effect {
            let kind = match &effect.shader {
                EffectShader::Builtin(id) => EffectKind::ALL
                    .into_iter()
                    .find(|kind| kind.is_shader() && kind.id() == id)
                    .unwrap_or(EffectKind::CustomShader),
                EffectShader::File(_) => EffectKind::CustomShader,
            };
            self.effect_stack.push(kind);
        }
        for (present, kind) in [
            (self.chroma_key.is_some(), EffectKind::ChromaKey),
            (!self.color.neutral(), EffectKind::ColorCorrection),
            (self.lut.is_some(), EffectKind::ColorLut),
            (self.mask.is_some(), EffectKind::Mask),
            (self.stabilization.is_some(), EffectKind::Stabilization),
            (self.playback == Playback::Reverse, EffectKind::Reverse),
            (self.playback == Playback::Freeze, EffectKind::FreezeFrame),
            (self.speed != 1.0, EffectKind::Speed),
            (self.noise_reduction, EffectKind::NoiseReduction),
            (
                self.fade_in_frames > 0 || self.fade_out_frames > 0,
                EffectKind::Fades,
            ),
        ] {
            if present {
                self.effect_stack.push(kind);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_stable_unique_ids_and_shared_gallery_entries() {
        let mut ids = HashSet::new();
        for kind in EffectKind::ALL {
            assert!(ids.insert(kind.id()));
            assert!(!kind.label().is_empty());
            assert!(!kind.description().is_empty());
            assert_eq!(serde_json::to_value(kind).unwrap(), kind.id());
            assert!(!kind.categories().is_empty());
        }
        assert_eq!(
            EffectKind::ALL
                .iter()
                .filter(|kind| kind.categories().contains(&EffectCategory::Video))
                .count(),
            16
        );
        assert_eq!(
            EffectKind::ALL
                .iter()
                .filter(|kind| kind.categories().contains(&EffectCategory::Audio))
                .count(),
            4
        );
        assert_eq!(
            EffectKind::Reverse.categories(),
            &[EffectCategory::Video, EffectCategory::Audio]
        );
        assert_eq!(
            EffectKind::Speed.categories(),
            EffectKind::Reverse.categories()
        );
    }
}
