//! Detect content-free commits from actual objects, independent of log filters.
use std::collections::HashMap;

use super::{pipe_through, pipe_through_bytes, Commit};

struct Header {
    tree: String,
    parents: Vec<String>,
}

pub(super) fn mark(commits: &mut [Commit]) {
    // Preserve Git's boundary, comparison, and patch-equivalence markers.
    let input: String = commits
        .iter()
        .filter(|commit| commit.graph_node == '*')
        .map(|commit| format!("{}\n", commit.hash))
        .collect();
    if input.is_empty() {
        return;
    }
    let Some(objects) = pipe_through_bytes(
        crate::git::read_command()
            .args(["cat-file", "--batch"])
            .env("GIT_NO_LAZY_FETCH", "1"),
        input.as_bytes(),
    ) else {
        return;
    };
    let headers = headers(&objects);
    let requests: Vec<_> = commits
        .iter()
        .enumerate()
        .filter(|(_, commit)| commit.graph_node == '*')
        .filter_map(|(index, commit)| {
            let header = headers.get(&commit.hash)?;
            let object = match header.parents.as_slice() {
                [] => header.tree.clone(),
                [parent] => format!("{parent}^{{tree}}"),
                // A merge joins histories even when its tree is unchanged.
                _ => return None,
            };
            Some((index, header, object))
        })
        .collect();
    if requests.is_empty() {
        return;
    }
    let input: String = requests
        .iter()
        .map(|(_, _, object)| format!("{object}\n"))
        .collect();
    let Some(output) = pipe_through(
        crate::git::read_command()
            .args(["cat-file", "--batch-check"])
            .env("GIT_NO_LAZY_FETCH", "1"),
        input.as_bytes(),
    ) else {
        return;
    };
    for ((index, header, _), response) in requests.iter().zip(output.lines()) {
        let fields: Vec<_> = response.split_whitespace().collect();
        let [tree, "tree", size] = fields.as_slice() else {
            // Missing parents in shallow or partial clones leave the marker
            // unchanged; identifying an empty commit must not fetch objects.
            continue;
        };
        let empty = if header.parents.is_empty() {
            *size == "0"
        } else {
            *tree == header.tree
        };
        if empty {
            let commit = &mut commits[*index];
            commit.graph_node = '.';
            if let Some(row) = commit.graph.last_mut() {
                *row = row.replacen('*', ".", 1);
            }
        }
    }
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
