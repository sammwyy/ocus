use creamui::{Color, Theme, ThemeDefinition};
use directories::{ProjectDirs, UserDirs};
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ThemeMode {
    #[default]
    System,
    Light,
    Dark,
    Midnight,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Settings {
    pub use_proxies: bool,
    pub theme: ThemeMode,
    pub accent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_export_directory: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            use_proxies: true,
            theme: ThemeMode::default(),
            accent: None,
            last_export_directory: None,
        }
    }
}

impl Settings {
    pub fn theme(&self) -> Theme {
        let theme = match self.theme {
            ThemeMode::System | ThemeMode::Dark => Theme::dark(),
            ThemeMode::Light => Theme::light(),
            ThemeMode::Midnight => Theme::midnight(),
        };
        self.accent_color()
            .map_or(theme, |color| theme.with_accent(color))
    }

    pub fn accent_color(&self) -> Option<Color> {
        if self.theme == ThemeMode::System {
            return None;
        }
        let id = self.accent.as_deref().unwrap_or("orange");
        ThemeDefinition::builtin_default()
            .accent_presets
            .into_iter()
            .find(|preset| preset.id == id)
            .map(|preset| preset.color)
    }

    pub fn validate(&self) -> Result<(), String> {
        if let Some(id) = &self.accent
            && (self.theme == ThemeMode::System
                || !ThemeDefinition::builtin_default()
                    .accent_presets
                    .iter()
                    .any(|preset| &preset.id == id))
        {
            return Err("accent must be a CreamUI preset in a custom theme".into());
        }
        Ok(())
    }
}

pub(crate) fn path() -> Option<PathBuf> {
    ProjectDirs::from("", "", "Ocus").map(|dirs| dirs.config_dir().join("settings.toml"))
}

pub(crate) fn export_directory(remembered: Option<&Path>) -> PathBuf {
    let user = UserDirs::new();
    first_existing_directory([
        remembered,
        user.as_ref().and_then(UserDirs::video_dir),
        user.as_ref().and_then(UserDirs::document_dir),
        user.as_ref().map(UserDirs::home_dir),
    ])
    .or_else(|| std::env::current_dir().ok())
    .unwrap_or_default()
}

fn first_existing_directory(candidates: [Option<&Path>; 4]) -> Option<PathBuf> {
    candidates
        .into_iter()
        .flatten()
        .find(|path| path.is_absolute() && path.is_dir())
        .map(Path::to_path_buf)
}

pub(crate) fn load(path: &Path) -> Result<Settings, Box<dyn std::error::Error>> {
    match std::fs::read_to_string(path) {
        Ok(contents) => {
            let settings: Settings = toml::from_str(&contents)?;
            settings.validate()?;
            Ok(settings)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn save(path: &Path, settings: &Settings) -> Result<(), Box<dyn std::error::Error>> {
    settings.validate()?;
    std::fs::create_dir_all(path.parent().ok_or("invalid settings path")?)?;
    std::fs::write(path, toml::to_string_pretty(settings)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxies_default_on_and_preference_round_trips() {
        let defaults: Settings = toml::from_str("").unwrap();
        assert!(defaults.use_proxies);
        let settings = Settings {
            use_proxies: false,
            ..defaults
        };
        assert_eq!(
            toml::from_str::<Settings>(&toml::to_string(&settings).unwrap()).unwrap(),
            settings
        );
    }

    #[test]
    fn settings_round_trip_and_reject_invalid_accent() {
        let settings = Settings {
            theme: ThemeMode::Midnight,
            accent: Some("purple".into()),
            ..Settings::default()
        };
        let serialized = toml::to_string(&settings).unwrap();
        let decoded: Settings = toml::from_str(&serialized).unwrap();
        assert_eq!(decoded, settings);
        assert_eq!(decoded.theme().colors.accent, Color::rgb(167, 123, 255));
        assert_eq!(
            Settings {
                theme: ThemeMode::Dark,
                ..Settings::default()
            }
            .theme()
            .colors
            .accent,
            Color::rgb(224, 165, 46)
        );
        assert!(
            Settings {
                theme: ThemeMode::System,
                accent: settings.accent,
                ..Settings::default()
            }
            .validate()
            .is_err()
        );
        assert!(toml::from_str::<Settings>("theme = 'unknown'").is_err());
        assert!(
            Settings {
                accent: Some("invalid".into()),
                theme: ThemeMode::Dark,
                ..Settings::default()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn missing_settings_use_system_theme() {
        let missing = std::env::temp_dir()
            .join(format!(
                "ocus-settings-missing-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ))
            .join("settings.toml");
        assert_eq!(load(&missing).unwrap(), Settings::default());
    }

    #[test]
    fn old_preferences_do_not_require_an_export_directory() {
        let settings: Settings = toml::from_str("theme = 'dark'\naccent = 'purple'").unwrap();
        assert_eq!(settings.last_export_directory, None);
        assert_eq!(settings.theme, ThemeMode::Dark);
    }

    #[test]
    fn export_directory_prefers_existing_user_folders() {
        let root = std::env::temp_dir().join(format!(
            "ocus-export-directories-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let chosen = root.join("chosen");
        let videos = root.join("Videos");
        let documents = root.join("Documents");
        for directory in [&chosen, &videos, &documents] {
            std::fs::create_dir_all(directory).unwrap();
        }
        let mut candidates = [
            Some(chosen.as_path()),
            Some(videos.as_path()),
            Some(documents.as_path()),
            Some(root.as_path()),
        ];
        for expected in [&chosen, &videos, &documents, &root] {
            assert_eq!(
                first_existing_directory(candidates).as_ref(),
                Some(expected)
            );
            if expected != &root {
                std::fs::remove_dir(expected).unwrap();
            }
        }
        candidates[0] = Some(Path::new("relative"));
        std::fs::write(&videos, b"not a directory").unwrap();
        assert_eq!(first_existing_directory(candidates), Some(root.clone()));
        std::fs::remove_file(&videos).unwrap();
        std::fs::remove_dir(&root).unwrap();
        assert_eq!(first_existing_directory(candidates), None);
    }

    #[test]
    fn saved_preset_survives_reload() {
        let directory = std::env::temp_dir().join(format!(
            "ocus-settings-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path = directory.join("settings.toml");
        let settings = Settings {
            theme: ThemeMode::Light,
            accent: Some("orange".into()),
            last_export_directory: Some(directory.clone()),
            ..Settings::default()
        };
        save(&path, &settings).unwrap();
        assert_eq!(load(&path).unwrap(), settings);
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }
}
