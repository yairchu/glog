mod ansi;
mod app;
mod diff;
mod git;
mod input;
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
    if let Some(information) = parse_information(&args) {
        println!("{information}");
        return ExitCode::SUCCESS;
    }
    let watch = match parse_watch(&args) {
        Ok(watch) => watch,
        Err(error) => {
            eprintln!("glog: {error}");
            return ExitCode::FAILURE;
        }
    };
    let git_args = if watch { &[][..] } else { args.as_slice() };
    let commits = match git::load_log(git_args) {
        Ok(commits) => commits,
        Err(error) => {
            eprintln!("glog: {error}");
            return ExitCode::FAILURE;
        }
    };
    if !should_start_tui(watch, commits.len()) {
        eprintln!("glog: no commits matched");
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
    let mut app = App::new(commits);
    app.watch = watch;
    app.context = args.join(" ");
    let result = run(&mut terminal, &mut app);
    let _ = guard.restore();
    if let Err(error) = result {
        eprintln!("glog: {error}");
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
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
       glog [git log arguments] [--] [pathspec...]

Options:
  --watch       Refresh the default HEAD view when the repository changes
  -h, --help    Print help
  -V, --version Print version

All other arguments are passed through to git log. Inside the TUI, press h for
key help, Tab to switch between Log and Show, and q or Ctrl-C to quit.";

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
    let mut fingerprint = app
        .watch
        .then(git::watch_fingerprint)
        .transpose()
        .map_err(io::Error::other)?;
    while !app.quit {
        terminal.draw(|frame| ui::draw(frame, app))?;
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
        if app.watch && last_watch.elapsed() >= Duration::from_secs(1) {
            match git::watch_fingerprint() {
                Ok(current) if fingerprint != Some(current) => match git::load_log(&[]) {
                    Ok(commits) => {
                        app.replace_commits(commits);
                        fingerprint = Some(current);
                        app.status = None;
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
