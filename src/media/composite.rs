use super::preview::{FrameImage, FrameSource, VideoLayers, blend};
use crate::project::Project;
use crate::timeline::{ClipContent, Title, TrackKind};
use crate::ui::{title_fonts, title_markup};
use creamui::fonts::{FontWeight, HorizontalAlign, LayoutSettings, cached_layout, resolve};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use swash::scale::{Render, ScaleContext, Source, image::Image};
use swash::zeno::{Format, Join, Stroke};

#[derive(Clone)]
pub struct VisualPlan {
    tracks: Vec<Vec<VisualClip>>,
    resolution: (u32, u32),
}

#[derive(Clone)]
enum VisualClip {
    Media(FrameSource),
    Title {
        clip_id: u64,
        start: u64,
        duration: u64,
        title: Title,
    },
}

impl VisualPlan {
    pub fn from_project(project: &Project) -> Self {
        Self::without_media(project, &HashSet::new())
    }

    /// The project's visuals with clips of the `missing` media left out.
    pub fn without_media(project: &Project, missing: &HashSet<u64>) -> Self {
        let tracks = project
            .tracks()
            .iter()
            .filter(|track| track.kind == TrackKind::Video)
            .map(|track| {
                track
                    .clips
                    .iter()
                    .filter_map(|clip| match &clip.content {
                        ClipContent::Media { media_id } if !missing.contains(media_id) => {
                            let media =
                                project.media().iter().find(|media| media.id == *media_id)?;
                            Some(VisualClip::Media(FrameSource {
                                playback: clip.playback,
                                clip_id: clip.id,
                                path: media.path.clone(),
                                kind: media.kind,
                                start: clip.start_frame,
                                source_start: clip.source_start_frame,
                                duration: clip.duration_frames,
                            }))
                        }
                        ClipContent::Media { .. } => None,
                        ClipContent::Title(title) => Some(VisualClip::Title {
                            clip_id: clip.id,
                            start: clip.start_frame,
                            duration: clip.duration_frames,
                            title: title.clone(),
                        }),
                    })
                    .collect()
            })
            .collect();
        Self {
            tracks,
            resolution: (project.settings().width, project.settings().height),
        }
    }

    pub fn replace_title(&mut self, clip_id: u64, title: Title) {
        for track in &mut self.tracks {
            for clip in track {
                if let VisualClip::Title {
                    clip_id: id,
                    title: current,
                    ..
                } = clip
                    && *id == clip_id
                {
                    *current = title;
                    return;
                }
            }
        }
    }
}

pub struct Compositor {
    width: u32,
    height: u32,
    plan: VisualPlan,
    layers: VideoLayers,
    glyphs: Glyphs,
}

enum Layer<'a> {
    Media(Arc<FrameImage>),
    Title(&'a Title),
}

impl Compositor {
    pub fn new(plan: VisualPlan, width: u32, height: u32, fps: (u32, u32)) -> Self {
        Self {
            width,
            height,
            plan,
            layers: VideoLayers::new(fps, (width, height)),
            glyphs: Glyphs::default(),
        }
    }

    pub fn frame(
        &mut self,
        position: u64,
        cancelled: &Arc<AtomicBool>,
    ) -> Result<Option<FrameImage>, String> {
        let mut pixels = vec![0; self.width as usize * self.height as usize * 4];
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel.copy_from_slice(&[0, 0, 0, 255]);
        }
        let mut active = Vec::new();
        let mut visible = false;
        let mut pending = Vec::new();
        'layers: for track in self.plan.tracks.iter().rev() {
            for clip in track.iter().rev() {
                match clip {
                    VisualClip::Media(source)
                        if source.start <= position
                            && position - source.start < source.duration =>
                    {
                        visible = true;
                        active.push(source.clip_id);
                        let image = self.layers.layer_at(source, position, cancelled)?;
                        let opaque = image.width == self.width
                            && image.height == self.height
                            && image.pixels.len() == pixels.len()
                            && image
                                .pixels
                                .as_chunks::<4>()
                                .0
                                .iter()
                                .all(|pixel| pixel[3] == 255);
                        pending.push(Layer::Media(image));
                        if opaque {
                            break 'layers;
                        }
                    }
                    VisualClip::Title {
                        clip_id: _,
                        start,
                        duration,
                        title,
                    } if *start <= position && position - *start < *duration => {
                        visible = true;
                        pending.push(Layer::Title(title));
                    }
                    _ => {}
                }
            }
        }
        for layer in pending.into_iter().rev() {
            match layer {
                Layer::Media(image) => {
                    for (dest, src) in pixels
                        .as_chunks_mut::<4>()
                        .0
                        .iter_mut()
                        .zip(image.pixels.as_chunks::<4>().0)
                    {
                        blend(dest, src);
                    }
                }
                Layer::Title(title) => {
                    self.glyphs.draw(
                        title,
                        self.width,
                        self.height,
                        self.plan.resolution.0,
                        self.width as usize * 4,
                        &mut pixels,
                    );
                }
            }
        }
        self.layers.retain(&active);
        Ok(visible.then_some(FrameImage {
            width: self.width,
            height: self.height,
            pixels,
        }))
    }
}

#[derive(Default)]
struct Glyphs {
    scaler: ScaleContext,
    cache: HashMap<(u64, u16, u32, bool), GlyphMasks>,
}

struct GlyphMasks {
    fill: Option<Image>,
    outline: Option<Image>,
}

pub(crate) fn title_outline_radius(scaled_font_size: f32) -> f32 {
    scaled_font_size * 0.02
}

impl Glyphs {
    fn draw(
        &mut self,
        title: &Title,
        width: u32,
        height: u32,
        native_width: u32,
        stride: usize,
        pixels: &mut [u8],
    ) {
        let mut canvas = Canvas {
            width,
            height,
            stride,
            pixels,
        };
        let size = (title.font_size * width as f32 / native_width.max(1) as f32)
            .max(1.0)
            .min(height as f32 * 2.0);
        let mut lines = vec![Vec::new()];
        for run in title_markup::parse(title) {
            let mut parts = run.text.split('\n').peekable();
            while let Some(part) = parts.next() {
                if !part.is_empty() {
                    let mut styled = run.clone();
                    styled.text = part.to_owned();
                    lines.last_mut().unwrap().push(styled);
                }
                if parts.peek().is_some() {
                    lines.push(Vec::new());
                }
            }
        }
        let line_height = size * 1.35;
        let start_y = height as f32 * title.y - line_height * lines.len() as f32 / 2.0;
        let family = title_fonts::family_stack(&title.font_family);
        for (line_index, line) in lines.iter().enumerate() {
            let layouts: Vec<_> = line
                .iter()
                .map(|run| {
                    let face = resolve(
                        &family,
                        if run.bold {
                            FontWeight::Bold
                        } else {
                            FontWeight::Regular
                        },
                    );
                    let layout = cached_layout(
                        &face,
                        &run.text,
                        size,
                        &LayoutSettings {
                            horizontal_align: HorizontalAlign::Left,
                            ..Default::default()
                        },
                    );
                    let run_width = layout.lines.first().map_or(0.0, |line| line.width);
                    (face, layout, run_width)
                })
                .collect();
            let mut x = width as f32 * title.x
                - layouts.iter().map(|(_, _, width)| width).sum::<f32>() / 2.0;
            for (run, (face, layout, run_width)) in line.iter().zip(layouts) {
                for glyph in &layout.glyphs {
                    let face = if glyph.face == 0 {
                        &face
                    } else {
                        &layout.fallback_faces[glyph.face - 1]
                    };
                    let key = (face.id(), glyph.id, size.to_bits(), title.outline);
                    if self.cache.len() >= 4096 && !self.cache.contains_key(&key) {
                        self.cache.clear();
                    }
                    let masks = self.cache.entry(key).or_insert_with(|| {
                        let mut scaler = self
                            .scaler
                            .builder(face.font_ref())
                            .size(size)
                            .hint(false)
                            .build();
                        let mut render = Render::new(&[Source::Outline]);
                        render.format(Format::Alpha);
                        let fill = render.render(&mut scaler, glyph.id);
                        let outline = if title.outline && fill.is_some() {
                            let mut stroke = Stroke::new(2.0 * title_outline_radius(size));
                            stroke.join(Join::Round);
                            render.style(stroke).render(&mut scaler, glyph.id)
                        } else {
                            None
                        };
                        GlyphMasks { fill, outline }
                    });
                    let Some(fill) = &masks.fill else {
                        continue;
                    };
                    let gx = (x + glyph.x).round() as i32;
                    let gy = (start_y + line_index as f32 * line_height + glyph.y).round() as i32;
                    // Use the same shear origin for both masks, even when the outline is taller.
                    let italic_bottom = run
                        .italic
                        .then(|| gy - fill.placement.top + fill.placement.height as i32 - 1);
                    if let Some(outline) = &masks.outline {
                        paint(
                            outline,
                            (gx, gy),
                            &mut canvas,
                            [0, 0, 0, run.color.a],
                            italic_bottom,
                        );
                    }
                    paint(
                        fill,
                        (gx, gy),
                        &mut canvas,
                        [run.color.r, run.color.g, run.color.b, run.color.a],
                        italic_bottom,
                    );
                }
                x += run_width;
            }
        }
    }
}

struct Canvas<'a> {
    width: u32,
    height: u32,
    stride: usize,
    pixels: &'a mut [u8],
}

fn paint(
    mask: &Image,
    origin: (i32, i32),
    canvas: &mut Canvas<'_>,
    color: [u8; 4],
    italic_bottom: Option<i32>,
) {
    let (x, y) = (
        origin.0 + mask.placement.left,
        origin.1 - mask.placement.top,
    );
    let width = mask.placement.width;
    for row in 0..mask.placement.height {
        let py = y + row as i32;
        if py < 0 || py >= canvas.height as i32 {
            continue;
        }
        let shear = italic_bottom.map_or(0, |bottom| ((bottom - py) as f32 * 0.22).round() as i32);
        for col in 0..width {
            let px = x + col as i32 + shear;
            if px < 0 || px >= canvas.width as i32 {
                continue;
            }
            let coverage = u32::from(mask.data[(row * width + col) as usize]);
            let offset = py as usize * canvas.stride + px as usize * 4;
            let mut rgba = color;
            rgba[3] = ((u32::from(color[3]) * coverage + 127) / 255) as u8;
            blend(&mut canvas.pixels[offset..offset + 4], &rgba);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::MediaKind;
    use crate::media::preview::{PreviewEvent, PreviewPlan, decode_sequence};
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEST: AtomicU64 = AtomicU64::new(0);

    #[test]
    #[ignore = "release-mode still-image benchmark; requires ffmpeg"]
    fn benchmark_still_image() {
        let root = std::env::temp_dir().join(format!("ocus-still-bench-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("still.png");
        assert!(
            std::process::Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-y",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=size=4000x3000",
                    "-frames:v",
                    "1",
                    "-threads",
                    "1"
                ])
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        let plan = VisualPlan {
            resolution: (1920, 1080),
            tracks: vec![vec![VisualClip::Media(FrameSource {
                playback: crate::timeline::Playback::Forward,
                clip_id: 1,
                path,
                kind: MediaKind::Image,
                start: 0,
                source_start: 0,
                duration: 300,
            })]],
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut compositor = Compositor::new(plan, 1920, 1080, (30, 1));
        let started = std::time::Instant::now();
        let mut hash = 0xcbf29ce484222325u64;
        for position in 0..300 {
            let frame = compositor.frame(position, &cancelled).unwrap().unwrap();
            for byte in &frame.pixels {
                hash = (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
            }
            std::hint::black_box(&frame);
        }
        println!(
            "still frames=300 elapsed_ms={} fnv1a={hash:016x}",
            started.elapsed().as_millis()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[ignore = "release-mode held-video-frame benchmark; requires ffmpeg"]
    fn benchmark_video_frame_rates() {
        let root = std::env::temp_dir().join(format!("ocus-video-bench-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        for (rate, fps) in [(5, (30, 1)), (24, (60, 1)), (30, (30, 1))] {
            let path = root.join(format!("{rate}fps.mp4"));
            assert!(
                std::process::Command::new("ffmpeg")
                    .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
                    .arg(format!("testsrc2=size=1920x1080:rate={rate}:duration=10"))
                    .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-threads", "1"])
                    .arg(&path)
                    .status()
                    .unwrap()
                    .success()
            );
            let frames = 10 * u64::from(fps.0 / fps.1);
            let plan = VisualPlan {
                resolution: (1920, 1080),
                tracks: vec![vec![VisualClip::Media(FrameSource {
                    playback: crate::timeline::Playback::Forward,
                    clip_id: 1,
                    path,
                    kind: MediaKind::Video,
                    start: 0,
                    source_start: 0,
                    duration: frames,
                })]],
            };
            let cancelled = Arc::new(AtomicBool::new(false));
            let mut compositor = Compositor::new(plan, 1920, 1080, fps);
            let mut elapsed = std::time::Duration::ZERO;
            let mut hash = 0xcbf29ce484222325u64;
            for position in 0..frames {
                let started = std::time::Instant::now();
                let frame = compositor.frame(position, &cancelled).unwrap().unwrap();
                elapsed += started.elapsed();
                for byte in &frame.pixels {
                    hash = (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
                }
            }
            println!(
                "video source_fps={rate} sequence_fps={} frames={frames} elapsed_ms={} fnv1a={hash:016x}",
                fps.0,
                elapsed.as_millis()
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    fn full_blend(plan: &VisualPlan, position: u64) -> Vec<u8> {
        let (width, height) = plan.resolution;
        let mut pixels = [0, 0, 0, 255].repeat(width as usize * height as usize);
        let mut layers = VideoLayers::new((30, 1), (width, height));
        let mut glyphs = Glyphs::default();
        let cancelled = Arc::new(AtomicBool::new(false));
        for clip in plan.tracks.iter().flatten() {
            match clip {
                VisualClip::Media(source)
                    if source.start <= position && position - source.start < source.duration =>
                {
                    let image = layers.layer_at(source, position, &cancelled).unwrap();
                    for (dest, src) in pixels
                        .as_chunks_mut::<4>()
                        .0
                        .iter_mut()
                        .zip(image.pixels.as_chunks::<4>().0)
                    {
                        blend(dest, src);
                    }
                }
                VisualClip::Title {
                    start,
                    duration,
                    title,
                    ..
                } if *start <= position && position - *start < *duration => {
                    glyphs.draw(title, width, height, width, width as usize * 4, &mut pixels);
                }
                _ => {}
            }
        }
        pixels
    }

    #[test]
    fn covered_layers_match_full_blend_without_decoding_hidden_media() {
        let dir = std::env::temp_dir().join(format!(
            "ocus-coverage-{}-{}",
            std::process::id(),
            NEXT_TEST.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        let fixture = |name: &str, width: u16, height: u16, alpha| {
            let path = dir.join(name);
            let mut tga = vec![0; 18];
            tga[2] = 2;
            tga[12..14].copy_from_slice(&width.to_le_bytes());
            tga[14..16].copy_from_slice(&height.to_le_bytes());
            tga[16] = 32;
            tga[17] = 40;
            tga.extend(
                [210 - alpha / 2, 70, alpha / 2, alpha].repeat(width as usize * height as usize),
            );
            fs::write(&path, tga).unwrap();
            path
        };
        let opaque = fixture("opaque.tga", 16, 9, 255);
        let fitted = fixture("fitted.tga", 8, 9, 255);
        let alpha = fixture("alpha.tga", 16, 9, 128);
        let transparent = fixture("transparent.tga", 16, 9, 0);
        let source = |id, path| {
            VisualClip::Media(FrameSource {
                playback: crate::timeline::Playback::Forward,
                clip_id: id,
                path,
                kind: MediaKind::Image,
                start: 0,
                source_start: 0,
                duration: 3,
            })
        };
        let title = VisualClip::Title {
            clip_id: 20,
            start: 0,
            duration: 3,
            title: Title {
                text: "X".into(),
                font_size: 20.0,
                ..Title::default()
            },
        };
        let stacked: Vec<_> = (0..10).map(|id| vec![source(id, opaque.clone())]).collect();
        let mut titled = stacked.clone();
        titled.push(vec![title.clone()]);
        let scenarios = [
            (stacked, 1),
            (titled, 1),
            (
                vec![vec![source(0, opaque.clone())], vec![source(1, fitted)]],
                2,
            ),
            (
                vec![vec![source(0, opaque.clone())], vec![source(1, alpha)]],
                2,
            ),
            (
                vec![
                    vec![source(0, opaque.clone())],
                    vec![source(1, transparent)],
                ],
                2,
            ),
            (vec![vec![source(0, opaque.clone())], vec![title]], 1),
        ];
        let cancelled = Arc::new(AtomicBool::new(false));
        for (tracks, decoded) in scenarios {
            let mut plan = VisualPlan {
                tracks,
                resolution: (160, 90),
            };
            let expected = full_blend(&plan, 0);
            let media_count = plan
                .tracks
                .iter()
                .flatten()
                .filter(|clip| matches!(clip, VisualClip::Media(_)))
                .count();
            for clip in plan
                .tracks
                .iter_mut()
                .flatten()
                .filter(|clip| matches!(clip, VisualClip::Media(_)))
                .take(media_count - decoded)
            {
                if let VisualClip::Media(source) = clip {
                    source.path = dir.join("must-not-be-decoded.tga");
                }
            }
            let mut compositor = Compositor::new(plan, 160, 90, (30, 1));
            assert_eq!(
                compositor.frame(0, &cancelled).unwrap().unwrap().pixels,
                expected
            );
            assert!(compositor.frame(3, &cancelled).unwrap().is_none());
        }
        let plan = VisualPlan {
            tracks: vec![
                vec![source(0, dir.join("missing.tga"))],
                vec![source(1, opaque)],
            ],
            resolution: (160, 90),
        };
        let mut compositor = Compositor::new(plan, 160, 90, (30, 1));
        assert!(compositor.frame(0, &cancelled).unwrap().is_some());
        compositor.plan.tracks.pop();
        assert!(compositor.frame(0, &cancelled).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn translucent_video_pixel_blends_over_lower_layer() {
        let mut background = [20, 40, 80, 255];
        crate::media::preview::blend(&mut background, &[220, 120, 0, 128]);
        assert_eq!(background, [120, 80, 40, 255]);
    }

    #[test]
    fn serif_title_matches_the_installed_serif_face() {
        let render = |family: &str| {
            let title = Title {
                text: "Serif".into(),
                font_family: family.into(),
                font_size: 42.0,
                ..Title::default()
            };
            let mut pixels = vec![0; 160 * 90 * 4];
            Glyphs::default().draw(&title, 160, 90, 160, 160 * 4, &mut pixels);
            pixels
        };
        let serif = render("Times New Roman, Georgia, DejaVu Serif, Liberation Serif, Noto Serif");
        assert!(
            render("Serif") == serif,
            "the generic Serif title should render like its concrete serif family"
        );
    }

    #[test]
    fn title_outline_keeps_its_weight_relative_to_the_fill() {
        let mut title = Title {
            text: "Outline".into(),
            font_size: 96.0,
            ..Title::default()
        };
        let mut glyphs = Glyphs::default();
        let mut weights = Vec::new();
        for (width, height) in [(640, 360), (1280, 720), (1920, 1080), (3840, 2160)] {
            let mut pixels = vec![128; width as usize * height as usize * 4];
            for pixel in pixels.as_chunks_mut::<4>().0 {
                pixel[3] = 255;
            }
            title.outline = false;
            glyphs.draw(&title, width, height, 1920, width as usize * 4, &mut pixels);
            let fill: u32 = pixels
                .as_chunks::<4>()
                .0
                .iter()
                .map(|pixel| u32::from(pixel[0] - 128))
                .sum();

            for pixel in pixels.as_chunks_mut::<4>().0 {
                *pixel = [128, 128, 128, 255];
            }
            title.outline = true;
            glyphs.draw(&title, width, height, 1920, width as usize * 4, &mut pixels);
            let outline: u32 = pixels
                .as_chunks::<4>()
                .0
                .iter()
                .map(|pixel| u32::from(128_u8.saturating_sub(pixel[0])))
                .sum();
            assert!(outline > 0 && fill > 0);
            weights.push(outline as f32 / fill as f32);
        }

        let reference = weights[2];
        // At monitor size, subpixel coverage varies more around the antialiased fill.
        assert!(
            weights
                .iter()
                .all(|weight| (0.70..=1.30).contains(&(weight / reference))),
            "outline-to-fill weights at preview, 720p, 1080p and 4K: {weights:?}"
        );
    }

    #[test]
    fn program_preview_and_export_share_lower_track_priority_and_title_occlusion() {
        let dir = std::env::temp_dir().join(format!(
            "ocus-layer-preview-{}-{}",
            std::process::id(),
            NEXT_TEST.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        let red = dir.join("base.ppm");
        let blue = dir.join("overlay.ppm");
        fs::write(
            &red,
            b"P6\n2 2\n255\n\xdc\x0a\x0a\xdc\x0a\x0a\xdc\x0a\x0a\xdc\x0a\x0a",
        )
        .unwrap();
        fs::write(
            &blue,
            b"P6\n2 2\n255\n\x0a\x0a\xdc\x0a\x0a\xdc\x0a\x0a\xdc\x0a\x0a\xdc",
        )
        .unwrap();
        let source = |id, path| {
            VisualClip::Media(FrameSource {
                playback: crate::timeline::Playback::Forward,
                clip_id: id,
                path,
                kind: MediaKind::Image,
                start: 0,
                source_start: 0,
                duration: 3,
            })
        };
        let title = VisualClip::Title {
            clip_id: 3,
            start: 0,
            duration: 3,
            title: Title {
                text: "X".into(),
                font_size: 42.0,
                color: [0, 255, 0, 255],
                ..Title::default()
            },
        };
        let plan = VisualPlan {
            tracks: vec![
                vec![source(1, red.clone())],
                vec![source(2, blue.clone())],
                vec![title.clone()],
            ],
            resolution: (160, 90),
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        let mut compositor = Compositor::new(plan.clone(), 160, 90, (30, 1));
        let image = compositor.frame(0, &cancelled).unwrap().unwrap();
        let picture = 80 * 4;
        assert!(image.pixels[picture + 2] > 180 && image.pixels[picture] < 60);
        assert!(
            image
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[1] > 150 && pixel[2] < 100)
        );
        let mut preview = None;
        decode_sequence(
            &PreviewPlan {
                sources: Vec::new(),
                visual: Some(plan.clone()),
                end: 3,
                fps: (30, 1),
                resolution: (160, 90),
            },
            0,
            false,
            cancelled.clone(),
            None,
            |event| {
                if let PreviewEvent::Frame(_, frame) = event {
                    preview = frame;
                }
            },
        )
        .unwrap();
        assert_eq!(preview.unwrap().pixels, image.pixels);
        let scaled = Compositor::new(plan.clone(), 80, 45, (30, 1))
            .frame(0, &cancelled)
            .unwrap()
            .unwrap();
        let green_count = |pixels: &[u8]| {
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .filter(|pixel| pixel[1] > 150 && pixel[2] < 100)
                .count()
        };
        assert!(green_count(&scaled.pixels) > 0);
        assert!(green_count(&scaled.pixels) < green_count(&image.pixels));
        let mut draft = plan.clone();
        draft.replace_title(
            3,
            Title {
                x: 0.8,
                text: "X".into(),
                font_size: 42.0,
                color: [0, 255, 0, 255],
                ..Title::default()
            },
        );
        let moved = Compositor::new(draft, 160, 90, (30, 1))
            .frame(0, &cancelled)
            .unwrap()
            .unwrap();
        let leftmost_green = |pixels: &[u8]| {
            pixels
                .as_chunks::<4>()
                .0
                .iter()
                .enumerate()
                .filter(|(_, pixel)| pixel[1] > 150 && pixel[2] < 100)
                .map(|(index, _)| index % 160)
                .min()
                .unwrap()
        };
        assert!(leftmost_green(&moved.pixels) > leftmost_green(&image.pixels));
        let behind = VisualPlan {
            tracks: vec![vec![title], vec![source(2, blue.clone())]],
            resolution: (160, 90),
        };
        let image = Compositor::new(behind, 160, 90, (30, 1))
            .frame(0, &cancelled)
            .unwrap()
            .unwrap();
        assert!(
            !image
                .pixels
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[1] > 150)
        );
        fs::remove_file(red).unwrap();
        fs::remove_file(blue).unwrap();
        fs::remove_dir(dir).unwrap();
    }
}
