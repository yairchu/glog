//! Mark connections to branches whose tips need not appear in the Log.
use std::collections::HashSet;

use super::{read_command_at, read_concurrently, Commit, Repository};

pub(super) fn mark(commits: &mut [Commit], args: &[String], repository: Option<&Repository>) {
    if commits.is_empty() {
        return;
    }
    let Ok(output) = read_command_at(repository)
        .args([
            "for-each-ref",
            "--format=%(objectname)",
            "refs/heads",
            "refs/remotes",
        ])
        .output()
    else {
        return;
    };
    if !output.status.success() {
        return;
    }
    let branches: HashSet<_> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::to_owned)
        .collect();
    let Some(anchor) = anchor(args, repository) else {
        return;
    };
    let branches: Vec<_> = branches.into_iter().filter(|tip| tip != &anchor).collect();
    let bases: HashSet<_> = read_concurrently(&branches, |branch| {
        read_command_at(repository)
            // There can be multiple best common ancestors (criss-cross merges).
            .args(["merge-base", "--all", &anchor, branch])
            .env("GIT_NO_LAZY_FETCH", "1")
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| {
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    })
    .into_iter()
    .flatten()
    .collect();
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
fn anchor(args: &[String], repository: Option<&Repository>) -> Option<String> {
    let mut revisions = Vec::new();
    let mut args = args.iter();
    let mut all = false;
    while let Some(arg) = args.next() {
        if arg == "--" {
            break;
        }
        if crate::log_format::takes_pattern(arg) {
            args.next();
            continue;
        }
        all |= arg == "--all";
        revisions.push(arg);
    }
    let output = read_command_at(repository)
        .args(["rev-parse", "--revs-only", "--default", "HEAD"])
        .args(revisions)
        .env("GIT_NO_LAZY_FETCH", "1")
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let tips: HashSet<_> = text.lines().filter(|line| !line.starts_with('^')).collect();
    if !all && tips.len() == 1 {
        return tips.into_iter().next().map(str::to_owned);
    }
    super::resolve_names(["HEAD"], repository).remove("HEAD")
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
    fn counted_branch_logs_keep_the_explicit_interchange_anchor() {
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
        git(&["checkout", "-q", "main"]);
        git(&["commit", "--allow-empty", "-qm", "main tip"]);

        // The stacked branch connects at side's base, while HEAD connects
        // only at root. Limiting the Log must not change which fork is marked.
        for args in [
            vec!["side"],
            vec!["-n", "10", "side"],
            vec!["-n10", "side"],
            vec!["--max-count", "10", "side"],
            vec!["--max-count=10", "side"],
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
    fn all_branch_interchanges_use_one_anchor_and_linear_git_processes() {
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
        assert_eq!(calls.matches("merge-base --all ").count(), 13, "{calls}");
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
