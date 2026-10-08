//! Commit and issue references in the message portion of Show, before any patch or notes.
use std::{collections::HashMap, ops::Range};

use ratatui::{
    style::Style,
    text::{Line, Span},
};

#[derive(Clone, Debug)]
pub struct Link {
    pub range: Range<usize>,
    pub target: Target,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Commit(String),
    Issue(String),
}

pub fn collect(text: &str) -> HashMap<usize, Vec<Link>> {
    let mut resolved = HashMap::new();
    let mut issue_repository = None;
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
            let target = if prefixed
                && (1..=6).contains(&token.len())
                && token.bytes().all(|b| b.is_ascii_digit())
            {
                // Short numeric references are issues even if they match a commit.
                // A qualified owner/repo#number must not point at the local repository.
                let standalone = plain[..start - 1]
                    .chars()
                    .next_back()
                    .is_none_or(|c| !c.is_alphanumeric() && !matches!(c, '_' | '/' | '.' | '-'));
                if standalone {
                    issue_repository
                        .get_or_insert_with(crate::git::github_repository)
                        .as_ref()
                        .map(|repo| Target::Issue(format!("{repo}/issues/{token}")))
                } else {
                    None
                }
            } else if (minimum..=64).contains(&token.len())
                && token
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                resolved
                    .entry(token.to_owned())
                    .or_insert_with(|| crate::git::resolve_commit_reference(token))
                    .as_ref()
                    .map(|hash| Target::Commit(hash.clone()))
            } else {
                None
            };
            if let Some(target) = target {
                links.entry(source).or_insert_with(Vec::new).push(Link {
                    range: start - usize::from(prefixed)..start + token.len(),
                    target,
                });
            }
            start += word.len();
        }
    }
    links
}

pub type BrowserResult = std::sync::mpsc::Receiver<Result<(), String>>;

/// Hand the URL to the OS without a shell or blocking the TUI on the browser.
pub fn open_issue(url: &str) -> Result<BrowserResult, String> {
    use std::process::{Command, Stdio};
    #[cfg(target_os = "macos")]
    let mut command = Command::new("open");
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("rundll32.exe");
        command.arg("url.dll,FileProtocolHandler");
        command
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut command = Command::new("xdg-open");
    let child = command
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("Could not open browser: {error}"))?;
    let (sender, receiver) = std::sync::mpsc::channel();
    // Some Linux openers wait for the browser to close. Reap them in the background.
    std::thread::spawn(move || {
        let result = child
            .wait_with_output()
            .map_err(|error| format!("Could not open browser: {error}"))
            .and_then(|output| {
                if output.status.success() {
                    Ok(())
                } else {
                    let error = String::from_utf8_lossy(&output.stderr);
                    let error = error.trim();
                    Err(format!(
                        "Could not open browser: {}",
                        if error.is_empty() {
                            output.status.to_string()
                        } else {
                            error.to_owned()
                        }
                    ))
                }
            });
        let _ = sender.send(result);
    });
    Ok(receiver)
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
            assert_eq!(link.target, Target::Commit(hash.clone()));
            assert!(text.lines().nth(3).unwrap()[link.range.clone()].starts_with(short));
        }
        // Decorations may split a hash into spans; all parts keep the link style.
        let line = crate::ansi::parse_line(&format!("{short}\x1b[31m{}", &hash[7..]));
        let styled = style_links(
            line,
            &[Link {
                range: 0..hash.len(),
                target: Target::Commit(hash.clone()),
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
        let hash = loop {
            let hash = commit("target");
            if hash[..4].bytes().any(|b| b.is_ascii_lowercase()) {
                break hash;
            }
        };
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
            assert_eq!(link.target, Target::Commit(hash.clone()));
            assert_eq!(&message[link.range.clone()], token);
        }
    }

    #[test]
    fn short_numeric_references_are_issues_and_seven_digits_can_be_commits() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        // Exercise the ambiguity with a real commit whose prefix is entirely numeric.
        let hash = loop {
            let hash = commit("numeric target");
            if hash[..7].bytes().all(|b| b.is_ascii_digit()) {
                break hash;
            }
        };
        let issue_tokens = [
            "#1".to_owned(),
            "#22".into(),
            "#333".into(),
            "#4444".into(),
            "#55555".into(),
            "#999999".into(),
            format!("#{}", &hash[..6]),
        ];
        let line = format!("    Fix {} and #{}.", issue_tokens.join(", "), &hash[..7]);
        let text = format!(
            "commit {hash}\n\n{line}\n    #12abc_thing #ABCDEF #1234567z owner/another#42\n"
        );
        let without_remote = collect(&text);
        assert_eq!(
            without_remote[&2].len(),
            1,
            "short numbers never fall back to commits"
        );
        assert_eq!(without_remote[&2][0].target, Target::Commit(hash.clone()));
        git(&["remote", "add", "origin", "git@github.com:owner/repo.git"]);
        let links = collect(&text);
        assert_eq!(links.len(), 1);
        assert_eq!(links[&2].len(), issue_tokens.len() + 1);
        for (link, token) in links[&2].iter().zip(issue_tokens) {
            assert_eq!(&line[link.range.clone()], token);
            assert_eq!(
                link.target,
                Target::Issue(format!(
                    "https://github.com/owner/repo/issues/{}",
                    &token[1..]
                ))
            );
        }
        assert_eq!(links[&2].last().unwrap().target, Target::Commit(hash));
    }

    #[cfg(unix)]
    #[test]
    fn issue_click_opens_browser_and_reports_failures_without_leaving_show() {
        use std::{
            env,
            os::unix::fs::PermissionsExt,
            process::Command,
            time::{Duration, Instant},
        };
        const CHILD: &str = "GLOG_TEST_ISSUE_BROWSER_CHILD";
        const CAPTURE: &str = "GLOG_TEST_ISSUE_BROWSER_CAPTURE";
        if let Ok(scenario) = env::var(CHILD) {
            let mut app = git::load_show_app(&[]).unwrap();
            let hash = app.commits[app.selected].hash.clone();
            let mut terminal = Terminal::new(TestBackend::new(17, 12)).unwrap();
            terminal
                .draw(|frame| crate::ui::draw(frame, &mut app))
                .unwrap();
            app.show_cursor = app
                .show_rows
                .iter()
                .position(|row| row.text.contains("#340"))
                .unwrap();
            app.show_scroll = Some(crate::app::ShowScroll::Cursor);
            terminal
                .draw(|frame| crate::ui::draw(frame, &mut app))
                .unwrap();
            let expected = "https://github.com/owner/repo/issues/340";
            let (rect, _) = app
                .visible_show_links
                .iter()
                .find(|(_, target)| target == &Target::Issue(expected.into()))
                .unwrap()
                .clone();
            let before = (app.show_text.clone(), app.show_cursor, app.show_offset);
            input::handle(
                Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: rect.x,
                    row: rect.y,
                    modifiers: KeyModifiers::NONE,
                }),
                &mut app,
            );
            let capture = env::var_os(CAPTURE).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                app.poll_browser();
                if std::path::Path::new(&capture).exists()
                    && (scenario == "success" || app.status.is_some())
                {
                    break;
                }
                assert!(Instant::now() < deadline, "browser opener did not finish");
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(std::fs::read_to_string(capture).unwrap().trim(), expected);
            assert_eq!(app.commits[app.selected].hash, hash);
            assert_eq!(app.mode, Mode::Show);
            assert_eq!(
                (app.show_text.clone(), app.show_cursor, app.show_offset),
                before
            );
            assert!(!app.has_reference_back());
            if scenario == "failure" {
                assert!(app.status.unwrap().contains("test browser failure"));
            } else {
                assert!(app.status.is_none());
            }
            return;
        }
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        git(&[
            "remote",
            "add",
            "origin",
            "https://github.com/owner/repo.git",
        ]);
        commit("fix a problem\n\n界 café: closes #340.");
        let bin = directory.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let opener = bin.join(if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        });
        std::fs::write(
            &opener,
            r##"#!/bin/sh
printf '%s\n' "$1" > "$GLOG_TEST_ISSUE_BROWSER_CAPTURE"
if [ "$GLOG_TEST_ISSUE_BROWSER_CHILD" = failure ]; then
    echo 'test browser failure' >&2
    exit 1
fi
"##,
        )
        .unwrap();
        std::fs::set_permissions(opener, std::fs::Permissions::from_mode(0o755)).unwrap();
        let original_path = env::var_os("PATH").unwrap();
        let path =
            env::join_paths(std::iter::once(bin).chain(env::split_paths(&original_path))).unwrap();
        for scenario in ["success", "failure"] {
            let output = Command::new(env::current_exe().unwrap())
                .args(["--exact", "commit_links::tests::issue_click_opens_browser_and_reports_failures_without_leaving_show", "--nocapture"])
                .env("PATH", &path).env(CHILD, scenario)
                .env(CAPTURE, directory.path().join(scenario))
                .output().unwrap();
            assert!(output.status.success(), "{output:?}");
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
            for (rect, _) in &app.visible_show_links {
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
                .visible_show_links
                .iter()
                .find(|(_, target)| target == &Target::Commit(second.clone()))
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
