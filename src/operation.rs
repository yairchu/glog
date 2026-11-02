//! Operations in progress in a worktree, such as a rebase or merge, read from
//! the state files `git status` reads to describe them.

use ratatui::text::Span;
use std::{collections::HashMap, fs, path::Path};

/// What `git status` reports beyond the changed files.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct State {
    /// The abbreviated commit of a detached HEAD.
    pub detached_at: Option<String>,
    /// Descriptions such as `Rebasing feature onto 1a2b3c4, 3/7`, the main
    /// operation first and a bisection last.
    pub operations: Vec<Text>,
    /// The commits being applied, such as the one a rebase stopped at.
    pub commits: Vec<Commit>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Commit {
    pub hash: String,
    /// Such as `Stopped at 1a2b3c4 Fix the layout`.
    pub label: Text,
}

/// Text whose commit hashes are highlighted as in Log.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct Text(Vec<(String, bool)>);

impl Text {
    fn push(&mut self, text: &str) {
        self.0.push((text.to_owned(), false));
    }
    fn push_hash(&mut self, hash: &str) {
        self.0.push((hash.to_owned(), true));
    }
    fn hashes(&self) -> impl Iterator<Item = &str> {
        self.0
            .iter()
            .filter(|(_, hash)| *hash)
            .map(|(text, _)| text.as_str())
    }
    fn abbreviate(&mut self, short: impl Fn(&str) -> String) {
        for (text, hash) in &mut self.0 {
            if *hash {
                *text = short(text);
            }
        }
    }
    pub fn spans(&self) -> Vec<Span<'static>> {
        self.0
            .iter()
            .map(|(text, hash)| match hash {
                true => crate::log_format::field_span(crate::log_format::Field::Hash, text.clone()),
                false => Span::raw(text.clone()),
            })
            .collect()
    }
}

impl std::fmt::Display for Text {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.iter().try_for_each(|(text, _)| f.write_str(text))
    }
}

enum Operation {
    Rebase {
        branch: Option<String>,
        onto: Option<String>,
        step: Option<String>,
        stopped: Option<String>,
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
    let commits: Vec<_> = operations.iter().flat_map(commits).collect();
    // Describe with whole hashes, then abbreviate the ones the text shows.
    let mut descriptions: Vec<_> = operations.iter().map(describe).collect();
    let hashes: Vec<_> = detached
        .into_iter()
        .chain(descriptions.iter().flat_map(Text::hashes))
        .chain(commits.iter().map(|(_, hash)| hash.as_str()))
        .collect();
    let abbreviated = abbreviate(root, &hashes);
    let short = |hash: &str| {
        abbreviated
            .get(hash)
            .map_or_else(|| hash.to_owned(), |(short, _)| short.clone())
    };
    for description in &mut descriptions {
        description.abbreviate(short);
    }
    Ok(State {
        detached_at: detached.map(short),
        operations: descriptions,
        commits: commits
            .into_iter()
            .map(|(verb, hash)| {
                let mut label = Text::default();
                label.push(&format!("{verb} "));
                label.push_hash(&short(hash));
                if let Some((_, subject)) = abbreviated.get(hash) {
                    label.push(&format!(" {subject}"));
                }
                Commit {
                    hash: hash.clone(),
                    label,
                }
            })
            .collect(),
    })
}

/// The commits `operation` is applying, each with what it is doing with it.
fn commits(operation: &Operation) -> Vec<(&'static str, &String)> {
    match operation {
        Operation::Rebase { stopped, .. } => {
            stopped.iter().map(|hash| ("Stopped at", hash)).collect()
        }
        Operation::Merge { heads } => heads.iter().map(|hash| ("Merging", hash)).collect(),
        Operation::CherryPick { commit } => {
            commit.iter().map(|hash| ("Cherry-picking", hash)).collect()
        }
        Operation::Revert { commit } => commit.iter().map(|hash| ("Reverting", hash)).collect(),
        Operation::Am { .. } | Operation::Bisect { .. } => Vec::new(),
    }
}

/// `operation` as `git status` would summarise it, with whole hashes.
fn describe(operation: &Operation) -> Text {
    let mut text = Text::default();
    let commit = |text: &mut Text, prefix: &str, hash: &str| {
        text.push(prefix);
        text.push_hash(hash);
    };
    match operation {
        Operation::Rebase {
            branch, onto, step, ..
        } => {
            text.push("Rebasing");
            if let Some(branch) = branch {
                text.push(&format!(" {branch}"));
            }
            if let Some(onto) = onto {
                commit(&mut text, " onto ", onto);
            }
            if let Some(step) = step {
                text.push(&format!(", {step}"));
            }
        }
        Operation::Am { step } => {
            text.push("Applying patches");
            if let Some(step) = step {
                text.push(&format!(", {step}"));
            }
        }
        Operation::Merge { heads } => {
            for (index, head) in heads.iter().enumerate() {
                commit(&mut text, if index == 0 { "Merging " } else { ", " }, head);
            }
        }
        Operation::CherryPick { commit: picked } => {
            text.push("Cherry-picking");
            if let Some(picked) = picked {
                commit(&mut text, " ", picked);
            }
        }
        Operation::Revert { commit: reverted } => {
            text.push("Reverting");
            if let Some(reverted) = reverted {
                commit(&mut text, " ", reverted);
            }
        }
        // Bisecting from a detached HEAD records its commit, otherwise the
        // branch name.
        Operation::Bisect { start } if is_hash(start) => {
            commit(&mut text, "Bisecting from ", start)
        }
        Operation::Bisect { start } => text.push(&format!("Bisecting from {start}")),
    }
    text
}

/// The operations in progress, in the order `load` describes them.
fn operations(root: &Path) -> Result<Vec<Operation>, String> {
    let output = crate::git::read_command_in(root)
        .args(["rev-parse", "--absolute-git-dir"])
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(crate::git::stderr_message(
            "git rev-parse failed",
            &output.stderr,
        ));
    }
    // All operation files belong to this worktree's Git directory, including
    // in linked worktrees. Read one raw path: newlines can be part of its name.
    let directory =
        crate::git::raw_path(output.stdout.strip_suffix(b"\n").unwrap_or(&output.stdout));
    let [rebase_merge, rebase_apply, merge_head, todo, bisect_log, bisect_start] = [
        "rebase-merge",
        "rebase-apply",
        "MERGE_HEAD",
        "sequencer/todo",
        "BISECT_LOG",
        "BISECT_START",
    ]
    .map(|name| directory.join(name));
    // Unlike MERGE_HEAD, these may be stored as refs rather than files.
    let [cherry_pick_head, revert_head, rebase_head] =
        resolve(root, ["CHERRY_PICK_HEAD", "REVERT_HEAD", "REBASE_HEAD"])?;
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
        stopped: rebase_head.clone(),
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

/// Each commit hash's abbreviation as Git would show it, with its subject.
/// These only add detail, so hashes Git cannot show, or a failure, stay whole.
fn abbreviate(root: &Path, hashes: &[&str]) -> HashMap<String, (String, String)> {
    if hashes.is_empty() {
        return HashMap::new();
    }
    let Ok(output) = crate::git::read_command_in(root)
        .args(["log", "--no-walk=unsorted", "--no-show-signature"])
        // Skip missing objects, as in a shallow clone, and non-commits,
        // without fetching what a partial clone omits for this detail.
        .arg("--ignore-missing")
        .env("GIT_NO_LAZY_FETCH", "1")
        .arg("--format=%H %h %s")
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
        .filter_map(|line| {
            let (hash, rest) = line.split_once(' ')?;
            let (short, subject) = rest.split_once(' ').unwrap_or((rest, ""));
            Some((hash.to_owned(), (short.to_owned(), subject.to_owned())))
        })
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
    fn detects_operations_in_a_git_directory_containing_a_newline() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        let metadata = directory.path().join("meta\nrepo");
        git(&[
            "init",
            "-qb",
            "main",
            "--separate-git-dir",
            metadata.to_str().unwrap(),
            ".",
        ]);
        fs::write("file", "base\n").unwrap();
        git(&["add", "file"]);
        git(&["commit", "-qm", "base"]);
        git(&["checkout", "-qb", "feature"]);
        fs::write("file", "feature\n").unwrap();
        git(&["commit", "-qam", "feature"]);
        let merging = git(&["rev-parse", "HEAD"]);
        let short = git(&["rev-parse", "--short", "HEAD"]);
        git(&["checkout", "-q", "main"]);
        fs::write("file", "main\n").unwrap();
        git(&["commit", "-qam", "main"]);
        stop(&["merge", "feature"]);

        let state = load(directory.path(), None).unwrap();
        assert_eq!(
            state
                .operations
                .iter()
                .map(Text::to_string)
                .collect::<Vec<_>>(),
            [format!("Merging {short}")]
        );
        assert_eq!(state.commits.len(), 1);
        assert_eq!(state.commits[0].hash, merging);
    }

    #[test]
    fn abbreviates_the_commits_git_can_show_beside_missing_ones() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        git(&["commit", "--allow-empty", "-qm", "base"]);
        let head = git(&["rev-parse", "HEAD"]);
        let missing = "1".repeat(40);
        let abbreviated = abbreviate(Path::new("."), &[&missing, &head]);
        assert_eq!(
            abbreviated.get(&head),
            Some(&(git(&["rev-parse", "--short", "HEAD"]), "base".to_owned()))
        );
        assert!(!abbreviated.contains_key(&missing));
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
        let operations = || {
            let operations = load(root, None).unwrap().operations;
            operations.iter().map(Text::to_string).collect::<Vec<_>>()
        };
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
        let state = load(root, Some(&head)).unwrap();
        assert_eq!(state.detached_at, Some(short("HEAD")));
        assert_eq!(
            operations(),
            [format!("Rebasing feature onto {}, 1/1", short("main"))]
        );
        let [stopped] = &state.commits[..] else {
            panic!("{state:?}");
        };
        assert_eq!(stopped.hash, git(&["rev-parse", "feature"]));
        assert_eq!(
            stopped.label.to_string(),
            format!("Stopped at {} feature", short("feature"))
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
