use super::audio::{self, AudioPlan, AudioQueue};
use crate::subtitles::Cue;
use directories::ProjectDirs;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

const MODEL_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin";
const MODEL_SHA256: [u8; 32] = [
    0x60, 0xed, 0x5b, 0xc3, 0xdd, 0x14, 0xee, 0xa8, 0x56, 0x49, 0x3d, 0x33, 0x43, 0x49, 0xb4, 0x05,
    0x78, 0x2d, 0xdc, 0xaf, 0x00, 0x28, 0xd4, 0xb5, 0xdf, 0x40, 0x88, 0x34, 0x5f, 0xba, 0x2e, 0xfe,
];
const MODEL_SIZE: usize = 147_951_465;

pub fn model_path() -> Result<PathBuf, String> {
    ProjectDirs::from("", "", "Ocus")
        .map(|dirs| model_in(dirs.data_dir()))
        .ok_or_else(|| "Cannot find the application data directory".into())
}

fn model_in(data: &Path) -> PathBuf {
    data.join("models").join("ggml-base.bin")
}

fn valid_model(bytes: &[u8]) -> bool {
    bytes.len() == MODEL_SIZE && Sha256::digest(bytes).as_slice() == MODEL_SHA256
}

fn check_cancel(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        Err("Cancelled".into())
    } else {
        Ok(())
    }
}

pub fn check_model(path: &Path, cancel: &AtomicBool) -> Result<bool, String> {
    check_cancel(cancel)?;
    let valid = std::fs::read(path).is_ok_and(|bytes| valid_model(&bytes));
    check_cancel(cancel)?;
    Ok(valid)
}

pub fn download_model(
    path: &Path,
    cancel: &AtomicBool,
    mut progress: impl FnMut(String),
) -> Result<(), String> {
    check_cancel(cancel)?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_connect(Some(std::time::Duration::from_secs(15)))
        .timeout_global(Some(std::time::Duration::from_secs(1800)))
        .build()
        .into();
    let mut response = agent.get(MODEL_URL).call().map_err(|e| e.to_string())?;
    let mut reader = response.body_mut().as_reader();
    let mut bytes = Vec::with_capacity(MODEL_SIZE);
    let mut buffer = [0; 64 * 1024];
    let mut last_mb = usize::MAX;
    loop {
        check_cancel(cancel)?;
        let count = reader.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        if bytes.len() + count > MODEL_SIZE {
            return Err("Speech model download is larger than expected".into());
        }
        bytes.extend_from_slice(&buffer[..count]);
        let mb = bytes.len() / 1_048_576;
        if mb != last_mb {
            progress(format!("Downloading speech model: {mb}/142 MB…"));
            last_mb = mb;
        }
    }
    if !valid_model(&bytes) {
        return Err("Speech model checksum mismatch; please try again".into());
    }
    check_cancel(cancel)?;
    let parent = path.parent().ok_or("Invalid speech model path")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    crate::atomic_file::write(
        path,
        parent,
        path.file_name().ok_or("Invalid speech model filename")?,
        &bytes,
        "Cannot allocate speech model temporary file",
    )
    .map_err(|e| e.to_string())
}

fn cue(start: i64, end: i64, text: &str) -> Option<Cue> {
    let text = text.trim();
    if text.is_empty() || start < 0 || end <= start {
        return None;
    }
    Some(Cue {
        start_us: (start as u64).checked_mul(10_000)?,
        end_us: (end as u64).checked_mul(10_000)?,
        text: text.into(),
    })
}

fn speech_offset(samples: &[f32]) -> Option<usize> {
    samples
        .iter()
        .position(|sample| sample.abs() >= 0.0001)
        .map(|start| start.saturating_sub(1_600) / 160 * 160)
}

pub fn transcribe(
    plan: &AudioPlan,
    model: &Path,
    cancel: Arc<AtomicBool>,
) -> Result<Vec<Cue>, String> {
    let samples = mix(plan, cancel.clone())?;
    check_cancel(&cancel)?;
    let Some(offset) = speech_offset(&samples) else {
        return Ok(Vec::new());
    };
    whisper_rs::install_logging_hooks();
    let mut context_params = WhisperContextParameters::default();
    context_params.use_gpu(false);
    let context =
        WhisperContext::new_with_params(model, context_params).map_err(|e| e.to_string())?;
    let mut state = context.create_state().map_err(|e| e.to_string())?;
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_language(None);
    params.set_translate(false);
    params.set_token_timestamps(true);
    params.set_max_len(42);
    params.set_split_on_word(true);
    params.set_n_threads(std::thread::available_parallelism().map_or(2, |n| n.get().min(8)) as i32);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    unsafe extern "C" fn abort(data: *mut std::ffi::c_void) -> bool {
        unsafe { &*data.cast::<AtomicBool>() }.load(Ordering::Relaxed)
    }
    // whisper-rs 0.16's safe abort wrapper passes a double-boxed trait object
    // to a trampoline expecting the concrete closure. Keep the Arc alive for
    // the synchronous full() call and pass its stable AtomicBool address instead.
    unsafe {
        params.set_abort_callback(Some(abort));
        params.set_abort_callback_user_data(Arc::as_ptr(&cancel).cast_mut().cast());
    }
    let result = state
        .full(params, &samples[offset..])
        .map_err(|e| e.to_string());
    check_cancel(&cancel)?;
    result?;
    state
        .as_iter()
        .filter_map(|segment| match segment.to_str() {
            Ok(text) => cue(
                segment.start_timestamp() + (offset / 160) as i64,
                segment.end_timestamp() + (offset / 160) as i64,
                text,
            )
            .map(Ok),
            Err(error) => Some(Err(error.to_string())),
        })
        .collect()
}

fn mix(plan: &AudioPlan, cancel: Arc<AtomicBool>) -> Result<Vec<f32>, String> {
    use ffmpeg_next as ffmpeg;
    ffmpeg::init().map_err(|e| e.to_string())?;
    let format = ffmpeg::format::Sample::F32(ffmpeg::format::sample::Type::Packed);
    let mut resampler = ffmpeg::software::resampling::Context::get(
        format,
        ffmpeg::ChannelLayout::STEREO,
        audio::RATE,
        format,
        ffmpeg::ChannelLayout::MONO,
        16_000,
    )
    .map_err(|e| e.to_string())?;
    let queue = Arc::new(AudioQueue::new());
    std::thread::scope(|scope| {
        let producer_queue = queue.clone();
        let producer_cancel = cancel.clone();
        let worker = scope.spawn(move || {
            let result = audio::feed(plan, 0, producer_queue.clone(), producer_cancel);
            producer_queue.finish();
            result
        });
        let mut samples = Vec::new();
        let mut stereo = Vec::new();
        let mut convert = |stereo: &mut Vec<f32>| -> Result<(), String> {
            let mut input =
                ffmpeg::frame::Audio::new(format, stereo.len() / 2, ffmpeg::ChannelLayout::STEREO);
            input.set_rate(audio::RATE);
            for (pair, values) in input
                .plane_mut::<(f32, f32)>(0)
                .iter_mut()
                .zip(stereo.as_chunks::<2>().0)
            {
                *pair = (values[0], values[1]);
            }
            let mut output = ffmpeg::frame::Audio::new(
                format,
                stereo.len() / 6 + 256,
                ffmpeg::ChannelLayout::MONO,
            );
            resampler
                .run(&input, &mut output)
                .map_err(|e| e.to_string())?;
            samples.extend_from_slice(output.plane::<f32>(0));
            stereo.clear();
            Ok(())
        };
        while !queue.is_drained() && !cancel.load(Ordering::Relaxed) {
            if let Some(sample) = queue.pop() {
                stereo.push(sample);
                if stereo.len() == 6144
                    && let Err(error) = convert(&mut stereo)
                {
                    cancel.store(true, Ordering::Relaxed);
                    let _ = worker.join();
                    return Err(error);
                }
            } else {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
        worker.join().map_err(|_| "Audio mixing worker failed")??;
        check_cancel(&cancel)?;
        if !stereo.is_empty() {
            convert(&mut stereo)?;
        }
        loop {
            let mut output = ffmpeg::frame::Audio::new(format, 256, ffmpeg::ChannelLayout::MONO);
            let delay = resampler.flush(&mut output).map_err(|e| e.to_string())?;
            samples.extend_from_slice(output.plane::<f32>(0));
            if delay.is_none() {
                break;
            }
        }
        Ok(samples)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leading_silence_keeps_preroll_and_timeline_offset() {
        assert_eq!(speech_offset(&[]), None);
        assert_eq!(speech_offset(&[0.0; 16_000]), None);
        assert_eq!(speech_offset(&[0.00001; 16_000]), None);
        assert_eq!(speech_offset(&[0.1; 16_000]), Some(0));
        let mut samples = vec![0.0; 16_000];
        samples.extend([0.1; 1_600]);
        let offset = speech_offset(&samples).unwrap();
        assert_eq!(offset, 14_400);
        assert_eq!(
            cue((offset / 160) as i64, 200, "hello").unwrap().start_us,
            900_000
        );
    }

    #[test]
    #[ignore = "requires the downloaded base model, espeak-ng and ffmpeg"]
    fn real_speech() {
        let model = model_path().unwrap();
        if !model.exists() {
            eprintln!("Speech model is absent: {}", model.display());
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let speech = dir.path().join("speech.wav");
        let path = dir.path().join("delayed.wav");
        assert!(std::process::Command::new("espeak-ng")
            .args(["-v", "en-us", "-s", "140", "-w"])
            .arg(&speech)
            .arg("Hello world. This is a test of automatic subtitles. The quick brown fox jumps over the lazy dog.")
            .status().unwrap().success());
        assert!(
            std::process::Command::new("ffmpeg")
                .args(["-v", "error", "-i"])
                .arg(&speech)
                .args(["-af", "adelay=1000:all=1,apad=whole_dur=15"])
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );
        let media = crate::media::probe(&path).unwrap();
        let duration = media.duration_us.unwrap().div_ceil(1_000_000 / 30);
        let plan = AudioPlan {
            sources: vec![audio::AudioSource {
                path,
                start: 0,
                source_start: 0,
                duration,
                speed: 1.0,
                playback: crate::timeline::Playback::Forward,
                volume: 1.0,
                fade_in_frames: 0,
                fade_out_frames: 0,
                noise_reduction: false,
            }],
            end: duration,
            fps: (30, 1),
        };
        let cancel = Arc::new(AtomicBool::new(false));
        assert!(check_model(&model, &cancel).unwrap());
        let cues = transcribe(&plan, &model, cancel).unwrap();
        println!("Produced cues: {cues:#?}");
        assert!(cues.first().unwrap().start_us >= 700_000);
        assert!(cues.iter().all(|cue| cue.text.chars().count() <= 42));
        assert!(
            cues.iter()
                .all(|cue| cue.end_us - cue.start_us <= 6_000_000)
        );
        let text = cues
            .iter()
            .map(|cue| cue.text.to_lowercase())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            text.contains("hello") && text.contains("world") && text.contains("fox"),
            "{text}"
        );
    }

    #[test]
    fn segments_use_timeline_microseconds_and_trim_text() {
        assert_eq!(
            cue(123, 456, " hello ").unwrap(),
            Cue {
                start_us: 1_230_000,
                end_us: 4_560_000,
                text: "hello".into(),
            }
        );
        for (start, end, text) in [
            (0, 10, " \n"),
            (-1, 10, "hi"),
            (10, 10, "hi"),
            (20, 10, "hi"),
            (0, i64::MAX, "hi"),
        ] {
            assert!(cue(start, end, text).is_none());
        }
    }

    #[test]
    fn model_location_and_corruption() {
        assert_eq!(
            model_in(Path::new("data")),
            Path::new("data/models/ggml-base.bin")
        );
        assert!(model_path().unwrap().ends_with("models/ggml-base.bin"));
        assert!(!valid_model(&[]));
        assert!(!valid_model(b"partial download"));
        assert!(!valid_model(&vec![0; MODEL_SIZE]));
    }

    #[test]
    fn missing_or_corrupt_model_needs_download_and_cancellation_never_writes() {
        let dir = tempfile::tempdir().unwrap();
        let path = model_in(dir.path());
        let cancel = AtomicBool::new(false);
        assert_eq!(check_model(&path, &cancel), Ok(false));
        assert!(!path.exists());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"broken").unwrap();
        assert_eq!(check_model(&path, &cancel), Ok(false));
        assert_eq!(std::fs::read(&path).unwrap(), b"broken");
        cancel.store(true, Ordering::Relaxed);
        assert_eq!(
            download_model(&path, &cancel, |_| {}),
            Err("Cancelled".into())
        );
        assert_eq!(check_model(&path, &cancel), Err("Cancelled".into()));
    }

    #[test]
    fn empty_mix_and_cancelled_mix() {
        let plan = AudioPlan {
            sources: vec![],
            end: 0,
            fps: (30, 1),
        };
        assert!(
            mix(&plan, Arc::new(AtomicBool::new(false)))
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            mix(&plan, Arc::new(AtomicBool::new(true))),
            Err("Cancelled".into())
        );
    }
}
