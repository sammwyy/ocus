use ocus::media::composite::{Compositor, VisualPlan};
use ocus::media::preview::StoryboardKey;
use ocus::media::{Media, MediaKind, cache};
use ocus::project::{Command, Editor, Project, store};
use ocus::timeline::{Clip, ClipContent, Playback, Transform};
use std::fs;
use std::hint::black_box;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

fn median(mut samples: Vec<f64>) -> f64 {
    samples.sort_by(f64::total_cmp);
    samples[samples.len() / 2]
}

fn measure(mut operation: impl FnMut()) -> f64 {
    median(
        (0..5)
            .map(|_| {
                let start = Instant::now();
                operation();
                start.elapsed().as_secs_f64() * 1000.0
            })
            .collect(),
    )
}

fn project(path: &Path) -> Editor {
    let mut editor = Editor::new(Project::new("CI 1080p"));
    assert!(editor.apply(Command::ImportMedia(Media {
        id: 1,
        path: path.to_owned(),
        kind: MediaKind::Video,
        duration_us: Some(2_000_000),
        has_audio: false,
    })));
    for id in 1..=100 {
        assert!(editor.apply(Command::InsertClip {
            track_id: 0,
            clip: Box::new(Clip {
                id,
                start_frame: (id - 1) * 60,
                source_start_frame: 0,
                duration_frames: 60,
                speed: 1.0,
                content: ClipContent::Media { media_id: 1 },
                volume: 1.0,
                muted: false,
                linked_audio: false,
                fade_in_frames: 0,
                fade_out_frames: 0,
                noise_reduction: false,
                playback: Playback::Forward,
                color: Default::default(),
                transform: Transform::default(),
                stabilization: None,
                keyframes: Vec::new(),
                lut: None,
                chroma_key: None,
                transition: None,
                transition_out: None,
            }),
        }));
    }
    editor
}

fn run() -> Result<bool, Box<dyn std::error::Error>> {
    ocus::media::hardware::set_enabled(false);
    let dir = tempfile::tempdir()?;
    let source = dir.path().join("source.mkv");
    // Only fixture generation uses the CLI; all timed operations use Ocus APIs.
    let status = std::process::Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=size=1920x1080:rate=30:duration=2",
            "-an",
            "-c:v",
            "mpeg4",
            "-y",
        ])
        .arg(&source)
        .status()?;
    if !status.success() {
        return Err("FFmpeg fixture generation failed".into());
    }
    let cancelled = Arc::new(AtomicBool::new(false));
    let mut cold = Vec::new();
    let mut warm = Vec::new();
    for sample in 0..5 {
        // A fresh source identity guarantees a cache miss without deleting user caches.
        let path = dir.path().join(format!("sample-{sample}.mkv"));
        fs::copy(&source, &path)?;
        let request = StoryboardKey {
            transform_bits: StoryboardKey::transform_bits(Transform::default()),
            lut: None,
            chroma_key: None,
            color_bits: [0.0f32, 1.0, 1.0].map(f32::to_bits),
            playback: Playback::Forward,
            path: path.clone(),
            kind: MediaKind::Video,
            source_start: 0,
            duration: 60,
            fps: (30, 1),
        };
        for samples in [&mut cold, &mut warm] {
            let start = Instant::now();
            let thumb = cache::thumbnail(&path)?;
            let frames = cache::storyboard(&request, cancelled.clone(), false)?;
            assert!(!thumb.pixels.is_empty());
            assert_eq!(frames.len(), 3);
            black_box((&thumb, &frames));
            samples.push(start.elapsed().as_secs_f64() * 1000.0);
        }
    }
    let mut editor = project(&source);
    let file = dir.path().join("project.ocus");
    let save = measure(|| store::save(&file, black_box(editor.project())).unwrap());
    let open = measure(|| assert_eq!(black_box(store::load(&file).unwrap()), *editor.project()));
    let edit = measure(|| {
        for _ in 0..100 {
            assert!(editor.apply(Command::RenameTrack {
                track_id: 0,
                name: "Video".into()
            }));
            assert!(editor.undo());
        }
    }) / 100.0;
    let render = measure(|| {
        let mut compositor = Compositor::new(
            VisualPlan::from_project(editor.project()),
            1920,
            1080,
            (30, 1),
        );
        for position in 0..30 {
            let frame = compositor
                .frame(position, &cancelled)
                .unwrap()
                .expect("decoded frame");
            assert_eq!(
                (frame.width, frame.height, frame.pixels.len()),
                (1920, 1080, 1920 * 1080 * 4)
            );
            black_box(frame);
        }
    }) / 30.0;
    let results = [
        ("1080p thumbnail + storyboard, cold", median(cold), 2000.0),
        ("1080p thumbnail + storyboard, warm", median(warm), 100.0),
        ("Save 100-clip project (atomic disk write)", save, 500.0),
        ("Open 100-clip project (including probe)", open, 500.0),
        ("Timeline command + undo", edit, 10.0),
        ("1080p decode + CPU composition, ms/frame", render, 100.0),
    ];
    let mut markdown =
        String::from("| Benchmark | Measured (ms) | Target (ms) | Result |\n|---|---:|---:|---|\n");
    let mut csv = String::from("benchmark,measured_ms,target_ms,pass\n");
    for (name, measured, target) in results {
        let pass = measured.is_finite() && measured < target;
        markdown.push_str(&format!(
            "| {name} | {measured:.6} | < {target:.0} | {} |\n",
            if pass { "PASS" } else { "FAIL" }
        ));
        csv.push_str(&format!("\"{name}\",{measured:.6},{target},{pass}\n"));
    }
    fs::create_dir_all("target/benchmarks")?;
    fs::write("target/benchmarks/results.md", &markdown)?;
    fs::write("target/benchmarks/results.csv", csv)?;
    println!("{markdown}");
    Ok(results
        .iter()
        .all(|(_, measured, target)| measured.is_finite() && measured < target))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if !std::env::args().any(|arg| arg == "--bench") {
        return Ok(());
    }
    if cfg!(debug_assertions) {
        return Err("Run with cargo bench --locked --bench performance".into());
    }
    if !run()? {
        return Err("Performance budget exceeded; see target/benchmarks/results.csv".into());
    }
    Ok(())
}
