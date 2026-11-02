use super::Project;
use serde::{Deserialize, Deserializer, Serialize, de::Error};

pub const PROJECT_VERSION: u32 = 1;

fn deserialize_version<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u32, D::Error> {
    let version = u32::deserialize(deserializer)?;
    if version != PROJECT_VERSION {
        return Err(D::Error::custom(format!(
            "unsupported project version {version}"
        )));
    }
    Ok(version)
}

#[derive(Serialize)]
struct ProjectOutput<'a> {
    version: u32,
    #[serde(flatten)]
    project: &'a Project,
}

#[derive(Deserialize)]
struct ProjectInput {
    #[serde(rename = "version", deserialize_with = "deserialize_version")]
    _version: u32,
    #[serde(flatten)]
    project: Project,
}

pub fn from_json(json: &str) -> Result<Project, serde_json::Error> {
    let document: ProjectInput = serde_json::from_str(json)?;
    document
        .project
        .validate()
        .map_err(<serde_json::Error as serde::de::Error>::custom)?;
    Ok(document.project)
}

pub fn to_json(project: &Project) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(&ProjectOutput {
        version: PROJECT_VERSION,
        project,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::{Media, MediaKind};
    use crate::timeline::{Clip, ClipContent, Title};
    use std::path::PathBuf;

    #[test]
    fn project_round_trips_media_and_title_clips() {
        let mut fixture: serde_json::Value =
            serde_json::from_str(&to_json(&Project::new("A short film")).unwrap()).unwrap();
        fixture["media"] = serde_json::json!([Media {
            id: 4,
            path: PathBuf::from("footage/scene.mp4"),
            kind: MediaKind::Video,
        }]);
        fixture["tracks"][0]["clips"] = serde_json::json!([Clip {
            id: 8,
            start_frame: 0,
            source_start_frame: 24,
            duration_frames: 72,
            content: ClipContent::Media { media_id: 4 },
            volume: 1.0,
            muted: false,
        }]);
        fixture["tracks"][1]["clips"] = serde_json::json!([Clip {
            id: 9,
            start_frame: 10,
            source_start_frame: 0,
            duration_frames: 48,
            content: ClipContent::Title(Title {
                text: "Hello".into(),
                font_family: "Sans".into(),
                font_size: 48.0,
                color: [255, 255, 255, 255],
                x: 0.5,
                y: 0.5,
            }),
            volume: 1.0,
            muted: false,
        }]);
        let project = from_json(&fixture.to_string()).unwrap();
        assert_eq!(from_json(&to_json(&project).unwrap()).unwrap(), project);
    }

    #[test]
    fn unsupported_and_missing_project_versions_are_rejected() {
        let json = to_json(&Project::new("Untitled")).unwrap();
        assert!(from_json(&json.replace("\"version\": 1", "\"version\": 2")).is_err());
        assert!(from_json(&json.replace("\"version\": 1,", "")).is_err());
    }

    #[test]
    fn malformed_settings_and_tracks_are_rejected() {
        let original: serde_json::Value =
            serde_json::from_str(&to_json(&Project::new("Film")).unwrap()).unwrap();
        for (field, value) in [
            ("/settings/frame_rate/denominator", serde_json::json!(0)),
            ("/settings/width", serde_json::json!(0)),
            ("/tracks/1/id", serde_json::json!(0)),
        ] {
            let mut document = original.clone();
            *document.pointer_mut(field).unwrap() = value;
            assert!(from_json(&document.to_string()).is_err(), "{field}");
        }
    }
}
