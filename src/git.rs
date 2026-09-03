use std::{
    collections::hash_map::DefaultHasher,
    env, fs,
    hash::Hasher,
    io::{Read, Write},
    process::{Command, Stdio},
};

const RECORD: char = '\x1e';
const FIELD: char = '\x1f';

#[cfg(windows)]
const NULL_DEVICE: &str = "NUL";
#[cfg(not(windows))]
const NULL_DEVICE: &str = "/dev/null";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    pub kind: CommitKind,
    pub hash: String,
    pub short_hash: String,
    pub decorations: String,
    pub subject: String,
    pub graph: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitKind {
    Revision,
    Staged,
    Unstaged,
}

pub fn load_log(user_args: &[String]) -> Result<Vec<Commit>, String> {
    let mut command = Command::new("git");
    command.env("LC_ALL", "C");
    command.args(["--no-pager", "log"]);
    let separator = user_args
        .iter()
        .position(|arg| arg == "--")
        .unwrap_or(user_args.len());
    command.args(&user_args[..separator]);
    command.args([
        "--graph",
        "--decorate=short",
        "--color=always",
        "--no-abbrev-commit",
        "--pretty=format:%x1e%H%x1f%h%x1f%D%x1f%s",
    ]);
    command.args(&user_args[separator..]);
    let output = command.output().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            "git executable not found".to_owned()
        } else {
            format!("could not run git log: {error}")
        }
    })?;
    let mut commits = if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("does not have any commits yet")
            || stderr.contains("your current branch appears to be broken")
        {
            Vec::new()
        } else {
            return Err(stderr_message("git log failed", &output.stderr));
        }
    } else {
        parse_log(&String::from_utf8_lossy(&output.stdout))
    };
    if user_args.is_empty() {
        let mut working_tree = working_tree_entries()?;
        working_tree.append(&mut commits);
        commits = working_tree;
    }
    Ok(commits)
}

pub fn watch_fingerprint() -> Result<u64, String> {
    let mut fingerprint = DefaultHasher::new();
    hash_command(
        &mut fingerprint,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        false,
    )?;
    hash_command(
        &mut fingerprint,
        &["diff", "--binary", "--no-ext-diff"],
        false,
    )?;
    hash_command(
        &mut fingerprint,
        &["diff", "--cached", "--binary", "--no-ext-diff"],
        false,
    )?;
    hash_command(&mut fingerprint, &["rev-parse", "--verify", "HEAD"], true)?;
    hash_command(
        &mut fingerprint,
        &["show-ref", "--head", "--dereference"],
        true,
    )?;
    for path in untracked_files()? {
        fingerprint.write(path.as_bytes());
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("could not inspect untracked file {path}: {error}"))?;
        if metadata.file_type().is_symlink() {
            let target = fs::read_link(&path)
                .map_err(|error| format!("could not read untracked symlink {path}: {error}"))?;
            fingerprint.write(target.to_string_lossy().as_bytes());
        } else {
            let mut file = fs::File::open(&path)
                .map_err(|error| format!("could not read untracked file {path}: {error}"))?;
            let mut buffer = [0; 16 * 1024];
            loop {
                let read = file
                    .read(&mut buffer)
                    .map_err(|error| format!("could not read untracked file {path}: {error}"))?;
                if read == 0 {
                    break;
                }
                fingerprint.write(&buffer[..read]);
            }
        }
    }
    Ok(fingerprint.finish())
}

fn hash_command(
    fingerprint: &mut impl Hasher,
    args: &[&str],
    allow_failure: bool,
) -> Result<(), String> {
    let output = Command::new("git")
        .args(args)
        .output()
        .map_err(|error| format!("could not inspect repository: {error}"))?;
    if !output.status.success() && !allow_failure {
        return Err(stderr_message(
            "git repository inspection failed",
            &output.stderr,
        ));
    }
    fingerprint.write(&output.stdout);
    fingerprint.write(&output.stderr);
    Ok(())
}

fn parse_log(output: &str) -> Vec<Commit> {
    let mut pending_graph = Vec::new();
    let mut commits = Vec::new();
    for line in output.lines() {
        if let Some(marker) = line.find(RECORD) {
            let fields: Vec<_> = line[marker + 1..].splitn(4, FIELD).collect();
            if fields.len() == 4 {
                pending_graph.push(line[..marker].to_owned());
                commits.push(Commit {
                    kind: CommitKind::Revision,
                    hash: fields[0].to_owned(),
                    short_hash: fields[1].to_owned(),
                    decorations: fields[2].to_owned(),
                    subject: fields[3].to_owned(),
                    graph: std::mem::take(&mut pending_graph),
                });
            }
        } else if !line.is_empty() {
            pending_graph.push(line.to_owned());
        }
    }
    commits
}

fn working_tree_entries() -> Result<Vec<Commit>, String> {
    let unstaged =
        has_diff(&["diff", "--quiet", "--no-ext-diff"])? || !untracked_files()?.is_empty();
    let staged = has_diff(&["diff", "--cached", "--quiet", "--no-ext-diff"])?;
    let mut entries = Vec::new();
    if unstaged {
        entries.push(pseudo_commit(
            CommitKind::Unstaged,
            "worktree",
            "Unstaged changes",
        ));
    }
    if staged {
        entries.push(pseudo_commit(CommitKind::Staged, "index", "Staged changes"));
    }
    Ok(entries)
}

fn pseudo_commit(kind: CommitKind, short_hash: &str, subject: &str) -> Commit {
    Commit {
        kind,
        hash: format!("[{short_hash}]"),
        short_hash: short_hash.to_owned(),
        decorations: String::new(),
        subject: subject.to_owned(),
        graph: vec!["* ".to_owned()],
    }
}

fn has_diff(args: &[&str]) -> Result<bool, String> {
    let status = Command::new("git")
        .args(args)
        .status()
        .map_err(|error| format!("could not inspect working tree: {error}"))?;
    match status.code() {
        Some(0) => Ok(false),
        Some(1) => Ok(true),
        _ => Err("git diff failed while inspecting working tree".to_owned()),
    }
}

pub fn show(commit: &Commit) -> Result<String, String> {
    match commit.kind {
        CommitKind::Revision => show_revision(&commit.hash),
        CommitKind::Staged => show_diff(&["diff", "--cached", "--color=always", "--no-ext-diff"]),
        CommitKind::Unstaged => show_unstaged(),
    }
}

fn show_revision(hash: &str) -> Result<String, String> {
    let output = Command::new("git")
        .args([
            "--no-pager",
            "show",
            "--color=always",
            "--no-ext-diff",
            hash,
        ])
        .env("GIT_PAGER", "cat")
        .output()
        .map_err(|error| format!("could not run git show: {error}"))?;
    if !output.status.success() {
        return Err(stderr_message("git show failed", &output.stderr));
    }
    format_output(output.stdout)
}

fn show_diff(args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .env("GIT_PAGER", "cat")
        .output()
        .map_err(|error| format!("could not run git diff: {error}"))?;
    if !output.status.success() {
        return Err(stderr_message("git diff failed", &output.stderr));
    }
    format_output(output.stdout)
}

fn show_unstaged() -> Result<String, String> {
    let mut output = Command::new("git")
        .args(["diff", "--color=always", "--no-ext-diff"])
        .output()
        .map_err(|error| format!("could not run git diff: {error}"))?;
    if !output.status.success() {
        return Err(stderr_message("git diff failed", &output.stderr));
    }
    for path in untracked_files()? {
        let untracked = Command::new("git")
            .args([
                "--no-pager",
                "diff",
                "--no-index",
                "--color=always",
                "--no-ext-diff",
                "--",
                NULL_DEVICE,
                &path,
            ])
            .output()
            .map_err(|error| format!("could not diff untracked file {path}: {error}"))?;
        if !matches!(untracked.status.code(), Some(0 | 1)) {
            return Err(stderr_message("git diff failed", &untracked.stderr));
        }
        output.stdout.extend_from_slice(&untracked.stdout);
    }
    format_output(output.stdout)
}

fn untracked_files() -> Result<Vec<String>, String> {
    let output = Command::new("git")
        .args(["ls-files", "--others", "--exclude-standard", "-z"])
        .output()
        .map_err(|error| format!("could not list untracked files: {error}"))?;
    if !output.status.success() {
        return Err(stderr_message("git ls-files failed", &output.stderr));
    }
    Ok(output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect())
}

fn format_output(output: Vec<u8>) -> Result<String, String> {
    let plain = String::from_utf8_lossy(&output).into_owned();
    if delta_enabled() {
        if let Some(formatted) = run_delta(&output) {
            return Ok(formatted);
        }
    }
    Ok(plain)
}

fn delta_enabled() -> bool {
    !matches!(
        env::var("GLOG_DELTA").as_deref(),
        Ok("0" | "false" | "no" | "off")
    )
}

fn run_delta(input: &[u8]) -> Option<String> {
    let mut child = Command::new("delta")
        .args(["--paging=never", "--color-only"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child.stdin.take()?.write_all(input).ok()?;
    let output = child.wait_with_output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

fn stderr_message(prefix: &str, stderr: &[u8]) -> String {
    let detail = String::from_utf8_lossy(stderr);
    let detail = detail.trim();
    if detail.is_empty() {
        prefix.to_owned()
    } else {
        format!("{prefix}: {detail}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        path::Path,
        sync::{Mutex, MutexGuard},
    };

    static CURRENT_DIR_LOCK: Mutex<()> = Mutex::new(());

    struct CurrentDirGuard {
        original: std::path::PathBuf,
        _lock: MutexGuard<'static, ()>,
    }

    impl CurrentDirGuard {
        fn enter(path: &Path) -> Self {
            let lock = CURRENT_DIR_LOCK.lock().unwrap();
            let original = env::current_dir().unwrap();
            env::set_current_dir(path).unwrap();
            Self {
                original,
                _lock: lock,
            }
        }
    }

    impl Drop for CurrentDirGuard {
        fn drop(&mut self) {
            env::set_current_dir(&self.original).unwrap();
        }
    }

    #[test]
    fn parses_full_identity_and_graph_lines() {
        let input = "|\\\n* \u{1e}abcdef\u{1f}abcdef0\u{1f}HEAD -> main\u{1f}hello\n| * \u{1e}123456\u{1f}1234567\u{1f}\u{1f}world\n";
        let commits = parse_log(input);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].hash, "abcdef");
        assert_eq!(commits[0].graph, ["|\\", "* "]);
        assert_eq!(commits[1].subject, "world");
    }

    #[test]
    fn parses_empty_output() {
        assert!(parse_log("").is_empty());
    }

    #[test]
    fn loads_log_and_show_from_a_repository() {
        let repository = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .args(args)
                .current_dir(repository.path())
                .status()
                .unwrap();
            assert!(status.success());
        };
        git(&["init", "-q"]);
        git(&["config", "user.name", "Test"]);
        git(&["config", "user.email", "test@example.com"]);
        fs::write(repository.path().join("note.txt"), "hello\n").unwrap();
        git(&["add", "note.txt"]);
        git(&["commit", "-qm", "first subject"]);

        let _current_dir = CurrentDirGuard::enter(repository.path());
        let commits = load_log(&[]).unwrap();
        let shown = show(&commits[0]).unwrap();
        let clean_fingerprint = watch_fingerprint().unwrap();

        git(&["branch", "watch-test"]);
        let ref_fingerprint = watch_fingerprint().unwrap();
        assert_ne!(clean_fingerprint, ref_fingerprint);

        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].subject, "first subject");
        assert_eq!(commits[0].hash.len(), 40);
        assert!(shown.contains("note.txt"));
        assert!(shown.contains("hello"));

        fs::write(repository.path().join("note.txt"), "staged\n").unwrap();
        git(&["add", "note.txt"]);
        fs::write(repository.path().join("note.txt"), "unstaged\n").unwrap();
        fs::write(repository.path().join("new.txt"), "untracked\n").unwrap();

        let dirty_fingerprint = watch_fingerprint().unwrap();
        assert_ne!(ref_fingerprint, dirty_fingerprint);
        fs::write(repository.path().join("new.txt"), "UNTRACKED\n").unwrap();
        assert_ne!(dirty_fingerprint, watch_fingerprint().unwrap());
        let commits = load_log(&[]).unwrap();
        assert_eq!(commits[0].kind, CommitKind::Unstaged);
        assert_eq!(commits[1].kind, CommitKind::Staged);
        assert_eq!(commits[2].kind, CommitKind::Revision);
        assert!(show(&commits[0]).unwrap().contains("UNTRACKED"));
        assert!(show(&commits[1]).unwrap().contains("staged"));

        let explicit = load_log(&["HEAD".to_owned()]).unwrap();
        assert_eq!(explicit.len(), 1);
        assert_eq!(explicit[0].kind, CommitKind::Revision);
    }
}
