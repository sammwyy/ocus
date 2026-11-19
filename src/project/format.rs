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
    from_json_with_repairs(json).map(|(project, _)| project)
}

pub(super) fn from_json_with_repairs(json: &str) -> Result<(Project, usize), serde_json::Error> {
    let mut document: ProjectInput = serde_json::from_str(json)?;
    let repaired = document.project.repair_linked_audio();
    document
        .project
        .validate()
        .map_err(<serde_json::Error as serde::de::Error>::custom)?;
    Ok((document.project, repaired))
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
    fn version_one_track_metadata_is_optional_and_round_trips() {
        let original = Project::new("Legacy");
        let json = to_json(&original).unwrap();
        assert!(!json.contains("locked"));
        assert!(!json.contains("hidden"));
        assert_eq!(from_json(&json).unwrap(), original);
        let mut document: serde_json::Value = serde_json::from_str(&json).unwrap();
        document["tracks"][0]["name"] = "Main picture".into();
        document["tracks"][0]["locked"] = true.into();
        document["tracks"][0]["hidden"] = true.into();
        let project = from_json(&document.to_string()).unwrap();
        assert_eq!(project.tracks()[0].name, "Main picture");
        assert!(project.tracks()[0].locked && project.tracks()[0].hidden);
        assert_eq!(from_json(&to_json(&project).unwrap()).unwrap(), project);
        assert_eq!(PROJECT_VERSION, 1);
        for name in [" untrimmed", "line\nbreak", &"x".repeat(33)] {
            document["tracks"][0]["name"] = name.into();
            assert!(from_json(&document.to_string()).is_err());
        }
    }

    #[test]
    fn transitions_round_trip_and_validate_without_changing_version() {
        let mut document = serde_json::to_value(Project::new("Transitions")).unwrap();
        document["version"] = 1.into();
        document["tracks"][0]["clips"] = serde_json::json!([{
            "id": 1, "start_frame": 0, "source_start_frame": 0,
            "duration_frames": 3, "content": {"title": Title::default()},
            "volume": 1.0, "muted": false
        }]);
        let legacy = from_json(&document.to_string()).unwrap();
        assert!(!to_json(&legacy).unwrap().contains("transition"));
        for kind in ["fade", "dissolve", "slide"] {
            document["tracks"][0]["clips"][0]["transition"] =
                serde_json::json!({"kind": kind, "duration_frames": 20});
            let project = from_json(&document.to_string()).unwrap();
            let saved: serde_json::Value =
                serde_json::from_str(&to_json(&project).unwrap()).unwrap();
            assert_eq!(
                saved["tracks"][0]["clips"][0]["transition"],
                document["tracks"][0]["clips"][0]["transition"]
            );
            assert_eq!(saved["version"], 1);
        }
        document["tracks"][0]["clips"][0]["transition"]["duration_frames"] = 0.into();
        assert!(from_json(&document.to_string()).is_err());
        document["tracks"][0]["clips"][0]["transition"]["duration_frames"] = 1.into();
        document["tracks"][0]["kind"] = "audio".into();
        document["media"] = serde_json::json!([Media {
            id: 2,
            path: PathBuf::from("sound.wav"),
            kind: MediaKind::Audio,
            duration_us: None,
            has_audio: true,
        }]);
        document["tracks"][0]["clips"][0]["content"] =
            serde_json::json!({"media": {"media_id": 2}});
        assert!(from_json(&document.to_string()).is_err());
        document["tracks"][0]["clips"][0]
            .as_object_mut()
            .unwrap()
            .remove("transition");
        assert!(from_json(&document.to_string()).is_ok());
    }

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
            transition: None,
            fade_in_frames: 0,
            fade_out_frames: 0,
            linked_audio: false,
        }]);
        fixture["tracks"][1]["clips"] = serde_json::json!([Clip {
            id: 9,
            start_frame: 10,
            source_start_frame: 0,
            duration_frames: 48,
            content: ClipContent::Title(Title {
                text: "<b>Hello</b> **world**".into(),
                font_family: "Sans".into(),
                font_size: 48.0,
                color: [255, 150, 40, 180],
                x: 0.3,
                y: 0.7,
                bold: false,
                italic: true,
                outline: true,
                parse_html: true,
                parse_markdown: true,
            }),
            volume: 1.0,
            muted: false,
            transition: None,
            fade_in_frames: 0,
            fade_out_frames: 0,
            linked_audio: false,
        }]);
        let project = from_json(&fixture.to_string()).unwrap();
        assert_eq!(from_json(&to_json(&project).unwrap()).unwrap(), project);
    }

    #[test]
    fn markers_round_trip_and_unordered_markers_are_rejected() {
        let old = to_json(&Project::new("Old")).unwrap();
        assert!(!old.contains("markers"));
        assert!(from_json(&old).unwrap().markers().is_empty());
        let mut editor = crate::project::Editor::new(Project::new("Marked"));
        assert!(editor.apply(crate::project::Command::AddMarker(48)));
        assert!(editor.apply(crate::project::Command::AddMarker(12)));
        let json = to_json(editor.project()).unwrap();
        let project = from_json(&json).unwrap();
        assert_eq!(&project, editor.project());
        let mut document: serde_json::Value = serde_json::from_str(&json).unwrap();
        document["markers"] = serde_json::json!([{ "frame": 48 }, { "frame": 12 }]);
        assert!(from_json(&document.to_string()).is_err());
        document["markers"] = serde_json::json!([{ "frame": 12 }, { "frame": 12 }]);
        assert!(from_json(&document.to_string()).is_err());
    }

    #[test]
    fn export_options_round_trip_without_breaking_existing_projects() {
        use crate::project::export_settings::{
            AudioCodec, Container, ExportSettings, LastExport, VideoCodec,
        };
        let old = to_json(&Project::new("Old")).unwrap();
        assert!(from_json(&old).unwrap().last_export().is_none());
        let mut editor = crate::project::Editor::new(Project::new("New"));
        let mut settings =
            ExportSettings::for_project(1920, 1080, editor.project().settings().frame_rate.clone());
        settings.container = Container::WebM;
        settings.video_codec = VideoCodec::Vp9;
        settings.audio_codec = AudioCodec::Opus;
        let last = LastExport {
            settings: settings.clone(),
            path: std::env::temp_dir().join("video.webm"),
        };
        assert!(editor.apply(crate::project::Command::SetLastExport(Some(last.clone()))));
        assert_eq!(
            from_json(&to_json(editor.project()).unwrap())
                .unwrap()
                .last_export(),
            Some(&last)
        );
        let mut older_export: serde_json::Value =
            serde_json::from_str(&to_json(editor.project()).unwrap()).unwrap();
        let fields = older_export["last_export"]["settings"]
            .as_object_mut()
            .unwrap();
        fields.remove("video_quality");
        fields.remove("speed");
        assert_eq!(
            from_json(&older_export.to_string())
                .unwrap()
                .last_export()
                .unwrap()
                .settings,
            settings
        );
        assert!(editor.undo());
        assert!(editor.project().last_export().is_none());
        assert!(editor.redo());
        assert!(editor.project().last_export().is_some());
        let mut malformed: serde_json::Value =
            serde_json::from_str(&to_json(editor.project()).unwrap()).unwrap();
        malformed["last_export"]["settings"]["audio_codec"] = "aac".into();
        assert!(from_json(&malformed.to_string()).is_err());
        malformed["last_export"]["settings"]["audio_codec"] = "opus".into();
        malformed["last_export"]["path"] = "relative.mp4".into();
        assert!(from_json(&malformed.to_string()).is_err());
    }

    #[test]
    fn older_titles_load_without_new_style_fields() {
        let mut document: serde_json::Value =
            serde_json::from_str(&to_json(&Project::new("Legacy title")).unwrap()).unwrap();
        let mut clip = serde_json::to_value(Clip {
            id: 1,
            start_frame: 0,
            source_start_frame: 0,
            duration_frames: 90,
            content: ClipContent::Title(Title::default()),
            volume: 1.0,
            muted: false,
            transition: None,
            fade_in_frames: 0,
            fade_out_frames: 0,
            linked_audio: false,
        })
        .unwrap();
        for field in ["bold", "italic", "outline", "parse_html", "parse_markdown"] {
            clip["content"]["title"]
                .as_object_mut()
                .unwrap()
                .remove(field);
        }
        clip["content"]["title"]["font_size"] = serde_json::json!(640.0);
        document["tracks"][0]["clips"] = serde_json::json!([clip]);
        let project = from_json(&document.to_string()).unwrap();
        assert_eq!(
            project.tracks()[0].clips[0].content,
            ClipContent::Title(Title {
                font_size: 640.0,
                ..Title::default()
            })
        );
    }

    #[test]
    fn unsupported_and_missing_project_versions_are_rejected() {
        let json = to_json(&Project::new("Untitled")).unwrap();
        assert!(from_json(&json.replace("\"version\": 1", "\"version\": 2")).is_err());
        assert!(from_json(&json.replace("\"version\": 1,", "")).is_err());
    }

    #[test]
    fn stale_linked_audio_is_repaired_without_relaxing_other_validation() {
        let mut document: serde_json::Value =
            serde_json::from_str(&to_json(&Project::new("Stale sound")).unwrap()).unwrap();
        document["media"] = serde_json::json!([{
            "id": 4, "path": "relative.mp4", "kind": "video", "has_audio": false
        }]);
        document["tracks"][0]["clips"] = serde_json::json!([{
            "id": 1, "start_frame": 0, "source_start_frame": 0,
            "duration_frames": 30, "content": {"media": {"media_id": 4}},
            "volume": 1.0, "muted": false, "linked_audio": true
        }]);
        let project = from_json(&document.to_string()).unwrap();
        assert!(!project.tracks()[0].clips[0].linked_audio);
        assert_eq!(project.media()[0].path, PathBuf::from("relative.mp4"));
        document["tracks"][0]["clips"][0]["duration_frames"] = 0.into();
        assert!(from_json(&document.to_string()).is_err());
        document["tracks"][0]["clips"][0]["duration_frames"] = 30.into();
        document["tracks"][0]["clips"][0]["content"]["media"]["media_id"] = 99.into();
        assert!(from_json(&document.to_string()).is_err());
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
            transition: None,
            fade_in_frames: 0,
            fade_out_frames: 0,
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
        assert!(!from_json(&document.to_string()).unwrap().tracks()[0].clips[0].linked_audio);
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
