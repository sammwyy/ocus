use super::{Project, format};
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_FILE: AtomicU64 = AtomicU64::new(0);
const MAX_PROJECT_BYTES: u64 = 16 * 1024 * 1024;

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
    for media in project.media() {
        if !media.path.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("Missing source: {}", media.path.display()),
            ));
        }
    }
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
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Missing project filename"))?;
    let mut temp = None;
    for _ in 0..8 {
        let candidate = parent.join(format!(
            ".{}.{}.{}.tmp",
            name.to_string_lossy(),
            std::process::id(),
            NEXT_FILE.fetch_add(1, Ordering::Relaxed)
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => {
                temp = Some((candidate, file));
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    let (temporary, mut file) = temp.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "Could not allocate temporary project file",
        )
    })?;
    let result = (|| {
        file.write_all(data.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{Command, Editor};
    use crate::timeline::{Clip, ClipContent, Title};

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
                }),
                volume: 1.0,
                muted: false,
            },
        }));
        save(&path, editor.project()).unwrap();
        assert_eq!(load(&path).unwrap(), *editor.project());
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
        assert_eq!(load(&path).unwrap_err().kind(), io::ErrorKind::NotFound);
        fs::remove_file(path).unwrap();
        fs::remove_dir(dir).unwrap();
    }
}
