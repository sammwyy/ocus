use super::CHANNELS;
use nnnoiseless::DenoiseState;
use std::ops::Range;

const BLOCK: usize = DenoiseState::FRAME_SIZE;
const PCM_SCALE: f32 = 32_768.0;

pub(super) struct NoiseReduction {
    states: [Box<DenoiseState<'static>>; CHANNELS],
    fresh: Vec<f32>,
    output: [[f32; BLOCK]; CHANNELS],
    ready: Range<usize>,
    previous: usize,
    remaining: u64,
}

impl NoiseReduction {
    pub(super) fn new(frames: u64) -> Self {
        Self {
            states: std::array::from_fn(|_| DenoiseState::new()),
            fresh: Vec::with_capacity(BLOCK * CHANNELS),
            output: [[0.0; BLOCK]; CHANNELS],
            ready: 0..0,
            previous: 0,
            remaining: frames,
        }
    }

    pub(super) fn read(
        &mut self,
        out: &mut Vec<f32>,
        frames: usize,
        mut read: impl FnMut(&mut Vec<f32>, usize) -> Result<usize, String>,
    ) -> Result<(), String> {
        let mut written = 0;
        while written < frames {
            if self.ready.is_empty() {
                if self.previous == 0 && self.remaining == 0 {
                    break;
                }
                let requested = self.remaining.min(BLOCK as u64) as usize;
                self.fresh.clear();
                let count = if requested > 0 {
                    read(&mut self.fresh, requested)?
                } else {
                    0
                };
                self.remaining = if count < requested {
                    0
                } else {
                    self.remaining - count as u64
                };
                self.process(count);
                continue;
            }
            let count = self.ready.len().min(frames - written);
            for index in self.ready.start..self.ready.start + count {
                out.extend(self.output.iter().map(|channel| channel[index] / PCM_SCALE));
            }
            self.ready.start += count;
            written += count;
        }
        Ok(())
    }

    fn process(&mut self, count: usize) {
        let mut input = [0.0; BLOCK];
        for (channel, (state, output)) in self.states.iter_mut().zip(&mut self.output).enumerate() {
            input.fill(0.0);
            for (value, &sample) in input
                .iter_mut()
                .zip(self.fresh.iter().skip(channel).step_by(CHANNELS))
            {
                if sample.is_finite() {
                    *value = (sample * PCM_SCALE).clamp(-PCM_SCALE, PCM_SCALE - 1.0);
                }
            }
            state.process_frame(output, &input);
        }
        // RNNoise outputs the previous block. Skip startup output and zero-pad
        // the final lookahead so every decoded frame keeps its timeline position.
        self.ready = 0..self.previous;
        self.previous = count;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compensates_block_delay_and_flushes_the_partial_tail_without_crossing_the_cut() {
        let frames = BLOCK + 123;
        let raw: Vec<_> = (0..2 * BLOCK)
            .map(|i| ((i * 29 % 1000) as f32 - 500.0) / PCM_SCALE)
            .collect();
        let mut reference = DenoiseState::new();
        let mut block = [0.0; BLOCK];
        let mut input = [0.0; BLOCK];
        for (sample, &raw) in input.iter_mut().zip(&raw) {
            *sample = raw * PCM_SCALE;
        }
        reference.process_frame(&mut block, &input);
        input.fill(0.0);
        for (sample, &raw) in input.iter_mut().zip(&raw[BLOCK..frames]) {
            *sample = raw * PCM_SCALE;
        }
        reference.process_frame(&mut block, &input);
        let mut expected = block.to_vec();
        reference.process_frame(&mut block, &[0.0; BLOCK]);
        expected.extend_from_slice(&block[..123]);

        for (clip_length, source_length) in [(frames, raw.len()), (raw.len(), frames)] {
            let mut filter = NoiseReduction::new(clip_length as u64);
            let mut position = 0;
            let mut result = Vec::new();
            for requested in [17, BLOCK, BLOCK, 1] {
                filter
                    .read(&mut result, requested, |out, count| {
                        let end = (position + count).min(source_length);
                        out.extend(raw[position..end].iter().flat_map(|&sample| [sample, 0.0]));
                        let count = end - position;
                        position = end;
                        Ok(count)
                    })
                    .unwrap();
            }
            assert_eq!(position, frames);
            assert_eq!(result.len(), frames * CHANNELS);
            assert!(expected[BLOCK..].iter().any(|&sample| sample != 0.0));
            for (frame, &sample) in result.as_chunks::<CHANNELS>().0.iter().zip(&expected) {
                assert_eq!(*frame, [sample / PCM_SCALE, 0.0]);
            }
        }
    }
}
