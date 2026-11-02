//! Operations in progress in a worktree, such as a rebase or merge, read from
//! the state files `git status` reads to describe them.

use std::{cell::RefCell, collections::HashMap, fs, path::Path};

/// What `git status` reports beyond the changed files.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct State {
    /// The abbreviated commit of a detached HEAD.
    pub detached_at: Option<String>,
    /// Descriptions such as `Rebasing feature onto 1a2b3c4, 3/7`, the main
    /// operation first and a bisection last.
    pub operations: Vec<String>,
}

enum Operation {
    Rebase {
        branch: Option<String>,
        onto: Option<String>,
        step: Option<String>,
    },
    Am {
        step: Option<String>,
    },
    Merge {
        heads: Vec<String>,
    },
    CherryPick {
        commit: Option<String>,
    },
    Revert {
        commit: Option<String>,
    },
    Bisect {
        start: String,
    },
}

/// The state of the worktree at `root`, whose HEAD is the commit `detached`
/// when detached.
pub fn load(root: &Path, detached: Option<&str>) -> Result<State, String> {
    let operations = operations(root)?;
    // Describe once to learn which commits need abbreviating.
    let hashes = RefCell::new(detached.map(str::to_owned).into_iter().collect::<Vec<_>>());
    for operation in &operations {
        describe(operation, &|hash| {
            hashes.borrow_mut().push(hash.to_owned());
            String::new()
        });
    }
    let short = abbreviate(root, &hashes.into_inner());
    let short = |hash: &str| short.get(hash).cloned().unwrap_or_else(|| hash.to_owned());
    Ok(State {
        detached_at: detached.map(short),
        operations: operations
            .iter()
            .map(|operation| describe(operation, &short))
            .collect(),
    })
}

/// `operation` as `git status` would summarise it, with `short` abbreviating
/// its commits.
fn describe(operation: &Operation, short: &dyn Fn(&str) -> String) -> String {
    let suffix = |prefix: &str, text: Option<String>| {
        text.map_or(String::new(), |text| format!("{prefix}{text}"))
    };
    match operation {
        Operation::Rebase { branch, onto, step } => format!(
            "Rebasing{}{}{}",
            suffix(" ", branch.clone()),
            suffix(" onto ", onto.as_deref().map(short)),
            suffix(", ", step.clone()),
        ),
        Operation::Am { step } => format!("Applying patches{}", suffix(", ", step.clone())),
        Operation::Merge { heads } => {
            let heads: Vec<_> = heads.iter().map(|head| short(head)).collect();
            format!("Merging {}", heads.join(", "))
        }
        Operation::CherryPick { commit } => {
            format!(
                "Cherry-picking{}",
                suffix(" ", commit.as_deref().map(short))
            )
        }
        Operation::Revert { commit } => {
            format!("Reverting{}", suffix(" ", commit.as_deref().map(short)))
        }
        // Bisecting from a detached HEAD records its commit, otherwise the
        // branch name.
        Operation::Bisect { start } if is_hash(start) => format!("Bisecting from {}", short(start)),
        Operation::Bisect { start } => format!("Bisecting from {start}"),
    }
}

/// The operations in progress, in the order `load` describes them.
fn operations(root: &Path) -> Result<Vec<Operation>, String> {
    const NAMES: [&str; 6] = [
        "rebase-merge",
        "rebase-apply",
        "MERGE_HEAD",
        "sequencer/todo",
        "BISECT_LOG",
        "BISECT_START",
    ];
    let output = crate::git::read_command_in(root)
        .arg("rev-parse")
        .args(NAMES.iter().flat_map(|name| ["--git-path", name]))
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(crate::git::stderr_message(
            "git rev-parse failed",
            &output.stderr,
        ));
    }
    let paths: Vec<_> = output
        .stdout
        .split(|&byte| byte == b'\n')
        .take(NAMES.len())
        .map(|line| root.join(crate::git::raw_path(line)))
        .collect();
    let Ok([rebase_merge, rebase_apply, merge_head, todo, bisect_log, bisect_start]) =
        <[_; NAMES.len()]>::try_from(paths)
    else {
        return Err("unexpected git rev-parse output".to_owned());
    };
    // Unlike MERGE_HEAD, these may be stored as refs rather than files.
    let [cherry_pick_head, revert_head] = resolve(root, ["CHERRY_PICK_HEAD", "REVERT_HEAD"])?;
    let mut operations = Vec::new();
    let step = |directory: &Path, current: &str, total: &str| {
        Some(format!(
            "{}/{}",
            read(&directory.join(current))?,
            read(&directory.join(total))?
        ))
    };
    let rebase = |directory: &Path, step| Operation::Rebase {
        branch: read(&directory.join("head-name"))
            .and_then(|name| name.strip_prefix("refs/heads/").map(str::to_owned)),
        onto: read(&directory.join("onto")),
        step,
    };
    if rebase_merge.is_dir() {
        operations.push(rebase(&rebase_merge, step(&rebase_merge, "msgnum", "end")));
    } else if rebase_apply.is_dir() {
        let step = step(&rebase_apply, "next", "last");
        operations.push(if rebase_apply.join("applying").exists() {
            Operation::Am { step }
        } else {
            rebase(&rebase_apply, step)
        });
    } else if let Some(heads) = read(&merge_head) {
        operations.push(Operation::Merge {
            heads: heads.lines().map(str::to_owned).collect(),
        });
    } else if let Some(commit) = cherry_pick_head {
        operations.push(Operation::CherryPick {
            commit: Some(commit),
        });
    } else if let Some(commit) = revert_head {
        operations.push(Operation::Revert {
            commit: Some(commit),
        });
    } else if let Some(todo) = read(&todo) {
        // A sequence of picks or reverts between commits, with no commit
        // currently stopped.
        let command = todo
            .lines()
            .find(|line| !line.is_empty() && !line.starts_with('#'))
            .and_then(|line| line.split_whitespace().next());
        match command {
            Some("pick" | "p") => operations.push(Operation::CherryPick { commit: None }),
            Some("revert") => operations.push(Operation::Revert { commit: None }),
            _ => {}
        }
    }
    if bisect_log.exists() {
        if let Some(start) = read(&bisect_start) {
            operations.push(Operation::Bisect { start });
        }
    }
    Ok(operations)
}

/// The commit each ref in `names` points to, if it exists.
fn resolve<const N: usize>(root: &Path, names: [&str; N]) -> Result<[Option<String>; N], String> {
    use std::io::Write;
    let mut child = crate::git::read_command_in(root)
        .args(["cat-file", "--batch-check=%(objectname)"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| error.to_string())?;
    let mut stdin = child.stdin.take().unwrap();
    stdin
        .write_all(names.map(|name| format!("{name}\n")).concat().as_bytes())
        .map_err(|error| error.to_string())?;
    drop(stdin);
    let output = child
        .wait_with_output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(crate::git::stderr_message(
            "git cat-file failed",
            &output.stderr,
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut lines = stdout.lines();
    // Missing refs print `<name> missing` instead of a hash.
    Ok(names.map(|_| lines.next().filter(|line| is_hash(line)).map(str::to_owned)))
}

/// A state file's trimmed contents, or `None` when it is missing or empty.
fn read(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

fn is_hash(text: &str) -> bool {
    text.len() >= 40 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Each commit hash's abbreviation as Git would show it. Abbreviating only
/// shortens the display, so a failure leaves the hashes whole.
fn abbreviate(root: &Path, hashes: &[String]) -> HashMap<String, String> {
    if hashes.is_empty() {
        return HashMap::new();
    }
    let Ok(output) = crate::git::read_command_in(root)
        .args(["log", "--no-walk=unsorted", "--no-show-signature"])
        .arg("--format=%H %h")
        .args(hashes)
        .arg("--")
        .output()
    else {
        return HashMap::new();
    };
    if !output.status.success() {
        return HashMap::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_once(' '))
        .map(|(hash, short)| (hash.to_owned(), short.to_owned()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::tests::{git, CurrentDirGuard, TestDirectory};

    /// Run Git expecting it to stop, as on a conflict.
    fn stop(args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(["-c", "user.name=Test", "-c", "user.email=test@example.com"])
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(!output.status.success(), "{output:?}");
    }

    #[test]
    fn describes_operations_from_real_git_state() {
        describe_operations(&[]);
        // Reftable keeps pseudo-refs such as CHERRY_PICK_HEAD out of files.
        // Git before 2.45 cannot create such repositories.
        let supported = std::process::Command::new("git")
            .args(["init", "-q", "--ref-format=reftable"])
            .arg(TestDirectory::new().path())
            .output()
            .unwrap()
            .status
            .success();
        if supported {
            describe_operations(&["--ref-format=reftable"]);
        }
    }

    fn describe_operations(init: &[&str]) {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        let root = Path::new(".");
        let operations = || load(root, None).unwrap().operations;
        git(&[&["init", "-q", "-b", "main"], init].concat());
        fs::write("file", "base\n").unwrap();
        git(&["add", "file"]);
        git(&["commit", "-qm", "base"]);
        git(&["checkout", "-qb", "feature"]);
        fs::write("file", "feature\n").unwrap();
        git(&["commit", "-qam", "feature"]);
        git(&["checkout", "-q", "main"]);
        fs::write("file", "main\n").unwrap();
        git(&["commit", "-qam", "main"]);
        let short = |revision: &str| git(&["rev-parse", "--short", revision]);
        assert_eq!(operations(), Vec::<String>::new());

        git(&["checkout", "-q", "feature"]);
        stop(&["rebase", "main"]);
        let head = git(&["rev-parse", "HEAD"]);
        assert_eq!(
            load(root, Some(&head)).unwrap(),
            State {
                detached_at: Some(short("HEAD")),
                operations: vec![format!("Rebasing feature onto {}, 1/1", short("main"))],
            }
        );
        git(&["rebase", "--abort"]);

        git(&["checkout", "-q", "main"]);
        stop(&["merge", "feature"]);
        assert_eq!(operations(), [format!("Merging {}", short("feature"))]);
        git(&["merge", "--abort"]);

        stop(&["cherry-pick", "feature"]);
        assert_eq!(
            operations(),
            [format!("Cherry-picking {}", short("feature"))]
        );
        git(&["cherry-pick", "--abort"]);

        let patch = git(&["format-patch", "-1", "feature", "--stdout"]);
        fs::write("patch", patch + "\n").unwrap();
        stop(&["am", "patch"]);
        assert_eq!(operations(), ["Applying patches, 1/1"]);
        git(&["am", "--abort"]);

        git(&["bisect", "start"]);
        assert_eq!(operations(), ["Bisecting from main"]);
    }
}
