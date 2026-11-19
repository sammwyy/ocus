use crate::project::Project;
use crate::timeline::{Clip, ClipContent, Title, Track, TrackKind};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cue {
    pub start_us: u64,
    pub end_us: u64,
    pub text: String,
}

fn timestamp(value: &str) -> Option<u64> {
    let parts = value.trim().split([':', ',', '.']).collect::<Vec<_>>();
    if parts.len() != 4 || parts[3].len() != 3 {
        return None;
    }
    let values = parts
        .iter()
        .map(|part| part.parse::<u64>().ok())
        .collect::<Option<Vec<_>>>()?;
    if values[1] >= 60 || values[2] >= 60 || values[3] >= 1000 {
        return None;
    }
    values[0]
        .checked_mul(3600)?
        .checked_add(values[1] * 60 + values[2])?
        .checked_mul(1_000_000)?
        .checked_add(values[3] * 1000)
}

pub fn parse(source: &str) -> Result<Vec<Cue>, String> {
    let lines = source
        .trim_start_matches('\u{feff}')
        .lines()
        .collect::<Vec<_>>();
    let mut cues = Vec::new();
    let mut first_error = 1;
    let mut index = 0;
    while index < lines.len() {
        let Some((start, end)) = lines[index].split_once("-->") else {
            index += 1;
            continue;
        };
        let line = index + 1;
        index += 1;
        let begin = index;
        while index < lines.len()
            && !lines[index].trim().is_empty()
            && !lines[index].contains("-->")
        {
            index += 1;
        }
        let text = lines[begin..index]
            .iter()
            .map(|line| line.trim_end())
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string();
        match (timestamp(start), timestamp(end)) {
            (Some(start_us), Some(end_us)) if end_us > start_us && !text.is_empty() => {
                cues.push(Cue {
                    start_us,
                    end_us,
                    text,
                });
            }
            _ => first_error = line,
        }
    }
    if cues.is_empty() {
        Err(format!("No usable subtitles (line {first_error})"))
    } else {
        cues.sort_by_key(|cue| cue.start_us);
        Ok(cues)
    }
}

fn markup(source: &str) -> (String, bool) {
    let mut text = String::new();
    let mut rest = source;
    let mut html = false;
    let mut fonts = Vec::new();
    while let Some(start) = rest.find('<') {
        text.push_str(&rest[..start]);
        let Some(end) = rest[start..].find('>').map(|end| start + end) else {
            text.push_str(&rest[start..]);
            return (text, html);
        };
        html = true;
        let token = rest[start + 1..end].trim().to_ascii_lowercase();
        match token.as_str() {
            "b" | "/b" | "i" | "/i" => {
                text.push('<');
                text.push_str(&token);
                text.push('>');
            }
            "/font" => {
                if fonts.pop() == Some(true) {
                    text.push_str("</color>");
                }
            }
            _ => {
                if token == "font" || token.starts_with("font ") {
                    let color = token
                        .split_once("color=")
                        .map(|(_, value)| {
                            let value = value.trim_start();
                            if let Some(quote @ ('\'' | '"')) = value.chars().next() {
                                value[1..].split(quote).next().unwrap_or("")
                            } else {
                                value.split_whitespace().next().unwrap_or("")
                            }
                        })
                        .unwrap_or("");
                    let supported = matches!(
                        color,
                        "red"
                            | "blue"
                            | "green"
                            | "white"
                            | "black"
                            | "yellow"
                            | "orange"
                            | "purple"
                            | "cyan"
                            | "pink"
                    ) || (color.len() == 7
                        && color.starts_with('#')
                        && color.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit));
                    fonts.push(supported);
                    if supported {
                        text.push_str(&format!("<color={color}>"));
                    }
                }
            }
        }
        rest = &rest[end + 1..];
    }
    text.push_str(rest);
    (text, html)
}

pub fn track(project: &Project, cues: &[Cue]) -> Result<Track, String> {
    let rate = &project.settings().frame_rate;
    let denominator = 1_000_000 * u128::from(rate.denominator);
    let frame = |us| -> Result<u64, String> {
        u64::try_from((u128::from(us) * u128::from(rate.numerator) + denominator / 2) / denominator)
            .map_err(|_| "Subtitle timestamp overflows".into())
    };
    let mut id = project
        .tracks()
        .iter()
        .flat_map(|track| &track.clips)
        .map(|clip| clip.id)
        .max()
        .unwrap_or(0);
    let mut ordered = cues.to_vec();
    ordered.sort_by_key(|cue| cue.start_us);
    let mut clips: Vec<Clip> = Vec::new();
    for cue in ordered {
        let start = frame(cue.start_us)?;
        let end =
            frame(cue.end_us)?.max(start.checked_add(1).ok_or("Subtitle timestamp overflows")?);
        if let Some(previous) = clips.last_mut() {
            previous.duration_frames = previous.duration_frames.min(start - previous.start_frame);
            if previous.duration_frames == 0 {
                clips.pop();
            }
        }
        id = id.checked_add(1).ok_or("No clip IDs available")?;
        let (text, parse_html) = markup(&cue.text);
        clips.push(Clip {
            transition: None,
            id,
            start_frame: start,
            source_start_frame: 0,
            duration_frames: end - start,
            content: ClipContent::Title(Title {
                text,
                parse_html,
                y: 0.9,
                font_size: 48.0,
                outline: true,
                bold: false,
                ..Title::default()
            }),
            volume: 1.0,
            fade_in_frames: 0,
            fade_out_frames: 0,
            muted: false,
            linked_audio: false,
        });
    }
    Ok(Track {
        id: project
            .tracks()
            .iter()
            .map(|track| track.id)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or("No track IDs available")?,
        kind: TrackKind::Video,
        subtitles: true,
        clips,
    })
}

pub fn write(project: &Project) -> Result<String, String> {
    let rate = &project.settings().frame_rate;
    let milliseconds = |frame: u64| -> Result<u64, String> {
        u64::try_from(
            (u128::from(frame) * u128::from(rate.denominator) * 1000
                + u128::from(rate.numerator) / 2)
                / u128::from(rate.numerator),
        )
        .map_err(|_| "Subtitle timestamp overflows".into())
    };
    let timecode = |ms: u64| {
        format!(
            "{:02}:{:02}:{:02},{:03}",
            ms / 3_600_000,
            ms / 60_000 % 60,
            ms / 1000 % 60,
            ms % 1000
        )
    };
    let mut clips = project
        .tracks()
        .iter()
        .filter(|track| track.subtitles)
        .flat_map(|track| &track.clips)
        .collect::<Vec<_>>();
    clips.sort_by_key(|clip| (clip.start_frame, clip.id));
    let mut result = String::new();
    let mut index = 0;
    for clip in clips {
        let ClipContent::Title(title) = &clip.content else {
            continue;
        };
        if title.text.trim().is_empty() {
            continue;
        }
        let start = milliseconds(clip.start_frame)?;
        let end = milliseconds(
            clip.start_frame
                .checked_add(clip.duration_frames)
                .ok_or("Subtitle timestamp overflows")?,
        )?
        .max(start.checked_add(1).ok_or("Subtitle timestamp overflows")?);
        let text = if title.parse_html {
            title
                .text
                .replace("<color=", "<font color=")
                .replace("</color>", "</font>")
        } else {
            title.text.clone()
        };
        index += 1;
        result.push_str(&format!(
            "{index}\n{} --> {}\n{}\n\n",
            timecode(start),
            timecode(end),
            text.trim()
        ));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writer_merges_tracks_and_round_trips_through_parser() {
        let project = Project::new("Subtitles");
        assert_eq!(write(&project).unwrap(), "");
        let mut editor = crate::project::Editor::new(project);
        for source in [
            "1\n00:00:02,000 --> 00:00:03,000\nLater\nline",
            "1\n00:00:00,000 --> 00:00:01,000\n<i>First</i>",
        ] {
            let track = track(editor.project(), &parse(source).unwrap()).unwrap();
            let mut tracks = editor.project().tracks().to_vec();
            tracks.push(track);
            assert!(editor.apply(crate::project::Command::ReplaceTracks(tracks)));
        }
        let source = write(editor.project()).unwrap();
        let cues = parse(&source).unwrap();
        assert_eq!(cues.len(), 2);
        assert_eq!(
            cues[0],
            Cue {
                start_us: 0,
                end_us: 1_000_000,
                text: "<i>First</i>".into()
            }
        );
        assert_eq!(
            cues[1],
            Cue {
                start_us: 2_000_000,
                end_us: 3_000_000,
                text: "Later\nline".into()
            }
        );
        let mut json: serde_json::Value =
            serde_json::from_str(&crate::project::format::to_json(editor.project()).unwrap())
                .unwrap();
        json["settings"]["frame_rate"] =
            serde_json::json!({ "numerator": 30000, "denominator": 1001 });
        json["tracks"][4]["clips"][0]["content"]["title"]["text"] =
            "<color=#ffffff>Later</color>".into();
        json["tracks"][4]["clips"][0]["content"]["title"]["parse_html"] = true.into();
        let fractional = crate::project::format::from_json(&json.to_string()).unwrap();
        let cues = parse(&write(&fractional).unwrap()).unwrap();
        assert_eq!(cues[1].start_us, 2_002_000);
        assert_eq!(cues[1].end_us, 3_003_000);
        assert_eq!(cues[1].text, "<font color=#ffffff>Later</font>");
        json["tracks"][4]["clips"][0]["duration_frames"] = u64::MAX.into();
        assert!(crate::project::format::from_json(&json.to_string()).is_err());
    }

    #[test]
    fn tolerant_srt() {
        let cues = parse("\u{feff}garbled\r\n00:00:01,000 --> 00:00:02.500  \r\n<i>Hello</i>  \r\nworld \r\n\r\n\r\n00:00:03.000 --> 00:00:04,000\nnext\n").unwrap();
        assert_eq!(cues.len(), 2);
        assert_eq!(
            cues[0],
            Cue {
                start_us: 1_000_000,
                end_us: 2_500_000,
                text: "<i>Hello</i>\nworld".into()
            }
        );
    }

    #[test]
    fn skips_invalid_and_empty() {
        let source = "1\n00:00:02,000 --> 00:00:01,000\nbad\n\n2\n00:00:01,000 --> 00:00:02,000\n\n3\ninvalid --> invalid\nbad\n\n4\n00:00:03,000 --> 00:00:04,000\nok";
        assert_eq!(parse(source).unwrap().len(), 1);
        assert!(
            parse("index\nbad --> bad\ntext")
                .unwrap_err()
                .contains("line 2")
        );
        assert!(parse("").unwrap_err().contains("line 1"));
    }

    #[test]
    fn converts_markup_and_strips_unsupported_tags() {
        assert_eq!(
            markup("<b>B</b><i>I</i><u>U</u><font color=\"red\">R</font><x>X</x>"),
            ("<b>B</b><i>I</i>U<color=red>R</color>X".into(), true)
        );
        assert_eq!(markup("plain"), ("plain".into(), false));
        assert_eq!(
            markup(
                "<font face=Arial>A</font><font color=\"#123456\" size=3>B</font><font color=invalid>C</font>"
            ),
            ("A<color=#123456>B</color>C".into(), true)
        );
    }

    #[test]
    fn rounds_clips_and_clips_overlaps() {
        let project = Project::new("Subtitles");
        let cues = vec![
            Cue {
                start_us: 20_000,
                end_us: 2_000_000,
                text: "first".into(),
            },
            Cue {
                start_us: 1_000_000,
                end_us: 1_000_001,
                text: "next".into(),
            },
        ];
        let result = track(&project, &cues).unwrap();
        let rate = &project.settings().frame_rate;
        let fps = f64::from(rate.numerator) / f64::from(rate.denominator);
        assert_eq!(result.clips[0].start_frame, (0.02 * fps).round() as u64);
        assert_eq!(
            result.clips[0].start_frame + result.clips[0].duration_frames,
            result.clips[1].start_frame
        );
        assert_eq!(result.clips[1].duration_frames, 1);
        assert_ne!(result.clips[0].id, result.clips[1].id);
        assert!(track(&project, &[]).unwrap().clips.is_empty());
    }

    #[test]
    fn fractional_frame_rate_and_identical_starts() {
        let mut document: serde_json::Value =
            serde_json::from_str(&crate::project::format::to_json(&Project::new("NTSC")).unwrap())
                .unwrap();
        document["settings"]["frame_rate"] =
            serde_json::json!({ "numerator": 30000, "denominator": 1001 });
        let project = crate::project::format::from_json(&document.to_string()).unwrap();
        let cues = vec![
            Cue {
                start_us: 1_001_000,
                end_us: 2_002_000,
                text: "first".into(),
            },
            Cue {
                start_us: 1_001_000,
                end_us: 3_003_000,
                text: "second".into(),
            },
        ];
        let result = track(&project, &cues).unwrap();
        assert_eq!(result.clips.len(), 1);
        assert_eq!(result.clips[0].start_frame, 30);
        assert_eq!(result.clips[0].duration_frames, 60);
        let ClipContent::Title(title) = &result.clips[0].content else {
            unreachable!()
        };
        assert_eq!(title.y, 0.9);
        assert!(title.outline);
        assert!(!title.bold);
        assert_eq!(title.background, None);
    }
}
