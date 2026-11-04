//! Operations in progress in a worktree, such as a rebase or merge, read from
//! the state files `git status` reads to describe them.

use ratatui::text::Span;
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};

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

/// The operations in progress, before describing them takes Git processes.
#[derive(Clone, Default, PartialEq, Eq, Hash)]
struct Operations(Vec<Operation>);

#[derive(Clone, PartialEq, Eq, Hash)]
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

/// Reads the operations in progress in one worktree. Status and watched
/// Logs read them every second, so this keeps what takes Git processes to
/// read or describe until the operations change.
pub struct Reader {
    root: PathBuf,
    /// Boxed, as views hold readers they may never use.
    cache: Box<Mutex<Cache>>,
}

#[derive(Default)]
struct Cache {
    /// The Git directory holding the operations, once found.
    git_dir: Option<PathBuf>,
    /// A reftable's pseudo-refs, with the table list they were read from.
    pseudo_refs: Option<(Vec<u8>, PseudoRefs)>,
    /// The last operations described, with the detached HEAD they were
    /// described for.
    described: Option<(Operations, Option<String>, State)>,
}

type PseudoRefs = [Option<String>; 3];

/// A clone starts with empty caches.
impl Clone for Reader {
    fn clone(&self) -> Self {
        Self::new(self.root.clone())
    }
}

impl Reader {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            cache: Box::default(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The state of the worktree, whose HEAD is the commit `detached` when
    /// detached.
    pub fn state(&self, detached: Option<&str>) -> State {
        let mut cache = self.lock();
        let operations = self.operations(&mut cache);
        if let Some((described, at, state)) = &cache.described {
            if *described == operations && at.as_deref() == detached {
                return state.clone();
            }
        }
        let state = describe_state(&self.root, &operations, detached);
        cache.described = Some((operations, detached.map(str::to_owned), state.clone()));
        state
    }

    /// Hash what `state` would describe, without the Git processes
    /// describing takes.
    pub fn fingerprint(&self, state: &mut impl std::hash::Hasher) {
        let mut cache = self.lock();
        std::hash::Hash::hash(&self.operations(&mut cache), state);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Cache> {
        self.cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Operations only add detail to the changed files, so without a Git
    /// directory, none show rather than failing the view.
    fn operations(&self, cache: &mut Cache) -> Operations {
        if cache.git_dir.is_none() {
            cache.git_dir = git_dir(&self.root).ok();
        }
        let Some(directory) = cache.git_dir.clone() else {
            return Operations::default();
        };
        let pseudo_refs = self.pseudo_refs(&directory, &mut cache.pseudo_refs);
        Operations(operations(&directory, pseudo_refs))
    }

    /// The commits of the per-worktree refs CHERRY_PICK_HEAD, REVERT_HEAD
    /// and REBASE_HEAD. Unlike MERGE_HEAD, they are stored in the ref
    /// backend, which is files in `directory` unless it keeps a reftable.
    /// Reading a reftable takes Git, so its result is kept until its table
    /// list changes. Failing to read it loses only these refs, not the
    /// operations read from other files.
    fn pseudo_refs(
        &self,
        directory: &Path,
        cached: &mut Option<(Vec<u8>, PseudoRefs)>,
    ) -> PseudoRefs {
        let Ok(tables) = fs::read(directory.join("reftable/tables.list")) else {
            *cached = None;
            return PSEUDO_REFS
                .map(|name| read(&directory.join(name)).filter(|text| is_hash(text)));
        };
        match cached {
            Some((read, refs)) if *read == tables => refs.clone(),
            _ => {
                let refs = resolve(&self.root);
                // Keep only successful reads, retrying failures.
                *cached = refs.clone().ok().map(|refs| (tables, refs));
                refs.unwrap_or_default()
            }
        }
    }
}

const PSEUDO_REFS: [&str; 3] = ["CHERRY_PICK_HEAD", "REVERT_HEAD", "REBASE_HEAD"];

/// `operations` in the worktree at `root`, whose HEAD is the commit
/// `detached` when detached, as Status shows them.
fn describe_state(root: &Path, operations: &Operations, detached: Option<&str>) -> State {
    let Operations(operations) = operations;
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
    State {
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
    }
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

/// The Git directory of the worktree at `root`, which holds its operation
/// files, including in linked worktrees.
fn git_dir(root: &Path) -> Result<PathBuf, String> {
    absolute_git_dir(crate::git::read_command_in(root))
}

/// The Git directory of the repository glog was started in, honoring
/// `GIT_DIR` as given.
pub fn current_git_dir() -> Result<PathBuf, String> {
    absolute_git_dir(crate::git::read_command())
}

fn absolute_git_dir(mut command: std::process::Command) -> Result<PathBuf, String> {
    let output = command
        .args(["rev-parse", "--absolute-git-dir"])
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(crate::git::stderr_message(
            "git rev-parse failed",
            &output.stderr,
        ));
    }
    // Read one raw path: newlines can be part of its name.
    Ok(crate::git::raw_path(
        output.stdout.strip_suffix(b"\n").unwrap_or(&output.stdout),
    ))
}

/// The operations in progress in the Git directory `directory`, whose
/// `PSEUDO_REFS` point to `pseudo_refs`, in the order Status describes them.
fn operations(directory: &Path, pseudo_refs: PseudoRefs) -> Vec<Operation> {
    let [merge_head, todo, bisect_log, bisect_start] =
        ["MERGE_HEAD", "sequencer/todo", "BISECT_LOG", "BISECT_START"]
            .map(|name| directory.join(name));
    let [cherry_pick_head, revert_head, rebase_head] = pseudo_refs;
    let mut operations = Vec::new();
    let step = |state: &Path, interactive| {
        let (current, total) = match interactive {
            true => ("msgnum", "end"),
            false => ("next", "last"),
        };
        Some(format!(
            "{}/{}",
            read(&state.join(current))?,
            read(&state.join(total))?
        ))
    };
    let rebase = |state: &Path, interactive| Operation::Rebase {
        branch: read(&state.join("head-name"))
            .and_then(|name| name.strip_prefix("refs/heads/").map(str::to_owned)),
        onto: read(&state.join("onto")),
        step: step(state, interactive),
        stopped: rebase_head.clone(),
    };
    let merge = read(&merge_head).map(|heads| Operation::Merge {
        heads: heads.lines().map(str::to_owned).collect(),
    });
    // Git's precedence: a merge hides the other operations, except an
    // interactive rebase, which stops on the merges it recreates.
    match (replay(directory), merge) {
        (
            Some(Replay::Rebase {
                state,
                interactive: true,
            }),
            merge,
        ) => {
            operations.push(rebase(&state, true));
            operations.extend(merge);
        }
        (_, Some(merge)) => operations.push(merge),
        (Some(Replay::Rebase { state, interactive }), None) => {
            operations.push(rebase(&state, interactive));
        }
        (Some(Replay::Am { state }), None) => operations.push(Operation::Am {
            step: step(&state, false),
        }),
        (None, None) => sequence(&mut operations, cherry_pick_head, revert_head, &todo),
    }
    if bisect_log.exists() {
        if let Some(start) = read(&bisect_start) {
            operations.push(Operation::Bisect { start });
        }
    }
    operations
}

/// A cherry-pick or revert, stopped at a commit or between them.
fn sequence(
    operations: &mut Vec<Operation>,
    cherry_pick_head: Option<String>,
    revert_head: Option<String>,
    todo: &Path,
) {
    if let Some(commit) = cherry_pick_head {
        operations.push(Operation::CherryPick {
            commit: Some(commit),
        });
    } else if let Some(commit) = revert_head {
        operations.push(Operation::Revert {
            commit: Some(commit),
        });
    } else if let Some(todo) = read(todo) {
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
}

/// A rebase or `git am`, which share their state directories.
enum Replay {
    /// `interactive` rebases, including merge-backend ones without `-i`,
    /// keep their state in `rebase-merge`, the others in `rebase-apply`.
    Rebase {
        state: PathBuf,
        interactive: bool,
    },
    Am {
        state: PathBuf,
    },
}

/// The rebase or `git am` in progress in the Git directory `directory`.
fn replay(directory: &Path) -> Option<Replay> {
    let merge = directory.join("rebase-merge");
    if merge.is_dir() {
        return Some(Replay::Rebase {
            state: merge,
            interactive: true,
        });
    }
    let apply = directory.join("rebase-apply");
    if !apply.is_dir() {
        return None;
    }
    // Rebases mark their state with `rebasing`. `git am` marks it with
    // `applying`, but older versions left no marker.
    Some(match apply.join("rebasing").exists() {
        true => Replay::Rebase {
            state: apply,
            interactive: false,
        },
        false => Replay::Am { state: apply },
    })
}

/// Whether the repository glog was started in has a rebase in progress that
/// the reflog already records. Failing to tell only keeps ongoing amendments
/// folded in Reflog, so it does not fail it.
pub fn rebase_logged() -> bool {
    let Ok(directory) = current_git_dir() else {
        return false;
    };
    match replay(&directory) {
        // Git records each todo command in `done` before running it, and
        // logs the rebase's start first. Without it, e.g. while the todo is
        // edited, no reflog entry belongs to the rebase yet.
        Some(Replay::Rebase {
            state,
            interactive: true,
        }) => state.join("done").exists(),
        Some(Replay::Rebase { .. }) => true,
        Some(Replay::Am { .. }) | None => false,
    }
}

/// `PSEUDO_REFS` as a reftable in the worktree at `root` stores them. Name
/// them exactly, as resolving a revision would fall back to branches named
/// like them. Git that can create a reftable lists them.
fn resolve(root: &Path) -> Result<PseudoRefs, String> {
    let output = crate::git::read_command_in(root)
        .args([
            "for-each-ref",
            "--include-root-refs",
            "--format=%(refname) %(objectname)",
        ])
        .args(PSEUDO_REFS)
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(crate::git::stderr_message(
            "git for-each-ref failed",
            &output.stderr,
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let refs: HashMap<_, _> = stdout
        .lines()
        .filter_map(|line| line.split_once(' '))
        .filter(|(_, hash)| is_hash(hash))
        .collect();
    Ok(PSEUDO_REFS.map(|name| refs.get(name).map(|hash| (*hash).to_owned())))
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
    use crate::git::tests::{fail, git, git_command, CurrentDirGuard, TestDirectory};

    /// Run Git expecting it to stop, as on a conflict.
    fn stop(args: &[&str]) {
        fail(&mut git_command(args));
    }

    #[test]
    fn reflog_detects_a_rebase_in_a_repository_selected_by_git_dir() {
        // GIT_DIR would redirect Git in concurrently running tests, so set it
        // only for a child process running just this test.
        const CHILD: &str = "GLOG_TEST_REBASE_GIT_DIR_CHILD";
        if std::env::var_os(CHILD).is_some() {
            assert!(rebase_logged());
            return;
        }
        let directory = TestDirectory::new();
        let git_dir = directory.path().join("repo.git");
        let elsewhere = directory.path().join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();
        crate::git::tests::succeed(
            git_command(&["init", "-q", "--bare"])
                .arg(&git_dir)
                .current_dir(directory.path()),
        );
        fs::create_dir(git_dir.join("rebase-apply")).unwrap();
        fs::write(git_dir.join("rebase-apply/rebasing"), "").unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "operation::tests::reflog_detects_a_rebase_in_a_repository_selected_by_git_dir",
            ])
            .current_dir(&elsewhere)
            .env(CHILD, "1")
            .env("GIT_DIR", &git_dir)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{stdout}");
        assert!(stdout.contains("1 passed"), "{stdout}");
    }

    // Windows file names cannot contain newlines.
    #[cfg(not(windows))]
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

        let state = Reader::new(directory.path().to_owned()).state(None);
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
    fn fingerprints_change_with_operations_alone() {
        use std::hash::Hasher;
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        git(&["commit", "--allow-empty", "-qm", "base"]);
        let reader = Reader::new(".".into());
        let hash = || {
            let mut state = std::collections::hash_map::DefaultHasher::new();
            reader.fingerprint(&mut state);
            state.finish()
        };
        let before = hash();
        git(&["bisect", "start"]);
        assert_ne!(hash(), before);
        git(&["bisect", "reset"]);
        assert_eq!(hash(), before);
    }

    #[test]
    fn describes_operations_from_real_git_state() {
        describe_operations(&[]);
        // Reftable keeps pseudo-refs such as CHERRY_PICK_HEAD out of files.
        // Git before 2.45 cannot create such repositories.
        let supported = git_command(&["init", "-q", "--ref-format=reftable"])
            .arg(TestDirectory::new().path())
            .output()
            .unwrap()
            .status
            .success();
        if supported {
            describe_operations(&["--ref-format=reftable"]);
        }
    }

    #[test]
    fn failing_to_read_reftable_pseudo_refs_keeps_other_operations() {
        let directory = TestDirectory::new();
        let repo = directory.path().join("repo");
        // Git fails here, unable to find the repository.
        let elsewhere = directory.path().join("elsewhere");
        fs::create_dir(&elsewhere).unwrap();
        let created = git_command(&["init", "-q", "-b", "main", "--ref-format=reftable"])
            .arg(&repo)
            .output()
            .unwrap();
        // Git before 2.45 cannot create reftable repositories.
        if !created.status.success() {
            return;
        }
        let _cwd = CurrentDirGuard::enter(&repo);
        git(&["commit", "--allow-empty", "-qm", "base"]);
        git(&["bisect", "start"]);
        let reader = Reader::new(elsewhere);
        let mut cache = reader.lock();
        cache.git_dir = Some(git_dir(&repo).unwrap());
        let Operations(operations) = reader.operations(&mut cache);
        assert_eq!(
            operations
                .iter()
                .map(|operation| describe(operation).to_string())
                .collect::<Vec<_>>(),
            ["Bisecting from main"]
        );
    }

    fn describe_operations(init: &[&str]) {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        // One reader throughout, as Status reads every second.
        let reader = Reader::new(".".into());
        let operations = || {
            let operations = reader.state(None).operations;
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
        let state = reader.state(Some(&head));
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

        // A rebase that recreates a merge stops on its conflict.
        git(&["checkout", "-qb", "side", "main"]);
        fs::write("other", "side\n").unwrap();
        git(&["add", "other"]);
        git(&["commit", "-qm", "side"]);
        git(&["checkout", "-qb", "topic", "main"]);
        fs::write("other", "topic\n").unwrap();
        git(&["add", "other"]);
        git(&["commit", "-qm", "topic"]);
        stop(&["merge", "side"]);
        fs::write("other", "merged\n").unwrap();
        git(&["commit", "-qam", "merged"]);
        git(&["checkout", "-q", "main"]);
        git(&["commit", "--allow-empty", "-qm", "later"]);
        git(&["checkout", "-q", "topic"]);
        stop(&["rebase", "--rebase-merges", "main"]);
        let [rebase, merge] = &operations()[..] else {
            panic!("{:?}", operations());
        };
        let rebasing = format!("Rebasing topic onto {}", short("main"));
        assert!(rebase.starts_with(&rebasing), "{rebase}");
        assert_eq!(merge, &format!("Merging {}", short("MERGE_HEAD")));
        git(&["rebase", "--abort"]);
        git(&["checkout", "-q", "main"]);

        git(&["bisect", "start"]);
        assert_eq!(operations(), ["Bisecting from main"]);
        git(&["bisect", "reset"]);

        // Branches named like pseudo-refs are not operations.
        for name in PSEUDO_REFS {
            git(&["branch", name]);
        }
        assert_eq!(operations(), Vec::<String>::new());
        assert!(reader.state(None).commits.is_empty());
    }
}
