//! Git's range-diff output is human-readable text, not a patch to parse.
use crate::{ansi, app::App, git, pager::PagerView};
use ratatui::{
    layout::Rect,
    style::Color,
    text::Text,
    widgets::{Paragraph, Wrap},
    Frame,
};

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
    Ok(App::range_diff(View::new(&String::from_utf8_lossy(
        &output.stdout,
    ))))
}

/// Git's colored lines, wrapped to the screen, with a line cursor for search.
pub struct View {
    lines: Vec<String>,
    /// Each line's first screen row at `width`, then the total row count.
    starts: Vec<usize>,
    width: u16,
    pub cursor: usize,
    pub offset: usize,
    height: usize,
    origin: u16,
    reveal: bool,
}

impl View {
    pub fn new(text: &str) -> Self {
        Self {
            lines: text.lines().map(str::to_owned).collect(),
            starts: Vec::new(),
            width: 0,
            cursor: 0,
            offset: 0,
            height: 0,
            origin: 0,
            reveal: false,
        }
    }
    pub fn len(&self) -> usize {
        self.lines.len()
    }
    fn last(&self) -> usize {
        self.lines.len().saturating_sub(1)
    }
    fn line_at(&self, screen: usize) -> usize {
        self.starts
            .partition_point(|&start| start <= screen)
            .saturating_sub(1)
            .min(self.last())
    }
    fn max_offset(&self) -> usize {
        let total = self.starts.last().copied().unwrap_or(self.lines.len());
        total.saturating_sub(self.height.max(1))
    }
    pub fn click(&mut self, row: u16) {
        if row < self.origin || usize::from(row - self.origin) >= self.height {
            return;
        }
        let screen = self.offset + usize::from(row - self.origin);
        if self.starts.last().is_some_and(|&total| screen < total) {
            self.cursor = self.line_at(screen);
        }
    }
    pub fn scroll(&mut self, delta: isize) {
        self.offset = self
            .offset
            .saturating_add_signed(delta)
            .min(self.max_offset());
        let first = self.line_at(self.offset);
        let last = self.line_at(self.offset + self.height.max(1) - 1);
        self.cursor = self.cursor.clamp(first, last);
    }
}

impl PagerView for View {
    fn cursor(&self) -> usize {
        self.cursor
    }
    fn wheel(&mut self, delta: isize) {
        self.scroll(delta);
    }
    fn move_by(&mut self, delta: isize) {
        self.cursor = self.cursor.saturating_add_signed(delta).min(self.last());
        self.reveal = true;
    }
    /// Scroll the screen, keeping the cursor on it.
    fn page(&mut self, delta: isize) {
        self.scroll(delta.signum() * self.height.max(1) as isize);
    }
    fn top(&mut self) {
        self.cursor = 0;
        self.offset = 0;
    }
    fn bottom(&mut self) {
        self.cursor = self.last();
        self.offset = self.max_offset();
        self.reveal = true;
    }
    /// Move the cursor to the next line containing the lowercase `query`.
    fn find(&mut self, query: &str, reverse: bool) -> bool {
        let n = self.lines.len();
        for step in 1..=n {
            let index = if reverse {
                (self.cursor + n - step % n) % n
            } else {
                (self.cursor + step) % n
            };
            if ansi::plain(&self.lines[index])
                .to_lowercase()
                .contains(query)
            {
                self.cursor = index;
                self.reveal = true;
                return true;
            }
        }
        false
    }
    fn draw(&mut self, frame: &mut Frame, area: Rect, query: Option<&str>, current: Option<usize>) {
        self.origin = area.y;
        self.height = usize::from(area.height);
        if self.width != area.width || self.starts.len() != self.lines.len() + 1 {
            // Use Ratatui's own wrapping rules, including word boundaries and wide glyphs.
            self.starts = vec![0];
            for text in &self.lines {
                let mut line = ansi::normalized_line(text);
                line.spans = ansi::display_spans(line.spans);
                let height = Paragraph::new(line)
                    .wrap(Wrap { trim: false })
                    .line_count(area.width)
                    .max(1);
                self.starts.push(self.starts.last().unwrap() + height);
            }
            self.width = area.width;
        }
        let cursor_start = self.starts[self.cursor.min(self.lines.len())];
        let cursor_end = self
            .starts
            .get(self.cursor + 1)
            .copied()
            .unwrap_or(cursor_start);
        if std::mem::take(&mut self.reveal) {
            if cursor_start < self.offset {
                self.offset = cursor_start;
            } else if cursor_end > self.offset + self.height {
                self.offset = cursor_end.saturating_sub(self.height).min(cursor_start);
            }
        }
        self.offset = self.offset.min(self.max_offset());
        let first = self.line_at(self.offset);
        let within = self.offset - self.starts[first];
        // Each line takes at least one screen row, so no later line is visible.
        let lines: Vec<_> = (first..self.lines.len().min(first + self.height))
            .map(|index| {
                let mut line = ansi::normalized_line(&self.lines[index]);
                if index == self.cursor {
                    line.style = line.style.bg(Color::DarkGray);
                }
                if let Some(query) = query {
                    line = crate::ui::highlight_matches(line, query, current == Some(index));
                }
                // Search ranges refer to the original bytes; expand tabs after.
                line.spans = ansi::display_spans(std::mem::take(&mut line.spans));
                line
            })
            .collect();
        frame.render_widget(
            Paragraph::new(Text::from(lines))
                .scroll((within.min(u16::MAX as usize) as u16, 0))
                .wrap(Wrap { trim: false }),
            area,
        );
        // Fill the cursor line's unused columns, including its continuations.
        for screen in cursor_start.max(self.offset)..cursor_end.min(self.offset + self.height) {
            let y = area.y + (screen - self.offset) as u16;
            for x in area.x..area.right() {
                let cell = &mut frame.buffer_mut()[(x, y)];
                if cell.bg == Color::Reset {
                    cell.bg = Color::DarkGray;
                }
            }
        }
    }
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

    fn view(app: &App) -> &View {
        app.range_diff_view.as_ref().unwrap()
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
            assert_eq!(app.history, History::RangeDiff);
            assert!(!app.has_log_view());
            assert_eq!(view(&app).lines, expected.lines().collect::<Vec<_>>());
            let mut terminal = Terminal::new(TestBackend::new(width, 10)).unwrap();
            terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
            mouse(MouseEventKind::ScrollDown, 3, &mut app);
            terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
            assert_eq!(view(&app).offset, 3);
            mouse(MouseEventKind::ScrollUp, 3, &mut app);
            terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
            assert_eq!(view(&app).offset, 0);
            for _ in 0..100 {
                mouse(MouseEventKind::ScrollDown, 3, &mut app);
            }
            terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
            assert_eq!(
                view(&app).offset,
                view(&app).starts.last().unwrap() - view(&app).height
            );
            key(KeyCode::Home, &mut app);
            terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
            assert_eq!(view(&app).offset, 0);
            key(KeyCode::Char(' '), &mut app);
            terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
            assert_eq!(view(&app).offset, view(&app).height);
            key(KeyCode::Char('/'), &mut app);
            for c in "new36_operation".chars() {
                key(KeyCode::Char(c), &mut app);
            }
            key(KeyCode::Enter, &mut app);
            terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
            let found = view(&app).cursor;
            assert!(ansi::plain(&view(&app).lines[found]).contains("new36_operation"));
            let start = view(&app).starts[found];
            assert!((view(&app).offset..view(&app).offset + view(&app).height).contains(&start));
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
            assert_eq!(app.mode, Mode::RangeDiff);
            assert!(!app.watch && app.status.is_none() && app.pending_editor.is_none());
            let row = view(&app).origin + 1;
            mouse(MouseEventKind::Down(MouseButton::Left), row, &mut app);
            assert_eq!(view(&app).cursor, view(&app).line_at(view(&app).offset + 1));
            key(KeyCode::Char('h'), &mut app);
            terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
            let offset = view(&app).offset;
            mouse(MouseEventKind::ScrollDown, 3, &mut app);
            assert_eq!(view(&app).offset, offset);
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
        let lines = |values: &[&str]| view(&load_app(&args(values)).unwrap()).lines.clone();
        let expected = lines(&["base..before", "base..after"]);
        assert!(!expected.is_empty());
        assert_eq!(lines(&["base", "before", "after"]), expected);
        assert_eq!(lines(&["before...after"]), expected);
        let plain: Vec<_> = expected.iter().map(|line| ansi::plain(line)).collect();
        assert_ne!(plain, expected);
        assert_eq!(lines(&["--no-color", "base..before", "base..after"]), plain);
        assert!(lines(&["base..before", "base..after", "--", "absent"]).is_empty());
        assert!(load_app(&[]).is_err());
        assert!(load_app(&args(&["missing..before", "base..after"])).is_err());
    }
}
