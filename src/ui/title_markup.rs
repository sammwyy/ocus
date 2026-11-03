use crate::timeline::Title;
use creamui::Color;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Run {
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

pub(super) fn parse(title: &Title) -> Vec<Run> {
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
    let (mut md_bold, mut md_italic) = (false, false);
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
                && (md_italic || remaining[1..].contains(marker))
                && !(source[..index]
                    .chars()
                    .last()
                    .is_some_and(char::is_alphanumeric)
                    && remaining[1..]
                        .chars()
                        .next()
                        .is_some_and(char::is_alphanumeric))
            {
                md_italic = !md_italic;
                index += 1;
                continue;
            }
        }
        let ch = remaining.chars().next().unwrap();
        let bold = style.bold || md_bold;
        let italic = style.italic || md_italic;
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
