//! Mark connections to branches whose tips need not appear in the Log.
use std::{
    collections::{HashMap, HashSet},
    process::Command,
    sync::mpsc,
};

use super::{read_command_at, Cancel, Commit, Repository};

/// Work travels with its loaded history; replacing that history drops and
/// cancels its reader, so an older result cannot mark newer commits.
pub struct Pending {
    request: Option<Request>,
    read: Option<Read>,
}

struct Read {
    receiver: mpsc::Receiver<HashSet<String>>,
    cancel: Cancel,
}

struct Request {
    args: Vec<String>,
    command: Command,
}

impl Pending {
    pub fn new(args: &[String], repository: Option<&Repository>) -> Self {
        // Capture both the directory and environment before leaving the caller.
        // This also preserves relative GIT_DIR/object paths and test isolation.
        let base = read_command_at(repository);
        let mut environment: std::collections::BTreeMap<_, _> = std::env::vars_os().collect();
        for (key, value) in base.get_envs() {
            match value {
                Some(value) => {
                    environment.insert(key.to_owned(), value.to_owned());
                }
                None => {
                    environment.remove(key);
                }
            }
        }
        let mut command = Command::new(base.get_program());
        command.args(base.get_args()).env_clear().envs(environment);
        if let Some(directory) = base
            .get_current_dir()
            .map(std::path::Path::to_owned)
            .or_else(|| std::env::current_dir().ok())
        {
            command.current_dir(directory);
        }
        Self {
            request: Some(Request {
                args: args.to_vec(),
                command,
            }),
            read: None,
        }
    }

    /// Called only after the initial frame, never while loading the history.
    pub fn start(&mut self) {
        let Some(request) = self.request.take() else {
            return;
        };
        let (sender, receiver) = mpsc::channel();
        let cancel = Cancel::default();
        let worker_cancel = cancel.clone();
        std::thread::spawn(move || {
            let _ = sender.send(request.bases(&worker_cancel).unwrap_or_default());
        });
        self.read = Some(Read { receiver, cancel });
    }

    pub fn poll(&mut self, commits: &mut [Commit]) -> bool {
        let Some(read) = &self.read else {
            return false;
        };
        match read.receiver.try_recv() {
            Ok(bases) => {
                mark(commits, &bases);
                true
            }
            Err(mpsc::TryRecvError::Empty) => false,
            Err(mpsc::TryRecvError::Disconnected) => true,
        }
    }

    #[cfg(test)]
    pub fn settle(mut self, commits: &mut [Commit]) {
        self.start();
        if let Ok(bases) = self.read.as_ref().unwrap().receiver.recv() {
            mark(commits, &bases);
        }
    }
}

impl Drop for Pending {
    fn drop(&mut self) {
        if let Some(read) = &self.read {
            read.cancel.cancel();
        }
    }
}

impl Request {
    fn git(&self) -> Command {
        let mut command = Command::new(self.command.get_program());
        command.args(self.command.get_args()).env_clear();
        for (key, value) in self.command.get_envs() {
            if let Some(value) = value {
                command.env(key, value);
            }
        }
        if let Some(directory) = self.command.get_current_dir() {
            command.current_dir(directory);
        }
        command.env("GIT_NO_LAZY_FETCH", "1");
        command
    }

    fn bases(&self, cancel: &Cancel) -> Option<HashSet<String>> {
        let output = self
            .git()
            .args([
                "for-each-ref",
                "--format=%(objectname)",
                "refs/heads",
                "refs/remotes",
            ])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let anchor = anchor(self)?;
        let branches: HashSet<_> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|&tip| tip != anchor)
            .map(str::to_owned)
            .collect();
        if branches.is_empty() {
            return Some(HashSet::new());
        }
        // One topological walk reads all branches, with no argument-count limit.
        // In particular, --octopus would find ancestors shared by *every* tip,
        // whereas we need the union of each branch's bases with the anchor.
        let branches: Vec<_> = branches.into_iter().collect();
        let input = std::iter::once(anchor.as_str())
            .chain(branches.iter().map(String::as_str))
            .map(|tip| format!("{tip}\n"))
            .collect::<String>();
        let output = super::pipe_through_cancellable(
            self.git()
                .args(["rev-list", "--topo-order", "--parents", "--stdin"]),
            input.as_bytes(),
            cancel,
        )?;
        let graph = String::from_utf8(output).ok()?;
        Some(bases_in_graph(&graph, &anchor, &branches))
    }
}

#[derive(Default)]
struct Paint {
    anchor: bool,
    reached: Vec<u64>,
    covered: Vec<u64>,
}

/// Propagate branch identities from children to parents. A common ancestor
/// is a best base for a branch only if no common descendant already covered
/// that branch. Tracking coverage separately retains all criss-cross bases.
/// Remove processed paint so memory follows the walk's frontier, not its depth.
fn bases_in_graph(graph: &str, anchor: &str, branches: &[String]) -> HashSet<String> {
    let words = branches.len().div_ceil(64);
    let mut pending = HashMap::<&str, Paint>::new();
    pending.entry(anchor).or_default().anchor = true;
    for (index, tip) in branches.iter().enumerate() {
        let paint = pending.entry(tip).or_default();
        paint.reached.resize(words, 0);
        paint.reached[index / 64] |= 1 << (index % 64);
    }
    let mut bases = HashSet::new();
    for line in graph.lines() {
        let mut fields = line.split_whitespace();
        let Some(hash) = fields.next() else {
            continue;
        };
        let Some(mut paint) = pending.remove(hash) else {
            continue;
        };
        paint.reached.resize(words, 0);
        paint.covered.resize(words, 0);
        if paint.anchor {
            if paint
                .reached
                .iter()
                .zip(&paint.covered)
                .any(|(reached, covered)| reached & !covered != 0)
            {
                bases.insert(hash.to_owned());
            }
            for (covered, reached) in paint.covered.iter_mut().zip(&paint.reached) {
                *covered |= reached;
            }
        }
        for parent in fields {
            let next = pending.entry(parent).or_default();
            next.anchor |= paint.anchor;
            next.reached.resize(words, 0);
            next.covered.resize(words, 0);
            for (next, reached) in next.reached.iter_mut().zip(&paint.reached) {
                *next |= reached;
            }
            for (next, covered) in next.covered.iter_mut().zip(&paint.covered) {
                *next |= covered;
            }
        }
    }
    bases
}

fn mark(commits: &mut [Commit], bases: &HashSet<String>) {
    for commit in commits {
        // Keep Git's comparison, patch-equivalence, and boundary markers.
        if matches!(commit.graph_node, '*' | '.') && bases.contains(&commit.hash) {
            if let Some(row) = commit.graph.last_mut() {
                *row = row.replacen(commit.graph_node, "@", 1);
            }
            commit.graph_node = '@';
        }
    }
}

/// A single positive revision anchors an explicitly opened history, even
/// when filters omit its tip. Multi-revision Logs use the checkout instead.
fn anchor(request: &Request) -> Option<String> {
    let args = &request.args;
    let mut revisions = Vec::new();
    let mut args = args.iter();
    let mut all = false;
    while let Some(arg) = args.next() {
        if arg == "--" {
            break;
        }
        // Unlike git log, rev-parse treats separate filter operands as paths
        // and stops resolving revisions there. Filters don't change the
        // endpoints, so consume their values before resolving the anchor.
        if crate::log_format::takes_pattern(arg)
            || matches!(
                arg.as_str(),
                "-n" | "--max-count"
                    | "--skip"
                    | "--since"
                    | "--after"
                    | "--since-as-filter"
                    | "--until"
                    | "--before"
            )
        {
            args.next();
            continue;
        }
        all |= arg == "--all";
        revisions.push(arg);
    }
    let output = request
        .git()
        // --revs-only also emits traversal flags such as --max-count=10.
        // Only resolved revisions may participate in choosing the anchor.
        .args([
            "rev-parse",
            "--revs-only",
            "--no-flags",
            "--default",
            "HEAD",
        ])
        .args(revisions)
        .env("GIT_NO_LAZY_FETCH", "1")
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let tips: HashSet<_> = text.lines().filter(|line| !line.starts_with('^')).collect();
    let tip = if !all && tips.len() == 1 {
        tips.into_iter().next()?
    } else {
        "HEAD"
    };
    // rev-parse --revs-only can return an annotated tag object. The graph
    // contains commits, so seed its peeled commit just as merge-base does.
    let output = request
        .git()
        .args(["rev-parse", "--verify", &format!("{tip}^{{commit}}")])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

#[cfg(test)]
mod tests {
    use crate::{
        ansi,
        git::{
            load_log,
            tests::{git, CurrentDirGuard, TestDirectory},
        },
        log_folds::LogFolds,
    };

    #[test]
    fn batched_bases_match_git_for_criss_cross_and_many_distinct_tips() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-qb", "main"]);
        git(&["commit", "--allow-empty", "-qm", "root"]);
        let root = git(&["rev-parse", "HEAD"]);
        let tree = git(&["rev-parse", "HEAD^{tree}"]);
        let commit = |name: &str, parents: &[&str]| {
            let mut args = vec!["commit-tree", &tree, "-m", name];
            for parent in parents {
                args.extend(["-p", parent]);
            }
            git(&args)
        };
        let a = commit("a", &[&root]);
        let b = commit("b", &[&root]);
        let left = commit("left merge", &[&a, &b]);
        let right = commit("right merge", &[&b, &a]);
        let anchor = commit("anchor", &[&left]);
        let unrelated = commit("unrelated", &[]);
        let mut tips = vec![right.clone(), unrelated, a.clone(), left];
        // Cross a machine-word boundary in the branch paint, and include both
        // dominated common ancestors and tips with two best common ancestors.
        for n in 0..70 {
            tips.push(commit(
                &format!("tip {n}"),
                &[if n % 2 == 0 { &right } else { &b }],
            ));
        }
        git(&["update-ref", "HEAD", &anchor]);
        for (index, tip) in tips.iter().enumerate() {
            git(&["update-ref", &format!("refs/heads/branch{index}"), tip]);
        }
        let expected: std::collections::HashSet<_> = tips
            .iter()
            .flat_map(|tip| {
                let mut command = crate::git::read_command();
                let output = command
                    .args(["merge-base", "--all", &anchor, tip])
                    .output()
                    .unwrap();
                String::from_utf8(output.stdout)
                    .unwrap()
                    .lines()
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .collect();
        assert!(expected.contains(&a));
        assert!(expected.contains(&b));
        let request = super::Pending::new(&[], None).request.take().unwrap();
        assert_eq!(
            request.bases(&crate::git::Cancel::default()).unwrap(),
            expected
        );
    }

    #[test]
    fn interchanges_wait_for_the_first_frame_and_preserve_navigation() {
        use crate::{app::App, git::load_log_at, ui};
        use ratatui::{backend::TestBackend, Terminal};

        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-qb", "main"]);
        git(&["commit", "--allow-empty", "-qm", "root"]);
        let root = git(&["rev-parse", "HEAD"]);
        git(&["branch", "side"]);
        git(&["commit", "--allow-empty", "-qm", "tip"]);
        let loaded = load_log_at(&[], None).unwrap();
        assert!(loaded.interchanges.as_ref().unwrap().read.is_none());
        assert!(loaded.interchanges.as_ref().unwrap().request.is_some());
        assert!(loaded.iter().all(|commit| commit.graph_node != '@'));
        let mut app = App::new(loaded);
        app.selected = 1;
        app.status = Some("keep message".into());
        let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        assert!(!app.poll_log());
        assert!(app.commits.iter().all(|commit| commit.graph_node != '@'));
        app.start_interchanges();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while app.commits[app.selected].graph_node != '@' {
            app.poll_log();
            assert!(
                std::time::Instant::now() < deadline,
                "marker read did not finish"
            );
            std::thread::yield_now();
        }
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        assert_eq!(app.commits[app.selected].hash, root);
        assert_eq!(app.commits[app.selected].graph_node, '@');
        assert_eq!(app.status.as_deref(), Some("keep message"));
        assert!(terminal
            .backend()
            .buffer()
            .content
            .iter()
            .any(|cell| cell.symbol() == "⦵"));
    }

    #[test]
    fn deferred_interchanges_keep_the_loading_repository() {
        let directory = TestDirectory::new();
        let other = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-qb", "main"]);
        git(&["commit", "--allow-empty", "-qm", "root"]);
        git(&["branch", "side"]);
        git(&["commit", "--allow-empty", "-qm", "tip"]);
        let mut loaded = crate::git::load_log_at(&[], None).unwrap();
        std::env::set_current_dir(other.path()).unwrap();
        git(&["init", "-qb", "other"]);
        git(&["commit", "--allow-empty", "-qm", "unrelated root"]);
        loaded
            .interchanges
            .take()
            .unwrap()
            .settle(&mut loaded.commits);
        assert_eq!(loaded.commits.last().unwrap().graph_node, '@');
    }

    #[test]
    fn replacing_history_discards_an_older_marker_result() {
        use crate::{
            app::App,
            git::{load_log_at, LoadedLog},
        };
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-qb", "main"]);
        git(&["commit", "--allow-empty", "-qm", "root"]);
        let mut loaded = load_log_at(&[], None).unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut pending = loaded.interchanges.take().unwrap();
        pending.request = None;
        pending.read = Some(super::Read {
            receiver,
            cancel: crate::git::Cancel::default(),
        });
        let commits = loaded.commits.clone();
        sender
            .send(commits.iter().map(|commit| commit.hash.clone()).collect())
            .unwrap();
        loaded.interchanges = Some(pending);
        let mut app = App::new(loaded);
        app.replace_commits(LoadedLog::from(commits));
        assert!(!app.poll_log());
        assert!(app.commits.iter().all(|commit| commit.graph_node != '@'));
    }

    #[test]
    fn filtered_branch_logs_keep_the_explicit_interchange_anchor() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        git(&["checkout", "-qb", "main"]);
        git(&["commit", "--allow-empty", "-qm", "root"]);
        git(&["checkout", "-qb", "side"]);
        git(&["commit", "--allow-empty", "-qm", "side base"]);
        let base = git(&["rev-parse", "HEAD"]);
        git(&["checkout", "-qb", "stacked"]);
        git(&["commit", "--allow-empty", "-qm", "stacked tip"]);
        git(&["checkout", "-q", "side"]);
        git(&["commit", "--allow-empty", "-qm", "side tip"]);
        git(&["tag", "-am", "annotated side", "side-tag"]);
        git(&["checkout", "-q", "main"]);
        git(&["commit", "--allow-empty", "-qm", "main tip"]);

        // The stacked branch connects at side's base, while HEAD connects
        // only at root. Limiting the Log must not change which fork is marked.
        for args in [
            vec!["side"],
            vec!["side-tag"],
            vec!["-n", "10", "side"],
            vec!["-n10", "side"],
            vec!["--max-count", "10", "side"],
            vec!["--max-count=10", "side"],
            vec!["--since", "2000-01-01", "side"],
            vec!["--after", "2000-01-01", "side"],
            vec!["--since-as-filter", "2000-01-01", "side"],
            vec!["--until", "2099-01-01", "side"],
            vec!["--before", "2099-01-01", "side"],
            vec!["--date", "iso", "side"],
            vec!["--skip", "1", "side"],
        ] {
            let args = args.into_iter().map(str::to_owned).collect::<Vec<_>>();
            let commits = load_log(&args).unwrap();
            assert_eq!(
                commits
                    .iter()
                    .find(|commit| commit.hash == base)
                    .unwrap()
                    .graph_node,
                '@',
                "{args:?}"
            );
        }
    }

    #[test]
    fn all_branch_interchanges_use_one_batched_ancestry_walk() {
        struct Trace(Option<std::ffi::OsString>);
        impl Drop for Trace {
            fn drop(&mut self) {
                match self.0.take() {
                    Some(value) => std::env::set_var("GIT_TRACE", value),
                    None => std::env::remove_var("GIT_TRACE"),
                }
            }
        }
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        git(&["checkout", "-qb", "main"]);
        git(&["commit", "--allow-empty", "-qm", "root"]);
        let root = git(&["rev-parse", "HEAD"]);
        git(&["checkout", "-qb", "side"]);
        git(&["commit", "--allow-empty", "-qm", "side base"]);
        let side_base = git(&["rev-parse", "HEAD"]);
        for n in 0..12 {
            let branch = format!("branch{n}");
            git(&["checkout", "-qB", &branch, &side_base]);
            git(&["commit", "--allow-empty", "-qm", &branch]);
            // Duplicate remote tips must not cause additional comparisons.
            git(&[
                "update-ref",
                &format!("refs/remotes/origin/{branch}"),
                "HEAD",
            ]);
        }
        git(&["checkout", "-q", "main"]);
        git(&["commit", "--allow-empty", "-qm", "main tip"]);
        let trace_path = directory.path().join("git trace.txt");
        let trace = Trace(std::env::var_os("GIT_TRACE"));
        std::env::set_var("GIT_TRACE", &trace_path);
        let commits = load_log(&["--all".into()]).unwrap();
        drop(trace);
        let calls = std::fs::read_to_string(trace_path).unwrap();
        assert!(!calls.contains("merge-base"), "{calls}");
        assert_eq!(
            calls
                .matches("rev-list --topo-order --parents --stdin")
                .count(),
            1,
            "{calls}"
        );
        assert!(!calls.contains("merge-base --independent"), "{calls}");
        let node = |hash: &str| commits.iter().find(|c| c.hash == hash).unwrap().graph_node;
        assert_eq!(node(&root), '@');
        assert_eq!(
            node(&side_base),
            '.',
            "other branches' own forks are not interchanges with main"
        );

        // A filtered, reversed explicit Log must use the named branch's actual
        // tip, even when neither the tip nor HEAD appears in the displayed rows.
        for args in [
            vec!["side", "--grep", "side", "--reverse", "--no-graph"],
            vec!["main..side"],
        ] {
            let commits =
                load_log(&args.into_iter().map(str::to_owned).collect::<Vec<_>>()).unwrap();
            assert_eq!(
                commits
                    .iter()
                    .find(|c| c.hash == side_base)
                    .unwrap()
                    .graph_node,
                '@'
            );
        }
    }

    #[test]
    fn interchanges_find_moved_local_and_remote_branches_in_filtered_history() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        git(&["symbolic-ref", "HEAD", "refs/heads/release"]);
        git(&["commit", "--allow-empty", "-qm", "keep root"]);
        let root = git(&["rev-parse", "HEAD"]);
        git(&["checkout", "-qb", "feature"]);
        git(&["commit", "--allow-empty", "-qm", "keep feature base"]);
        let feature_base = git(&["rev-parse", "HEAD"]);
        git(&["checkout", "-qb", "stacked"]);
        git(&["commit", "--allow-empty", "-qm", "keep ordinary"]);
        let ordinary = git(&["rev-parse", "HEAD"]);
        git(&["commit", "--allow-empty", "-qm", "omit middle"]);
        git(&["commit", "--allow-empty", "-qm", "keep tip"]);
        let tip = git(&["rev-parse", "HEAD"]);
        // A remote copy at the same tip must not mark the tip itself.
        git(&["update-ref", "refs/remotes/origin/stacked", &tip]);
        git(&["checkout", "-q", "feature"]);
        git(&["commit", "--allow-empty", "-qm", "feature advanced"]);
        git(&["checkout", "-q", "release"]);
        git(&["commit", "--allow-empty", "-qm", "release advanced"]);
        git(&["update-ref", "refs/remotes/origin/release", "HEAD"]);
        git(&["checkout", "-q", "stacked"]);
        git(&["branch", "-D", "release"]);

        for args in [vec![], vec!["--grep=keep".into()], vec!["--all".into()]] {
            let commits = load_log(&args).unwrap();
            let mut folds = LogFolds::default();
            folds.refresh(&commits).unwrap();
            folds.settle(&commits);
            for ascii in [false, true] {
                folds.ascii_graph = ascii;
                for filtered in [false, true] {
                    if filtered {
                        folds.toggle_type(Some("nonexistent"), &commits).unwrap();
                    }
                    for hash in [&root, &feature_base, &ordinary, &tip] {
                        let index = commits.iter().position(|c| &c.hash == hash).unwrap();
                        let row = ansi::plain(
                            folds.display_graph(index, &commits[index]).last().unwrap(),
                        );
                        let expected = if hash == &tip || hash == &ordinary {
                            if ascii {
                                '.'
                            } else {
                                '•'
                            }
                        } else if ascii {
                            '@'
                        } else {
                            '⦵'
                        };
                        assert!(
                            row.contains(expected),
                            "{args:?}: expected {expected} in {row:?}"
                        );
                    }
                    if filtered {
                        folds.toggle_type(None, &commits).unwrap();
                    }
                }
            }
        }
        let reversed = load_log(&["--reverse".into(), "--no-graph".into()]).unwrap();
        assert_eq!(reversed.first().unwrap().hash, root);
        assert_eq!(reversed.first().unwrap().graph_node, '@');
        assert_eq!(reversed.last().unwrap().hash, tip);
        assert_eq!(reversed.last().unwrap().graph_node, '.');
        // Explicitly opening a branch works independently of the checkout.
        git(&["checkout", "-q", "feature"]);
        let commits = load_log(&["stacked".into()]).unwrap();
        assert_eq!(
            commits.iter().find(|c| c.hash == root).unwrap().graph_node,
            '@'
        );
        assert_eq!(
            commits
                .iter()
                .find(|c| c.hash == feature_base)
                .unwrap()
                .graph_node,
            '@'
        );
        let commits = load_log(&["--boundary".into(), format!("{root}..stacked")]).unwrap();
        assert_eq!(
            commits.iter().find(|c| c.hash == root).unwrap().graph_node,
            'o'
        );
    }
}
