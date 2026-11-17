//! Compare independently resolved local and tracking-branch commit series.
use std::{
    collections::HashMap,
    sync::mpsc::{self, Receiver},
};

use super::{Branch, BranchKind};
use crate::{
    git::{self, Commit, Repository},
    range_diff,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Counterpart {
    pub name: String,
    pub hash: String,
    pub linear: bool,
}

// Resolve availability from the same pinned ancestry as the picker, including
// counterparts that are not themselves displayed as branch tips.
pub(super) fn mark_linear_counterparts(
    graph: &crate::compact_graph::Graph,
    tips: &mut HashMap<String, Vec<Branch>>,
) {
    for (hash, branches) in tips {
        for remote in branches
            .iter_mut()
            .filter_map(|branch| branch.counterpart.as_mut())
        {
            let (Some(&local), Some(&other)) =
                (graph.indices.get(hash), graph.indices.get(&remote.hash))
            else {
                continue;
            };
            // Topological order puts descendants before their ancestors.
            let (start, target) = (local.min(other), local.max(other));
            let mut todo = vec![start];
            let mut seen = std::collections::HashSet::new();
            remote.linear = false;
            while let Some(index) = todo.pop() {
                if index == target {
                    remote.linear = true;
                    break;
                }
                if index < target && seen.insert(index) {
                    todo.extend(graph.nodes[index].parents.iter().copied());
                }
            }
        }
    }
}

struct Range {
    name: String,
    base: String,
    tip: String,
}

impl Range {
    fn argument(&self) -> String {
        format!("{}..{}", self.base, self.tip)
    }
    fn label(&self) -> String {
        format!(
            "{}: {}..{}",
            self.name,
            &self.base[..12.min(self.base.len())],
            &self.tip[..12.min(self.tip.len())]
        )
    }
}

struct Request {
    local: Range,
    remote: Counterpart,
    branch: Branch,
    tips: HashMap<String, Vec<Branch>>,
    primary: Option<String>,
}

fn range(node: &Commit, rows: &[Commit], name: String) -> Result<Range, String> {
    let bases = super::log_bases(node, rows);
    if bases.len() != 1 {
        return Err(format!(
            "Cannot resolve a single branching point for {name}"
        ));
    }
    Ok(Range {
        name,
        base: bases.into_iter().next().unwrap(),
        tip: node.hash.clone(),
    })
}

impl Request {
    fn new(node: &Commit, branch: &Branch, rows: &[Commit]) -> Result<Self, String> {
        branch.remote_diff_name(&node.hash)?;
        let remote = branch
            .counterpart
            .clone()
            .ok_or_else(|| format!("No stored remote counterpart for {}", branch.name))?;
        let local = range(node, rows, branch.name.clone())?;
        let mut tips: HashMap<String, Vec<Branch>> = rows
            .iter()
            .filter_map(|row| {
                let branches = &row.branch()?.branches;
                (!branches.is_empty()).then(|| (row.hash.clone(), branches.clone()))
            })
            .collect();
        let remote_default = tips
            .values()
            .flatten()
            .find(|branch| branch.kind == BranchKind::Remote)
            .map(|branch| branch.name.clone());
        let primary = super::default_branch(&tips, remote_default.as_deref());
        for branches in tips.values_mut() {
            branches.retain(|candidate| !candidate.same_ref(branch));
            for branch in branches {
                branch.merged_into = None;
            }
        }
        tips.retain(|_, branches| !branches.is_empty());
        let mut replacement = branch.clone();
        replacement.current = false;
        replacement.merged_into = None;
        replacement.counterpart = None;
        tips.entry(remote.hash.clone())
            .or_default()
            .push(replacement);
        Ok(Self {
            local,
            remote,
            branch: branch.clone(),
            tips,
            primary,
        })
    }

    fn remote_range(&self, repository: &Repository, cancel: &git::Cancel) -> Result<Range, String> {
        let ancestry = super::read_ancestry_at(
            self.tips.keys().cloned().collect(),
            Some(repository),
            cancel,
        )?;
        let rows = super::compact(&ancestry, self.tips.clone(), self.primary.as_deref())?;
        let node = rows
            .iter()
            .find(|row| row.hash == self.remote.hash)
            .ok_or("Remote tip is missing from the branch graph")?;
        range(node, &rows, self.remote.name.clone())
    }

    fn load(self, repository: &Repository, cancel: &git::Cancel) -> Result<Loaded, String> {
        let remote = self.remote_range(repository, cancel)?;
        let mut view = range_diff::load_view(
            &[remote.argument(), self.local.argument()],
            repository,
            cancel,
        )?;
        view.set_heading(format!(
            "Remote {}\nLocal  {}",
            remote.label(),
            self.local.label()
        ));
        Ok(Loaded {
            view,
            context: format!("range-diff {} ↔ {}", remote.name, self.branch.name),
        })
    }
}

pub(crate) struct Loaded {
    pub view: range_diff::View,
    pub context: String,
}

pub(crate) struct Pending {
    pub receiver: Receiver<Result<Loaded, String>>,
    cancel: git::Cancel,
}

impl Pending {
    pub(crate) fn start(
        node: &Commit,
        branch: &Branch,
        rows: &[Commit],
        repository: Repository,
    ) -> Result<Self, String> {
        let request = Request::new(node, branch, rows)?;
        let (sender, receiver) = mpsc::channel();
        let cancel = git::Cancel::default();
        let worker_cancel = cancel.clone();
        std::thread::spawn(move || {
            let _ = sender.send(request.load(&repository, &worker_cancel));
        });
        Ok(Self { receiver, cancel })
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::{App, History, Mode},
        git::tests::{git, CurrentDirGuard, TestDirectory},
        input, ui,
    };
    use crossterm::event::{
        Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use ratatui::{backend::TestBackend, Terminal};
    use std::{
        fs,
        time::{Duration, Instant},
    };

    #[test]
    fn a_distinct_push_repository_does_not_use_the_fetch_tracking_ref() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        init();
        commit("base.txt", "base");
        git(&["checkout", "-qb", "topic"]);
        commit("local.txt", "local");
        git(&["checkout", "-qb", "temporary", "main"]);
        let fetch_tip = commit("upstream.txt", "fetch repository only");
        git(&["checkout", "-q", "topic"]);
        git(&["branch", "-D", "temporary"]);
        git(&["config", "push.default", "current"]);
        git(&[
            "remote",
            "set-url",
            "origin",
            "https://github.com/upstream/repo.git",
        ]);
        git(&[
            "remote",
            "set-url",
            "--push",
            "origin",
            "git@github.com:fork/repo.git",
        ]);
        git(&["update-ref", "refs/remotes/origin/topic", &fetch_tip]);

        let mut app = super::super::load_picker().unwrap();
        select(&mut app, "topic");
        assert!(
            app.selected_branch_for_diff().is_err(),
            "upstream's fetched topic does not identify the fork's pushed topic"
        );
        let mut terminal = Terminal::new(TestBackend::new(120, 10)).unwrap();
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        assert!(app.branch_diff_button.is_none());
        press(&mut app, KeyCode::Char('r'));
        assert_eq!(app.history, History::Branches);
        assert!(app
            .status
            .as_deref()
            .unwrap()
            .contains("No stored remote counterpart"));
    }

    fn init() {
        git(&["init", "-qb", "main"]);
        git(&["config", "user.name", "Test"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&[
            "remote",
            "add",
            "origin",
            "https://example.invalid/repo.git",
        ]);
    }

    fn commit(file: &str, text: &str) -> String {
        fs::write(file, text).unwrap();
        git(&["add", "--", file]);
        git(&["commit", "-qm", text]);
        git(&["rev-parse", "HEAD"])
    }

    fn select(app: &mut App, name: &str) {
        app.selected = app
            .commits
            .iter()
            .position(|row| {
                row.branch()
                    .is_some_and(|node| node.branches.iter().any(|branch| branch.name == name))
            })
            .unwrap();
    }

    fn request(app: &App) -> Request {
        Request::new(
            &app.commits[app.selected],
            app.selected_branch_for_diff().unwrap(),
            &app.commits,
        )
        .unwrap()
    }

    fn press(app: &mut App, key: KeyCode) {
        input::handle(Event::Key(KeyEvent::new(key, KeyModifiers::NONE)), app);
    }

    fn wait(app: &mut App) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !app.poll_branch_diff() {
            assert!(
                Instant::now() < deadline,
                "range-diff stalled: {:?}",
                app.status
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(app.mode, Mode::RangeDiff, "{:?}", app.status);
    }

    #[test]
    fn custom_fetch_namespaces_keep_remote_series_available() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        init();
        let base = commit("base.txt", "base");
        git(&["checkout", "-qb", "topic"]);
        let local = commit("local.txt", "local");
        git(&["checkout", "-qb", "temporary", "main"]);
        let remote = commit("remote.txt", "remote");
        git(&["checkout", "-q", "topic"]);
        git(&["branch", "-D", "temporary"]);
        git(&["config", "push.default", "current"]);
        git(&[
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/vendor/*",
        ]);
        git(&["update-ref", "refs/vendor/topic", &remote]);
        assert_eq!(
            git(&["for-each-ref", "--format=%(push)", "refs/heads/topic"]),
            "refs/vendor/topic"
        );
        let mut app = super::super::load_picker().unwrap();
        select(&mut app, "topic");
        let plan = request(&app);
        assert_eq!(plan.local.base, base);
        assert_eq!(plan.local.tip, local);
        let other = plan
            .remote_range(&Repository::main().unwrap(), &git::Cancel::default())
            .unwrap();
        assert_eq!(other.name, "refs/vendor/topic");
        assert_eq!(other.base, base);
        assert_eq!(other.tip, remote);
        press(&mut app, KeyCode::Char('r'));
        wait(&mut app);
        assert!(app.context.contains("refs/vendor/topic"));
    }

    #[test]
    fn remote_rebases_use_independent_bases_and_button_and_shortcut_return_to_branches() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        init();
        let base = commit("base.txt", "base");
        git(&["checkout", "-qb", "topic"]);
        let local = commit("space 界.txt", "feature");
        git(&["checkout", "-q", "main"]);
        let remote_base = commit("advance.txt", "advance");
        git(&["update-ref", "refs/remotes/origin/main", &remote_base]);
        git(&["checkout", "-qb", "temporary"]);
        git(&["cherry-pick", &local]);
        let remote = git(&["rev-parse", "HEAD"]);
        git(&["update-ref", "refs/remotes/origin/topic", &remote]);
        git(&["checkout", "-q", "topic"]);
        git(&["branch", "-D", "temporary"]);
        git(&["config", "branch.topic.remote", "origin"]);
        git(&["config", "branch.topic.merge", "refs/heads/topic"]);
        let mut app = super::super::load_picker().unwrap();
        select(&mut app, "topic");
        let selected = app.selected;
        let plan = request(&app);
        assert_eq!(plan.local.base, base);
        assert_eq!(plan.local.tip, local);
        let other = plan
            .remote_range(&Repository::main().unwrap(), &git::Cancel::default())
            .unwrap();
        assert_eq!(other.base, remote_base);
        assert_eq!(other.tip, remote);
        assert_eq!(other.name, "origin/topic");
        // The action uses the displayed snapshot even if refs move meanwhile.
        git(&["update-ref", "refs/remotes/origin/topic", &local]);
        press(&mut app, KeyCode::Char('r'));
        let now = Instant::now();
        app.poll_activity(now);
        app.poll_activity(now + Duration::from_millis(200));
        assert!(app.activity.text().unwrap().contains("Loading range-diff…"));
        wait(&mut app);
        let mut terminal = Terminal::new(TestBackend::new(130, 16)).unwrap();
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(screen.contains(&format!(
            "Remote origin/topic: {}..{}",
            &remote_base[..12],
            &remote[..12]
        )));
        assert!(screen.contains(&format!("Local  topic: {}..{}", &base[..12], &local[..12])));
        assert!(screen.contains('='));
        press(&mut app, KeyCode::Char('G'));
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        let first_row: String = (0..130)
            .map(|x| terminal.backend().buffer()[(x, 1)].symbol())
            .collect();
        assert!(
            first_row.contains("Remote origin/topic"),
            "comparison ranges should remain visible while scrolling"
        );
        terminal.backend_mut().resize(20, 4);
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        terminal.backend_mut().resize(130, 16);
        press(&mut app, KeyCode::Backspace);
        assert_eq!(app.history, History::Branches);
        assert_eq!(app.selected, selected);
        assert!(app.status.is_none());
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        let button = app.branch_diff_button.expect("remote range-diff button");
        input::handle(
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: button.x,
                row: button.y,
                modifiers: KeyModifiers::NONE,
            }),
            &mut app,
        );
        wait(&mut app);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.history, History::Branches);
        assert_eq!(app.selected, selected);
        press(&mut app, KeyCode::Char('r'));
        press(&mut app, KeyCode::Esc);
        assert!(
            !app.poll_branch_diff(),
            "cancelled requests must not replace the picker"
        );
        assert!(app.status.is_none());
        assert_eq!(git(&["rev-parse", "HEAD"]), local);
    }

    #[test]
    fn ahead_and_behind_counterparts_hide_and_reject_the_action() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        init();
        commit("base.txt", "base");
        git(&["checkout", "-qb", "topic"]);
        let first = commit("first.txt", "first");
        let second = commit("second.txt", "second");
        git(&["checkout", "-q", "main"]);
        for (local, remote) in [(&first, &second), (&second, &first)] {
            git(&["update-ref", "refs/heads/topic", local]);
            git(&["update-ref", "refs/remotes/origin/topic", remote]);
            let mut app = super::super::load_picker().unwrap();
            select(&mut app, "topic");
            let mut terminal = Terminal::new(TestBackend::new(130, 12)).unwrap();
            terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
            assert!(app.branch_diff_button.is_none());
            let screen: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|cell| cell.symbol())
                .collect();
            assert!(!screen.contains("remote range-diff"));
            press(&mut app, KeyCode::Char('r'));
            assert!(app
                .status
                .as_deref()
                .unwrap()
                .contains("no diverged history"));
            assert!(!app.poll_branch_diff());
            assert_eq!(app.history, History::Branches);
        }
    }

    #[test]
    fn matching_refs_hide_the_action_and_watch_refreshes_update_its_availability() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        init();
        commit("base.txt", "base");
        git(&["checkout", "-qb", "topic"]);
        let first = commit("first.txt", "first");
        let second = commit("second.txt", "second");
        git(&["checkout", "-qb", "old-topic", &first]);
        let remote = commit("remote.txt", "remote");
        git(&["checkout", "-q", "topic"]);
        git(&["branch", "-D", "old-topic"]);
        git(&["update-ref", "refs/remotes/origin/topic", &second]);
        let mut app = super::super::load_picker().unwrap();
        select(&mut app, "topic");
        let mut terminal = Terminal::new(TestBackend::new(130, 12)).unwrap();
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        assert!(app.branch_diff_button.is_none());
        let screen: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(
            !screen.contains("remote range-diff"),
            "matching refs must not advertise the shortcut"
        );
        press(&mut app, KeyCode::Char('r'));
        assert_eq!(
            app.status.as_deref(),
            Some("topic already matches origin/topic")
        );
        assert!(
            !app.poll_branch_diff(),
            "matching refs must not start a comparison"
        );
        assert_eq!(app.history, History::Branches);

        git(&["update-ref", "refs/remotes/origin/topic", &remote]);
        app.commits = super::super::load_watched(&[]).unwrap();
        app.status = None;
        select(&mut app, "topic");
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        assert!(app.branch_diff_button.is_some());
        let screen: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(screen.contains("r remote range-diff"));
        press(&mut app, KeyCode::Char('r'));
        wait(&mut app);
        press(&mut app, KeyCode::Esc);

        git(&["update-ref", "refs/remotes/origin/topic", &second]);
        app.commits = super::super::load_watched(&[]).unwrap();
        select(&mut app, "topic");
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        assert!(
            app.branch_diff_button.is_none(),
            "the old button hit target must be cleared"
        );
    }

    #[test]
    fn unresolved_upstream_fetch_mappings_do_not_fall_back_to_origin() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        init();
        commit("base.txt", "base");
        git(&["checkout", "-qb", "topic"]);
        let tip = commit("topic.txt", "topic");
        git(&["checkout", "-q", "main"]);
        let remote = commit("remote.txt", "remote");
        git(&["checkout", "-q", "topic"]);
        git(&["update-ref", "refs/remotes/origin/topic", &remote]);
        git(&[
            "remote",
            "add",
            "upstream",
            "https://example.invalid/upstream.git",
        ]);
        git(&["config", "branch.topic.remote", "upstream"]);
        git(&["config", "branch.topic.merge", "refs/heads/topic"]);
        // A single-branch fetch mapping cannot resolve topic's upstream,
        // even when a remote-tracking ref remains from an earlier fetch.
        git(&[
            "config",
            "remote.upstream.fetch",
            "+refs/heads/main:refs/remotes/upstream/main",
        ]);
        git(&["update-ref", "refs/remotes/upstream/topic", &tip]);
        let mut app = super::super::load_picker().unwrap();
        select(&mut app, "topic");
        assert!(app.selected_branch_for_diff().is_err());
        press(&mut app, KeyCode::Char('r'));
        assert!(app
            .status
            .as_deref()
            .unwrap()
            .contains("No stored remote counterpart"));
        assert_eq!(app.history, History::Branches);
        assert!(!app.poll_branch_diff());

        // Once the mapping is restored, honor the configured upstream.
        git(&[
            "config",
            "remote.upstream.fetch",
            "+refs/heads/*:refs/remotes/upstream/*",
        ]);
        git(&["update-ref", "refs/remotes/upstream/topic", &remote]);
        app.commits = super::super::load_watched(&[]).unwrap();
        select(&mut app, "topic");
        assert_eq!(
            app.selected_branch_for_diff().unwrap().remote_name(),
            Some("upstream/topic")
        );

        // Removing the upstream configuration permits the origin fallback.
        git(&["config", "--remove-section", "branch.topic"]);
        app.commits = super::super::load_watched(&[]).unwrap();
        select(&mut app, "topic");
        assert_eq!(
            app.selected_branch_for_diff().unwrap().remote_name(),
            Some("origin/topic")
        );
    }

    #[test]
    fn missing_upstreams_and_linear_histories_leave_the_picker_intact() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        init();
        commit("base.txt", "base");
        git(&["checkout", "-qb", "topic"]);
        let tip = commit("topic.txt", "topic");
        git(&["update-ref", "refs/remotes/origin/topic", &tip]);
        git(&["config", "branch.topic.remote", "origin"]);
        git(&["config", "branch.topic.merge", "refs/heads/deleted"]);
        let mut app = super::super::load_picker().unwrap();
        select(&mut app, "topic");
        assert!(
            app.selected_branch_for_diff().is_err(),
            "a missing upstream must not fall back to another remote branch"
        );
        press(&mut app, KeyCode::Char('r'));
        assert!(app
            .status
            .as_deref()
            .unwrap()
            .contains("No stored remote counterpart"));
        assert_eq!(app.history, History::Branches);
        git(&["update-ref", "refs/remotes/origin/main", &tip]);
        let mut app = super::super::load_picker().unwrap();
        select(&mut app, "main");
        press(&mut app, KeyCode::Char('r'));
        assert!(app
            .status
            .as_deref()
            .unwrap()
            .contains("no diverged history"));
        assert_eq!(app.history, History::Branches);
    }
}
