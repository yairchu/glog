use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

pub fn normalized_line(input: &str) -> Line<'static> {
    let parsed = parse_line(input);
    if parsed
        .spans
        .iter()
        .all(|span| span.content.chars().all(char::is_whitespace))
    {
        Line::default()
    } else {
        parsed
    }
}

pub fn parse_line(input: &str) -> Line<'static> {
    let mut spans = Vec::new();
    let mut style = Style::default();
    let mut rest = input;
    while let Some(start) = rest.find("\x1b[") {
        if start > 0 {
            spans.push(Span::styled(rest[..start].to_owned(), style));
        }
        let sequence = &rest[start + 2..];
        let Some(end) = sequence.find('m') else {
            spans.push(Span::styled(rest[start..].to_owned(), style));
            rest = "";
            break;
        };
        apply_sgr(&mut style, &sequence[..end]);
        rest = &sequence[end + 1..];
    }
    if !rest.is_empty() {
        spans.push(Span::styled(rest.to_owned(), style));
    }
    Line::from(spans)
}

/// Git leaves tabs to the pager, whose default stops `less` and terminals
/// place every 8 columns.
const TAB_WIDTH: usize = 8;

/// Expand tabs and replace control characters, keeping each span's style.
pub fn display_spans(spans: Vec<Span<'static>>) -> Vec<Span<'static>> {
    let mut column = 0;
    let mut output = Vec::with_capacity(spans.len());
    for span in spans {
        let content = span.content.strip_suffix('\r').unwrap_or(&span.content);
        let mut text = String::with_capacity(content.len());
        for (index, segment) in content.split('\t').enumerate() {
            if index > 0 {
                let spaces = TAB_WIDTH - column % TAB_WIDTH;
                text.extend(std::iter::repeat_n(' ', spaces));
                column += spaces;
            }
            let start = text.len();
            text.extend(segment.chars().map(|ch| {
                if ch.is_control() {
                    char::REPLACEMENT_CHARACTER
                } else {
                    ch
                }
            }));
            // Measure rendered text together so combining marks and emoji
            // sequences use the same display width as the terminal UI.
            column += Span::raw(&text[start..]).width();
        }
        output.push(Span::styled(text, span.style));
    }
    output
}

pub fn plain(input: &str) -> String {
    parse_line(input)
        .spans
        .into_iter()
        .map(|span| span.content.into_owned())
        .collect()
}

/// Patch non-overlapping, ordered byte ranges while preserving existing ANSI styles.
pub fn style_ranges(
    mut line: Line<'static>,
    ranges: impl IntoIterator<Item = (std::ops::Range<usize>, Style)>,
) -> Line<'static> {
    let ranges: Vec<_> = ranges.into_iter().collect();
    let mut spans = Vec::new();
    let mut offset = 0;
    for span in line.spans {
        let end = offset + span.content.len();
        let mut cursor = 0;
        for (range, style) in ranges
            .iter()
            .filter(|(range, _)| range.start < end && range.end > offset)
        {
            let start = range.start.max(offset) - offset;
            let stop = range.end.min(end) - offset;
            if cursor < start {
                spans.push(Span::styled(
                    span.content[cursor..start].to_owned(),
                    span.style,
                ));
            }
            spans.push(Span::styled(
                span.content[start..stop].to_owned(),
                span.style.patch(*style),
            ));
            cursor = stop;
        }
        if cursor < span.content.len() {
            spans.push(Span::styled(span.content[cursor..].to_owned(), span.style));
        }
        offset = end;
    }
    line.spans = spans;
    line
}

fn apply_sgr(style: &mut Style, params: &str) {
    let values: Vec<u16> = if params.is_empty() {
        vec![0]
    } else {
        params.split(';').filter_map(|v| v.parse().ok()).collect()
    };
    let mut i = 0;
    while i < values.len() {
        match values[i] {
            0 => *style = Style::default(),
            1 => *style = style.add_modifier(Modifier::BOLD),
            2 => *style = style.add_modifier(Modifier::DIM),
            3 => *style = style.add_modifier(Modifier::ITALIC),
            4 => *style = style.add_modifier(Modifier::UNDERLINED),
            7 => *style = style.add_modifier(Modifier::REVERSED),
            22 => *style = style.remove_modifier(Modifier::BOLD | Modifier::DIM),
            23 => *style = style.remove_modifier(Modifier::ITALIC),
            24 => *style = style.remove_modifier(Modifier::UNDERLINED),
            27 => *style = style.remove_modifier(Modifier::REVERSED),
            30..=37 => style.fg = Some(basic(values[i] - 30, false)),
            39 => style.fg = None,
            40..=47 => style.bg = Some(basic(values[i] - 40, false)),
            49 => style.bg = None,
            90..=97 => style.fg = Some(basic(values[i] - 90, true)),
            100..=107 => style.bg = Some(basic(values[i] - 100, true)),
            38 | 48 if values.get(i + 1) == Some(&5) && values.get(i + 2).is_some() => {
                let color = Color::Indexed(values[i + 2] as u8);
                if values[i] == 38 {
                    style.fg = Some(color)
                } else {
                    style.bg = Some(color)
                }
                i += 2;
            }
            38 | 48 if values.get(i + 1) == Some(&2) && values.get(i + 4).is_some() => {
                let color = Color::Rgb(
                    values[i + 2] as u8,
                    values[i + 3] as u8,
                    values[i + 4] as u8,
                );
                if values[i] == 38 {
                    style.fg = Some(color)
                } else {
                    style.bg = Some(color)
                }
                i += 4;
            }
            _ => {}
        }
        i += 1;
    }
}

fn basic(index: u16, bright: bool) -> Color {
    match (index, bright) {
        (0, false) => Color::Black,
        (1, false) => Color::Red,
        (2, false) => Color::Green,
        (3, false) => Color::Yellow,
        (4, false) => Color::Blue,
        (5, false) => Color::Magenta,
        (6, false) => Color::Cyan,
        (7, false) => Color::Gray,
        (0, true) => Color::DarkGray,
        (1, true) => Color::LightRed,
        (2, true) => Color::LightGreen,
        (3, true) => Color::LightYellow,
        (4, true) => Color::LightBlue,
        (5, true) => Color::LightMagenta,
        (6, true) => Color::LightCyan,
        _ => Color::White,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strips_and_styles_ansi() {
        let line = parse_line("a\x1b[31mred\x1b[0mz");
        assert_eq!(
            line.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>(),
            "aredz"
        );
        assert_eq!(line.spans[1].style.fg, Some(Color::Red));
    }

    #[test]
    fn normalizes_whitespace_only_lines() {
        let parsed: Vec<_> = "message\n    \nbody".lines().map(normalized_line).collect();
        assert_eq!(parsed.len(), 3);
        assert!(parsed[1].spans.is_empty());
    }
}
