use crate::{
    ansi,
    app::{App, History, LogAction, Mode, ShowScroll},
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
    app.visible_show_links.clear();
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
        Mode::Log | Mode::Grep | Mode::RangeDiff => 0,
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
    let file_view = app.file_view();
    let log_label = match app.history {
        History::Log => "Log",
        History::Reflog => "Reflog",
        History::Blame if file_view => "File",
        History::Blame => "Blame",
        History::Stash => "Stash",
        History::Branches => "Branches",
        // These views have no tabs.
        History::RangeDiff | History::Grep => "",
    };
    // File and Branches have no detail tab; branches open a separate Log.
    let labels: &[&str] = match (has_log, file_view || app.history == History::Branches) {
        (false, _) => &[],
        (true, true) => &[log_label],
        (true, false) => &[log_label, detail],
    };
    // Tabs pads each label with a space on either side and separates them
    // with a one-cell divider.
    let width = |label: &&str| label.len() as u16 + 2;
    let log_width = labels.first().map_or(0, width);
    let detail_width = labels.get(1).map_or(0, width);
    let tab_width = labels.iter().map(|label| width(label) + 1).sum();
    let tabs = Tabs::new(labels.to_vec())
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
    app.log_tab_end = header[1].x.saturating_add(log_width);
    app.show_tab_start = header[1]
        .x
        .saturating_add(if has_log { log_width + 1 } else { 0 });
    app.show_tab_end = app.show_tab_start.saturating_add(detail_width);
    frame.render_widget(
        Paragraph::new(command).style(header_style.add_modifier(Modifier::BOLD)),
        header[0],
    );
    if has_log {
        frame.render_widget(tabs, header[1]);
    }
    if app.mode == Mode::Log && app.history == History::Log {
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
        Mode::Grep | Mode::RangeDiff => {
            let current = app
                .search_match
                .filter(|(mode, _)| *mode == app.mode)
                .map(|(_, index)| index);
            let search = app.search.clone();
            if let Some(view) = app.pager_view() {
                view.draw(frame, chunks[1], search.as_deref(), current);
            }
        }
        Mode::Log => draw_log(frame, app, chunks[1]),
        Mode::Show => draw_show(frame, app, chunks[1]),
    }
    let mut help = if app.mode == Mode::Status {
        app.status_view.as_ref().and_then(|view| view.error.clone()).or_else(|| app.status.clone()).unwrap_or_else(|| "h help  q quit  ↑/k ↓/j  e edit  Enter show/fold  s summary  ←/→ commit  Shift-←/→ pan  Tab Log  Esc Log  Ctrl-L redraw · WATCH".to_owned())
    } else if let Some(input) = &app.search_input {
        let prefix = if app.search_reverse { '?' } else { '/' };
        format!("{prefix}{input}█")
    } else if let Some(message) = app.message() {
        message.to_owned()
    } else if app.mode == Mode::Grep {
        "h help  q quit  ←/→ path/pan  Enter open/fold  z fold  m fold all  t hide extension  T reset  / ? search".to_owned()
    } else if app.mode == Mode::RangeDiff {
        "h help  q quit  ↑/k ↓/j  wheel scroll  Space/b page  / ? search".to_owned()
    } else if app.history == History::Branches {
        "h help  q quit  ↑/k ↓/j  Enter/click branch log  d date  / ? search".to_owned()
    } else if app.mode == Mode::Log && app.history == History::Reflog {
        "h help  q quit  ↑/k ↓/j  ←/→ entry  Enter show  z fold  / ? search".to_owned()
    } else if app.mode == Mode::Log && app.history == History::Stash {
        "h help  q quit  ↑/k ↓/j  ←/→ stash  Enter show  a author  d date  / ? search".to_owned()
    } else if app.mode == Mode::Log && file_view {
        "h help  q quit  ↑/k ↓/j  a author  d date  x hash  / ? search".to_owned()
    } else if app.mode == Mode::Log && app.history == History::Blame {
        "h help  q quit  ↑/k ↓/j  ←/→ chunk  Enter show  p blame parent  Esc/Backspace back  a author  d date  x hash  / ? search"
            .to_owned()
    } else if app.mode == Mode::Log {
        // Keep the hint short by omitting the row format toggles (x hash,
        // s subject); the help screen lists every key.
        "h help  q quit  ↑/k ↓/j  ←/→ commit  Enter show  t hide type  z fold merge  m fold all  a author  d date  r refs  / ? search".to_owned()
    } else if !has_log {
        "h help  q quit  ↑/k ↓/j  [/ ] file  B blame  e edit  Enter/z fold  s summary  L lockfiles  / ? search"
            .to_owned()
    } else if app.history == History::Stash {
        "h help  q quit  ↑/k ↓/j  ←/→ stash  [/ ] file  B blame  e edit  Enter/z fold  s summary  Tab Stash  / ? search"
            .to_owned()
    } else {
        "h help  q quit  ↑/k ↓/j  ←/→ commit  [/ ] file  B blame  e edit  Enter/z fold  s summary  L lockfiles  / ? search"
            .to_owned()
    };
    if app.has_reference_back() && app.search_input.is_none() && app.message().is_none() {
        help = format!("Backspace back  {help}");
    }
    frame.render_widget(
        Paragraph::new(help).style(Style::default().fg(Color::Gray)),
        chunks[2],
    );
    if app.show_help {
        draw_help(
            frame,
            match app.history {
                History::RangeDiff => HelpView::RangeDiff,
                History::Grep => HelpView::Grep,
                _ if !has_log => HelpView::Detail,
                History::Log => HelpView::Log,
                History::Reflog => HelpView::Reflog,
                History::Blame if file_view => HelpView::File,
                History::Blame => HelpView::Blame,
                History::Stash => HelpView::Stash,
                History::Branches => HelpView::Branches,
            },
        );
    }
}

/// The session a help screen describes.
#[derive(Clone, Copy)]
enum HelpView {
    Log,
    Reflog,
    Blame,
    Stash,
    /// Show, Diff, or Status without a Log to return to.
    Detail,
    File,
    Branches,
    RangeDiff,
    Grep,
}

/// A key's description in each help screen; `None` omits its row there.
#[derive(Clone, Copy)]
struct HelpTexts {
    log: Option<&'static str>,
    reflog: Option<&'static str>,
    blame: Option<&'static str>,
    stash: Option<&'static str>,
    /// Show, Diff, or Status without a Log to return to.
    detail: Option<&'static str>,
    file: Option<&'static str>,
    branches: Option<&'static str>,
    range_diff: Option<&'static str>,
    grep: Option<&'static str>,
}

impl HelpTexts {
    fn get(self, view: HelpView) -> Option<&'static str> {
        match view {
            HelpView::Log => self.log,
            HelpView::Reflog => self.reflog,
            HelpView::Blame => self.blame,
            HelpView::Stash => self.stash,
            HelpView::Detail => self.detail,
            HelpView::File => self.file,
            HelpView::Branches => self.branches,
            HelpView::RangeDiff => self.range_diff,
            HelpView::Grep => self.grep,
        }
    }
}

const NO_HELP: HelpTexts = HelpTexts {
    log: None,
    reflog: None,
    blame: None,
    stash: None,
    detail: None,
    file: None,
    branches: None,
    range_diff: None,
    grep: None,
};

/// A key and its descriptions. Rows without a key are shown verbatim.
type HelpRow = (&'static str, HelpTexts);

const fn every(key: &'static str, text: &'static str) -> HelpRow {
    let text = Some(text);
    (
        key,
        HelpTexts {
            log: text,
            reflog: text,
            blame: text,
            stash: text,
            detail: text,
            file: text,
            branches: text,
            range_diff: None,
            grep: None,
        },
    )
}

const fn pager(key: &'static str, text: &'static str) -> HelpRow {
    let mut row = every(key, text);
    row.1.range_diff = Some(text);
    row.1.grep = Some(text);
    row
}

const fn branches(key: &'static str, text: &'static str) -> HelpRow {
    (
        key,
        HelpTexts {
            branches: Some(text),
            ..NO_HELP
        },
    )
}

/// Detail actions are available in every session except the plain file view
/// and Branches.
const fn detail(key: &'static str, text: &'static str) -> HelpRow {
    let text = Some(text);
    (
        key,
        HelpTexts {
            log: text,
            reflog: text,
            blame: text,
            stash: text,
            detail: text,
            ..NO_HELP
        },
    )
}

const AUTHOR_BADGES: &str =
    "  Author badges: +꩜ Codex  +❋ Claude Code  +✦ Gemini  +N other coauthors";
const FILE_FOLD: Option<&str> = Some("toggle current file fold (Show)");
const FILE_SUMMARY: Option<&str> = Some("toggle file summary / patch (Show/Status)");

const HELP_ROWS: &[HelpRow] = &[
    pager("", "Navigation"),
    (
        "↑/k, ↓/j",
        HelpTexts {
            log: Some("previous / next; move Show cursor"),
            reflog: Some("previous / next; move Show cursor"),
            blame: Some("previous / next; move Show cursor"),
            stash: Some("previous / next; move Show cursor"),
            detail: Some("previous / next; move Show cursor"),
            file: Some("previous / next line"),
            branches: Some("select a branch or shared ancestor"),
            range_diff: Some("move cursor"),
            grep: Some("previous / next result or group"),
        },
    ),
    (
        "Mouse wheel",
        HelpTexts {
            range_diff: Some("scroll up / down"),
            grep: Some("scroll; click groups to fold, matches to select/open"),
            ..NO_HELP
        },
    ),
    pager("Page Up/b", "page up"),
    pager("Page Down/Space/f", "page down"),
    (
        "←/→",
        HelpTexts {
            log: Some("previous / next commit (Log/Show/Status)"),
            reflog: Some("previous / next reflog entry"),
            blame: Some("previous / next commit chunk"),
            grep: Some("select fold target; at last target, → pans and ← pans back"),
            stash: Some("previous / next stash"),
            ..NO_HELP
        },
    ),
    detail("[, ]", "previous / next changed file (Show)"),
    (
        "Click link",
        HelpTexts {
            log: Some("follow a commit or open a link (Show)"),
            reflog: Some("follow a commit or open a link (Show)"),
            blame: Some("follow a commit or open a link (Show)"),
            // Show links nothing in stash messages.
            stash: None,
            detail: Some("follow a commit or open a link (Show)"),
            ..NO_HELP
        },
    ),
    pager("g/<, G/>", "top / bottom (also Home/End)"),
    pager("", ""),
    (
        "a/d/r/x/s",
        HelpTexts {
            log: Some("toggle author/date/refs/hash/subject (Log)"),
            ..NO_HELP
        },
    ),
    (
        "a/d/x",
        HelpTexts {
            blame: Some("toggle author/date/hash"),
            file: Some("show author/date/hash; enable line history"),
            ..NO_HELP
        },
    ),
    branches("d", "show / hide dates"),
    (
        "a/d",
        HelpTexts {
            stash: Some("toggle author/date (Stash)"),
            ..NO_HELP
        },
    ),
    (
        "",
        HelpTexts {
            log: Some(AUTHOR_BADGES),
            blame: Some(AUTHOR_BADGES),
            ..NO_HELP
        },
    ),
    pager("", "Views and search"),
    (
        "W",
        HelpTexts {
            log: Some("toggle watch (Log/Show)"),
            branches: Some("toggle watch"),
            ..NO_HELP
        },
    ),
    every("O", "open commit on GitHub (selected file/line in Show)"),
    detail("B", "blame selected line (Show)"),
    (
        "Enter",
        HelpTexts {
            log: Some("open commit / toggle section or file"),
            reflog: Some("open commit / toggle section or file"),
            blame: Some("open commit / toggle section or file"),
            stash: Some("open stash / toggle section or file"),
            detail: Some("open commit (Status) / toggle section or file"),
            branches: Some("open its Log up to the next shared node"),
            grep: Some("open match in Blame / fold group"),
            ..NO_HELP
        },
    ),
    (
        "z",
        HelpTexts {
            log: Some("fold merge (Log) / file (Show)"),
            reflog: Some("fold rebase, rebase chain, amend chain, or visits / file (Show)"),
            grep: Some("fold file or folder highlighted by the path cursor"),
            blame: FILE_FOLD,
            stash: FILE_FOLD,
            detail: FILE_FOLD,
            ..NO_HELP
        },
    ),
    (
        "",
        HelpTexts {
            reflog: Some("At a chain head: rebases / latest steps / folded chain"),
            ..NO_HELP
        },
    ),
    (
        "m",
        HelpTexts {
            log: Some("expand / fold all merges (Log)"),
            grep: Some("expand / fold all files and folders"),
            ..NO_HELP
        },
    ),
    (
        "t / T",
        HelpTexts {
            log: Some("hide selected type / clear filters (Log)"),
            grep: Some("hide selected extension / clear filters"),
            ..NO_HELP
        },
    ),
    (
        "M",
        HelpTexts {
            log: Some("hide / show merge commits (Log)"),
            ..NO_HELP
        },
    ),
    (
        "p",
        HelpTexts {
            blame: Some("blame the file before this commit"),
            ..NO_HELP
        },
    ),
    (
        "Backspace",
        HelpTexts {
            // A followed reference opens as a Log, whatever it was followed from.
            log: Some("return after following a link or opening blame"),
            reflog: Some("return after opening blame"),
            blame: Some("return to the previous blame or review"),
            detail: Some("return after opening blame"),
            file: Some("return to the previous blame, if any"),
            branches: Some("return here from a branch Log"),
            ..NO_HELP
        },
    ),
    detail("L", "expand / fold all lockfiles (Show)"),
    detail("e", "open working file in editor (Show/Status)"),
    (
        "s",
        HelpTexts {
            log: FILE_SUMMARY,
            reflog: Some("toggle file summary / patch (Show)"),
            blame: FILE_SUMMARY,
            stash: Some("toggle file summary / patch (Show)"),
            detail: FILE_SUMMARY,
            ..NO_HELP
        },
    ),
    (
        "Escape",
        HelpTexts {
            log: Some("return to Log / cancel"),
            reflog: Some("return to Reflog / cancel"),
            blame: Some("return to Blame (Show) / previous blame or review / cancel"),
            stash: Some("return to Stash / cancel"),
            file: Some("close help / cancel search"),
            range_diff: Some("close help / cancel search"),
            grep: Some("close help / cancel search"),
            branches: Some("return here from a branch Log / cancel"),
            ..NO_HELP
        },
    ),
    (
        "Tab",
        HelpTexts {
            log: Some("switch Log / detail"),
            reflog: Some("switch Reflog / Show"),
            blame: Some("switch Blame / detail"),
            stash: Some("switch Stash / Show"),
            grep: Some("open match in Blame / fold group"),
            ..NO_HELP
        },
    ),
    pager("/, ?", "search forward / backward"),
    pager("n, N", "repeat / reverse search"),
    pager("Ctrl-L", "redraw the screen"),
    pager("", ""),
    pager("h", "close help"),
    pager("q", "quit (or close help)"),
    branches("", ""),
    branches("", "  Refs use the same decorations as Log."),
    branches("", "  Branches at the same commit share a node."),
    branches(
        "",
        "  Dates show the latest ref update, or the commit date.",
    ),
    branches(
        "",
        "  The remote default tip is labelled, e.g. origin/main.",
    ),
    branches("", "  ◆ current branch   ● local branch   ○ remote tip"),
    branches("", "  ● shared ancestor (where histories meet)"),
    branches("", "  Merged branches open their full history."),
    branches("", "  Merged tips alone do not cut off other Logs."),
];

fn help_text(view: HelpView) -> String {
    HELP_ROWS
        .iter()
        .filter_map(|(key, texts)| {
            let text = texts.get(view)?;
            Some(if key.is_empty() {
                text.to_owned()
            } else {
                format!("  {key:<18}{text}")
            })
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn draw_help(frame: &mut Frame, view: HelpView) {
    let screen = frame.area();
    let width = screen.width.saturating_sub(4).min(68);
    let height = screen.height.saturating_sub(2).min(28);
    let area = Rect::new(
        screen.x + screen.width.saturating_sub(width) / 2,
        screen.y + screen.height.saturating_sub(height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(help_text(view)).block(Block::bordered().title(" Help ")),
        area,
    );
}

fn draw_type_controls(frame: &mut Frame, app: &mut App, area: Rect) {
    let mut items = Vec::new();
    if !app.log_folds.hidden_types.is_empty() || app.log_folds.hide_merges {
        let count = app.log_folds.hidden_count();
        items.push((
            format!(
                "Hiding {count} commit{} · ",
                if count == 1 { "" } else { "s" }
            ),
            LogAction::Reset,
            false,
        ));
        items.push(("[T show all] ".to_owned(), LogAction::Reset, true));
        for kind in &app.log_folds.hidden_types {
            items.push((format!("[{kind} ×] "), LogAction::Type(kind.clone()), true));
        }
    }
    if let Some(kind) = app.selected_hideable_type() {
        items.push((
            format!("[t hide {kind}] "),
            LogAction::Type(kind.into_owned()),
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
            LogAction::Merges,
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
                Color::LightBlue
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
    app.visible_log_links.clear();
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
    // Blame rows of uncommitted lines are interleaved with committed ones.
    let separator_before = app
        .commits
        .iter()
        .enumerate()
        .filter(|_| app.history == History::Log)
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
        .map(|(i, commit)| app.log_folds.display_height(i, commit))
        .sum::<usize>()
        + usize::from(separator_before.is_some_and(|index| app.selected >= index));
    let selected = &app.commits[app.selected];
    let selected_subject_row = selected_start_row
        + app
            .log_folds
            .display_height(app.selected, selected)
            .saturating_sub(1);
    if !app.log_folds.visible(app.selected) {
        app.log_offset = 0;
    } else if std::mem::take(&mut app.center_selection) {
        app.log_offset = selected_subject_row.saturating_sub(height / 2);
    } else if selected_subject_row < app.log_offset {
        app.log_offset = selected_start_row;
    } else if selected_subject_row >= app.log_offset + height.max(1) {
        app.log_offset = selected_subject_row + 1 - height.max(1);
    }
    if app.history == History::Blame {
        // Blame rows are one line each; keep the last line at the bottom.
        app.log_offset = app
            .log_offset
            .min(app.commits.len().saturating_sub(height.max(1)));
    }
    let date_column = app.log_date_column();
    let selection = app.selected;
    let fixups = &app.fixups;
    let fixup = |index: usize| fixups.get(index).copied().flatten();
    let label =
        |index: usize| fixup(index).map(|target| target.label(&app.commits[index], &app.commits));
    let related = |index: usize| {
        fixup(selection) == Some(crate::fixups::Target::Commit(index))
            || fixup(index) == Some(crate::fixups::Target::Commit(selection))
    };
    let mut log_links = Vec::new();
    let mut lines = Vec::new();
    let show_hash = app.log_format.shows(crate::log_format::Field::Hash);
    let hash_style = |index: usize| {
        if app.history == History::Reflog
            && app.commits[index].hash == app.commits[app.selected].hash
        {
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD)
        } else if related(index) {
            Style::default()
                .fg(Color::LightCyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        }
    };
    // Collect actual on-screen commit rows before measuring their combined
    // graph/hash column. Routing lines and off-screen history reserve no space.
    let mut metadata_rows = Vec::new();
    let mut graph_row = 0;
    // Routing rows below a filtered-out commit carry its edges, so clicking
    // them selects nothing rather than the commit they lead to.
    let mut after_filtered = false;
    'commits: for (index, commit) in app.commits.iter().enumerate() {
        let graph_only = !app.log_folds.visible(index);
        let routing_selectable = !graph_only && !after_filtered;
        if app.log_folds.filtered_out(index) {
            after_filtered = true;
        } else if !graph_only {
            after_filtered = false;
        }
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
        // Rows above the screen are counted without drawing them.
        let rows = app.log_folds.display_height(index, commit);
        if graph_row + rows <= app.log_offset {
            graph_row += rows;
            continue;
        }
        let graph_rows = app.log_folds.display_graph(index, commit);
        for (part, graph) in graph_rows.iter().enumerate() {
            if graph_row < app.log_offset {
                graph_row += 1;
                continue;
            }
            if lines.len() >= height {
                break 'commits;
            }
            let mut line = ansi::parse_line(graph);
            let mut deferred_metadata = false;
            let mut stand_ins = StandIns::default();
            if !graph_only && part + 1 == graph_rows.len() {
                let deferred =
                    date_column.is_some() && commit.kind == crate::git::CommitKind::Revision;
                let marker = app.log_folds.marker(index);
                // Reflog keeps its own nodes. Its fold control follows the
                // hash in aligned columns, otherwise it leads the row.
                let reflog = commit.reflog().is_some();
                let reflog_marker = marker.filter(|_| reflog);
                let hash_marker = reflog_marker.filter(|_| deferred && show_hash);
                if let Some(node) = commit.branch() {
                    line = node.style_graph(line, commit.graph_node, index == app.selected);
                } else if reflog && !(deferred && show_hash) {
                    // Every row reserves the control's slot to keep text aligned.
                    line.spans.push(Span::styled(
                        format!("{} ", reflog_marker.unwrap_or(" ")),
                        Style::default().fg(Color::Yellow),
                    ));
                } else if let Some(marker) = marker.filter(|_| !reflog) {
                    // Git's graph and the ASCII fallback keep Git's node marker.
                    let nodes = [
                        commit.graph_node,
                        crate::log_folds::unicode_node(commit.graph_node),
                    ];
                    if let Some(span) = line
                        .spans
                        .iter_mut()
                        .find(|span| span.content.contains(nodes))
                    {
                        span.content = span.content.replacen(nodes, marker, 1).into();
                    }
                }
                if deferred {
                    metadata_rows.push((lines.len(), index, hash_marker));
                    deferred_metadata = true;
                } else {
                    let start: usize = line.spans.iter().map(|span| span.content.len()).sum();
                    let row = match commit.blame() {
                        // Name the commit of a chunk that starts above the screen.
                        Some(blame) => crate::blame::row(
                            commit,
                            blame,
                            blame.starts_chunk || lines.is_empty(),
                            &app.log_format,
                        ),
                        None => app.log_format.row_with_fixup(
                            commit,
                            None,
                            hash_style(index),
                            label(index).as_ref(),
                        ),
                    };
                    log_links.extend(placed_links(row.links, index, lines.len(), start));
                    line.spans.extend(row.spans);
                    stand_ins = row.stand_ins.shifted(start);
                    line.spans.push(Span::styled(
                        app.log_folds.label(index),
                        Style::default().fg(Color::Yellow),
                    ));
                }
            }
            if let Some(query) = app.search.as_ref().filter(|_| !deferred_metadata) {
                let current = app.search_match == Some((Mode::Log, index));
                line = stand_ins.highlight(line, query, current);
            }
            if !graph_only && index == app.selected {
                line.style = Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD);
            }
            lines.push(line);
            app.visible_log_rows.push(
                (routing_selectable || !graph_only && part + 1 == graph_rows.len())
                    .then_some(index),
            );
            graph_row += 1;
        }
    }
    if let Some(date_column) = date_column {
        let author = crate::log_format::author_column(
            metadata_rows
                .iter()
                .map(|&(_, index, _)| &app.commits[index]),
        );
        let width = metadata_rows
            .iter()
            .map(|&(row, index, marker)| {
                lines[row].width()
                    + if show_hash {
                        Span::raw(&app.commits[index].short_hash).width()
                            + marker
                                .map_or(0, |marker| crate::log_format::hash_marker(marker).width())
                    } else {
                        0
                    }
            })
            .max()
            .unwrap_or(0);
        for (row, index, marker) in metadata_rows {
            let line = &mut lines[row];
            let hash_width = width.saturating_sub(line.width());
            if !show_hash && hash_width > 0 {
                line.spans.push(Span::raw(" ".repeat(hash_width)));
            }
            // The hash touches its graph; any fold control precedes the padding.
            let columns = date_column
                .columns(hash_width, author)
                .with_hash_marker(marker);
            let start: usize = line.spans.iter().map(|span| span.content.len()).sum();
            let metadata = app.log_format.row_with_fixup(
                &app.commits[index],
                Some(&columns),
                hash_style(index),
                label(index).as_ref(),
            );
            log_links.extend(placed_links(metadata.links, index, row, start));
            line.spans.extend(metadata.spans);
            line.spans.push(Span::styled(
                app.log_folds.label(index),
                Style::default().fg(Color::Yellow),
            ));
            if let Some(query) = &app.search {
                let current = app.search_match == Some((Mode::Log, index));
                let stand_ins = metadata.stand_ins.shifted(start);
                *line = stand_ins.highlight(std::mem::take(line), query, current);
            }
        }
    }
    for (row, range, link) in log_links {
        let text: String = lines[row]
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        let x = Span::raw(&text[..range.start]).width();
        let width = Span::raw(&text[range]).width();
        let visible_width = width.min((area.width as usize).saturating_sub(x));
        if visible_width > 0 {
            app.visible_log_links.push((
                Rect::new(
                    area.x + x as u16,
                    area.y + row as u16,
                    visible_width as u16,
                    1,
                ),
                link,
            ));
        }
    }
    frame.render_widget(Paragraph::new(Text::from(lines)), area);
}

fn draw_show(frame: &mut Frame, app: &mut App, area: Rect) {
    app.ensure_show_rows();
    app.show_row_origin = area.y;
    app.visible_show_rows = area.height as usize;
    // Rows only change when rebuilt, which clears their starts, so wrap
    // them once per width rather than on every frame.
    if app.show_row_starts.len() != app.show_rows.len() + 1 || app.show_rows_width != area.width {
        // Use Ratatui's own wrapping rules, including word boundaries and wide glyphs.
        let mut starts = Vec::with_capacity(app.show_rows.len() + 1);
        starts.push(0);
        for row in &app.show_rows {
            let mut line = show_row_line(row, area.width);
            line.spans = ansi::display_spans(line.spans);
            let height = Paragraph::new(line)
                .wrap(Wrap { trim: false })
                .line_count(area.width)
                .max(1);
            starts.push(starts.last().unwrap() + height);
        }
        app.show_row_starts = starts;
        app.show_rows_width = area.width;
    }
    let rows = app.show_rows.len();
    let cursor_line =
        (app.show_cursor < rows).then(|| styled_show_line(app, app.show_cursor, area.width));
    let cursor_background = cursor_line
        .as_ref()
        .and_then(|line| line.spans.last())
        .and_then(|span| span.style.bg)
        .unwrap_or(Color::DarkGray);
    let cursor_line = cursor_line.map(|line| drawn_show_line(app, app.show_cursor, line));
    let cursor_start = app.show_row_starts[app.show_cursor.min(rows)];
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
        cursor_line.as_ref().and_then(|line| {
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
        .min(rows);
    let within = app.show_offset - app.show_row_starts[first];
    // Each row takes at least one screen line, so no later row can be visible.
    let visible = first..rows.min(first + height);
    if app.show_rows[visible.clone()]
        .iter()
        .any(|row| message_entry(&app.show_links, row).is_some())
    {
        // Render an ID mask with exactly the viewport's wrapping and scroll rules.
        // This keeps hit targets correct for wide characters and wrapped hashes.
        use ratatui::{buffer::Buffer, widgets::Widget};
        let mut targets = Vec::new();
        let marker_lines: Vec<_> = visible
            .clone()
            .map(|index| {
                let mut marked =
                    Line::raw(ansi::normalized_line(&app.show_rows[index].text).to_string());
                if let Some(links) = message_entry(&app.show_links, &app.show_rows[index]) {
                    marked = ansi::style_ranges(
                        marked,
                        links.iter().map(|link| {
                            targets.push(link.target.clone());
                            let id = targets.len() as u32;
                            let style = Style::default().fg(Color::Rgb(
                                (id >> 16) as u8,
                                (id >> 8) as u8,
                                id as u8,
                            ));
                            (link.range.clone(), style)
                        }),
                    );
                }
                marked.spans = ansi::display_spans(marked.spans);
                marked
            })
            .collect();
        let mut mask = Buffer::empty(area);
        Paragraph::new(Text::from(marker_lines))
            .scroll((within.min(u16::MAX as usize) as u16, 0))
            .wrap(Wrap { trim: false })
            .render(area, &mut mask);
        for y in area.y..area.bottom() {
            let mut previous = None;
            let mut x = area.x;
            while x < area.right() {
                let cell = &mask[(x, y)];
                // A wide glyph's trailing cells are reset, so they take the glyph's link.
                let width = (Span::raw(cell.symbol()).width().max(1) as u16).min(area.right() - x);
                let start = x;
                x += width;
                let id = match cell.fg {
                    Color::Rgb(r, g, b) => {
                        (usize::from(r) << 16) | (usize::from(g) << 8) | usize::from(b)
                    }
                    _ => 0,
                };
                let Some(target) = id.checked_sub(1).and_then(|index| targets.get(index)) else {
                    previous = None;
                    continue;
                };
                // Adjacent cells of one link form a single hit target.
                if previous == Some(id) {
                    app.visible_show_links.last_mut().unwrap().0.width += width;
                } else {
                    app.visible_show_links
                        .push((Rect::new(start, y, width, 1), target.clone()));
                }
                previous = Some(id);
            }
        }
    }
    let lines: Vec<_> = visible
        .clone()
        .map(|index| match &cursor_line {
            Some(line) if index == app.show_cursor => line.clone(),
            _ => drawn_show_line(app, index, styled_show_line(app, index, area.width)),
        })
        .collect();
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .scroll((within.min(u16::MAX as usize) as u16, 0))
            .wrap(Wrap { trim: false }),
        area,
    );
    for index in visible {
        if let Some(preview) = &app.show_rows[index].preview {
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

/// A Show row's text, before styles that depend on the view.
fn show_row_line(row: &crate::app::ShowRow, width: u16) -> Line<'static> {
    if row.fold_separator {
        Line::from("━".repeat(width as usize))
    } else if row.summary {
        summary_line(&row.text)
    } else {
        ansi::normalized_line(&row.text)
    }
}

/// A Show row with the styles of its content and the cursor.
fn styled_show_line(app: &App, index: usize, width: u16) -> Line<'static> {
    let row = &app.show_rows[index];
    let mut line = show_row_line(row, width);
    if let Some(ranges) = message_entry(&app.show_message_styles, row) {
        line = ansi::style_ranges(line, ranges.iter().cloned());
    }
    if let Some(links) = message_entry(&app.show_links, row) {
        let style = Style::default()
            .fg(Color::LightCyan)
            .add_modifier(Modifier::UNDERLINED);
        line = ansi::style_ranges(line, links.iter().map(|link| (link.range.clone(), style)));
    }
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
}

/// Highlight search matches in a styled Show row, then expand its tabs.
fn drawn_show_line(app: &App, index: usize, mut line: Line<'static>) -> Line<'static> {
    if let Some(query) = &app.search {
        let current = app.search_match == Some((Mode::Show, index));
        line = highlight_matches(line, query, current);
    }
    // Style and search ranges refer to the original bytes; expand only after styling.
    line.spans = ansi::display_spans(std::mem::take(&mut line.spans));
    line
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

/// The entry for a row of the commit message, from a map keyed by Show line.
/// Links and message styles apply only to the message, never to patch text.
fn message_entry<'a, T>(
    entries: &'a std::collections::HashMap<usize, T>,
    row: &crate::app::ShowRow,
) -> Option<&'a T> {
    entries.get(&row.source).filter(|_| row.file.is_none())
}

/// Texts that byte ranges of a drawn row stand for without showing them
/// whole, such as a full author name behind its initials. Search matches
/// them as well as the drawn text.
#[derive(Default)]
pub(crate) struct StandIns(Vec<(std::ops::Range<usize>, String)>);

impl StandIns {
    pub fn push(&mut self, range: std::ops::Range<usize>, text: impl Into<String>) {
        self.0.push((range, text.into()));
    }

    /// A text the row stands for without drawing any part of it, which search
    /// matches but cannot mark.
    pub fn push_undrawn(&mut self, text: impl Into<String>) {
        self.push(0..0, text);
    }

    pub fn append(&mut self, other: StandIns) {
        self.0.extend(other.0);
    }

    /// The same texts, for a row drawn after `start` bytes of other text.
    pub fn shifted(self, start: usize) -> Self {
        Self(
            self.0
                .into_iter()
                .map(|(range, text)| (start + range.start..start + range.end, text))
                .collect(),
        )
    }

    /// Whether a lowercase query matches the drawn text or a text it stands for.
    pub fn matches(&self, drawn: &str, query: &str) -> bool {
        drawn.to_lowercase().contains(query)
            || self
                .0
                .iter()
                .any(|(_, text)| text.to_lowercase().contains(query))
    }

    /// Mark a range whole when its text matches, as part may be hidden, then
    /// the visible matches.
    pub fn highlight(self, line: Line<'static>, query: &str, current: bool) -> Line<'static> {
        let folded = query.to_lowercase();
        let hidden_matches: Vec<_> = self
            .0
            .into_iter()
            .filter(|(_, text)| !query.is_empty() && text.to_lowercase().contains(&folded))
            .map(|(range, _)| (range, match_style(current)))
            .collect();
        highlight_matches(ansi::style_ranges(line, hidden_matches), query, current)
    }
}

/// The links of Log row `from`, drawn on screen line `line` after `start`
/// bytes of other text.
fn placed_links(
    links: Vec<(std::ops::Range<usize>, usize)>,
    from: usize,
    line: usize,
    start: usize,
) -> impl Iterator<Item = (usize, std::ops::Range<usize>, crate::app::LogLink)> {
    links.into_iter().map(move |(range, target)| {
        let link = crate::app::LogLink { from, target };
        (line, range.start + start..range.end + start, link)
    })
}

pub(crate) fn highlight_matches(line: Line<'static>, query: &str, current: bool) -> Line<'static> {
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
    let highlight = match_style(current);
    ansi::style_ranges(line, ranges.into_iter().map(|range| (range, highlight)))
}

fn match_style(current: bool) -> Style {
    if current {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(Color::LightYellow)
            .add_modifier(Modifier::UNDERLINED)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::LogLink;
    use crate::git::{Commit, CommitKind};
    use crate::input::handle;
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn file_help_explains_how_to_enable_line_history() {
        use crate::log_format::Field;

        let mut app = App::new(Vec::new());
        app.history = History::Blame;
        app.show_help = true;
        for field in Field::ATTRIBUTION {
            app.log_format.toggle(field);
        }
        let mut terminal = Terminal::new(TestBackend::new(100, 32)).unwrap();
        let render = |app: &mut App, terminal: &mut Terminal<TestBackend>| {
            terminal.draw(|frame| draw(frame, app)).unwrap();
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
        };
        let file_help = render(&mut app, &mut terminal);
        assert!(file_help.contains("show author/date/hash; enable line history"));
        assert!(file_help.contains("previous / next line"));
        for unavailable in [
            "Enter",
            "←/→",
            "Tab",
            "before this commit",
            "(Show)",
            "Show/Status",
        ] {
            assert!(!file_help.contains(unavailable), "{unavailable}");
        }
        for field in Field::ATTRIBUTION {
            app.log_format.toggle(field);
            let blame_help = render(&mut app, &mut terminal);
            assert!(blame_help.contains("previous / next commit chunk"));
            assert!(blame_help.contains("blame the file before this commit"));
            app.log_format.toggle(field);
            assert_eq!(render(&mut app, &mut terminal), file_help);
        }
    }

    #[test]
    fn all_hidden_message_ignores_graph_only_merge_rows() {
        // Nested merges hidden by the merge filter still draw junctions,
        // but nothing is selectable once types hide every other commit.
        let base = commit("fix: base", 1);
        let mut outer = commit("Merge outer", 1);
        let mut rows = Vec::new();
        for name in ["left", "right"] {
            let first = commit(&format!("fix: {name} first"), 1);
            let second = commit(&format!("fix: {name} second"), 1);
            let mut merge = commit(&format!("Merge {name}"), 1);
            // Separate roots leave no branch point exempt from filtering.
            merge.parents = vec![first.hash.clone(), second.hash.clone()];
            outer.parents.push(merge.hash.clone());
            rows.extend([merge, second, first]);
        }
        rows.insert(0, outer);
        rows.push(base);
        let mut app = App::new(rows);
        app.apply_log_action(LogAction::Merges);
        app.apply_log_action(LogAction::Type("fix".into()));
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
        for action in [LogAction::Merges, LogAction::Type("chore".into())] {
            let mut merge = commit("chore: folded merge", 1);
            let mut side = commit("fix: side", 1);
            let mut main = commit("feat: main", 1);
            let base = commit("base", 1);
            merge.parents = vec![main.hash.clone(), side.hash.clone()];
            main.parents = vec![base.hash.clone()];
            side.parents = vec![base.hash.clone()];
            let mut app = App::new(vec![merge, side, main, base]);
            app.apply_log_action(LogAction::Fold);
            assert!(!app.log_folds.visible(1));
            app.apply_log_action(action);
            assert!(app.log_folds.visible(0));
            assert_eq!(app.log_folds.hidden_count(), 0);
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
            // The stand-in's type is already hidden, so `t` has nothing to hide.
            let hidden_types = app.log_folds.hidden_types.clone();
            assert_eq!(
                screen.contains("[t hide chore]"),
                !hidden_types.contains("chore")
            );
            if hidden_types.contains("chore") {
                app.apply_log_action(LogAction::HideSelectedType);
                assert_eq!(app.log_folds.hidden_types, hidden_types);
            }
            // Expanding reveals the side history and applies the filter.
            app.apply_log_action(LogAction::Fold);
            assert!(!app.log_folds.visible(0));
            assert!(app.log_folds.visible(1));
            assert_eq!(app.log_folds.hidden_count(), 1);
            assert_eq!(app.selected, 1);
        }
    }

    #[test]
    fn routing_rows_select_the_commit_they_lead_to() {
        let mut merge = commit("merge", 1);
        let mut side = commit("side", 1);
        let mut main = commit("main", 1);
        let base = commit("base", 1);
        merge.parents = vec![main.hash.clone(), side.hash.clone()];
        main.parents = vec![base.hash.clone()];
        side.parents = vec![base.hash.clone()];
        side.graph = vec!["|\\".into(), "| * ".into()];
        let mut app = App::new(vec![merge, side, main, base]);
        let mut terminal = Terminal::new(TestBackend::new(60, 10)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        assert_eq!(app.visible_log_rows[..3], [Some(0), Some(1), Some(1)]);
    }

    #[test]
    fn hidden_merges_keep_noninteractive_graph_junctions_for_both_filters() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        for action in [LogAction::Merges, LogAction::Type("chore".into())] {
            let mut merge = commit("chore: hidden merge text", 1);
            let mut side = commit("fix: side", 1);
            let mut main = commit("feat: main", 1);
            let base = commit("base", 1);
            merge.parents = vec![main.hash.clone(), side.hash.clone()];
            main.parents = vec![base.hash.clone()];
            side.parents = vec![base.hash.clone()];
            let mut app = App::new(vec![merge, side, main, base]);
            app.apply_log_action(action);
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
            assert!(row(app.log_row_origin).contains('├'));
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
            assert!(screen.contains('╯') || screen.contains('╮'));
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
            app.apply_log_action(LogAction::Reset);
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
            .find(|(_, a)| *a == LogAction::Merges)
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
        assert_eq!(app.log_folds.hidden_count(), 2);
        assert!(app.log_folds.visible(2) && app.log_folds.visible(3));
        app.search = Some("Merge topic".into());
        app.next_match(false);
        assert_eq!(app.selected, 2);
        app.apply_log_action(LogAction::HideSelectedType);
        // Side is a branch point shared by both merges, so it stays selected.
        assert_eq!(app.log_folds.hidden_count(), 2);
        assert_eq!(app.selected, 2);
        app.replace_commits(app.commits.clone());
        assert!(app.log_folds.hide_merges);
        assert_eq!(app.log_folds.hidden_count(), 2);
        handle(
            Event::Key(KeyEvent::new(KeyCode::Char('M'), KeyModifiers::NONE)),
            &mut app,
        );
        assert!(!app.log_folds.hide_merges);
        assert!(!app.log_folds.visible(0)); // Type filter still applies.
        assert!(app.log_folds.visible(1));
        assert_eq!(app.log_folds.hidden_count(), 1);
        handle(
            Event::Key(KeyEvent::new(KeyCode::Char('M'), KeyModifiers::NONE)),
            &mut app,
        );
        handle(
            Event::Key(KeyEvent::new(KeyCode::Char('T'), KeyModifiers::NONE)),
            &mut app,
        );
        assert!(!app.log_folds.hide_merges);
        assert_eq!(app.log_folds.hidden_count(), 0);
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
        assert_eq!(app.type_buttons[0].1, LogAction::Type("test".into()));
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
        assert_eq!(app.log_folds.hidden_count(), 3);
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
            .find(|(_, kind)| *kind == LogAction::Type("test".into()))
            .unwrap()
            .0;
        click(&mut app, button);
        assert!(app.log_folds.hidden_types.is_empty());
        assert_eq!(app.log_folds.hidden_count(), 0);
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
            app.apply_log_action(LogAction::HideSelectedType);
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
            app.log_folds.ascii_graph = true;
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
                .any(|(_, action)| *action == LogAction::Merges));
            for y in 1..8 {
                for x in 0..100 {
                    if (x, y) != (column, 2) {
                        assert_eq!(before[(x, y)], after[(x, y)], "cell {x},{y}");
                    }
                }
            }
        }
    }

    #[test]
    fn comparison_merges_show_fold_markers() {
        for ascii_graph in [false, true] {
            let mut merge = commit("merge", 1);
            let mut side = commit("side", 1);
            let mut main = commit("main", 1);
            let base = commit("base", 1);
            merge.parents = vec![main.hash.clone(), side.hash.clone()];
            side.parents = vec![base.hash.clone()];
            main.parents = vec![base.hash.clone()];
            merge.graph_node = '<';
            merge.graph = vec!["<   ".into()];
            let mut app = App::new(vec![merge, side, main, base]);
            app.log_folds.ascii_graph = ascii_graph;
            app.log_folds.refresh(&app.commits).unwrap();
            let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            let y = app.log_row_origin;
            assert_eq!(terminal.backend().buffer()[(0, y)].symbol(), "▼");
            app.apply_log_action(LogAction::Fold);
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            assert_eq!(terminal.backend().buffer()[(0, y)].symbol(), "▶");
        }
    }

    #[test]
    fn unicode_merge_nodes_and_routing_rows_keep_selection_and_folds() {
        let mut merge = commit("merge", 1);
        let mut side = commit("side", 1);
        let mut main = commit("main", 1);
        let base = commit("base", 1);
        merge.parents = vec![main.hash.clone(), side.hash.clone()];
        side.parents = vec![base.hash.clone()];
        main.parents = vec![base.hash.clone()];
        let mut app = App::new(vec![merge, side, main, base]);
        app.log_folds.refresh(&app.commits).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let y = app.log_row_origin;
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, y)].symbol(), "▼");
        assert_eq!(buffer[(0, y + 1)].symbol(), "├");
        assert_eq!(buffer[(1, y + 1)].symbol(), "─");
        assert_eq!(buffer[(2, y + 1)].symbol(), "╮");
        assert_eq!(buffer[(2, y + 2)].symbol(), "●");
        assert_eq!(app.visible_log_rows[1], Some(1));
        handle(
            Event::Mouse(crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: 2,
                row: y + 1,
                modifiers: KeyModifiers::NONE,
            }),
            &mut app,
        );
        assert_eq!(app.selected, 1);
        assert_eq!(app.mode, Mode::Log);
        app.selected = 0;
        app.apply_log_action(LogAction::Fold);
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        assert_eq!(terminal.backend().buffer()[(0, y)].symbol(), "▶");
        assert!(!app.log_folds.visible(1));
        app.apply_log_action(LogAction::Fold);
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        assert_eq!(terminal.backend().buffer()[(2, y + 2)].symbol(), "●");
        terminal.resize(Rect::new(0, 0, 20, 4)).unwrap();
        app.bottom();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        assert!(app.visible_log_rows.contains(&Some(3)));
    }

    fn commit(subject: &str, graph_rows: usize) -> Commit {
        Commit {
            kind: CommitKind::Revision,
            annotation: None,
            parents: Vec::new(),
            diff_args: Vec::new(),
            hash: subject.repeat(40).chars().take(40).collect(),
            short_hash: subject.to_owned(),
            decorations: Default::default(),
            author: String::new(),
            author_email: String::new(),
            author_date: String::new(),
            collaborators: crate::git::Collaborators::default(),
            subject: subject.to_owned(),
            graph: vec!["* ".to_owned(); graph_rows],
            traversal: crate::git::Traversal::default(),
            graph_node: '*',
        }
    }

    #[test]
    fn fixup_matching_rejects_ambiguous_newer_and_synthetic_targets() {
        use crate::fixups::{resolve, Target};
        let mut synthetic = commit("working", 1);
        synthetic.kind = CommitKind::WorkingTree;
        let commits = vec![
            commit("fixup! duplicate", 1),
            commit("duplicate", 1),
            commit("duplicate", 1),
            commit("newer", 1),
            commit("fixup! newer", 1),
            commit("squash! feat: 界", 1),
            commit("amend! feat: 界", 1),
            commit("feat: 界", 1),
            commit("fixup! working", 1),
            synthetic,
        ];
        let targets = resolve(&commits, None);
        assert_eq!(targets[0], Some(Target::Ambiguous));
        assert_eq!(targets[4], Some(Target::Missing));
        assert_eq!(targets[5], Some(Target::Commit(7)));
        assert_eq!(targets[6], Some(Target::Commit(7)));
        assert_eq!(targets[8], Some(Target::Missing));
        let mut app = App::new(commits);
        app.refresh_log_folds();
        app.log_format = crate::log_format::LogFormat::parse("%s").unwrap();
        let mut terminal = Terminal::new(TestBackend::new(80, 14)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(screen.contains("fixup! → ambiguous duplicate"));
        assert!(screen.contains("fixup! → missing newer"));
        assert!(app
            .visible_log_links
            .iter()
            .all(|(_, link)| link.target == 7));
    }

    #[test]
    fn fixup_links_follow_display_columns_and_restore_selection_and_filters() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        for format in [None, Some("界 %an (%s) %s")] {
            for width in [30, 120] {
                let mut fixup = commit("fixup! feat: café", 1);
                fixup.short_hash = "1234567".into();
                fixup.author = "界 Alice".into();
                let mut target = commit("feat: café", 1);
                target.short_hash = "abcdef0".into();
                fixup.parents = vec![target.hash.clone()];
                let mut app = App::new(vec![fixup, target]);
                app.refresh_log_folds();
                if let Some(format) = format {
                    app.log_format = crate::log_format::LogFormat::parse(format).unwrap();
                }
                app.apply_log_action(LogAction::Type("feat".into()));
                assert!(!app.log_folds.visible(1));
                let mut terminal = Terminal::new(TestBackend::new(width, 8)).unwrap();
                terminal.draw(|frame| draw(frame, &mut app)).unwrap();
                if app.visible_log_links.is_empty() {
                    assert_eq!(width, 30); // Target hash was clipped entirely.
                    continue;
                }
                for (rect, link) in &app.visible_log_links {
                    assert_eq!(link.target, 1);
                    let visible: String = rect
                        .positions()
                        .map(|position| {
                            let cell = &terminal.backend().buffer()[position];
                            assert!(cell.modifier.contains(Modifier::UNDERLINED));
                            cell.symbol().to_owned()
                        })
                        .collect();
                    assert!("abcdef0".starts_with(&visible), "{visible}");
                }
                if width == 120 {
                    let screen: String = terminal
                        .backend()
                        .buffer()
                        .content
                        .iter()
                        .map(|cell| cell.symbol())
                        .collect();
                    assert!(screen.contains("fixup! → abcdef0 feat: café"));
                }
                let (rect, _) = app.visible_log_links[0];
                handle(
                    Event::Mouse(MouseEvent {
                        kind: MouseEventKind::Down(MouseButton::Left),
                        column: rect.x,
                        row: rect.y,
                        modifiers: KeyModifiers::NONE,
                    }),
                    &mut app,
                );
                assert_eq!(app.mode, Mode::Log);
                assert_eq!(app.selected, 1);
                assert!(app.log_folds.visible(1));
                terminal.draw(|frame| draw(frame, &mut app)).unwrap();
                let source_row = app
                    .visible_log_rows
                    .iter()
                    .position(|index| *index == Some(0))
                    .unwrap();
                assert_eq!(
                    terminal.backend().buffer()[(0, app.log_row_origin + source_row as u16)].bg,
                    Color::Reset
                );
                handle(
                    Event::Key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE)),
                    &mut app,
                );
                assert_eq!(app.selected, 0);
                assert!(!app.log_folds.visible(1));
                assert!(!app.has_reference_back());
            }
        }
    }

    #[test]
    fn returning_from_a_clicked_fixup_link_selects_its_row() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let mut app = App::new(vec![
            commit("fixup! feat: x", 1),
            commit("test: y", 1),
            commit("feat: x", 1),
        ]);
        app.refresh_log_folds();
        app.selected = 1;
        let mut terminal = Terminal::new(TestBackend::new(80, 8)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let (rect, _) = app.visible_log_links[0];
        handle(
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: rect.x,
                row: rect.y,
                modifiers: KeyModifiers::NONE,
            }),
            &mut app,
        );
        assert_eq!(app.selected, 2);
        app.reference_back();
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn returning_from_a_fixup_target_keeps_later_filter_changes() {
        let mut app = App::new(vec![
            commit("fixup! feat: x", 1),
            commit("test: y", 1),
            commit("feat: x", 1),
        ]);
        app.refresh_log_folds();
        app.follow_fixup(LogLink { from: 0, target: 2 });
        app.apply_log_action(LogAction::Type("test".into()));
        assert!(!app.log_folds.visible(1));
        app.reference_back();
        assert_eq!(app.selected, 0);
        assert!(!app.log_folds.visible(1));
    }

    #[test]
    fn returning_from_a_fixup_target_keeps_filters_cleared_since() {
        let mut app = App::new(vec![commit("fixup! feat: x", 1), commit("feat: x", 1)]);
        app.refresh_log_folds();
        app.apply_log_action(LogAction::Type("feat".into()));
        app.follow_fixup(LogLink { from: 0, target: 1 });
        app.apply_log_action(LogAction::Reset);
        app.reference_back();
        assert!(app.log_folds.hidden_types.is_empty());
        assert!(app.log_folds.visible(1));
    }

    #[test]
    fn search_finds_fixup_target_hashes_it_highlights() {
        let mut fixup = commit("fixup! feat: side", 1);
        fixup.hash = "1234567".repeat(6);
        fixup.short_hash = "1234567".into();
        let mut target = commit("feat: side", 1);
        target.short_hash = "abcdef0".into();
        let mut app = App::new(vec![fixup, target]);
        app.refresh_log_folds();
        let key =
            |app: &mut App, code| handle(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)), app);
        key(&mut app, KeyCode::Char('/'));
        for c in "abcdef0".chars() {
            key(&mut app, KeyCode::Char(c));
        }
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.selected, 1);
        key(&mut app, KeyCode::Char('n'));
        assert_eq!(app.selected, 0, "the fixup row shows the target's hash");

        // The inserted hash must not hide the fixup's own subject.
        app.selected = 1;
        app.search_match = None;
        key(&mut app, KeyCode::Char('/'));
        for c in "fixup! feat".chars() {
            key(&mut app, KeyCode::Char(c));
        }
        key(&mut app, KeyCode::Enter);
        assert_eq!(app.selected, 0, "the fixup's subject matches as written");
        let mut terminal = Terminal::new(TestBackend::new(80, 6)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let y = app.log_row_origin
            + app
                .visible_log_rows
                .iter()
                .position(|index| *index == Some(0))
                .unwrap() as u16;
        let marked: String = (0..80)
            .map(|x| &terminal.backend().buffer()[(x, y)])
            .filter(|cell| cell.bg == Color::Yellow)
            .map(|cell| cell.symbol())
            .collect();
        assert_eq!(marked, "fixup! → abcdef0 feat: side");
    }

    #[test]
    fn multiple_fixups_highlight_only_related_hashes_and_reveal_folded_targets() {
        let mut fixup = commit("fixup! feat: side", 1);
        let mut another = commit("fixup! feat: side", 1);
        another.hash = "another".repeat(6);
        let mut merge = commit("merge", 1);
        let mut target = commit("feat: side", 1);
        let mut main = commit("main", 1);
        let base = commit("base", 1);
        fixup.parents = vec![another.hash.clone()];
        another.parents = vec![merge.hash.clone()];
        merge.parents = vec![main.hash.clone(), target.hash.clone()];
        target.parents = vec![base.hash.clone()];
        main.parents = vec![base.hash.clone()];
        let mut commits = vec![fixup, another, merge, target, main, base];
        for (index, commit) in commits.iter_mut().enumerate() {
            commit.short_hash = format!("{:07x}", index + 1);
        }
        let mut app = App::new(commits);
        app.log_folds.start_collapsed = true;
        app.refresh_log_folds();
        assert!(!app.log_folds.visible(3));
        app.follow_fixup(LogLink { from: 0, target: 3 });
        assert!(app.log_folds.visible(3));
        let mut terminal = Terminal::new(TestBackend::new(120, 12)).unwrap();
        for selection in [0, 1, 3] {
            app.selected = selection;
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            let buffer = terminal.backend().buffer();
            let mut checked_hashes = 0;
            for (row, index) in app.visible_log_rows.iter().enumerate() {
                let Some(index) = *index else { continue };
                let y = app.log_row_origin + row as u16;
                let selected = index == selection;
                assert_eq!(
                    buffer[(0, y)].bg,
                    if selected {
                        Color::DarkGray
                    } else {
                        Color::Reset
                    }
                );
                assert_eq!(buffer[(0, y)].modifier.contains(Modifier::BOLD), selected);
                let text: String = (0..120).map(|x| buffer[(x, y)].symbol()).collect();
                // Routing rows carry graph edges without commit metadata.
                let Some(start) = text.find(&app.commits[index].short_hash) else {
                    continue;
                };
                checked_hashes += 1;
                let x = Span::raw(&text[..start]).width() as u16;
                let hash = &buffer[(x, y)];
                let related = if selection == 3 {
                    index < 2
                } else {
                    index == 3
                };
                assert_eq!(
                    hash.fg,
                    if related {
                        Color::LightCyan
                    } else {
                        Color::Yellow
                    }
                );
                assert_eq!(hash.modifier.contains(Modifier::BOLD), selected || related);
            }
            assert_eq!(checked_hashes, app.commits.len());
            assert_eq!(app.visible_log_links.len(), 2);
            assert!(app
                .visible_log_links
                .iter()
                .all(|(_, link)| link.target == 3));
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
        for (codex, claude, gemini, others, glyph, color) in [
            (true, false, false, 0, "꩜", Color::Reset),
            (false, true, false, 0, "❋", Color::Rgb(215, 119, 87)),
            (false, false, true, 0, "✦", Color::Rgb(66, 133, 244)),
            (false, false, false, 1, "1", Color::Gray),
            (true, true, false, 0, "2", Color::Gray),
            (false, true, false, 1, "2", Color::Gray),
            (false, false, true, 1, "2", Color::Gray),
        ] {
            let mut c = crate::log_format::tests::commit();
            c.collaborators = crate::git::Collaborators {
                codex,
                claude,
                gemini,
                others,
                ..Default::default()
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
    fn cached_log_columns_follow_history_refresh_and_format_changes() {
        let mut short = crate::log_format::tests::commit();
        short.author_date = "Nov 1".into();
        short.author = "Al".into();
        short.decorations.clear();
        let mut long = short.clone();
        long.hash = "other".into();
        long.author_date = "2026-11-01 15:30".into();
        let mut app = App::new(vec![short.clone(), long]);
        let mut terminal = Terminal::new(TestBackend::new(110, 4)).unwrap();
        let author_column = |app: &mut App, terminal: &mut Terminal<TestBackend>| {
            terminal.draw(|frame| draw(frame, app)).unwrap();
            let text: String = (0..110)
                .map(|x| terminal.backend().buffer()[(x, app.log_row_origin)].symbol())
                .collect();
            text.find("Al").unwrap() - text.find(&app.commits[0].author_date).unwrap_or(0)
        };
        let wide = author_column(&mut app, &mut terminal);
        assert_eq!(author_column(&mut app, &mut terminal), wide);
        app.log_format = crate::log_format::LogFormat::parse("%an %s").unwrap();
        assert_eq!(author_column(&mut app, &mut terminal), 2);
        app.log_format = crate::log_format::LogFormat::default();
        assert_eq!(author_column(&mut app, &mut terminal), wide);
        app.replace_commits(vec![short]);
        assert_eq!(author_column(&mut app, &mut terminal), wide - 11);
        let mut longer = app.commits[0].clone();
        longer.author_date = "2026-11-01 15:30 UTC".into();
        app.replace_commits(vec![longer]);
        assert_eq!(author_column(&mut app, &mut terminal), wide + 4);
    }

    fn reflog_event(hash: &str, date: &str, actor: &str, action: &str) -> Commit {
        let mut commit = crate::log_format::tests::commit();
        commit.hash = hash.into();
        commit.short_hash = hash.into();
        // Reflog must measure update metadata, rather than commit metadata.
        commit.author = "Original author, not the ref updater".into();
        commit.author_date = "Original commit date, not the ref update time".into();
        commit.annotation = Some(crate::git::Annotation::Reflog(crate::git::ReflogEntry {
            reference: "HEAD".into(),
            show_reference: false,
            rebasing: false,
            updated_at: date.into(),
            actor: actor.into(),
            action: action.into(),
        }));
        commit
    }

    #[test]
    fn reflog_columns_align_update_metadata_across_visible_graph_lanes() {
        let mut commits = vec![
            reflog_event("aaaaaaa", "Nov 1", "Al", "commit: event-0"),
            reflog_event(
                "bbbbbbbbb",
                "2026-11-01 15:06:47",
                "李 e\u{301}",
                "commit: event-1",
            ),
            reflog_event(
                "ccccccc",
                "Nov 1",
                "ExtremelyLongUnbrokenActorNameToExerciseTheLimit",
                "commit: event-2",
            ),
            reflog_event(
                "ddddddd",
                "Nov 1",
                "Margaret Hamilton",
                "commit (initial): event-3",
            ),
        ];
        commits[0].parents = vec![commits[2].hash.clone()];
        commits[1].parents = vec![commits[2].hash.clone()];
        commits[2].parents = vec![commits[3].hash.clone()];
        let mut app = App::new(commits);
        let mut terminal = Terminal::new(TestBackend::new(130, 4)).unwrap();
        for (selected, author_width) in [(0, 4), (3, 32), (0, 4)] {
            app.selected = selected;
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            let mut date_head = None;
            for (row, index) in app.visible_log_rows.iter().enumerate() {
                let index = index.unwrap();
                let commit = &app.commits[index];
                let entry = commit.reflog().unwrap();
                let y = app.log_row_origin + row as u16;
                // Use cell coordinates so wide and combining characters do
                // not accidentally count as extra author padding.
                let column = |text: &str| {
                    (0..130 - text.len() as u16)
                        .find(|&x| {
                            text.chars().enumerate().all(|(offset, ch)| {
                                terminal.backend().buffer()[(x + offset as u16, y)].symbol()
                                    == ch.to_string()
                            })
                        })
                        .unwrap()
                };
                let date = column(&entry.updated_at);
                assert_eq!(date, *date_head.get_or_insert(date));
                let actor = date + 20; // The history-wide update date width is 19.
                let action = column(entry.action.split_once(": ").unwrap().0);
                assert_eq!(action, actor + author_width + 1);
                assert_eq!(terminal.backend().buffer()[(date, y)].fg, Color::Gray);
                assert_eq!(terminal.backend().buffer()[(actor, y)].fg, Color::Cyan);
                assert_eq!(
                    terminal.backend().buffer()[(action, y)].fg,
                    Color::Rgb(255, 165, 0)
                );
                let graph_width = Span::raw(crate::ansi::plain(
                    &app.log_folds.display_graph(index, commit)[0],
                ))
                .width();
                assert_eq!(column(&commit.short_hash) as usize, graph_width);
            }
        }
        // Replacing the history also invalidates Reflog's cached date width.
        app.replace_commits(vec![reflog_event(
            "eeeeeee",
            "Nov 2",
            "Al",
            "commit: refreshed",
        )]);
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let y = app.log_row_origin;
        let text: String = (0..130)
            .map(|x| terminal.backend().buffer()[(x, y)].symbol())
            .collect();
        assert!(text.contains("Nov 2 Al commit: refreshed"), "{text}");
    }

    #[test]
    fn reflog_fold_controls_follow_hashes_before_column_padding() {
        let mut commits = vec![
            reflog_event(
                "aaaaaaa",
                "Nov 1",
                "Margaret Hamilton",
                "commit (amend): amended",
            ),
            reflog_event("bbbbbbbbb", "Nov 1", "Al", "commit: original"),
            reflog_event("ccccccc", "Nov 1", "Al", "commit (initial): base"),
        ];
        commits[0].parents = vec![commits[2].hash.clone()];
        commits[1].parents = vec![commits[2].hash.clone()];
        let mut app = App::new(commits);
        let mut terminal = Terminal::new(TestBackend::new(130, 6)).unwrap();
        for (step, marker, date_column) in [(0, "▶", 12), (1, "▼", 14), (2, "▶", 12)] {
            if step > 0 {
                app.apply_log_action(LogAction::Fold);
            }
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            for (row, index) in app.visible_log_rows.iter().enumerate() {
                let index = index.unwrap();
                let commit = &app.commits[index];
                let graph = crate::ansi::plain(&app.log_folds.display_graph(index, commit)[0]);
                let y = app.log_row_origin + row as u16;
                let text: String = (0..130)
                    .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                    .collect();
                // No fold slot separates any hash from its graph. Only the
                // fold header gets a marker, immediately after its hash.
                let prefix = if index == 0 {
                    format!("{graph}{} {marker}", commit.short_hash)
                } else {
                    format!("{graph}{} ", commit.short_hash)
                };
                assert!(text.starts_with(&prefix), "{text}");
                let date = text.find("Nov 1").unwrap();
                assert_eq!(Span::raw(&text[..date]).width(), date_column);
                if index == 0 {
                    let column = (Span::raw(&graph).width() + commit.short_hash.len() + 1) as u16;
                    assert_eq!(terminal.backend().buffer()[(column, y)].symbol(), marker);
                    assert_eq!(terminal.backend().buffer()[(column, y)].fg, Color::Yellow);
                    if marker == "▶" {
                        assert!(text.contains("· 1 earlier version"), "{text}");
                    }
                }
            }
        }
        // Inserting the marker must also preserve shortened-actor search ranges.
        app.search = Some("Hamilton".into());
        app.search_match = Some((Mode::Log, 0));
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let y = app.log_row_origin;
        let text: String = (0..130)
            .map(|x| terminal.backend().buffer()[(x, y)].symbol())
            .collect();
        let actor = text.find("Margaret H.").unwrap();
        let actor = Span::raw(&text[..actor]).width() as u16;
        assert_eq!(terminal.backend().buffer()[(actor, y)].bg, Color::Yellow);
    }

    #[test]
    fn reflog_fold_controls_show_in_formats_without_aligned_hashes() {
        for format in ["%s", "%h %s"] {
            let mut commits = vec![
                reflog_event("aaaaaaa", "Nov 1", "Al", "commit (amend): amended"),
                reflog_event("bbbbbbb", "Nov 1", "Al", "commit: original"),
                reflog_event("ccccccc", "Nov 1", "Al", "commit (initial): base"),
            ];
            commits[0].parents = vec!["ccccccc".into()];
            commits[1].parents = vec!["ccccccc".into()];
            let mut app = App::new(commits);
            app.log_format = crate::log_format::LogFormat::parse(format).unwrap();
            let mut terminal = Terminal::new(TestBackend::new(80, 4)).unwrap();
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            let row = |y: u16| -> String {
                (0..80)
                    .map(|x| terminal.backend().buffer()[(x, app.log_row_origin + y)].symbol())
                    .collect()
            };
            let (folded, plain) = (row(0), row(1));
            assert!(folded.contains("▶ "), "{format}: {folded}");
            // Rows without a control keep its slot, aligning their text.
            let column = |row: &str, text| row[..row.find(text).unwrap()].chars().count();
            assert_eq!(
                column(&folded, "commit (amend)"),
                column(&plain, "commit (initial)"),
                "{format}:\n{folded}\n{plain}"
            );
        }
    }

    #[test]
    fn reflog_search_reveals_and_highlights_a_shortened_actor_in_a_fold() {
        let mut commits = vec![
            reflog_event("aaaaaaa", "Nov 1", "Al", "commit (amend): amended"),
            reflog_event("bbbbbbb", "Nov 1", "Margaret Hamilton", "commit: original"),
            reflog_event("ccccccc", "Nov 1", "Al", "commit (initial): base"),
        ];
        commits[0].parents = vec![commits[2].hash.clone()];
        commits[1].parents = vec![commits[2].hash.clone()];
        let mut app = App::new(commits);
        assert!(!app.log_folds.visible(1));
        app.search_input = Some("Hamilton".into());
        app.submit_search();
        assert_eq!(app.selected, 1);
        assert!(app.log_folds.visible(1));
        let mut terminal = Terminal::new(TestBackend::new(130, 6)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let y = app.log_row_origin
            + app
                .visible_log_rows
                .iter()
                .position(|&row| row == Some(1))
                .unwrap() as u16;
        let text: String = (0..130)
            .map(|x| terminal.backend().buffer()[(x, y)].symbol())
            .collect();
        assert!(!text.contains("Hamilton"), "{text}");
        let actor = text.find("Margaret H.").unwrap();
        assert_eq!(
            terminal.backend().buffer()[(actor as u16, y)].bg,
            Color::Yellow
        );
    }

    #[test]
    fn search_marks_shortened_authors_matched_by_their_full_name() {
        let mut commit = crate::log_format::tests::commit();
        commit.decorations.clear();
        commit.author = "copilot-swe-agent[bot]".into();
        commit.subject = "subject".into();
        let mut app = App::new(vec![commit]);
        app.search = Some("SWE-agent".into());
        app.search_match = Some((Mode::Log, 0));
        let mut terminal = Terminal::new(TestBackend::new(110, 4)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let row: Vec<_> = (0..110).map(|x| &buffer[(x, app.log_row_origin)]).collect();
        let text: String = row.iter().map(|cell| cell.symbol()).collect();
        let author = text.find("Copilot").unwrap();
        assert_eq!(row[author].bg, Color::Yellow);
        assert_ne!(row[text.find("subject").unwrap()].bg, Color::Yellow);
    }

    #[test]
    fn search_marks_cut_off_author_labels_that_match() {
        let mut commit = crate::log_format::tests::commit();
        commit.decorations.clear();
        // Even with initials, the label is too wide for the column.
        commit.author = "Maximilianusvonhabsburgerlothringen Ferdinand".into();
        commit.subject = "subject".into();
        let mut app = App::new(vec![commit]);
        app.search = Some("N F.".into());
        app.search_match = Some((Mode::Log, 0));
        let mut terminal = Terminal::new(TestBackend::new(110, 4)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let row: Vec<_> = (0..110).map(|x| &buffer[(x, app.log_row_origin)]).collect();
        let text: String = row.iter().map(|cell| cell.symbol()).collect();
        assert!(!text.contains("n F."), "{text}");
        assert_eq!(row[text.find("Maximilianus").unwrap()].bg, Color::Yellow);
    }

    #[test]
    fn default_log_graph_hash_column_uses_visible_commit_rows() {
        let mut commits: Vec<_> = (0..4)
            .map(|index| {
                let mut commit = crate::log_format::tests::commit();
                commit.hash = index.to_string();
                commit.short_hash = if index == 1 { "bbbbbbbbb" } else { "aaaaaaa" }.into();
                commit.subject = format!("subject-{index}");
                commit.decorations.clear();
                commit.author = if index == 3 { "Longest author" } else { "Al" }.into();
                commit.graph = vec![match index {
                    1 => "| * ",
                    3 => "| | | | | * ",
                    _ => "* ",
                }
                .into()];
                commit
            })
            .collect();
        commits[0].decorations = "HEAD -> main".into();
        let mut app = App::new(commits);
        let mut terminal = Terminal::new(TestBackend::new(110, 4)).unwrap();
        for show_hash in [true, false] {
            if !show_hash {
                app.log_format.toggle(crate::log_format::Field::Hash);
            }
            for (selected, graph_width, hash_width) in [(0, 4, 9), (3, 12, 7), (0, 4, 9)] {
                app.selected = selected;
                terminal.draw(|frame| draw(frame, &mut app)).unwrap();
                let mut subject_head = None;
                for (row, index) in app.visible_log_rows.iter().enumerate() {
                    let Some(index) = index else { continue };
                    let commit = &app.commits[*index];
                    let y = app.log_row_origin + row as u16;
                    let text: String = (0..110)
                        .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                        .collect();
                    let column = |start| Span::raw(&text[..start]).width();
                    if show_hash {
                        assert_eq!(
                            column(text.find(&commit.short_hash).unwrap()),
                            Span::raw(&commit.graph[0]).width()
                        );
                    } else {
                        assert!(!text.contains(&commit.short_hash));
                    }
                    assert_eq!(
                        column(text.find(&commit.author_date).unwrap()),
                        graph_width + if show_hash { hash_width + 1 } else { 0 }
                    );
                    let start = text
                        .find(if *index == 0 { "(HEAD" } else { "subject-" })
                        .unwrap();
                    assert_eq!(column(start), *subject_head.get_or_insert(column(start)));
                }
            }
        }
    }

    #[test]
    fn author_columns_follow_visible_labels_and_exclude_filtered_authors() {
        let names = [
            "copilot-swe-agent[bot]",
            "dependabot[bot]",
            "Longer-author-outside-viewport",
            "李 e\u{301}",
        ];
        let commits = names
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let mut commit = crate::log_format::tests::commit();
                commit.hash = index.to_string();
                commit.author = (*name).into();
                commit.subject = format!(
                    "{}: subject-{index}",
                    if index == 2 { "chore" } else { "feat" }
                );
                commit.decorations.clear();
                commit.collaborators.codex = index == 0 || index == 3;
                commit.collaborators.others = usize::from(index == 1);
                commit
            })
            .collect();
        let mut app = App::new(commits);
        let render = |app: &mut App, terminal: &mut Terminal<TestBackend>, width| {
            terminal.draw(|frame| draw(frame, app)).unwrap();
            for (row, index) in app.visible_log_rows.iter().enumerate() {
                let Some(index) = index else { continue };
                let commit = &app.commits[*index];
                let y = app.log_row_origin + row as u16;
                // Inspect actual cell coordinates: emoji continuation cells
                // must not count as extra spaces in the author column.
                let column = |text: &str| {
                    (0..110 - text.len() as u16)
                        .find(|&x| {
                            text.chars().enumerate().all(|(offset, ch)| {
                                terminal.backend().buffer()[(x + offset as u16, y)].symbol()
                                    == ch.to_string()
                            })
                        })
                        .unwrap()
                };
                let author_start =
                    column(&commit.author_date) + commit.author_date.len() as u16 + 1;
                assert_eq!(column(&commit.subject) - author_start, width + 1);
            }
        };
        let mut small = Terminal::new(TestBackend::new(110, 4)).unwrap();
        render(&mut app, &mut small, 15);
        app.selected = 3;
        let long_width = Span::raw(names[2]).width() as u16;
        render(&mut app, &mut small, long_width);
        app.selected = 0;
        render(&mut app, &mut small, 15);
        let mut large = Terminal::new(TestBackend::new(110, 8)).unwrap();
        render(&mut app, &mut large, long_width);
        app.apply_log_action(LogAction::Type("chore".into()));
        render(&mut app, &mut large, 15);
        assert!(!app.visible_log_rows.contains(&Some(2)));
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
    fn makefile_tabs_keep_text_and_cursor_rows_aligned() {
        let mut terminal = Terminal::new(TestBackend::new(36, 10)).unwrap();
        let mut app = App::new(Vec::new());
        app.mode = Mode::Show;
        app.show_text = concat!(
            " \x1b[38;2;230;219;116m\tbin/chopi.sh \\\x1b[0m\n",
            "\x1b[48;2;0;40;0m+\x1b[38;2;230;219;116m\t.internal/session-proxy.sh \\\x1b[0m\x1b[48;2;0;40;0m\x1b[0K\x1b[0m\n",
            "after",
        )
        .to_owned();
        app.ensure_show_rows();
        for cursor in 0..3 {
            app.show_cursor = cursor;
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            assert_eq!(app.show_row_starts, vec![0, 1, 2, 3]);
            let row_text = |y| -> String {
                (0..36)
                    .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                    .collect()
            };
            assert!(row_text(1).starts_with("        bin/chopi.sh"));
            assert!(row_text(2).starts_with("+       .internal/session-proxy.sh"));
            for row in 0..3 {
                assert_eq!(
                    terminal.backend().buffer()[(35, row + 1)].bg,
                    if row as usize == cursor {
                        if cursor == 1 {
                            Color::Rgb(32, 72, 32)
                        } else {
                            Color::DarkGray
                        }
                    } else if row == 1 {
                        Color::Rgb(0, 40, 0)
                    } else {
                        Color::Reset
                    },
                    "cursor={cursor}, row={row}",
                );
            }
        }
    }

    #[test]
    fn tab_expansion_preserves_search_and_link_positions() {
        use crate::commit_links::{Link, Target};
        let mut terminal = Terminal::new(TestBackend::new(16, 8)).unwrap();
        let mut app = App::new(Vec::new());
        app.mode = Mode::Show;
        app.show_text = "\t1234567890123456\n\tlink".to_owned();
        app.ensure_show_rows();
        let target = Target::Url("https://example.com".to_owned());
        app.show_links.insert(
            1,
            vec![Link {
                range: 1..5,
                target: target.clone(),
            }],
        );
        app.search = Some("link".to_owned());
        app.search_match = Some((Mode::Show, 1));
        app.show_cursor = 1;
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert_eq!(app.show_row_starts, vec![0, 2, 3]);
        assert_eq!(
            app.visible_show_links,
            vec![(Rect::new(8, 3, 4, 1), target)]
        );
        for x in 8..12 {
            let cell = &terminal.backend().buffer()[(x, 3)];
            assert_eq!(cell.bg, Color::Yellow);
            assert!(cell.modifier.contains(Modifier::UNDERLINED));
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
        for input in [
            "abcdefghijklmno",
            "界界界界界界",
            "one two three four",
            "one\ttwo\tthree\tfour",
            "",
        ] {
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
