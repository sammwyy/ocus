use crate::media::audio::{AudioQueue, CHANNELS, Clock, RATE};
use rodio::source::Source;
use rodio::{DeviceSinkBuilder, MixerDeviceSink, Player};
use std::num::NonZero;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// The default output device; opened lazily and kept for the app's lifetime.
pub struct Output {
    sink: MixerDeviceSink,
}

impl Output {
    pub fn open() -> Result<Self, String> {
        // Ask for stereo explicitly: a device reporting a mono default config
        // would otherwise downmix the mixer's output to one channel.
        DeviceSinkBuilder::from_default_device()
            .and_then(|builder| {
                builder
                    .with_channels(NonZero::new(CHANNELS as u16).unwrap())
                    .open_sink_or_fallback()
            })
            .map(|sink| Self { sink })
            .map_err(|error| format!("No audio output device: {error}"))
    }
}

/// One run of sound: the shared buffer the device drains, the flag that
/// stops its decode worker and the clock video pacing follows.
pub struct Session {
    pub queue: Arc<AudioQueue>,
    pub clock: Clock,
    pub cancel: Arc<AtomicBool>,
    player: Player,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.player.stop();
    }
}

/// Attaches a queue-driven source to the device and returns its session.
pub fn play(output: &Output) -> Session {
    let queue = Arc::new(AudioQueue::new());
    let player = Player::connect_new(output.sink.mixer());
    player.append(QueueSource {
        queue: queue.clone(),
    });
    Session {
        clock: queue.clock(),
        queue,
        cancel: Arc::new(AtomicBool::new(false)),
        player,
    }
}

/// Drains the queue for the device callback. Underruns yield uncounted
/// silence so the clock only measures decoded audio; once the stream ends
/// the silence still counts so the clock keeps wall-time pace.
struct QueueSource {
    queue: Arc<AudioQueue>,
}

impl Iterator for QueueSource {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        match self.queue.pop() {
            Some(sample) => Some(sample),
            None => {
                if self.queue.is_done() {
                    self.queue.note_played();
                }
                Some(0.0)
            }
        }
    }
}

impl Source for QueueSource {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> NonZero<u16> {
        NonZero::new(CHANNELS as u16).unwrap()
    }

    fn sample_rate(&self) -> NonZero<u32> {
        NonZero::new(RATE).unwrap()
    }

    fn total_duration(&self) -> Option<Duration> {
        None
    }
}
