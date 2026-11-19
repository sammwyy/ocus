use crate::timeline::Title;
use creamui::Color;

pub(crate) fn animated(title: &Title, motion: crate::timeline::TitleMotion) -> (Title, Vec<Run>) {
    let mut runs = parse(title);
    let count: usize = runs.iter().map(|run| run.text.chars().count()).sum();
    let mut remaining = (count as f32 * motion.reveal).floor() as usize;
    for run in &mut runs {
        let length = run.text.chars().count().min(remaining);
        run.text = run.text.chars().take(length).collect();
        remaining -= length;
        run.color.a = (f32::from(run.color.a) * motion.opacity).round() as u8;
    }
    let mut title = title.clone();
    title.y += motion.y_offset;
    title.font_size *= motion.scale;
    if let Some(color) = &mut title.background {
        color[3] = (f32::from(color[3]) * motion.opacity).round() as u8;
    }
    (title, runs)
}

#[test]
fn animated_markup_reveals_rendered_unicode_characters_and_fades_all_styles() {
    let title = Title {
        text: "<color=red>é🙂</color> **ok**".into(),
        parse_html: true,
        parse_markdown: true,
        background: Some([0, 0, 0, 200]),
        ..Title::default()
    };
    let (resolved, runs) = animated(
        &title,
        crate::timeline::TitleMotion {
            opacity: 0.5,
            y_offset: 0.05,
            scale: 0.6,
            reveal: 0.4,
        },
    );
    assert_eq!(
        runs.iter().map(|run| run.text.as_str()).collect::<String>(),
        "é🙂"
    );
    assert!(runs.iter().all(|run| run.color.a == 128));
    assert_eq!(resolved.background.unwrap()[3], 100);
    assert_eq!(resolved.y, 0.55);
    assert_eq!(resolved.font_size, title.font_size * 0.6);
    let empty = animated(
        &Title::default(),
        crate::timeline::TitleAnimation::Typewriter.resolve(0, 1, (30, 1)),
    );
    assert!(empty.1.iter().all(|run| run.text.is_empty()));
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Run {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub color: Color,
}

#[derive(Clone, Copy)]
struct Style {
    bold: bool,
    italic: bool,
    color: Color,
}

fn named_color(value: &str) -> Option<Color> {
    match value.to_ascii_lowercase().as_str() {
        "red" => Some(Color::rgb(255, 0, 0)),
        "blue" => Some(Color::rgb(0, 0, 255)),
        "green" => Some(Color::rgb(0, 128, 0)),
        "white" => Some(Color::rgb(255, 255, 255)),
        "black" => Some(Color::rgb(0, 0, 0)),
        "yellow" => Some(Color::rgb(255, 255, 0)),
        "orange" => Some(Color::rgb(255, 165, 0)),
        "purple" => Some(Color::rgb(128, 0, 128)),
        "cyan" => Some(Color::rgb(0, 255, 255)),
        "pink" => Some(Color::rgb(255, 192, 203)),
        _ => None,
    }
}

pub(crate) fn parse(title: &Title) -> Vec<Run> {
    let source = title.text.as_str();
    let base = Style {
        bold: title.bold,
        italic: title.italic,
        color: Color::rgba(
            title.color[0],
            title.color[1],
            title.color[2],
            title.color[3],
        ),
    };
    let mut style = base;
    let mut html: Vec<(&str, Style)> = Vec::new();
    let mut md_bold = false;
    let mut md_italic = None;
    let mut runs: Vec<Run> = Vec::new();
    let mut index = 0;
    while index < source.len() {
        let remaining = &source[index..];
        if title.parse_html
            && remaining.starts_with('<')
            && let Some(end) = remaining.find('>')
        {
            let token = &remaining[1..end];
            let name = match token {
                "b" | "bold" => Some("bold"),
                "i" | "italic" => Some("italic"),
                _ if token.starts_with("color=") => Some("color"),
                _ => None,
            };
            if let Some(name) = name {
                let color = if name == "color" {
                    let value = &token[6..];
                    named_color(value).or_else(|| {
                        (value.len() == 7
                            && value.starts_with('#')
                            && value.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit))
                        .then(|| value.parse::<Color>().ok())
                        .flatten()
                    })
                } else {
                    Some(style.color)
                };
                if let Some(color) = color {
                    html.push((name, style));
                    if name == "bold" {
                        style.bold = true;
                    } else if name == "italic" {
                        style.italic = true;
                    } else {
                        style.color = Color::rgba(color.r, color.g, color.b, style.color.a);
                    }
                    index += end + 1;
                    continue;
                }
            }
            if let Some(name) = token.strip_prefix('/')
                && html.last().is_some_and(|(open, _)| {
                    *open
                        == match name {
                            "b" | "bold" => "bold",
                            "i" | "italic" => "italic",
                            _ => name,
                        }
                })
            {
                style = html.pop().unwrap().1;
                index += end + 1;
                continue;
            }
        }
        if title.parse_markdown {
            if remaining.starts_with("**") && (md_bold || remaining[2..].contains("**")) {
                md_bold = !md_bold;
                index += 2;
                continue;
            }
            if let Some(marker) = remaining
                .chars()
                .next()
                .filter(|ch| *ch == '_' || *ch == '*')
                && !(marker == '*' && remaining.starts_with("**"))
                && (md_italic == Some(marker)
                    || (md_italic.is_none() && remaining[1..].contains(marker)))
                && !(source[..index]
                    .chars()
                    .last()
                    .is_some_and(char::is_alphanumeric)
                    && remaining[1..]
                        .chars()
                        .next()
                        .is_some_and(char::is_alphanumeric))
            {
                md_italic = if md_italic.is_some() {
                    None
                } else {
                    Some(marker)
                };
                index += 1;
                continue;
            }
        }
        let ch = remaining.chars().next().unwrap();
        let bold = style.bold || md_bold;
        let italic = style.italic || md_italic.is_some();
        if let Some(last) = runs.last_mut()
            && last.bold == bold
            && last.italic == italic
            && last.color == style.color
        {
            last.text.push(ch);
        } else {
            runs.push(Run {
                text: ch.to_string(),
                bold,
                italic,
                color: style.color,
            });
        }
        index += ch.len_utf8();
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_and_markdown_styles_combine_without_evaluating_html() {
        let title = Title {
            text: "<b>hello **bold**</b> _italic_ <color=red>red</color> <color=#ff0000>x</color> <script>bad</script>".into(),
            bold: false,
            parse_html: true,
            parse_markdown: true,
            ..Title::default()
        };
        let runs = parse(&title);
        assert!(
            runs.iter()
                .any(|run| run.text.contains("hello ") && run.bold)
        );
        assert!(
            runs.iter()
                .any(|run| run.text.contains("italic") && run.italic)
        );
        assert!(
            runs.iter()
                .any(|run| run.text == "red" && run.color == Color::rgb(255, 0, 0))
        );
        assert!(
            runs.iter()
                .any(|run| run.text == "x" && run.color == Color::rgb(255, 0, 0))
        );
        assert!(
            runs.iter()
                .any(|run| run.text.contains("<script>bad</script>"))
        );
    }

    #[test]
    fn disabled_parsers_leave_source_text_unchanged() {
        let title = Title {
            text: "<bold>hello</bold> **world**".into(),
            ..Title::default()
        };
        assert_eq!(parse(&title)[0].text, title.text);
    }

    #[test]
    fn color_tags_keep_the_title_opacity() {
        let title = Title {
            text: "<color=red>text</color>".into(),
            color: [255, 255, 255, 128],
            parse_html: true,
            ..Title::default()
        };
        assert_eq!(parse(&title)[0].color, Color::rgba(255, 0, 0, 128));
    }

    #[test]
    fn html_and_markdown_flags_are_independent() {
        let title = Title {
            text: "<b>html</b> **markdown** _italic_".into(),
            bold: false,
            parse_html: true,
            ..Title::default()
        };
        let html = parse(&title);
        assert!(html.iter().any(|run| run.text == "html" && run.bold));
        assert!(
            html.iter()
                .any(|run| run.text.contains("**markdown** _italic_") && !run.bold)
        );
        let markdown = parse(&Title {
            parse_html: false,
            parse_markdown: true,
            ..title.clone()
        });
        assert!(
            markdown
                .iter()
                .any(|run| run.text.contains("<b>html</b>") && !run.bold)
        );
        assert!(
            markdown
                .iter()
                .any(|run| run.text == "markdown" && run.bold)
        );
        assert!(
            markdown
                .iter()
                .any(|run| run.text == "italic" && run.italic)
        );
        let both = parse(&Title {
            parse_markdown: true,
            ..title
        });
        assert!(both.iter().any(|run| run.text == "html" && run.bold));
        assert!(both.iter().any(|run| run.text == "markdown" && run.bold));
    }

    #[test]
    fn long_html_tags_and_nested_styles_restore_the_previous_style() {
        let title = Title {
            text: "<bold>A<italic>B</italic>C</bold><color=#00ff00>D</color>E".into(),
            bold: false,
            parse_html: true,
            ..Title::default()
        };
        let runs = parse(&title);
        assert!(
            runs.iter()
                .any(|run| run.text == "A" && run.bold && !run.italic)
        );
        assert!(
            runs.iter()
                .any(|run| run.text == "B" && run.bold && run.italic)
        );
        assert!(
            runs.iter()
                .any(|run| run.text == "C" && run.bold && !run.italic)
        );
        assert!(
            runs.iter()
                .any(|run| run.text == "D" && run.color == Color::rgb(0, 255, 0))
        );
        assert!(
            runs.iter()
                .any(|run| run.text == "E" && !run.bold && run.color == Color::rgb(255, 255, 255))
        );
    }

    #[test]
    fn markdown_italic_closes_only_with_its_opening_delimiter() {
        for (text, expected) in [
            ("_hello* world_", "hello* world"),
            ("*hello_ world*", "hello_ world"),
            ("_hello* world* again_", "hello* world* again"),
            ("*hello_ world_ again*", "hello_ world_ again"),
        ] {
            let title = Title {
                text: text.into(),
                bold: false,
                parse_markdown: true,
                ..Title::default()
            };
            assert_eq!(
                parse(&title),
                vec![Run {
                    text: expected.into(),
                    bold: false,
                    italic: true,
                    color: Color::rgb(255, 255, 255),
                }],
                "{text}"
            );
        }
    }

    #[test]
    fn malformed_markup_is_displayed_as_text() {
        let title = Title {
            text: "left **unclosed <color=#ééé>hello</color> last_name".into(),
            bold: false,
            parse_html: true,
            parse_markdown: true,
            ..Title::default()
        };
        let runs = parse(&title);
        assert_eq!(
            runs.iter().map(|run| run.text.as_str()).collect::<String>(),
            title.text
        );
    }
}
