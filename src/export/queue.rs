use super::{CANCELLED, ExportControl, render_with_settings};
use crate::project::{Project, export_settings::LastExport};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const HISTORY_LIMIT: usize = 20;

pub(crate) struct Request {
    pub project: Project,
    pub output: LastExport,
    pub total: u64,
    pub auto_number: bool,
}

pub(crate) struct Work {
    pub id: u64,
    pub request: Arc<Request>,
    pub control: Arc<ExportControl>,
}

impl Work {
    pub fn render(self, on_destination: impl FnOnce(PathBuf)) -> (LastExport, Result<(), String>) {
        let mut output = self.request.output.clone();
        if self.request.auto_number {
            output.path = unused_export_path(output.path);
            on_destination(output.path.clone());
        }
        let result = render_with_settings(
            &self.request.project,
            &output.path,
            &output.settings,
            self.control,
        );
        (output, result)
    }
}

#[derive(Clone)]
pub(crate) enum Status {
    Queued(Arc<Request>),
    Encoding {
        control: Arc<ExportControl>,
        total: u64,
        cancelling: bool,
        resolving: bool,
    },
    Completed,
    Cancelled,
    Failed(String),
}

impl Status {
    pub fn pending(&self) -> bool {
        matches!(self, Self::Queued(_) | Self::Encoding { .. })
    }
}

#[derive(Clone)]
pub(crate) struct Job {
    pub id: u64,
    pub path: PathBuf,
    pub status: Status,
}

#[derive(Clone, Default)]
pub(crate) struct Queue {
    jobs: Vec<Job>,
    next_id: u64,
}

impl Queue {
    pub fn jobs(&self) -> &[Job] {
        &self.jobs
    }

    pub fn active(&self) -> Option<&Job> {
        self.jobs
            .iter()
            .find(|job| matches!(job.status, Status::Encoding { .. }))
    }

    pub fn accepts_jobs_while_busy(&self) -> bool {
        self.active().map_or_else(
            || {
                self.jobs
                    .iter()
                    .any(|job| matches!(job.status, Status::Queued(_)))
            },
            |job| {
                matches!(
                    job.status,
                    Status::Encoding {
                        resolving: false,
                        ..
                    }
                )
            },
        )
    }

    pub fn suggested_path(&self, mut path: PathBuf) -> PathBuf {
        while self.jobs.iter().any(|job| {
            job.path == path && (job.status.pending() || matches!(job.status, Status::Completed))
        }) {
            path = next_export_path(&path);
        }
        path
    }

    pub fn enqueue(&mut self, request: Request) -> Result<(), String> {
        if self
            .jobs
            .iter()
            .any(|job| job.status.pending() && job.path == request.output.path)
        {
            return Err(
                "An export already uses this destination. Choose a different filename.".into(),
            );
        }
        self.next_id += 1;
        self.jobs.push(Job {
            id: self.next_id,
            path: request.output.path.clone(),
            status: Status::Queued(Arc::new(request)),
        });
        Ok(())
    }

    pub fn start_next(&mut self) -> Option<Work> {
        if self.active().is_some() {
            return None;
        }
        let (index, request) = self.jobs.iter().enumerate().find_map(|(index, job)| {
            if let Status::Queued(request) = &job.status {
                Some((index, request.clone()))
            } else {
                None
            }
        })?;
        let control = Arc::new(ExportControl::default());
        let job = &mut self.jobs[index];
        job.status = Status::Encoding {
            control: control.clone(),
            total: request.total,
            cancelling: false,
            resolving: request.auto_number,
        };
        Some(Work {
            id: job.id,
            request,
            control,
        })
    }

    pub fn set_active_path(&mut self, id: u64, path: PathBuf) {
        if let Some(job) = self.jobs.iter_mut().find(|job| job.id == id)
            && let Status::Encoding { resolving, .. } = &mut job.status
        {
            job.path = path;
            *resolving = false;
        }
    }

    pub fn finish(&mut self, id: u64, path: PathBuf, result: &Result<(), String>) {
        let Some(job) = self
            .jobs
            .iter_mut()
            .find(|job| job.id == id && matches!(job.status, Status::Encoding { .. }))
        else {
            return;
        };
        job.path = path;
        job.status = match result {
            Ok(()) => Status::Completed,
            Err(error) if error == CANCELLED => Status::Cancelled,
            Err(error) => Status::Failed(error.clone()),
        };
        self.trim_history();
    }

    pub fn cancel(&mut self, id: u64) {
        let Some(job) = self.jobs.iter_mut().find(|job| job.id == id) else {
            return;
        };
        match &mut job.status {
            Status::Queued(_) => {
                job.status = Status::Cancelled;
                self.trim_history();
            }
            Status::Encoding {
                control,
                cancelling,
                ..
            } => {
                *cancelling = true;
                control.cancel();
            }
            _ => {}
        }
    }

    pub fn clear_finished(&mut self) {
        self.jobs.retain(|job| job.status.pending());
    }

    fn trim_history(&mut self) {
        let mut excess = self
            .jobs
            .iter()
            .filter(|job| !job.status.pending())
            .count()
            .saturating_sub(HISTORY_LIMIT);
        self.jobs.retain(|job| {
            if excess > 0 && !job.status.pending() {
                excess -= 1;
                return false;
            }
            true
        });
    }
}

pub(crate) fn next_export_path(path: &Path) -> PathBuf {
    let stem = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("export");
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("mp4");
    let (prefix, next) = stem
        .rsplit_once('-')
        .and_then(|(prefix, number)| {
            number
                .parse::<u32>()
                .ok()
                .and_then(|n| n.checked_add(1))
                .map(|next| (prefix, next))
        })
        .unwrap_or((stem, 2));
    path.with_file_name(format!("{prefix}-{next}.{extension}"))
}

fn unused_export_path(mut path: PathBuf) -> PathBuf {
    for _ in 0..10_000 {
        if !path.exists() {
            break;
        }
        path = next_export_path(&path);
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{Command, Editor, format};
    use crate::timeline::{Clip, ClipContent, Title, Transform};
    use ffmpeg_next as ffmpeg;
    use std::fs;

    fn request(path: PathBuf) -> Request {
        let fresh = Project::new("Queued timeline");
        let mut project: serde_json::Value =
            serde_json::from_str(&format::to_json(&fresh).unwrap()).unwrap();
        project["settings"]["width"] = 64.into();
        project["settings"]["height"] = 64.into();
        let mut editor = Editor::new(format::from_json(&project.to_string()).unwrap());
        assert!(editor.apply(Command::InsertClip {
            track_id: 0,
            clip: Clip {
                id: 1,
                start_frame: 0,
                duration_frames: 8,
                source_start_frame: 0,
                content: ClipContent::Title(Title::default()),
                volume: 1.0,
                muted: false,
                color: Default::default(),
                lut: None,
                transition: None,
                transition_out: None,
                fade_in_frames: 0,
                fade_out_frames: 0,
                noise_reduction: false,
                linked_audio: false,
                keyframes: Vec::new(),
                transform: Transform::default(),
            },
        }));
        let settings = super::super::presets::builtins(editor.project())
            .into_iter()
            .map(|preset| preset.settings)
            .find(|settings| path.extension().unwrap() == settings.container.extension())
            .unwrap();
        Request {
            project: editor.project().clone(),
            output: LastExport { path, settings },
            total: 8,
            auto_number: false,
        }
    }

    fn run(queue: &mut Queue) -> (LastExport, Result<(), String>) {
        let work = queue.start_next().unwrap();
        let id = work.id;
        let (output, result) = work.render(|path| queue.set_active_path(id, path));
        queue.finish(id, output.path.clone(), &result);
        (output, result)
    }

    #[test]
    fn cancellation_waits_for_worker_and_skips_removed_jobs() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("active.gif");
        let second = dir.path().join("removed.wav");
        let third = dir.path().join("next.wav");
        let mut queue = Queue::default();
        for path in [&first, &second, &third] {
            queue.enqueue(request(path.clone())).unwrap();
        }
        let active = queue.start_next().unwrap();
        assert_eq!(active.request.output.path, first);
        queue.cancel(queue.jobs()[1].id);
        assert!(matches!(queue.jobs()[1].status, Status::Cancelled));
        assert!(active.control.check().is_ok());
        queue.cancel(active.id);
        assert!(matches!(
            queue.active().unwrap().status,
            Status::Encoding {
                cancelling: true,
                ..
            }
        ));
        assert!(queue.start_next().is_none());
        let id = active.id;
        let (output, result) = active.render(|_| unreachable!());
        assert_eq!(result.as_ref().unwrap_err(), CANCELLED);
        queue.finish(id, output.path, &result);
        assert!(matches!(queue.jobs()[0].status, Status::Cancelled));
        assert!(fs::read_dir(dir.path()).unwrap().next().is_none());
        assert_eq!(queue.start_next().unwrap().request.output.path, third);
        queue.clear_finished();
        assert_eq!(queue.jobs().len(), 1);
        assert_eq!(queue.active().unwrap().path, third);
    }

    #[test]
    fn queue_renders_real_outputs_in_order_and_continues_after_failure() {
        let dir = tempfile::tempdir().unwrap();
        let gif = dir.path().join("preview.gif");
        let collision = dir.path().join("existing.wav");
        let wav = dir.path().join("mix.wav");
        fs::write(&collision, b"preserve existing output").unwrap();
        let mut queue = Queue::default();
        for path in [&gif, &collision, &wav] {
            queue.enqueue(request(path.clone())).unwrap();
        }

        let (output, result) = run(&mut queue);
        assert_eq!(output.path, gif);
        result.unwrap();
        assert!(!wav.exists());
        let input = ffmpeg::format::input(&gif).unwrap();
        let stream = input.streams().best(ffmpeg::media::Type::Video).unwrap();
        let decoder = ffmpeg::codec::context::Context::from_parameters(stream.parameters())
            .unwrap()
            .decoder()
            .video()
            .unwrap();
        assert_eq!(decoder.id(), ffmpeg::codec::Id::GIF);
        assert_eq!((decoder.width(), decoder.height()), (64, 64));
        println!("First: GIF, {} × {}", decoder.width(), decoder.height());

        let (output, result) = run(&mut queue);
        assert_eq!(output.path, collision);
        assert!(
            matches!(&queue.jobs()[1].status, Status::Failed(error) if error.contains("already exists"))
        );
        assert_eq!(fs::read(&collision).unwrap(), b"preserve existing output");
        println!("Second: {}", result.unwrap_err());

        let (output, result) = run(&mut queue);
        assert_eq!(output.path, wav);
        result.unwrap();
        let input = ffmpeg::format::input(&wav).unwrap();
        let stream = input.streams().best(ffmpeg::media::Type::Audio).unwrap();
        let decoder = ffmpeg::codec::context::Context::from_parameters(stream.parameters())
            .unwrap()
            .decoder()
            .audio()
            .unwrap();
        assert_eq!(decoder.id(), ffmpeg::codec::Id::PCM_F32LE);
        assert_eq!((decoder.rate(), decoder.channels()), (48_000, 2));
        println!(
            "Third: WAV, {} Hz, {} channels",
            decoder.rate(),
            decoder.channels()
        );

        assert!(matches!(queue.jobs()[0].status, Status::Completed));
        assert!(matches!(queue.jobs()[2].status, Status::Completed));
        assert!(queue.start_next().is_none());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 3);
    }

    #[test]
    fn quick_export_reserves_its_resolved_destination_without_overwriting() {
        let dir = tempfile::tempdir().unwrap();
        let requested = dir.path().join("mix-2.wav");
        let actual = dir.path().join("mix-3.wav");
        fs::write(&requested, b"existing output").unwrap();
        let mut queue = Queue::default();
        let mut quick = request(requested.clone());
        quick.auto_number = true;
        queue.enqueue(quick).unwrap();
        let work = queue.start_next().unwrap();
        assert!(!queue.accepts_jobs_while_busy());
        let id = work.id;
        let (output, result) = work.render(|path| {
            assert_eq!(path, actual);
            queue.set_active_path(id, path.clone());
            assert!(queue.accepts_jobs_while_busy());
            assert!(queue.enqueue(request(path.clone())).is_err());
            assert_eq!(queue.suggested_path(path), dir.path().join("mix-4.wav"));
        });
        assert!(result.is_ok());
        queue.finish(id, output.path, &result);
        assert_eq!(queue.jobs()[0].path, actual);
        assert_eq!(fs::read(&requested).unwrap(), b"existing output");
    }

    #[test]
    fn history_is_bounded_without_discarding_pending_jobs() {
        let mut queue = Queue::default();
        for index in 0..HISTORY_LIMIT + 2 {
            queue
                .enqueue(request(PathBuf::from(format!("{index}.wav"))))
                .unwrap();
        }
        let active = queue.start_next().unwrap();
        for id in 2..=HISTORY_LIMIT as u64 + 2 {
            queue.cancel(id);
        }
        assert_eq!(queue.jobs().len(), HISTORY_LIMIT + 1);
        assert_eq!(queue.jobs()[1].id, 3);
        queue.clear_finished();
        assert_eq!(queue.active().unwrap().id, active.id);
    }
}
