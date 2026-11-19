use ocus::media::effects;
use ocus::project::{Project, format, store};
use ocus::timeline::EffectShader;
use std::{fs, process::Command};

#[test]
fn grayscale_effect_survives_project_save_and_video_export() {
    let shader = EffectShader::Builtin("grayscale".into());
    if let Err(error) = effects::validate(&shader) {
        if error.contains("No GPU adapter") {
            eprintln!("Skipping GPU effect export: {error}");
            return;
        }
        panic!("Could not compile built-in: {error}");
    }
    let temporary = tempfile::tempdir().unwrap();
    let root = std::env::var_os("OCUS_EFFECT_EVIDENCE")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| temporary.path().to_owned());
    fs::create_dir_all(&root).unwrap();
    let image = root.join("red.tga");
    let mut tga = vec![0; 18];
    tga[2] = 2;
    tga[12..14].copy_from_slice(&320u16.to_le_bytes());
    tga[14..16].copy_from_slice(&180u16.to_le_bytes());
    tga[16] = 32;
    tga[17] = 40;
    tga.extend([0, 0, 255, 255].repeat(320 * 180));
    fs::write(&image, tga).unwrap();
    let mut document = serde_json::to_value(Project::new("WGSL grayscale demo")).unwrap();
    document["version"] = 1.into();
    document["settings"]["width"] = 320.into();
    document["settings"]["height"] = 180.into();
    document["media"] = serde_json::json!([
        {"id": 1, "path": image, "kind": "image", "has_audio": false}
    ]);
    document["tracks"][0]["clips"] = serde_json::json!([
        {"id": 1, "start_frame": 0, "source_start_frame": 0, "duration_frames": 30,
         "content": {"media": {"media_id": 1}}, "volume": 1.0, "muted": false,
         "effect": {"shader": {"builtin": "grayscale"}, "amount": 1.0}}
    ]);
    let project = format::from_json(&document.to_string()).unwrap();
    let path = root.join("grayscale.ocus");
    store::save(&path, &project).unwrap();
    let loaded = store::load(&path).unwrap();
    assert_eq!(loaded, project);
    for format in [
        ocus::project::interchange::Format::Otio,
        ocus::project::interchange::Format::Edl,
    ] {
        let notes = ocus::project::interchange::save(
            &root.join(format!("grayscale.{}", format.extension())),
            &loaded,
            format,
        )
        .unwrap();
        assert!(
            notes
                .iter()
                .any(|note| note.contains("effects") && note.contains("not exported"))
        );
    }
    let video = root.join("grayscale.mp4");
    ocus::export::render(&loaded, &video).unwrap();
    let output = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&video)
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "rgba", "-"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout.len(), 320 * 180 * 4);
    for pixel in output.stdout.as_chunks::<4>().0 {
        assert!(
            pixel[..3].iter().all(|channel| channel.abs_diff(54) <= 3),
            "{pixel:?}"
        );
        assert_eq!(pixel[3], 255);
    }
    println!("Exported grayscale RGBA frame: all 57600 pixels within 3 of [54, 54, 54, 255]");
}
