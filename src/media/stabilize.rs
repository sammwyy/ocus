use super::preview::{FrameImage, FrameSource, VideoLayers};
use crate::timeline::{Playback, Stabilization, Transform};
use std::sync::{Arc, atomic::AtomicBool};

pub fn analyze(mut source: FrameSource, fps: (u32, u32)) -> Result<Stabilization, String> {
    if fps.0 == 0
        || fps.1 == 0
        || source.kind != super::MediaKind::Video
        || source.source_start.checked_add(source.duration).is_none()
    {
        return Err("Invalid video analysis settings".into());
    }
    if source.duration == 0 || source.duration > 216_000 {
        return Err("Choose a range between 1 and 216,000 source frames".into());
    }
    let start_frame = source.source_start;
    source.start = 0;
    source.speed = 1.0;
    source.playback = Playback::default();
    let mut layers = VideoLayers::new(fps, (320, 320));
    let cancelled = Arc::new(AtomicBool::new(false));
    let mut previous: Option<Vec<u8>> = None;
    let mut motion = Vec::with_capacity(source.duration as usize);
    let mut path = [0.0; 2];
    for frame in 0..source.duration {
        let image = layers.layer_at(&source, frame, &cancelled)?;
        let (left, top, width, height) = image.picture;
        let stride = image.width;
        let gray: Vec<u8> = (top..top + height)
            .flat_map(|y| (left..left + width).map(move |x| ((y * stride + x) * 4) as usize))
            .map(|offset| &image.pixels[offset..offset + 4])
            .map(|p| {
                ((u32::from(p[0]) * 77 + u32::from(p[1]) * 150 + u32::from(p[2]) * 29) >> 8) as u8
            })
            .collect();
        if let Some(previous) = previous.as_ref() {
            let shift = estimate(previous, &gray, width as usize, height as usize);
            path[0] += shift[0] / image.picture.2.max(1) as f32;
            path[1] += shift[1] / image.picture.3.max(1) as f32;
        }
        motion.push(path);
        previous = Some(gray);
    }
    Ok(Stabilization::new(start_frame, motion))
}

fn estimate(previous: &[u8], current: &[u8], width: usize, height: usize) -> [f32; 2] {
    if width < 24 || height < 24 {
        return [0.0, 0.0];
    }
    let mut pyramid = vec![(previous.to_vec(), current.to_vec(), width, height)];
    while pyramid.last().unwrap().2 > 96 && pyramid.last().unwrap().3 > 48 {
        let (previous, current, width, height) = pyramid.last().unwrap();
        let reduce = |image: &[u8]| {
            (0..height / 2)
                .flat_map(|y| {
                    (0..width / 2).map(move |x| {
                        let i = y * 2 * width + x * 2;
                        ((u32::from(image[i])
                            + u32::from(image[i + 1])
                            + u32::from(image[i + width])
                            + u32::from(image[i + width + 1]))
                            / 4) as u8
                    })
                })
                .collect::<Vec<_>>()
        };
        pyramid.push((reduce(previous), reduce(current), width / 2, height / 2));
    }
    let mut guides = vec![[0i32; 2]];
    let mut guide = [0i32; 2];
    for (level, (previous, current, width, height)) in pyramid.iter().rev().enumerate() {
        let search = if level == 0 {
            (width.min(height) / 4).min(16) as i32
        } else {
            3
        };
        if level != 0 {
            for guide in &mut guides {
                *guide = guide.map(|v| v * 2);
            }
        }
        let margin = guides
            .iter()
            .flatten()
            .map(|v| v.unsigned_abs() as usize)
            .max()
            .unwrap()
            + search as usize
            + 1;
        if *width <= margin * 2 || *height <= margin * 2 {
            return [0.0, 0.0];
        }
        let stride = (width / 80).max(1);
        let mut scores = Vec::new();
        let mut candidates = std::collections::BTreeSet::new();
        for guide in &guides {
            for dy in guide[1] - search..=guide[1] + search {
                for dx in guide[0] - search..=guide[0] + search {
                    candidates.insert((dx, dy));
                }
            }
        }
        for (dx, dy) in candidates {
            let mut score = 0u64;
            for y in (margin..height - margin).step_by(stride) {
                for x in (margin..width - margin).step_by(stride) {
                    let difference = i32::from(previous[y * width + x])
                        - i32::from(
                            current[(y as i32 + dy) as usize * width + (x as i32 + dx) as usize],
                        );
                    score += (difference * difference) as u64;
                }
            }
            scores.push((score, dx, dy));
        }
        scores.sort_by_key(|&(score, dx, dy)| (score, dx.abs() + dy.abs()));
        guides.clear();
        for (_, dx, dy) in scores {
            if guides
                .iter()
                .all(|p| (p[0] - dx).abs() > 1 || (p[1] - dy).abs() > 1)
            {
                guides.push([dx, dy]);
                if guides.len() == 16 {
                    break;
                }
            }
        }
        if level + 1 == pyramid.len() {
            let mut refined = Vec::new();
            for candidate in &guides {
                let mut best = f64::INFINITY;
                for oy in [-0.5, 0.0, 0.5] {
                    for ox in [-0.5, 0.0, 0.5] {
                        let dx = candidate[0] as f64 + ox;
                        let dy = candidate[1] as f64 + oy;
                        let mut score = 0.0;
                        for y in (margin..height - margin).step_by(stride) {
                            for x in (margin..width - margin).step_by(stride) {
                                let sx = x as f64 + dx;
                                let sy = y as f64 + dy;
                                let (ix, iy) = (sx.floor() as usize, sy.floor() as usize);
                                let (fx, fy) = (sx - ix as f64, sy - iy as f64);
                                let i = iy * width + ix;
                                let sample = f64::from(current[i]) * (1.0 - fx) * (1.0 - fy)
                                    + f64::from(current[i + 1]) * fx * (1.0 - fy)
                                    + f64::from(current[i + width]) * (1.0 - fx) * fy
                                    + f64::from(current[i + width + 1]) * fx * fy;
                                score += (f64::from(previous[y * width + x]) - sample).powi(2);
                            }
                        }
                        best = best.min(score);
                    }
                }
                refined.push((best, *candidate));
            }
            refined.sort_by(|a, b| a.0.total_cmp(&b.0));
            if refined.len() < 2 || refined[0].0 >= refined[1].0 * 0.95 {
                return [0.0, 0.0];
            }
            guide = refined[0].1;
        }
    }
    let block = (width.min(height) / 16).clamp(2, 8);
    let search = 3;
    let margin = block
        + guide
            .iter()
            .map(|v| v.unsigned_abs() as usize)
            .max()
            .unwrap()
        + search as usize
        + 1;
    if width <= margin * 2 || height <= margin * 2 {
        return [0.0, 0.0];
    }
    let mut shifts = Vec::new();
    for row in 0..4 {
        let cy = margin + (height - 1 - margin * 2) * row / 3;
        for column in 0..5 {
            let cx = margin + (width - 1 - margin * 2) * column / 4;
            let variance = |image: &[u8], cx: usize, cy: usize| {
                let mut sum = 0.0;
                let mut squared = 0.0;
                let mut count = 0.0;
                for y in (cy - block..cy + block).step_by(2) {
                    for x in (cx - block..cx + block).step_by(2) {
                        let value = f64::from(image[y * width + x]);
                        sum += value;
                        squared += value * value;
                        count += 1.0;
                    }
                }
                squared / count - (sum / count).powi(2)
            };
            if variance(previous, cx, cy) < 36.0 {
                continue;
            }
            let error = |dx: i32, dy: i32| {
                let mut error = 0u32;
                for y in (cy - block..cy + block).step_by(2) {
                    for x in (cx - block..cx + block).step_by(2) {
                        let other = (y as i32 + dy) as usize * width + (x as i32 + dx) as usize;
                        let difference =
                            i32::from(previous[y * width + x]) - i32::from(current[other]);
                        error += (difference * difference) as u32;
                    }
                }
                f64::from(error)
            };
            let mut best = (f64::INFINITY, 0i32, 0i32);
            let mut scores = Vec::new();
            for dy in guide[1] - search..=guide[1] + search {
                for dx in guide[0] - search..=guide[0] + search {
                    let score = error(dx, dy);
                    scores.push((score, dx, dy));
                    if (score, dx.abs() + dy.abs()) < (best.0, best.1.abs() + best.2.abs()) {
                        best = (score, dx, dy);
                    }
                }
            }
            let alternative = scores
                .iter()
                .filter(|s| (s.1 - best.1).abs() > 1 || (s.2 - best.2).abs() > 1)
                .map(|s| s.0)
                .fold(f64::INFINITY, f64::min);
            if best.0 >= alternative * 0.9
                || (best.1 - guide[0]).abs() == search
                || (best.2 - guide[1]).abs() == search
                || variance(
                    current,
                    (cx as i32 + best.1) as usize,
                    (cy as i32 + best.2) as usize,
                ) < 36.0
            {
                continue;
            }
            let refine = |left: f64, right: f64| {
                let curvature = left - 2.0 * best.0 + right;
                if curvature > 0.0 {
                    (0.5 * (left - right) / curvature).clamp(-0.5, 0.5) as f32
                } else {
                    0.0
                }
            };
            shifts.push([
                best.1 as f32 + refine(error(best.1 - 1, best.2), error(best.1 + 1, best.2)),
                best.2 as f32 + refine(error(best.1, best.2 - 1), error(best.1, best.2 + 1)),
            ]);
        }
    }
    if shifts.len() < 3 {
        return [0.0, 0.0];
    }
    let median = |shifts: &mut Vec<[f32; 2]>| {
        std::array::from_fn(|axis| {
            shifts.sort_by(|a, b| a[axis].total_cmp(&b[axis]));
            let middle = shifts.len() / 2;
            (shifts[(shifts.len() - 1) / 2][axis] + shifts[middle][axis]) / 2.0
        })
    };
    let center = median(&mut shifts);
    let valid = shifts.len();
    shifts.retain(|p| (p[0] - center[0]).abs() < 1.0 && (p[1] - center[1]).abs() < 1.0);
    if shifts.len() < 3 || shifts.len() * 2 <= valid {
        return [0.0, 0.0];
    }
    median(&mut shifts)
}

pub fn corrections(data: &Stabilization) -> Vec<Transform> {
    let radius = (data.strength * 45.0).round() as usize;
    let data = &data.analysis;
    let mut prefix = vec![[0.0f64; 2]; data.motion.len() + 1];
    for (i, point) in data.motion.iter().enumerate() {
        prefix[i + 1] = std::array::from_fn(|axis| prefix[i][axis] + f64::from(point[axis]));
    }
    let offsets: Vec<[f32; 2]> = data
        .motion
        .iter()
        .enumerate()
        .map(|(i, point)| {
            let start = i.saturating_sub(radius);
            let end = (i + radius + 1).min(data.motion.len());
            std::array::from_fn(|axis| {
                ((prefix[end][axis] - prefix[start][axis]) / (end - start) as f64) as f32
                    - point[axis]
            })
        })
        .collect();
    let maximum = offsets.iter().flatten().fold(0.0f32, |a, b| a.max(b.abs()));
    let scale = (1.0 / (1.0 - 2.0 * maximum.min(0.1))).min(1.25);
    let coverage = (scale - 1.0) / 2.0;
    offsets
        .into_iter()
        .map(|p| Transform {
            scale,
            x: (p[0] * scale).clamp(-coverage, coverage),
            y: (p[1] * scale).clamp(-coverage, coverage),
            ..Transform::default()
        })
        .collect()
}

pub(crate) fn apply(image: Arc<FrameImage>, correction: Transform) -> Arc<FrameImage> {
    if correction.is_identity() {
        return image;
    }
    let mut pixels = image.pixels.clone();
    let (left, top, width, height) = image.picture;
    let center_x = left as f32 + width as f32 / 2.0;
    let center_y = top as f32 + height as f32 / 2.0;
    for y in top..top + height {
        for x in left..left + width {
            let sx = ((x as f32 + 0.5 - center_x - correction.x * width as f32) / correction.scale
                + center_x
                - 0.5)
                .clamp(left as f32, (left + width - 1) as f32);
            let sy = ((y as f32 + 0.5 - center_y - correction.y * height as f32)
                / correction.scale
                + center_y
                - 0.5)
                .clamp(top as f32, (top + height - 1) as f32);
            let x0 = sx as u32;
            let y0 = sy as u32;
            let fx = sx - x0 as f32;
            let fy = sy - y0 as f32;
            let mut sample = [0.0; 4];
            for (px, py, weight) in [
                (x0, y0, (1.0 - fx) * (1.0 - fy)),
                ((x0 + 1).min(left + width - 1), y0, fx * (1.0 - fy)),
                (x0, (y0 + 1).min(top + height - 1), (1.0 - fx) * fy),
                (
                    (x0 + 1).min(left + width - 1),
                    (y0 + 1).min(top + height - 1),
                    fx * fy,
                ),
            ] {
                let offset = ((py * image.width + px) * 4) as usize;
                let pixel = &image.pixels[offset..offset + 4];
                let alpha = f32::from(pixel[3]) / 255.0;
                for channel in 0..3 {
                    sample[channel] += f32::from(pixel[channel]) * alpha * weight;
                }
                sample[3] += f32::from(pixel[3]) * weight;
            }
            let offset = ((y * image.width + x) * 4) as usize;
            for channel in 0..3 {
                pixels[offset + channel] = if sample[3] > 0.0 {
                    (sample[channel] * 255.0 / sample[3]).round() as u8
                } else {
                    0
                };
            }
            pixels[offset + 3] = sample[3].round() as u8;
        }
    }
    Arc::new(FrameImage {
        pixels,
        picture: image.picture,
        width: image.width,
        height: image.height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::composite::{Compositor, VisualPlan};
    use crate::media::{Media, MediaKind};
    use crate::project::{Command, Editor, Project, format};
    use crate::timeline::{Clip, Transition, TransitionKind};
    use std::io::Write;
    use std::path::Path;

    fn source(path: &Path, duration: u64) -> FrameSource {
        FrameSource {
            chroma_key: None,
            stabilization: None,
            effect: None,
            mask: None,
            playback: Playback::Forward,
            lut: None,
            color: Default::default(),
            clip_id: 1,
            path: path.into(),
            kind: MediaKind::Video,
            start: 0,
            source_start: 0,
            duration,
            speed: 1.0,
            transform: Transform::default(),
            keyframes: Vec::new(),
        }
    }

    fn editor(path: &Path, duration: u64) -> Editor {
        let mut editor = Editor::new(Project::new("Stabilization"));
        assert!(editor.apply(Command::ImportMedia(Media {
            id: 1,
            path: path.into(),
            kind: MediaKind::Video,
            duration_us: Some(10_000_000),
            has_audio: false,
        })));
        let clip: Clip = serde_json::from_value(serde_json::json!({
            "id":1, "start_frame":0, "source_start_frame":0, "duration_frames":duration,
            "content":{"media":{"media_id":1}}, "volume":1.0,"muted":false
        }))
        .unwrap();
        assert!(clip.stabilization.is_none());
        assert!(editor.apply(Command::InsertClip {
            track_id: 0,
            clip: Box::new(clip)
        }));
        editor
    }

    #[test]
    fn recovers_known_translation_and_ignores_flat_frames() {
        let (width, height) = (192, 108);
        let previous: Vec<u8> = (0..width * height)
            .map(|i| {
                let (x, y) = (i % width, i / width);
                ((x * 73 + y * 151 + x * y * 17) % 251) as u8
            })
            .collect();
        for (dx, dy) in [(4, -3), (-7, 5), (0, 0)] {
            let mut current = vec![0; previous.len()];
            for y in 0..height as i32 {
                for x in 0..width as i32 {
                    if (0..width as i32).contains(&(x + dx))
                        && (0..height as i32).contains(&(y + dy))
                    {
                        current[((y + dy) as usize * width) + (x + dx) as usize] =
                            previous[y as usize * width + x as usize];
                    }
                }
            }
            let found = estimate(&previous, &current, width, height);
            assert!((found[0] - dx as f32).abs() <= 0.5, "{found:?}");
            assert!((found[1] - dy as f32).abs() <= 0.5, "{found:?}");
        }
        assert_eq!(
            estimate(
                &vec![80; width * height],
                &vec![80; width * height],
                width,
                height
            ),
            [0.0, 0.0]
        );
    }

    #[test]
    fn wide_motion_and_ambiguous_patterns() {
        let (width, height) = (320, 180);
        let mut seed = 71u32;
        let previous: Vec<u8> = (0..width * height)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed as u8
            })
            .collect();
        let shifted = |image: &[u8], dx: i32, dy: i32| {
            let mut current = vec![0; image.len()];
            for y in 0..height as i32 {
                for x in 0..width as i32 {
                    if (0..width as i32).contains(&(x + dx))
                        && (0..height as i32).contains(&(y + dy))
                    {
                        current[(y + dy) as usize * width + (x + dx) as usize] =
                            image[y as usize * width + x as usize];
                    }
                }
            }
            current
        };
        for (dx, dy) in [(29, -22), (-35, 18)] {
            let found = estimate(&previous, &shifted(&previous, dx, dy), width, height);
            assert!(
                (found[0] - dx as f32).abs() < 0.15 && (found[1] - dy as f32).abs() < 0.15,
                "{dx},{dy} -> {found:?}"
            );
        }
        for one_dimensional in [false, true] {
            let image: Vec<u8> = (0..width * height)
                .map(|i| {
                    let (x, y) = (i % width, i / width);
                    if one_dimensional {
                        (120.0 + 70.0 * (x as f32 * 0.37).sin()) as u8
                    } else {
                        50 + (((x / 4) ^ (y / 4)) % 2) as u8 * 100
                    }
                })
                .collect();
            assert_eq!(
                estimate(&image, &shifted(&image, 3, 2), width, height),
                [0.0, 0.0]
            );
        }
    }

    #[test]
    fn recovers_subpixel_translation() {
        let (width, height) = (320, 180);
        let image = |dx: f32, dy: f32| {
            (0..width * height)
                .map(|i| {
                    let x = (i % width) as f32 - dx;
                    let y = (i / width) as f32 - dy;
                    (120.0
                        + 35.0 * (x * 0.45).sin()
                        + 28.0 * (y * 0.39).cos()
                        + 20.0 * (x * 0.17 + y * 0.23).sin())
                    .round() as u8
                })
                .collect::<Vec<_>>()
        };
        let previous = image(0.0, 0.0);
        for expected in [[0.5, 0.0], [-0.5, 0.5], [2.4, -1.6]] {
            let found = estimate(&previous, &image(expected[0], expected[1]), width, height);
            for axis in 0..2 {
                assert!(
                    (found[axis] - expected[axis]).abs() < 0.15,
                    "{expected:?} -> {found:?}"
                );
            }
        }
    }

    #[test]
    fn textured_region_can_track_a_mostly_flat_frame() {
        let (width, height) = (320, 180);
        let image = |dx: i32, dy: i32, textured_width: i32| {
            (0..width * height)
                .map(|i| {
                    let x = (i % width) as i32 - dx;
                    let y = (i / width) as i32 - dy;
                    if (80..80 + textured_width).contains(&x) && (45..135).contains(&y) {
                        ((x * 73 + y * 151 + x * y * 17) % 251) as u8
                    } else {
                        80
                    }
                })
                .collect::<Vec<_>>()
        };
        let found = estimate(&image(0, 0, 160), &image(3, -2, 160), width, height);
        assert!(
            (found[0] - 3.0).abs() < 0.15 && (found[1] + 2.0).abs() < 0.15,
            "{found:?}"
        );
        assert_eq!(
            estimate(&image(0, 0, 8), &image(3, -2, 8), width, height),
            [0.0, 0.0]
        );
    }

    #[test]
    fn smoothing_zoom_limits_and_source_mapping() {
        let mut data = Stabilization::new(20, vec![[0.0, 0.0], [0.04, -0.02], [0.0, 0.0]]);
        data.strength = 1.0;
        let values = corrections(&data);
        assert!(values[1].x < 0.0 && values[1].y > 0.0);
        assert!(values.iter().all(|t| t.scale == values[0].scale));
        assert!(values[0].scale > 1.0 && values[0].scale <= 1.25);
        assert!(
            values
                .iter()
                .all(|t| t.x.abs().max(t.y.abs()) <= (t.scale - 1.0) / 2.0)
        );
        let mut src = source(Path::new("unused"), 3);
        src.source_start = 20;
        src.stabilization = Some((20, Arc::new(values.clone())));
        assert_eq!(src.stabilization_at(1), values[1]);
        src.playback = Playback::Reverse;
        assert_eq!(src.stabilization_at(0), values[2]);
        src.playback = Playback::Freeze;
        assert_eq!(src.stabilization_at(2), values[0]);
        src.playback = Playback::Forward;
        src.speed = 2.0;
        assert_eq!(src.stabilization_at(1), values[2]);
        assert!(src.stabilization_at(10).is_identity());
        data.strength = 0.0;
        assert!(corrections(&data).iter().all(Transform::is_identity));
        data.strength = 1.0;
        data.analysis.motion[1] = [10.0, -10.0];
        assert!(
            corrections(&data)
                .iter()
                .all(|t| t.scale <= 1.25 && t.x.abs().max(t.y.abs()) <= (t.scale - 1.0) / 2.0)
        );
        data.analysis.motion.clear();
        assert!(!data.valid());
        assert!(corrections(&data).is_empty());
    }

    #[test]
    fn project_compatibility_validation_undo_redo_and_split() {
        let mut editor = editor(Path::new("shake.mkv"), 60);
        let legacy = format::to_json(editor.project()).unwrap();
        assert!(!legacy.contains("stabilization"));
        assert_eq!(format::from_json(&legacy).unwrap(), *editor.project());
        let data = Stabilization::new(0, vec![[0.0, 0.0], [0.02, -0.01]]);
        let command = |stabilization| Command::SetClipStabilization {
            track_id: 0,
            clip_id: 1,
            stabilization,
        };
        assert!(editor.apply(command(Some(data.clone()))));
        assert!(editor.undo());
        assert!(
            editor.project().tracks()[0].clips[0]
                .stabilization
                .is_none()
        );
        assert!(editor.redo());
        let json = format::to_json(editor.project()).unwrap();
        let mut expected = serde_json::to_value(editor.project()).unwrap();
        expected["tracks"][0]["clips"][0]["effect_stack"] = serde_json::json!(["stabilization"]);
        assert_eq!(
            serde_json::to_value(format::from_json(&json).unwrap()).unwrap(),
            expected
        );
        for invalid in [
            Stabilization {
                strength: f32::NAN,
                ..data.clone()
            },
            Stabilization::new(0, vec![]),
            Stabilization::new(0, vec![[f32::INFINITY, 0.0]]),
        ] {
            assert!(!editor.apply(command(Some(invalid))));
        }
        assert!(editor.apply(Command::SetTrackLocked {
            track_id: 0,
            locked: true
        }));
        assert!(!editor.apply(command(None)));
        assert!(editor.undo());
        assert!(editor.apply(Command::Split {
            track_id: 0,
            clip_id: 1,
            at_frame: 30,
            new_id: 2
        }));
        assert!(
            editor.project().tracks()[0]
                .clips
                .iter()
                .all(|c| c.stabilization.as_ref() == Some(&data))
        );
        assert!(editor.apply(command(None)));
        assert!(editor.undo());
        assert_eq!(
            editor.project().tracks()[0].clips[0].stabilization,
            Some(data)
        );
    }

    #[test]
    fn correction_moves_pixels_and_preserves_alpha_and_letterboxing() {
        let mut image = FrameImage {
            width: 10,
            height: 6,
            picture: (1, 1, 8, 4),
            pixels: vec![0; 10 * 6 * 4],
        };
        let original = (2 * 10 + 3) * 4;
        image.pixels[original..original + 4].copy_from_slice(&[180, 80, 20, 128]);
        let image = Arc::new(image);
        assert!(Arc::ptr_eq(
            &image,
            &apply(image.clone(), Transform::default())
        ));
        let corrected = apply(
            image,
            Transform {
                x: 0.25,
                ..Default::default()
            },
        );
        let shifted = (2 * 10 + 5) * 4;
        assert_eq!(&corrected.pixels[shifted..shifted + 4], &[180, 80, 20, 128]);
        assert_eq!(&corrected.pixels[original..original + 4], &[0, 0, 0, 0]);
        assert!(corrected.pixels[..40].iter().all(|v| *v == 0));
    }

    fn centroid(image: &FrameImage) -> [f64; 2] {
        let mut total = [0.0; 3];
        for (i, pixel) in image.pixels.as_chunks::<4>().0.iter().enumerate() {
            let weight = (f64::from(pixel[0].min(pixel[1]).min(pixel[2])) - 200.0).max(0.0);
            if weight > 0.0 {
                total[0] += (i % image.width as usize) as f64 * weight;
                total[1] += (i / image.width as usize) as f64 * weight;
                total[2] += weight;
            }
        }
        assert!(total[2] > 10.0, "Tracking marker missing");
        [total[0] / total[2], total[1] / total[2]]
    }

    #[test]
    fn real_shaky_video_compositor_reduces_tracked_jitter() {
        check_shaky_fixture(
            "Integer crop jitter",
            "nullsrc=s=224x140:r=30,geq=lum='mod(X*73+Y*151+X*Y*17,180)':cb=128:cr=128,drawbox=x=104:y=64:w=10:h=10:color=white:t=fill,crop=192:108:x='16+4*sin(n*2)':y='16+3*cos(n*1.7)':exact=1",
            (192, 108),
        );
    }

    #[test]
    fn fractional_shaky_video_compositor_reduces_tracked_jitter() {
        check_shaky_fixture(
            "Fractional bilinear jitter (4.5 px X / 3.25 px Y)",
            "nullsrc=s=352x212:r=30,geq=lum='75+20*sin(X*.37)+20*cos(Y*.41)+15*sin(X*.21+Y*.17)+15*cos(X*.11-Y*.29)':cb=128:cr=128,drawbox=x=168:y=100:w=14:h=14:color=white:t=fill,geq=lum='lum(X+4.5*sin(N*2),Y+3.25*cos(N*1.7))':cb=128:cr=128:interpolation=bilinear,crop=320:180:16:16",
            (320, 180),
        );
    }

    fn check_shaky_fixture(label: &str, filter: &str, output_size: (u32, u32)) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shake.mkv");
        let output = std::process::Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                filter,
                "-frames:v",
                "60",
                "-c:v",
                "ffv1",
                "-pix_fmt",
                "yuv444p",
                "-y",
            ])
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut data = analyze(source(&path, 60), (30, 1)).unwrap();
        data.strength = 0.8;
        assert_eq!(data.analysis.motion.len(), 60);
        let mut editor = editor(&path, 60);
        let cancelled = Arc::new(AtomicBool::new(false));
        let measure = |editor: &Editor| {
            let plan = VisualPlan::from_project(editor.project());
            let mut compositor = Compositor::new(plan, output_size.0, output_size.1, (30, 1));
            (0..60)
                .map(|i| centroid(&compositor.frame(i, &cancelled).unwrap().unwrap()))
                .collect::<Vec<_>>()
        };
        let before = measure(&editor);
        assert!(editor.apply(Command::SetClipStabilization {
            track_id: 0,
            clip_id: 1,
            stabilization: Some(data)
        }));
        let after = measure(&editor);
        let rms = |points: &[[f64; 2]], axis: usize| {
            let mean = points.iter().map(|p| p[axis]).sum::<f64>() / points.len() as f64;
            (points.iter().map(|p| (p[axis] - mean).powi(2)).sum::<f64>() / points.len() as f64)
                .sqrt()
        };
        let mut table = format!(
            "\n{label}: tracked feature RMS jitter (pixels, 60 decoded/composited frames)\nAxis | Before | Stabilized | Reduction\n"
        );
        for axis in 0..2 {
            let (before, after) = (rms(&before, axis), rms(&after, axis));
            table.push_str(&format!(
                "{} | {before:.3} | {after:.3} | {:.1}%\n",
                ["X", "Y"][axis],
                (1.0 - after / before) * 100.0
            ));
            assert!(
                after < before * 0.35,
                "Insufficient jitter reduction on axis {axis}: {before} -> {after}"
            );
        }
        println!("{table}");
        let render = |editor: &Editor| {
            Compositor::new(
                VisualPlan::from_project(editor.project()),
                output_size.0,
                output_size.1,
                (30, 1),
            )
            .frame(20, &cancelled)
            .unwrap()
            .unwrap()
        };
        let normal = render(&editor);
        assert!(editor.apply(Command::SetTransition {
            track_id: 0,
            clip_id: 1,
            transition: Some(Transition {
                kind: TransitionKind::Dissolve,
                duration_frames: 30,
                direction: Default::default()
            })
        }));
        let transition = render(&editor);
        for (normal, transition) in normal
            .pixels
            .as_chunks::<4>()
            .0
            .iter()
            .zip(transition.pixels.as_chunks::<4>().0)
        {
            for channel in 0..3 {
                assert!(
                    (f64::from(transition[channel]) - f64::from(normal[channel]) * 21.0 / 31.0)
                        .abs()
                        <= 1.0
                );
            }
        }
        let mut missing = source(&dir.path().join("missing.mkv"), 2);
        assert!(analyze(missing.clone(), (30, 1)).is_err());
        missing.duration = 0;
        assert!(analyze(missing, (30, 1)).is_err());
    }

    #[test]
    fn repetitive_grid_does_not_introduce_drift() {
        let dir = tempfile::tempdir().unwrap();
        let generated = dir.path().join("shaky.mp4");
        let path = if let Some(path) = std::env::var_os("OCUS_STABILIZATION_FIXTURE") {
            std::path::PathBuf::from(path)
        } else {
            let texture = dir.path().join("texture.png");
            let result = std::process::Command::new("ffmpeg").args([
                "-v", "error", "-f", "lavfi", "-i",
                "testsrc2=size=1408x848:rate=30,drawgrid=w=53:h=47:t=2:c=white@0.6,drawbox=x=604:y=324:w=200:h=200:color=black:t=fill,drawbox=x=664:y=384:w=80:h=80:color=magenta:t=fill",
                "-frames:v", "1",
            ]).arg(&texture).output().unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            let result = std::process::Command::new("ffmpeg").args(["-v", "error", "-loop", "1", "-i"]).arg(&texture).args([
                "-vf", "crop=1280:720:x='64+24*sin(n*1.7)+12*sin(n*0.71)':y='64+18*sin(n*1.31)+8*cos(n*0.83)'",
                "-t", "8", "-r", "30", "-c:v", "libx264", "-crf", "14", "-pix_fmt", "yuv420p",
            ]).arg(&generated).output().unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            generated
        };
        let mut data = analyze(source(&path, 240), (30, 1)).unwrap();
        data.strength = 0.86;
        let mut editor = editor(&path, 240);
        let cancelled = Arc::new(AtomicBool::new(false));
        let measure = |editor: &Editor| {
            let mut renderer = Compositor::new(
                VisualPlan::from_project(editor.project()),
                320,
                180,
                (30, 1),
            );
            (0..240)
                .map(|i| {
                    let image = renderer.frame(i, &cancelled).unwrap().unwrap();
                    let mut sum = [0.0; 3];
                    for y in 60..120 {
                        for x in 130..190 {
                            let offset = (y * 320 + x) * 4;
                            let p = &image.pixels[offset..offset + 4];
                            if p[0] > 180 && p[1] < 80 && p[2] > 180 {
                                sum[0] += x as f64 * 4.0;
                                sum[1] += y as f64 * 4.0;
                                sum[2] += 1.0;
                            }
                        }
                    }
                    assert!(sum[2] > 30.0, "Marker missing at {i}");
                    [sum[0] / sum[2], sum[1] / sum[2]]
                })
                .collect::<Vec<_>>()
        };
        let before = measure(&editor);
        assert!(editor.apply(Command::SetClipStabilization {
            track_id: 0,
            clip_id: 1,
            stabilization: Some(data)
        }));
        let after = measure(&editor);
        let metrics = |points: &[[f64; 2]], axis: usize| {
            let mean = points.iter().map(|p| p[axis]).sum::<f64>() / points.len() as f64;
            let stddev = (points.iter().map(|p| (p[axis] - mean).powi(2)).sum::<f64>()
                / points.len() as f64)
                .sqrt();
            let rms = (points
                .windows(2)
                .map(|p| (p[1][axis] - p[0][axis]).powi(2))
                .sum::<f64>()
                / (points.len() - 1) as f64)
                .sqrt();
            [stddev, rms]
        };
        let comparisons: Vec<_> = (0..2)
            .map(|axis| (metrics(&before, axis), metrics(&after, axis)))
            .collect();
        println!("Repetitive grid / 240 frames / 86% strength / metrics in 720p pixels");
        for (axis, (before, after)) in comparisons.iter().enumerate() {
            println!(
                "{} | stddev {:.3} -> {:.3} | frame RMS {:.3} -> {:.3}",
                ["X", "Y"][axis],
                before[0],
                after[0],
                before[1],
                after[1]
            );
        }
        if let Some(output) = std::env::var_os("OCUS_STABILIZATION_OUTPUT") {
            let mut encoder = std::process::Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "rawvideo",
                    "-pixel_format",
                    "rgba",
                    "-video_size",
                    "1280x720",
                    "-framerate",
                    "30",
                    "-i",
                    "-",
                    "-c:v",
                    "libx264",
                    "-crf",
                    "14",
                    "-pix_fmt",
                    "yuv420p",
                    "-y",
                ])
                .arg(output)
                .stdin(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            let mut input = encoder.stdin.take().unwrap();
            let mut renderer = Compositor::new(
                VisualPlan::from_project(editor.project()),
                1280,
                720,
                (30, 1),
            );
            for i in 0..240 {
                input
                    .write_all(&renderer.frame(i, &cancelled).unwrap().unwrap().pixels)
                    .unwrap();
            }
            drop(input);
            assert!(encoder.wait().unwrap().success());
        }
        for (before, after) in comparisons {
            for metric in 0..2 {
                assert!(
                    after[metric] < before[metric] * 0.5,
                    "Repetitive-grid jitter: {before:?} -> {after:?}"
                );
            }
        }
    }

    #[test]
    #[ignore = "release timing with a generated ten-second 1080p fixture"]
    fn analysis_1080p_release_timing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("1080p.mp4");
        let output = std::process::Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=s=1920x1080:r=30:d=10",
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-threads",
                "2",
                "-y",
            ])
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        crate::media::hardware::set_enabled(false);
        let mut timings = Vec::new();
        for _ in 0..3 {
            let start = std::time::Instant::now();
            let data = analyze(source(&path, 300), (30, 1)).unwrap();
            assert_eq!(data.analysis.motion.len(), 300);
            timings.push(start.elapsed().as_secs_f64() * 1000.0 / 300.0);
        }
        timings.sort_by(f64::total_cmp);
        println!("1080p / 10 s / 300 frames / software decode + scale + analysis / release");
        println!(
            "ms/frame (3 runs) | {:.3} | {:.3} | {:.3} | median {:.3}",
            timings[0], timings[1], timings[2], timings[1]
        );
    }
}
