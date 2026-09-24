mod ansi;
mod app;
mod diff;
mod git;
mod images;
mod input;
mod log_format;
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
    let (log_format, log_args) = match if command == Command::Log {
        log_format::parse_args(command_args)
    } else {
        Ok((log_format::LogFormat::default(), command_args.to_vec()))
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
        parse_watch(command_args)
    } {
        Ok(watch) => watch,
        Err(error) => {
            eprintln!("glog: {error}");
            return ExitCode::FAILURE;
        }
    };
    let git_args = if watch { &[][..] } else { &log_args };
    let loaded = match command {
        Command::Status => {
            if !command_args.is_empty() {
                Err("usage: glog status".to_owned())
            } else {
                crate::status::StatusView::load().map(|view| {
                    let mut app = App::new(vec![git::working_tree_commit(&view.summary())]);
                    app.status_view = Some(view);
                    app.mode = app::Mode::Status;
                    app.pending_history = Some(Vec::new());
                    app
                })
            }
        }
        Command::Show => git::load_show_app(command_args),
        Command::Diff => git::load_diff_app(command_args),
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
            _ => "no commits matched",
        };
        eprintln!("glog: {message}");
        return ExitCode::SUCCESS;
    }

    let mut terminal = match start_terminal() {
        Ok(terminal) => terminal,
        Err(error) => {
            eprintln!("glog: could not initialize terminal: {error}");
            return ExitCode::FAILURE;
        }
    };
    let mut guard = TerminalGuard(true);
    app.enable_images();
    app.log_format = log_format;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Command {
    Log,
    Show,
    Diff,
    Status,
}

fn parse_command(args: &[String]) -> (Command, &[String]) {
    match args.first().map(String::as_str) {
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

Usage: glog [--watch]
       glog [log] [git log arguments] [--] [pathspec...]
       glog show [--stat] [commit] [-- pathspec...]
       glog diff [--cached] [--stat] [revision [revision]] [[--] pathspec...]
       glog status

Options:
  --pretty=format:FORMAT / --format=FORMAT
                Format Log rows (%h %H %ad %an %ae %d %D %s %%)
  --date=STYLE  Format author dates using Git (e.g. short, relative, iso)
  --oneline     Use the compact hash, refs, and subject layout
  --watch       Include a Working tree item and refresh the default HEAD view
  -h, --help    Print help
  -V, --version Print version

Status opens the Working tree detail view in a watch session.
Watch mode includes one Working tree item; commits open Show.
Log shows committed history, loaded once unless --watch is used.
Show opens HEAD or the specified commit, with history available via Tab.
Diff opens unstaged changes (including untracked files), or staged changes
with --cached. Revisions compare commits (A..B, A...B, A B) or a commit
against the working tree (A); omitted range endpoints default to HEAD.
Diffs open standalone, without a Log tab or adjacent-commit navigation.
Diff exits if empty. Add --stat to Show or Diff to start with
expandable file summaries; press s in Show to toggle summary / patch.
Use glog log show (or glog log diff) to browse a branch named after a command.
Log arguments are passed through to git log. Inside the TUI, press h for
key help, Tab to switch between Log and its detail view, Ctrl-L to redraw the screen, and q
or Ctrl-C to quit.";

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
    while !app.quit {
        if app.redraw {
            app.images.clear(&mut io::stdout())?;
            terminal.clear()?;
            app.redraw = false;
        }
        app.images.poll();
        terminal.draw(|frame| ui::draw(frame, app))?;
        app.images.flush(&mut io::stdout())?;
        if event::poll(Duration::from_millis(250))? {
            input::handle(event::read()?, app);
            // Terminals report a fast trackpad/wheel gesture as a burst of discrete
            // events. Apply the whole queued burst before drawing another frame.
            for _ in 0..1024 {
                if !event::poll(Duration::ZERO)? {
                    break;
                }
                input::handle(event::read()?, app);
                if app.quit {
                    break;
                }
            }
        }
        if app.mode == app::Mode::Status && last_status.elapsed() >= Duration::from_secs(1) {
            if let Some(view) = &mut app.status_view {
                // The view records a failed refresh in its footer.
                let _ = view.refresh();
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
                    }
                    Err(error) => app.status = Some(format!("Watch refresh failed: {error}")),
                },
                Ok(_) => {}
                Err(error) => app.status = Some(format!("Watch refresh failed: {error}")),
            }
            last_watch = Instant::now();
        }
    }
    Ok(())
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

    #[test]
    fn commands_preserve_log_shorthand_and_escape_reserved_names() {
        for (args, show, remaining) in [
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
    fn watch_must_be_the_only_argument() {
        assert!(parse_watch(&[]).is_ok_and(|watch| !watch));
        assert!(parse_watch(&["--watch".to_owned()]).is_ok_and(|watch| watch));
        assert!(parse_watch(&["--watch".to_owned(), "--all".to_owned()]).is_err());
        assert!(parse_watch(&["main".to_owned(), "--watch".to_owned()]).is_err());
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
