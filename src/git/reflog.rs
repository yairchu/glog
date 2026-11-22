//! Publish the newest events from the same Git stream used for the full view.
use std::io::{self, BufRead, BufReader, Read};
use std::sync::mpsc::{self, Receiver};

use super::{Cancel, Commit};
use crate::app::{App, History, WatchSnapshot, WatchSource};
use crate::log_folds::PreparedReflog;

const RECENT: usize = 128;
pub(super) const FIELDS: usize = 9;

pub(crate) struct Page {
    pub commits: Vec<Commit>,
    pub folds: PreparedReflog,
}

impl Page {
    fn new(commits: Vec<Commit>, recent: bool) -> Self {
        let folds = PreparedReflog::new(&commits, recent);
        Self { commits, folds }
    }
}

pub(crate) enum Update {
    Recent(Page),
    Complete(Result<Page, String>),
}

pub(crate) struct PendingReflog {
    pub result: Receiver<(Option<WatchSnapshot>, Update)>,
    cancel: Cancel,
}

impl Drop for PendingReflog {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

pub(crate) fn start_reflog_app(args: &[String]) -> App {
    let (watch, args) = crate::app::watch_args(args);
    let source = WatchSource::Reflog(args.clone());
    let (sender, result) = mpsc::channel();
    let cancel = Cancel::default();
    let mut app = App::new(Vec::new());
    app.history = History::Reflog;
    app.pending_reflog = Some(PendingReflog {
        result,
        cancel: cancel.clone(),
    });
    app.watch = watch;
    app.set_watch_source(source, None);
    std::thread::spawn(move || {
        let mut snapshot = watch.then(super::reflog_fingerprint);
        let rows = super::read_reflog(
            &args,
            &cancel,
            Some(|rows| {
                let _ = sender.send((snapshot.take(), Update::Recent(Page::new(rows, true))));
            }),
        );
        if !cancel.is_cancelled() {
            let page = rows.map(|rows| Page::new(rows, false));
            let _ = sender.send((snapshot, Update::Complete(page)));
        }
    });
    app
}

/// Menu switches validate the destination before replacing the current view.
pub(crate) fn load_reflog_app(args: &[String]) -> Result<App, String> {
    let mut app = start_reflog_app(args);
    let (snapshot, update) = app
        .pending_reflog
        .as_ref()
        .unwrap()
        .result
        .recv()
        .map_err(|_| "reflog reader stopped")?;
    app.apply_reflog(snapshot, update)?;
    Ok(app)
}

/// Pass the prefix to the view only if a longer read follows. Keep every byte
/// in the full snapshot, including read-ahead buffered past the last field.
pub(super) fn read_with_prefix(
    stdout: impl Read,
    output: &mut Vec<u8>,
    prefix: impl FnOnce(&[u8]),
) -> io::Result<usize> {
    let mut reader = BufReader::new(stdout);
    for _ in 0..RECENT * FIELDS {
        if reader.read_until(0, output)? == 0 {
            return Ok(output.len());
        }
    }
    if !reader.fill_buf()?.is_empty() {
        prefix(output);
    }
    reader.read_to_end(output)?;
    Ok(output.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{LogAction, Mode};
    use crate::git::tests::{git, git_command, CurrentDirGuard, TestDirectory};
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use std::time::{Duration, Instant};

    #[test]
    fn completing_reflog_preserves_the_open_visit_and_expanded_fold() {
        let dir = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(dir.path());
        git(&["init", "-q"]);
        git(&["commit", "--allow-empty", "-qm", "base"]);
        let reset = || {
            assert!(git_command(&["reset", "--soft", "HEAD"])
                .env("GIT_COMMITTER_DATE", "2020-01-02T00:00:00+00:00")
                .status()
                .unwrap()
                .success());
        };
        for _ in 0..RECENT + 2 {
            reset();
        }
        for limit in [None, Some("-n3")] {
            let mut args = vec!["--watch".to_owned()];
            args.extend(limit.map(str::to_owned));
            let mut app = start_reflog_app(&args);
            app.switch_mode();
            assert_eq!(app.mode, Mode::Log);
            let first = app.pending_reflog.as_ref().unwrap().result.recv().unwrap();
            app.apply_reflog(first.0, first.1).unwrap();
            assert_eq!(app.commits.len(), limit.map_or(RECENT, |_| 3));
            app.apply_log_action(LogAction::Fold);
            app.move_by(1, 10);
            assert_eq!(app.selected, 1);
            crate::input::handle(
                Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
                &mut app,
            );
            assert_eq!(app.mode, Mode::Show);
            let text = app.show_text.clone();
            let now = Instant::now();
            app.poll_activity(now);
            app.poll_activity(now + Duration::from_millis(250));
            assert!(app.activity.text().unwrap().contains("Loading full reflog"));
            let (snapshot, update) = app
                .pending_reflog
                .as_ref()
                .unwrap()
                .result
                .recv_timeout(Duration::from_secs(10))
                .unwrap();
            // A newer identical visit arrives before the completed read is installed.
            reset();
            assert!(!app.poll_watch());
            app.apply_reflog(snapshot, update).unwrap();
            assert_eq!(app.commits.len(), limit.map_or(RECENT + 3, |_| 3));
            assert_eq!(app.selected, 1);
            assert!(app.log_folds.visible(0));
            assert!(app.log_folds.visible(2));
            assert_eq!(app.mode, Mode::Show);
            assert_eq!(app.show_text, text);
            app.poll_activity(now + Duration::from_secs(1));
            assert!(app.activity.text().is_none());
            assert!(app.poll_watch());
            assert_eq!(app.selected, 2);
        }
    }
}
