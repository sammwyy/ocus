mod edl;
mod otio;

use super::{FrameRate, Project, store};
use crate::media::{self, Media, MediaKind, MediaProbe};
use crate::timeline::{Clip, ClipContent, Playback, Track, TrackKind};
use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Format {
    Otio,
    Edl,
}

impl Format {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Otio => "otio",
            Self::Edl => "edl",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Otio => "OpenTimelineIO",
            Self::Edl => "CMX3600 EDL",
        }
    }
}

pub struct Imported {
    pub project: Project,
    pub missing: Vec<u64>,
    pub notes: Vec<String>,
}

pub fn load(path: &Path, fallback: &FrameRate) -> Result<Imported, String> {
    let path = std::path::absolute(path).map_err(|error| error.to_string())?;
    let input = read_text(&path)?;
    let mut timeline = match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) if ext.eq_ignore_ascii_case("otio") => otio::read(&input)?,
        Some(ext) if ext.eq_ignore_ascii_case("edl") => edl::read(&input, fallback)?,
        _ => return Err("Choose an .otio or .edl timeline file".into()),
    };
    if timeline.name.trim().is_empty() {
        timeline.name = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
    }
    timeline.into_project(
        path.parent().ok_or("Missing timeline directory")?,
        media::probe,
    )
}

pub fn edl_rate_needed(path: &Path) -> Result<Option<bool>, String> {
    if path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("edl"))
    {
        return edl::rate_needed(&read_text(path)?);
    }
    Ok(None)
}

fn read_text(path: &Path) -> Result<String, String> {
    let mut input = String::new();
    File::open(path)
        .and_then(|file| {
            file.take(store::MAX_PROJECT_BYTES + 1)
                .read_to_string(&mut input)
        })
        .map_err(|error| error.to_string())?;
    if input.len() as u64 > store::MAX_PROJECT_BYTES {
        return Err("Timeline interchange file is too large".into());
    }
    Ok(input)
}

pub fn save(path: &Path, project: &Project, format: Format) -> Result<Vec<String>, String> {
    let absolute = std::path::absolute(path).map_err(|error| error.to_string())?;
    let target = path.canonicalize().ok();
    if project.media().iter().any(|media| {
        std::path::absolute(&media.path).is_ok_and(|source| source == absolute)
            || target.as_ref().is_some_and(|target| {
                media
                    .path
                    .canonicalize()
                    .is_ok_and(|source| source == *target)
            })
    }) {
        return Err("Choose a destination that is not a source media file".into());
    }
    let (data, notes) = encode(project, format)?;
    if data.len() as u64 > store::MAX_PROJECT_BYTES {
        return Err("Timeline interchange file is too large".into());
    }
    store::atomic_write(path, data.as_bytes()).map_err(|error| error.to_string())?;
    Ok(notes)
}

fn encode(project: &Project, format: Format) -> Result<(String, Vec<String>), String> {
    project.validate()?;
    let mut notes = BTreeSet::new();
    let data = match format {
        Format::Otio => otio::write(project, &mut notes)?,
        Format::Edl => edl::write(project, &mut notes)?,
    };
    if !project.markers.is_empty() {
        notes.insert("Timeline markers were not exported.".into());
    }
    Ok((data, notes.into_iter().collect()))
}

fn source<'a>(
    project: &'a Project,
    clip: &Clip,
    notes: &mut BTreeSet<String>,
) -> Result<&'a Media, String> {
    let ClipContent::Media { media_id } = clip.content else {
        return Err("Titles and subtitles cannot be exported. Hide their tracks first to export the media cuts.".into());
    };
    let media = project
        .media()
        .iter()
        .find(|media| media.id == media_id)
        .ok_or("Missing clip source")?;
    if clip.playback != Playback::Forward
        || clip.speed != 1.0
        || clip.transition.is_some()
        || clip.transition_out.is_some()
    {
        return Err(format!(
            "{} uses a transition, speed change, reverse or freeze frame. Only straight cuts can be exported.",
            media.path.display()
        ));
    }
    if clip.volume != 1.0
        || clip.fade_in_frames != 0
        || clip.fade_out_frames != 0
        || clip.noise_reduction
        || !clip.transform.is_identity()
        || !clip.keyframes.is_empty()
        || !clip.color.neutral()
        || clip.lut.is_some()
    {
        notes.insert(
            "Clip effects, transforms, color adjustments and audio settings were not exported."
                .into(),
        );
    }
    Ok(media)
}

fn source_path(path: &Path) -> Result<String, String> {
    let absolute = std::path::absolute(path).map_err(|error| error.to_string())?;
    absolute
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("Media path is not valid UTF-8: {}", path.display()))
}

struct Sequence {
    name: String,
    rate: FrameRate,
    size: Option<(u32, u32)>,
    tracks: Vec<Lane>,
    notes: BTreeSet<String>,
}

struct Lane {
    kind: TrackKind,
    name: String,
    cuts: Vec<Cut>,
}

struct Cut {
    path: PathBuf,
    start: u64,
    source: u64,
    duration: u64,
    linked_audio: bool,
}

impl Sequence {
    fn new(name: String, rate: FrameRate) -> Self {
        Self {
            name,
            rate,
            size: None,
            tracks: Vec::new(),
            notes: BTreeSet::new(),
        }
    }

    fn into_project(
        self,
        directory: &Path,
        probe: impl Fn(&Path) -> Result<MediaProbe, String>,
    ) -> Result<Imported, String> {
        let mut project = Project::new(if self.name.trim().is_empty() {
            "Imported timeline".into()
        } else {
            self.name
        });
        project.settings.frame_rate = self.rate;
        if let Some((width, height)) = self.size {
            project.settings.width = width;
            project.settings.height = height;
        }
        project.tracks.clear();
        let mut uses = HashMap::<PathBuf, (bool, bool)>::new();
        for lane in &self.tracks {
            for cut in &lane.cuts {
                let use_ = uses.entry(directory.join(&cut.path)).or_default();
                use_.0 |= lane.kind == TrackKind::Video;
                use_.1 |= lane.kind == TrackKind::Audio || cut.linked_audio;
            }
        }
        let mut ids = HashMap::new();
        let mut missing = Vec::new();
        let mut next_clip = 0;
        for lane in self.tracks {
            let mut track = Track {
                id: project.tracks.len() as u64,
                kind: lane.kind,
                subtitles: false,
                name: lane
                    .name
                    .trim()
                    .chars()
                    .filter(|ch| !ch.is_control())
                    .take(32)
                    .collect::<String>()
                    .trim_end()
                    .into(),
                locked: false,
                hidden: false,
                clips: Vec::new(),
            };
            for cut in lane.cuts {
                let path = directory.join(cut.path);
                let media_id = if let Some(&id) = ids.get(&path) {
                    id
                } else {
                    let id = project.media.len();
                    let info = if path.exists() {
                        probe(&path).map_err(|error| format!("{}: {error}", path.display()))?
                    } else {
                        let (video, audio) = uses[&path];
                        missing.push(id as u64);
                        MediaProbe {
                            kind: if video && !audio && media::still_image_path(&path) {
                                MediaKind::Image
                            } else if video {
                                MediaKind::Video
                            } else {
                                MediaKind::Audio
                            },
                            has_audio: audio,
                            duration_us: None,
                        }
                    };
                    ids.insert(path.clone(), id);
                    project.media.push(Media {
                        id: id as u64,
                        path,
                        kind: info.kind,
                        has_audio: info.has_audio,
                        duration_us: info.duration_us,
                    });
                    id
                };
                let media = &project.media[media_id];
                let clip = Clip {
                    id: next_clip,
                    start_frame: cut.start,
                    source_start_frame: cut.source,
                    duration_frames: cut.duration,
                    content: ClipContent::Media { media_id: media.id },
                    volume: 1.0,
                    muted: false,
                    linked_audio: cut.linked_audio,
                    speed: 1.0,
                    playback: Playback::Forward,
                    fade_in_frames: 0,
                    fade_out_frames: 0,
                    noise_reduction: false,
                    color: Default::default(),
                    transform: Default::default(),
                    keyframes: Vec::new(),
                    lut: None,
                    transition: None,
                    transition_out: None,
                };
                if !project.media_clip_fits(media, track.kind, &clip)
                    || (cut.linked_audio && !media.has_audio)
                {
                    return Err(format!(
                        "{}: cut at frame {} is outside its source range or the file has no matching {} stream.",
                        media.path.display(),
                        cut.start,
                        if cut.linked_audio {
                            "video and audio"
                        } else if lane.kind == TrackKind::Video {
                            "video"
                        } else {
                            "audio"
                        }
                    ));
                }
                next_clip += 1;
                track.clips.push(clip);
            }
            project.tracks.push(track);
        }
        if project.tracks.is_empty() {
            project.tracks = Project::new("").tracks;
        }
        project.validate()?;
        Ok(Imported {
            project,
            missing,
            notes: self.notes.into_iter().collect(),
        })
    }
}

fn rate_value(rate: &FrameRate) -> f64 {
    f64::from(rate.numerator) / f64::from(rate.denominator)
}

fn frame_rate(value: f64) -> Result<FrameRate, String> {
    if !value.is_finite() || !(1.0..=1000.0).contains(&value) {
        return Err("Invalid timeline frame rate".into());
    }
    for (alias, numerator) in [
        (23.976, 24000),
        (29.97, 30000),
        (47.952, 48000),
        (59.94, 60000),
        (119.88, 120000),
    ] {
        if (value - alias).abs() < 0.001 {
            return Ok(FrameRate {
                numerator,
                denominator: 1001,
            });
        }
    }
    let precision = 1_000_000;
    let numerator = (value * f64::from(precision)).round() as u32;
    let (mut a, mut b) = (numerator, precision);
    while b != 0 {
        (a, b) = (b, a % b);
    }
    Ok(FrameRate {
        numerator: numerator / a,
        denominator: precision / a,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const OTIO: &str = include_str!("../../tests/fixtures/interchange.otio");
    const EDL: &str = include_str!("../../tests/fixtures/interchange.edl");

    fn fake_probe(_: &Path) -> Result<MediaProbe, String> {
        Ok(MediaProbe {
            kind: MediaKind::Video,
            duration_us: Some(5_000_000),
            has_audio: true,
        })
    }

    fn cuts(project: &Project, lane: usize) -> Vec<(u64, u64, u64, bool)> {
        project.tracks()[lane]
            .clips
            .iter()
            .map(|clip| {
                (
                    clip.start_frame,
                    clip.source_start_frame,
                    clip.duration_frames,
                    clip.linked_audio,
                )
            })
            .collect()
    }

    #[test]
    fn official_otio_fixture_preserves_gaps_and_source_timecode_offsets() {
        let folder = tempfile::tempdir().unwrap();
        let file = folder.path().join("timeline.otio");
        std::fs::write(&file, OTIO).unwrap();
        let imported = load(&file, &frame_rate(30.0).unwrap()).unwrap();
        assert_eq!(
            imported.project.settings.frame_rate,
            frame_rate(24.0).unwrap()
        );
        assert_eq!(imported.project.tracks()[0].name, "Camera");
        assert_eq!(cuts(&imported.project, 0), [(3, 12, 48, false)]);
        assert_eq!(imported.missing, [0]);
        assert_eq!(
            imported.project.media()[0].path,
            folder.path().join("shot.mov")
        );
        let still = OTIO.replace("shot.mov", "poster.png");
        let imported = otio::read(&still)
            .unwrap()
            .into_project(folder.path(), fake_probe)
            .unwrap();
        assert_eq!(imported.project.media()[0].kind, MediaKind::Image);
    }

    #[test]
    fn otio_round_trip_preserves_trimmed_cuts_and_linked_sound_as_audio() {
        let folder = tempfile::tempdir().unwrap();
        std::fs::create_dir(folder.path().join("media")).unwrap();
        for path in ["shot.mov", "media/shot with sound.mov"] {
            File::create(folder.path().join(path)).unwrap();
        }
        let mut project = edl::read(EDL, &frame_rate(24.0).unwrap())
            .unwrap()
            .into_project(folder.path(), fake_probe)
            .unwrap()
            .project;
        project.settings.width = 1024;
        let (data, notes) = encode(&project, Format::Otio).unwrap();
        let reloaded = otio::read(&data)
            .unwrap()
            .into_project(folder.path(), fake_probe)
            .unwrap()
            .project;
        assert!(data.contains("shot%20with%20sound.mov"));
        assert!(
            notes
                .iter()
                .any(|note| note.contains("separate audio tracks"))
        );
        assert_eq!(reloaded.settings(), project.settings());
        assert_eq!(reloaded.media(), project.media());
        assert_eq!(
            cuts(&reloaded, 0),
            [(3, 12, 48, false), (72, 60, 24, false)]
        );
        assert_eq!(reloaded.tracks()[1].kind, TrackKind::Audio);
        assert_eq!(cuts(&reloaded, 1), [(72, 60, 24, false)]);
    }

    #[test]
    fn edl_round_trip_keeps_record_offset_source_trim_and_av_link() {
        let folder = tempfile::tempdir().unwrap();
        let rate = frame_rate(24.0).unwrap();
        let imported = edl::read(EDL, &rate)
            .unwrap()
            .into_project(folder.path(), fake_probe)
            .unwrap();
        assert!(imported.notes[0].contains("24.000 fps"));
        assert_eq!(
            imported.project.media()[1].path,
            folder.path().join("media/shot with sound.mov")
        );
        assert_eq!(
            cuts(&imported.project, 0),
            [(3, 12, 48, false), (72, 60, 24, true)]
        );
        let file = folder.path().join("cuts.edl");
        assert!(
            save(&file, &imported.project, Format::Edl)
                .unwrap()
                .is_empty()
        );
        let reloaded = load(&file, &frame_rate(30.0).unwrap()).unwrap();
        assert_eq!(reloaded.project, imported.project);
        assert!(reloaded.notes.is_empty());
    }

    #[test]
    fn unsupported_edits_fail_with_a_reason() {
        let transition = OTIO.replace("Gap.1", "Transition.1");
        assert!(
            otio::read(&transition)
                .err()
                .unwrap()
                .contains("Transition.1")
        );
        let dissolve = EDL.replace(" C ", " D ");
        assert!(
            edl::read(&dissolve, &frame_rate(24.0).unwrap())
                .err()
                .unwrap()
                .contains("edit type D")
        );

        let folder = tempfile::tempdir().unwrap();
        let mut project = otio::read(OTIO)
            .unwrap()
            .into_project(folder.path(), fake_probe)
            .unwrap()
            .project;
        let mut track = project.tracks[0].clone();
        track.id += 1;
        track.clips[0].id += 1;
        project.tracks.push(track);
        assert!(
            encode(&project, Format::Edl)
                .unwrap_err()
                .contains("overlaps")
        );
        assert!(encode(&project, Format::Otio).is_ok());
    }
}
