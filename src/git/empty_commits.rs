//! Detect content-free commits from actual objects, independent of log filters.
use std::collections::{HashMap, HashSet};

use super::{pipe_through, pipe_through_bytes, Commit, Repository};

struct Header {
    tree: String,
    parents: Vec<String>,
}

pub(super) fn mark(commits: &mut [Commit], repository: Option<&Repository>) {
    // Preserve Git's boundary, comparison, and patch-equivalence markers.
    let candidates = || commits.iter().filter(|commit| commit.graph_node == '*');
    let mut headers = listed_headers(candidates().map(|commit| commit.hash.as_str()), repository);
    // Git reports a shallow commit as parentless. Its raw object keeps the
    // parent, so that a missing parent cannot make it look like a root.
    let apparent_roots: Vec<_> = headers
        .iter()
        .filter(|(_, header)| header.parents.is_empty())
        .map(|(hash, _)| hash.clone())
        .collect();
    headers.extend(read_headers(
        apparent_roots.iter().map(String::as_str),
        repository,
    ));
    // Parents are usually listed too. Look up only the others, by plain object
    // name: peeling each `parent^{tree}` makes Git resolve it as a revision,
    // which is orders of magnitude slower over a long history.
    let missing: Vec<_> = candidates()
        .filter_map(
            |commit| match headers.get(&commit.hash)?.parents.as_slice() {
                [parent] if !headers.contains_key(parent) => Some(parent.clone()),
                _ => None,
            },
        )
        .collect();
    headers.extend(read_headers(missing.iter().map(String::as_str), repository));
    // Only a root's tree must be read, to tell whether it is the empty tree.
    let roots: Vec<_> = candidates()
        .filter_map(|commit| {
            let header = headers.get(&commit.hash)?;
            header.parents.is_empty().then(|| header.tree.clone())
        })
        .collect();
    let empty_roots = empty_trees(&roots, repository);
    for commit in commits.iter_mut().filter(|commit| commit.graph_node == '*') {
        let Some(header) = headers.get(&commit.hash) else {
            continue;
        };
        let empty = match header.parents.as_slice() {
            [] => empty_roots.contains(&header.tree),
            // Missing parents in shallow or partial clones leave the marker
            // unchanged; identifying an empty commit must not fetch objects.
            [parent] => headers
                .get(parent)
                .is_some_and(|parent| parent.tree == header.tree),
            // A merge joins histories even when its tree is unchanged.
            _ => false,
        };
        if empty {
            commit.graph_node = '.';
            if let Some(row) = commit.graph.last_mut() {
                *row = row.replacen('*', ".", 1);
            }
        }
    }
}

/// Headers of commits Git just listed, so they exist. Unlike `cat-file`,
/// `rev-list` reads them from the commit-graph when there is one.
fn listed_headers<'a>(
    hashes: impl Iterator<Item = &'a str>,
    repository: Option<&Repository>,
) -> HashMap<String, Header> {
    let input: String = hashes.map(|hash| format!("{hash}\n")).collect();
    if input.is_empty() {
        return HashMap::new();
    }
    let Some(output) = pipe_through(
        crate::git::read_command_at(repository)
            .args([
                "rev-list",
                "--no-walk=unsorted",
                "--stdin",
                "--format=%H %T %P",
            ])
            .env("GIT_NO_LAZY_FETCH", "1"),
        input.as_bytes(),
    ) else {
        return HashMap::new();
    };
    output
        .lines()
        .filter(|line| !line.starts_with("commit "))
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let hash = fields.next()?.to_owned();
            let tree = fields.next()?.to_owned();
            let parents = fields.map(str::to_owned).collect();
            Some((hash, Header { tree, parents }))
        })
        .collect()
}

/// Raw headers, without fetching or failing on missing objects.
fn read_headers<'a>(
    hashes: impl Iterator<Item = &'a str>,
    repository: Option<&Repository>,
) -> HashMap<String, Header> {
    let input: String = hashes.map(|hash| format!("{hash}\n")).collect();
    if input.is_empty() {
        return HashMap::new();
    }
    pipe_through_bytes(
        crate::git::read_command_at(repository)
            .args(["cat-file", "--batch"])
            .env("GIT_NO_LAZY_FETCH", "1"),
        input.as_bytes(),
    )
    .map(|objects| headers(&objects))
    .unwrap_or_default()
}

fn empty_trees(trees: &[String], repository: Option<&Repository>) -> HashSet<String> {
    if trees.is_empty() {
        return HashSet::new();
    }
    let input: String = trees.iter().map(|tree| format!("{tree}\n")).collect();
    let Some(output) = pipe_through(
        crate::git::read_command_at(repository)
            .args(["cat-file", "--batch-check"])
            .env("GIT_NO_LAZY_FETCH", "1"),
        input.as_bytes(),
    ) else {
        return HashSet::new();
    };
    output
        .lines()
        .filter_map(
            |response| match response.split_whitespace().collect::<Vec<_>>()[..] {
                [tree, "tree", "0"] => Some(tree.to_owned()),
                _ => None,
            },
        )
        .collect()
}

fn headers(mut output: &[u8]) -> HashMap<String, Header> {
    let mut headers = HashMap::new();
    while let Some(end) = output.iter().position(|&byte| byte == b'\n') {
        let line = String::from_utf8_lossy(&output[..end]);
        output = &output[end + 1..];
        let fields: Vec<_> = line.split_whitespace().collect();
        let [hash, "commit", size] = fields.as_slice() else {
            continue;
        };
        let Some(size) = size
            .parse::<usize>()
            .ok()
            .filter(|&size| size < output.len())
        else {
            break;
        };
        let object = String::from_utf8_lossy(&output[..size]);
        output = &output[size + 1..];
        let mut lines = object.lines().take_while(|line| !line.is_empty());
        if let Some(tree) = lines.next().and_then(|line| line.strip_prefix("tree ")) {
            headers.insert(
                (*hash).to_owned(),
                Header {
                    tree: tree.to_owned(),
                    parents: lines
                        .filter_map(|line| line.strip_prefix("parent ").map(str::to_owned))
                        .collect(),
                },
            );
        }
    }
    headers
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
    fn empty_markers_use_actual_trees_and_never_mark_merges_empty() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        git(&["symbolic-ref", "HEAD", "refs/heads/main"]);
        git(&["commit", "--allow-empty", "-qm", "empty root"]);
        let root = git(&["rev-parse", "HEAD"]);
        std::fs::write("file", "contents\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "content"]);
        let content = git(&["rev-parse", "HEAD"]);
        git(&["commit", "--allow-empty", "-qm", "empty change"]);
        let empty = git(&["rev-parse", "HEAD"]);
        git(&["checkout", "-qb", "side"]);
        git(&["commit", "--allow-empty", "-qm", "empty side"]);
        git(&["checkout", "-q", "main"]);
        git(&["merge", "--no-ff", "-qm", "unchanged merge", "side"]);
        let merge = git(&["rev-parse", "HEAD"]);
        assert_eq!(
            git(&["rev-parse", "HEAD^{tree}"]),
            git(&["rev-parse", "HEAD^1^{tree}"])
        );
        std::fs::write("other", "unrelated contents\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "unrelated change"]);
        let unrelated = git(&["rev-parse", "HEAD"]);
        git(&["commit", "--allow-empty", "-qm", "empty tip"]);
        let tip = git(&["rev-parse", "HEAD"]);

        let commits = load_log(&[]).unwrap();
        let mut folds = LogFolds::default();
        folds.refresh(&commits).unwrap();
        folds.settle(&commits);
        for (hash, marker) in [
            (&root, '•'),
            (&content, '●'),
            (&empty, '•'),
            (&merge, '●'),
            (&unrelated, '●'),
            (&tip, '•'),
        ] {
            let index = commits.iter().position(|c| &c.hash == hash).unwrap();
            let row = ansi::plain(folds.display_graph(index, &commits[index]).last().unwrap());
            assert!(row.contains(marker), "{row}: {}", commits[index].subject);
        }
        assert_eq!(load_log(&["-n1".into()]).unwrap()[0].graph_node, '.');
        let filtered = load_log(&["--sparse".into(), "--".into(), "file".into()]).unwrap();
        assert_eq!(
            filtered
                .iter()
                .find(|c| c.hash == unrelated)
                .unwrap()
                .graph_node,
            '*'
        );
        let first_parent = load_log(&["--first-parent".into()]).unwrap();
        assert_eq!(
            first_parent
                .iter()
                .find(|c| c.hash == merge)
                .unwrap()
                .graph_node,
            '*'
        );

        // A shallow commit with an empty tree may have deleted its parent's
        // files. Missing ancestry must not make it look like an empty root.
        let empty_tree = git(&["mktree"]);
        let deletion = git(&[
            "commit-tree",
            &empty_tree,
            "-p",
            &tip,
            "-m",
            "delete everything",
        ]);
        git(&["update-ref", "refs/heads/shallow-target", &deletion]);
        let shallow = TestDirectory::new();
        let shallow_path = shallow.path().to_str().unwrap();
        git(&["init", "-q", shallow_path]);
        git(&[
            "-C",
            shallow_path,
            "fetch",
            "-q",
            "--depth=1",
            directory.path().to_str().unwrap(),
            "shallow-target",
        ]);
        git(&["-C", shallow_path, "checkout", "-q", "FETCH_HEAD"]);
        std::env::set_current_dir(shallow.path()).unwrap();
        let commits = load_log(&[]).unwrap();
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].hash, deletion);
        assert_eq!(commits[0].graph_node, '*');
        std::env::set_current_dir(directory.path()).unwrap();
    }
}
