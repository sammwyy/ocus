use super::{Project, format, store};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

const FILE: &str = "unsaved.ocus-recovery";

#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub original_path: Option<PathBuf>,
    pub project: Project,
}

#[derive(Serialize, Deserialize)]
struct Document {
    version: u32,
    original_path: Option<PathBuf>,
    project: String,
}

pub fn directory() -> Option<PathBuf> {
    ProjectDirs::from("", "", "Ocus").map(|dirs| dirs.data_local_dir().join("recovery"))
}

pub fn read(dir: &Path) -> Result<Option<Snapshot>, String> {
    (|| -> io::Result<Option<Snapshot>> {
        let mut bytes = Vec::new();
        let file = match fs::File::open(dir.join(FILE)) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        file.take(store::MAX_PROJECT_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > store::MAX_PROJECT_BYTES {
            return Err(io::Error::other("Recovery file is too large"));
        }
        let document: Document = serde_json::from_slice(&bytes)?;
        if document.version != 1 {
            return Err(io::Error::other(format!(
                "Unsupported recovery version: {}",
                document.version
            )));
        }
        Ok(Some(Snapshot {
            original_path: document.original_path,
            project: format::from_json(&document.project).map_err(io::Error::other)?,
        }))
    })()
    .map_err(|error| error.to_string())
}

pub fn clear(dir: &Path) -> io::Result<()> {
    match fs::remove_file(dir.join(FILE)) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn write(dir: &Path, snapshot: Snapshot) -> io::Result<()> {
    snapshot.project.validate().map_err(io::Error::other)?;
    let data = serde_json::to_vec(&Document {
        version: 1,
        original_path: snapshot.original_path,
        project: format::to_json(&snapshot.project)?,
    })?;
    if data.len() as u64 > store::MAX_PROJECT_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Recovery file is too large",
        ));
    }
    fs::create_dir_all(dir)?;
    store::atomic_write(&dir.join(FILE), &data)
}

enum Message {
    Write(Snapshot),
    Clear,
}

pub struct Autosaver {
    sender: Option<Sender<Message>>,
    worker: Option<JoinHandle<()>>,
    results: Mutex<Option<Receiver<Result<(), String>>>>,
}

impl Autosaver {
    pub fn new(dir: PathBuf) -> Self {
        let (sender, receiver) = mpsc::channel();
        let (report, results) = mpsc::channel();
        let worker = thread::spawn(move || {
            while let Ok(mut message) = receiver.recv() {
                while let Ok(next) = receiver.try_recv() {
                    message = next;
                }
                let writing = matches!(message, Message::Write(_));
                let result = match message {
                    Message::Write(snapshot) => write(&dir, snapshot),
                    Message::Clear => clear(&dir),
                };
                if writing || result.is_err() {
                    let _ = report.send(result.map_err(|error| error.to_string()));
                }
            }
        });
        Self {
            sender: Some(sender),
            worker: Some(worker),
            results: Mutex::new(Some(results)),
        }
    }

    pub fn take_results(&self) -> Option<Receiver<Result<(), String>>> {
        self.results.lock().unwrap().take()
    }

    pub fn write(&self, snapshot: Snapshot) {
        let _ = self.sender.as_ref().unwrap().send(Message::Write(snapshot));
    }

    pub fn clear(&self) {
        let _ = self.sender.as_ref().unwrap().send(Message::Clear);
    }
}

impl Drop for Autosaver {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    pub(crate) fn test_dir() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "ocus-recovery-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn round_trip_never_touches_original() {
        let root = test_dir();
        fs::create_dir_all(&root).unwrap();
        let original = root.join("film.ocus");
        fs::write(&original, "keep me").unwrap();
        let dir = root.join("recovery");
        let snapshot = Snapshot {
            original_path: Some(original.clone()),
            project: Project::new("Film"),
        };
        let saver = Autosaver::new(dir.clone());
        saver.write(snapshot.clone());
        drop(saver);
        assert_eq!(read(&dir).unwrap(), Some(snapshot));
        assert_eq!(fs::read_to_string(original).unwrap(), "keep me");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        assert_eq!(fs::read_dir(&root).unwrap().count(), 2);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn corrupt_unsupported_and_invalid_projects_are_preserved_for_warning() {
        let dir = test_dir();
        assert_eq!(read(&dir).unwrap(), None);
        fs::create_dir_all(&dir).unwrap();
        for data in [
            "broken".to_owned(),
            serde_json::json!({"version":2,"original_path":null,"project":"{}"}).to_string(),
            serde_json::json!({"version":1,"original_path":null,"project":"{}"}).to_string(),
        ] {
            fs::write(dir.join(FILE), data).unwrap();
            assert!(!read(&dir).unwrap_err().is_empty());
            assert!(dir.join(FILE).exists());
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn autosaver_reports_failure_and_next_success() {
        let root = test_dir();
        fs::create_dir_all(&root).unwrap();
        let dir = root.join("blocked");
        fs::write(&dir, "not a directory").unwrap();
        let saver = Autosaver::new(dir.clone());
        let results = saver.take_results().unwrap();
        let snapshot = Snapshot {
            original_path: None,
            project: Project::new("Film"),
        };
        saver.write(snapshot.clone());
        assert!(results.recv().unwrap().is_err());
        fs::remove_file(&dir).unwrap();
        saver.write(snapshot);
        assert_eq!(results.recv().unwrap(), Ok(()));
        drop(saver);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn clear_after_queued_writes_leaves_no_file() {
        let dir = test_dir();
        fs::create_dir_all(&dir).unwrap();
        let saver = Autosaver::new(dir.clone());
        for _ in 0..100 {
            saver.write(Snapshot {
                original_path: None,
                project: Project::new("Untitled"),
            });
        }
        saver.clear();
        drop(saver);
        assert!(!dir.join(FILE).exists());
        fs::remove_dir_all(dir).unwrap();
    }
}
