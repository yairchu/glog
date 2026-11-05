//! Git's range-diff output is human-readable text, not a patch to parse.
use crate::{app::App, git};

pub fn load_app(args: &[String]) -> Result<App, String> {
    let output = git::patch_command()
        .args([
            "--no-pager",
            "range-diff",
            "--color=always",
            "--no-ext-diff",
        ])
        .args(args)
        .output()
        .map_err(|error| format!("could not run git range-diff: {error}"))?;
    if !output.status.success() {
        return Err(git::stderr_message("git range-diff failed", &output.stderr));
    }
    // Keep Git's dual coloring; passing this diff of patches through a patch
    // formatter or file-fold parser would change its meaning and layout.
    Ok(App::range_diff(
        String::from_utf8_lossy(&output.stdout).into_owned(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::tests::{git, CurrentDirGuard, TestDirectory};
    use crate::{
        ansi,
        app::{History, Mode},
        input, ui,
    };
    use crossterm::event::{
        Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use ratatui::{backend::TestBackend, Terminal};

    fn fixture() {
        git(&["init", "-q"]);
        std::fs::write("file.rs", "base\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "base"]);
        git(&["tag", "base"]);
        let lines = |changed| {
            (0..80)
                .map(|index| {
                    let call = if changed && [12, 36, 60].contains(&index) {
                        "new"
                    } else {
                        "old"
                    };
                    format!("let value{index} = {call}{index}_operation();\n")
                })
                .collect::<String>()
        };
        std::fs::write("file.rs", lines(false)).unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "feature"]);
        git(&["tag", "before"]);
        std::fs::write("file.rs", lines(true)).unwrap();
        git(&["add", "."]);
        git(&["commit", "--amend", "-qm", "feature"]);
        git(&["tag", "after"]);
    }

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).into()).collect()
    }

    fn key(code: KeyCode, app: &mut App) {
        input::handle(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)), app);
    }

    fn mouse(kind: MouseEventKind, row: u16, app: &mut App) {
        input::handle(
            Event::Mouse(MouseEvent {
                kind,
                column: 0,
                row,
                modifiers: KeyModifiers::NONE,
            }),
            app,
        );
    }

    #[test]
    fn nested_diff_keeps_git_colors_and_supports_wrapped_scrolling_and_search() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        fixture();
        let ranges = args(&["base..before", "base..after"]);
        let output = git::patch_command()
            .args([
                "--no-pager",
                "range-diff",
                "--color=always",
                "--no-ext-diff",
            ])
            .args(&ranges)
            .output()
            .unwrap();
        assert!(output.status.success());
        let expected = String::from_utf8(output.stdout).unwrap();
        assert!(expected.contains("\x1b["));
        assert!(expected.contains("new36_operation"));
        for width in [20, 100] {
            let mut app = load_app(&ranges).unwrap();
            assert_eq!(app.show_text, expected);
            assert_eq!(app.history, History::RangeDiff);
            assert!(!app.has_log_view());
            let mut terminal = Terminal::new(TestBackend::new(width, 10)).unwrap();
            terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
            assert_eq!(
                app.show_rows
                    .iter()
                    .map(|row| row.text.as_str())
                    .collect::<Vec<_>>(),
                expected.lines().collect::<Vec<_>>()
            );
            assert!(app
                .show_rows
                .iter()
                .all(|row| row.file.is_none() && !row.folded && !row.summary));
            mouse(MouseEventKind::ScrollDown, 3, &mut app);
            terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
            assert_eq!(app.show_offset, 3);
            mouse(MouseEventKind::ScrollUp, 3, &mut app);
            terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
            assert_eq!(app.show_offset, 0);
            for _ in 0..100 {
                mouse(MouseEventKind::ScrollDown, 3, &mut app);
            }
            terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
            assert_eq!(
                app.show_offset,
                app.show_row_starts.last().unwrap() - app.visible_show_rows
            );
            key(KeyCode::Home, &mut app);
            terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
            assert_eq!(app.show_offset, 0);
            key(KeyCode::Char(' '), &mut app);
            terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
            assert_eq!(app.show_offset, app.visible_show_rows);
            key(KeyCode::Char('/'), &mut app);
            for c in "new36_operation".chars() {
                key(KeyCode::Char(c), &mut app);
            }
            key(KeyCode::Enter, &mut app);
            terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
            assert!(ansi::plain(&app.show_rows[app.show_cursor].text).contains("new36_operation"));
            // Patch-only actions must not interpret a diff of diffs as a file.
            for code in [
                KeyCode::Tab,
                KeyCode::Enter,
                KeyCode::Char('s'),
                KeyCode::Char('z'),
                KeyCode::Char('B'),
                KeyCode::Char('e'),
                KeyCode::Char('W'),
                KeyCode::Char('O'),
            ] {
                key(code, &mut app);
            }
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                app.show_row_origin,
                &mut app,
            );
            assert_eq!(app.mode, Mode::Show);
            assert_eq!(app.show_text, expected);
            assert!(!app.show_stat && !app.watch && app.status.is_none());
            assert!(app.pending_editor.is_none());
            key(KeyCode::Char('h'), &mut app);
            terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
            let offset = app.show_offset;
            mouse(MouseEventKind::ScrollDown, 3, &mut app);
            assert_eq!(app.show_offset, offset);
            key(KeyCode::Esc, &mut app);
            assert!(!app.show_help && !app.quit);
            key(KeyCode::Esc, &mut app);
            assert!(!app.quit, "Esc is not a quit key");
            key(KeyCode::Char('q'), &mut app);
            assert!(app.quit);
        }
    }

    #[test]
    fn range_forms_colors_pathspecs_and_errors_are_delegated_to_git() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        fixture();
        let expected = load_app(&args(&["base..before", "base..after"]))
            .unwrap()
            .show_text;
        assert_eq!(
            load_app(&args(&["base", "before", "after"]))
                .unwrap()
                .show_text,
            expected
        );
        assert_eq!(
            load_app(&args(&["before...after"])).unwrap().show_text,
            expected
        );
        let plain = load_app(&args(&["--no-color", "base..before", "base..after"])).unwrap();
        assert_eq!(plain.show_text, ansi::plain(&expected));
        assert!(
            load_app(&args(&["base..before", "base..after", "--", "absent"]))
                .unwrap()
                .show_text
                .is_empty()
        );
        assert!(load_app(&[]).is_err());
        assert!(load_app(&args(&["missing..before", "base..after"])).is_err());
    }
}
