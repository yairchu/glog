mod ansi;
mod app;
mod blame;
mod commit_links;
mod diff;
mod editor;
mod git;
mod images;
mod input;
mod log_folds;
mod log_format;
mod message_format;
mod patch_options;
mod search_history;
mod stash;
mod status;
mod ui;

use std::{
    env, io,
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
    let (command, command_args) = parse_command(&args);
    if let Some(information) = parse_information(command_args) {
        println!("{information}");
        return ExitCode::SUCCESS;
    }
    let (color_moved, command_args) = match patch_options::parse_args(command_args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("glog: {error}");
            return ExitCode::FAILURE;
        }
    };
    patch_options::initialize(color_moved);
    let command_args = command_args.as_slice();
    let (log_format, log_options, log_args) = match if command == Command::Log {
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
    let watch = match if command == Command::Status {
        Ok(true)
    } else if command != Command::Log {
        Ok(false)
    } else {
        parse_watch(&log_args)
    } {
        Ok(watch) => watch,
        Err(error) => {
            eprintln!("glog: {error}");
            return ExitCode::FAILURE;
        }
    };
    let git_args = if watch { &[][..] } else { &log_args };
    let loaded = match command {
        Command::Status => status::load_app(command_args),
        Command::Show => git::load_show_app(command_args),
        Command::Diff => git::load_diff_app(command_args),
        Command::Reflog => git::load_reflog(command_args).map(App::new),
        Command::Stash => stash::load_app(command_args),
        Command::Blame => blame::load(command_args).map(|(rows, selected)| {
            let mut app = App::new(rows);
            app.selected = selected;
            app.center_selection = true;
            app
        }),
        Command::Log if watch => git::load_watch_log().map(App::new),
        Command::Log => git::load_log(git_args).map(App::new),
    };
    let mut app = match loaded {
        Ok(app) => app,
        Err(error) => {
            eprintln!("glog: {error}");
            return ExitCode::FAILURE;
        }
    };
    if command != Command::Status && !should_start_tui(watch, app.commits.len()) {
        let message = match command {
            Command::Diff => "no changes",
            Command::Reflog => "no reflog entries matched",
            Command::Blame => "the file is empty",
            _ if app.history == app::History::Blame => "the file is empty",
            Command::Stash => "no stashes matched",
            _ => "no commits matched",
        };
        eprintln!("glog: {message}");
        return ExitCode::SUCCESS;
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
    app.context = args.join(" ");
    let result = run(&mut terminal, &mut app);
    let _ = app.images.clear(&mut io::stdout());
    let _ = guard.restore();
    if let Err(error) = result {
        eprintln!("glog: {error}");
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
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
    Show,
    Diff,
    Status,
}

fn parse_command(args: &[String]) -> (Command, &[String]) {
    match args.first().map(String::as_str) {
        Some("reflog") => (Command::Reflog, &args[1..]),
        Some("blame") => (Command::Blame, &args[1..]),
        Some("stash") => (Command::Stash, &args[1..]),
        Some("status") => (Command::Status, &args[1..]),
        Some("show") => (Command::Show, &args[1..]),
        Some("diff") => (Command::Diff, &args[1..]),
        Some("log") => (Command::Log, &args[1..]),
        _ => (Command::Log, args),
    }
}

fn parse_information(args: &[String]) -> Option<&'static str> {
    match args {
        [flag] if flag == "--help" || flag == "-h" => Some(HELP),
        [flag] if flag == "--version" || flag == "-V" => {
            Some(concat!("glog ", env!("CARGO_PKG_VERSION")))
        }
        _ => None,
    }
}

const HELP: &str = "glog — an interactive git log and git show browser

Usage: glog [--watch] [--fold-merges] [--hide-merges] [--hide-types=TYPES]
            [--exit-on-esc]
       glog [log] [git log arguments] [--] [pathspec...]
       glog reflog [ref] [--all] [-n COUNT] [--date=STYLE]
       glog blame [-L LINE] [--date=STYLE] [revision] [--] file
       glog stash [list [-n COUNT] [--date=STYLE]]
       glog stash show [--stat] [stash]
       glog show [--stat] [commit] [-- pathspec...]
       glog show REV:path
       glog diff [--cached] [--stat] [revision [revision]] [[--] pathspec...]
       glog status

Options:
  --pretty=format:FORMAT / --format=FORMAT
                Format Log rows (%h %H %ad %an %ae %d %D %s %%)
  --date=STYLE  Format Log, Blame, and Stash dates or Reflog update times
                using Git (e.g. short, relative, iso)
  -L LINE       Open Blame at a line
  --oneline     Use the compact hash, refs, and subject layout
  --ascii-graph Use Git-style ASCII paths instead of Unicode lines
  --fold-merges Start Log with merge histories collapsed; z expands a merge
  --hide-merges Hide merge commits; M toggles this filter
  --hide-types=TYPES
                Hide comma-separated commit types (e.g. test,refactor,docs);
                T clears filters
  --watch       Include a Working tree item and refresh the default HEAD view
  --exit-on-esc Exit from the Log list with Escape (glog / glog log)
  --color-moved[=MODE]
                Highlight moved code for this session (bare flag: zebra).
                Modes: no, default, plain, blocks, zebra, dimmed-zebra
  --no-color-moved
                Disable moved-code highlighting for this session
  -h, --help    Print help
  -V, --version Print version

Status opens the Working tree detail view in a watch session.
Watch mode includes one Working tree item; commits open Show.
Log shows committed history, loaded once unless --watch is used.
Graphs use rounded paths, ● for commits, • for empty non-merge commits or a
clean working tree, and ○ for boundary commits. Merge commits stay filled.
╪ crosses paths without joining them.
Reflog shows local ref updates with their update times; Enter opens the commit.
Finished or aborted rebases and amend chains start folded in Reflog;
z toggles a fold.
Stash lists saved work; Enter opens its changes, including saved untracked files.
Stash show opens a stash by index or reference (the latest by default).
Blame shows which commit last changed each line; Enter opens it, p blames
the file before it, and Backspace returns.
Show opens HEAD or the specified commit, with history available via Tab.
Breaking-change markers, section headings, and footer labels are emphasized.
Patches highlight moved blocks using diff.colorMoved, or zebra if unset.
Git's diff.colorMovedWS controls whitespace matching. Session color-moved
flags override diff.colorMoved and apply across views, including watch mode.
Click underlined commit hashes in Show messages to follow them; Backspace returns.
Hashes use lowercase hex: 7+ characters, or with a # prefix, 7+ or 4-5
including a letter. On GitHub repositories, # plus 1-5 digits opens an issue
in the browser.
Paths after -- filter both Show patches and the Log history.
Show REV:path reads the complete text file; x/d/a show the hash/date/author
column and turn it into a blame, loading attribution only then.
Diff opens unstaged changes (including untracked files), or staged changes
with --cached. Revisions compare commits (A..B, A...B, A B) or a commit
against the working tree (A); omitted range endpoints default to HEAD.
Diffs open standalone, without a Log tab or adjacent-commit navigation.
Diff exits if empty. Add --stat to Show or Diff to start with
expandable file summaries; press s in Show to toggle summary / patch.
Use glog log show (or glog log diff) to browse a branch named after a command.
Log arguments are passed through to git log. Inside the TUI, press h for
key help, Tab to switch between Log and its detail view, Ctrl-L to redraw the screen, and q
or Ctrl-C to quit.
Search history persists between sessions; use Up/Down at / or ? to recall it.
GLOG_HISTFILE overrides the history path; set it to - to disable persistence.
The default is $XDG_STATE_HOME/glog/search_history, or
$HOME/.local/state/glog/search_history (Windows: %LOCALAPPDATA%/glog/search_history).";

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

fn run<B: ratatui::backend::Backend>(terminal: &mut Terminal<B>, app: &mut App) -> io::Result<()> {
    let mut last_watch = Instant::now();
    let mut last_status = Instant::now();
    let mut fingerprint = app
        .watch
        .then(git::watch_fingerprint)
        .transpose()
        .map_err(io::Error::other)?;
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
            let result = command.status();
            // Restore the UI even when the editor is missing or exits with an error.
            enable_raw_mode()?;
            execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
            terminal.hide_cursor()?;
            terminal.clear()?;
            match result {
                Ok(status) if status.success() => app.set_editor_error(None),
                Ok(status) => app.set_editor_error(Some(format!("Editor exited with {status}"))),
                Err(error) => app.set_editor_error(Some(format!("Could not open editor: {error}"))),
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
        changed |= app.poll_browser();
        changed |= app.poll_log();
        if changed {
            terminal.draw(|frame| ui::draw(frame, app))?;
            app.images.flush(&mut io::stdout())?;
            changed = false;
        }
        let first = match deferred.take() {
            Some(click) => Some(click),
            None if event::poll(Duration::from_millis(250))? => Some(event::read()?),
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
            match git::watch_fingerprint() {
                Ok(current) if fingerprint != Some(current) => match git::load_watch_log() {
                    Ok(commits) => {
                        app.status = None;
                        app.replace_commits(commits);
                        fingerprint = Some(current);
                        changed = true;
                    }
                    Err(error) => {
                        app.status = Some(format!("Watch refresh failed: {error}"));
                        changed = true;
                    }
                },
                Ok(_) => {}
                Err(error) => {
                    app.status = Some(format!("Watch refresh failed: {error}"));
                    changed = true;
                }
            }
            last_watch = Instant::now();
        }
    }
    Ok(())
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

fn parse_watch(args: &[String]) -> Result<bool, String> {
    match args {
        [flag] if flag == "--watch" => Ok(true),
        _ if args.iter().any(|arg| arg == "--watch") => {
            Err("--watch currently supports only the default HEAD view".to_owned())
        }
        _ => Ok(false),
    }
}

fn should_start_tui(watch: bool, commit_count: usize) -> bool {
    watch || commit_count > 0
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
        ] {
            let args: Vec<String> = args.into_iter().map(str::to_owned).collect();
            let (actual, rest) = parse_command(&args);
            assert_eq!(actual, show);
            assert_eq!(rest, remaining);
        }
    }

    #[test]
    fn watch_limits_traversal_but_allows_display_filters() {
        let check = |args: &[&str]| {
            let args = args.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
            let (_, _, git) = log_format::parse_args(&args)?;
            parse_watch(&git)
        };
        assert!(!check(&[]).unwrap());
        assert!(!check(&["--hide-types=test", "--hide-merges"]).unwrap());
        assert!(check(&[
            "--watch",
            "--fold-merges",
            "--hide-types=tests,doc",
            "--hide-merges"
        ])
        .unwrap());
        assert!(check(&["--watch", "--hide-types", "test", "--oneline"]).unwrap());
        assert!(check(&["--watch", "--hide-merges", "--all"]).is_err());
        assert!(check(&["main", "--watch"]).is_err());
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

    #[test]
    fn empty_results_skip_tui_except_when_watching() {
        assert!(!should_start_tui(false, 0));
        assert!(should_start_tui(false, 1));
        assert!(should_start_tui(true, 0));
    }

    #[test]
    fn recognizes_only_standalone_information_flags() {
        assert_eq!(parse_information(&["--help".to_owned()]), Some(HELP));
        assert!(parse_information(&["--version".to_owned()])
            .is_some_and(|version| version.starts_with("glog ")));
        assert_eq!(parse_information(&["--all".to_owned()]), None);
        assert_eq!(
            parse_information(&["--help".to_owned(), "main".to_owned()]),
            None
        );
    }
}
