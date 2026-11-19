use super::{Cut, Lane, Sequence, frame_rate, rate_value, source, source_path};
use crate::project::{FrameRate, Project};
use crate::timeline::TrackKind;
use std::collections::{BTreeSet, HashMap};
use std::fmt::Write;
use std::path::PathBuf;

struct Timecode {
    base: u64,
    drop: u64,
}

impl Timecode {
    fn new(rate: &FrameRate, drop: bool) -> Result<Self, String> {
        let fps = rate_value(rate);
        let base = fps.round() as u64;
        if !(1..=60).contains(&base)
            || (fps - base as f64).abs() > 0.001
                && (fps - base as f64 * 1000.0 / 1001.0).abs() > 0.001
        {
            return Err("CMX3600 requires a whole or NTSC frame rate up to 60 fps; use OTIO for other rates".into());
        }
        let dropped = if drop {
            match (rate.numerator, rate.denominator) {
                (30_000, 1001) => 2,
                (60_000, 1001) => 4,
                _ => return Err("Drop-frame EDL requires 29.97 or 59.94 fps".into()),
            }
        } else {
            0
        };
        Ok(Self {
            base,
            drop: dropped,
        })
    }

    fn parse(&self, value: &str) -> Result<u64, String> {
        let parts: Vec<_> = value.split([':', ';']).collect();
        let [h, m, s, f] = parts.as_slice() else {
            return Err(format!("Invalid EDL timecode: {value}"));
        };
        let number = |s: &str| {
            s.parse::<u64>()
                .ok()
                .filter(|_| s.len() == 2 && s.bytes().all(|b| b.is_ascii_digit()))
        };
        if let (Some(h), Some(m), Some(s), Some(f)) = (number(h), number(m), number(s), number(f))
            && h < 24
            && m < 60
            && s < 60
            && f < self.base
            && !(s == 0 && m % 10 != 0 && f < self.drop)
        {
            let minutes = h * 60 + m;
            return Ok((minutes * 60 + s) * self.base + f - self.drop * (minutes - minutes / 10));
        }
        Err(format!("Invalid EDL timecode: {value}"))
    }

    fn format(&self, frames: u64) -> Result<String, String> {
        let minute = self.base * 60 - self.drop;
        let ten = minute * 10 + self.drop;
        let omitted =
            self.drop * (9 * (frames / ten) + (frames % ten).saturating_sub(self.drop) / minute);
        let value = frames.checked_add(omitted).ok_or("EDL timecode overflow")?;
        let seconds = value / self.base;
        if seconds >= 24 * 3600 {
            return Err("EDL timecode reaches 24 hours; use OTIO for this timeline".into());
        }
        let separator = if self.drop == 0 { ':' } else { ';' };
        Ok(format!(
            "{:02}:{:02}:{:02}{separator}{:02}",
            seconds / 3600,
            seconds / 60 % 60,
            seconds % 60,
            value % self.base
        ))
    }
}

struct Event<'a> {
    number: &'a str,
    reel: &'a str,
    path: Option<&'a str>,
    video: bool,
    audio: bool,
    source: u64,
    record: u64,
    duration: u64,
}

fn lines(input: &str) -> impl Iterator<Item = &str> {
    input.trim_start_matches('\u{feff}').lines().map(str::trim)
}

fn header(input: &str) -> Result<(bool, Option<&str>), String> {
    let drop = match lines(input)
        .find_map(|line| line.strip_prefix("FCM:"))
        .map(str::trim)
    {
        Some("DROP FRAME") => true,
        Some("NON-DROP FRAME") | None => false,
        Some(_) => return Err("Unrecognized EDL frame-counting mode (FCM)".into()),
    };
    Ok((
        drop,
        lines(input).find_map(|line| line.strip_prefix("* FRAME RATE:")),
    ))
}

pub(super) fn rate_needed(input: &str) -> Result<Option<bool>, String> {
    let (drop, explicit) = header(input)?;
    Ok(explicit.is_none().then_some(drop))
}

pub(super) fn read(input: &str, fallback: &FrameRate) -> Result<Sequence, String> {
    let title = lines(input)
        .find_map(|line| line.strip_prefix("TITLE:"))
        .unwrap_or_default()
        .trim();
    let (drop, explicit) = header(input)?;
    let rate = if let Some(value) = explicit {
        frame_rate(value.trim().parse().map_err(|_| "Invalid EDL frame rate")?)?
    } else if drop {
        frame_rate(if rate_value(fallback).round() == 60.0 {
            59.94
        } else {
            29.97
        })?
    } else {
        fallback.clone()
    };
    let tc = Timecode::new(&rate, drop)?;
    let mut sequence = Sequence::new(title.into(), rate);
    if explicit.is_none() {
        sequence.notes.insert(format!(
            "No frame rate was declared in the EDL. Interpreted as {:.3} fps.",
            rate_value(&sequence.rate)
        ));
    }
    let mut events: Vec<Event<'_>> = Vec::new();
    for line in lines(input) {
        if line.is_empty() || line.starts_with("TITLE:") || line.starts_with("FCM:") {
            continue;
        }
        if let Some(comment) = line.strip_prefix('*') {
            let comment = comment.trim_start();
            if let Some(event) = events.last_mut() {
                if let Some(path) = ["SOURCE FILE:", "FROM FILE:"]
                    .iter()
                    .find_map(|key| comment.strip_prefix(key))
                {
                    event.path = Some(path.trim());
                } else if let Some(path) = comment.strip_prefix("FROM CLIP NAME:")
                    && event.path.is_none()
                {
                    event.path = Some(path.trim());
                }
            }
            continue;
        }
        if line == "M2" || line.starts_with("M2 ") || line.starts_with("M2\t") {
            return Err("EDL speed changes (M2) are not supported".into());
        }
        let mut cols = line.split_whitespace();
        let (Some(number), Some(reel), Some(channel), Some(edit)) =
            (cols.next(), cols.next(), cols.next(), cols.next())
        else {
            return Err(format!("Unrecognized EDL line: {line}"));
        };
        if number.parse::<u32>().is_err() {
            return Err(format!("Unrecognized EDL line: {line}"));
        }
        if edit != "C" {
            return Err(format!(
                "EDL event {number} uses edit type {edit}; only straight cuts (C) are supported"
            ));
        }
        let (video, audio) = match channel {
            "V" => (true, false),
            "A" | "AA" | "A1" | "A2" => (false, true),
            "B" | "AV" | "AA/V" | "A/V" | "V/A" | "V/AA" => (true, true),
            _ => {
                return Err(format!(
                    "EDL event {number} uses unsupported channels: {channel}"
                ));
            }
        };
        if matches!(channel, "A1" | "A2") {
            sequence
                .notes
                .insert("EDL channel-specific edits use the full source audio stream.".into());
        }
        let mut times = cols.map(|value| tc.parse(value));
        let mut next = || {
            times
                .next()
                .ok_or_else(|| format!("EDL event {number} is missing timecodes"))?
        };
        let [source, source_out, record, record_out] = [next()?, next()?, next()?, next()?];
        if times.next().is_some()
            || record_out <= record
            || source_out.checked_sub(source) != Some(record_out - record)
        {
            return Err(format!(
                "EDL event {number} has mismatched source and record durations"
            ));
        }
        events.push(Event {
            number,
            reel,
            path: None,
            video,
            audio,
            source,
            record,
            duration: record_out - record,
        });
    }
    if events.is_empty() {
        return Err("The EDL contains no cut events".into());
    }
    let origin = tc.parse("01:00:00:00")?;
    let origin = if events.iter().all(|event| event.record >= origin) {
        origin
    } else {
        0
    };
    let mut video = Lane {
        kind: TrackKind::Video,
        name: String::new(),
        cuts: Vec::new(),
    };
    let mut audio = Lane {
        kind: TrackKind::Audio,
        name: String::new(),
        cuts: Vec::new(),
    };
    let mut audio_ranges = Vec::new();
    for event in events {
        if matches!(event.reel, "BL" | "BLACK") && event.path.is_none() {
            continue;
        }
        let path = event.path.filter(|path| !path.is_empty() && !path.contains('\0')).ok_or_else(|| {
            format!("EDL event {} uses reel {} without a media filename. Add a * SOURCE FILE: comment for that event.", event.number, event.reel)
        })?;
        if event.audio {
            audio_ranges.push((event.record, event.record + event.duration));
        }
        let lane = if event.video { &mut video } else { &mut audio };
        lane.cuts.push(Cut {
            path: PathBuf::from(path),
            start: event.record - origin,
            source: event.source,
            duration: event.duration,
            linked_audio: event.video && event.audio,
        });
    }
    audio_ranges.sort_unstable();
    if audio_ranges.windows(2).any(|pair| pair[0].1 > pair[1].0) {
        return Err(
            "The EDL has overlapping audio events. Only one audio lane is supported.".into(),
        );
    }
    for mut lane in [video, audio] {
        lane.cuts.sort_by_key(|cut| cut.start);
        if lane
            .cuts
            .windows(2)
            .any(|pair| pair[0].start + pair[0].duration > pair[1].start)
        {
            return Err(
                "The EDL has overlapping video events. Only one video lane is supported.".into(),
            );
        }
        if !lane.cuts.is_empty() {
            sequence.tracks.push(lane);
        }
    }
    Ok(sequence)
}

pub(super) fn write(project: &Project, notes: &mut BTreeSet<String>) -> Result<String, String> {
    let rate = &project.settings().frame_rate;
    let drop = matches!((rate.numerator, rate.denominator), (30_000 | 60_000, 1001));
    let tc = Timecode::new(rate, drop)?;
    let origin = tc.parse("01:00:00:00")?;
    let name = project.name().replace(['\r', '\n'], " ");
    let fcm = if drop { "DROP FRAME" } else { "NON-DROP FRAME" };
    let mut out = format!(
        "TITLE: {name}\nFCM: {fcm}\n* FRAME RATE: {:.9}\n\n",
        rate_value(rate)
    );
    let mut events = Vec::new();
    for track in project.tracks() {
        if track.hidden {
            if !track.clips.is_empty() {
                notes.insert("Hidden tracks were not exported.".into());
            }
            continue;
        }
        for clip in &track.clips {
            let video = track.kind == TrackKind::Video;
            let audio = (!video || clip.linked_audio) && !clip.muted;
            if clip.muted && (!video || clip.linked_audio) {
                notes.insert("Muted audio was omitted.".into());
            }
            if !video && !audio {
                continue;
            }
            let media = source(project, clip, notes)?;
            events.push((clip, media, video, audio));
        }
    }
    if events.is_empty() {
        return Err("There are no visible, unmuted cuts to export to EDL".into());
    }
    if events.len() > 999 {
        return Err("CMX3600 supports at most 999 cut events; export as OTIO instead".into());
    }
    events.sort_by_key(|(clip, _, video, _)| (clip.start_frame, !video));
    let (mut video_end, mut audio_end) = (0, 0);
    let mut reels = HashMap::new();
    for (index, (clip, media, video, audio)) in events.into_iter().enumerate() {
        let end = clip.start_frame + clip.duration_frames;
        if (video && clip.start_frame < video_end) || (audio && clip.start_frame < audio_end) {
            return Err("EDL supports only one video and one audio lane without overlaps; export as OTIO instead".into());
        }
        if video {
            video_end = end;
        }
        if audio {
            audio_end = end;
        }
        let channel = match (video, audio) {
            (true, true) => "AA/V",
            (true, false) => "V",
            _ => "AA",
        };
        let path = source_path(&media.path)?;
        if path.trim() != path || path.contains(['\r', '\n']) {
            return Err("EDL cannot represent media paths with line breaks or trailing whitespace; use OTIO instead".into());
        }
        let reel_number = reels.len() + 1;
        let reel = reels.entry(media.id).or_insert(reel_number);
        let record =
            |frame: u64| tc.format(origin.checked_add(frame).ok_or("EDL timecode overflow")?);
        let filename = media.path.file_name().unwrap_or_default().to_string_lossy();
        writeln!(out, "{:03}  R{:07}  {:<4} C        {} {} {} {}\n* FROM CLIP NAME: {filename}\n* SOURCE FILE: {path}\n",
            index + 1, reel, channel, tc.format(clip.source_start_frame)?,
            tc.format(clip.source_start_frame + clip.duration_frames)?, record(clip.start_frame)?, record(end)?,
        ).expect("writing to a String cannot fail");
    }
    if project.settings().width != 1920 || project.settings().height != 1080 {
        notes.insert("EDL does not store project resolution.".into());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drop_frame_timecodes_skip_frame_numbers_not_timeline_frames() {
        for (rate, examples) in [
            (
                29.97,
                [
                    (1799, "00:00:59;29"),
                    (1800, "00:01:00;02"),
                    (17982, "00:10:00;00"),
                ],
            ),
            (
                59.94,
                [
                    (3599, "00:00:59;59"),
                    (3600, "00:01:00;04"),
                    (35964, "00:10:00;00"),
                ],
            ),
        ] {
            let tc = Timecode::new(&frame_rate(rate).unwrap(), true).unwrap();
            for (frames, value) in examples {
                assert_eq!(tc.parse(value).unwrap(), frames);
                assert_eq!(tc.format(frames).unwrap(), value);
            }
            assert!(tc.parse("00:01:00;00").is_err());
        }
    }
}
