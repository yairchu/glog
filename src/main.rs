mod ansi;
mod app;
mod git;
mod input;
mod ui;

use std::{env, io, process::ExitCode, time::Duration};

use app::App;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::Terminal;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let commits = match git::load_log(&args) {
        Ok(commits) => commits,
        Err(error) => {
            eprintln!("glog: {error}");
            return ExitCode::FAILURE;
        }
    };

    let mut terminal = match start_terminal() {
        Ok(terminal) => terminal,
        Err(error) => {
            eprintln!("glog: could not initialize terminal: {error}");
            return ExitCode::FAILURE;
        }
    };
    let mut guard = TerminalGuard(true);
    let mut app = App::new(commits);
    let result = run(&mut terminal, &mut app);
    let _ = guard.restore();
    if let Err(error) = result {
        eprintln!("glog: {error}");
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
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

fn run<B: ratatui::backend::Backend>(terminal: &mut Terminal<B>, app: &mut App) -> io::Result<()> {
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
    }
    Ok(())
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
