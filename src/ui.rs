use crate::{
    ansi,
    app::{App, Mode},
};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Span, Text},
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
    let header = Layout::horizontal([
        Constraint::Length(7),
        Constraint::Length(13),
        Constraint::Min(0),
        Constraint::Length(8),
    ])
    .split(chunks[0]);
    frame.render_widget(
        Paragraph::new(" glog ").style(header_style.add_modifier(Modifier::BOLD)),
        header[0],
    );
    frame.render_widget(tabs, header[1]);
    frame.render_widget(
        Paragraph::new("h help  ")
            .alignment(Alignment::Right)
            .style(header_style),
        header[3],
    );
    match app.mode {
        Mode::Log => draw_log(frame, app, chunks[1]),
        Mode::Show => draw_show(frame, app, chunks[1]),
    }
    let help = if let Some(input) = &app.search_input {
        let prefix = if app.search_reverse { '?' } else { '/' };
        format!("{prefix}{input}█")
    } else if let Some(status) = &app.status {
        status.clone()
    } else {
        "↑/k ↓/j  PgUp/b PgDn/Space  / ? search  Enter/Tab view  h help  q quit".to_owned()
    };
    frame.render_widget(
        Paragraph::new(help).style(Style::default().fg(Color::DarkGray)),
        chunks[2],
    );
    if app.show_help {
        draw_help(frame);
    }
}

fn draw_help(frame: &mut Frame) {
    let screen = frame.area();
    let width = screen.width.saturating_sub(4).min(68);
    let height = screen.height.saturating_sub(2).min(17);
    let area = Rect::new(
        screen.x + screen.width.saturating_sub(width) / 2,
        screen.y + screen.height.saturating_sub(height) / 2,
        width,
        height,
    );
    let text = [
        "Navigation",
        "  ↑/k, ↓/j          previous / next; scroll Show",
        "  Page Up/b         page up",
        "  Page Down/Space   page down",
        "  g, G              top / bottom",
        "",
        "Views and search",
        "  Enter             open selected commit",
        "  Escape            return to Log / cancel",
        "  Tab               switch Log / Show",
        "  /, ?              search forward / backward",
        "  n, N              repeat / reverse search",
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
