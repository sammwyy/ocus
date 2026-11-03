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
            duration_us: Some(3_200_000),
            has_audio: false,
        }]);
        fixture["tracks"][0]["clips"] = serde_json::json!([Clip {
            id: 8,
            start_frame: 0,
            source_start_frame: 24,
            duration_frames: 72,
            content: ClipContent::Media { media_id: 4 },
            volume: 1.0,
            muted: false,
            linked_audio: false,
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
            linked_audio: false,
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
    fn old_media_references_without_probe_metadata_remain_readable() {
        let mut document: serde_json::Value =
            serde_json::from_str(&to_json(&Project::new("Legacy")).unwrap()).unwrap();
        document["media"] = serde_json::json!([{
            "id": 4, "path": "footage/old.mp4", "kind": "video"
        }]);
        let project = from_json(&document.to_string()).unwrap();
        assert_eq!(project.media()[0].duration_us, None);
        assert!(!project.media()[0].has_audio);
        assert_eq!(from_json(&to_json(&project).unwrap()).unwrap(), project);
    }

    #[test]
    fn paired_video_and_audio_clips_round_trip_with_probe_metadata() {
        let mut document: serde_json::Value =
            serde_json::from_str(&to_json(&Project::new("Paired")).unwrap()).unwrap();
        document["media"] = serde_json::json!([Media {
            id: 8,
            path: PathBuf::from("footage/video.mp4"),
            kind: MediaKind::Video,
            duration_us: Some(1_000_000),
            has_audio: true,
        }]);
        let clip = Clip {
            id: 9,
            start_frame: 0,
            source_start_frame: 0,
            duration_frames: 30,
            content: ClipContent::Media { media_id: 8 },
            volume: 1.0,
            muted: false,
            linked_audio: false,
        };
        document["tracks"][0]["clips"] = serde_json::json!([clip.clone()]);
        document["tracks"][2]["clips"] = serde_json::json!([Clip { id: 10, ..clip }]);
        let project = from_json(&document.to_string()).unwrap();
        assert_eq!(from_json(&to_json(&project).unwrap()).unwrap(), project);
        assert!(!to_json(&project).unwrap().contains("linked_audio"));

        document["tracks"][2]["clips"] = serde_json::json!([]);
        document["tracks"][0]["clips"][0]["linked_audio"] = serde_json::json!(true);
        let linked = from_json(&document.to_string()).unwrap();
        assert!(linked.tracks()[0].clips[0].linked_audio);
        assert_eq!(from_json(&to_json(&linked).unwrap()).unwrap(), linked);
        document["media"][0]["has_audio"] = serde_json::json!(false);
        assert!(from_json(&document.to_string()).is_err());
        document["media"][0]["has_audio"] = serde_json::json!(true);
        document["tracks"][2]["clips"] = document["tracks"][0]["clips"].clone();
        document["tracks"][0]["clips"] = serde_json::json!([]);
        assert!(from_json(&document.to_string()).is_err());
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
