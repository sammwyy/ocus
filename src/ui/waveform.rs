use crate::media::audio::Waveform;
use creamui::Color;
use creamui::core::{Painter, Rect};
use std::sync::Arc;

pub(super) struct WaveformSlice {
    pub peaks: Arc<Waveform>,
    pub start: u64,
    pub duration: u64,
    pub fps: (u32, u32),
}

fn frame_at(x: f32, full: Rect, start: u64, duration: u64) -> f64 {
    let fraction = ((f64::from(x) - f64::from(full.x)) / f64::from(full.width)).clamp(0.0, 1.0);
    start as f64 + duration as f64 * fraction
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
            let peak = self.peaks.peak(
                frame_at(x, full, self.start, self.duration),
                frame_at(next, full, self.start, self.duration),
                self.fps,
            );
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
        let full = Rect {
            x: -50.0,
            width: 200.0,
            ..Rect::default()
        };
        for (x, expected) in [(-50.0, 30.0), (0.0, 45.0), (100.0, 75.0), (150.0, 90.0)] {
            assert_eq!(frame_at(x, full, 30, 60), expected);
        }
        let zoomed = Rect {
            x: -100.0,
            width: 400.0,
            ..full
        };
        assert_eq!(frame_at(200.0, zoomed, 30, 60), 75.0);
    }
}
