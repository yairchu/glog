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
        let file_type = metadata.file_type();
        fingerprint.write_u64(metadata.len());
        fingerprint.write_u8(u8::from(file_type.is_file()));
        fingerprint.write_u8(u8::from(file_type.is_dir()));
        fingerprint.write_u8(u8::from(file_type.is_symlink()));
        fingerprint.write_u8(u8::from(metadata.permissions().readonly()));
        let modified = metadata
            .modified()
            .map_err(|error| format!("could not inspect untracked file {path}: {error}"))?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| format!("invalid modification time for {path}: {error}"))?;
        fingerprint.write_u64(modified.as_secs());
        fingerprint.write_u32(modified.subsec_nanos());
        if file_type.is_symlink() {
            let target = fs::read_link(&path)
                .map_err(|error| format!("could not read untracked symlink {path}: {error}"))?;
            fingerprint.write(target.to_string_lossy().as_bytes());
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
    let output = Command::new("git")
        .args(["diff", "--color=always", "--no-ext-diff"])
        .output()
        .map_err(|error| format!("could not run git diff: {error}"))?;
    if !output.status.success() {
        return Err(stderr_message("git diff failed", &output.stderr));
    }
    let mut formatted = format_output(output.stdout)?;
    for path in untracked_files()? {
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("could not inspect untracked file {path}: {error}"))?;
        let old_path = git_quote_path(&format!("a/{path}"));
        let new_path = git_quote_path(&format!("b/{path}"));
        let mode = file_mode(&metadata);
        formatted.push_str(&format!(
            "\x1b[1mdiff --git {old_path} {new_path}\x1b[m\n\x1b[1mnew file mode {mode:o}\x1b[m\nglog-lazy-untracked:{}\n",
            hex_encode(path.as_bytes())
        ));
    }
    Ok(formatted)
}

pub fn show_untracked(path: &str) -> Result<String, String> {
    let mut output = Vec::new();
    if !untracked_regular_file_diff(path, &mut output)? {
        let untracked = Command::new("git")
            .args([
                "--no-pager",
                "diff",
                "--no-index",
                "--color=always",
                "--no-ext-diff",
                "--",
                NULL_DEVICE,
                path,
            ])
            .output()
            .map_err(|error| format!("could not diff untracked file {path}: {error}"))?;
        if !matches!(untracked.status.code(), Some(0 | 1)) {
            return Err(stderr_message("git diff failed", &untracked.stderr));
        }
        output = untracked.stdout;
    }
    format_output(output)
}

fn untracked_regular_file_diff(path: &str, output: &mut Vec<u8>) -> Result<bool, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("could not inspect untracked file {path}: {error}"))?;
    if !metadata.file_type().is_file() {
        return Ok(false);
    }

    // Generate the simple new-file patch directly. Spawning `git diff
    // --no-index` once per untracked file is painfully slow for generated
    // trees containing thousands of files, even when every binary file only
    // contributes a three-line notice.
    let mut file = fs::File::open(path)
        .map_err(|error| format!("could not read untracked file {path}: {error}"))?;
    let mut contents = Vec::with_capacity(8_000);
    Read::by_ref(&mut file)
        .take(8_000)
        .read_to_end(&mut contents)
        .map_err(|error| format!("could not read untracked file {path}: {error}"))?;

    let old_path = git_quote_path(&format!("a/{path}"));
    let new_path = git_quote_path(&format!("b/{path}"));
    let mode = file_mode(&metadata);
    output.extend_from_slice(
        format!(
            "\x1b[1mdiff --git {old_path} {new_path}\x1b[m\n\x1b[1mnew file mode {mode:o}\x1b[m\n"
        )
        .as_bytes(),
    );
    if contents.contains(&0) {
        output.extend_from_slice(
            format!("\x1b[1mBinary files /dev/null and {new_path} differ\x1b[m\n").as_bytes(),
        );
        return Ok(true);
    }

    file.read_to_end(&mut contents)
        .map_err(|error| format!("could not read untracked file {path}: {error}"))?;

    let line_count = contents.iter().filter(|byte| **byte == b'\n').count()
        + usize::from(!contents.is_empty() && contents.last() != Some(&b'\n'));
    if contents.is_empty() {
        return Ok(true);
    }
    output.extend_from_slice(
        format!(
            "\x1b[1m--- /dev/null\x1b[m\n\x1b[1m+++ {new_path}\x1b[m\n\x1b[36m@@ -0,0 +1,{line_count} @@\x1b[m\n"
        )
        .as_bytes(),
    );
    for line in contents.split_inclusive(|byte| *byte == b'\n') {
        output.extend_from_slice(b"\x1b[32m+");
        output.extend_from_slice(line);
        output.extend_from_slice(b"\x1b[m");
        if line.last() != Some(&b'\n') {
            output.extend_from_slice(b"\n\x1b[1m\\ No newline at end of file\x1b[m\n");
        }
    }
    Ok(true)
}

fn git_quote_path(path: &str) -> String {
    if path
        .bytes()
        .all(|byte| (b' '..=b'~').contains(&byte) && byte != b'"' && byte != b'\\')
    {
        return path.to_owned();
    }
    let mut quoted = String::from("\"");
    for byte in path.bytes() {
        match byte {
            b'"' => quoted.push_str("\\\""),
            b'\\' => quoted.push_str("\\\\"),
            b'\n' => quoted.push_str("\\n"),
            b'\r' => quoted.push_str("\\r"),
            b'\t' => quoted.push_str("\\t"),
            b' '..=b'~' => quoted.push(byte as char),
            _ => quoted.push_str(&format!("\\{byte:03o}")),
        }
    }
    quoted.push('"');
    quoted
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[usize::from(byte >> 4)] as char);
        encoded.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    encoded
}

#[cfg(unix)]
fn file_mode(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    if metadata.file_type().is_symlink() {
        0o120000
    } else if metadata.permissions().mode() & 0o111 == 0 {
        0o100644
    } else {
        0o100755
    }
}

#[cfg(not(unix))]
fn file_mode(_metadata: &fs::Metadata) -> u32 {
    0o100644
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
    pipe_through(
        Command::new("delta").args(["--paging=never", "--color-only"]),
        input,
    )
}

fn pipe_through(command: &mut Command, input: &[u8]) -> Option<String> {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdin = child.stdin.take()?;
    let output = std::thread::scope(|scope| {
        let writer = scope.spawn(move || stdin.write_all(input));
        let output = child.wait_with_output().ok()?;
        writer.join().ok()?.ok()?;
        Some(output)
    })?;
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
        path::{Path, PathBuf},
        sync::{atomic::AtomicUsize, atomic::Ordering, Mutex, MutexGuard},
    };

    static CURRENT_DIR_LOCK: Mutex<()> = Mutex::new(());
    static NEXT_TEST_DIRECTORY: AtomicUsize = AtomicUsize::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let sequence = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let path = env::temp_dir().join(format!("glog-test-{}-{sequence}", std::process::id()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

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

    #[cfg(unix)]
    #[test]
    fn pipes_input_larger_than_the_pipe_buffers() {
        let input = b"x\n".repeat(512 * 1024);
        let input_len = input.len();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(pipe_through(&mut Command::new("cat"), &input));
        });
        let output = receiver
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("pipe_through deadlocked");
        assert_eq!(output.unwrap().len(), input_len);
    }

    #[test]
    fn formats_untracked_binary_without_running_git_diff() {
        let repository = TestDirectory::new();
        fs::write(repository.path().join("generated.bin"), b"header\0payload").unwrap();
        let _current_dir = CurrentDirGuard::enter(repository.path());
        let mut output = Vec::new();

        assert!(untracked_regular_file_diff("generated.bin", &mut output).unwrap());
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("diff --git a/generated.bin b/generated.bin"));
        assert!(output.contains("new file mode 100644"));
        assert!(output.contains("Binary files /dev/null and b/generated.bin differ"));
    }

    #[test]
    fn loads_log_and_show_from_a_repository() {
        let repository = TestDirectory::new();
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
        fs::write(repository.path().join("new.txt"), "UNTRACKED LONGER\n").unwrap();
        assert_ne!(dirty_fingerprint, watch_fingerprint().unwrap());
        let commits = load_log(&[]).unwrap();
        assert_eq!(commits[0].kind, CommitKind::Unstaged);
        assert_eq!(commits[1].kind, CommitKind::Staged);
        assert_eq!(commits[2].kind, CommitKind::Revision);
        assert!(show(&commits[0]).unwrap().contains("glog-lazy-untracked:"));
        assert!(show_untracked("new.txt")
            .unwrap()
            .contains("UNTRACKED LONGER"));
        assert!(show(&commits[1]).unwrap().contains("staged"));

        let mut app = crate::app::App::new(commits.clone());
        app.switch_mode();
        assert!(!app.show_text.contains("UNTRACKED"));
        app.show_cursor = app
            .show_rows
            .iter()
            .position(|row| row.folded && row.text.contains("new.txt"))
            .unwrap();
        app.toggle_show_file();
        assert!(app.show_text.contains("UNTRACKED"));

        let explicit = load_log(&["HEAD".to_owned()]).unwrap();
        assert_eq!(explicit.len(), 1);
        assert_eq!(explicit[0].kind, CommitKind::Revision);
    }
}
