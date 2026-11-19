use super::{Project, format};
use crate::media::{Media, MediaKind};
use crate::timeline::ClipContent;
use std::fs;
use std::io::{self, Read};
use std::path::Path;
pub(super) const MAX_PROJECT_BYTES: u64 = 16 * 1024 * 1024;

pub fn load(path: &Path) -> io::Result<Project> {
    load_with_repairs(path).map(|(project, _)| project)
}

pub fn load_with_repairs(path: &Path) -> io::Result<(Project, usize)> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(MAX_PROJECT_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_PROJECT_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Project file is too large",
        ));
    }
    let json = String::from_utf8(bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let (mut project, mut repaired) = format::from_json_with_repairs(&json)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    repaired += migrate_media(&mut project, Some(path))?;
    Ok((project, repaired))
}

pub(super) fn migrate_media(project: &mut Project, path: Option<&Path>) -> io::Result<usize> {
    if let Some(path) = path {
        let absolute = std::path::absolute(path)?;
        let parent = absolute
            .parent()
            .ok_or_else(|| io::Error::other("Missing project directory"))?;
        for media in &mut project.media {
            if media.path.is_relative() {
                media.path = parent.join(&media.path);
            }
        }
    }
    let mut repaired = 0;
    for index in 0..project.media.len() {
        let source = &project.media[index];
        if !source.path.is_file() {
            continue;
        }
        let Ok(info) = crate::media::probe(&source.path) else {
            continue;
        };
        if info.kind != source.kind {
            continue;
        }
        let replacement = Media {
            duration_us: info.duration_us,
            has_audio: info.has_audio,
            ..source.clone()
        };
        let content = ClipContent::Media {
            media_id: source.id,
        };
        if replacement.duration_us == Some(0)
            || project.tracks.iter().any(|track| {
                track.clips.iter().any(|clip| {
                    clip.content == content
                        && !project.media_clip_fits(&replacement, track.kind, clip)
                })
            })
        {
            continue;
        }
        let has_sound = replacement.kind == MediaKind::Video && replacement.has_audio;
        project.media[index] = replacement;
        if !has_sound {
            for clip in project.tracks.iter_mut().flat_map(|track| &mut track.clips) {
                if clip.content == content && clip.linked_audio {
                    clip.linked_audio = false;
                    repaired += 1;
                }
            }
        }
    }
    Ok(repaired)
}

pub fn save(path: &Path, project: &Project) -> io::Result<()> {
    project
        .validate()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let data = format::to_json(project)?;
    if data.len() as u64 > MAX_PROJECT_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Project file is too large",
        ));
    }
    atomic_write(path, data.as_bytes())
}

pub(super) fn atomic_write(path: &Path, data: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Missing project filename"))?;
    crate::atomic_file::write(
        path,
        parent,
        name,
        data,
        "Could not allocate temporary project file",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::export_settings::{ExportSettings, LastExport};
    use crate::project::{Command, Editor};
    use crate::timeline::Transform;
    use crate::timeline::{Clip, ClipContent, Title};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn opening_legacy_media_adopts_probe_metadata_only_for_matching_kinds() {
        let dir = std::env::temp_dir().join(format!("ocus-probe-migration-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let video = dir.join("sound.mp4");
        assert!(
            std::process::Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-y",
                    "-f",
                    "lavfi",
                    "-i",
                    "color=size=32x32:rate=30:duration=2",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=duration=2",
                    "-c:v",
                    "libx264",
                    "-c:a",
                    "aac",
                ])
                .arg(&video)
                .status()
                .unwrap()
                .success()
        );
        let mut document = serde_json::to_value(Project::new("Legacy")).unwrap();
        document["version"] = 1.into();
        document["media"] = serde_json::json!([{
            "id": 4, "path": video, "kind": "video"
        }]);
        let path = dir.join("film.ocus");
        fs::write(&path, document.to_string()).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.media()[0].path, video);
        assert_eq!(loaded.media()[0].duration_us, Some(2_000_000));
        assert!(loaded.media()[0].has_audio);
        document["media"][0]["kind"] = "audio".into();
        fs::write(&path, document.to_string()).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.media()[0].duration_us, None);
        assert!(!loaded.media()[0].has_audio);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn opening_repairs_paths_metadata_and_linked_audio() {
        let dir = std::env::temp_dir().join(format!("ocus-repair-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let video = dir.join("silent.mp4");
        assert!(
            std::process::Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-y",
                    "-f",
                    "lavfi",
                    "-i",
                    "color=size=32x32:rate=30:duration=2",
                    "-c:v",
                    "libx264",
                ])
                .arg(&video)
                .status()
                .unwrap()
                .success()
        );
        let mut document = serde_json::to_value(Project::new("Repair")).unwrap();
        document["version"] = 1.into();
        document["media"] = serde_json::json!([{
            "id": 4, "path": "silent.mp4", "kind": "video", "has_audio": true
        }]);
        document["tracks"][0]["clips"] = serde_json::json!([{
            "id": 1, "start_frame": 0, "source_start_frame": 0,
            "duration_frames": 30, "content": {"media": {"media_id": 4}},
            "volume": 1.0, "muted": false, "linked_audio": true
        }]);
        let path = dir.join("film.ocus");
        fs::write(&path, document.to_string()).unwrap();
        let (loaded, repaired) = load_with_repairs(&path).unwrap();
        assert_eq!(repaired, 1);
        assert_eq!(loaded.media()[0].path, video);
        assert_eq!(loaded.media()[0].duration_us, Some(2_000_000));
        assert!(!loaded.media()[0].has_audio);
        assert!(!loaded.tracks()[0].clips[0].linked_audio);
        assert!(!Editor::new(loaded).is_dirty());
        document["tracks"][2]["clips"] = document["tracks"][0]["clips"].clone();
        document["tracks"][2]["clips"][0]["id"] = 2.into();
        document["tracks"][2]["clips"][0]["linked_audio"] = false.into();
        fs::write(&path, document.to_string()).unwrap();
        let retained = load(&path).unwrap();
        assert_eq!(retained.media()[0].duration_us, None);
        assert!(retained.media()[0].has_audio);
        assert!(retained.tracks()[0].clips[0].linked_audio);
        document["tracks"][2]["clips"] = serde_json::json!([]);
        document["media"][0]["duration_us"] = 4_000_000.into();
        document["tracks"][0]["clips"][0]["duration_frames"] = 90.into();
        fs::write(&path, document.to_string()).unwrap();
        let retained = load(&path).unwrap();
        assert_eq!(retained.media()[0].duration_us, Some(4_000_000));
        assert!(retained.media()[0].has_audio);
        assert!(retained.tracks()[0].clips[0].linked_audio);
        document["media"][0]["path"] = "missing.mp4".into();
        fs::write(&path, document.to_string()).unwrap();
        assert_eq!(load(&path).unwrap().media()[0].duration_us, Some(4_000_000));
        document["media"][0]["path"] = "broken.mp4".into();
        fs::write(dir.join("broken.mp4"), "not a video").unwrap();
        fs::write(&path, document.to_string()).unwrap();
        assert!(load(&path).unwrap().media()[0].has_audio);
        document["media"][0]["has_audio"] = false.into();
        assert!(format::from_json(&document.to_string()).is_ok());
        document["tracks"][0]["clips"][0]["duration_frames"] = 0.into();
        assert!(format::from_json(&document.to_string()).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn save_preserves_atomic_write_behavior() {
        let dir = std::env::temp_dir().join(format!(
            "ocus-project-atomic-{}-{}",
            std::process::id(),
            NEXT_FILE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("film.ocus");
        save(&path, &Project::new("Original")).unwrap();
        let replacement = Project::new("Replacement");
        save(&path, &replacement).unwrap();
        assert_eq!(load(&path).unwrap(), replacement);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        let original = fs::read(&path).unwrap();
        let target = dir.join("directory.ocus");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("keep"), b"untouched").unwrap();
        assert!(save(&target, &replacement).is_err());
        assert_eq!(fs::read(target.join("keep")).unwrap(), b"untouched");
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 2);
        let missing = dir.join("missing");
        assert_eq!(
            save(&missing.join("film.ocus"), &replacement)
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
        assert!(!missing.exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn atomic_save_round_trips_and_rejects_bad_documents_without_overwriting() {
        let dir = std::env::temp_dir().join(format!(
            "ocus-project-test-{}-{}",
            std::process::id(),
            NEXT_FILE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("film.ocus");
        let mut editor = Editor::new(Project::new("Film"));
        assert!(editor.apply(Command::InsertClip {
            track_id: 0,
            clip: Clip {
                id: 1,
                start_frame: 0,
                source_start_frame: 0,
                duration_frames: 90,
                content: ClipContent::Title(Title {
                    text: "Opening".into(),
                    font_family: "Sans".into(),
                    font_size: 48.0,
                    color: [255; 4],
                    x: 0.5,
                    y: 0.5,
                    ..Default::default()
                }),
                volume: 1.0,
                muted: false,
                transition: None,
                fade_in_frames: 0,
                fade_out_frames: 0,
                linked_audio: false,
                transform: Transform::default(),
            },
        }));
        let last_export = LastExport {
            settings: ExportSettings::for_project(
                1280,
                720,
                editor.project().settings().frame_rate.clone(),
            ),
            path: dir.join("film-2.mp4"),
        };
        assert!(editor.apply(Command::SetLastExport(Some(last_export.clone()))));
        save(&path, editor.project()).unwrap();
        assert_eq!(load(&path).unwrap(), *editor.project());
        assert_eq!(load(&path).unwrap().last_export(), Some(&last_export));
        let original = fs::read(&path).unwrap();
        assert!(save(&dir, editor.project()).is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        let bad = format::from_json(
            &String::from_utf8(original.clone())
                .unwrap()
                .replace("\"version\": 1", "\"version\": 3"),
        );
        assert!(bad.is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
        let missing_media = serde_json::json!({
            "id": 6,
            "path": dir.join("absent.mp4"),
            "kind": "video"
        });
        let mut document: serde_json::Value = serde_json::from_slice(&original).unwrap();
        document["media"] = serde_json::json!([missing_media]);
        fs::write(&path, document.to_string()).unwrap();
        let loaded = load(&path).expect("Projects with missing media must still open");
        assert_eq!(loaded.media()[0].id, 6);
        assert!(!loaded.media()[0].path.is_file());
        fs::remove_file(path).unwrap();
        fs::remove_dir(dir).unwrap();
    }
}
