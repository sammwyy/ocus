use crate::media::{Media, MediaKind};
use crate::timeline::{ClipContent, Track, TrackKind};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

mod edit;
pub mod format;
pub mod store;

pub use edit::{Command, Editor};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Project {
    name: String,
    settings: VideoSettings,
    media: Vec<Media>,
    tracks: Vec<Track>,
}

impl Project {
    pub fn new(name: impl Into<String>) -> Self {
        let tracks = [
            TrackKind::Video,
            TrackKind::Video,
            TrackKind::Audio,
            TrackKind::Audio,
        ]
        .into_iter()
        .enumerate()
        .map(|(id, kind)| Track {
            id: id as u64,
            kind,
            clips: Vec::new(),
        })
        .collect();
        Self {
            name: name.into(),
            settings: VideoSettings::default(),
            media: Vec::new(),
            tracks,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn settings(&self) -> &VideoSettings {
        &self.settings
    }

    pub fn media(&self) -> &[Media] {
        &self.media
    }

    pub fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    pub(crate) fn replace_name(&mut self, name: String) -> String {
        std::mem::replace(&mut self.name, name)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty()
            || self.settings.width == 0
            || self.settings.height == 0
            || self.settings.frame_rate.numerator == 0
            || self.settings.frame_rate.denominator == 0
        {
            return Err("Invalid project name or video settings".into());
        }
        let mut media_ids = HashSet::new();
        for media in &self.media {
            if !media_ids.insert(media.id)
                || media.path.as_os_str().is_empty()
                || media.duration_us == Some(0)
            {
                return Err("Duplicate media ID or invalid media metadata".into());
            }
        }
        let mut track_ids = HashSet::new();
        let mut clip_ids = HashSet::new();
        for track in &self.tracks {
            if !track_ids.insert(track.id) {
                return Err("Duplicate track ID".into());
            }
            let mut end = 0;
            for clip in &track.clips {
                let next = clip
                    .start_frame
                    .checked_add(clip.duration_frames)
                    .ok_or("Clip range overflows")?;
                clip.source_start_frame
                    .checked_add(clip.duration_frames)
                    .ok_or("Clip source range overflows")?;
                if clip.duration_frames == 0
                    || clip.start_frame < end
                    || !clip_ids.insert(clip.id)
                    || !clip.volume.is_finite()
                    || !(0.0..=2.0).contains(&clip.volume)
                {
                    return Err("Invalid or overlapping clip".into());
                }
                end = next;
                match &clip.content {
                    ClipContent::Title(title)
                        if track.kind == TrackKind::Video && title.valid() => {}
                    ClipContent::Media { media_id }
                        if self.media.iter().any(|media| {
                            media.id == *media_id && self.media_clip_fits(media, track.kind, clip)
                        }) => {}
                    _ => return Err("Invalid title or incompatible media on track".into()),
                }
                if clip.linked_audio
                    && !(track.kind == TrackKind::Video
                        && matches!(clip.content, ClipContent::Media { media_id }
                            if self.media.iter().any(|media| media.id == media_id
                                && media.kind == MediaKind::Video
                                && media.has_audio)))
                {
                    return Err("Linked audio needs a video source with sound".into());
                }
            }
        }
        Ok(())
    }

    pub(crate) fn media_clip_fits(
        &self,
        media: &Media,
        kind: TrackKind,
        clip: &crate::timeline::Clip,
    ) -> bool {
        if !(matches!(
            (kind, media.kind),
            (TrackKind::Video, MediaKind::Video | MediaKind::Image)
                | (TrackKind::Audio, MediaKind::Audio)
        ) || kind == TrackKind::Audio && media.kind == MediaKind::Video && media.has_audio)
        {
            return false;
        }
        let Some(duration_us) = media.duration_us else {
            return true;
        };
        if media.kind == MediaKind::Image {
            return true;
        }
        let max_frames = u128::from(duration_us) * u128::from(self.settings.frame_rate.numerator)
            / (1_000_000_u128 * u128::from(self.settings.frame_rate.denominator));
        u128::from(clip.source_start_frame) + u128::from(clip.duration_frames) <= max_frames
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VideoSettings {
    pub width: u32,
    pub height: u32,
    pub frame_rate: FrameRate,
}

impl Default for VideoSettings {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            frame_rate: FrameRate {
                numerator: 30,
                denominator: 1,
            },
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FrameRate {
    pub numerator: u32,
    pub denominator: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_project_starts_with_two_tracks_of_each_kind() {
        let project = Project::new("Untitled");
        assert_eq!(project.tracks().len(), 4);
        assert_eq!(
            project
                .tracks()
                .iter()
                .filter(|track| track.kind == TrackKind::Video)
                .count(),
            2
        );
        assert_eq!(
            project
                .tracks()
                .iter()
                .filter(|track| track.kind == TrackKind::Audio)
                .count(),
            2
        );
    }
}
