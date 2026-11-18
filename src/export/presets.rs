use crate::project::Project;
use crate::project::export_settings::{AudioCodec, Container, ExportSettings, VideoCodec};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
const MAX_PRESET_BYTES: u64 = 64 * 1024;
const BUILTIN_NAMES: [&str; 8] = [
    "Project original",
    "Full HD 1080p",
    "HD 720p",
    "Portrait 9:16",
    "Square 1:1",
    "UHD 4K",
    "WebM 1080p",
    "HEVC 1080p",
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamedPreset {
    pub name: String,
    pub settings: ExportSettings,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresetLibrary {
    version: u32,
    pub presets: Vec<NamedPreset>,
}

impl Default for PresetLibrary {
    fn default() -> Self {
        Self {
            version: 1,
            presets: Vec::new(),
        }
    }
}

impl PresetLibrary {
    pub fn validate(&self) -> Result<(), String> {
        if self.version != 1 || self.presets.len() > 64 {
            return Err("Unsupported or oversized preset library".into());
        }
        let mut names = std::collections::HashSet::new();
        for preset in &self.presets {
            let name = preset.name.trim();
            if name.is_empty()
                || name.len() > 64
                || name.chars().any(char::is_control)
                || !names.insert(name.to_lowercase())
                || BUILTIN_NAMES
                    .iter()
                    .any(|builtin| builtin.eq_ignore_ascii_case(name))
            {
                return Err("Preset names must be unique and 1–64 characters long".into());
            }
            preset.settings.validate()?;
        }
        Ok(())
    }

    pub fn insert(&mut self, name: String, settings: ExportSettings) -> Result<(), String> {
        let mut updated = self.clone();
        updated.presets.push(NamedPreset { name, settings });
        updated.validate()?;
        *self = updated;
        Ok(())
    }
}

pub fn builtins(project: &Project) -> Vec<NamedPreset> {
    let video = project.settings();
    let base = ExportSettings::for_project(video.width, video.height, video.frame_rate.clone());
    [
        (video.width, video.height, 8_000, None),
        (1920, 1080, 8_000, None),
        (1280, 720, 5_000, None),
        (1080, 1920, 8_000, None),
        (1080, 1080, 6_000, None),
        (3840, 2160, 20_000, None),
        (
            1920,
            1080,
            6_000,
            Some((Container::WebM, VideoCodec::Vp9, AudioCodec::Opus)),
        ),
        (
            1920,
            1080,
            5_000,
            Some((Container::Mp4, VideoCodec::Hevc, AudioCodec::Aac)),
        ),
    ]
    .into_iter()
    .zip(BUILTIN_NAMES)
    .map(|((width, height, video_bitrate_kbps, codecs), name)| {
        let mut settings = base.clone();
        settings.width = width;
        settings.height = height;
        settings.video_bitrate_kbps = video_bitrate_kbps;
        if let Some((container, video_codec, audio_codec)) = codecs {
            settings.container = container;
            settings.video_codec = video_codec;
            settings.audio_codec = audio_codec;
        }
        NamedPreset {
            name: name.into(),
            settings,
        }
    })
    .collect()
}

pub fn path() -> Option<PathBuf> {
    crate::settings::path().map(|path| path.with_file_name("export-presets.json"))
}

pub fn load(path: &Path) -> Result<PresetLibrary, String> {
    let mut bytes = Vec::new();
    match fs::File::open(path) {
        Ok(file) => file
            .take(MAX_PRESET_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(PresetLibrary::default());
        }
        Err(error) => return Err(error.to_string()),
    };
    if bytes.len() as u64 > MAX_PRESET_BYTES {
        return Err("Preset library is too large".into());
    }
    let library: PresetLibrary =
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    library.validate()?;
    Ok(library)
}

pub fn save(path: &Path, library: &PresetLibrary) -> Result<(), String> {
    library.validate()?;
    let bytes = serde_json::to_vec_pretty(library).map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_PRESET_BYTES {
        return Err("Preset library is too large".into());
    }
    let parent = path.parent().ok_or("Invalid preset path")?;
    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let name = path.file_name().ok_or("Invalid preset filename")?;
    crate::atomic_file::write(path, parent, name, &bytes, "Could not allocate preset file")
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn save_preserves_atomic_write_behavior() {
        let directory = std::env::temp_dir().join(format!(
            "ocus-presets-atomic-{}-{}",
            std::process::id(),
            NEXT_FILE.fetch_add(1, Ordering::Relaxed)
        ));
        let parent = directory.join("nested");
        let path = parent.join("presets.json");
        let mut library = PresetLibrary::default();
        save(&path, &library).unwrap();
        assert!(load(&path).unwrap().presets.is_empty());
        library
            .insert(
                "Custom".into(),
                builtins(&Project::new("Test"))[0].settings.clone(),
            )
            .unwrap();
        save(&path, &library).unwrap();
        assert_eq!(load(&path).unwrap().presets, library.presets);
        assert_eq!(fs::read_dir(&parent).unwrap().count(), 1);
        let original = fs::read(&path).unwrap();
        let target = parent.join("directory.json");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("keep"), b"untouched").unwrap();
        assert!(save(&target, &library).is_err());
        assert_eq!(fs::read(target.join("keep")).unwrap(), b"untouched");
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(fs::read_dir(&parent).unwrap().count(), 2);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn builtin_presets_cover_landscape_portrait_and_alternate_formats() {
        let project = Project::new("Test");
        let presets = builtins(&project);
        assert!(
            presets
                .iter()
                .any(|p| p.settings.width == 1920 && p.settings.height == 1080)
        );
        assert!(
            presets
                .iter()
                .any(|p| p.settings.width == 1280 && p.settings.height == 720)
        );
        assert!(
            presets
                .iter()
                .any(|p| p.settings.width == 1080 && p.settings.height == 1920)
        );
        assert!(
            presets
                .iter()
                .any(|p| p.settings.container == Container::WebM)
        );
        assert!(presets.iter().all(|p| p.settings.validate().is_ok()));
    }

    #[test]
    fn custom_presets_round_trip_and_reject_invalid_or_duplicate_names() {
        let directory = std::env::temp_dir().join(format!(
            "ocus-presets-{}-{}",
            std::process::id(),
            NEXT_FILE.fetch_add(1, Ordering::Relaxed)
        ));
        let file = directory.join("export-presets.json");
        assert!(load(&file).unwrap().presets.is_empty());
        let mut library = PresetLibrary::default();
        let settings = builtins(&Project::new("Test"))[2].settings.clone();
        library.insert("My HD".into(), settings.clone()).unwrap();
        assert!(library.insert(" my hd ".into(), settings.clone()).is_err());
        assert!(library.insert(" ".into(), settings.clone()).is_err());
        assert!(
            library
                .insert("full hd 1080p".into(), settings.clone())
                .is_err()
        );
        assert_eq!(library.presets.len(), 1);
        save(&file, &library).unwrap();
        assert_eq!(load(&file).unwrap().presets, library.presets);
        let original = fs::read(&file).unwrap();
        let mut invalid = library.clone();
        invalid.version = 2;
        assert!(save(&file, &invalid).is_err());
        assert_eq!(fs::read(&file).unwrap(), original);
        fs::remove_file(file).unwrap();
        fs::remove_dir(directory).unwrap();
    }
}
