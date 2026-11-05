use super::preview::{FrameImage, FrameSource, VideoLayers, blend};
use crate::project::Project;
use crate::timeline::{ClipContent, Title, TrackKind};
use crate::ui::title_markup;
use creamui::fonts::{FontWeight, HorizontalAlign, LayoutSettings, cached_layout, resolve};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use swash::scale::{Render, ScaleContext, Source};
use swash::zeno::Format;

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
        let tracks = project
            .tracks()
            .iter()
            .filter(|track| track.kind == TrackKind::Video)
            .map(|track| {
                track
                    .clips
                    .iter()
                    .filter_map(|clip| match &clip.content {
                        ClipContent::Media { media_id } => {
                            let media =
                                project.media().iter().find(|media| media.id == *media_id)?;
                            Some(VisualClip::Media(FrameSource {
                                clip_id: clip.id,
                                path: media.path.clone(),
                                kind: media.kind,
                                start: clip.start_frame,
                                source_start: clip.source_start_frame,
                                duration: clip.duration_frames,
                            }))
                        }
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
        for pixel in pixels.chunks_exact_mut(4) {
            pixel.copy_from_slice(&[0, 0, 0, 255]);
        }
        let mut active = Vec::new();
        let mut visible = false;
        for track in &self.plan.tracks {
            for clip in track {
                match clip {
                    VisualClip::Media(source)
                        if source.start <= position
                            && position - source.start < source.duration =>
                    {
                        visible = true;
                        active.push(source.clip_id);
                        let image = self.layers.layer_at(source, position, cancelled)?;
                        for (dest, src) in
                            pixels.chunks_exact_mut(4).zip(image.pixels.chunks_exact(4))
                        {
                            blend(dest, src);
                        }
                    }
                    VisualClip::Title {
                        clip_id: _,
                        start,
                        duration,
                        title,
                    } if *start <= position && position - *start < *duration => {
                        visible = true;
                        self.glyphs.draw(
                            title,
                            self.width,
                            self.height,
                            self.plan.resolution.0,
                            self.width as usize * 4,
                            &mut pixels,
                        );
                    }
                    _ => {}
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
    cache: HashMap<(u64, u16, u32), GlyphMask>,
}

struct GlyphMask {
    width: u32,
    height: u32,
    left: i32,
    top: i32,
    coverage: Vec<u8>,
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
        for (line_index, line) in lines.iter().enumerate() {
            let layouts: Vec<_> = line
                .iter()
                .map(|run| {
                    let face = resolve(
                        &title.font_family,
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
                    let key = (face.id(), glyph.id, size.to_bits());
                    if self.cache.len() >= 4096 && !self.cache.contains_key(&key) {
                        self.cache.clear();
                    }
                    let mask = self.cache.entry(key).or_insert_with(|| {
                        let mut scaler = self
                            .scaler
                            .builder(face.font_ref())
                            .size(size)
                            .hint(false)
                            .build();
                        let image = Render::new(&[Source::Outline])
                            .format(Format::Alpha)
                            .render(&mut scaler, glyph.id);
                        match image {
                            Some(image) => GlyphMask {
                                width: image.placement.width,
                                height: image.placement.height,
                                left: image.placement.left,
                                top: image.placement.top,
                                coverage: image.data,
                            },
                            None => GlyphMask {
                                width: 0,
                                height: 0,
                                left: 0,
                                top: 0,
                                coverage: Vec::new(),
                            },
                        }
                    });
                    let gx = (x + glyph.x).round() as i32 + mask.left;
                    let gy = (start_y + line_index as f32 * line_height + glyph.y).round() as i32
                        - mask.top;
                    if title.outline {
                        for (dx, dy) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
                            paint(
                                mask,
                                gx + dx,
                                gy + dy,
                                &mut canvas,
                                [0, 0, 0, run.color.a],
                                run.italic,
                            );
                        }
                    }
                    paint(
                        mask,
                        gx,
                        gy,
                        &mut canvas,
                        [run.color.r, run.color.g, run.color.b, run.color.a],
                        run.italic,
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

fn paint(mask: &GlyphMask, x: i32, y: i32, canvas: &mut Canvas<'_>, color: [u8; 4], italic: bool) {
    for row in 0..mask.height {
        let py = y + row as i32;
        if py < 0 || py >= canvas.height as i32 {
            continue;
        }
        for col in 0..mask.width {
            let shear = if italic {
                ((mask.height.saturating_sub(1 + row)) as f32 * 0.22).round() as i32
            } else {
                0
            };
            let px = x + col as i32 + shear;
            if px < 0 || px >= canvas.width as i32 {
                continue;
            }
            let coverage = u32::from(mask.coverage[(row * mask.width + col) as usize]);
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
    fn translucent_video_pixel_blends_over_lower_layer() {
        let mut background = [20, 40, 80, 255];
        crate::media::preview::blend(&mut background, &[220, 120, 0, 128]);
        assert_eq!(background, [120, 80, 40, 255]);
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
        assert!(image.pixels[2] > 180 && image.pixels[0] < 60);
        assert!(
            image
                .pixels
                .chunks_exact(4)
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
                .chunks_exact(4)
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
                .chunks_exact(4)
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
        assert!(!image.pixels.chunks_exact(4).any(|pixel| pixel[1] > 150));
        fs::remove_file(red).unwrap();
        fs::remove_file(blue).unwrap();
        fs::remove_dir(dir).unwrap();
    }
}
