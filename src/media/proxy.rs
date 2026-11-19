use super::cache;
use ffmpeg_next as ffmpeg;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

const PARAMETERS: &[u8] = b"proxy:v1:mjpeg:1280x720:bt601:origin";
const CAP: u64 = 512 * 1024 * 1024;

fn entry(root: &Path, source: &Path) -> Option<PathBuf> {
    Some(
        root.join("proxies")
            .join(format!("{}.nut", cache::key(source, PARAMETERS)?)),
    )
}

pub fn ready(source: &Path) -> Option<PathBuf> {
    ready_in(&cache::directory()?, source)
}

fn ready_in(root: &Path, source: &Path) -> Option<PathBuf> {
    let path = entry(root, source)?;
    let file = fs::File::open(&path).ok()?;
    if file.metadata().ok()?.len() == 0 {
        return None;
    }
    let _ = file.set_modified(SystemTime::now());
    Some(path)
}

pub fn generate(source: &Path, cancelled: &AtomicBool) -> Result<bool, String> {
    let root = cache::directory().ok_or("Media cache is unavailable")?;
    generate_in(&root, source, cancelled)
}

fn generate_in(root: &Path, source: &Path, cancelled: &AtomicBool) -> Result<bool, String> {
    if cancelled.load(Ordering::Relaxed) {
        return Err("Proxy cancelled".into());
    }
    if ready_in(root, source).is_some() {
        return Ok(true);
    }
    ffmpeg::init().map_err(|error| error.to_string())?;
    if ffmpeg::encoder::find(ffmpeg::codec::Id::MJPEG).is_none() {
        return Err("FFmpeg MJPEG encoder is unavailable".into());
    }
    let input = ffmpeg::format::input(source).map_err(|error| error.to_string())?;
    let stream = input
        .streams()
        .best(ffmpeg::media::Type::Video)
        .ok_or("No video")?;
    let decoder = ffmpeg::codec::context::Context::from_parameters(stream.parameters())
        .and_then(|context| context.decoder().video())
        .map_err(|error| error.to_string())?;
    let video_stream = format!("0:{}", stream.index());
    let (width, height) = super::Rotation::from_stream(&stream)
        .dimensions(super::display_dimensions(&decoder).0, decoder.height());
    if width <= 1920 && height <= 1080 {
        return Ok(false);
    }
    let destination = entry(root, source).ok_or("Cannot identify proxy source")?;
    let directory = destination.parent().unwrap();
    fs::create_dir_all(directory).map_err(|error| error.to_string())?;
    let temporary = tempfile::Builder::new()
        .prefix(".proxy-")
        .suffix(".nut")
        .tempfile_in(directory)
        .map_err(|error| error.to_string())?;
    // copyts/start_at_zero retains a late video stream's offset from the format origin.
    let mut child = Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-y", "-copyts", "-start_at_zero", "-i"])
        .arg(source)
        .args(["-map", &video_stream, "-an", "-sn", "-dn", "-vf",
            "scale=w='max(2,trunc(iw*sar*min(1280/(iw*sar),720/ih)/2)*2)':h='max(2,trunc(ih*min(1280/(iw*sar),720/ih)/2)*2)':out_color_matrix=bt601:out_range=full,setsar=1",
            "-vsync", "0", "-c:v", "mjpeg", "-threads", "2", "-q:v", "3", "-pix_fmt", "yuvj420p",
            "-colorspace", "smpte170m", "-color_range", "pc", "-map_metadata", "-1", "-avoid_negative_ts", "disabled", "-enc_time_base", "-1", "-f", "nut"])
        .arg(temporary.path()).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
        .spawn().map_err(|error| format!("Cannot start FFmpeg for proxy: {error}"))?;
    let status = loop {
        if cancelled.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Proxy cancelled".into());
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.to_string());
            }
        }
    };
    if !status.success() || cancelled.load(Ordering::Relaxed) {
        return Err("Proxy generation failed or was cancelled".into());
    }
    if entry(root, source).as_ref() != Some(&destination) {
        return Err("Source changed during proxy generation".into());
    }
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| error.to_string())?;
    temporary
        .persist(&destination)
        .map_err(|error| error.to_string())?;
    cache::evict(root, CAP).map_err(|error| error.to_string())?;
    Ok(destination.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(path: &Path, duration: &str, size: &str) {
        assert!(
            Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-y",
                    "-f",
                    "lavfi",
                    "-i",
                    &format!("testsrc2=s={size}:r=30:d={duration}"),
                    "-c:v",
                    "mpeg4",
                    "-threads",
                    "2"
                ])
                .arg(path)
                .status()
                .unwrap()
                .success()
        );
    }

    #[test]
    fn cache_identity_and_atomic_generation() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("cache");
        let source = directory.path().join("source.mp4");
        fixture(&source, "0.1", "2048x1152");
        let key = entry(&root, &source).unwrap();
        assert!(ready_in(&root, &source).is_none());
        assert!(generate_in(&root, &source, &AtomicBool::new(true)).is_err());
        assert!(!key.exists());
        assert!(generate_in(&root, &source, &AtomicBool::new(false)).unwrap());
        let contents = fs::read(&key).unwrap();
        assert!(generate_in(&root, &source, &AtomicBool::new(false)).unwrap());
        assert_eq!(contents, fs::read(&key).unwrap());
        fs::File::options()
            .write(true)
            .open(&source)
            .unwrap()
            .set_modified(SystemTime::now() + Duration::from_secs(2))
            .unwrap();
        assert_ne!(entry(&root, &source).unwrap(), key);
        assert!(ready_in(&root, &source).is_none());
        fs::write(&source, b"changed").unwrap();
        assert_ne!(entry(&root, &source).unwrap(), key);
        assert!(ready_in(&root, &source).is_none());
        assert!(generate_in(&root, &source, &AtomicBool::new(false)).is_err());
        assert_eq!(fs::read_dir(root.join("proxies")).unwrap().count(), 1);
    }

    #[test]
    fn cancellation_removes_in_progress_file_and_small_media_is_skipped() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.mp4");
        fixture(&source, "4", "2048x1152");
        let root = directory.path().join("cache");
        let cancelled = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let worker = scope.spawn(|| generate_in(&root, &source, &cancelled));
            let started = std::time::Instant::now();
            while !root.join("proxies").is_dir() {
                assert!(started.elapsed() < Duration::from_secs(10));
                std::thread::sleep(Duration::from_millis(1));
            }
            cancelled.store(true, Ordering::Relaxed);
            assert!(worker.join().unwrap().is_err());
        });
        assert_eq!(fs::read_dir(root.join("proxies")).unwrap().count(), 0);
        fixture(&source, "0.1", "320x180");
        assert!(!generate_in(&root, &source, &AtomicBool::new(false)).unwrap());
        assert_eq!(fs::read_dir(root.join("proxies")).unwrap().count(), 0);
    }
}
