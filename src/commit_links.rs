//! Commit references, issue references and URLs in the message portion of
//! Show, before any patch or notes.
use std::{collections::HashMap, ops::Range};

#[derive(Clone, Debug)]
pub struct Link {
    pub range: Range<usize>,
    pub target: Target,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Commit(String),
    /// A `base..head` range of full hashes, browsed as a Log.
    Range(String),
    Url(String),
}

/// What a URL to this repository names in the local history.
enum Local<'a> {
    Commit(&'a str),
    Range(&'a str, &'a str),
}

#[cfg(test)]
pub fn collect(text: &str, repository: &mut Option<Option<String>>) -> HashMap<usize, Vec<Link>> {
    collect_at(text, repository, None)
}

/// `repository` caches the GitHub repository for `git_repository`; look it up
/// only once an issue reference or URL appears.
pub fn collect_at(
    text: &str,
    repository: &mut Option<Option<String>>,
    git_repository: Option<&crate::git::Repository>,
) -> HashMap<usize, Vec<Link>> {
    enum Candidate<'a> {
        Commit(&'a str),
        Issue(&'a str),
        Url(&'a str, Option<Local<'a>>),
    }
    let mut candidates = Vec::new();
    let message = crate::git::commit_message_range(text);
    let lines: Vec<_> = text
        .lines()
        .skip(message.start)
        .take(message.len())
        .map(crate::ansi::plain)
        .collect();
    for (index, plain) in lines.iter().enumerate() {
        let urls = urls(plain);
        let mut start = 0;
        // Treat an entire word as one token so identifiers containing hex aren't links.
        for word in plain.split_inclusive(|c: char| !c.is_alphanumeric() && c != '_') {
            // A URL is one link, whatever hashes or numbers its path contains.
            let end = start + word.len();
            if let Some(url) = urls.iter().find(|url| url.start < end && start < url.end) {
                // The first word overlapping a URL adds it, even if it starts mid-word.
                if url.start >= start {
                    let url_text = &plain[url.clone()];
                    let local = repository
                        .get_or_insert_with(|| crate::git::github_repository_at(git_repository))
                        .as_deref()
                        .and_then(|repository| local(url_text, repository));
                    candidates.push((
                        message.start + index,
                        url.clone(),
                        Candidate::Url(url_text, local),
                    ));
                }
                start += word.len();
                continue;
            }
            let token = word.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '_');
            let prefixed = plain[..start].ends_with('#');
            // `repo#abcd123`, `repo@abc1234` and paths such as
            // `owner/repo/commit/abc1234` likely refer to other repositories.
            let qualified = {
                let mut before = plain[..start].chars().rev();
                matches!(before.next(), Some('#' | '@' | '/'))
                    && before.next().is_some_and(char::is_alphanumeric)
            };
            let minimum = if prefixed { 4 } else { 7 };
            // #rrggbb is far more likely a color than an issue or an abbreviated hash.
            let candidate = if prefixed && token.len() == 6 {
                None
            } else if prefixed
                && (1..=5).contains(&token.len())
                && token.bytes().all(|b| b.is_ascii_digit())
            {
                // Short numeric references are issues even if they match a commit.
                // A qualified owner/repo#number must not point at the local repository,
                // and &#number; is an HTML character reference.
                let standalone = plain[..start - 1].chars().next_back().is_none_or(|c| {
                    !c.is_alphanumeric() && !matches!(c, '_' | '/' | '.' | '-' | '&')
                });
                standalone.then_some(Candidate::Issue(token))
            } else if !qualified && (minimum..=64).contains(&token.len()) && is_hash(token) {
                Some(Candidate::Commit(token))
            } else {
                None
            };
            if let Some(candidate) = candidate {
                let range = start - usize::from(prefixed)..start + token.len();
                candidates.push((message.start + index, range, candidate));
            }
            start += word.len();
        }
    }
    let resolved = crate::git::resolve_commits(
        candidates
            .iter()
            .flat_map(|(_, _, candidate)| match candidate {
                Candidate::Commit(hash) | Candidate::Url(_, Some(Local::Commit(hash))) => {
                    vec![*hash]
                }
                Candidate::Url(_, Some(Local::Range(base, head))) => vec![*base, *head],
                Candidate::Issue(_) | Candidate::Url(_, None) => Vec::new(),
            }),
        git_repository,
    );
    let commit = |hash: &str| resolved.get(hash).cloned();
    let mut links = HashMap::new();
    for (source, range, candidate) in candidates {
        let target = match candidate {
            Candidate::Commit(hash) => commit(hash).map(Target::Commit),
            Candidate::Issue(number) => repository
                .get_or_insert_with(|| crate::git::github_repository_at(git_repository))
                .as_ref()
                .map(|repo| Target::Url(format!("{repo}/issues/{number}"))),
            Candidate::Url(url, local) => Some(
                match local {
                    Some(Local::Commit(hash)) => commit(hash).map(Target::Commit),
                    Some(Local::Range(base, head)) => resolved
                        .get(base)
                        .zip(resolved.get(head))
                        .map(|(base, head)| Target::Range(format!("{base}..{head}"))),
                    None => None,
                }
                // Revisions missing locally can still be seen on GitHub.
                .unwrap_or_else(|| Target::Url(url.to_owned())),
            ),
        };
        if let Some(target) = target {
            links
                .entry(source)
                .or_insert_with(Vec::new)
                .push(Link { range, target });
        }
    }
    links
}

fn is_hash(token: &str) -> bool {
    token
        .bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The byte ranges of `http` and `https` URLs, leaving out punctuation that
/// more likely ends the surrounding sentence.
fn urls(line: &str) -> Vec<Range<usize>> {
    let mut urls: Vec<Range<usize>> = Vec::new();
    for (start, _) in line.match_indices("http") {
        let rest = &line[start..];
        if urls.last().is_some_and(|url| url.end > start)
            || line[..start]
                .chars()
                .next_back()
                .is_some_and(char::is_alphanumeric)
            || !(rest.starts_with("https://") || rest.starts_with("http://"))
        {
            continue;
        }
        // A closing bracket ends the URL unless it closes one opened within it,
        // as in Wikipedia's parentheses, so Markdown links end at their `)`.
        let mut open = Vec::new();
        let end = rest
            .find(|c: char| match c {
                '(' | '[' | '{' => {
                    open.push(c);
                    false
                }
                ')' | ']' | '}' => open.pop() != Some(matching_open(c)),
                _ => c.is_whitespace() || matches!(c, '<' | '>' | '"' | '`'),
            })
            .unwrap_or(rest.len());
        let url = rest[..end].trim_end_matches(['.', ',', ':', ';', '!', '?', '\'', '*']);
        if url
            .split_once("://")
            .is_some_and(|(_, host)| !host.is_empty())
        {
            urls.push(start..start + url.len());
        }
    }
    urls
}

fn matching_open(close: char) -> char {
    match close {
        ')' => '(',
        ']' => '[',
        _ => '{',
    }
}

/// The commit or comparison a GitHub URL names within `repository`.
fn local<'a>(url: &'a str, repository: &str) -> Option<Local<'a>> {
    fn without_scheme(url: &str) -> Option<&str> {
        url.strip_prefix("https://")
            .or_else(|| url.strip_prefix("http://"))
    }
    let path = without_scheme(url)?;
    let path = path.strip_prefix("www.").unwrap_or(path);
    let repository = without_scheme(repository)?;
    // GitHub owners and repositories are case-insensitive.
    let path = path
        .get(..repository.len())
        .filter(|prefix| prefix.eq_ignore_ascii_case(repository))
        .and_then(|_| path[repository.len()..].strip_prefix('/'))?;
    let path = path.split(['?', '#']).next()?.trim_end_matches('/');
    if let Some(hash) = path.strip_prefix("commit/") {
        return (hash.len() >= 4 && is_hash(hash)).then_some(Local::Commit(hash));
    }
    let (base, head) = path
        .strip_prefix("compare/")?
        .split_once("...")
        .or_else(|| path.strip_prefix("compare/")?.split_once(".."))?;
    // Branch names would resolve to local branches, which may differ from GitHub's.
    [base, head]
        .iter()
        .all(|hash| hash.len() >= 4 && is_hash(hash))
        .then_some(Local::Range(base, head))
}

pub type BrowserResult = std::sync::mpsc::Receiver<Result<(), String>>;

/// GitHub identifies a file's diff by the SHA-256 of its repository-relative
/// path, including for renamed files (the destination) and deleted files.
pub fn diff_anchor(path: &[u8], position: Option<crate::hunk::Position>) -> String {
    use sha2::{Digest, Sha256};
    let mut anchor = format!("#diff-{:x}", Sha256::digest(path));
    if let Some(crate::hunk::Position {
        row: crate::hunk::Row::Line { result, .. },
        old,
        new,
    }) = position
    {
        // GitHub's two-sided diff cannot address combined parent columns.
        if let [old] = old.as_slice() {
            let (side, line) = if result { ('R', new) } else { ('L', *old) };
            anchor.push_str(&format!("{side}{line}"));
        }
    }
    anchor
}

// A browser that xdg-open launches inherits its stderr, so a pipe there
// would stay open for the browser's lifetime and break once glog exits.
const XDG_OPEN: bool = cfg!(not(any(target_os = "macos", target_os = "windows")));

/// Why an opener failed, when it gave no reason of its own.
fn exit_reason(status: std::process::ExitStatus) -> String {
    match status.code() {
        // As xdg-open documents its exit codes.
        Some(3) if XDG_OPEN => "no tool found to open URLs".into(),
        Some(4) if XDG_OPEN => "the browser failed to open the URL".into(),
        _ => status.to_string(),
    }
}

/// Hand the URL to the OS without a shell or blocking the TUI on the browser.
pub fn open_url(url: &str) -> Result<BrowserResult, String> {
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
        .stderr(if XDG_OPEN {
            Stdio::null()
        } else {
            Stdio::piped()
        })
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
                            exit_reason(output.status)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::Mode,
        git::{
            self,
            tests::{git, CurrentDirGuard, TestDirectory},
        },
        input,
    };
    use crossterm::event::{
        Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use ratatui::{backend::TestBackend, Terminal};

    fn commit(message: &str) -> String {
        git(&["add", "."]);
        git(&["commit", "--allow-empty", "-qm", message]);
        git(&["rev-parse", "HEAD"])
    }

    #[test]
    fn configured_pretty_formats_keep_show_links_and_message_styles() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        git(&[
            "remote",
            "add",
            "origin",
            "https://github.com/owner/repo.git",
        ]);
        let target = commit("target");
        commit(&format!(
            "feat!: follow {}\n\nMigration:\nUse the updated API.\n\nBREAKING CHANGE: old API removed\nRefs #123",
            &target[..7]
        ));

        for pretty in ["oneline", "short", "format:%s"] {
            git(&["config", "format.pretty", pretty]);
            let app = git::load_show_app(&[]).unwrap();
            let targets: Vec<_> = app
                .show_links
                .values()
                .flatten()
                .map(|link| link.target.clone())
                .collect();
            assert!(
                targets.contains(&Target::Commit(target.clone())),
                "commit link missing with format.pretty={pretty}"
            );
            assert!(
                targets.contains(&Target::Url(
                    "https://github.com/owner/repo/issues/123".into()
                )),
                "issue link missing with format.pretty={pretty}"
            );
            let lines: Vec<_> = app.show_text.lines().map(crate::ansi::plain).collect();
            let emphasized: Vec<_> = app
                .show_message_styles
                .iter()
                .flat_map(|(source, ranges)| {
                    ranges
                        .iter()
                        .map(|(range, _)| &lines[*source][range.clone()])
                })
                .collect();
            for marker in ["feat", "!", "Migration:", "BREAKING CHANGE:", "Refs"] {
                assert!(
                    emphasized.contains(&marker),
                    "{marker:?} style missing with format.pretty={pretty}"
                );
            }
        }
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
        let text = format!("commit {hash}\nAuthor: Test\n\n    Fix ({short}), reverting {hash}.\n    {short}_identifier identifier{short} repo#{short} repo@{short} https://github.com/other/repo/commit/{short} {blob} 0000000\n\nNotes:\n    {hash}\ndiff --git a/file b/file\n    {hash}\n");
        let links = collect(&text, &mut None);
        assert_eq!(
            links.len(),
            2,
            "exclude headers, notes, patches, blobs, identifiers and other repositories"
        );
        let url = format!("https://github.com/other/repo/commit/{short}");
        assert_eq!(
            links[&4]
                .iter()
                .map(|link| &link.target)
                .collect::<Vec<_>>(),
            [&Target::Url(url)],
            "a hash in another repository's URL is part of that URL's link"
        );
        assert_eq!(links[&3].len(), 2);
        for link in &links[&3] {
            assert_eq!(link.target, Target::Commit(hash.clone()));
            assert!(text.lines().nth(3).unwrap()[link.range.clone()].starts_with(short));
        }
        // Decorations may split a hash into spans; all parts keep the link style.
        let line = crate::ansi::parse_line(&format!("{short}\x1b[31m{}", &hash[7..]));
        let styled = crate::ansi::style_ranges(
            line,
            [(
                0..hash.len(),
                ratatui::style::Style::default().add_modifier(ratatui::style::Modifier::UNDERLINED),
            )],
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
            format!("#{}", &hash[..7]),
            hash.clone(),
        ];
        let message = format!("    See {}.", tokens.join(", "));
        let text = format!(
            "commit {hash}\n\n{message}\n    {} {} {} #{} #{} 2026 {uppercase} {mixed} #{uppercase} #{mixed}\n",
            &hash[..4],
            &hash[..5],
            &hash[..6],
            &hash[..3],
            &hash[..6]
        );
        let links = collect(&text, &mut None);
        assert_eq!(
            links.len(),
            1,
            "bare short hashes, prefixes below four characters, color-like #rrggbb, and uppercase or mixed-case hashes stay plain"
        );
        assert_eq!(links[&2].len(), tokens.len());
        for (link, token) in links[&2].iter().zip(tokens) {
            assert_eq!(link.target, Target::Commit(hash.clone()));
            assert_eq!(&message[link.range.clone()], token);
        }
    }

    #[test]
    fn hashes_resolve_past_refs_named_like_them() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        let target = commit("target");
        commit("other");
        git(&["branch", &target[..7]]);
        git(&["tag", &target[..8]]);
        let text = format!(
            "commit {target}\n\n    Fix {} and {}.\n",
            &target[..7],
            &target[..8]
        );
        let links = collect(&text, &mut None);
        assert_eq!(links[&2].len(), 2);
        for link in &links[&2] {
            assert_eq!(link.target, Target::Commit(target.clone()));
        }
    }

    #[test]
    fn both_cells_of_a_wide_glyph_in_a_link_are_clickable() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        commit("wide\n\nhttps://example.com/日本");
        let mut app = git::load_show_app(&[]).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let glyph = buffer
            .content()
            .iter()
            .position(|cell| cell.symbol() == "日")
            .unwrap();
        let (x, y) = buffer.pos_of(glyph);
        for column in [x, x + 1] {
            assert!(
                app.visible_show_links
                    .iter()
                    .any(|(rect, _)| rect.contains(ratatui::layout::Position::new(column, y))),
                "column {column} of the wide glyph is not clickable"
            );
        }
    }

    #[test]
    fn short_numeric_references_are_issues_and_six_digits_are_colors() {
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
        ];
        let line = format!("    Fix {} and #{}.", issue_tokens.join(", "), &hash[..7]);
        let text = format!(
            "commit {hash}\n\n{line}\n    #12abc_thing #ABCDEF #999999 #{} #1234567z owner/another#42 &#8212;\n",
            &hash[..6]
        );
        let without_remote = collect(&text, &mut None);
        assert_eq!(
            without_remote[&2].len(),
            1,
            "short numbers never fall back to commits"
        );
        assert_eq!(without_remote[&2][0].target, Target::Commit(hash.clone()));
        git(&["remote", "add", "origin", "git@github.com:owner/repo.git"]);
        let links = collect(&text, &mut None);
        assert_eq!(links.len(), 1);
        assert_eq!(links[&2].len(), issue_tokens.len() + 1);
        for (link, token) in links[&2].iter().zip(issue_tokens) {
            assert_eq!(&line[link.range.clone()], token);
            assert_eq!(
                link.target,
                Target::Url(format!(
                    "https://github.com/owner/repo/issues/{}",
                    &token[1..]
                ))
            );
        }
        assert_eq!(links[&2].last().unwrap().target, Target::Commit(hash));
    }

    #[test]
    fn urls_end_at_brackets_they_did_not_open() {
        let line = "[https://a.example](https://a.example/(b)) \
            [![age](https://img.example/x)](https://docs.example/y)";
        let found: Vec<_> = urls(line).into_iter().map(|url| &line[url]).collect();
        assert_eq!(
            found,
            [
                "https://a.example",
                "https://a.example/(b)",
                "https://img.example/x",
                "https://docs.example/y",
            ]
        );
    }

    #[test]
    fn urls_to_this_repository_open_in_glog_and_others_in_the_browser() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        git(&["remote", "add", "origin", "git@github.com:Owner/Repo.git"]);
        let base = commit("base");
        git(&["tag", "v1"]);
        let middle = commit("middle");
        let head = commit("head");
        let repo = "https://github.com/owner/repo";
        let urls = [
            format!("{repo}/commit/{}", &middle[..7]),
            format!("{repo}/compare/{}...{}", &base[..7], &head[..9]),
            format!("https://www.github.com/OWNER/REPO/commit/{head}#diff-1"),
            format!("{repo}/commit/{}", "0".repeat(40)),
            // Local branches and tags may differ from GitHub's.
            format!("{repo}/compare/v1...{}", &head[..7]),
            format!("{repo}/pull/7"),
            "https://example.com/wiki/Git_(software)".to_owned(),
            "https://example.com/a".to_owned(),
            "https://example.com/b".to_owned(),
            "https://example.com/c".to_owned(),
        ];
        let line = format!(
            "    See {}, ({}). <{}> {}; [x]({}) {}, {}!",
            urls[0], urls[1], urls[2], urls[3], urls[4], urls[5], urls[6]
        ) + &format!(" see_{} [{}] **{}**", urls[7], urls[8], urls[9]);
        let text = format!("commit {head}\n\n{line}\n");
        let links = collect(&text, &mut None);
        let found: Vec<_> = links[&2]
            .iter()
            .map(|link| (&line[link.range.clone()], link.target.clone()))
            .collect();
        let expected = [
            Target::Commit(middle),
            Target::Range(format!("{base}..{head}")),
            Target::Commit(head.clone()),
            Target::Url(urls[3].clone()),
            Target::Url(urls[4].clone()),
            Target::Url(urls[5].clone()),
            Target::Url(urls[6].clone()),
            Target::Url(urls[7].clone()),
            Target::Url(urls[8].clone()),
            Target::Url(urls[9].clone()),
        ];
        assert_eq!(
            found,
            urls.iter()
                .map(String::as_str)
                .zip(expected)
                .collect::<Vec<_>>()
        );

        let mut app = git::load_show_app(&[head]).unwrap();
        app.follow_link(&found[1].1);
        assert_eq!(app.mode, Mode::Log);
        assert_eq!(
            app.commits
                .iter()
                .map(|commit| commit.subject.as_str())
                .collect::<Vec<_>>(),
            ["head", "middle"]
        );
        assert!(app.has_reference_back());
        app.follow_link(&Target::Range(format!("{base}..{base}")));
        assert_eq!(
            app.status.as_deref(),
            Some(&*format!("no commits in {}..{}", &base[..7], &base[..7]))
        );
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
                .find(|(_, target)| target == &Target::Url(expected.into()))
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
            if scenario == "followed" {
                // The opener's result must reach whichever view is active when it finishes.
                app.follow_reference(&hash);
                assert!(app.has_reference_back());
            }
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
            if scenario != "followed" {
                assert_eq!(
                    (app.show_text.clone(), app.show_cursor, app.show_offset),
                    before
                );
                assert!(!app.has_reference_back());
            }
            if scenario != "success" {
                let reason = if super::XDG_OPEN {
                    "the browser failed to open the URL"
                } else {
                    "test browser failure"
                };
                assert!(app.status.unwrap().contains(reason));
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
if [ "$GLOG_TEST_ISSUE_BROWSER_CHILD" != success ]; then
    sleep 0.1
    echo 'test browser failure' >&2
    exit 4
fi
"##,
        )
        .unwrap();
        std::fs::set_permissions(opener, std::fs::Permissions::from_mode(0o755)).unwrap();
        let original_path = env::var_os("PATH").unwrap();
        let path =
            env::join_paths(std::iter::once(bin).chain(env::split_paths(&original_path))).unwrap();
        for scenario in ["success", "failure", "followed"] {
            let output = Command::new(env::current_exe().unwrap())
                .args(["--exact", "commit_links::tests::issue_click_opens_browser_and_reports_failures_without_leaving_show", "--nocapture"])
                .env("PATH", &path).env(CHILD, scenario)
                .env(CAPTURE, directory.path().join(scenario))
                .output().unwrap();
            assert!(output.status.success(), "{output:?}");
        }
    }

    #[test]
    fn followed_references_reuse_the_looked_up_repository() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        git(&[
            "remote",
            "add",
            "origin",
            "https://github.com/owner/repo.git",
        ]);
        let first = commit("first, for #1");
        commit(&format!("second, after {first} for #2"));
        let mut app = git::load_show_app(&[]).unwrap();
        // A changed remote shows whether the followed view looks it up again.
        git(&[
            "remote",
            "set-url",
            "origin",
            "https://github.com/other/repo.git",
        ]);
        app.follow_reference(&first);
        assert_eq!(app.commits[app.selected].hash, first);
        let targets: Vec<_> = app
            .show_links
            .values()
            .flatten()
            .map(|link| &link.target)
            .collect();
        assert_eq!(
            targets,
            [&Target::Url(
                "https://github.com/owner/repo/issues/1".into()
            )]
        );
    }

    #[test]
    fn followed_ranges_select_a_commit_their_filters_show() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        let base = commit("base");
        let shown = commit("feat: shown");
        let hidden = commit("chore: hidden");
        commit("tip");
        let mut app = crate::app::App::new(git::load_watch_log().unwrap());
        app.log_folds.hidden_types.insert("chore".into());
        app.refresh_log_folds();
        app.follow_link(&Target::Range(format!("{base}..{hidden}")));
        assert_eq!(app.commits[app.selected].hash, shown);
    }

    #[test]
    fn following_a_side_branch_suspends_watch_until_returning() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        commit("base");
        git(&["checkout", "-qb", "side"]);
        let side = commit("side");
        git(&["checkout", "-q", "-"]);
        let main = commit(&format!("main\n\nSee {side}"));
        let mut app = crate::app::App::new(git::load_watch_log().unwrap());
        app.watch = true;
        app.selected = 1;
        app.switch_mode();
        app.follow_reference(&side);
        assert_eq!(app.commits[app.selected].hash, side);
        // The event loop must not replace this snapshot with default HEAD history.
        assert!(!app.watch);
        std::fs::write("new-file", "change\n").unwrap();
        app.switch_mode();
        assert_eq!(app.commits[app.selected].hash, side);
        assert!(app
            .commits
            .iter()
            .all(|commit| commit.kind == git::CommitKind::Revision));
        app.switch_mode();
        assert_eq!(app.commits[app.selected].hash, side);
        app.reference_back();
        assert!(app.watch);
        app.replace_commits(git::load_watch_log().unwrap());
        assert_eq!(app.commits[app.selected].hash, main);
        assert_eq!(app.mode, Mode::Show);
    }

    #[test]
    fn escape_returns_from_a_followed_reference_before_exiting() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        let first = commit("first");
        let second = commit(&format!("second\n\nSee {first}"));
        let mut app = git::load_show_app(std::slice::from_ref(&second)).unwrap();
        app.exit_on_esc = true;
        app.follow_reference(&first);
        let escape = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        input::handle(escape.clone(), &mut app);
        assert_eq!(app.mode, Mode::Log);
        input::handle(escape.clone(), &mut app);
        assert!(!app.quit);
        assert_eq!(app.commits[app.selected].hash, second);
        assert!(!app.has_reference_back());
        input::handle(escape.clone(), &mut app);
        input::handle(escape, &mut app);
        assert!(app.quit);
    }

    #[test]
    fn search_history_is_shared_with_followed_references() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        let first = commit("first");
        let second = commit(&format!("second\n\nSee {first}"));
        let mut app = git::load_show_app(&[second]).unwrap();
        let search = |app: &mut crate::app::App, query: &str| {
            app.begin_search(false);
            app.search_input = Some(query.to_owned());
            input::handle(Event::Key(KeyEvent::from(KeyCode::Enter)), app);
        };
        let recall = |app: &mut crate::app::App| {
            app.begin_search(false);
            input::handle(Event::Key(KeyEvent::from(KeyCode::Up)), app);
            let query = app.search_input.clone();
            input::handle(Event::Key(KeyEvent::from(KeyCode::Esc)), app);
            query
        };
        search(&mut app, "origin");
        app.follow_reference(&first);
        assert_eq!(recall(&mut app).as_deref(), Some("origin"));
        search(&mut app, "followed");
        app.reference_back();
        assert_eq!(recall(&mut app).as_deref(), Some("followed"));
    }

    #[test]
    fn returning_from_a_reference_keeps_show_state_across_tab_switches() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        let first = commit("first");
        std::fs::write("Cargo.lock", "first\nsecond\n").unwrap();
        let second = commit(&format!("second\n\nSee {first}\n\nMore context"));
        let mut app = git::load_show_app(&[second]).unwrap();
        app.show_cursor = app.show_rows.iter().position(|row| row.folded).unwrap();
        app.toggle_show_file();
        let mut terminal = Terminal::new(TestBackend::new(30, 9)).unwrap();
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        let cursor = app.show_cursor;
        let offset = app.show_offset;
        let rows: Vec<_> = app.show_rows.iter().map(|row| row.text.clone()).collect();
        assert!(offset > 0);
        app.follow_reference(&first);
        app.reference_back();
        app.switch_mode();
        app.switch_mode();
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        assert_eq!(app.show_cursor, cursor);
        assert_eq!(app.show_offset, offset);
        assert_eq!(
            app.show_rows
                .iter()
                .map(|row| row.text.clone())
                .collect::<Vec<_>>(),
            rows
        );
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
            app.log_format = crate::log_format::LogFormat::parse("%s").unwrap();
            app.log_folds.hidden_types.insert("test".into());
            app.log_folds.hide_merges = true;
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
                for position in rect.positions() {
                    let cell = &terminal.backend().buffer()[position];
                    assert!(
                        cell.symbol().bytes().all(|byte| byte.is_ascii_hexdigit()),
                        "hit target must cover the displayed hash: {cell:?}"
                    );
                    assert!(cell.modifier.contains(ratatui::style::Modifier::UNDERLINED));
                }
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
            // The followed history keeps the Log's settings for when Tab opens it.
            assert!(!app.watch);
            assert_eq!(app.context, format!("show {}", &second[..7]));
            assert_eq!(app.log_format.text(&app.commits[app.selected]), "second");
            assert!(app.log_folds.hidden_types.contains("test"));
            assert!(app.log_folds.hide_merges);
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
