//! Scope terminal titles to the interactive session, including editor handoffs.
use std::{
    io::{self, Write},
    path::PathBuf,
};

pub(crate) struct Title {
    repository: PathBuf,
    enabled: bool,
    current: Option<String>,
}

impl Title {
    pub(crate) fn new() -> Self {
        Self {
            repository: crate::git::Repository::main()
                .map(|repo| repo.directory().to_owned())
                .or_else(|_| std::env::current_dir())
                .unwrap_or_default(),
            enabled: std::env::var_os("GLOG_TITLE").is_none_or(|value| value != "0"),
            current: None,
        }
    }

    pub(crate) fn update(&mut self, app: &crate::app::App) -> io::Result<()> {
        if !self.enabled {
            return Ok(());
        }
        let title = app.terminal_title(&self.repository);
        self.write_title(&mut io::stdout(), title)
    }

    fn write_title(&mut self, output: &mut impl Write, title: String) -> io::Result<()> {
        if self.current.as_ref() == Some(&title) {
            return Ok(());
        }
        if self.current.is_none() {
            // Save the window title without querying it through terminal input.
            output.write_all(b"\x1b[22;2t")?;
            self.current = Some(String::new());
        }
        write!(output, "\x1b]2;{title}\x07")?;
        output.flush()?;
        self.current = Some(title);
        Ok(())
    }

    pub(crate) fn restore(&mut self) -> io::Result<()> {
        self.restore_to(&mut io::stdout())
    }

    fn restore_to(&mut self, output: &mut impl Write) -> io::Result<()> {
        if self.current.is_some() {
            output.write_all(b"\x1b[23;2t")?;
            output.flush()?;
            self.current = None;
        }
        Ok(())
    }
}

impl Drop for Title {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_titles_follow_watch_state_and_repository_without_controls() {
        use crate::app::{App, Mode, WatchSource};
        use std::path::Path;
        let mut app = App::new(Vec::new());
        let root = Path::new("/work/repo");
        app.watch_source = Some(WatchSource::log(Vec::new()));
        assert_eq!(app.terminal_title(root), "Log (repo)");
        app.watch = true;
        assert_eq!(app.terminal_title(root), "Status (repo)");
        app.mode = Mode::Show;
        assert_eq!(app.terminal_title(root), "Status (repo)");
        app.watch = false;
        app.title_revision = Some("feature/topic".into());
        assert_eq!(app.terminal_title(root), "Log feature/topic (repo)");
        app.mode = Mode::Status;
        assert_eq!(app.terminal_title(root), "Status (repo)");
        app.set_repository(Some(crate::git::Repository::checked_out(
            "/work/sub\x1b\x07\n\u{009c}".into(),
        )));
        assert_eq!(app.terminal_title(root), "Status (sub)");
    }

    #[test]
    fn title_stack_stays_balanced_across_updates_and_editor_handoffs() {
        let mut title = Title {
            repository: PathBuf::new(),
            enabled: true,
            current: None,
        };
        let mut output = Vec::new();
        title.write_title(&mut output, "Log (repo)".into()).unwrap();
        let first = output.clone();
        title.write_title(&mut output, "Log (repo)".into()).unwrap();
        assert_eq!(output, first);
        title
            .write_title(&mut output, "Status (repo)".into())
            .unwrap();
        title.restore_to(&mut output).unwrap();
        let restored = output.clone();
        title.restore_to(&mut output).unwrap();
        assert_eq!(output, restored);
        title
            .write_title(&mut output, "Status (repo)".into())
            .unwrap();
        title.restore_to(&mut output).unwrap();
        let output = String::from_utf8(output).unwrap();
        assert_eq!(output.matches("\x1b]2;").count(), 3);
        assert_eq!(output.matches("\x1b[22;2t").count(), 2);
        assert_eq!(output.matches("\x1b[23;2t").count(), 2);
    }
}
