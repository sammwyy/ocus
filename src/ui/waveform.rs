use crate::media::audio::Waveform;
use crate::timeline::{Clip, Playback};
use creamui::Color;
use creamui::core::{Painter, Rect};
use std::sync::Arc;

pub(super) struct WaveformSlice {
    pub peaks: Arc<Waveform>,
    pub clip: Clip,
    pub fps: (u32, u32),
}

fn frame_at(x: f32, full: Rect, clip: &Clip) -> f64 {
    let fraction = ((f64::from(x) - f64::from(full.x)) / f64::from(full.width)).clamp(0.0, 1.0);
    let first = clip.source_frame_at(0).unwrap_or(0) as f64;
    match clip.playback {
        Playback::Forward => first + clip.duration_frames as f64 * fraction,
        Playback::Reverse => first + 1.0 - clip.duration_frames as f64 * fraction,
        Playback::Freeze => first,
    }
}

impl WaveformSlice {
    pub fn paint(
        &self,
        painter: &mut dyn Painter,
        full: Rect,
        visible: Rect,
        gain: f32,
        color: Color,
    ) {
        if full.width <= 0.0 || visible.width < 1.0 || visible.height < 2.0 {
            return;
        }
        let center = visible.y + visible.height * 0.5;
        let end = visible.x + visible.width;
        let mut x = visible.x;
        while x < end {
            let next = (x + 3.0).min(end);
            let from = frame_at(x, full, &self.clip);
            let until = frame_at(next, full, &self.clip);
            let peak = self.peaks.peak(from.min(until), from.max(until), self.fps);
            let height = (peak * gain).min(1.0) * visible.height;
            let height = height.max(1.0);
            painter.fill_rect(
                Rect {
                    x,
                    y: center - height * 0.5,
                    width: (next - x - 0.6).max(0.0),
                    height,
                },
                color,
                0.0,
            );
            x = next;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waveform_geometry_maps_trim_and_horizontal_scroll_to_the_same_source() {
        let mut clip: Clip = serde_json::from_value(serde_json::json!({
            "id": 1, "start_frame": 0, "source_start_frame": 30,
            "duration_frames": 60, "content": {"media": {"media_id": 1}},
            "volume": 1.0, "muted": false
        }))
        .unwrap();
        let full = Rect {
            x: -50.0,
            width: 200.0,
            ..Rect::default()
        };
        for (x, expected) in [(-50.0, 30.0), (0.0, 45.0), (100.0, 75.0), (150.0, 90.0)] {
            assert_eq!(frame_at(x, full, &clip), expected);
        }
        let zoomed = Rect {
            x: -100.0,
            width: 400.0,
            ..full
        };
        assert_eq!(frame_at(200.0, zoomed, &clip), 75.0);
        clip.playback = Playback::Reverse;
        for (x, expected) in [(-50.0, 90.0), (0.0, 75.0), (100.0, 45.0), (150.0, 30.0)] {
            assert_eq!(frame_at(x, full, &clip), expected);
        }
        assert_eq!(frame_at(200.0, zoomed, &clip), 45.0);
        clip.playback = Playback::Freeze;
        assert_eq!(frame_at(200.0, zoomed, &clip), 30.0);
    }
}
