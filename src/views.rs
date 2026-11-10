//! Session-level views keep their own navigation and followed-reference stacks.
use std::collections::HashMap;

use crate::app::{App, Mode, WatchSource};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) enum View {
    #[default]
    StatusLog,
    Branches,
    Stashes,
    Reflog,
    Original,
}

#[derive(Default)]
pub(crate) struct Session {
    pub active: View,
    pub saved: HashMap<View, Box<App>>,
    pub original_label: Option<&'static str>,
}

impl Session {
    pub fn options(&self) -> Vec<View> {
        let mut options = vec![View::StatusLog, View::Branches, View::Stashes, View::Reflog];
        if self.original_label.is_some() {
            options.push(View::Original);
        }
        options
    }

    pub fn label(&self, view: View) -> &'static str {
        match view {
            View::StatusLog => "Status and Log",
            View::Branches => "Branches",
            View::Stashes => "Stashes",
            View::Reflog => "Reflog",
            View::Original => self.original_label.unwrap_or("Original view"),
        }
    }
}

impl View {
    pub fn load(self) -> Result<App, String> {
        let mut app = match self {
            Self::StatusLog => {
                let source = WatchSource::log(Vec::new());
                let snapshot = source.snapshot(None);
                let mut app = crate::status::load_app(&[])?;
                app.mode = Mode::Status;
                app.watch = true;
                app.set_watch_source(source, Some(snapshot));
                app
            }
            Self::Branches => crate::branches::load_picker()?,
            Self::Stashes => crate::stash::load_app(&[])?,
            Self::Reflog => {
                let mut app = App::new(crate::git::load_reflog(&[])?);
                app.history = crate::app::History::Reflog;
                app
            }
            Self::Original => return Err("Original view is no longer available".into()),
        };
        app.context = match self {
            Self::StatusLog => "status",
            Self::Branches => "branch",
            Self::Stashes => "stash",
            Self::Reflog => "reflog",
            Self::Original => unreachable!(),
        }
        .into();
        Ok(app)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::History,
        git::tests::{git, CurrentDirGuard, TestDirectory},
        input, ui,
    };
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{backend::TestBackend, Terminal};
    use std::{fs, path::Path};

    fn press(app: &mut App, code: KeyCode) {
        input::handle(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)), app);
    }

    fn switch(app: &mut App, view: View) {
        press(app, KeyCode::Char('V'));
        let index = app.views.options().iter().position(|v| *v == view).unwrap();
        while app.view_menu != Some(index) {
            press(
                app,
                if app.view_menu.unwrap() < index {
                    KeyCode::Down
                } else {
                    KeyCode::Up
                },
            );
        }
        press(app, KeyCode::Enter);
        assert_eq!(app.views.active, view);
        assert_eq!(app.view_menu, None);
    }

    fn repository() {
        git(&["init", "-q"]);
        git(&["config", "user.name", "Test"]);
        git(&["config", "user.email", "test@example.com"]);
        fs::write("file.txt", "base\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "base"]);
    }

    #[test]
    fn switching_preserves_status_and_followed_branch_navigation() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        repository();
        fs::write("file.txt", "changed\n").unwrap();
        let mut app = View::StatusLog.load().unwrap();
        app.views.active = View::StatusLog;
        app.status_view.as_mut().unwrap().cursor = 1;
        app.status_view.as_mut().unwrap().horizontal = 8;

        // Internal branch browsing must work even for a single current branch.
        switch(&mut app, View::Branches);
        assert_eq!(app.history, History::Branches);
        assert!(!app.watch);
        let picker = crate::branches::summary(&app);
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.history, History::Log);
        assert!(app.has_reference_back());
        app.log_offset = 3;
        app.search = Some("base".into());
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.mode, Mode::Show);
        app.show_cursor = 2;
        app.show_offset = 1;

        switch(&mut app, View::Stashes);
        assert!(app.commits.is_empty());
        assert_eq!(app.history, History::Stash);
        assert_eq!(app.terminal_title(Path::new("/work/repo")), "Stash (repo)");
        assert_eq!(
            crate::branches::summary(app.branch_picker().unwrap()),
            picker
        );

        switch(&mut app, View::Reflog);
        assert_eq!(app.history, History::Reflog);
        app.bottom();
        let reflog_selected = app.selected;
        app.log_offset = 2;

        switch(&mut app, View::Branches);
        assert_eq!(app.mode, Mode::Show);
        assert_eq!(app.show_cursor, 2);
        assert_eq!(app.show_offset, 1);
        assert_eq!(app.log_offset, 3);
        assert_eq!(app.search.as_deref(), Some("base"));
        press(&mut app, KeyCode::Backspace);
        assert_eq!(app.history, History::Branches);

        switch(&mut app, View::StatusLog);
        assert_eq!(app.mode, Mode::Status);
        assert_eq!(app.status_view.as_ref().unwrap().cursor, 1);
        assert_eq!(app.status_view.as_ref().unwrap().horizontal, 8);
        assert!(app.watch);
        assert!(app.watch_source.is_some());
        assert_eq!(app.views.saved.len(), 3);
        switch(&mut app, View::Reflog);
        assert_eq!(app.history, History::Reflog);
        assert_eq!(app.selected, reflog_selected);
        assert_eq!(app.log_offset, 2);
    }

    #[test]
    fn original_diff_and_status_log_keep_separate_return_paths() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        repository();
        fs::write("file.txt", "changed\n").unwrap();
        let mut app = crate::git::load_diff_app(&[]).unwrap();
        app.views.active = View::Original;
        app.views.original_label = Some("Diff");
        app.show_cursor = 2;
        switch(&mut app, View::StatusLog);
        app.follow_reference("HEAD");
        assert!(app.has_reference_back());
        switch(&mut app, View::Original);
        assert_eq!(app.mode, Mode::Show);
        assert_eq!(app.show_cursor, 2);
        assert!(!app.has_reference_back());
        assert_eq!(app.terminal_title(Path::new("/work/repo")), "Diff (repo)");
        switch(&mut app, View::StatusLog);
        assert!(app.has_reference_back());
        press(&mut app, KeyCode::Backspace);
        assert!(!app.has_reference_back());
        assert_eq!(app.views.active, View::StatusLog);
    }

    #[test]
    fn menu_shields_navigation_and_mouse_and_renders_on_small_screens() {
        let mut app = App::new(Vec::new());
        app.views.active = View::StatusLog;
        app.mode = Mode::Status;
        press(&mut app, KeyCode::Char('V'));
        press(&mut app, KeyCode::Tab);
        assert_eq!(app.mode, Mode::Status);
        press(&mut app, KeyCode::Down);
        assert_eq!(app.view_menu, Some(1));
        input::handle(
            Event::Mouse(crossterm::event::MouseEvent {
                kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: 8,
                row: 0,
                modifiers: KeyModifiers::NONE,
            }),
            &mut app,
        );
        assert_eq!(app.mode, Mode::Status);
        assert_eq!(app.view_menu, Some(1));
        let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        for label in [
            "Views",
            "Status and Log",
            "Branches",
            "Stashes",
            "Reflog",
            "Esc cancel",
        ] {
            assert!(screen.contains(label), "{screen}");
        }
        let mut tiny = Terminal::new(TestBackend::new(8, 3)).unwrap();
        tiny.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.view_menu, None);
        assert!(!app.quit);
        app.search_input = Some(String::new());
        press(&mut app, KeyCode::Char('V'));
        assert_eq!(app.search_input.as_deref(), Some("V"));
        assert_eq!(app.view_menu, None);
    }

    #[test]
    fn a_failed_switch_keeps_the_current_view() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "--bare", "-q"]);
        let mut app = App::new(Vec::new());
        app.views.active = View::Original;
        app.views.original_label = Some("Diff");
        press(&mut app, KeyCode::Char('V'));
        for _ in 0..app.views.options().len() {
            press(&mut app, KeyCode::Up);
        }
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.views.active, View::Original);
        assert_eq!(app.view_menu, None);
        assert!(app.views.saved.is_empty());
        assert!(app.status.is_some());
        assert!(!app.quit);
    }
}
