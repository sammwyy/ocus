use super::{Project, format};
use std::fs;
use std::io::{self, Read};
use std::path::Path;
pub(super) const MAX_PROJECT_BYTES: u64 = 16 * 1024 * 1024;

pub fn load(path: &Path) -> io::Result<Project> {
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
    let project = format::from_json(&json)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    Ok(project)
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
    use crate::timeline::{Clip, ClipContent, Title};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_FILE: AtomicU64 = AtomicU64::new(0);

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
                fade_in_frames: 0,
                fade_out_frames: 0,
                linked_audio: false,
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
