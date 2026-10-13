//! Presentation of headings and Conventional Commit markers inside Show messages.
use ratatui::style::{Color, Modifier, Style};
use std::{collections::HashMap, ops::Range};

pub type Ranges = Vec<(Range<usize>, Style)>;

pub fn breaking_style() -> Style {
    Style::default()
        .fg(Color::Rgb(255, 165, 0))
        .add_modifier(Modifier::BOLD)
}

/// Shared title styles for Log, Show, and subjects carried by Reflog actions.
pub fn subject_ranges(subject: &str, offset: usize) -> Ranges {
    let Some(kind) = crate::log_folds::subject_type_prefix(subject) else {
        return Vec::new();
    };
    let mut ranges = vec![(
        offset..offset + kind.len(),
        Style::default().fg(Color::LightBlue),
    )];
    if let Some(marker) = crate::log_folds::subject_breaking_marker(subject) {
        ranges.push((offset + marker..offset + marker + 1, breaking_style()));
    }
    ranges
}

fn footer_label(text: &str) -> Option<usize> {
    let colon = text.find(": ").map(|start| (start, start + 1));
    let hash = text.find(" #").map(|start| (start, start));
    let (token_end, label_end) = colon
        .into_iter()
        .chain(hash)
        .min_by_key(|(start, _)| *start)?;
    let label = &text[..token_end];
    let valid = label == "BREAKING CHANGE"
        || (!label.is_empty()
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            && label.bytes().any(|b| b.is_ascii_alphabetic()));
    if !valid || text.len() == token_end + 2 {
        return None;
    }
    Some(label_end)
}

fn breaking_marker(body: &str) -> Option<&'static str> {
    ["BREAKING CHANGE:", "BREAKING-CHANGE:"]
        .into_iter()
        .find(|marker| {
            body.strip_prefix(marker)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
        })
}

/// Footers form the final paragraph after the subject, which holds only
/// footer lines and their indented continuations.
fn footer_start(bodies: &[(usize, String)]) -> Option<usize> {
    let is_footer = |body: &str| breaking_marker(body).is_some() || footer_label(body).is_some();
    let end = bodies
        .iter()
        .rposition(|(_, body)| !body.trim().is_empty())?;
    let start = bodies[..end]
        .iter()
        .rposition(|(_, body)| body.trim().is_empty())?
        + 1;
    let (first, rest) = bodies[start..=end].split_first()?;
    (is_footer(&first.1)
        && rest
            .iter()
            .all(|(_, body)| is_footer(body) || body.starts_with(char::is_whitespace)))
    .then_some(first.0)
}

pub fn collect(text: &str) -> HashMap<usize, Ranges> {
    let message = crate::git::commit_message_range(text);
    let bodies: Vec<_> = text
        .lines()
        .enumerate()
        .skip(message.start)
        .take(message.len())
        .filter_map(|(source, raw)| {
            let plain = crate::ansi::plain(raw);
            plain
                .strip_prefix("    ")
                .map(|body| (source, body.to_owned()))
        })
        .collect();
    let footer = footer_start(&bodies);
    let mut result = HashMap::new();
    let mut paragraph_start = false;
    let mut fence = None;
    for (source, body) in &bodies {
        let (source, body) = (*source, body.as_str());
        if let Some(delimiter) = fence {
            if body.starts_with(delimiter) {
                fence = None;
            }
            paragraph_start = body.trim().is_empty();
            continue;
        }
        if body.starts_with("```") || body.starts_with("~~~") {
            fence = Some(if body.starts_with("```") {
                "```"
            } else {
                "~~~"
            });
            paragraph_start = false;
            continue;
        }
        if body.trim().is_empty() {
            paragraph_start = true;
            continue;
        }
        // Additional indentation denotes examples or continuation lines.
        if body.starts_with(char::is_whitespace) {
            paragraph_start = false;
            continue;
        }
        let mut ranges = Vec::new();
        if source == message.start {
            ranges = subject_ranges(body, 4);
        } else if let Some(marker) = breaking_marker(body) {
            ranges.push((4..4 + marker.len(), breaking_style()));
        } else if let Some(length) =
            footer_label(body).filter(|_| footer.is_some_and(|start| source >= start))
        {
            ranges.push((4..4 + length, Style::default().add_modifier(Modifier::BOLD)));
        } else if paragraph_start
            && body.ends_with(':')
            && body.len() <= 80
            && body.chars().any(char::is_alphabetic)
            && body[..body.len() - 1]
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_'))
        {
            ranges.push((
                4..4 + body.len(),
                Style::default().add_modifier(Modifier::BOLD),
            ));
        }
        if !ranges.is_empty() {
            result.insert(source, ranges);
        }
        paragraph_start = false;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_labels_and_breaking_markers_preserve_values_and_exclude_examples() {
        let text = "commit abcdef0\nAuthor: Test\n\n    Feat(api)!: new API\n    \n    Migration:\n    Read the guide.\n    \n    Tests:\n    All pass.\n    \n    ```text\n    BREAKING CHANGE: example only\n    Reviewed-by: example\n    ```\n    \n        BREAKING CHANGE: indented example\n    \n    BREAKING CHANGE:\n    The old API is removed.\n    \n    BREAKING-CHANGE: use the new API\n    Reviewed-by: Alice\n      continuation with café\n    Refs #123: more context\n    Signed-off-by: Bob\n\ndiff --git a/file b/file\n+BREAKING CHANGE: code\n+Reviewed-by: code\n";
        let styles = collect(text);
        let emphasized: Vec<_> = text
            .lines()
            .enumerate()
            .filter_map(|(source, line)| {
                styles.get(&source).map(|ranges| {
                    let rendered = crate::ansi::style_ranges(
                        crate::ansi::parse_line(line),
                        ranges.iter().cloned(),
                    );
                    assert_eq!(rendered.to_string(), line);
                    let marked: Vec<_> = rendered
                        .spans
                        .iter()
                        .filter(|span| span.style.add_modifier.contains(Modifier::BOLD))
                        .map(|span| (span.content.as_ref(), span.style.fg))
                        .collect();
                    (
                        line,
                        marked
                            .into_iter()
                            .map(|(content, color)| (content.to_owned(), color))
                            .collect::<Vec<_>>(),
                    )
                })
            })
            .collect();
        assert_eq!(
            emphasized,
            vec![
                (
                    "    Feat(api)!: new API",
                    vec![("!".into(), breaking_style().fg)]
                ),
                ("    Migration:", vec![("Migration:".into(), None)]),
                ("    Tests:", vec![("Tests:".into(), None)]),
                (
                    "    BREAKING CHANGE:",
                    vec![("BREAKING CHANGE:".into(), breaking_style().fg)]
                ),
                (
                    "    BREAKING-CHANGE: use the new API",
                    vec![("BREAKING-CHANGE:".into(), breaking_style().fg)]
                ),
                (
                    "    Reviewed-by: Alice",
                    vec![("Reviewed-by:".into(), None)]
                ),
                ("    Refs #123: more context", vec![("Refs".into(), None)]),
                (
                    "    Signed-off-by: Bob",
                    vec![("Signed-off-by:".into(), None)]
                ),
            ]
        );
    }

    #[test]
    fn footer_labels_only_apply_to_a_final_footer_paragraph() {
        let bold = |text: &str| -> Vec<usize> {
            let mut lines: Vec<_> = collect(text).into_keys().collect();
            lines.sort();
            lines
        };
        // Prose resembling footers stays plain; only the final paragraph is footers.
        let text = "commit abcdef0\n\n    fix: thing\n    \n    Note: prose begins here\n    Later: still prose\n    \n    See #12 for details.\n    \n    Refs #12\n    Signed-off-by: Bob\n";
        assert_eq!(bold(text), [2, 9, 10]);
        let text = "commit abcdef0\n\n    fix: thing\n    \n    Reviewed-by: Alice\n    but this is prose\n";
        assert_eq!(bold(text), [2]);
    }

    #[test]
    fn show_styling_keeps_links_and_ansi_colors_in_footer_values() {
        use crate::{
            app::{App, Mode},
            git::tests::{CurrentDirGuard, TestDirectory},
        };
        use ratatui::{backend::TestBackend, Terminal};
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        let mut app = App::new(vec![crate::log_format::tests::commit()]);
        app.mode = Mode::Show;
        app.show_text = "commit abcdef0\n\n    feat!: new API\n    \n    BREAKING CHANGE: old API removed\n    \n    Reviewed-by: \x1b[36mAlice\x1b[m\n    Refs #123\n".into();
        app.ensure_show_rows();
        let source = app
            .show_rows
            .iter()
            .find(|row| row.text.contains("Refs"))
            .unwrap()
            .source;
        app.show_links.insert(
            source,
            vec![crate::commit_links::Link {
                range: 9..13,
                target: crate::commit_links::Target::Issue(
                    "https://github.com/owner/repo/issues/123".into(),
                ),
            }],
        );
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let title = app
            .show_rows
            .iter()
            .position(|row| row.text.contains("feat!: new API"))
            .unwrap() as u16
            + app.show_row_origin;
        for column in 4..8 {
            assert_eq!(buffer[(column, title)].fg, Color::LightBlue);
        }
        assert_eq!(buffer[(8, title)].fg, breaking_style().fg.unwrap());
        assert!(buffer[(8, title)].modifier.contains(Modifier::BOLD));
        let reviewer = app
            .show_rows
            .iter()
            .position(|row| row.text.contains("Reviewed-by:"))
            .unwrap() as u16
            + app.show_row_origin;
        assert!(buffer[(4, reviewer)].modifier.contains(Modifier::BOLD));
        assert!(!buffer[(17, reviewer)].modifier.contains(Modifier::BOLD));
        assert_eq!(buffer[(17, reviewer)].fg, Color::Cyan);
        let reference = source as u16 + app.show_row_origin;
        assert!(buffer[(4, reference)].modifier.contains(Modifier::BOLD));
        assert!(buffer[(9, reference)]
            .modifier
            .contains(Modifier::UNDERLINED));
        assert!(!buffer[(9, reference)].modifier.contains(Modifier::BOLD));
        assert_eq!(app.visible_show_links.len(), 4);
    }
}
