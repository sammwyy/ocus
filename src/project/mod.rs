use crate::media::Media;
use crate::timeline::{Track, TrackKind};
use serde::{Deserialize, Serialize};

mod edit;
pub mod format;

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
