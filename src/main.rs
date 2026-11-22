mod activity;
mod ansi;
mod app;
mod bisect;
mod blame;
mod branches;
mod code_stats;
mod commit_links;
mod compact_graph;
mod diff;
mod editor;
mod file_filters;
mod fixups;
mod git;
mod github_search;
mod grep;
mod help;
mod hunk;
mod images;
mod input;
mod log_folds;
mod log_format;
mod message_format;
mod operation;
mod pager;
mod patch_options;
mod path_groups;
mod range_diff;
mod search_history;
mod stash;
mod status;
mod summary_paths;
mod terminal_title;
mod ui;
mod views;

use std::{
    env, io,
    io::IsTerminal,
    process::ExitCode,
    time::{Duration, Instant},
};

use app::App;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::Terminal;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if let Some(information) = help::information(&args) {
        println!("{information}");
        return ExitCode::SUCCESS;
    }
    let (command, command_args) = parse_command(&args);
    let command_args = match initialize_session(command, command_args) {
        Ok(args) => args,
        Err(error) => {
            eprintln!("glog: {error}");
            return ExitCode::FAILURE;
        }
    };
    let command_args = command_args.as_slice();
    let (log_format, log_options, git_args) = match if command == Command::Log {
        log_format::parse_args(command_args)
    } else {
        Ok((
            log_format::LogFormat::default(),
            log_format::LogOptions::default(),
            command_args.to_vec(),
        ))
    } {
        Ok(options) => options,
        Err(error) => {
            eprintln!("glog: {error}");
            return ExitCode::FAILURE;
        }
    };
    let watch = log_options.watch || command == Command::Status;
    let watch_source = match command {
        Command::Status => Some(app::WatchSource::log(Vec::new())),
        Command::Log => Some(app::WatchSource::log(git_args.clone())),
        _ => None,
    };
    let watch_snapshot = watch_source
        .as_ref()
        .filter(|_| watch)
        .map(|source| source.snapshot(None));
    let loaded = match command {
        Command::Status => status::load_app(command_args),
        Command::Show => git::load_show_app(command_args),
        Command::Diff => git::load_diff_app(command_args),
        Command::RangeDiff => range_diff::load_app(command_args),
        Command::Grep => grep::load_app(command_args),
        Command::GithubSearch => github_search::load_app(command_args),
        Command::Reflog if io::stdout().is_terminal() => Ok(git::start_reflog_app(command_args)),
        Command::Reflog => git::load_reflog_app(command_args),
        Command::Stash => stash::load_app(command_args),
        Command::Bisect => bisect::load_app(command_args),
        Command::Branch => match branches::load(command_args) {
            Ok(branches::Launch::Print(text)) => {
                print!("{text}");
                return ExitCode::SUCCESS;
            }
            Ok(branches::Launch::Interactive(app)) => Ok(*app),
            Err(error) => Err(error),
        },
        Command::Blame => blame::load_app(command_args),
        Command::Log => match &watch_source {
            Some(source) if watch => source.load(None),
            _ => git::load_log_at(&git_args, None),
        }
        .map(App::new),
    };
    let mut app = match loaded {
        Ok(app) => app,
        Err(error) => {
            eprintln!("glog: {error}");
            return ExitCode::FAILURE;
        }
    };
    let watch = watch || app.watch;
    initialize_views(&mut app, command, &git_args);
    // Watching waits for content, as Status always does.
    if !watch && app.pending_reflog.is_none() && app.is_empty() {
        eprintln!("glog: {}", command.empty_message(app.history));
        return ExitCode::SUCCESS;
    }

    if command == Command::Log {
        app.title_revision = git::log_title_revision(&git_args);
    }
    app.load_search_history();
    app.exit_on_esc = log_options.exit_on_esc;
    initialize_log_filters(&mut app, log_options);

    let mut terminal = match start_terminal() {
        Ok(terminal) => terminal,
        Err(error) => {
            eprintln!("glog: could not initialize terminal: {error}");
            return ExitCode::FAILURE;
        }
    };
    let mut guard = TerminalGuard(true);
    app.enable_images();
    if command == Command::Log {
        app.log_format = log_format;
    }
    app.watch = watch;
    if let Some(source) = watch_source {
        app.set_watch_source(source, watch_snapshot);
    }
    app.context = args.join(" ");
    let mut title = terminal_title::Title::new();
    let result = run(&mut terminal, &mut app, &mut title);
    let title_result = title.restore();
    let _ = app.images.clear(&mut io::stdout());
    let restored = title_result.and(guard.restore());
    let result = result.and_then(|message| {
        restored?;
        if let Some(picker) = app.branch_picker() {
            ansi::write_lines(io::stdout().lock(), branches::summary(picker))?;
        }
        Ok(message)
    });
    match result {
        Ok(message) => {
            if let Some(message) = message {
                eprintln!("glog: {message}");
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("glog: {error}");
            ExitCode::FAILURE
        }
    }
}

fn initialize_views(app: &mut App, command: Command, git_args: &[String]) {
    app.views.active = match command {
        Command::Status => views::View::StatusLog,
        // A scoped Log cannot include the repository-wide working tree. Keep
        // it separately so Status and Log can open an unrestricted view.
        // An unrestricted Log shares this slot even when initially unwatched:
        // W adds the Working tree entry, which opens Status with Enter or Tab.
        Command::Log if git::log_shows_working_tree(git_args) => views::View::StatusLog,
        Command::Branch => views::View::Branches,
        Command::Stash => views::View::Stashes,
        Command::Reflog => views::View::Reflog,
        _ => {
            app.views.original_label = Some(match command {
                Command::Log => "Log",
                Command::Show => "Show",
                Command::Diff => "Diff",
                Command::Blame => "Blame",
                Command::RangeDiff => "Range diff",
                Command::Grep => "Grep",
                Command::GithubSearch => "GitHub search",
                Command::Bisect => "Bisect",
                _ => unreachable!(),
            });
            views::View::Original
        }
    };
}

fn initialize_log_filters(app: &mut App, options: log_format::LogOptions) {
    if app.history != app::History::Log {
        return;
    }
    app.log_folds.ascii_graph = options.ascii_graph;
    app.log_folds.start_collapsed = options.fold_merges;
    app.log_folds.hidden_types = options.hidden_types;
    app.log_folds.hide_merges = options.hide_merges;
    // As on watch refreshes, a failure leaves the Log unfolded and says why.
    app.refresh_log_folds();
    app.log_folds.reveal(app.selected, &app.commits);
    if app.mode == app::Mode::Log {
        app.top();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Command {
    Log,
    Reflog,
    Blame,
    Stash,
    Branch,
    Bisect,
    Show,
    Diff,
    RangeDiff,
    Grep,
    GithubSearch,
    Status,
}

impl Command {
    /// Whether the arguments are search patterns, which go to Git whole,
    /// even those spelled like display options.
    fn takes_patterns(self) -> bool {
        matches!(self, Command::Grep | Command::GithubSearch)
    }

    /// Why the command has nothing to browse.
    fn empty_message(self, history: app::History) -> &'static str {
        use Command::*;
        match self {
            Diff | RangeDiff => "no changes",
            Reflog => "no reflog entries matched",
            Grep | GithubSearch => "no matches",
            Blame => "the file is empty",
            Log | Show | Branch | Stash | Status if history == app::History::Blame => {
                "the file is empty"
            }
            Stash => "no stashes matched",
            Bisect => "no bisect session in progress",
            Log | Show | Branch | Status => "no commits matched",
        }
    }
}

/// Apply the patch and date options a session shares, returning the
/// arguments left for the command.
fn initialize_session(command: Command, args: &[String]) -> Result<Vec<String>, String> {
    if command.takes_patterns() {
        patch_options::initialize(patch_options::Options::default());
        blame::initialize_date(&[]);
        return Ok(args.to_vec());
    }
    let (settings, args) = patch_options::parse_args(args)?;
    patch_options::initialize(settings);
    blame::initialize_date(&args);
    Ok(args)
}

fn parse_command(args: &[String]) -> (Command, &[String]) {
    if args
        .get(..3)
        .is_some_and(|prefix| prefix == ["gh", "search", "code"])
    {
        return (Command::GithubSearch, &args[3..]);
    }
    match args.first().map(String::as_str) {
        Some("reflog") => (Command::Reflog, &args[1..]),
        Some("blame") => (Command::Blame, &args[1..]),
        Some("stash") => (Command::Stash, &args[1..]),
        Some("branch") => (Command::Branch, &args[1..]),
        Some("bisect") => (Command::Bisect, &args[1..]),
        Some("status") => (Command::Status, &args[1..]),
        Some("show") => (Command::Show, &args[1..]),
        Some("diff") => (Command::Diff, &args[1..]),
        Some("range-diff") => (Command::RangeDiff, &args[1..]),
        Some("grep") => (Command::Grep, &args[1..]),
        Some("log") => (Command::Log, &args[1..]),
        _ => (Command::Log, args),
    }
}

fn start_terminal() -> io::Result<Terminal<ratatui::backend::CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    if let Err(error) = execute!(stdout, EnterAlternateScreen, EnableMouseCapture) {
        let _ = disable_raw_mode();
        return Err(error);
    }
    let backend = ratatui::backend::CrosstermBackend::new(stdout);
    match Terminal::new(backend) {
        Ok(terminal) => Ok(terminal),
        Err(error) => {
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
            Err(error)
        }
    }
}

fn run<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    title: &mut terminal_title::Title,
) -> io::Result<Option<&'static str>> {
    let mut last_watch = Instant::now();
    let mut last_status = Instant::now();
    let mut deferred = None;
    // App state changes only within this loop, so a frame is drawn only after
    // input, a completed background task, or a refresh.
    let mut changed = true;
    while !app.quit {
        if let Some(mut command) = app.pending_editor.take() {
            app.images.clear(&mut io::stdout())?;
            disable_raw_mode()?;
            execute!(
                io::stdout(),
                DisableMouseCapture,
                LeaveAlternateScreen,
                crossterm::cursor::Show
            )?;
            title.restore()?;
            let result = command.status();
            // Restore the UI even when the editor is missing or exits with an error.
            enable_raw_mode()?;
            execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
            terminal.hide_cursor()?;
            terminal.clear()?;
            match result {
                Ok(status) if status.success() => app.set_external_error(None),
                Ok(status) => app.set_external_error(Some(format!("Editor exited with {status}"))),
                Err(error) => {
                    app.set_external_error(Some(format!("Could not open editor: {error}")))
                }
            }
            if let Some(view) = &mut app.status_view {
                let _ = view.refresh();
            }
            changed = true;
        }
        if app.redraw {
            app.images.clear(&mut io::stdout())?;
            terminal.clear()?;
            app.redraw = false;
            changed = true;
        }
        changed |= app.images.poll();
        changed |= app.poll_github_file();
        changed |= app.poll_branch_diff();
        if app.history == app::History::Branches {
            changed |= app.branch_pull_requests.poll(&mut app.commits, app.watch);
            changed |= app.branch_similarity.poll(&mut app.commits);
        }
        changed |= app.poll_browser();
        let opening_standalone_reflog = app.opening_reflog() && app.views.saved.is_empty();
        match app.poll_reflog() {
            Ok(true) if opening_standalone_reflog && app.commits.is_empty() && !app.watch => {
                return Ok(Some(Command::Reflog.empty_message(app.history)))
            }
            Ok(loaded) => changed |= loaded,
            Err(error) if opening_standalone_reflog => return Err(io::Error::other(error)),
            Err(error) => {
                app.status = Some(error);
                changed = true;
            }
        }
        changed |= app.poll_log();
        changed |= app.poll_code_counts();
        changed |= app.poll_keyboard_layout_hint(Instant::now());
        changed |= app.poll_activity(Instant::now());
        if changed {
            title.update(app)?;
            terminal.draw(|frame| ui::draw(frame, app))?;
            app.images.flush(&mut io::stdout())?;
            changed = false;
        }
        app.start_interchanges();
        // Show the first reflog rows and restore a visual bell promptly.
        let timeout =
            if app.opening_reflog() || app.keyboard_layout_hint.is_some_and(|hint| hint.flashing) {
                Duration::from_millis(25)
            } else {
                Duration::from_millis(250)
            };
        let first = match deferred.take() {
            Some(click) => Some(click),
            None if event::poll(timeout)? => Some(event::read()?),
            None => None,
        };
        if let Some(first) = first {
            (changed, deferred) = handle_burst(app, first, || {
                event::poll(Duration::ZERO)?.then(event::read).transpose()
            })?;
        }
        if app.mode == app::Mode::Status && last_status.elapsed() >= Duration::from_secs(1) {
            if let Some(view) = &mut app.status_view {
                // The view records a failed refresh in its footer.
                let _ = view.refresh();
                changed = true;
            }
            last_status = Instant::now();
        }
        if app.watch && last_watch.elapsed() >= Duration::from_secs(1) {
            changed |= app.poll_watch();
            last_watch = Instant::now();
        }
    }
    Ok(None)
}

/// Terminals report a fast trackpad/wheel gesture as a burst of discrete
/// events. Apply `first` and the events queued behind it before drawing
/// another frame. A click ends the burst, since it targets the screen as last
/// drawn; it's returned to be handled once a frame shows the burst's effect.
/// Also returns whether the handled events may have changed what's drawn.
fn handle_burst(
    app: &mut App,
    first: event::Event,
    mut next: impl FnMut() -> io::Result<Option<event::Event>>,
) -> io::Result<(bool, Option<event::Event>)> {
    let mut changed = input::handle(first, app);
    for _ in 0..1024 {
        if app.quit {
            break;
        }
        let Some(event) = next()? else {
            break;
        };
        if input::is_click(&event) {
            return Ok((changed, Some(event)));
        }
        changed |= input::handle(event, app);
    }
    Ok((changed, None))
}

struct TerminalGuard(bool);

impl TerminalGuard {
    fn restore(&mut self) -> io::Result<()> {
        if !self.0 {
            return Ok(());
        }
        self.0 = false;
        disable_raw_mode()?;
        execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use app::LogAction;

    #[test]
    fn clicks_after_a_wheel_burst_wait_for_the_scrolled_frame() {
        use event::{Event, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        let mut app = App::new(Vec::new());
        app.mode = app::Mode::Show;
        app.show_rows = (0..30)
            .map(|source| app::ShowRow {
                preview: None,
                path: None,
                text: String::new(),
                source,
                file: None,
                folded: false,
                fold_separator: false,
                summary: false,
            })
            .collect();
        app.visible_show_rows = 5;
        let mouse = |kind| {
            Event::Mouse(MouseEvent {
                kind,
                column: 0,
                row: 2,
                modifiers: KeyModifiers::NONE,
            })
        };
        let wheel = mouse(MouseEventKind::ScrollDown);
        let click = mouse(MouseEventKind::Down(MouseButton::Left));
        let mut queue = vec![wheel.clone(), click.clone(), wheel.clone()].into_iter();

        let (_, deferred) = handle_burst(&mut app, wheel.clone(), || Ok(queue.next())).unwrap();
        assert_eq!(deferred, Some(click.clone()));
        assert_eq!(
            app.show_offset, 6,
            "the wheel events before the click apply"
        );
        assert_eq!(
            queue.next(),
            Some(wheel),
            "events after the click stay queued"
        );

        // After the frame is drawn, the deferred click is handled first.
        let mut queue = std::iter::empty();
        assert_eq!(
            handle_burst(&mut app, click, || Ok(queue.next())).unwrap(),
            (true, None)
        );
    }

    #[test]
    fn commands_preserve_log_shorthand_and_escape_reserved_names() {
        for (args, show, remaining) in [
            (vec!["reflog", "main"], Command::Reflog, vec!["main"]),
            (
                vec!["blame", "src/main.rs"],
                Command::Blame,
                vec!["src/main.rs"],
            ),
            (vec!["log", "blame"], Command::Log, vec!["blame"]),
            (vec!["log", "reflog"], Command::Log, vec!["reflog"]),
            (vec!["branch"], Command::Branch, vec![]),
            (vec!["bisect", "log"], Command::Bisect, vec!["log"]),
            (vec!["log", "branch"], Command::Log, vec!["branch"]),
            (vec!["stash"], Command::Stash, vec![]),
            (
                vec!["stash", "show", "1"],
                Command::Stash,
                vec!["show", "1"],
            ),
            (vec!["log", "stash"], Command::Log, vec!["stash"]),
            (vec!["status"], Command::Status, vec![]),
            (vec!["log", "status"], Command::Log, vec!["status"]),
            (vec!["show", "HEAD~2"], Command::Show, vec!["HEAD~2"]),
            (vec!["log", "show"], Command::Log, vec!["show"]),
            (vec!["log", "log"], Command::Log, vec!["log"]),
            (
                vec!["main", "--", "show"],
                Command::Log,
                vec!["main", "--", "show"],
            ),
            (vec![], Command::Log, vec![]),
            (vec!["diff"], Command::Diff, vec![]),
            (vec!["diff", "--cached"], Command::Diff, vec!["--cached"]),
            (vec!["log", "diff"], Command::Log, vec!["diff"]),
            (
                vec!["range-diff", "old...new"],
                Command::RangeDiff,
                vec!["old...new"],
            ),
            (vec!["log", "range-diff"], Command::Log, vec!["range-diff"]),
            (
                vec!["grep", "-i", "needle"],
                Command::Grep,
                vec!["-i", "needle"],
            ),
            (vec!["log", "grep"], Command::Log, vec!["grep"]),
            (
                vec!["gh", "search", "code", "--repo", "owner/repo", "needle"],
                Command::GithubSearch,
                vec!["--repo", "owner/repo", "needle"],
            ),
            (vec!["log", "gh"], Command::Log, vec!["gh"]),
        ] {
            let args: Vec<String> = args.into_iter().map(str::to_owned).collect();
            let (actual, rest) = parse_command(&args);
            assert_eq!(actual, show);
            assert_eq!(rest, remaining);
        }
    }

    #[test]
    fn watch_preserves_traversal_arguments_and_pathspecs() {
        let check = |args: &[&str]| {
            let args = args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
            let (_, options, git) = log_format::parse_args(&args)?;
            Ok::<_, String>((options.watch, git))
        };
        assert!(!check(&[]).unwrap().0);
        assert!(!check(&["--hide-types=test", "--hide-merges"]).unwrap().0);
        assert!(
            check(&[
                "--watch",
                "--fold-merges",
                "--hide-types=tests,doc",
                "--hide-merges"
            ])
            .unwrap()
            .0
        );
        assert!(
            check(&["--watch", "--hide-types", "test", "--oneline"])
                .unwrap()
                .0
        );
        assert_eq!(
            check(&["--watch", "--hide-merges", "--all"]).unwrap(),
            (true, vec!["--all".into()])
        );
        assert_eq!(
            check(&["main", "--watch"]).unwrap(),
            (true, vec!["main".into()])
        );
        assert_eq!(
            check(&["--watch", "main", "--", "src"]).unwrap(),
            (true, vec!["main".into(), "--".into(), "src".into()])
        );
        assert_eq!(
            check(&["--", "--watch"]).unwrap(),
            (false, vec!["--".into(), "--watch".into()])
        );
    }

    #[test]
    fn watch_preserves_git_search_pattern_operands() {
        for option in [
            "--grep",
            "--grep-reflog",
            "--author",
            "--committer",
            "-G",
            "-S",
        ] {
            for pattern in ["--watch", "--"] {
                for watch in [false, true] {
                    let mut args = vec![option.to_owned(), pattern.to_owned()];
                    let expected = args.clone();
                    if watch {
                        args.push("--watch".into());
                    }
                    let (_, options, git_args) = log_format::parse_args(&args).unwrap();
                    assert_eq!((options.watch, git_args), (watch, expected), "{args:?}");
                }
            }
        }
    }

    #[test]
    fn startup_filters_select_visible_history_and_can_be_cleared() {
        let mut commits = Vec::new();
        for (i, subject) in ["Merge topic", "tests: coverage", "feat: feature"]
            .iter()
            .enumerate()
        {
            let mut c = log_format::tests::commit();
            c.hash = i.to_string();
            c.subject = (*subject).into();
            c.decorations.clear();
            c.parents = if i == 0 {
                vec!["1".into(), "2".into()]
            } else {
                Vec::new()
            };
            commits.push(c);
        }
        let (_, options, _) =
            log_format::parse_args(&["--hide-merges".into(), "--hide-types=tests".into()]).unwrap();
        let mut app = App::new(commits);
        initialize_log_filters(&mut app, options);
        assert_eq!(app.selected, 2);
        assert_eq!(app.log_folds.hidden_count(), 2);
        assert!(app.status.is_none());
        app.replace_commits(app.commits.clone());
        assert_eq!(app.selected, 2);
        assert_eq!(app.log_folds.hidden_count(), 2);
        app.apply_log_action(LogAction::Reset);
        assert!((0..3).all(|i| app.log_folds.visible(i)));
        let (_, options, _) =
            log_format::parse_args(&["--hide-merges".into(), "--hide-types=test,feature".into()])
                .unwrap();
        initialize_log_filters(&mut app, options);
        assert_eq!(app.log_folds.hidden_count(), 3);
        assert_eq!(app.mode, app::Mode::Log);
        app.apply_log_action(LogAction::Reset);
        assert_eq!(app.log_folds.hidden_count(), 0);
    }
}
