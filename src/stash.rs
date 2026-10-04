//! Read-only browsing of saved stashes and their complete patches.
use std::process::Command;

use ratatui::{
    style::{Color, Style},
    text::Span,
};

use crate::{
    app::App,
    git::{self, Annotation, Collaborators, Commit, CommitKind},
    log_format::{Field, LogFormat},
};

const USAGE: &str = "use glog stash [list [-n COUNT] [--date=STYLE] | show [--stat] [stash]]";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StashEntry {
    pub selector: String,
    pub message: String,
    selector_padding: usize,
}

pub fn load_app(args: &[String]) -> Result<App, String> {
    match args.first().map(String::as_str) {
        None => load_list(&[]).map(into_app),
        Some("list") => load_list(&args[1..]).map(into_app),
        Some("show") => load_show(&args[1..]),
        _ => Err(USAGE.into()),
    }
}

fn into_app(mut rows: Vec<Commit>) -> App {
    // Use the whole snapshot, so scrolling and toggling columns cannot move
    // the columns after the selector. Detached stash refs participate too.
    let width = rows
        .iter()
        .filter_map(|row| match &row.annotation {
            Some(Annotation::Stash(entry)) => Some(entry),
            _ => None,
        })
        .map(|entry| Span::raw(&entry.selector).width())
        .max()
        .unwrap_or(0);
    for row in &mut rows {
        if let Some(Annotation::Stash(entry)) = &mut row.annotation {
            entry.selector_padding = width.saturating_sub(Span::raw(&entry.selector).width());
        }
    }
    App::new(rows)
}

fn load_list(args: &[String]) -> Result<Vec<Commit>, String> {
    let mut date = git::configured_log_date("format-local:%Y-%m-%d %H:%M")?;
    let mut count = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let value = match arg.as_str() {
            "-n" | "--max-count" | "--date" => args
                .next()
                .ok_or_else(|| format!("{arg} requires a value; {USAGE}"))?
                .as_str(),
            _ if arg.starts_with("--date=") => &arg[7..],
            _ if arg.starts_with("--max-count=") => &arg[12..],
            _ if arg.starts_with("-n") => &arg[2..],
            _ => return Err(format!("unsupported stash list argument {arg:?}; {USAGE}")),
        };
        if arg.starts_with("--date") {
            date = value.to_owned();
        } else {
            count = Some(
                value
                    .parse::<usize>()
                    .map_err(|_| format!("invalid stash count {value:?}; {USAGE}"))?,
            );
        }
    }
    let output = Command::new("git")
        .args(["--no-pager", "stash", "list"])
        .arg(format!("--date={date}"))
        .args(count.map(|count| format!("--max-count={count}")))
        .args([
            "--no-color",
            "--no-patch",
            "--no-show-signature",
            "--no-notes",
            "-z",
            "--format=%H%x00%h%x00%gs%x00%an%x00%ae%x00%cd%x00%s",
        ])
        .env("LC_ALL", "C")
        .output()
        .map_err(|error| format!("could not run git stash list: {error}"))?;
    if !output.status.success() {
        return Err(git::stderr_message("git stash list failed", &output.stderr));
    }
    let output = String::from_utf8_lossy(&output.stdout);
    if output.is_empty() {
        return Ok(Vec::new());
    }
    let fields: Vec<_> = output
        .strip_suffix('\0')
        .unwrap_or(&output)
        .split('\0')
        .collect();
    let (records, remainder) = fields.as_chunks::<7>();
    if !remainder.is_empty() {
        return Err("invalid Git stash list output".into());
    }
    records
        .iter()
        .enumerate()
        .map(|(index, fields)| {
            if fields[5].chars().any(char::is_control) {
                return Err("Git stash dates must fit on one line; use --date=iso-strict".into());
            }
            Ok(Commit {
                kind: CommitKind::Revision,
                // Only a leading count limit is supported, so list positions
                // are the real stash indices, even with custom date formats.
                annotation: Some(Annotation::Stash(StashEntry {
                    selector: format!("stash@{{{index}}}"),
                    message: fields[2].into(),
                    selector_padding: 0,
                })),
                diff_args: Vec::new(),
                hash: fields[0].into(),
                short_hash: fields[1].into(),
                subject: fields[6].into(),
                author: fields[3].into(),
                author_email: fields[4].into(),
                author_date: fields[5].into(),
                collaborators: Collaborators::default(),
                decorations: String::new(),
                graph: vec![String::new()],
                parents: Vec::new(),
            })
        })
        .collect()
}

fn load_show(args: &[String]) -> Result<App, String> {
    let mut stat = false;
    let mut revision = None;
    for arg in args {
        if arg == "--stat" {
            stat = true;
        } else if !arg.starts_with('-') && revision.is_none() {
            revision = Some(arg.as_str());
        } else {
            return Err(USAGE.into());
        }
    }
    let revision = revision.unwrap_or("stash@{0}");
    let revision = if !revision.is_empty() && revision.bytes().all(|b| b.is_ascii_digit()) {
        format!("stash@{{{revision}}}")
    } else {
        revision.to_owned()
    };
    let output = Command::new("git")
        .args(["rev-parse", "--verify", "--end-of-options"])
        .arg(format!("{revision}^{{commit}}"))
        .output()
        .map_err(|error| format!("could not resolve stash: {error}"))?;
    if !output.status.success() {
        return Err(git::stderr_message(
            "could not resolve stash",
            &output.stderr,
        ));
    }
    let hash = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let mut rows = load_list(&[])?;
    let selected = rows.iter().position(|row| {
        row.hash == hash
            && matches!(&row.annotation, Some(Annotation::Stash(entry)) if entry.selector == revision)
    }).or_else(|| rows.iter().position(|row| row.hash == hash));
    let selected = match selected {
        Some(index) => index,
        None => {
            // Git also accepts a stash commit that is no longer in the list.
            let mut commits = git::load_log(&["-1".into(), hash, "--".into()])?;
            let mut commit = commits.pop().ok_or("could not read stash commit")?;
            commit.annotation = Some(Annotation::Stash(StashEntry {
                selector: revision,
                message: commit.subject.clone(),
                selector_padding: 0,
            }));
            commit.graph = vec![String::new()];
            commit.parents.clear();
            rows.insert(0, commit);
            0
        }
    };
    // Validate the stash and surface patch errors before entering the TUI.
    let text = git::show(&rows[selected], &[])?;
    let mut app = into_app(rows);
    app.selected = selected;
    app.show_stat = stat;
    app.show_text = text;
    app.ensure_show_rows();
    app.mode = crate::app::Mode::Show;
    Ok(app)
}

/// Use stash show instead of a merge commit's combined diff, which omits
/// staged-only changes and the separate parent containing untracked files.
pub fn show(commit: &Commit, entry: &StashEntry) -> Result<String, String> {
    let output = git::patch_command()
        .args([
            "-c",
            "stash.showStat=false",
            "-c",
            "stash.showPatch=true",
            "--no-pager",
            "stash",
            "show",
            "--patch",
            "--include-untracked",
            "--color=always",
            "--no-ext-diff",
            "--full-index",
            "--submodule=short",
        ])
        .args(git::DIFF_PREFIX_ARGS)
        .arg(&commit.hash)
        .env("LC_ALL", "C")
        .output()
        .map_err(|error| format!("could not run git stash show: {error}"))?;
    if !output.status.success() {
        return Err(git::stderr_message("git stash show failed", &output.stderr));
    }
    let mut text = format!(
        "\x1b[33mstash {} ({})\x1b[m\nAuthor: {} <{}>\nDate:   {}\n\n    {}\n\n",
        commit.hash,
        entry.selector,
        commit.author,
        commit.author_email,
        commit.author_date,
        commit.subject,
    )
    .into_bytes();
    text.extend(output.stdout);
    git::format_output(text)
}

pub fn spans(commit: &Commit, entry: &StashEntry, format: &LogFormat) -> Vec<Span<'static>> {
    let mut spans = vec![Span::styled(
        format!("{}{} ", entry.selector, " ".repeat(entry.selector_padding)),
        Style::default().fg(Color::Yellow),
    )];
    if format.shows(Field::Date) {
        spans.push(Span::styled(
            format!("{} ", commit.author_date),
            Style::default().fg(Color::Gray),
        ));
    }
    if format.shows(Field::Author) {
        spans.push(Span::styled(
            format!("{} ", commit.author),
            Style::default().fg(Color::Cyan),
        ));
    }
    spans.push(Span::raw(entry.message.clone()));
    spans
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::{History, Mode},
        git::tests::{CurrentDirGuard, TestDirectory},
        input, ui,
    };
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{backend::TestBackend, Terminal};
    use std::fs;

    fn git(args: &[&str]) -> String {
        let output = Command::new("git")
            .args([
                "-c",
                "user.name=Alice",
                "-c",
                "user.email=alice@example.com",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .env("GIT_AUTHOR_NAME", "Alice")
            .env("GIT_AUTHOR_EMAIL", "alice@example.com")
            .env("GIT_AUTHOR_DATE", "2020-01-01T12:00:00+00:00")
            .env("GIT_COMMITTER_DATE", "2020-01-01T12:00:00+00:00")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn key(app: &mut App, code: KeyCode) {
        input::handle(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)), app);
    }

    #[test]
    fn stash_preserves_staged_content_overwritten_in_the_worktree() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        fs::write("file.txt", "base\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "base"]);
        fs::write("file.txt", "saved staged content\n").unwrap();
        git(&["add", "."]);
        fs::write("file.txt", "base\n").unwrap();
        git(&["stash", "push"]);

        let mut app = load_app(&["show".into()]).unwrap();
        let patch = crate::ansi::plain(&app.show_text);
        assert!(patch.contains("+saved staged content"), "{patch}");
        assert!(patch.contains("-saved staged content"), "{patch}");
        assert!(patch.contains("+base"), "{patch}");

        key(&mut app, KeyCode::Char('s'));
        for label in ["Staged changes", "Unstaged changes"] {
            assert!(app
                .show_rows
                .iter()
                .any(|row| crate::ansi::plain(&row.text) == label));
        }
        // Both layers affect the same filename, but expanding one must not
        // expand the other or move a summary bookmark to the wrong patch.
        app.show_cursor = app
            .show_rows
            .iter()
            .position(|row| row.file == Some(1))
            .unwrap();
        key(&mut app, KeyCode::Enter);
        assert!(app
            .show_rows
            .iter()
            .any(|row| row.file == Some(0) && row.folded));
        assert!(app
            .show_rows
            .iter()
            .any(|row| row.file == Some(1) && !row.folded));
        key(&mut app, KeyCode::Char('s'));
        assert_eq!(app.show_rows[app.show_cursor].file, Some(1));
    }

    #[test]
    fn stash_columns_align_across_index_widths_and_toggle_independently() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        fs::write("file.txt", "base\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "base"]);
        for index in 0..11 {
            fs::write("file.txt", format!("saved {index}\n")).unwrap();
            git(&["stash", "push", "-m", "saved work"]);
        }
        let mut app = load_app(&["list".into(), "--date=short".into()]).unwrap();
        let row = |app: &App, index| app.log_format.text(&app.commits[index]);
        assert!(row(&app, 9).starts_with("stash@{9}  2020-01-01 Alice "));
        assert!(row(&app, 10).starts_with("stash@{10} 2020-01-01 Alice "));

        key(&mut app, KeyCode::Char('d'));
        assert!(row(&app, 9).starts_with("stash@{9}  Alice "));
        assert!(row(&app, 10).starts_with("stash@{10} Alice "));
        key(&mut app, KeyCode::Char('a'));
        let Some(Annotation::Stash(entry)) = &app.commits[9].annotation else {
            panic!("expected a stash entry");
        };
        assert_eq!(row(&app, 9), format!("stash@{{9}}  {}", entry.message));
        key(&mut app, KeyCode::Char('d'));
        assert!(row(&app, 9).starts_with("stash@{9}  2020-01-01 "));
        assert!(!row(&app, 9).contains("Alice"));
        key(&mut app, KeyCode::Char('a'));
        assert!(row(&app, 9).starts_with("stash@{9}  2020-01-01 Alice "));
    }

    #[test]
    fn stash_browsing_preserves_complete_patches_and_snapshot_identity() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        assert!(load_app(&[]).unwrap().commits.is_empty());
        assert!(load_app(&["show".into()]).is_err());
        for path in ["staged.txt", "unstaged.txt"] {
            fs::write(path, "base\n").unwrap();
        }
        git(&["add", "."]);
        git(&["commit", "-qm", "base"]);
        fs::write("staged.txt", "staged change\n").unwrap();
        git(&["add", "staged.txt"]);
        fs::write("unstaged.txt", "unstaged change\n").unwrap();
        let untracked = "saved \"é\".txt";
        fs::write(untracked, "saved untracked\n").unwrap();
        fs::write("saved.png", b"\x89PNG\0saved image").unwrap();
        git(&["stash", "push", "-u", "-m", "first saved work"]);
        let first_hash = git(&["rev-parse", "stash@{0}"]);
        let image_hash = git(&["rev-parse", "stash@{0}^3:saved.png"]);
        fs::write("unstaged.txt", "second change\n").unwrap();
        git(&["stash", "push", "-m", "second saved work"]);

        // These preferences must not hide patches or change path identities.
        for (name, value) in [
            ("stash.showStat", "true"),
            ("stash.showPatch", "false"),
            ("stash.showIncludeUntracked", "false"),
            ("diff.relative", "true"),
            ("diff.noprefix", "true"),
            ("core.quotePath", "false"),
            ("log.date", "short"),
        ] {
            git(&["config", name, value]);
        }
        fs::create_dir("sub").unwrap();
        std::env::set_current_dir("sub").unwrap();
        let mut app = load_app(&[]).unwrap();
        assert_eq!(app.history, History::Stash);
        assert_eq!(app.commits.len(), 2);
        let text = app.log_format.text(&app.commits[1]);
        assert!(text.starts_with("stash@{1} 2020-01-01 Alice "), "{text}");
        assert!(text.contains("first saved work"));
        assert_eq!(load_list(&["-n1".into()]).unwrap().len(), 1);
        assert!(load_list(&["-n".into(), "0".into()]).unwrap().is_empty());
        let dated = load_list(&["--date=format:%Y@{x".into()]).unwrap();
        assert!(app
            .log_format
            .text(&dated[1])
            .starts_with("stash@{1} 2020@{x "));

        app.search_input = Some("first saved work".into());
        app.submit_search();
        assert_eq!(app.selected, 1);
        for action in ['t', 'M', 'z', 'm'] {
            key(&mut app, KeyCode::Char(action));
        }
        assert!(app.log_folds.visible(1));
        let mut terminal = Terminal::new(TestBackend::new(120, 35)).unwrap();
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        assert!(app.type_buttons.is_empty());
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.mode, Mode::Show);
        assert!(app.status.is_none(), "{:?}", app.status);
        let patch = crate::ansi::plain(&app.show_text);
        for content in ["+staged change", "+unstaged change", "+saved untracked"] {
            assert!(patch.contains(content), "missing {content}: {patch}");
        }
        assert!(patch.contains(&image_hash));
        assert!(crate::diff::file_sections(&app.show_text)
            .iter()
            .any(|file| file.path_bytes == untracked.as_bytes()));
        key(&mut app, KeyCode::Char('s'));
        assert!(app.show_stat);
        key(&mut app, KeyCode::Left);
        assert_eq!(app.selected, 0);
        assert!(crate::ansi::plain(&app.show_text).contains("+second change"));
        key(&mut app, KeyCode::Tab);
        assert_eq!(app.mode, Mode::Log);
        key(&mut app, KeyCode::Right);
        assert_eq!(app.selected, 1);

        let direct = load_app(&["show".into(), "--stat".into(), "1".into()]).unwrap();
        assert_eq!(direct.history, History::Stash);
        assert_eq!(direct.mode, Mode::Show);
        assert_eq!(direct.selected, 1);
        assert!(direct.show_stat);
        assert!(crate::ansi::plain(&direct.show_text).contains("+saved untracked"));

        // Dropping another stash moves indices; existing rows still open the
        // saved objects, including files absent from the current checkout.
        git(&["stash", "drop", "stash@{0}"]);
        let patch = crate::ansi::plain(&git::show(&app.commits[1], &[]).unwrap());
        assert!(patch.contains("+staged change"));
        assert_eq!(app.commits[1].hash, first_hash);
        git(&["stash", "store", "-m", "other stash", &app.commits[0].hash]);
        git(&["stash", "store", "-m", "same stash again", &first_hash]);
        let mut repeated = load_app(&["show".into(), "2".into()]).unwrap();
        assert_eq!(repeated.selected, 2);
        let heading =
            |app: &App| crate::ansi::plain(app.show_text.lines().next().unwrap_or_default());
        assert_eq!(
            heading(&repeated),
            format!("stash {first_hash} (stash@{{2}})")
        );
        // The same saved commit appears twice. Returning through the Show
        // cache must retain the selected entry's selector, not the last one's.
        for (direction, index) in [
            (KeyCode::Left, 1),
            (KeyCode::Left, 0),
            (KeyCode::Right, 1),
            (KeyCode::Right, 2),
            (KeyCode::Left, 1),
            (KeyCode::Left, 0),
        ] {
            key(&mut repeated, direction);
            assert_eq!(repeated.selected, index);
            assert_eq!(
                heading(&repeated),
                format!("stash {} (stash@{{{index}}})", repeated.commits[index].hash,)
            );
        }
        git(&["stash", "clear"]);
        let detached = load_app(&["show".into(), first_hash]).unwrap();
        assert_eq!(detached.commits.len(), 1);
        assert!(crate::ansi::plain(&detached.show_text).contains("+saved untracked"));
    }

    #[test]
    fn stash_rejects_mutations_and_invalid_arguments_without_changing_saved_work() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        fs::write("file.txt", "base\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "base"]);
        fs::write("file.txt", "saved\n").unwrap();
        git(&["stash", "push", "-m", "saved"]);
        fs::write("file.txt", "current\n").unwrap();
        let before = git(&["stash", "list", "--format=%H %gs"]);
        for args in [
            vec!["push"],
            vec!["pop"],
            vec!["apply"],
            vec!["drop"],
            vec!["clear"],
            vec!["list", "--all"],
            vec!["list", "-n"],
            vec!["list", "-nwat"],
            vec!["list", "--date=format:%n"],
            vec!["show", "--output=written.patch"],
            vec!["show", "--wat"],
            vec!["show", "0", "1"],
            vec!["show", "99"],
            vec!["show", "HEAD"],
        ] {
            let args: Vec<_> = args.into_iter().map(str::to_owned).collect();
            assert!(load_app(&args).is_err(), "{args:?}");
        }
        assert_eq!(git(&["stash", "list", "--format=%H %gs"]), before);
        assert_eq!(fs::read_to_string("file.txt").unwrap(), "current\n");
        assert!(!std::path::Path::new("written.patch").exists());
    }
}
