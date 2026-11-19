use super::{Cut, Lane, Sequence, frame_rate, rate_value, source, source_path};
use crate::project::{FrameRate, Project};
use crate::timeline::TrackKind;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::fmt::Write;
use std::path::{Path, PathBuf};

fn schema(value: &Value) -> &str {
    value["OTIO_SCHEMA"].as_str().unwrap_or("unknown item")
}

fn children(value: &Value) -> Result<&[Value], String> {
    value["children"]
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| format!("{} has no children list", schema(value)))
}

fn inspect(value: &Value, notes: &mut BTreeSet<String>) -> Result<(), String> {
    if value["effects"]
        .as_array()
        .is_some_and(|effects| !effects.is_empty())
    {
        return Err(format!(
            "OTIO effects on {} are not supported",
            schema(value)
        ));
    }
    if value["markers"]
        .as_array()
        .is_some_and(|markers| !markers.is_empty())
    {
        notes.insert("OTIO markers were not imported.".into());
    }
    Ok(())
}

fn time(value: &Value) -> Result<(f64, f64), String> {
    let frames = value["value"].as_f64().ok_or("Missing OTIO time value")?;
    let rate = value["rate"].as_f64().ok_or("Missing OTIO time rate")?;
    if !frames.is_finite() || !rate.is_finite() || rate <= 0.0 {
        return Err("Invalid OTIO time".into());
    }
    Ok((frames, rate))
}

fn frames(value: &Value, timeline_rate: f64) -> Result<f64, String> {
    let (frames, rate) = time(value)?;
    // OTIO adapters use both decimal NTSC shorthands and exact rational rates.
    let rate = frame_rate(rate).map_or(rate, |rate| rate_value(&rate));
    Ok(frames / rate * timeline_rate)
}

fn frame_index(value: f64) -> Result<u64, String> {
    let round = value.round();
    if !(0.0..(1_u64 << 53) as f64).contains(&round) {
        return Err("OTIO time is outside the supported frame range".into());
    }
    Ok(round as u64)
}

fn reference(clip: &Value) -> &Value {
    match clip["active_media_reference_key"].as_str() {
        Some(key) => &clip["media_references"][key],
        None => &clip["media_reference"],
    }
}

fn range(clip: &Value) -> &Value {
    if schema(clip).starts_with("Clip.") && clip["source_range"].is_null() {
        &reference(clip)["available_range"]
    } else {
        &clip["source_range"]
    }
}

pub(super) fn read(input: &str) -> Result<Sequence, String> {
    let root: Value =
        serde_json::from_str(input).map_err(|error| format!("Invalid OTIO JSON: {error}"))?;
    let stack = &root["tracks"];
    if schema(&root) != "Timeline.1" || schema(stack) != "Stack.1" {
        return Err("OTIO import requires a Timeline with a stack of tracks".into());
    }
    let metadata = &root["metadata"]["ocus"];
    let rate = serde_json::from_value::<FrameRate>(metadata["frame_rate"].clone())
        .ok()
        .filter(|rate| rate.denominator != 0 && frame_rate(rate_value(rate)).is_ok())
        .map(Ok)
        .unwrap_or_else(|| {
            let nominal = root["global_start_time"]["rate"]
                .as_f64()
                .or_else(|| {
                    children(stack)
                        .ok()?
                        .iter()
                        .filter_map(|track| children(track).ok())
                        .flatten()
                        .find_map(|item| range(item)["duration"]["rate"].as_f64())
                })
                .unwrap_or(30.0);
            frame_rate(nominal)
        })?;
    let rate_fps = rate_value(&rate);
    let mut sequence = Sequence::new(root["name"].as_str().unwrap_or_default().into(), rate);
    if root["global_start_time"]["value"]
        .as_f64()
        .is_some_and(|value| value != 0.0)
    {
        sequence
            .notes
            .insert("The OTIO sequence timecode origin was reset to zero.".into());
    }
    if let (Some(width), Some(height)) = (metadata["width"].as_u64(), metadata["height"].as_u64())
        && let (Ok(width), Ok(height)) = (u32::try_from(width), u32::try_from(height))
        && width > 0
        && height > 0
    {
        sequence.size = Some((width, height));
    }
    if !stack["source_range"].is_null() {
        return Err("Trimmed OTIO track stacks are not supported".into());
    }
    inspect(stack, &mut sequence.notes)?;
    if stack["enabled"] == false {
        sequence
            .notes
            .insert("Disabled OTIO tracks were not imported.".into());
        return Ok(sequence);
    }
    let mut timeline_end = 0;
    let mut last_cut = 0;
    for track in children(stack)? {
        if schema(track) != "Track.1" || !track["source_range"].is_null() {
            return Err("Nested compositions and trimmed OTIO tracks are not supported".into());
        }
        if track["enabled"] == false {
            sequence
                .notes
                .insert("Disabled OTIO tracks were not imported.".into());
            continue;
        }
        inspect(track, &mut sequence.notes)?;
        let kind = match track["kind"].as_str() {
            Some("Video") => TrackKind::Video,
            Some("Audio") => TrackKind::Audio,
            _ => return Err("Unsupported OTIO track kind (expected Video or Audio)".into()),
        };
        let mut lane = Lane {
            kind,
            name: track["name"].as_str().unwrap_or_default().into(),
            cuts: Vec::new(),
        };
        let mut cursor = 0.0;
        for item in children(track)? {
            let is_clip = match schema(item) {
                "Clip.1" | "Clip.2" => true,
                "Gap.1" => false,
                other => {
                    return Err(format!(
                        "Unsupported OTIO item {other}; only media clips and gaps are supported"
                    ));
                }
            };
            let range = range(item);
            let length = frames(&range["duration"], rate_fps)?;
            if length < 0.0 || (is_clip && length == 0.0) {
                return Err("Invalid OTIO clip or gap duration".into());
            }
            let start = frame_index(cursor)?;
            cursor += length;
            let end = frame_index(cursor)?;
            timeline_end = timeline_end.max(end);
            if !is_clip {
                inspect(item, &mut sequence.notes)?;
                continue;
            }
            if item["enabled"] == false {
                sequence
                    .notes
                    .insert("Disabled OTIO clips were imported as gaps.".into());
                continue;
            }
            inspect(item, &mut sequence.notes)?;
            if start == end {
                return Err("An OTIO clip is too short for the timeline frame rate".into());
            }
            let reference = reference(item);
            if schema(reference) != "ExternalReference.1" {
                return Err(format!(
                    "OTIO clip {:?} has no external media file ({})",
                    item["name"].as_str().unwrap_or("unnamed"),
                    schema(reference)
                ));
            }
            let path = target_path(
                reference["target_url"]
                    .as_str()
                    .ok_or("Missing OTIO media path")?,
            )?;
            let source = frames(&range["start_time"], rate_fps)?;
            let available_start = &reference["available_range"]["start_time"];
            let origin = if available_start.is_null() {
                0.0
            } else {
                frames(available_start, rate_fps)?
            };
            lane.cuts.push(Cut {
                path,
                start,
                source: frame_index(source - origin)?,
                duration: end - start,
                linked_audio: false,
            });
            last_cut = last_cut.max(end);
        }
        sequence.tracks.push(lane);
    }
    if timeline_end > last_cut {
        sequence
            .notes
            .insert("Trailing OTIO gaps after the last media cut were not imported.".into());
    }
    Ok(sequence)
}

fn target_path(target: &str) -> Result<PathBuf, String> {
    if target.trim().is_empty() || target.contains('\0') {
        return Err("OTIO media path is empty or invalid".into());
    }
    let Some(url) = target.strip_prefix("file://") else {
        if target.contains("://") {
            return Err(format!(
                "Unsupported OTIO media URL: {target}; use local files"
            ));
        }
        return Ok(target.into());
    };
    let path = url
        .strip_prefix("localhost")
        .filter(|path| path.starts_with('/'))
        .unwrap_or(url);
    if !path.starts_with('/') {
        return Err("OTIO file URLs must point to local media".into());
    }
    let mut bytes = Vec::with_capacity(path.len());
    let mut input = path.bytes();
    while let Some(byte) = input.next() {
        bytes.push(if byte == b'%' {
            let hex = |byte: u8| char::from(byte).to_digit(16).map(|digit| digit as u8);
            let hi = input
                .next()
                .and_then(hex)
                .ok_or("Invalid escape in OTIO file URL")?;
            let lo = input
                .next()
                .and_then(hex)
                .ok_or("Invalid escape in OTIO file URL")?;
            hi * 16 + lo
        } else {
            byte
        });
    }
    let decoded = String::from_utf8(bytes).map_err(|_| "OTIO file URL is not UTF-8")?;
    if decoded.contains('\0') {
        return Err("Invalid OTIO file URL".into());
    }
    #[cfg(windows)]
    let decoded = decoded
        .strip_prefix('/')
        .filter(|path| path.as_bytes().get(1) == Some(&b':'))
        .unwrap_or(&decoded);
    Ok(decoded.into())
}

fn target_url(path: &Path) -> Result<String, String> {
    let path = source_path(path)?;
    #[cfg(windows)]
    let path = path
        .strip_prefix(r"\\?\")
        .unwrap_or(&path)
        .replace('\\', "/");
    let mut url = String::from(if path.starts_with('/') {
        "file://"
    } else {
        "file:///"
    });
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~/:".contains(&byte) {
            url.push(char::from(byte));
        } else {
            write!(url, "%{byte:02X}").expect("writing to a String cannot fail");
        }
    }
    Ok(url)
}

fn rational(value: u64, rate: f64) -> Value {
    json!({"OTIO_SCHEMA": "RationalTime.1", "value": value, "rate": rate})
}

fn time_range(start: u64, duration: u64, rate: f64) -> Value {
    json!({
        "OTIO_SCHEMA": "TimeRange.1",
        "start_time": rational(start, rate),
        "duration": rational(duration, rate),
    })
}

fn item(schema: &str, name: &str, range: Value) -> Value {
    json!({
        "OTIO_SCHEMA": schema, "name": name, "metadata": {}, "source_range": range,
        "effects": [], "markers": [], "enabled": true, "color": null,
    })
}

fn push(
    children: &mut Vec<Value>,
    cursor: &mut u64,
    start: u64,
    duration: u64,
    clip: Value,
    rate: f64,
) {
    if start > *cursor {
        children.push(item("Gap.1", "", time_range(0, start - *cursor, rate)));
    }
    children.push(clip);
    *cursor = start + duration;
}

fn track(name: &str, kind: &str, children: Vec<Value>) -> Value {
    let mut track = item("Track.1", name, Value::Null);
    track["kind"] = json!(kind);
    track["children"] = json!(children);
    track
}

pub(super) fn write(project: &Project, notes: &mut BTreeSet<String>) -> Result<String, String> {
    let settings = project.settings();
    let rate = rate_value(&settings.frame_rate);
    frame_rate(rate)?;
    let mut tracks = Vec::new();
    for lane in project.tracks() {
        if lane.hidden {
            if !lane.clips.is_empty() {
                notes.insert("Hidden tracks were not exported.".into());
            }
            continue;
        }
        let kind = if lane.kind == TrackKind::Video {
            "Video"
        } else {
            "Audio"
        };
        let mut children = Vec::new();
        let mut linked = Vec::new();
        let (mut cursor, mut linked_cursor) = (0, 0);
        for clip in &lane.clips {
            if lane.kind == TrackKind::Audio && clip.muted {
                notes.insert("Muted audio was omitted.".into());
                continue;
            }
            let media = source(project, clip, notes)?;
            if clip.start_frame + clip.duration_frames >= 1 << 53
                || clip.source_start_frame + clip.duration_frames >= 1 << 53
            {
                return Err("Clip exceeds the supported OTIO frame range".into());
            }
            let mut value = item(
                "Clip.2",
                &media.path.file_name().unwrap_or_default().to_string_lossy(),
                time_range(clip.source_start_frame, clip.duration_frames, rate),
            );
            value["active_media_reference_key"] = json!("DEFAULT_MEDIA");
            value["media_references"] = json!({
                "DEFAULT_MEDIA": {
                    "OTIO_SCHEMA": "ExternalReference.1", "name": "", "metadata": {},
                    "available_range": null, "available_image_bounds": null,
                    "target_url": target_url(&media.path)?,
                },
            });
            if clip.linked_audio && !clip.muted {
                push(
                    &mut linked,
                    &mut linked_cursor,
                    clip.start_frame,
                    clip.duration_frames,
                    value.clone(),
                    rate,
                );
            } else if clip.linked_audio {
                notes.insert("Muted audio was omitted.".into());
            }
            push(
                &mut children,
                &mut cursor,
                clip.start_frame,
                clip.duration_frames,
                value,
                rate,
            );
        }
        tracks.push(track(&lane.name, kind, children));
        if !linked.is_empty() {
            tracks.push(track(&lane.display_name("Video"), "Audio", linked));
            notes.insert("Linked video sound was exported on separate audio tracks.".into());
        }
    }
    let mut stack = item("Stack.1", "tracks", Value::Null);
    stack["children"] = json!(tracks);
    serde_json::to_string_pretty(&json!({
        "OTIO_SCHEMA": "Timeline.1",
        "name": project.name(),
        "metadata": {"ocus": {
            "frame_rate": settings.frame_rate, "width": settings.width, "height": settings.height,
        }},
        "global_start_time": rational(0, rate),
        "tracks": stack,
    }))
    .map_err(|error| error.to_string())
}
