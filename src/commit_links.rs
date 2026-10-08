//! Commit references in the message portion of Show, before any patch or notes.
use std::{collections::HashMap, ops::Range};

use ratatui::{
    style::Style,
    text::{Line, Span},
};

#[derive(Clone, Debug)]
pub struct Link {
    pub range: Range<usize>,
    pub hash: String,
}

pub fn collect(text: &str) -> HashMap<usize, Vec<Link>> {
    let mut resolved = HashMap::new();
    let mut links = HashMap::new();
    let message = crate::git::commit_message_range(text);
    for (source, line) in text
        .lines()
        .enumerate()
        .skip(message.start)
        .take(message.len())
    {
        let plain = crate::ansi::plain(line);
        let mut start = 0;
        // Treat an entire word as one token so identifiers containing hex aren't links.
        for word in plain.split_inclusive(|c: char| !c.is_alphanumeric() && c != '_') {
            let token = word.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '_');
            let prefixed = plain[..start].ends_with('#');
            let minimum = if prefixed { 4 } else { 7 };
            if (minimum..=64).contains(&token.len())
                && token
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                let hash = resolved
                    .entry(token.to_owned())
                    .or_insert_with(|| crate::git::resolve_commit_reference(token));
                if let Some(hash) = hash {
                    links.entry(source).or_insert_with(Vec::new).push(Link {
                        range: start - usize::from(prefixed)..start + token.len(),
                        hash: hash.clone(),
                    });
                }
            }
            start += word.len();
        }
    }
    links
}

/// Preserve existing ANSI styling while decorating the byte ranges of links.
pub fn style_links(
    mut line: Line<'static>,
    links: &[Link],
    mut style: impl FnMut(&Link) -> Style,
) -> Line<'static> {
    let mut spans = Vec::new();
    let mut offset = 0;
    for span in line.spans {
        let end = offset + span.content.len();
        let mut cursor = 0;
        for link in links
            .iter()
            .filter(|link| link.range.start < end && link.range.end > offset)
        {
            let start = link.range.start.max(offset) - offset;
            let stop = link.range.end.min(end) - offset;
            if cursor < start {
                spans.push(Span::styled(
                    span.content[cursor..start].to_owned(),
                    span.style,
                ));
            }
            spans.push(Span::styled(
                span.content[start..stop].to_owned(),
                span.style.patch(style(link)),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::Mode,
        git::{
            self,
            tests::{CurrentDirGuard, TestDirectory},
        },
        input,
    };
    use crossterm::event::{
        Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use ratatui::{backend::TestBackend, Terminal};

    fn git(args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn commit(message: &str) -> String {
        git(&["add", "."]);
        git(&["commit", "--allow-empty", "-qm", message]);
        git(&["rev-parse", "HEAD"])
    }

    #[test]
    fn only_resolved_commit_hashes_in_messages_become_links() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        std::fs::write("file", "contents\n").unwrap();
        let hash = commit("target");
        let short = &hash[..7];
        let blob = git(&["rev-parse", "HEAD:file"]);
        let text = format!("commit {hash}\nAuthor: Test\n\n    Fix ({short}), reverting {hash}.\n    {short}_identifier identifier{short} {blob} 0000000\n\nNotes:\n    {hash}\ndiff --git a/file b/file\n    {hash}\n");
        let links = collect(&text);
        assert_eq!(
            links.len(),
            1,
            "exclude headers, notes, patches, blobs and identifiers"
        );
        assert_eq!(links[&3].len(), 2);
        for link in &links[&3] {
            assert_eq!(link.hash, hash);
            assert!(text.lines().nth(3).unwrap()[link.range.clone()].starts_with(short));
        }
        // Decorations may split a hash into spans; all parts keep the link style.
        let line = crate::ansi::parse_line(&format!("{short}\x1b[31m{}", &hash[7..]));
        let styled = style_links(
            line,
            &[Link {
                range: 0..hash.len(),
                hash: hash.clone(),
            }],
            |_| Style::default().add_modifier(ratatui::style::Modifier::UNDERLINED),
        );
        assert_eq!(styled.to_string(), hash);
        assert!(styled.spans.iter().all(|span| span
            .style
            .add_modifier
            .contains(ratatui::style::Modifier::UNDERLINED)));
    }

    #[test]
    fn only_lowercase_hashes_and_prefixed_short_hashes_are_linked() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        let hash = commit("target");
        let uppercase = hash.to_ascii_uppercase();
        let mixed: String = hash
            .chars()
            .enumerate()
            .map(|(index, c)| {
                if index % 2 == 0 {
                    c.to_ascii_uppercase()
                } else {
                    c
                }
            })
            .collect();
        let tokens = [
            format!("#{}", &hash[..4]),
            format!("#{}", &hash[..5]),
            format!("#{}", &hash[..6]),
            hash.clone(),
        ];
        let message = format!("    See {}.", tokens.join(", "));
        let text = format!(
            "commit {hash}\n\n{message}\n    {} {} {} #{} 2026 {uppercase} {mixed} #{uppercase} #{mixed}\n",
            &hash[..4],
            &hash[..5],
            &hash[..6],
            &hash[..3]
        );
        let links = collect(&text);
        assert_eq!(
            links.len(),
            1,
            "bare short hashes, prefixes below four characters, and uppercase or mixed-case hashes stay plain"
        );
        assert_eq!(links[&2].len(), tokens.len());
        for (link, token) in links[&2].iter().zip(tokens) {
            assert_eq!(link.hash, hash);
            assert_eq!(&message[link.range.clone()], token);
        }
    }

    #[test]
    fn wrapped_clicks_follow_nested_references_and_back_restores_the_original_view() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        std::fs::write("Cargo.lock", "first\n").unwrap();
        let first = commit("first");
        let second = commit(&format!("second\n\nSee {}.", &first[..9]));
        std::fs::write("Cargo.lock", "first\nsecond\n").unwrap();
        std::fs::write("only-third.txt", "new\n").unwrap();
        let third = commit(&format!(
            "third\n\n界 café: review {} and {}.\n\nMore context.",
            &second[..12],
            first
        ));
        for width in [16, 29, 80] {
            // References must work even when Log and Show are filtered to a different file.
            let mut app =
                git::load_show_app(&[third.clone(), "--".into(), "only-third.txt".into()]).unwrap();
            app.watch = true;
            app.toggle_show_stat();
            app.show_cursor = app.show_rows.iter().position(|row| row.summary).unwrap();
            app.toggle_show_file();
            let mut terminal = Terminal::new(TestBackend::new(width, 9)).unwrap();
            terminal
                .draw(|frame| crate::ui::draw(frame, &mut app))
                .unwrap();
            app.show_cursor = app
                .show_rows
                .iter()
                .position(|row| row.text.contains("界 café"))
                .unwrap();
            app.show_offset = app.show_row_starts[app.show_cursor] + 1;
            app.search = Some(second[..12].to_owned());
            terminal
                .draw(|frame| crate::ui::draw(frame, &mut app))
                .unwrap();
            for (rect, _) in &app.visible_commit_links {
                let cell = &terminal.backend().buffer()[(rect.x, rect.y)];
                assert!(
                    cell.symbol().bytes().all(|byte| byte.is_ascii_hexdigit()),
                    "hit target must cover the displayed hash: {cell:?}"
                );
                assert!(cell.modifier.contains(ratatui::style::Modifier::UNDERLINED));
            }
            let origin_text = app.show_text.clone();
            let origin_rows: Vec<_> = app.show_rows.iter().map(|row| row.text.clone()).collect();
            let origin_cursor = app.show_cursor;
            let origin_offset = app.show_offset;
            let origin_pending = app.pending_history.clone();
            let (rect, _) = app
                .visible_commit_links
                .iter()
                .find(|(_, hash)| hash == &second)
                .unwrap()
                .clone();
            input::handle(
                Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: rect.x,
                    row: rect.y,
                    modifiers: KeyModifiers::NONE,
                }),
                &mut app,
            );
            assert_eq!(app.commits[app.selected].hash, second);
            assert_eq!(app.mode, Mode::Show);
            assert!(app.show_paths.is_empty());
            assert!(app.show_stat);
            assert!(app.has_reference_back());
            terminal
                .draw(|frame| crate::ui::draw(frame, &mut app))
                .unwrap();
            app.follow_reference(&first);
            assert_eq!(app.commits[app.selected].hash, first);
            let back = Event::Key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
            input::handle(back.clone(), &mut app);
            assert_eq!(app.commits[app.selected].hash, second);
            // Invalid references leave both the view and return path intact.
            app.follow_reference("0000000000000000000000000000000000000000");
            assert_eq!(app.commits[app.selected].hash, second);
            input::handle(back, &mut app);
            assert_eq!(app.commits[app.selected].hash, third);
            assert_eq!(app.show_text, origin_text);
            assert_eq!(
                app.show_rows
                    .iter()
                    .map(|row| row.text.clone())
                    .collect::<Vec<_>>(),
                origin_rows
            );
            assert_eq!(app.show_cursor, origin_cursor);
            assert_eq!(app.show_offset, origin_offset);
            assert_eq!(app.pending_history, origin_pending);
            assert_eq!(app.search.as_deref(), Some(&second[..12]));
            assert_eq!(app.show_paths, vec!["only-third.txt"]);
            assert!(app.watch);
            assert!(!app.has_reference_back());
        }
    }
}
