use crate::{
    ansi,
    app::{App, LogFilterAction, Mode, ShowScroll},
};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Clear, Paragraph, Tabs, Wrap},
    Frame,
};

pub fn draw(frame: &mut Frame, app: &mut App) {
    app.type_buttons.clear();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(frame.area());
    let has_log = app.has_log_view();
    let selected = match app.mode {
        Mode::Log => 0,
        Mode::Show => 1,
        Mode::Status => 1,
    };
    let header_style = Style::default().fg(Color::White).bg(Color::DarkGray);
    let detail = if app.mode == Mode::Status
        || app
            .commits
            .get(app.selected)
            .is_some_and(|commit| commit.kind == crate::git::CommitKind::WorkingTree)
    {
        "Status"
    } else {
        "Show"
    };
    let tab_width: u16 = if !has_log {
        0
    } else if detail == "Status" {
        15
    } else {
        13
    };
    let tabs = Tabs::new(["Log", detail])
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
    let live = app.watch || app.mode == Mode::Status;
    let reserved = tab_width + if live { 8 } else { 0 };
    let command_width =
        (command.chars().count() as u16).min(chunks[0].width.saturating_sub(reserved));
    let header = Layout::horizontal([
        Constraint::Length(command_width),
        Constraint::Length(tab_width),
        Constraint::Min(0),
        Constraint::Length(if live { 8 } else { 0 }),
    ])
    .split(chunks[0]);
    app.log_tab_start = header[1].x;
    app.log_tab_end = header[1].x.saturating_add(if has_log { 5 } else { 0 });
    app.show_tab_start = header[1].x.saturating_add(if has_log { 6 } else { 0 });
    app.show_tab_end = header[1].x.saturating_add(tab_width.saturating_sub(1));
    frame.render_widget(
        Paragraph::new(command).style(header_style.add_modifier(Modifier::BOLD)),
        header[0],
    );
    if has_log {
        frame.render_widget(tabs, header[1]);
    }
    if app.mode == Mode::Log {
        draw_type_controls(frame, app, header[2]);
    }
    if live {
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
        Mode::Status => {
            if let Some(view) = &mut app.status_view {
                view.draw_with_images(frame, chunks[1], &mut app.images);
            }
        }
        Mode::Log => draw_log(frame, app, chunks[1]),
        Mode::Show => draw_show(frame, app, chunks[1]),
    }
    let help = if app.mode == Mode::Status {
        app.status_view.as_ref().and_then(|view| view.error.clone()).or_else(|| app.status.clone()).unwrap_or_else(|| "h help  q quit  ↑/k ↓/j  Enter/z fold  s summary  ←/→ commit  Shift-←/→ pan  Tab Log  Esc Log  Ctrl-L redraw · WATCH".to_owned())
    } else if let Some(input) = &app.search_input {
        let prefix = if app.search_reverse { '?' } else { '/' };
        format!("{prefix}{input}█")
    } else if let Some(status) = &app.status {
        status.clone()
    } else if app.mode == Mode::Log {
        // Keep the hint short by omitting the row format toggles (x hash,
        // s subject); the help screen lists every key.
        "h help  q quit  ↑/k ↓/j  ←/→ commit  Enter show  t hide type  z fold merge  m fold all  a author  d date  r refs  / ? search".to_owned()
    } else if !has_log {
        "h help  q quit  ↑/k ↓/j  [/ ] file  Enter/z fold  s summary  L lockfiles  / ? search"
            .to_owned()
    } else {
        "h help  q quit  ↑/k ↓/j  ←/→ commit  [/ ] file  Enter/z fold  s summary  L lockfiles  / ? search"
            .to_owned()
    };
    frame.render_widget(
        Paragraph::new(help).style(Style::default().fg(Color::Gray)),
        chunks[2],
    );
    if app.show_help {
        draw_help(frame, has_log);
    }
}

fn draw_help(frame: &mut Frame, has_log: bool) {
    let screen = frame.area();
    let width = screen.width.saturating_sub(4).min(68);
    let height = screen.height.saturating_sub(2).min(28);
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
        "  Page Down/Space/f page down",
        "  ←/→                previous / next commit (Log/Show/Status)",
        "  [, ]              previous / next changed file (Show)",
        "  g/<, G/>          top / bottom (also Home/End)",
        "",
        "  a/d/r/x/s         toggle author/date/refs/hash/subject (Log)",
        "  Author badges: +꩜ Codex  +❋ Claude Code  +N other coauthors",
        "Views and search",
        "  Enter             open commit / toggle section or file",
        "  z                 fold merge (Log) / file (Show)",
        "  m                 expand / fold all merges (Log)",
        "  t / T             hide selected type / clear filters (Log)",
        "  M                 hide / show merge commits (Log)",
        "  L                 expand / fold all lockfiles (Show)",
        "  s                 toggle file summary / patch (Show/Status)",
        "  Escape            return to Log / cancel",
        "  Tab               switch Log / detail",
        "  /, ?              search forward / backward",
        "  n, N              repeat / reverse search",
        "  Ctrl-L            redraw the screen",
        "",
        "  h                 close help",
        "  q                 quit (or close help)",
    ]
    .into_iter()
    .filter(|line| {
        has_log
            || line.starts_with("  z ")
            || !(line.contains("Log")
                || line.contains("previous / next commit")
                || line.contains("Author badges"))
    })
    .map(|line| {
        if !has_log && line.contains("open commit") {
            "  Enter             toggle section or file"
        } else if !has_log && line.starts_with("  z ") {
            "  z                 toggle current file fold (Show)"
        } else {
            line
        }
    })
    .collect::<Vec<_>>()
    .join("\n");
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(text).block(Block::bordered().title(" Help ")),
        area,
    );
}

fn draw_type_controls(frame: &mut Frame, app: &mut App, area: Rect) {
    let mut items = Vec::new();
    if !app.log_folds.hidden_types.is_empty() || app.log_folds.hide_merges {
        let count = app.log_folds.hidden_count;
        items.push((
            format!(
                "Hiding {count} commit{} · ",
                if count == 1 { "" } else { "s" }
            ),
            LogFilterAction::Reset,
            false,
        ));
        items.push(("[T show all] ".to_owned(), LogFilterAction::Reset, true));
        for kind in &app.log_folds.hidden_types {
            items.push((
                format!("[{kind} ×] "),
                LogFilterAction::Type(kind.clone()),
                true,
            ));
        }
    }
    if let Some(kind) = app
        .commits
        .get(app.selected)
        .filter(|_| app.log_folds.visible(app.selected))
        .and_then(crate::log_folds::commit_type)
    {
        items.push((
            format!("[t hide {kind}] "),
            LogFilterAction::Type(kind.to_owned()),
            true,
        ));
    }
    if app.log_folds.hide_merges
        || app.commits.get(app.selected).is_some_and(|commit| {
            app.log_folds.visible(app.selected)
                && commit.kind == crate::git::CommitKind::Revision
                && commit.parents.len() > 1
        })
    {
        items.push((
            if app.log_folds.hide_merges {
                "[M show merges] "
            } else {
                "[M hide merges] "
            }
            .to_owned(),
            LogFilterAction::Merges,
            true,
        ));
    }
    if area.is_empty() {
        return;
    }
    let mut x = area.x;
    for (label, kind, clickable) in items {
        let width = (Line::from(label.as_str()).width() as u16).min(area.right() - x);
        if width == 0 {
            break;
        }
        let rect = Rect::new(x, area.y, width, 1);
        frame.render_widget(
            Paragraph::new(label).style(Style::default().bg(Color::DarkGray).fg(if clickable {
                Color::Cyan
            } else {
                Color::Yellow
            })),
            rect,
        );
        if clickable {
            app.type_buttons.push((rect, kind));
        }
        x += width;
    }
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
    if !(0..app.commits.len()).any(|i| app.log_folds.visible(i)) {
        frame.render_widget(
            Paragraph::new("All commits hidden. Press T to clear filters."),
            area,
        );
        return;
    }
    let separator_before = app
        .commits
        .iter()
        .enumerate()
        .find(|(i, commit)| {
            app.log_folds.graph_visible(*i) && commit.kind == crate::git::CommitKind::Revision
        })
        .map(|(i, _)| i)
        .filter(|index| {
            app.commits[..*index]
                .iter()
                .any(|c| c.kind != crate::git::CommitKind::Revision)
        });
    let selected_start_row: usize = app
        .commits
        .iter()
        .enumerate()
        .take(app.selected)
        .filter(|(i, _)| app.log_folds.graph_visible(*i))
        .map(|(i, commit)| app.log_folds.graph(i, commit).len())
        .sum::<usize>()
        + usize::from(separator_before.is_some_and(|index| app.selected >= index));
    let selected_subject_row = selected_start_row
        + app
            .log_folds
            .graph(app.selected, &app.commits[app.selected])
            .len()
            .saturating_sub(1);
    if !app.log_folds.visible(app.selected) {
        app.log_offset = 0;
    } else if selected_subject_row < app.log_offset {
        app.log_offset = selected_start_row;
    } else if selected_subject_row >= app.log_offset + height.max(1) {
        app.log_offset = selected_subject_row + 1 - height.max(1);
    }
    let mut lines = Vec::new();
    let mut graph_row = 0;
    'commits: for (index, commit) in app.commits.iter().enumerate() {
        if !app.log_folds.graph_visible(index) {
            continue;
        }
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
        let graph_only = !app.log_folds.visible(index);
        let graph_rows = app.log_folds.graph(index, commit);
        for (part, graph) in graph_rows.iter().enumerate() {
            if graph_row < app.log_offset {
                graph_row += 1;
                continue;
            }
            if lines.len() >= height {
                break 'commits;
            }
            let mut line = ansi::parse_line(graph);
            if !graph_only && part + 1 == graph_rows.len() {
                if let Some(marker) = app.log_folds.marker(index) {
                    if let Some(span) = line
                        .spans
                        .iter_mut()
                        .find(|span| span.content.contains('*'))
                    {
                        span.content = span.content.replacen('*', marker, 1).into();
                    }
                }
                line.spans.extend(app.log_format.spans(commit));
                line.spans.push(Span::styled(
                    app.log_folds.label(index),
                    Style::default().fg(Color::Yellow),
                ));
            }
            if let Some(query) = &app.search {
                let current = app.search_match == Some((Mode::Log, index));
                line = highlight_matches(line, query, current);
            }
            if !graph_only && index == app.selected {
                line.style = Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD);
            }
            lines.push(line);
            app.visible_log_rows
                .push((!graph_only && part + 1 == graph_rows.len()).then_some(index));
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
            } else if row.summary {
                summary_line(&row.text)
            } else {
                ansi::normalized_line(&row.text)
            };
            if row.file.is_none() {
                let text = line.to_string();
                if text == "Notes:" || (text.starts_with("Notes (") && text.ends_with("):")) {
                    for span in &mut line.spans {
                        span.style = span.style.add_modifier(Modifier::BOLD);
                    }
                }
            }
            if row.folded || row.summary {
                line.style = Style::default()
                    .fg(Color::Yellow)
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
            }
            line
        })
        .collect();
    let cursor_background = lines
        .get(app.show_cursor)
        .and_then(|line| line.spans.last())
        .and_then(|span| span.style.bg)
        .unwrap_or(Color::DarkGray);
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
    // Use Ratatui's own wrapping rules, including word boundaries and wide glyphs.
    let mut starts = Vec::with_capacity(lines.len() + 1);
    starts.push(0);
    for line in &lines {
        let height = Paragraph::new(line.clone())
            .wrap(Wrap { trim: false })
            .line_count(area.width)
            .max(1);
        starts.push(starts.last().unwrap() + height);
    }
    app.show_row_starts = starts;
    let cursor_start = app.show_row_starts[app.show_cursor.min(lines.len())];
    let cursor_end = app
        .show_row_starts
        .get(app.show_cursor + 1)
        .copied()
        .unwrap_or(cursor_start);
    let height = usize::from(area.height).max(1);
    let max = app
        .show_row_starts
        .last()
        .copied()
        .unwrap_or(0)
        .saturating_sub(height);
    let request = app.show_scroll.take();
    let matched_rows = if matches!(request, Some(ShowScroll::Search)) {
        lines.get(app.show_cursor).and_then(|line| {
            app.search.as_deref().and_then(|query| {
                wrapped_match_rows(line, query, area.width, cursor_end - cursor_start)
            })
        })
    } else {
        None
    };
    match request {
        Some(ShowScroll::PreserveCursorPosition(position)) => {
            app.show_offset = cursor_start.saturating_add_signed(-position);
        }
        Some(ShowScroll::Bottom) => app.show_offset = max,
        Some(ShowScroll::Cursor) => app.show_offset = cursor_start,
        _ if matched_rows.is_some() => {
            let rows = matched_rows.unwrap();
            let start = cursor_start + rows.start;
            let end = cursor_start + rows.end;
            if start < app.show_offset {
                app.show_offset = start;
            } else if end > app.show_offset + height {
                app.show_offset = end.saturating_sub(height).min(start);
            }
        }
        _ if cursor_end <= app.show_offset => app.show_offset = cursor_start,
        _ if cursor_start >= app.show_offset + height => {
            app.show_offset = cursor_start + 1 - height;
        }
        _ => {}
    }
    app.show_offset = app.show_offset.min(max);
    // Skip complete logical rows first so scrolling is not limited to u16::MAX.
    let first = app
        .show_row_starts
        .partition_point(|&start| start <= app.show_offset)
        .saturating_sub(1)
        .min(lines.len());
    let within = app.show_offset - app.show_row_starts[first];
    frame.render_widget(
        Paragraph::new(Text::from(
            lines.into_iter().skip(first).collect::<Vec<_>>(),
        ))
        .scroll((within.min(u16::MAX as usize) as u16, 0))
        .wrap(Wrap { trim: false }),
        area,
    );
    for (index, row) in app.show_rows.iter().enumerate() {
        if let Some(preview) = &row.preview {
            let screen = app.show_row_starts[index];
            if screen >= app.show_offset && screen < app.show_offset + usize::from(area.height) {
                app.images.render(
                    frame,
                    preview,
                    Rect::new(
                        area.x,
                        area.y + (screen - app.show_offset) as u16,
                        area.width,
                        1,
                    ),
                    0,
                );
            }
        }
    }
    // Fill empty lines and the unused columns of every visible cursor continuation.
    for screen in cursor_start.max(app.show_offset)
        ..cursor_end.min(app.show_offset + usize::from(area.height))
    {
        let y = area.y + (screen - app.show_offset) as u16;
        for x in area.x..area.right() {
            let cell = &mut frame.buffer_mut()[(x, y)];
            if cell.bg == Color::Reset {
                cell.bg = cursor_background;
            }
        }
    }
}

pub(crate) fn summary_line(text: &str) -> Line<'static> {
    if let Some((prefix, stats)) = text.rsplit_once(" | ") {
        if let Some((added, deleted)) = stats.split_once(' ') {
            if added
                .strip_prefix('+')
                .is_some_and(|count| count.parse::<usize>().is_ok())
                && deleted
                    .strip_prefix('−')
                    .is_some_and(|count| count.parse::<usize>().is_ok())
            {
                return Line::from(vec![
                    Span::raw(format!("{prefix} | ")),
                    Span::styled(added.to_owned(), Style::default().fg(Color::Green)),
                    Span::raw(" "),
                    Span::styled(deleted.to_owned(), Style::default().fg(Color::Red)),
                ]);
            }
        }
    }
    ansi::normalized_line(text)
}

// Render only on a search request, using the same word wrapping as the viewport.
// Probe in small chunks so a long minified line does not allocate a huge buffer.
fn wrapped_match_rows(
    line: &Line<'_>,
    query: &str,
    width: u16,
    height: usize,
) -> Option<std::ops::Range<usize>> {
    use ratatui::{buffer::Buffer, widgets::Widget};

    if width == 0 {
        return None;
    }
    let text = line
        .spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>();
    let mut marked = highlight_matches(Line::raw(text), query, true);
    // Search navigation selects a logical line; reveal its first occurrence.
    let mut found = false;
    for span in &mut marked.spans {
        if span.style.bg == Some(Color::Yellow) && !found {
            found = true;
        } else {
            span.style = Style::default();
        }
    }
    if !found {
        return None;
    }
    let paragraph = Paragraph::new(marked).wrap(Wrap { trim: false });
    let mut rows: Option<std::ops::Range<usize>> = None;
    for offset in (0..height.min(usize::from(u16::MAX) + 1)).step_by(64) {
        let area = Rect::new(0, 0, width, (height - offset).min(64) as u16);
        let mut buffer = Buffer::empty(area);
        paragraph
            .clone()
            .scroll((offset as u16, 0))
            .render(area, &mut buffer);
        for y in 0..area.height {
            if (0..width).any(|x| buffer[(x, y)].bg == Color::Yellow) {
                let screen = offset + usize::from(y);
                rows.get_or_insert(screen..screen + 1).end = screen + 1;
            } else if rows.is_some() {
                return rows;
            }
        }
    }
    rows
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

    #[test]
    fn all_hidden_message_ignores_graph_only_merge_rows() {
        // Nested merges hidden by the merge filter still draw junctions,
        // but nothing is selectable once types hide every other commit.
        let base = commit("fix: base", 1);
        let mut outer = commit("Merge outer", 1);
        let mut rows = Vec::new();
        for name in ["left", "right"] {
            let mut first = commit(&format!("fix: {name} first"), 1);
            let mut second = commit(&format!("fix: {name} second"), 1);
            let mut merge = commit(&format!("Merge {name}"), 1);
            first.parents = vec![base.hash.clone()];
            second.parents = vec![base.hash.clone()];
            merge.parents = vec![first.hash.clone(), second.hash.clone()];
            outer.parents.push(merge.hash.clone());
            rows.extend([merge, second, first]);
        }
        rows.insert(0, outer);
        rows.push(base);
        let mut app = App::new(rows);
        app.apply_log_filter(LogFilterAction::Merges);
        app.apply_log_filter(LogFilterAction::Type("fix".into()));
        assert!((0..app.commits.len()).all(|i| !app.log_folds.visible(i)));
        let mut terminal = Terminal::new(TestBackend::new(60, 10)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(screen.contains("All commits hidden. Press T to clear filters."));
    }

    #[test]
    fn filters_keep_folded_merges_that_stand_in_for_their_history() {
        for action in [
            LogFilterAction::Merges,
            LogFilterAction::Type("chore".into()),
        ] {
            let mut merge = commit("chore: folded merge", 1);
            let mut side = commit("fix: side", 1);
            let mut main = commit("feat: main", 1);
            let base = commit("base", 1);
            merge.parents = vec![main.hash.clone(), side.hash.clone()];
            main.parents = vec![base.hash.clone()];
            side.parents = vec![base.hash.clone()];
            let mut app = App::new(vec![merge, side, main, base]);
            app.toggle_log_merge();
            assert!(!app.log_folds.visible(1));
            app.apply_log_filter(action);
            assert!(app.log_folds.visible(0));
            assert_eq!(app.log_folds.hidden_count, 0);
            assert_eq!(app.selected, 0);
            let mut terminal = Terminal::new(TestBackend::new(100, 12)).unwrap();
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            let screen: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect();
            assert!(screen.contains("chore: folded merge"));
            assert!(screen.contains("1 merged commit"));
            // Expanding reveals the side history and applies the filter.
            app.toggle_log_merge();
            assert!(!app.log_folds.visible(0));
            assert!(app.log_folds.visible(1));
            assert_eq!(app.log_folds.hidden_count, 1);
            assert_eq!(app.selected, 1);
        }
    }

    #[test]
    fn hidden_merges_keep_noninteractive_graph_junctions_for_both_filters() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        for action in [
            LogFilterAction::Merges,
            LogFilterAction::Type("chore".into()),
        ] {
            let mut merge = commit("chore: hidden merge text", 1);
            let mut side = commit("fix: side", 1);
            let mut main = commit("feat: main", 1);
            let base = commit("base", 1);
            merge.parents = vec![main.hash.clone(), side.hash.clone()];
            main.parents = vec![base.hash.clone()];
            side.parents = vec![base.hash.clone()];
            let mut app = App::new(vec![merge, side, main, base]);
            app.apply_log_filter(action);
            assert_eq!(app.selected, 1);
            assert!(!app.log_folds.visible(0));
            assert!(!app.log_folds.graph_visible(0));
            assert!(app.log_folds.graph(0, &app.commits[0]).is_empty());
            let mut terminal = Terminal::new(TestBackend::new(100, 12)).unwrap();
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            let row = |y| {
                (0..100)
                    .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                    .collect::<String>()
            };
            assert!(row(app.log_row_origin).contains('\\'));
            let screen: String = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect();
            assert!(!screen.contains("hidden merge text"));
            assert!((app.log_row_origin..11).all(|y| !row(y).contains('·')));
            assert!(screen.contains("fix: side") && screen.contains("feat: main"));
            assert!(screen.contains('/') || screen.contains('\\'));
            assert_eq!(app.visible_log_rows[0], None);
            handle(
                Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: 0,
                    row: app.log_row_origin,
                    modifiers: KeyModifiers::NONE,
                }),
                &mut app,
            );
            assert_eq!(app.selected, 1);
            assert_eq!(app.mode, Mode::Log);
            app.top();
            assert_eq!(app.selected, 1);
            app.move_by(1, 1);
            assert_eq!(app.selected, 2);
            terminal.resize(Rect::new(0, 0, 100, 4)).unwrap();
            app.bottom();
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            assert!(app.visible_log_rows.contains(&Some(3)));
            app.apply_log_filter(LogFilterAction::Reset);
            assert!(app.log_folds.visible(0));
        }
    }

    #[test]
    fn merge_filter_keeps_branch_history_and_combines_with_types() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let mut merge = commit("test: merge", 1);
        let mut plain_merge = commit("Merge topic", 1);
        let side = commit("test: side", 1);
        let main = commit("feat: main", 1);
        merge.parents = vec![plain_merge.hash.clone(), side.hash.clone()];
        plain_merge.parents = vec![main.hash.clone(), side.hash.clone()];
        let mut app = App::new(vec![merge, plain_merge, side, main]);
        let mut terminal = Terminal::new(TestBackend::new(120, 10)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let button = app
            .type_buttons
            .iter()
            .find(|(_, a)| *a == LogFilterAction::Merges)
            .unwrap()
            .0;
        handle(
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: button.x,
                row: button.y,
                modifiers: KeyModifiers::NONE,
            }),
            &mut app,
        );
        assert!(app.log_folds.hide_merges);
        assert_eq!(app.selected, 2);
        assert_eq!(app.log_folds.hidden_count, 2);
        assert!(app.log_folds.visible(2) && app.log_folds.visible(3));
        app.search = Some("Merge topic".into());
        app.next_match(false);
        assert_eq!(app.selected, 2);
        app.hide_selected_type();
        assert_eq!(app.log_folds.hidden_count, 3); // Matching merges count once.
        assert_eq!(app.selected, 3);
        app.replace_commits(app.commits.clone());
        assert!(app.log_folds.hide_merges);
        assert_eq!(app.log_folds.hidden_count, 3);
        handle(
            Event::Key(KeyEvent::new(KeyCode::Char('M'), KeyModifiers::NONE)),
            &mut app,
        );
        assert!(!app.log_folds.hide_merges);
        assert!(!app.log_folds.visible(0)); // Type filter still applies.
        assert!(app.log_folds.visible(1));
        assert_eq!(app.log_folds.hidden_count, 2);
        handle(
            Event::Key(KeyEvent::new(KeyCode::Char('M'), KeyModifiers::NONE)),
            &mut app,
        );
        handle(
            Event::Key(KeyEvent::new(KeyCode::Char('T'), KeyModifiers::NONE)),
            &mut app,
        );
        assert!(!app.log_folds.hide_merges);
        assert_eq!(app.log_folds.hidden_count, 0);
        assert!((0..4).all(|i| app.log_folds.visible(i)));
    }

    #[test]
    fn type_filter_mouse_keyboard_search_and_refresh() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let mut app = App::new(vec![
            commit("tests #234(failing): reproduce", 1),
            commit("feat: feature", 1),
            commit("test: coverage", 1),
            commit("ordinary", 1),
        ]);
        let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let click = |app: &mut App, rect: Rect| {
            handle(
                Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: rect.x,
                    row: rect.y,
                    modifiers: KeyModifiers::NONE,
                }),
                app,
            )
        };
        assert_eq!(app.type_buttons[0].1, LogFilterAction::Type("test".into()));
        let button = app.type_buttons[0].0;
        assert_eq!(button.y, 0);
        let origin = app.log_row_origin;
        let rows = app.visible_log_rows.clone();
        app.selected = 3; // Untyped selection must not move the history.
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        assert_eq!(app.log_row_origin, origin);
        assert_eq!(app.visible_log_rows, rows);
        assert!(app.type_buttons.is_empty());
        app.selected = 0;
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        click(&mut app, button);
        assert_eq!(app.selected, 1);
        app.move_by(1, 1);
        assert_eq!(app.selected, 3);
        app.top();
        assert_eq!(app.selected, 1);
        app.search = Some("coverage".into());
        app.next_match(false);
        assert_eq!(app.selected, 1);
        let mut refreshed = app.commits.clone();
        refreshed.insert(0, commit("tests #233 #234: newly arrived", 1));
        app.replace_commits(refreshed);
        assert_eq!(app.selected, 2);
        assert!(!app.log_folds.visible(0));
        assert_eq!(app.log_folds.hidden_count, 3);
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(screen.contains("Hiding 3 commits"));
        assert_eq!(app.log_row_origin, origin);
        assert!(app.type_buttons.iter().all(|(rect, _)| rect.y == 0));
        assert!(!screen.contains("newly arrived"));
        let button = app
            .type_buttons
            .iter()
            .find(|(_, kind)| *kind == LogFilterAction::Type("test".into()))
            .unwrap()
            .0;
        click(&mut app, button);
        assert!(app.log_folds.hidden_types.is_empty());
        assert_eq!(app.log_folds.hidden_count, 0);
        app.top();
        handle(
            Event::Key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE)),
            &mut app,
        );
        assert!(!app.log_folds.visible(0));
        handle(
            Event::Key(KeyEvent::new(KeyCode::Char('T'), KeyModifiers::NONE)),
            &mut app,
        );
        assert!(app.log_folds.visible(0));
    }

    #[test]
    fn all_types_hidden_remains_recoverable_on_small_terminals() {
        for (width, height) in [(80, 8), (24, 6), (1, 3)] {
            let mut app = App::new(vec![commit("test: only commit", 1)]);
            app.hide_selected_type();
            app.switch_mode();
            assert_eq!(app.mode, Mode::Log);
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            assert!(app.visible_log_rows.is_empty());
            handle(
                Event::Key(KeyEvent::new(KeyCode::Char('T'), KeyModifiers::NONE)),
                &mut app,
            );
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            assert!(app.log_folds.visible(0));
        }
    }

    #[test]
    fn log_hint_keeps_help_and_quit_visible_on_narrow_terminals() {
        let mut app = App::new(vec![commit("subject", 1)]);
        let mut terminal = Terminal::new(TestBackend::new(80, 8)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let hint: String = (0..80).map(|x| buffer[(x, 7)].symbol()).collect();
        assert!(hint.contains("h help") && hint.contains("q quit"), "{hint}");
    }

    #[test]
    fn merge_disclosure_replaces_the_graph_node_without_shifting_fields() {
        for (graph, column) in [("\x1b[32m*\x1b[m ", 0), ("| \x1b[32m*\x1b[m ", 2)] {
            let mut entry = commit("merge subject", 1);
            entry.graph = vec!["|\\ ".into(), graph.into()];
            let side = commit("side", 1);
            let side_hash = side.hash.clone();
            let mut app = App::new(vec![entry, side]);
            let mut terminal = Terminal::new(TestBackend::new(100, 8)).unwrap();
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            let before = terminal.backend().buffer().clone();
            // The side commit is the oldest row, so its parent bounds the
            // ancestry and cannot lead back to it.
            app.commits[0].parents = vec!["first".into(), side_hash];
            app.commits[1].parents = vec!["first".into()];
            app.log_folds.refresh(&app.commits).unwrap();
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            let after = terminal.backend().buffer();
            assert_eq!(before[(column, 2)].symbol(), "*");
            assert_eq!(after[(column, 2)].symbol(), "▼");
            assert_eq!(before[(column, 2)].style(), after[(column, 2)].style());
            // The header now offers the merge filter; history stays aligned.
            assert!(app
                .type_buttons
                .iter()
                .any(|(_, action)| *action == LogFilterAction::Merges));
            for y in 1..8 {
                for x in 0..100 {
                    if (x, y) != (column, 2) {
                        assert_eq!(before[(x, y)], after[(x, y)], "cell {x},{y}");
                    }
                }
            }
        }
    }

    fn commit(subject: &str, graph_rows: usize) -> Commit {
        Commit {
            kind: CommitKind::Revision,
            parents: Vec::new(),
            diff_args: Vec::new(),
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
    fn status_footer_shows_app_messages() {
        let mut terminal = Terminal::new(TestBackend::new(80, 6)).unwrap();
        let mut app = App::new(Vec::new());
        app.mode = Mode::Status;
        app.status_view = Some(crate::status::StatusView::default());
        app.status = Some("Watch refresh failed: boom".to_owned());
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let footer: String = (0..80)
            .map(|x| terminal.backend().buffer()[(x, 5)].symbol())
            .collect();
        assert!(footer.starts_with("Watch refresh failed: boom"), "{footer}");
    }
    #[test]
    fn stat_view_renders_compact_summaries_and_keeps_expanded_header() {
        let mut terminal = Terminal::new(TestBackend::new(60, 10)).unwrap();
        let mut app = App::new(Vec::new());
        app.mode = Mode::Show;
        app.show_stat = true;
        app.show_text = "message\ndiff --git a/one.rs b/one.rs\n--- a/one.rs\n+++ b/one.rs\n-old\n+new\ndiff --git a/image.bin b/image.bin\nBinary files a/image.bin and b/image.bin differ\n".to_owned();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let row_text = |terminal: &Terminal<TestBackend>, y| {
            (0..60)
                .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                .collect::<String>()
        };
        assert!(row_text(&terminal, 1).starts_with("message"));
        assert!(row_text(&terminal, 2).starts_with("▶ one.rs | +1 −1"));
        assert!(row_text(&terminal, 3).starts_with("▶ image.bin | binary"));
        for (x, color) in [
            (11, Color::Green),
            (12, Color::Green),
            (14, Color::Red),
            (15, Color::Red),
        ] {
            assert_eq!(terminal.backend().buffer()[(x, 2)].fg, color);
            assert_eq!(terminal.backend().buffer()[(x, 2)].bg, Color::Reset);
        }
        app.show_cursor = 1;
        handle(
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            &mut app,
        );
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(row_text(&terminal, 2).starts_with("▼ one.rs | +1 −1"));
        assert!(row_text(&terminal, 3).contains("diff --git a/one.rs"));
        assert_eq!(terminal.backend().buffer()[(11, 2)].fg, Color::Green);
        assert_eq!(terminal.backend().buffer()[(14, 2)].fg, Color::Red);
        assert_eq!(terminal.backend().buffer()[(11, 2)].bg, Color::DarkGray);
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
    fn comparisons_hide_log_and_ignore_history_navigation() {
        use crossterm::event::{
            Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
        };

        let mut comparison = commit("Diff A..B", 1);
        comparison.kind = crate::git::CommitKind::Comparison { worktree: false };
        let mut app = App::new(vec![comparison]);
        app.mode = Mode::Show;
        app.context = "diff A..B".to_owned();
        app.show_text = "comparison patch".to_owned();
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let row_text = |terminal: &Terminal<TestBackend>, y| {
            (0..100)
                .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                .collect::<String>()
        };
        assert!(!row_text(&terminal, 0).contains("Log"));
        assert!(!row_text(&terminal, 0).contains("Show"));
        assert_eq!(row_text(&terminal, 0).trim(), "glog diff A..B");
        assert_eq!(app.show_tab_start, app.show_tab_end);
        assert!(!row_text(&terminal, 29).contains("commit"));
        assert_eq!(app.log_tab_start, app.log_tab_end);
        for code in [KeyCode::Tab, KeyCode::Esc, KeyCode::Left, KeyCode::Right] {
            crate::input::handle(
                Event::Key(KeyEvent::new(code, KeyModifiers::NONE)),
                &mut app,
            );
            assert_eq!(app.mode, Mode::Show);
            assert_eq!(app.show_text, "comparison patch");
        }
        crate::input::handle(
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: app.show_tab_start,
                row: 0,
                modifiers: KeyModifiers::NONE,
            }),
            &mut app,
        );
        assert_eq!(app.mode, Mode::Show);
        app.show_help = true;
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let screen = (0..30).map(|y| row_text(&terminal, y)).collect::<String>();
        assert!(!screen.contains("Log"));
        assert!(!screen.contains("previous / next commit"));
        assert!(screen.contains("toggle section or file"));
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
mod show_wrapping_tests {
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
    #[test]
    fn wrapped_cursor_continuations_and_blank_rows_have_full_background() {
        for input in ["abcdefghijklmno", "界界界界界界", "one two three four", ""] {
            let mut terminal = Terminal::new(TestBackend::new(10, 8)).unwrap();
            let mut app = App::new(Vec::new());
            app.mode = Mode::Show;
            app.show_text = format!("before\n{input}\nafter");
            app.ensure_show_rows();
            app.show_cursor = 1;
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            let end = app.show_row_starts[2];
            for y in 2..=end as u16 {
                let mut x = 0;
                while x < 10 {
                    let cell = &terminal.backend().buffer()[(x, y)];
                    assert_eq!(cell.bg, Color::DarkGray, "input={input:?}, x={x}, y={y}");
                    // A wide glyph's trailing cell is not drawn by the backend.
                    x += Span::raw(cell.symbol()).width().max(1) as u16;
                }
            }
            assert_eq!(
                terminal.backend().buffer()[(0, end as u16 + 1)].bg,
                Color::Reset
            );
        }
    }

    #[test]
    fn scrolling_and_clicking_use_wrapped_screen_rows() {
        let mut terminal = Terminal::new(TestBackend::new(10, 6)).unwrap();
        let mut app = App::new(Vec::new());
        app.mode = Mode::Show;
        app.show_text = format!("{}\nsecond\nthird\nfourth", "a".repeat(70));
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        app.scroll_show(4);
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert_eq!(app.show_offset, 4);
        assert_eq!(app.show_cursor, 0);
        app.click_show_row(2); // Still the first logical line.
        assert_eq!(app.show_cursor, 0);
        app.click_show_row(3);
        assert_eq!(app.show_cursor, 1);
        app.scroll_show(4);
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert_eq!(app.show_offset, 6);
        assert_eq!(app.show_cursor, 1);
        app.scroll_show(-4);
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert_eq!(app.show_offset, 2);
        assert_eq!(app.show_cursor, 0);
        app.top();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert_eq!(app.show_offset, 0);
    }

    #[test]
    fn resizing_recomputes_wrapping_and_keeps_cursor_visible() {
        let mut terminal = Terminal::new(TestBackend::new(20, 6)).unwrap();
        let mut app = App::new(Vec::new());
        app.mode = Mode::Show;
        app.show_text = "abcdefghijklmnopqrst\nabcdefghijklmnopqrst\nselected\nafter".to_owned();
        app.ensure_show_rows();
        app.show_cursor = 2;
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert_eq!(app.show_offset, 0);
        terminal.backend_mut().resize(10, 6);
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert_eq!(app.show_offset, 1);
        assert_eq!(terminal.backend().buffer()[(0, 4)].symbol(), "s");
    }
}

#[cfg(test)]
mod release_review_tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn visible_text(terminal: &Terminal<TestBackend>) -> String {
        let buffer = terminal.backend().buffer();
        (1..5)
            .flat_map(|y| (0..10).map(move |x| buffer[(x, y)].symbol()))
            .collect()
    }

    #[test]
    fn search_reveals_match_in_wrapped_continuation() {
        for prefix in [
            "a".repeat(60),
            "界".repeat(30),
            "one two ".repeat(8),
            "a".repeat(635), // Match crosses a probe chunk boundary.
            format!("\x1b[32m{}\x1b[0m", "a".repeat(60)),
        ] {
            let mut terminal = Terminal::new(TestBackend::new(10, 6)).unwrap();
            let mut app = App::new(Vec::new());
            app.mode = Mode::Show;
            app.show_text = format!("start\n{prefix}needle\nafter");
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            app.search = Some("needle".to_owned());
            app.repeat_search(false);
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            assert!(
                visible_text(&terminal).contains("needle"),
                "match hidden for {prefix:?}"
            );
            let offset = app.show_offset;
            app.repeat_search(true);
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            assert_eq!(app.show_offset, offset);
            app.scroll_show(-100);
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            assert_eq!(
                app.show_offset, 0,
                "manual scrolling must remain possible after searching"
            );
        }
    }

    #[test]
    fn bottom_reveals_end_of_wrapped_last_line() {
        let mut terminal = Terminal::new(TestBackend::new(10, 6)).unwrap();
        let mut app = App::new(Vec::new());
        app.mode = Mode::Show;
        app.show_text = format!("start\n{}END", "a".repeat(60));
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        app.bottom();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(visible_text(&terminal).contains("END"));
        app.scroll_show(-1);
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert_eq!(app.show_offset, 3);
    }
}
