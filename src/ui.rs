use crate::{
    ansi,
    app::{App, Mode},
};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Span, Text},
    widgets::{Block, Borders, Paragraph, Tabs, Wrap},
    Frame,
};

pub fn draw(frame: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(frame.area());
    let selected = if app.mode == Mode::Log { 0 } else { 1 };
    let tabs = Tabs::new([" Log ", " Show "])
        .select(selected)
        .block(Block::default().title(" glog ").borders(Borders::ALL))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_widget(tabs, chunks[0]);
    match app.mode {
        Mode::Log => draw_log(frame, app, chunks[1]),
        Mode::Show => draw_show(frame, app, chunks[1]),
    }
    let help = if let Some(input) = &app.search_input {
        format!("/{input}█")
    } else if let Some(status) = &app.status {
        status.clone()
    } else {
        "↑/k ↓/j  PgUp/b PgDn/Space  / search  Tab view  q quit".to_owned()
    };
    frame.render_widget(
        Paragraph::new(help).style(Style::default().fg(Color::DarkGray)),
        chunks[2],
    );
}

fn draw_log(frame: &mut Frame, app: &mut App, area: Rect) {
    let height = area.height as usize;
    if app.commits.is_empty() {
        frame.render_widget(
            Paragraph::new("No commits matched the supplied arguments."),
            area,
        );
        return;
    }
    if app.selected < app.log_offset {
        app.log_offset = app.selected;
    }
    if app.selected >= app.log_offset + height.max(1) {
        app.log_offset = app.selected + 1 - height.max(1);
    }
    let mut lines = Vec::new();
    for (index, commit) in app.commits.iter().enumerate().skip(app.log_offset) {
        if lines.len() >= height {
            break;
        }
        for (part, graph) in commit.graph.iter().enumerate() {
            if lines.len() >= height {
                break;
            }
            let mut line = ansi::parse_line(graph);
            if part + 1 == commit.graph.len() {
                line.spans.push(Span::styled(
                    format!("{} ", commit.short_hash),
                    Style::default().fg(Color::Yellow),
                ));
                if !commit.decorations.is_empty() {
                    line.spans.push(Span::styled(
                        format!("({}) ", commit.decorations),
                        Style::default().fg(Color::Green),
                    ));
                }
                line.spans.push(Span::raw(commit.subject.clone()));
            }
            if index == app.selected {
                line.style = Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD);
            }
            lines.push(line);
        }
    }
    frame.render_widget(Paragraph::new(Text::from(lines)), area);
}

fn draw_show(frame: &mut Frame, app: &mut App, area: Rect) {
    let lines = ansi::lines(&app.show_text);
    let max = lines.len().saturating_sub(area.height as usize);
    app.show_offset = app.show_offset.min(max);
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .scroll((app.show_offset.min(u16::MAX as usize) as u16, 0))
            .wrap(Wrap { trim: false }),
        area,
    );
}
