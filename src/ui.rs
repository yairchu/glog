use crate::{
    ansi,
    app::{App, Mode},
};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Clear, Paragraph, Tabs, Wrap},
    Frame,
};

pub fn draw(frame: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(frame.area());
    let selected = if app.mode == Mode::Log { 0 } else { 1 };
    let header_style = Style::default().fg(Color::White).bg(Color::DarkGray);
    let tabs = Tabs::new(["Log", "Show"])
        .select(selected)
        .style(header_style)
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_widget(Block::default().style(header_style), chunks[0]);
    let command = if app.context.is_empty() {
        " glog ".to_owned()
    } else {
        format!(" glog {} ", app.context)
    };
    let reserved = 13 + if app.watch { 8 } else { 0 };
    let command_width =
        (command.chars().count() as u16).min(chunks[0].width.saturating_sub(reserved));
    let header = Layout::horizontal([
        Constraint::Length(command_width),
        Constraint::Length(13),
        Constraint::Min(0),
        Constraint::Length(if app.watch { 8 } else { 0 }),
    ])
    .split(chunks[0]);
    app.log_tab_start = header[1].x;
    app.log_tab_end = header[1].x.saturating_add(5);
    app.show_tab_start = header[1].x.saturating_add(6);
    app.show_tab_end = header[1].x.saturating_add(12);
    frame.render_widget(
        Paragraph::new(command).style(header_style.add_modifier(Modifier::BOLD)),
        header[0],
    );
    frame.render_widget(tabs, header[1]);
    if app.watch {
        frame.render_widget(
            Paragraph::new(" WATCH  ").style(
                header_style
                    .fg(Color::LightGreen)
                    .add_modifier(Modifier::BOLD),
            ),
            header[3],
        );
    }
    match app.mode {
        Mode::Log => draw_log(frame, app, chunks[1]),
        Mode::Show => draw_show(frame, app, chunks[1]),
    }
    let help = if let Some(input) = &app.search_input {
        let prefix = if app.search_reverse { '?' } else { '/' };
        format!("{prefix}{input}█")
    } else if let Some(status) = &app.status {
        status.clone()
    } else if app.mode == Mode::Log {
        "↑/k ↓/j  Enter show  a author  d date  r refs  x hash  s subject  / ? search  h help  q quit".to_owned()
    } else {
        "↑/k ↓/j  ←/→ commit  [/ ] file  Enter/z fold  L lockfiles  / ? search  h help  q quit"
            .to_owned()
    };
    frame.render_widget(
        Paragraph::new(help).style(Style::default().fg(Color::Gray)),
        chunks[2],
    );
    if app.show_help {
        draw_help(frame);
    }
}

fn draw_help(frame: &mut Frame) {
    let screen = frame.area();
    let width = screen.width.saturating_sub(4).min(68);
    let height = screen.height.saturating_sub(2).min(25);
    let area = Rect::new(
        screen.x + screen.width.saturating_sub(width) / 2,
        screen.y + screen.height.saturating_sub(height) / 2,
        width,
        height,
    );
    let text = [
        "Navigation",
        "  ↑/k, ↓/j          previous / next; move Show cursor",
        "  Page Up/b         page up",
        "  Page Down/Space/f page down (f in Show)",
        "  ←/→                previous / next commit (Show)",
        "  [, ]              previous / next changed file (Show)",
        "  g, G              top / bottom",
        "",
        "  a/d/r/x/s         toggle author/date/refs/hash/subject (Log)",
        "  Author badges: +꩜ Codex  +❋ Claude Code  +N other coauthors",
        "Views and search",
        "  Enter             open commit / toggle folded file",
        "  z                 toggle current file fold (Show)",
        "  L                 expand / fold all lockfiles (Show)",
        "  Escape            return to Log / cancel",
        "  Tab               switch Log / Show",
        "  /, ?              search forward / backward",
        "  n, N              repeat / reverse search",
        "  Ctrl-L            redraw the screen",
        "",
        "  h                 close help",
        "  q                 quit (or close help)",
    ]
    .join("\n");
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(text).block(Block::bordered().title(" Help ")),
        area,
    );
}

fn draw_log(frame: &mut Frame, app: &mut App, area: Rect) {
    let height = area.height as usize;
    app.log_row_origin = area.y;
    app.visible_log_rows.clear();
    if app.commits.is_empty() {
        frame.render_widget(
            Paragraph::new("No commits matched the supplied arguments."),
            area,
        );
        return;
    }
    let separator_before = app
        .commits
        .iter()
        .position(|commit| commit.kind == crate::git::CommitKind::Revision)
        .filter(|index| *index > 0);
    let selected_start_row: usize = app
        .commits
        .iter()
        .take(app.selected)
        .map(|commit| commit.graph.len())
        .sum::<usize>()
        + usize::from(separator_before.is_some_and(|index| app.selected >= index));
    let selected_subject_row =
        selected_start_row + app.commits[app.selected].graph.len().saturating_sub(1);
    if selected_subject_row < app.log_offset {
        app.log_offset = selected_start_row;
    } else if selected_subject_row >= app.log_offset + height.max(1) {
        app.log_offset = selected_subject_row + 1 - height.max(1);
    }
    let mut lines = Vec::new();
    let mut graph_row = 0;
    'commits: for (index, commit) in app.commits.iter().enumerate() {
        if separator_before == Some(index) {
            if graph_row >= app.log_offset {
                if lines.len() >= height {
                    break;
                }
                let label = " committed history ";
                let suffix = "─".repeat((area.width as usize).saturating_sub(label.len() + 2));
                lines.push(
                    ratatui::text::Line::from(format!("──{label}{suffix}"))
                        .style(Style::default().fg(Color::DarkGray)),
                );
                app.visible_log_rows.push(None);
            }
            graph_row += 1;
        }
        for (part, graph) in commit.graph.iter().enumerate() {
            if graph_row < app.log_offset {
                graph_row += 1;
                continue;
            }
            if lines.len() >= height {
                break 'commits;
            }
            let mut line = ansi::parse_line(graph);
            if part + 1 == commit.graph.len() {
                line.spans.extend(app.log_format.spans(commit));
            }
            if let Some(query) = &app.search {
                let current = app.search_match == Some((Mode::Log, index));
                line = highlight_matches(line, query, current);
            }
            if index == app.selected {
                line.style = Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD);
            }
            lines.push(line);
            app.visible_log_rows.push(Some(index));
            graph_row += 1;
        }
    }
    frame.render_widget(Paragraph::new(Text::from(lines)), area);
}

fn draw_show(frame: &mut Frame, app: &mut App, area: Rect) {
    app.ensure_show_rows();
    app.show_row_origin = area.y;
    app.visible_show_rows = area.height as usize;
    let mut lines: Vec<_> = app
        .show_rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            let mut line = if row.fold_separator {
                Line::from("━".repeat(area.width as usize))
            } else {
                ansi::normalized_line(&row.text)
            };
            if row.folded {
                line.style = Style::default()
                    .fg(Color::Black)
                    .bg(Color::LightYellow)
                    .add_modifier(Modifier::BOLD);
            } else if row.fold_separator {
                line.style = Style::default().fg(Color::Yellow);
            }
            if index == app.show_cursor {
                line.style = line.style.bg(Color::DarkGray);
                if row.folded {
                    line.style = line.style.fg(Color::LightYellow);
                }
                for span in &mut line.spans {
                    if let Some(bg) = span.style.bg {
                        span.style.bg = Some(brighten_background(bg));
                    }
                }
                let padding = usize::from(area.width).saturating_sub(line.width());
                let background = line
                    .spans
                    .last()
                    .and_then(|span| span.style.bg)
                    .unwrap_or(Color::DarkGray);
                if line.width() > 0 && padding > 0 {
                    line.spans.push(Span::styled(
                        " ".repeat(padding),
                        Style::default().bg(background),
                    ));
                }
            }
            line
        })
        .collect();
    if let Some(query) = &app.search {
        lines = lines
            .into_iter()
            .enumerate()
            .map(|(index, line)| {
                let current = app.search_match == Some((Mode::Show, index));
                highlight_matches(line, query, current)
            })
            .collect();
    }
    let max = lines.len().saturating_sub(area.height as usize);
    if app.show_cursor < app.show_offset {
        app.show_offset = app.show_cursor;
    } else if app.show_cursor >= app.show_offset + app.visible_show_rows.max(1) {
        app.show_offset = app.show_cursor + 1 - app.visible_show_rows.max(1);
    }
    app.show_offset = app.show_offset.min(max);
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .scroll((app.show_offset.min(u16::MAX as usize) as u16, 0))
            .wrap(Wrap { trim: false }),
        area,
    );
    // Empty lines have no spans to carry the cursor background.
    if let Some(y) = app.show_cursor.checked_sub(app.show_offset) {
        if y < usize::from(area.height) {
            let y = area.y + y as u16;
            for x in area.x..area.right() {
                let cell = &mut frame.buffer_mut()[(x, y)];
                if cell.bg == Color::Reset {
                    cell.bg = Color::DarkGray;
                }
            }
        }
    }
}

fn brighten_background(color: Color) -> Color {
    match color {
        Color::Reset | Color::Black => Color::DarkGray,
        Color::Red => Color::LightRed,
        Color::Green => Color::LightGreen,
        Color::Yellow => Color::LightYellow,
        Color::Blue => Color::LightBlue,
        Color::Magenta => Color::LightMagenta,
        Color::Cyan => Color::LightCyan,
        Color::Gray => Color::White,
        Color::Rgb(r, g, b) => Color::Rgb(
            r.saturating_add(32),
            g.saturating_add(32),
            b.saturating_add(32),
        ),
        Color::Indexed(index) => match index {
            0..=7 => Color::Indexed(index + 8),
            8..=15 => color,
            16..=231 => {
                let index = index - 16;
                let levels = [0, 95, 135, 175, 215, 255];
                brighten_background(Color::Rgb(
                    levels[usize::from(index / 36)],
                    levels[usize::from(index / 6 % 6)],
                    levels[usize::from(index % 6)],
                ))
            }
            232..=255 => {
                let level = 8 + (index - 232) * 10;
                brighten_background(Color::Rgb(level, level, level))
            }
        },
        _ => color,
    }
}

fn highlight_matches(mut line: Line<'static>, query: &str, current: bool) -> Line<'static> {
    if query.is_empty() {
        return line;
    }
    let visible = line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    let folded_visible = visible.to_lowercase();
    let folded_query = query.to_lowercase();
    let ranges: Vec<_> =
        if folded_visible.len() == visible.len() && folded_query.len() == query.len() {
            folded_visible
                .match_indices(&folded_query)
                .map(|(start, matched)| start..start + matched.len())
                .collect()
        } else {
            visible
                .match_indices(query)
                .map(|(start, matched)| start..start + matched.len())
                .collect()
        };
    if ranges.is_empty() {
        return line;
    }

    let highlight = if current {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(Color::LightYellow)
            .add_modifier(Modifier::UNDERLINED)
    };
    let mut highlighted = Vec::new();
    let mut span_start = 0;
    for span in line.spans {
        let content = span.content.into_owned();
        let span_end = span_start + content.len();
        let mut cursor = 0;
        for range in ranges
            .iter()
            .filter(|range| range.start < span_end && range.end > span_start)
        {
            let start = range.start.max(span_start) - span_start;
            let end = range.end.min(span_end) - span_start;
            if cursor < start {
                highlighted.push(Span::styled(content[cursor..start].to_owned(), span.style));
            }
            highlighted.push(Span::styled(
                content[start..end].to_owned(),
                span.style.patch(highlight),
            ));
            cursor = end;
        }
        if cursor < content.len() {
            highlighted.push(Span::styled(content[cursor..].to_owned(), span.style));
        }
        span_start = span_end;
    }
    line.spans = highlighted;
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::{Commit, CommitKind};
    use crate::input::handle;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{backend::TestBackend, Terminal};

    fn commit(subject: &str, graph_rows: usize) -> Commit {
        Commit {
            kind: CommitKind::Revision,
            hash: subject.repeat(40).chars().take(40).collect(),
            short_hash: subject.to_owned(),
            decorations: String::new(),
            author: String::new(),
            author_email: String::new(),
            author_date: String::new(),
            collaborators: crate::git::Collaborators::default(),
            subject: subject.to_owned(),
            graph: vec!["* ".to_owned(); graph_rows],
        }
    }

    #[test]
    fn collaborator_badges_have_distinct_colors_on_selected_rows() {
        for (codex, claude, others, glyph, color) in [
            (true, false, 0, "꩜", Color::Reset),
            (false, true, 0, "❋", Color::Rgb(215, 119, 87)),
            (false, false, 1, "1", Color::Gray),
            (true, true, 0, "2", Color::Gray),
            (false, true, 1, "2", Color::Gray),
        ] {
            let mut c = crate::log_format::tests::commit();
            c.collaborators = crate::git::Collaborators {
                codex,
                claude,
                others,
            };
            let mut app = App::new(vec![c]);
            app.log_format = crate::log_format::LogFormat::parse("%an").unwrap();
            let mut terminal = Terminal::new(TestBackend::new(80, 6)).unwrap();
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            for (glyph, color) in [(glyph, color), ("+", Color::Rgb(160, 160, 160))] {
                let cell = (0..80)
                    .map(|x| &terminal.backend().buffer()[(x, app.log_row_origin)])
                    .find(|cell| cell.symbol() == glyph)
                    .unwrap();
                assert_eq!(cell.fg, color);
                assert_eq!(cell.bg, Color::DarkGray);
            }
            assert_eq!(
                (0..80)
                    .filter(
                        |&x| terminal.backend().buffer()[(x, app.log_row_origin)].symbol() == "+"
                    )
                    .count(),
                1
            );
        }
    }

    #[test]
    fn log_fields_render_with_color_and_toggle_without_changing_selection() {
        let mut app = App::new(vec![crate::log_format::tests::commit()]);
        app.log_format = crate::log_format::LogFormat::parse("%h [%ad] (%an) %s").unwrap();
        let mut terminal = Terminal::new(TestBackend::new(80, 6)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let row = app.log_row_origin;
        let text = |terminal: &Terminal<TestBackend>| {
            (0..80)
                .map(|x| terminal.backend().buffer()[(x, row)].symbol())
                .collect::<String>()
        };
        assert!(text(&terminal).contains("abcdef0 [2026-09-14] (Alice) A subject"));
        assert_eq!(terminal.backend().buffer()[(2, row)].fg, Color::Yellow);
        handle(
            Event::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)),
            &mut app,
        );
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(text(&terminal).contains("abcdef0 [2026-09-14] A subject"));
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn selected_commit_stays_visible_with_multiline_graph_rows() {
        let backend = TestBackend::new(40, 6);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new(vec![
            commit("merge", 3),
            commit("middle", 1),
            commit("selected", 1),
        ]);
        app.selected = 2;

        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        assert!(
            app.log_offset > 0,
            "viewport did not reveal selected commit"
        );
    }

    #[test]
    fn working_tree_entries_are_separated_from_committed_history() {
        let backend = TestBackend::new(40, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut worktree = commit("Unstaged changes", 1);
        worktree.kind = CommitKind::Unstaged;
        let mut app = App::new(vec![worktree, commit("first commit", 1)]);

        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        assert_eq!(app.visible_log_rows, [Some(0), None, Some(1)]);
    }

    #[test]
    fn show_renders_indented_blank_message_line_once() {
        let backend = TestBackend::new(40, 6);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new(Vec::new());
        app.mode = Mode::Show;
        app.show_text = "title\n    \nbody".to_owned();

        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let body = (0..4)
            .map(|x| terminal.backend().buffer()[(x, 3)].symbol())
            .collect::<Vec<_>>()
            .concat();
        assert_eq!(body, "body");
    }

    #[test]
    fn keyboard_expands_a_folded_file_in_the_last_screenful() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new(Vec::new());
        app.mode = Mode::Show;
        app.show_text = "commit metadata\ndiff --git a/new.txt b/new.txt\nnew file mode 100644\n--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1 @@\n+added\ndiff --git a/old.txt b/old.txt\n--- a/old.txt\n+++ b/old.txt\n@@ -1 +1 @@\n-before\n+after\n"
            .to_owned();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(!app.show_rows.iter().any(|row| row.text == "+added"));

        let key = |code| Event::Key(KeyEvent::new(code, KeyModifiers::NONE));
        handle(key(KeyCode::Char(']')), &mut app);
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(app.show_rows[app.show_cursor].folded);
        let cursor_y = 1 + app.show_cursor - app.show_offset;
        let cursor = &terminal.backend().buffer()[(0, cursor_y as u16)];
        assert_eq!(cursor.bg, Color::DarkGray);
        assert_eq!(cursor.fg, Color::LightYellow);
        assert!(!cursor.modifier.contains(Modifier::REVERSED));
        handle(key(KeyCode::Enter), &mut app);

        assert!(app.show_rows.iter().any(|row| row.text == "+added"));
    }

    #[test]
    fn repeated_show_search_scrolls_only_to_reveal_hidden_matches() {
        let mut terminal = Terminal::new(TestBackend::new(40, 6)).unwrap();
        let mut app = App::new(Vec::new());
        app.mode = Mode::Show;
        app.show_text =
            "zero\nneedle one\ntwo\nneedle three\nfour\nfive\nneedle six\nseven\neight".to_owned();
        app.ensure_show_rows();
        app.show_cursor = 1;
        app.show_offset = 1;
        app.search = Some("needle".to_owned());
        app.search_match = Some((Mode::Show, 1));
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        for (key, cursor, offset) in [
            ('n', 3, 1), // Already visible.
            ('n', 6, 3), // Reveal below the viewport.
            ('N', 3, 3), // Already visible at its top edge.
            ('N', 1, 1), // Reveal above the viewport.
            ('N', 6, 3), // Wrap to the last match.
            ('n', 1, 1), // Wrap to the first match.
        ] {
            handle(
                Event::Key(KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE)),
                &mut app,
            );
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();

            assert_eq!(app.show_cursor, cursor);
            assert_eq!(app.show_offset, offset);
            let matched_cell = &terminal.backend().buffer()[(0, 1 + (cursor - offset) as u16)];
            assert_eq!(matched_cell.symbol(), "n");
            assert_eq!(matched_cell.bg, Color::Yellow);
        }
    }

    #[test]
    fn show_cursor_fills_row_and_brightens_diff_backgrounds() {
        for (input, expected) in [
            ("plain", Color::DarkGray),
            ("", Color::DarkGray),
            ("\x1b[48;2;0;48;0m+added", Color::Rgb(32, 80, 32)),
            ("\x1b[48;2;48;0;0m-removed", Color::Rgb(80, 32, 32)),
            ("\x1b[48;5;22m+added", Color::Rgb(32, 127, 32)),
        ] {
            let mut terminal = Terminal::new(TestBackend::new(40, 6)).unwrap();
            let mut app = App::new(Vec::new());
            app.mode = Mode::Show;
            app.show_text = format!("{input}\nnext");

            terminal.draw(|frame| draw(frame, &mut app)).unwrap();

            let buffer = terminal.backend().buffer();
            for x in 0..40 {
                assert_eq!(buffer[(x, 1)].bg, expected, "input={input:?}, x={x}");
                assert!(!buffer[(x, 1)].modifier.contains(Modifier::REVERSED));
            }
            assert_eq!(buffer[(39, 2)].bg, Color::Reset);
        }
    }

    #[test]
    fn show_cursor_preserves_current_search_highlight() {
        let mut terminal = Terminal::new(TestBackend::new(40, 6)).unwrap();
        let mut app = App::new(Vec::new());
        app.mode = Mode::Show;
        app.show_text = "\x1b[48;2;0;48;0m+added".to_owned();
        app.search = Some("added".to_owned());
        app.search_match = Some((Mode::Show, 0));

        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 1)].bg, Color::Rgb(32, 80, 32));
        for x in 1..6 {
            assert_eq!(buffer[(x, 1)].fg, Color::Black);
            assert_eq!(buffer[(x, 1)].bg, Color::Yellow);
            assert!(!buffer[(x, 1)].modifier.contains(Modifier::REVERSED));
        }
        assert_eq!(buffer[(39, 1)].bg, Color::Rgb(32, 80, 32));
    }

    #[test]
    fn search_highlight_crosses_ansi_style_spans() {
        let line = Line::from(vec![
            Span::styled("Nee", Style::default().fg(Color::Red)),
            Span::styled("dle", Style::default().fg(Color::Blue)),
        ]);

        let highlighted = highlight_matches(line, "needle", true);

        assert_eq!(highlighted.spans.len(), 2);
        assert!(highlighted
            .spans
            .iter()
            .all(|span| span.style.bg == Some(Color::Yellow)));
        assert_eq!(highlighted.spans[0].style.fg, Some(Color::Black));
        assert_eq!(highlighted.spans[1].style.fg, Some(Color::Black));
    }

    #[test]
    fn non_current_search_matches_are_secondary() {
        let highlighted = highlight_matches(Line::from("needle"), "needle", false);

        assert_eq!(highlighted.spans[0].style.fg, Some(Color::LightYellow));
        assert_eq!(highlighted.spans[0].style.bg, None);
        assert!(highlighted.spans[0]
            .style
            .add_modifier
            .contains(Modifier::UNDERLINED));
    }

    #[test]
    fn header_shows_the_git_log_context() {
        let backend = TestBackend::new(50, 6);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut app = App::new(Vec::new());
        app.context = "origin/main.. -- src/".to_owned();

        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let header = (0..50)
            .map(|x| terminal.backend().buffer()[(x, 0)].symbol())
            .collect::<Vec<_>>()
            .concat();
        assert!(header.contains("origin/main.. -- src/"));
        assert!(app.log_tab_start > 7);
    }
}
#[cfg(test)]
mod release_review_tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};
    #[test]
    fn wrapped_preceding_line_does_not_get_cursor_background() {
        let mut terminal = Terminal::new(TestBackend::new(10, 8)).unwrap();
        let mut app = App::new(Vec::new());
        app.mode = Mode::Show;
        app.show_text = "abcdefghijklmno\nselected\nafter".to_owned();
        app.ensure_show_rows();
        app.show_cursor = 1;
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert_eq!(
            terminal.backend().buffer()[(0, 2)].bg,
            Color::Reset,
            "Continuation of unselected first line must not look selected"
        );
    }
    #[test]
    fn repeated_search_reveals_match_after_wrapped_lines() {
        let mut terminal = Terminal::new(TestBackend::new(10, 6)).unwrap();
        let mut app = App::new(Vec::new());
        app.mode = Mode::Show;
        app.show_text =
            "needle 0\nabcdefghijklmno\nabcdefghijklmno\nneedle 3\nfour\nfive\nsix".to_owned();
        app.ensure_show_rows();
        app.search = Some("needle".to_owned());
        app.search_match = Some((Mode::Show, 0));
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        app.repeat_search(false);
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let rows: Vec<String> = (1..5)
            .map(|y| (0..10).map(|x| buffer[(x, y)].symbol()).collect())
            .collect();
        assert!(
            rows.iter().any(|r| r.contains("needle 3")),
            "Match not visible: {rows:?}"
        );
    }
}
