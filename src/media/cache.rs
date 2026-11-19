use super::preview::{FrameImage, StoryboardKey};
use super::{MediaKind, Thumbnail};
use directories::ProjectDirs;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const VERSION: u32 = 1;
const CAP: u64 = 512 * 1024 * 1024;
const MAX_ENTRY: u64 = 4 * 1024 * 1024;
const MAGIC: &[u8; 8] = b"OCUSRGBA";

pub fn directory() -> Option<PathBuf> {
    ProjectDirs::from("", "", "Ocus").map(|dirs| dirs.cache_dir().to_owned())
}

pub fn thumbnail(path: &Path) -> Result<Thumbnail, String> {
    thumbnail_in(directory().as_deref(), path)
}

fn thumbnail_in(root: Option<&Path>, path: &Path) -> Result<Thumbnail, String> {
    let entry = root
        .zip(key(path, b"thumbnail:208x116"))
        .map(|(root, key)| root.join("thumbnails").join(key));
    if let Some(frames) = entry.as_deref().and_then(|path| read(path, 1)) {
        let frame = frames.into_iter().next().unwrap();
        return Ok(Thumbnail {
            width: frame.width,
            height: frame.height,
            pixels: frame.pixels,
        });
    }
    let image = super::thumbnail(path)?;
    if let (Some(root), Some(entry)) = (root, entry) {
        let frames = [FrameImage {
            width: image.width,
            height: image.height,
            pixels: image.pixels.clone(),
        }];
        let _ = write(root, &entry, &frames, CAP);
    }
    Ok(image)
}

pub fn storyboard(
    key: &StoryboardKey,
    cancelled: Arc<AtomicBool>,
) -> Result<Vec<FrameImage>, String> {
    storyboard_in(directory().as_deref(), key, cancelled)
}

fn storyboard_in(
    root: Option<&Path>,
    request: &StoryboardKey,
    cancelled: Arc<AtomicBool>,
) -> Result<Vec<FrameImage>, String> {
    let mut params = Vec::from(b"storyboard:160x90".as_slice());
    params.push(match request.kind {
        MediaKind::Video => 0,
        MediaKind::Image => 1,
        MediaKind::Audio => 2,
    });
    params.push(match request.playback {
        crate::timeline::Playback::Forward => 0,
        crate::timeline::Playback::Reverse => 1,
        crate::timeline::Playback::Freeze => 2,
    });
    params.extend(request.source_start.to_le_bytes());
    params.extend(request.duration.to_le_bytes());
    params.extend(request.fps.0.to_le_bytes());
    params.extend(request.fps.1.to_le_bytes());
    let entry = root
        .zip(key(&request.path, &params))
        .map(|(root, key)| root.join("frames").join(key));
    let expected = if request.kind == MediaKind::Image {
        1
    } else {
        3
    };
    if !cancelled.load(Ordering::Relaxed)
        && let Some(frames) = entry.as_deref().and_then(|path| read(path, expected))
    {
        return Ok(frames);
    }
    let frames = super::preview::storyboard(request, cancelled.clone())?;
    if frames.len() == expected
        && !cancelled.load(Ordering::Relaxed)
        && let (Some(root), Some(entry)) = (root, entry)
    {
        let _ = write(root, &entry, &frames, CAP);
    }
    Ok(frames)
}

fn key(path: &Path, params: &[u8]) -> Option<String> {
    let path = path.canonicalize().ok()?;
    let metadata = path.metadata().ok()?;
    let modified = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    let path = path.as_os_str().as_encoded_bytes();
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in VERSION
        .to_le_bytes()
        .into_iter()
        .chain((path.len() as u64).to_le_bytes())
        .chain(path.iter().copied())
        .chain(metadata.len().to_le_bytes())
        .chain(modified.as_secs().to_le_bytes())
        .chain(modified.subsec_nanos().to_le_bytes())
        .chain(params.iter().copied())
    {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3);
    }
    Some(format!("{hash:016x}"))
}

fn read(path: &Path, expected: usize) -> Option<Vec<FrameImage>> {
    let file = File::open(path).ok()?;
    let result = (|| -> Option<Vec<FrameImage>> {
        if file.metadata().ok()?.len() > MAX_ENTRY {
            return None;
        }
        let mut bytes = Vec::new();
        (&file).take(MAX_ENTRY + 1).read_to_end(&mut bytes).ok()?;
        let mut data = bytes.as_slice();
        if data.get(..8)? != MAGIC {
            return None;
        }
        data = &data[8..];
        if take_u32(&mut data)? != VERSION || take_u32(&mut data)? as usize != expected {
            return None;
        }
        let mut dimensions = Vec::new();
        for _ in 0..expected {
            let width = take_u32(&mut data)?;
            let height = take_u32(&mut data)?;
            let size = (width as usize)
                .checked_mul(height as usize)?
                .checked_mul(4)?;
            if width == 0 || height == 0 || size as u64 > MAX_ENTRY {
                return None;
            }
            dimensions.push((width, height, size));
        }
        let mut frames = Vec::new();
        for (width, height, size) in dimensions {
            let pixels = data.get(..size)?.to_vec();
            data = &data[size..];
            frames.push(FrameImage {
                width,
                height,
                pixels,
            });
        }
        data.is_empty().then_some(frames)
    })();
    if result.is_some() {
        let _ = file.set_modified(SystemTime::now());
    } else {
        let _ = fs::remove_file(path);
    }
    result
}

fn take_u32(data: &mut &[u8]) -> Option<u32> {
    let value = u32::from_le_bytes(data.get(..4)?.try_into().ok()?);
    *data = &data[4..];
    Some(value)
}

fn write(root: &Path, path: &Path, frames: &[FrameImage], cap: u64) -> io::Result<()> {
    let mut bytes = MAGIC.to_vec();
    bytes.extend(VERSION.to_le_bytes());
    bytes.extend((frames.len() as u32).to_le_bytes());
    for frame in frames {
        bytes.extend(frame.width.to_le_bytes());
        bytes.extend(frame.height.to_le_bytes());
    }
    for frame in frames {
        bytes.extend(&frame.pixels);
    }
    let parent = path.parent().unwrap();
    fs::create_dir_all(parent)?;
    crate::atomic_file::write(
        path,
        parent,
        path.file_name().unwrap(),
        &bytes,
        "Cannot allocate media cache file",
    )?;
    evict(root, cap)
}

fn evict(root: &Path, cap: u64) -> io::Result<()> {
    let mut entries = Vec::new();
    let mut total = 0;
    for directory in ["thumbnails", "frames"] {
        let Ok(files) = fs::read_dir(root.join(directory)) else {
            continue;
        };
        for entry in files {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.len() != 16 || !name.bytes().all(|b| b.is_ascii_hexdigit()) {
                continue;
            }
            let metadata = entry.metadata()?;
            if metadata.is_file() {
                total += metadata.len();
                entries.push((metadata.modified()?, metadata.len(), entry.path()));
            }
        }
    }
    entries.sort_by_key(|entry| entry.0);
    for (_, size, path) in entries {
        if total <= cap {
            break;
        }
        fs::remove_file(path)?;
        total -= size;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use std::time::{Duration, Instant};

    fn fixture(root: &Path, size: &str) -> StoryboardKey {
        let path = root.join("source.mkv");
        assert!(
            Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    &format!("testsrc2=size={size}:rate=30:duration=2"),
                    "-c:v",
                    "mpeg4",
                    "-y",
                ])
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        StoryboardKey {
            playback: crate::timeline::Playback::Forward,
            path,
            kind: MediaKind::Video,
            source_start: 0,
            duration: 60,
            fps: (30, 1),
        }
    }

    fn cancel(value: bool) -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(value))
    }

    fn same(a: &[FrameImage], b: &[FrameImage]) {
        assert_eq!(a.len(), b.len());
        for (a, b) in a.iter().zip(b) {
            assert_eq!(
                (a.width, a.height, &a.pixels),
                (b.width, b.height, &b.pixels)
            );
        }
    }

    #[test]
    fn decoded_entries_round_trip_and_io_errors_fall_back() {
        let dir = tempfile::tempdir().unwrap();
        let request = fixture(dir.path(), "320x180");
        let root = dir.path().join("cache");
        let first = thumbnail_in(Some(&root), &request.path).unwrap();
        let entry = fs::read_dir(root.join("thumbnails"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        File::open(&entry)
            .unwrap()
            .set_modified(UNIX_EPOCH + Duration::from_secs(1))
            .unwrap();
        let second = thumbnail_in(Some(&root), &request.path).unwrap();
        assert_eq!(
            (first.width, first.height, first.pixels),
            (second.width, second.height, second.pixels)
        );
        assert!(
            entry.metadata().unwrap().modified().unwrap() > UNIX_EPOCH + Duration::from_secs(1)
        );
        let cold = storyboard_in(Some(&root), &request, cancel(false)).unwrap();
        let warm = storyboard_in(Some(&root), &request, cancel(false)).unwrap();
        same(&cold, &warm);
        let frame_entry = fs::read_dir(root.join("frames"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        same(&cold, &read(&frame_entry, 3).unwrap());
        let blocked = dir.path().join("not-a-directory");
        fs::write(&blocked, b"blocked").unwrap();
        assert!(thumbnail_in(Some(&blocked), &request.path).is_ok());
        same(
            &cold,
            &storyboard_in(Some(&blocked), &request, cancel(false)).unwrap(),
        );
    }

    #[test]
    fn storyboard_cache_separates_playback_directions_and_holds() {
        let dir = tempfile::tempdir().unwrap();
        let mut request = fixture(dir.path(), "64x36");
        let root = dir.path().join("cache");
        for playback in [
            crate::timeline::Playback::Forward,
            crate::timeline::Playback::Reverse,
            crate::timeline::Playback::Freeze,
        ] {
            request.playback = playback;
            let cold = super::super::preview::storyboard(&request, cancel(false)).unwrap();
            let cached = storyboard_in(Some(&root), &request, cancel(false)).unwrap();
            same(&cold, &cached);
            same(
                &cold,
                &storyboard_in(Some(&root), &request, cancel(false)).unwrap(),
            );
        }
        assert_eq!(fs::read_dir(root.join("frames")).unwrap().count(), 3);
    }

    #[test]
    fn source_metadata_and_parameters_invalidate_keys() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        fs::write(&source, b"one").unwrap();
        let initial = key(&source, b"params").unwrap();
        File::open(&source)
            .unwrap()
            .set_modified(UNIX_EPOCH + Duration::from_secs(123))
            .unwrap();
        let touched = key(&source, b"params").unwrap();
        assert_ne!(initial, touched);
        fs::write(&source, b"longer").unwrap();
        assert_ne!(touched, key(&source, b"params").unwrap());
        assert_ne!(key(&source, b"params"), key(&source, b"other"));
        fs::remove_file(&source).unwrap();
        assert!(key(&source, b"params").is_none());
    }

    fn pixel() -> Vec<FrameImage> {
        vec![FrameImage {
            width: 1,
            height: 1,
            pixels: vec![1, 2, 3, 4],
        }]
    }

    #[test]
    fn malformed_entries_are_removed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("frames/0000000000000000");
        write(dir.path(), &path, &pixel(), CAP).unwrap();
        let valid = fs::read(&path).unwrap();
        for length in 0..valid.len() {
            fs::write(&path, &valid[..length]).unwrap();
            assert!(read(&path, 1).is_none());
            assert!(!path.exists());
        }
        for (offset, value) in [(0, 0), (8, 255), (12, 0), (16, 0), (19, 255), (20, 0)] {
            let mut invalid = valid.clone();
            invalid[offset] = value;
            fs::write(&path, invalid).unwrap();
            assert!(read(&path, 1).is_none());
            assert!(!path.exists());
        }
        let mut trailing = valid;
        trailing.push(0);
        fs::write(&path, trailing).unwrap();
        assert!(read(&path, 1).is_none());
        assert!(!path.exists());
    }

    #[test]
    fn eviction_is_shared_and_keeps_recent_hits() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("thumbnails/0000000000000000");
        let b = dir.path().join("frames/0000000000000001");
        let c = dir.path().join("frames/0000000000000002");
        write(dir.path(), &a, &pixel(), CAP).unwrap();
        write(dir.path(), &b, &pixel(), CAP).unwrap();
        for path in [&a, &b] {
            File::open(path).unwrap().set_modified(UNIX_EPOCH).unwrap();
        }
        assert!(read(&a, 1).is_some());
        let size = a.metadata().unwrap().len();
        write(dir.path(), &c, &pixel(), size * 2).unwrap();
        assert!(a.exists());
        assert!(!b.exists());
        assert!(c.exists());
        assert!(a.metadata().unwrap().len() + c.metadata().unwrap().len() <= size * 2);
        evict(dir.path(), 0).unwrap();
        assert!(!a.exists() && !c.exists());
    }

    #[test]
    fn failures_empty_and_cancelled_storyboards_are_not_cached() {
        let dir = tempfile::tempdir().unwrap();
        let mut request = fixture(dir.path(), "160x90");
        let root = dir.path().join("cache");
        assert!(
            storyboard_in(Some(&root), &request, cancel(true))
                .unwrap()
                .is_empty()
        );
        request.duration = 0;
        assert!(storyboard_in(Some(&root), &request, cancel(false)).is_err());
        fs::write(&request.path, b"not media").unwrap();
        request.duration = 60;
        assert!(storyboard_in(Some(&root), &request, cancel(false)).is_err());
        assert!(thumbnail_in(Some(&root), &request.path).is_err());
        assert!(!root.exists());
    }

    #[test]
    #[ignore]
    fn disk_media_cache_benchmark() {
        let dir = tempfile::tempdir().unwrap();
        let request = fixture(dir.path(), "1920x1080");
        let root = dir.path().join("cache");
        let mut cold = Vec::new();
        let mut warm = Vec::new();
        for _ in 0..5 {
            if root.exists() {
                fs::remove_dir_all(&root).unwrap();
            }
            for samples in [&mut cold, &mut warm] {
                let start = Instant::now();
                let thumb = thumbnail_in(Some(&root), &request.path).unwrap();
                let frames = storyboard_in(Some(&root), &request, cancel(false)).unwrap();
                assert!(!thumb.pixels.is_empty());
                assert_eq!(frames.len(), 3);
                samples.push(start.elapsed());
            }
        }
        cold.sort();
        warm.sort();
        println!(
            "\n| 1080p thumbnail + storyboard (5 runs) | Median |\n|---|---:|\n| Cold | {:.3} ms |\n| Warm | {:.3} ms |\n| Speedup | {:.2}x |",
            cold[2].as_secs_f64() * 1000.0,
            warm[2].as_secs_f64() * 1000.0,
            cold[2].as_secs_f64() / warm[2].as_secs_f64()
        );
    }
}
