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
    for track in &mut document.project.tracks {
        let legacy: Vec<_> = track
            .clips
            .iter()
            .map(|clip| clip.effect_stack.is_empty())
            .collect();
        for clip in &mut track.clips {
            clip.normalize_effect_stack();
        }
        let stacks: Vec<_> = track.clips.iter().zip(legacy).map(|(clip, legacy)| {
            legacy.then(|| {
                let media = document.project.media.iter().find(|media| {
                    matches!(clip.content, crate::timeline::ClipContent::Media { media_id } if media_id == media.id)
                });
                clip.effect_stack.iter().copied().filter(|kind| {
                    media.is_some_and(|media| crate::timeline::effects::compatible(
                        *kind, crate::timeline::effects::EffectTarget { clip, track, media },
                    ).is_ok())
                }).collect()
            })
        }).collect();
        for (clip, stack) in track.clips.iter_mut().zip(stacks) {
            if let Some(stack) = stack {
                clip.effect_stack = stack;
            }
        }
    }
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
    use crate::timeline::{Clip, ClipContent, Title, Transform};
    use std::path::PathBuf;

    fn effect_fixture() -> serde_json::Value {
        let mut value = serde_json::to_value(Project::new("Effects")).unwrap();
        value["version"] = serde_json::json!(1);
        value["media"] = serde_json::json!([{
            "id": 1, "path": "video.mp4", "kind": "video",
            "duration_us": 10_000_000, "has_audio": true
        }]);
        value["tracks"][0]["clips"] = serde_json::json!([{
            "id": 1, "start_frame": 0, "source_start_frame": 0, "duration_frames": 60,
            "content": {"media": {"media_id": 1}}, "volume": 1.0, "muted": false,
            "linked_audio": true
        }]);
        value
    }

    #[test]
    fn legacy_effect_fields_derive_membership_and_round_trip_in_version_one() {
        use crate::timeline::effects::EffectKind;
        let original = effect_fixture();
        let empty = from_json(&original.to_string()).unwrap();
        assert!(empty.tracks[0].clips[0].effect_stack.is_empty());
        assert!(!to_json(&empty).unwrap().contains("effect_stack"));
        let cases = [
            (
                "chroma_key",
                serde_json::to_value(crate::timeline::ChromaKey::default()).unwrap(),
                EffectKind::ChromaKey,
            ),
            (
                "color",
                serde_json::json!({"brightness": 0.1, "contrast": 1.0, "saturation": 1.0}),
                EffectKind::ColorCorrection,
            ),
            ("lut", serde_json::json!("look.cube"), EffectKind::ColorLut),
            (
                "mask",
                serde_json::to_value(crate::timeline::Mask::default()).unwrap(),
                EffectKind::Mask,
            ),
            (
                "stabilization",
                serde_json::to_value(crate::timeline::Stabilization::new(0, vec![[0.1, 0.2]]))
                    .unwrap(),
                EffectKind::Stabilization,
            ),
            ("speed", serde_json::json!(2.0), EffectKind::Speed),
            ("fade_in_frames", serde_json::json!(10), EffectKind::Fades),
            ("fade_out_frames", serde_json::json!(10), EffectKind::Fades),
            (
                "noise_reduction",
                serde_json::json!(true),
                EffectKind::NoiseReduction,
            ),
            (
                "playback",
                serde_json::json!("reverse"),
                EffectKind::Reverse,
            ),
            (
                "playback",
                serde_json::json!("freeze"),
                EffectKind::FreezeFrame,
            ),
        ];
        for (field, parameter, kind) in cases {
            let mut value = original.clone();
            value["tracks"][0]["clips"][0][field] = parameter;
            let project = from_json(&value.to_string()).unwrap();
            assert_eq!(
                project.tracks[0].clips[0].effect_stack,
                vec![kind],
                "{field}"
            );
            assert_eq!(from_json(&to_json(&project).unwrap()).unwrap(), project);
        }
        for kind in EffectKind::ALL.into_iter().filter(|kind| kind.is_shader()) {
            let mut value = original.clone();
            let shader = if kind == EffectKind::CustomShader {
                serde_json::json!({"file": "effect.wgsl"})
            } else {
                serde_json::json!({"builtin": kind.id()})
            };
            value["tracks"][0]["clips"][0]["effect"] = serde_json::json!({"shader": shader});
            let project = from_json(&value.to_string()).unwrap();
            assert_eq!(project.tracks[0].clips[0].effect_stack, vec![kind]);
            assert_eq!(
                project.tracks[0].clips[0].effect.as_ref().unwrap().amount,
                1.0
            );
            assert_eq!(from_json(&to_json(&project).unwrap()).unwrap(), project);
        }
    }

    #[test]
    fn legacy_incompatible_effects_stay_dormant_without_losing_parameters() {
        use crate::timeline::effects::EffectKind;
        let mut value = effect_fixture();
        value["media"][0]["has_audio"] = serde_json::json!(false);
        value["tracks"][0]["clips"][0]["noise_reduction"] = serde_json::json!(true);
        value["tracks"][0]["clips"][0]["fade_in_frames"] = serde_json::json!(10);
        value["tracks"][0]["clips"][0]["color"] = serde_json::json!({
            "brightness": 0.1, "contrast": 1.0, "saturation": 1.0
        });
        let restored = from_json(&value.to_string()).unwrap();
        let clip = &restored.tracks[0].clips[0];
        assert!(!clip.linked_audio);
        assert!(clip.noise_reduction);
        assert_eq!(clip.fade_in_frames, 10);
        assert_eq!(clip.effect_stack, vec![EffectKind::ColorCorrection]);
        assert_eq!(from_json(&to_json(&restored).unwrap()).unwrap(), restored);
        value["tracks"][0]["clips"][0]
            .as_object_mut()
            .unwrap()
            .remove("color");
        let dormant = from_json(&value.to_string()).unwrap();
        assert!(dormant.tracks[0].clips[0].effect_stack.is_empty());
        assert_eq!(from_json(&to_json(&dormant).unwrap()).unwrap(), dormant);
        value["tracks"][0]["clips"][0]["effect_stack"] = serde_json::json!(["noise_reduction"]);
        assert!(
            from_json(&value.to_string())
                .unwrap_err()
                .to_string()
                .contains("incompatible")
        );
    }

    #[test]
    fn ordered_neutral_and_pending_effect_membership_is_not_rederived() {
        use crate::timeline::effects::EffectKind;
        let mut value = effect_fixture();
        let stack = vec![
            EffectKind::Stabilization,
            EffectKind::Fades,
            EffectKind::ColorCorrection,
            EffectKind::Speed,
        ];
        value["tracks"][0]["clips"][0]["effect_stack"] = serde_json::to_value(&stack).unwrap();
        let project = from_json(&value.to_string()).unwrap();
        assert_eq!(project.tracks[0].clips[0].effect_stack, stack);
        assert_eq!(project.tracks[0].clips[0].speed, 1.0);
        assert!(project.tracks[0].clips[0].color.neutral());
        assert!(project.tracks[0].clips[0].stabilization.is_none());
        let json = to_json(&project).unwrap();
        assert!(json.contains("\"version\": 1"));
        assert_eq!(from_json(&json).unwrap(), project);
    }

    #[test]
    fn invalid_effect_memberships_are_rejected_on_load_and_validation() {
        let original = effect_fixture();
        for (stack, message) in [
            (
                serde_json::json!(["blur", "blur"]),
                "Duplicate applied effect",
            ),
            (
                serde_json::json!(["blur", "sepia"]),
                "Only one shader effect can be applied",
            ),
            (
                serde_json::json!(["reverse", "freeze_frame"]),
                "Reverse and Freeze frame are mutually exclusive",
            ),
        ] {
            let mut value = original.clone();
            value["tracks"][0]["clips"][0]["effect_stack"] = stack;
            assert!(
                from_json(&value.to_string())
                    .unwrap_err()
                    .to_string()
                    .contains(message)
            );
            let project: Project = serde_json::from_value(value).unwrap();
            assert_eq!(project.validate().unwrap_err(), message);
        }
        for (media_kind, track_kind, stack, playback) in [
            ("image", "video", "speed", "forward"),
            ("image", "video", "reverse", "forward"),
            ("image", "video", "stabilization", "forward"),
            ("audio", "audio", "blur", "forward"),
            ("video", "video", "speed", "freeze"),
        ] {
            let mut value = original.clone();
            value["media"][0]["kind"] = serde_json::json!(media_kind);
            value["tracks"][0]["kind"] = serde_json::json!(track_kind);
            value["tracks"][0]["clips"][0]["linked_audio"] = serde_json::json!(false);
            value["tracks"][0]["clips"][0]["effect_stack"] = serde_json::json!([stack]);
            value["tracks"][0]["clips"][0]["playback"] = serde_json::json!(playback);
            assert!(
                from_json(&value.to_string())
                    .unwrap_err()
                    .to_string()
                    .contains("incompatible")
            );
        }
        let mut value = original.clone();
        value["tracks"][0]["clips"][0]["linked_audio"] = serde_json::json!(false);
        value["tracks"][0]["clips"][0]["effect_stack"] = serde_json::json!(["noise_reduction"]);
        assert!(from_json(&value.to_string()).is_err());
        value["tracks"][0]["clips"][0]["effect_stack"] = serde_json::json!(["unknown_effect"]);
        assert!(from_json(&value.to_string()).is_err());
    }

    #[test]
    fn subtitle_fields_round_trip_without_a_version_change() {
        let project = Project::new("Legacy");
        let legacy = to_json(&project).unwrap();
        assert!(!legacy.contains("subtitles"));
        let loaded = from_json(&legacy).unwrap();
        assert!(loaded.tracks().iter().all(|track| !track.subtitles));
        let mut tracks = loaded.tracks().to_vec();
        tracks.push(
            crate::subtitles::track(
                &loaded,
                &[crate::subtitles::Cue {
                    start_us: 0,
                    end_us: 1_000_000,
                    text: "Hello".into(),
                }],
            )
            .unwrap(),
        );
        let ClipContent::Title(title) = &mut tracks.last_mut().unwrap().clips[0].content else {
            unreachable!()
        };
        title.background = Some([0, 0, 0, 180]);
        let mut editor = crate::project::Editor::new(loaded);
        assert!(editor.apply(crate::project::Command::ReplaceTracks(tracks)));
        let json = to_json(editor.project()).unwrap();
        assert!(json.contains("\"version\": 1"));
        let restored = from_json(&json).unwrap();
        assert_eq!(restored.tracks(), editor.project().tracks());
        let mut document: serde_json::Value = serde_json::from_str(&json).unwrap();
        document["tracks"][4]["clips"][0]["content"]["title"]
            .as_object_mut()
            .unwrap()
            .remove("background");
        document["tracks"][4]
            .as_object_mut()
            .unwrap()
            .remove("subtitles");
        let restored = from_json(&document.to_string()).unwrap();
        assert!(!restored.tracks()[4].subtitles);
        let ClipContent::Title(title) = &restored.tracks()[4].clips[0].content else {
            unreachable!()
        };
        assert_eq!(title.background, None);
    }

    #[test]
    fn out_transitions_and_slide_directions_round_trip_and_validate() {
        use crate::timeline::SlideDirection;
        let mut document = serde_json::to_value(Project::new("Out transitions")).unwrap();
        document["version"] = 1.into();
        document["tracks"][0]["clips"] = serde_json::json!([{
            "id": 1, "start_frame": 0, "source_start_frame": 0,
            "duration_frames": 3, "content": {"title": Title::default()},
            "volume": 1.0, "muted": false,
            "transition": {"kind": "slide", "duration_frames": 3}
        }]);
        let legacy = from_json(&document.to_string()).unwrap();
        let clip = &legacy.tracks()[0].clips[0];
        assert!(clip.transition_out.is_none());
        assert_eq!(
            clip.transition.as_ref().unwrap().direction,
            SlideDirection::Right
        );
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&to_json(&legacy).unwrap()).unwrap()["tracks"]
                [0]["clips"][0]["transition"],
            document["tracks"][0]["clips"][0]["transition"]
        );
        for direction in ["left", "right", "up", "down"] {
            document["tracks"][0]["clips"][0]["transition_out"] =
                serde_json::json!({"kind": "slide", "duration_frames": 3, "direction": direction});
            let project = from_json(&document.to_string()).unwrap();
            assert_eq!(from_json(&to_json(&project).unwrap()).unwrap(), project);
        }
        document["tracks"][0]["clips"][0]["transition_out"]["direction"] = "diagonal".into();
        assert!(from_json(&document.to_string()).is_err());
        document["tracks"][0]["clips"][0]["transition_out"]["direction"] = "right".into();
        document["tracks"][0]["clips"][0]["transition_out"]["duration_frames"] = 0.into();
        assert!(from_json(&document.to_string()).is_err());
        document["tracks"][0]["clips"][0]["transition_out"]["duration_frames"] = 1.into();
        document["tracks"][0]["clips"][0]
            .as_object_mut()
            .unwrap()
            .remove("transition");
        document["tracks"][0]["kind"] = "audio".into();
        document["media"] = serde_json::json!([Media {
            id: 2,
            path: "sound.wav".into(),
            kind: MediaKind::Audio,
            duration_us: None,
            has_audio: true
        }]);
        document["tracks"][0]["clips"][0]["content"] =
            serde_json::json!({"media": {"media_id": 2}});
        assert!(from_json(&document.to_string()).is_err());
        document["tracks"][0]["clips"][0]
            .as_object_mut()
            .unwrap()
            .remove("transition_out");
        assert!(from_json(&document.to_string()).is_ok());
    }

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
            effect_stack: Vec::new(),
            playback: crate::timeline::Playback::Forward,
            effect: None,
            mask: None,
            lut: None,
            chroma_key: None,
            id: 8,
            start_frame: 0,
            source_start_frame: 24,
            duration_frames: 72,
            content: ClipContent::Media { media_id: 4 },
            volume: 1.0,
            muted: false,
            speed: 1.0,
            color: Default::default(),
            transition: None,
            transition_out: None,
            fade_in_frames: 0,
            fade_out_frames: 0,
            noise_reduction: false,
            linked_audio: false,
            stabilization: None,
            keyframes: Vec::new(),
            transform: Transform::default(),
        }]);
        fixture["tracks"][1]["clips"] = serde_json::json!([Clip {
            effect_stack: Vec::new(),
            playback: crate::timeline::Playback::Forward,
            effect: None,
            mask: None,
            lut: None,
            chroma_key: None,
            id: 9,
            start_frame: 10,
            source_start_frame: 0,
            duration_frames: 48,
            content: ClipContent::Title(Title {
                animation: crate::timeline::TitleAnimation::Fade,
                text: "<b>Hello</b> **world**".into(),
                font_family: "Sans".into(),
                font_size: 48.0,
                color: [255, 150, 40, 180],
                background: Some([0, 0, 0, 128]),
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
            speed: 1.0,
            color: Default::default(),
            transition: None,
            transition_out: None,
            fade_in_frames: 0,
            fade_out_frames: 0,
            noise_reduction: false,
            linked_audio: false,
            stabilization: None,
            keyframes: Vec::new(),
            transform: Transform::default(),
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
            effect_stack: Vec::new(),
            playback: crate::timeline::Playback::Forward,
            effect: None,
            mask: None,
            lut: None,
            chroma_key: None,
            id: 1,
            start_frame: 0,
            source_start_frame: 0,
            duration_frames: 90,
            content: ClipContent::Title(Title::default()),
            volume: 1.0,
            muted: false,
            speed: 1.0,
            color: Default::default(),
            transition: None,
            transition_out: None,
            fade_in_frames: 0,
            fade_out_frames: 0,
            noise_reduction: false,
            linked_audio: false,
            stabilization: None,
            keyframes: Vec::new(),
            transform: Transform::default(),
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
            effect_stack: Vec::new(),
            playback: crate::timeline::Playback::Forward,
            effect: None,
            mask: None,
            lut: None,
            chroma_key: None,
            id: 9,
            start_frame: 0,
            source_start_frame: 0,
            duration_frames: 30,
            content: ClipContent::Media { media_id: 8 },
            volume: 1.0,
            muted: false,
            speed: 1.0,
            color: Default::default(),
            transition: None,
            transition_out: None,
            fade_in_frames: 0,
            fade_out_frames: 0,
            noise_reduction: false,
            linked_audio: false,
            stabilization: None,
            keyframes: Vec::new(),
            transform: Transform::default(),
        };
        document["tracks"][0]["clips"] = serde_json::json!([clip.clone()]);
        document["tracks"][2]["clips"] = serde_json::json!([Clip { id: 10, ..clip }]);
        let project = from_json(&document.to_string()).unwrap();
        assert_eq!(from_json(&to_json(&project).unwrap()).unwrap(), project);
        assert!(!to_json(&project).unwrap().contains("linked_audio"));
        assert!(!to_json(&project).unwrap().contains("transform"));
        document["tracks"][0]["clips"][0]["transform"] = serde_json::json!(Transform {
            scale: 0.5,
            rotation: 90.0,
            crop_left: 0.1,
            opacity: 0.5,
            ..Default::default()
        });
        let transformed = from_json(&document.to_string()).unwrap();
        assert_eq!(
            from_json(&to_json(&transformed).unwrap()).unwrap(),
            transformed
        );
        document["tracks"][0]["clips"][0]["transform"]["opacity"] = serde_json::json!(2.0);
        assert!(from_json(&document.to_string()).is_err());
        document["tracks"][0]["clips"][0]
            .as_object_mut()
            .unwrap()
            .remove("transform");

        document["tracks"][2]["clips"] = serde_json::json!([]);
        document["tracks"][0]["clips"][0]["linked_audio"] = serde_json::json!(true);
        let linked = from_json(&document.to_string()).unwrap();
        assert!(linked.tracks()[0].clips[0].linked_audio);
        assert!(!linked.tracks()[0].clips[0].noise_reduction);
        assert!(!to_json(&linked).unwrap().contains("noise_reduction"));
        document["tracks"][0]["clips"][0]["noise_reduction"] = true.into();
        let enabled = from_json(&document.to_string()).unwrap();
        assert!(enabled.tracks()[0].clips[0].noise_reduction);
        assert_eq!(from_json(&to_json(&enabled).unwrap()).unwrap(), enabled);
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
