use std::{
    collections::{hash_map::DefaultHasher, HashSet},
    env, fs,
    hash::Hasher,
    io::{Read, Write},
    process::{Command, Stdio},
};

const RECORD: char = '\x1e';
const FIELD: char = '\x1f';
const COAUTHOR: char = '\x1d';

// Patch paths are parsed relative to the repository, independent of user
// preferences such as diff.relative, diff.noprefix and diff.mnemonicPrefix.
pub(crate) const DIFF_PREFIX_ARGS: [&str; 3] =
    ["--no-relative", "--src-prefix=a/", "--dst-prefix=b/"];

pub(crate) fn patch_command() -> Command {
    let mut command = Command::new("git");
    // Patches pass through UTF-8 text (and optionally delta) before parsing.
    // Quote non-ASCII path bytes so this conversion cannot lose file identities.
    command.args(["-c", "core.quotePath=true"]);
    command
}

#[cfg(windows)]
const NULL_DEVICE: &str = "NUL";
#[cfg(not(windows))]
const NULL_DEVICE: &str = "/dev/null";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    pub kind: CommitKind,
    pub diff_args: Vec<String>,
    pub hash: String,
    pub short_hash: String,
    pub decorations: String,
    pub subject: String,
    pub author: String,
    pub author_email: String,
    pub author_date: String,
    pub collaborators: Collaborators,
    pub graph: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Collaborators {
    pub codex: bool,
    pub claude: bool,
    pub others: usize,
}

impl Collaborators {
    fn parse(trailers: &str, author_email: &str) -> Self {
        let mut result = Self::default();
        let mut seen = HashSet::new();
        seen.insert(author_email.trim().to_ascii_lowercase());
        for trailer in trailers.split(COAUTHOR) {
            let Some((name, email)) = trailer.trim().rsplit_once('<') else {
                continue;
            };
            let Some(email) = email.strip_suffix('>') else {
                continue;
            };
            let email = email.trim().to_ascii_lowercase();
            if name.trim().is_empty() || !email.contains('@') || !seen.insert(email.clone()) {
                continue;
            }
            match email.as_str() {
                "codex@openai.com" | "noreply@openai.com" => result.codex = true,
                "noreply@anthropic.com" => result.claude = true,
                _ => result.others += 1,
            }
        }
        result
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommitKind {
    WorkingTree,
    Revision,
    Staged,
    Unstaged,
    Comparison { worktree: bool },
}

fn log_command(user_args: &[String]) -> Result<Command, String> {
    let configured_date = Command::new("git")
        .args(["config", "--get", "log.date"])
        .output()
        .map_err(|error| format!("could not read Git date configuration: {error}"))?;
    let mut command = Command::new("git");
    command.env("LC_ALL", "C");
    // Supply a configuration fallback rather than a --date argument, so Git
    // retains its own precedence for --date and --relative-date.
    match configured_date.status.code() {
        Some(0) => {}
        Some(1) => {
            command.args(["-c", "log.date=format-local:%Y-%m-%d %H:%M"]);
        }
        _ => {
            return Err(stderr_message(
                "could not read Git date configuration",
                &configured_date.stderr,
            ))
        }
    }
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
        "--pretty=format:%x1e%H%x1f%h%x1f%D%x1f%an%x1f%ae%x1f%ad%x1f%(trailers:key=Co-authored-by,valueonly,unfold,separator=%x1d)%x1f%s",
    ]);
    command.args(&user_args[separator..]);
    Ok(command)
}

pub fn load_log(user_args: &[String]) -> Result<Vec<Commit>, String> {
    let output = log_command(user_args)?.output().map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            "git executable not found".to_owned()
        } else {
            format!("could not run git log: {error}")
        }
    })?;
    let commits = if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("does not have any commits yet")
            || stderr.contains("your current branch appears to be broken")
        {
            Vec::new()
        } else {
            return Err(stderr_message("git log failed", &output.stderr));
        }
    } else {
        parse_log(&String::from_utf8_lossy(&output.stdout))?
    };
    Ok(commits)
}

/// Watch mode includes one stable working-tree item ahead of committed history.
pub fn load_watch_log() -> Result<Vec<Commit>, String> {
    let commits = load_log(&[])?;
    let summary = crate::status::working_tree_summary()?;
    let mut entries = vec![working_tree_commit(&summary)];
    entries.extend(commits);
    Ok(entries)
}

pub fn working_tree_commit(summary: &str) -> Commit {
    pseudo_commit(
        CommitKind::WorkingTree,
        "worktree",
        &format!("Working tree · {summary}"),
    )
}

/// Resolve a single commit before opening the terminal; defer ancestry traversal.
pub fn load_show_app(args: &[String]) -> Result<crate::app::App, String> {
    let separator = args
        .iter()
        .position(|arg| arg == "--")
        .unwrap_or(args.len());
    let stat = args[..separator].iter().any(|arg| arg == "--stat");
    let revisions: Vec<_> = args[..separator]
        .iter()
        .filter(|arg| *arg != "--stat")
        .collect();
    let revision = match revisions.as_slice() {
        [] => "HEAD",
        [revision] if !revision.starts_with('-') => revision.as_str(),
        _ => return Err("usage: glog show [--stat] [commit] [-- pathspec...]".to_owned()),
    };
    let output = Command::new("git")
        .args([
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{revision}^{{commit}}"),
        ])
        .output()
        .map_err(|error| format!("could not resolve commit: {error}"))?;
    if !output.status.success() {
        return Err(stderr_message("could not resolve commit", &output.stderr));
    }
    let hash = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let commits = load_log(&["-1".to_owned(), hash.clone(), "--".to_owned()])?;
    let mut app = crate::app::App::new(commits);
    app.show_stat = stat;
    app.pending_history = Some(if revisions.is_empty() {
        Vec::new()
    } else {
        vec![hash, "--".to_owned()]
    });
    app.show_paths = args.get(separator + 1..).unwrap_or_default().to_vec();
    app.switch_mode();
    Ok(app)
}

/// Open a diff without traversing committed history.
pub fn load_diff_app(args: &[String]) -> Result<crate::app::App, String> {
    let (separator, paths_start) = match args.iter().position(|arg| arg == "--") {
        Some(separator) => (separator, separator + 1),
        None => {
            let start = diff_paths_start(args)?;
            (start, start)
        }
    };
    let stat = args[..separator].iter().any(|arg| arg == "--stat");
    let options: Vec<_> = args[..separator]
        .iter()
        .filter(|arg| *arg != "--stat")
        .cloned()
        .collect();
    let cached = options.iter().any(|arg| arg == "--cached");
    let revisions: Vec<_> = options.iter().filter(|arg| *arg != "--cached").collect();
    if revisions.iter().any(|arg| arg.starts_with('-'))
        || revisions.len() > if cached { 1 } else { 2 }
    {
        return Err(
            "usage: glog diff [--cached] [--stat] [revision [revision]] [[--] pathspec...]"
                .to_owned(),
        );
    }
    let kind = if revisions.is_empty() {
        if cached {
            CommitKind::Staged
        } else {
            CommitKind::Unstaged
        }
    } else {
        CommitKind::Comparison {
            worktree: !cached && comparison_uses_worktree(&revisions)?,
        }
    };
    let mut entry = match kind {
        CommitKind::Unstaged => pseudo_commit(kind, "worktree", "Unstaged changes"),
        CommitKind::Staged => pseudo_commit(kind, "index", "Staged changes"),
        _ => pseudo_commit(kind, "diff", &format!("Diff {}", options.join(" "))),
    };
    if matches!(kind, CommitKind::Comparison { .. }) {
        entry.diff_args = options;
    }
    let paths = args[paths_start..].to_vec();
    let text = show(&entry, &paths)?;
    let mut app = crate::app::App::new(if text.is_empty() {
        Vec::new()
    } else {
        vec![entry]
    });
    app.show_stat = stat;
    app.show_paths = paths;
    if !app.commits.is_empty() {
        if app.has_log_view() {
            app.pending_history = Some(Vec::new());
        }
        app.mode = crate::app::Mode::Show;
        app.show_text = text;
        app.ensure_show_rows();
    }
    Ok(app)
}

/// Without `--`, git diff starts its paths at the first argument that is not
/// a revision. As in Git, they must all exist and cannot be followed by options.
fn diff_paths_start(args: &[String]) -> Result<usize, String> {
    let candidates: Vec<_> = (0..args.len())
        .filter(|&index| !args[index].starts_with('-'))
        .collect();
    if candidates.is_empty() {
        return Ok(args.len());
    }
    let output = Command::new("git")
        .args(["rev-parse", "--no-revs"])
        .args(candidates.iter().map(|&index| &args[index]))
        .output()
        .map_err(|error| format!("could not resolve diff revisions: {error}"))?;
    if !output.status.success() {
        return Err(stderr_message(
            "could not resolve diff revisions",
            &output.stderr,
        ));
    }
    // Git echoes the paths, which are the trailing candidates.
    let first = (0..=candidates.len())
        .find(|&first| {
            let mut expected = Vec::new();
            for &index in &candidates[first..] {
                expected.extend_from_slice(args[index].as_bytes());
                expected.push(b'\n');
            }
            expected == output.stdout
        })
        .ok_or("could not separate diff revisions from paths")?;
    let start = candidates.get(first).copied().unwrap_or(args.len());
    if let Some(option) = args[start..].iter().find(|arg| arg.starts_with('-')) {
        return Err(format!(
            "option '{option}' must come before non-option arguments"
        ));
    }
    Ok(start)
}

fn comparison_uses_worktree(revisions: &[&String]) -> Result<bool, String> {
    // One argument can expand to multiple endpoints (HEAD^!, HEAD^-),
    // while HEAD^@ can expand to just one parent or none for a root commit.
    // Like git diff, use the working tree when there are fewer than two.
    let output = Command::new("git")
        .args(["rev-parse", "--revs-only", "--end-of-options"])
        .args(revisions)
        .arg("--")
        .output()
        .map_err(|error| format!("could not resolve diff revisions: {error}"))?;
    if !output.status.success() {
        return Err(stderr_message(
            "could not resolve diff revisions",
            &output.stderr,
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).lines().count() < 2)
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
    let root = repository_root()?;
    for path in untracked_paths(&[])? {
        fingerprint.write(path.as_os_str().as_encoded_bytes());
        let display = path.display();
        let path = root.join(&path);
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("could not inspect untracked file {display}: {error}"))?;
        let file_type = metadata.file_type();
        fingerprint.write_u64(metadata.len());
        fingerprint.write_u8(u8::from(file_type.is_file()));
        fingerprint.write_u8(u8::from(file_type.is_dir()));
        fingerprint.write_u8(u8::from(file_type.is_symlink()));
        fingerprint.write_u8(u8::from(metadata.permissions().readonly()));
        let modified = metadata
            .modified()
            .map_err(|error| format!("could not inspect untracked file {display}: {error}"))?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| format!("invalid modification time for {display}: {error}"))?;
        fingerprint.write_u64(modified.as_secs());
        fingerprint.write_u32(modified.subsec_nanos());
        if file_type.is_symlink() {
            let target = fs::read_link(&path)
                .map_err(|error| format!("could not read untracked symlink {display}: {error}"))?;
            fingerprint.write(target.as_os_str().as_encoded_bytes());
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

fn parse_log(output: &str) -> Result<Vec<Commit>, String> {
    // Inspect dates before splitting into graph lines: a newline in %ad would
    // otherwise split the metadata record and silently discard the commit.
    for record in output.split(RECORD).skip(1) {
        if let Some(date) = record.split(FIELD).nth(5) {
            if date.contains(['\n', '\r']) {
                return Err(
                    "Git author dates must fit on one line; use --date=short or change log.date"
                        .to_owned(),
                );
            }
        }
    }
    let mut pending_graph = Vec::new();
    let mut commits = Vec::new();
    for line in output.lines() {
        if let Some(marker) = line.find(RECORD) {
            let fields: Vec<_> = line[marker + 1..].splitn(8, FIELD).collect();
            if fields.len() == 8 {
                pending_graph.push(line[..marker].to_owned());
                commits.push(Commit {
                    kind: CommitKind::Revision,
                    diff_args: Vec::new(),
                    hash: fields[0].to_owned(),
                    short_hash: fields[1].to_owned(),
                    decorations: fields[2].to_owned(),
                    author: fields[3].to_owned(),
                    author_email: fields[4].to_owned(),
                    author_date: fields[5].to_owned(),
                    collaborators: Collaborators::parse(fields[6], fields[4]),
                    subject: fields[7].to_owned(),
                    graph: std::mem::take(&mut pending_graph),
                });
            }
        } else if !line.is_empty() {
            pending_graph.push(line.to_owned());
        }
    }
    Ok(commits)
}

#[cfg(test)]
fn working_tree_entries() -> Result<Vec<Commit>, String> {
    let unstaged =
        has_diff(&["diff", "--quiet", "--no-ext-diff"])? || !untracked_paths(&[])?.is_empty();
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
        diff_args: Vec::new(),
        hash: format!("[{short_hash}]"),
        short_hash: short_hash.to_owned(),
        decorations: String::new(),
        author: String::new(),
        author_email: String::new(),
        author_date: String::new(),
        collaborators: Collaborators::default(),
        subject: subject.to_owned(),
        graph: vec!["* ".to_owned()],
    }
}

#[cfg(test)]
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

pub fn show(commit: &Commit, paths: &[String]) -> Result<String, String> {
    match commit.kind {
        CommitKind::WorkingTree => Err("Working tree opens the Status view".to_owned()),
        CommitKind::Comparison { .. } => {
            let mut args = vec!["diff", "--color=always", "--no-ext-diff"];
            args.extend(commit.diff_args.iter().map(String::as_str));
            show_diff(&args, paths)
        }
        CommitKind::Revision => show_revision(&commit.hash, paths),
        CommitKind::Staged => show_diff(
            &["diff", "--cached", "--color=always", "--no-ext-diff"],
            paths,
        ),
        CommitKind::Unstaged => show_unstaged(paths),
    }
}

fn show_revision(hash: &str, paths: &[String]) -> Result<String, String> {
    let output = patch_command()
        .args([
            "--no-pager",
            "show",
            "--decorate=short",
            "--color=always",
            "--no-ext-diff",
            "--full-index",
            "--submodule=short",
        ])
        .args(DIFF_PREFIX_ARGS)
        .arg(hash)
        .arg("--")
        .args(paths)
        .env("GIT_PAGER", "cat")
        .output()
        .map_err(|error| format!("could not run git show: {error}"))?;
    if !output.status.success() {
        return Err(stderr_message("git show failed", &output.stderr));
    }
    format_output(output.stdout)
}

fn show_diff(args: &[&str], paths: &[String]) -> Result<String, String> {
    let output = patch_command()
        .args(args)
        .args(["--full-index", "--submodule=short"])
        .args(DIFF_PREFIX_ARGS)
        .arg("--")
        .args(paths)
        .env("GIT_PAGER", "cat")
        .output()
        .map_err(|error| format!("could not run git diff: {error}"))?;
    if !output.status.success() {
        return Err(stderr_message("git diff failed", &output.stderr));
    }
    format_output(output.stdout)
}

fn show_unstaged(paths: &[String]) -> Result<String, String> {
    let output = patch_command()
        .args([
            "diff",
            "--color=always",
            "--no-ext-diff",
            "--full-index",
            "--submodule=short",
        ])
        .args(DIFF_PREFIX_ARGS)
        .arg("--")
        .args(paths)
        .output()
        .map_err(|error| format!("could not run git diff: {error}"))?;
    if !output.status.success() {
        return Err(stderr_message("git diff failed", &output.stderr));
    }
    let mut formatted = format_output(output.stdout)?;
    let root = repository_root()?;
    for path in untracked_paths(paths)? {
        let metadata = fs::symlink_metadata(root.join(&path)).map_err(|error| {
            format!(
                "could not inspect untracked file {}: {error}",
                path.display()
            )
        })?;
        let bytes = path.as_os_str().as_encoded_bytes();
        let old_path = git_quote_path(&[b"a/", bytes].concat());
        let new_path = git_quote_path(&[b"b/", bytes].concat());
        let mode = file_mode(&metadata);
        formatted.push_str(&format!(
            "\x1b[1mdiff --git {old_path} {new_path}\x1b[m\n\x1b[1mnew file mode {mode:o}\x1b[m\nglog-lazy-untracked:{}\n",
            hex_encode(bytes)
        ));
    }
    Ok(formatted)
}

/// Read only local objects; never fetch or compare against the current checkout.
pub fn show_submodule(
    root: Option<&std::path::Path>,
    path: impl AsRef<std::path::Path>,
    old: &str,
    new: &str,
) -> Result<(std::path::PathBuf, String), String> {
    let root = if let Some(root) = root {
        root.to_owned()
    } else {
        repository_root()?
    };
    let directory = root.join(path.as_ref());
    let path = path.as_ref().display();
    // Without this check Git can walk up to the superproject for an empty,
    // uninitialized submodule directory.
    if !directory.join(".git").exists() {
        return Err(format!("Submodule {path} is not initialized locally"));
    }
    let output = repository_env(&mut patch_command(), &directory)
        .current_dir(&directory)
        .env("GIT_NO_LAZY_FETCH", "1")
        .args([
            "--no-pager",
            "diff",
            "--color=always",
            "--no-ext-diff",
            "--no-textconv",
            "--full-index",
            "--submodule=short",
        ])
        .args(DIFF_PREFIX_ARGS)
        .args([old, new, "--"])
        .output()
        .map_err(|e| format!("could not read submodule {path}: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "Submodule {path}: cannot compare recorded commits (history may be missing locally)"
        ));
    }
    Ok((directory, format_output(output.stdout)?))
}

/// Shows untracked `path`, relative to the repository root.
pub fn show_untracked(path: &std::path::Path) -> Result<String, String> {
    show_untracked_in(&repository_root()?, path)
}

/// Shows untracked `path`, relative to `root`, labeled by that relative path.
pub fn show_untracked_in(root: &std::path::Path, path: &std::path::Path) -> Result<String, String> {
    let name = path.to_string_lossy();
    // Git lists untracked nested repositories as directories, which have no
    // content of their own to diff.
    if fs::symlink_metadata(root.join(path)).is_ok_and(|metadata| metadata.is_dir()) {
        return Err(format!(
            "{name} is an untracked nested repository; add it as a submodule to see its changes"
        ));
    }
    let mut output = Vec::new();
    if !untracked_regular_file_diff(&root.join(path), path, &mut output)? {
        let untracked = repository_env(&mut patch_command(), root)
            .current_dir(root)
            .args([
                "--no-pager",
                "diff",
                "--no-index",
                "--color=always",
                "--no-ext-diff",
            ])
            .args(DIFF_PREFIX_ARGS)
            .args(["--", NULL_DEVICE])
            .arg(path)
            .output()
            .map_err(|error| format!("could not diff untracked file {name}: {error}"))?;
        // Exit code 1 means the files differ, but Git also uses it for
        // errors such as unreadable paths.
        let reported_error = untracked
            .stderr
            .split(|&byte| byte == b'\n')
            .any(|line| line.starts_with(b"error:"));
        if !matches!(untracked.status.code(), Some(0 | 1)) || reported_error {
            return Err(stderr_message("git diff failed", &untracked.stderr));
        }
        output = untracked.stdout;
    }
    format_output(output)
}

fn untracked_regular_file_diff(
    full_path: &std::path::Path,
    path: &std::path::Path,
    output: &mut Vec<u8>,
) -> Result<bool, String> {
    let name = path.to_string_lossy();
    let metadata = fs::symlink_metadata(full_path)
        .map_err(|error| format!("could not inspect untracked file {name}: {error}"))?;
    if !metadata.file_type().is_file() {
        return Ok(false);
    }

    // Generate the simple new-file patch directly. Spawning `git diff
    // --no-index` once per untracked file is painfully slow for generated
    // trees containing thousands of files, even when every binary file only
    // contributes a three-line notice.
    let mut file = fs::File::open(full_path)
        .map_err(|error| format!("could not read untracked file {name}: {error}"))?;
    let mut contents = Vec::with_capacity(8_000);
    Read::by_ref(&mut file)
        .take(8_000)
        .read_to_end(&mut contents)
        .map_err(|error| format!("could not read untracked file {name}: {error}"))?;

    let bytes = path.as_os_str().as_encoded_bytes();
    let old_path = git_quote_path(&[b"a/", bytes].concat());
    let new_path = git_quote_path(&[b"b/", bytes].concat());
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
        .map_err(|error| format!("could not read untracked file {name}: {error}"))?;

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

fn git_quote_path(path: &[u8]) -> String {
    if path
        .iter()
        .all(|&byte| (b' '..=b'~').contains(&byte) && byte != b'"' && byte != b'\\')
    {
        return String::from_utf8_lossy(path).into_owned();
    }
    let mut quoted = String::from("\"");
    for &byte in path {
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

/// Lists untracked paths relative to the repository root, matching patch
/// headers, while `paths` stay relative to the current directory.
fn untracked_paths(paths: &[String]) -> Result<Vec<std::path::PathBuf>, String> {
    // Unlike `git diff`, `ls-files` only covers the current directory by default.
    let whole_repository = [":/".to_owned()];
    let output = Command::new("git")
        .args([
            "ls-files",
            "--others",
            "--exclude-standard",
            "--full-name",
            "-z",
            "--",
        ])
        .args(if paths.is_empty() {
            &whole_repository[..]
        } else {
            paths
        })
        .output()
        .map_err(|error| format!("could not list untracked files: {error}"))?;
    if !output.status.success() {
        return Err(stderr_message("git ls-files failed", &output.stderr));
    }
    Ok(parse_untracked_paths(&output.stdout))
}

pub(crate) fn raw_path(bytes: &[u8]) -> std::path::PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        std::ffi::OsStr::from_bytes(bytes).into()
    }
    #[cfg(not(unix))]
    {
        String::from_utf8_lossy(bytes).into_owned().into()
    }
}

/// Repository variables such as GIT_DIR take precedence over `-C` and the
/// working directory. They describe the repository glog was started in, so
/// like Git's own submodule commands, clear them (keeping `-c` configuration)
/// for commands in another repository.
const REPOSITORY_ENV: [&str; 13] = [
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CONFIG",
    "GIT_OBJECT_DIRECTORY",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_IMPLICIT_WORK_TREE",
    "GIT_GRAFT_FILE",
    "GIT_INDEX_FILE",
    "GIT_NO_REPLACE_OBJECTS",
    "GIT_REPLACE_REF_BASE",
    "GIT_PREFIX",
    "GIT_SHALLOW_FILE",
    "GIT_COMMON_DIR",
];

/// Prepares `command` to run in `directory`, which may be a submodule.
pub(crate) fn repository_env<'a>(
    command: &'a mut Command,
    directory: &std::path::Path,
) -> &'a mut Command {
    static MAIN_ROOT: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    if !REPOSITORY_ENV
        .iter()
        .any(|name| env::var_os(name).is_some())
    {
        return command;
    }
    if MAIN_ROOT.get_or_init(|| repository_root().ok()).as_deref() != Some(directory) {
        for name in REPOSITORY_ENV {
            command.env_remove(name);
        }
        return command;
    }
    // Git resolves relative paths in these after changing to `directory`, so
    // anchor them to the directory glog was started in.
    let Ok(current) = env::current_dir() else {
        return command;
    };
    for name in REPOSITORY_PATH_ENV {
        if let Some(value) = env::var_os(name) {
            command.env(name, current.join(value));
        }
    }
    if let Some(value) = env::var_os("GIT_ALTERNATE_OBJECT_DIRECTORIES") {
        let paths = env::split_paths(&value).map(|path| current.join(path));
        if let Ok(value) = env::join_paths(paths) {
            command.env("GIT_ALTERNATE_OBJECT_DIRECTORIES", value);
        }
    }
    command
}

/// The variables in [`REPOSITORY_ENV`] holding a single path.
const REPOSITORY_PATH_ENV: [&str; 8] = [
    "GIT_CONFIG",
    "GIT_OBJECT_DIRECTORY",
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_GRAFT_FILE",
    "GIT_INDEX_FILE",
    "GIT_SHALLOW_FILE",
    "GIT_COMMON_DIR",
];

/// Git terminates its raw repository path with exactly one newline.
pub(crate) fn repository_root() -> Result<std::path::PathBuf, String> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .map_err(|error| format!("could not locate repository: {error}"))?;
    if !output.status.success() {
        return Err(stderr_message(
            "could not locate repository",
            &output.stderr,
        ));
    }
    Ok(raw_path(
        output.stdout.strip_suffix(b"\n").unwrap_or(&output.stdout),
    ))
}

fn parse_untracked_paths(output: &[u8]) -> Vec<std::path::PathBuf> {
    output
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(raw_path)
        .collect()
}

pub(crate) fn format_output(output: Vec<u8>) -> Result<String, String> {
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

pub(crate) fn pipe_through(command: &mut Command, input: &[u8]) -> Option<String> {
    pipe_through_bytes(command, input).map(|output| String::from_utf8_lossy(&output).into_owned())
}

pub(crate) fn pipe_through_bytes(command: &mut Command, input: &[u8]) -> Option<Vec<u8>> {
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
    output.status.success().then_some(output.stdout)
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
        fmt::Write,
        fs,
        path::{Path, PathBuf},
        sync::{atomic::AtomicUsize, atomic::Ordering, Mutex, MutexGuard},
    };

    static CURRENT_DIR_LOCK: Mutex<()> = Mutex::new(());
    static NEXT_TEST_DIRECTORY: AtomicUsize = AtomicUsize::new(0);

    #[test]
    fn patch_paths_ignore_disabled_git_quoting() {
        let results = {
            let directory = TestDirectory::new();
            let _guard = CurrentDirGuard::enter(directory.path());
            let git = |args: &[&str]| {
                let output = Command::new("git").args(args).output().unwrap();
                assert!(output.status.success(), "{:?}", output);
                output.stdout
            };
            git(&["init", "-q"]);
            git(&["config", "user.name", "Test"]);
            git(&["config", "user.email", "test@example.com"]);
            git(&["config", "commit.gpgsign", "false"]);
            git(&["config", "core.quotePath", "false"]);
            git(&["commit", "--allow-empty", "-qm", "base"]);
            // Build the index directly: these names need not be supported by
            // the host filesystem (notably on macOS).
            let blob = pipe_through_bytes(
                Command::new("git").args(["hash-object", "-w", "--stdin"]),
                b"contents\n",
            )
            .unwrap();
            let oid = String::from_utf8(blob).unwrap();
            let mut index = Vec::new();
            for path in [b"deps-\xfe.lock", b"deps-\xff.lock"] {
                index.extend_from_slice(format!("100644 {}\t", oid.trim()).as_bytes());
                index.extend_from_slice(path);
                index.push(0);
            }
            pipe_through_bytes(
                Command::new("git").args(["update-index", "-z", "--index-info"]),
                &index,
            )
            .unwrap();
            let staged = load_diff_app(&["--cached".into(), "--stat".into()]).unwrap();
            git(&["commit", "-qm", "raw names"]);
            let mut results = Vec::new();
            for mut app in [
                staged,
                load_show_app(&["--stat".into()]).unwrap(),
                load_diff_app(&["HEAD~..HEAD".into(), "--stat".into()]).unwrap(),
                load_diff_app(&["--stat".into()]).unwrap(),
            ] {
                let paths: Vec<_> = crate::diff::file_sections(&app.show_text)
                    .into_iter()
                    .map(|file| file.path_bytes)
                    .collect();
                app.show_cursor = app.show_rows.iter().position(|row| row.summary).unwrap();
                app.toggle_show_file();
                let expanded = app
                    .show_rows
                    .iter()
                    .filter(|row| row.summary && !row.folded)
                    .count();
                results.push((paths, Some(expanded)));
            }
            let (_, patch) = show_submodule(Some(directory.path()), ".", "HEAD~", "HEAD").unwrap();
            results.push((
                crate::diff::file_sections(&patch)
                    .into_iter()
                    .map(|file| file.path_bytes)
                    .collect(),
                None,
            ));
            results
        };
        for (paths, expanded) in results {
            assert_eq!(
                paths,
                [b"deps-\xfe.lock".to_vec(), b"deps-\xff.lock".to_vec()]
            );
            if let Some(expanded) = expanded {
                assert_eq!(expanded, 1, "only the selected summary should expand");
            }
        }
    }

    #[test]
    fn watch_summary_honors_the_starting_repository_environment() {
        watch_summary_with_repository_environment(
            "watch_summary_honors_the_starting_repository_environment",
            false,
        );
    }

    #[test]
    fn watch_summary_honors_a_relative_repository_environment() {
        watch_summary_with_repository_environment(
            "watch_summary_honors_a_relative_repository_environment",
            true,
        );
    }

    /// Runs `test` in a dotfiles-style repository selected by GIT_DIR,
    /// GIT_WORK_TREE and GIT_INDEX_FILE, relative to a work tree subdirectory
    /// if `relative`.
    fn watch_summary_with_repository_environment(test: &str, relative: bool) {
        // GIT_DIR would redirect Git in concurrently running tests, so set it
        // only for a child process running just this test.
        const CHILD: &str = "GLOG_TEST_REPOSITORY_ENV_CHILD";
        if env::var_os(CHILD).is_some() {
            let log = load_watch_log().unwrap();
            assert_eq!(log[0].subject, "Working tree · 2 changed files");
            assert_eq!(log.len(), 2);
            return;
        }
        let directory = TestDirectory::new();
        let git_dir = directory.path().join("dotfiles.git");
        let work_tree = directory.path().join("home");
        let index = directory.path().join("index");
        fs::create_dir(&work_tree).unwrap();
        let git = |index_file: Option<&Path>, args: &[&str]| {
            let mut command = Command::new("git");
            command
                .arg("--git-dir")
                .arg(&git_dir)
                .arg("--work-tree")
                .arg(&work_tree)
                .args(args);
            if let Some(index_file) = index_file {
                command.env("GIT_INDEX_FILE", index_file);
            }
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(None, &["init", "-q"]);
        git(None, &["config", "user.name", "Test"]);
        git(None, &["config", "user.email", "test@example.com"]);
        git(None, &["config", "commit.gpgsign", "false"]);
        fs::write(work_tree.join(".profile"), "base\n").unwrap();
        git(None, &["add", ".profile"]);
        git(None, &["commit", "-qm", "base"]);
        fs::write(work_tree.join(".profile"), "edited\n").unwrap();
        // Only the alternate index tracks this file; in the default one it
        // would count as untracked.
        fs::write(work_tree.join(".extra"), "extra\n").unwrap();
        fs::copy(git_dir.join("index"), &index).unwrap();
        git(Some(&index), &["add", ".extra"]);
        let mut child = Command::new(env::current_exe().unwrap());
        child
            .args(["--exact", &format!("git::tests::{test}")])
            .env(CHILD, "1");
        if relative {
            let subdirectory = work_tree.join("sub");
            fs::create_dir(&subdirectory).unwrap();
            child
                .current_dir(subdirectory)
                .env("GIT_DIR", "../../dotfiles.git")
                .env("GIT_WORK_TREE", "..")
                .env("GIT_INDEX_FILE", "../../index");
        } else {
            child
                .current_dir(&work_tree)
                .env("GIT_DIR", &git_dir)
                .env("GIT_WORK_TREE", &work_tree)
                .env("GIT_INDEX_FILE", &index);
        }
        let output = child.output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{stdout}");
        assert!(stdout.contains("1 passed"), "{stdout}");
    }

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

    #[cfg(unix)]
    #[test]
    fn untracked_paths_preserve_bytes_for_watch_fingerprints() {
        use std::os::unix::ffi::OsStrExt;
        // Exercise Git's NUL-delimited output even on filesystems that cannot
        // create these names (including the default macOS filesystem).
        let paths = parse_untracked_paths(b"caf\xe9.txt\0ordinary.txt\0");
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0].as_os_str().as_bytes(), b"caf\xe9.txt");
        assert_eq!(paths[1].as_os_str().as_bytes(), b"ordinary.txt");
    }

    #[cfg(unix)]
    #[test]
    fn watch_fingerprint_accepts_non_utf8_untracked_files() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
        let directory = TestDirectory::new();
        let _guard = CurrentDirGuard::enter(directory.path());
        assert!(Command::new("git")
            .args(["init", "-q"])
            .status()
            .unwrap()
            .success());
        let path = OsStr::from_bytes(b"caf\xe9.txt");
        if let Err(error) = fs::write(path, "first\n") {
            // Darwin EILSEQ: this filesystem rejects non-UTF-8 names.
            #[cfg(target_os = "macos")]
            if error.raw_os_error() == Some(92) {
                return;
            }
            panic!("could not create filename fixture: {error}");
        }
        let before = watch_fingerprint().unwrap();
        fs::write(path, "different contents\n").unwrap();
        assert_ne!(before, watch_fingerprint().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn diff_expands_non_utf8_untracked_files_independently() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
        let directory = TestDirectory::new();
        let _guard = CurrentDirGuard::enter(directory.path());
        assert!(Command::new("git")
            .args(["init", "-q"])
            .status()
            .unwrap()
            .success());
        for (name, contents) in [(b"a\xff.txt", "first\n"), (b"a\xfe.txt", "second\n")] {
            if let Err(error) = fs::write(OsStr::from_bytes(name), contents) {
                // Darwin EILSEQ: this filesystem rejects non-UTF-8 names.
                #[cfg(target_os = "macos")]
                if error.raw_os_error() == Some(92) {
                    return;
                }
                panic!("could not create filename fixture: {error}");
            }
        }
        let mut app = load_diff_app(&[]).unwrap();
        let files = crate::diff::file_sections(&app.show_text);
        assert_eq!(files.len(), 2);
        assert_ne!(files[0].path_bytes, files[1].path_bytes);
        for file in 0..2 {
            app.show_cursor = app
                .show_rows
                .iter()
                .position(|row| row.folded && row.file == Some(file))
                .unwrap();
            app.toggle_show_file();
            assert!(app.status.is_none(), "{:?}", app.status);
        }
        let text = crate::ansi::plain(&app.show_text);
        assert!(text.contains("+first"), "{text}");
        assert!(text.contains("+second"), "{text}");
    }

    #[test]
    fn submodule_expansion_uses_recorded_commits_and_nested_folds() {
        let directory = TestDirectory::new();
        let _guard = CurrentDirGuard::enter(directory.path());
        let git = |args: &[&str]| {
            let output = Command::new("git").args(args).output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        };
        git(&["init", "-q"]);
        git(&["config", "user.name", "Test"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "commit.gpgsign", "false"]);
        git(&["init", "-q", "module space"]);
        for (name, value) in [
            ("user.name", "Test"),
            ("user.email", "test@example.com"),
            ("commit.gpgsign", "false"),
        ] {
            git(&["-C", "module space", "config", name, value]);
        }
        fs::write("module space/file.txt", "before\n").unwrap();
        git(&["-C", "module space", "add", "."]);
        git(&["-C", "module space", "commit", "-qm", "first"]);
        let old = git(&["-C", "module space", "rev-parse", "HEAD"]);
        fs::write(
            ".gitmodules",
            "[submodule \"module space\"]\n\tpath = module space\n\turl = ./unused-local-url\n",
        )
        .unwrap();
        git(&["add", "module space", ".gitmodules"]);
        git(&["commit", "-qm", "base"]);
        git(&["submodule", "absorbgitdirs"]);
        fs::write("module space/file.txt", "after\n").unwrap();
        git(&["-C", "module space", "commit", "-qam", "second"]);
        let new = git(&["-C", "module space", "rev-parse", "HEAD"]);
        git(&["add", "module space"]);
        git(&["commit", "-qm", "update"]);
        // Both merge parents record the old gitlink; the merge records the new
        // one. Git emits a combined diff with no mode on its index header.
        let old_tree = git(&["rev-parse", "HEAD~^{tree}"]);
        let new_tree = git(&["rev-parse", "HEAD^{tree}"]);
        let left = git(&["commit-tree", &old_tree, "-p", "HEAD~", "-m", "left"]);
        let right = git(&["commit-tree", &old_tree, "-p", "HEAD~", "-m", "right"]);
        let merge = git(&[
            "commit-tree",
            &new_tree,
            "-p",
            &left,
            "-p",
            &right,
            "-m",
            "merge",
        ]);
        // User configuration must not change the parseable gitlink format.
        git(&["config", "diff.submodule", "log"]);
        git(&["config", "diff.noprefix", "true"]);
        git(&["-C", "module space", "checkout", "-q", &old]);
        fs::write("module space/file.txt", "unrelated checkout edit\n").unwrap();
        fs::create_dir("subdirectory").unwrap();
        env::set_current_dir(directory.path().join("subdirectory")).unwrap();
        for mut app in [
            load_diff_app(&["HEAD~..HEAD".into()]).unwrap(),
            load_diff_app(&["HEAD~..HEAD".into(), "--stat".into()]).unwrap(),
            load_show_app(&["HEAD".into()]).unwrap(),
            load_show_app(&["HEAD".into(), "--stat".into()]).unwrap(),
            load_show_app(std::slice::from_ref(&merge)).unwrap(),
            load_show_app(&[merge, "--stat".into()]).unwrap(),
        ] {
            app.show_cursor = app
                .show_rows
                .iter()
                .position(|r| r.folded && r.text.contains("module space"))
                .unwrap();
            assert!(!app.show_rows.iter().any(|r| r.text.contains("file.txt")));
            app.toggle_show_file();
            assert!(app.status.is_none(), "{:?}", app.status);
            let parent = app.show_cursor;
            app.show_cursor = app
                .show_rows
                .iter()
                .position(|r| r.folded && r.text.contains("file.txt"))
                .unwrap();
            app.toggle_show_file();
            let text = app
                .show_rows
                .iter()
                .map(|r| crate::ansi::plain(&r.text))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(text.contains("-before"), "{text}");
            assert!(text.contains("+after"), "{text}");
            assert!(!text.contains("unrelated checkout edit"));
            app.show_cursor = parent;
            app.toggle_show_file();
            assert!(!app.show_rows.iter().any(|r| r.text.contains("file.txt")));
            app.toggle_show_file();
            assert!(app.show_rows.iter().any(|r| r.text.contains("file.txt")));
            // Search should reveal already-loaded contents even after folding
            // both the nested file and its submodule.
            app.show_cursor = app
                .show_rows
                .iter()
                .position(|r| r.summary && r.text.contains("file.txt"))
                .unwrap();
            app.toggle_show_file();
            app.show_cursor = parent;
            app.toggle_show_file();
            app.begin_search(false);
            app.search_input = Some("+after".to_owned());
            app.submit_search();
            assert!(app.status.is_none(), "{:?}", app.status);
            assert!(crate::ansi::plain(&app.show_rows[app.show_cursor].text).contains("+after"));
        }
        assert!(show_submodule(None, "module space", &old, &"1".repeat(40))
            .unwrap_err()
            .contains("history may be missing"));
        fs::rename(
            directory.path().join("module space/.git"),
            directory.path().join("module-git"),
        )
        .unwrap();
        assert!(show_submodule(None, "module space", &old, &new)
            .unwrap_err()
            .contains("not initialized"));
    }

    #[test]
    fn committed_submodule_images_use_recorded_blobs() {
        let results = {
            let directory = TestDirectory::new();
            let _guard = CurrentDirGuard::enter(directory.path());
            let git = |args: &[&str]| {
                let output = Command::new("git")
                    .args([
                        "-c",
                        "user.name=Test",
                        "-c",
                        "user.email=test@example.com",
                        "-c",
                        "commit.gpgsign=false",
                    ])
                    .args(args)
                    .output()
                    .unwrap();
                assert!(output.status.success(), "{:?}", output);
                String::from_utf8(output.stdout).unwrap().trim().to_owned()
            };
            git(&["init", "-q"]);
            git(&["init", "-q", "module"]);
            let picture = |value| {
                image::RgbaImage::from_pixel(2, 2, image::Rgba([value, 10, 20, 255]))
                    .save("module/picture.png")
                    .unwrap();
            };
            picture(1);
            git(&["-C", "module", "add", "."]);
            git(&["-C", "module", "commit", "-qm", "before"]);
            let before = git(&["-C", "module", "rev-parse", "HEAD:picture.png"]);
            git(&["add", "module"]);
            git(&["commit", "-qm", "base"]);
            picture(2);
            git(&["-C", "module", "commit", "-qam", "after"]);
            let after = git(&["-C", "module", "rev-parse", "HEAD:picture.png"]);
            git(&["add", "module"]);
            git(&["commit", "-qm", "update"]);
            // Previews must use recorded objects, even with another checkout.
            git(&["-C", "module", "checkout", "-q", "HEAD~"]);
            picture(3);
            let module_root = directory.path().join("module").canonicalize().unwrap();
            let expected = vec![
                crate::images::Source::Blob(module_root.clone(), before),
                crate::images::Source::Blob(module_root, after),
            ];
            let mut results = Vec::new();
            for mut app in [
                load_show_app(&["HEAD".into(), "--stat".into()]).unwrap(),
                load_diff_app(&["HEAD~..HEAD".into(), "--stat".into()]).unwrap(),
            ] {
                app.images.enabled = true;
                app.images.root = directory.path().to_owned();
                app.show_cursor = app.show_rows.iter().position(|r| r.summary).unwrap();
                app.toggle_show_file();
                app.show_cursor = app
                    .show_rows
                    .iter()
                    .position(|r| r.summary && r.text.contains("picture.png"))
                    .unwrap();
                app.toggle_show_file();
                let previews: Vec<_> = app
                    .show_rows
                    .iter()
                    .filter_map(|row| row.preview.as_ref())
                    .filter(|preview| preview.row == 0)
                    // Git reports roots in its own spelling, e.g. C:/ on Windows.
                    .map(|preview| match &preview.source {
                        crate::images::Source::Blob(root, id) => {
                            crate::images::Source::Blob(root.canonicalize().unwrap(), id.clone())
                        }
                        source => source.clone(),
                    })
                    .collect();
                results.push((previews, expected.clone()));
            }
            results
        };
        for (previews, expected) in results {
            assert_eq!(
                previews, expected,
                "nested Before/After previews must use submodule blobs"
            );
        }
    }

    #[test]
    fn relative_diffs_preview_the_working_image_from_a_subdirectory() {
        let results = {
            let directory = TestDirectory::new();
            let _guard = CurrentDirGuard::enter(directory.path());
            let git = |args: &[&str]| {
                let output = Command::new("git").args(args).output().unwrap();
                assert!(output.status.success(), "{:?}", output);
            };
            git(&["init", "-q"]);
            git(&["config", "user.name", "Test"]);
            git(&["config", "user.email", "test@example.com"]);
            git(&["config", "commit.gpgsign", "false"]);
            fs::create_dir("nested").unwrap();
            let picture = |path: &str, value| {
                image::RgbaImage::from_pixel(2, 2, image::Rgba([value, 10, 20, 255]))
                    .save(path)
                    .unwrap();
            };
            picture("nested/picture.png", 1);
            git(&["add", "."]);
            git(&["commit", "-qm", "base"]);
            picture("nested/picture.png", 2);
            let expected = fs::read("nested/picture.png").unwrap();
            // A wrong repository-relative lookup can silently show another image.
            picture("picture.png", 3);
            env::set_current_dir(directory.path().join("nested")).unwrap();
            let mut results = Vec::new();
            for relative in ["false", "true"] {
                git(&["config", "diff.relative", relative]);
                for args in [vec!["--stat".into()], vec!["--stat".into(), "HEAD".into()]] {
                    let mut app = load_diff_app(&args).unwrap();
                    app.images.enabled = true;
                    app.images.root = directory.path().to_owned();
                    app.show_cursor = app.show_rows.iter().position(|r| r.summary).unwrap();
                    app.toggle_show_file();
                    let after =
                        app.show_rows
                            .iter()
                            .find_map(|row| match &row.preview.as_ref()?.source {
                                crate::images::Source::File(path, _, _) => fs::read(path).ok(),
                                _ => None,
                            });
                    results.push((relative, args, after, expected.clone()));
                }
            }
            results
        };
        // Keep regression assertions outside the cwd guard to avoid poisoning it.
        for (relative, args, after, expected) in results {
            assert_eq!(
                after,
                Some(expected),
                "diff.relative={relative}, {args:?}: After must show nested/picture.png"
            );
        }
    }

    #[test]
    fn diffs_preview_untracked_images_from_a_subdirectory() {
        let (after, expected) = {
            let directory = TestDirectory::new();
            let _guard = CurrentDirGuard::enter(directory.path());
            let git = |args: &[&str]| {
                let output = Command::new("git").args(args).output().unwrap();
                assert!(output.status.success(), "{:?}", output);
            };
            git(&["init", "-q"]);
            git(&["config", "user.name", "Test"]);
            git(&["config", "user.email", "test@example.com"]);
            git(&["config", "commit.gpgsign", "false"]);
            fs::write("tracked.txt", "tracked\n").unwrap();
            git(&["add", "."]);
            git(&["commit", "-qm", "base"]);
            let picture = |path: &str, value| {
                image::RgbaImage::from_pixel(2, 2, image::Rgba([value, 10, 20, 255]))
                    .save(path)
                    .unwrap();
            };
            fs::create_dir_all("nested/nested").unwrap();
            picture("nested/picture.png", 1);
            let expected = fs::read("nested/picture.png").unwrap();
            // A lookup relative to the current directory can silently show another image.
            picture("nested/nested/picture.png", 2);
            env::set_current_dir(directory.path().join("nested")).unwrap();
            let mut app = load_diff_app(&["--stat".into()]).unwrap();
            app.images.enabled = true;
            app.images.root = directory.path().to_owned();
            let plain = |row: &crate::app::ShowRow| crate::ansi::plain(&row.text);
            app.show_cursor = app
                .show_rows
                .iter()
                .position(|row| {
                    row.summary
                        && plain(row).contains("nested/picture.png")
                        && !plain(row).contains("nested/nested/")
                })
                .unwrap();
            let file = app.show_rows[app.show_cursor].file;
            app.toggle_show_file();
            let after = app
                .show_rows
                .iter()
                .filter(|row| row.file == file)
                .find_map(|row| match &row.preview.as_ref()?.source {
                    crate::images::Source::File(path, _, _) => fs::read(path).ok(),
                    _ => None,
                });
            (after, expected)
        };
        // Keep regression assertions outside the cwd guard to avoid poisoning it.
        assert_eq!(
            after,
            Some(expected),
            "After must show the untracked nested/picture.png"
        );
    }

    #[test]
    fn watch_refresh_preserves_expanded_submodule_patch_and_reading_line() {
        let (before, after, expanded, decorations, fingerprint_changed) = {
            let directory = TestDirectory::new();
            let _guard = CurrentDirGuard::enter(directory.path());
            let git = |args: &[&str]| {
                let output = Command::new("git").args(args).output().unwrap();
                assert!(output.status.success(), "{:?}", output);
            };
            git(&["init", "-q"]);
            git(&["init", "-q", "module"]);
            for root in [".", "module"] {
                git(&["-C", root, "config", "user.name", "Test"]);
                git(&["-C", root, "config", "user.email", "test@example.com"]);
                git(&["-C", root, "config", "commit.gpgsign", "false"]);
            }
            fs::write("module/file.txt", "before\n").unwrap();
            git(&["-C", "module", "add", "."]);
            git(&["-C", "module", "commit", "-qm", "before"]);
            fs::write(
                ".gitmodules",
                "[submodule \"module\"]\n\tpath = module\n\turl = ./unused-local-url\n",
            )
            .unwrap();
            git(&["add", ".gitmodules", "module"]);
            git(&["commit", "-qm", "base"]);
            git(&["submodule", "absorbgitdirs"]);
            fs::write("module/file.txt", "after\n").unwrap();
            git(&["-C", "module", "commit", "-qam", "after"]);
            git(&["add", "module"]);
            git(&["commit", "-qm", "update"]);
            let mut app = load_show_app(&[]).unwrap();
            app.watch = true;
            for path in ["module", "file.txt"] {
                app.show_cursor = app
                    .show_rows
                    .iter()
                    .position(|row| row.folded && row.text.contains(path))
                    .unwrap();
                app.toggle_show_file();
            }
            app.show_cursor = app
                .show_rows
                .iter()
                .position(|row| crate::ansi::plain(&row.text).contains("+after"))
                .unwrap();
            let before = crate::ansi::plain(&app.show_rows[app.show_cursor].text);
            let fingerprint = watch_fingerprint().unwrap();
            // Changing only decorations must not discard loaded child patches.
            git(&["tag", "review-tag"]);
            let fingerprint_changed = watch_fingerprint().unwrap() != fingerprint;
            app.replace_commits(load_watch_log().unwrap());
            let after = crate::ansi::plain(&app.show_rows[app.show_cursor].text);
            let expanded = app
                .show_rows
                .iter()
                .any(|row| row.summary && !row.folded && row.text.contains("file.txt"));
            let decorations = app.show_text.contains("review-tag");
            (before, after, expanded, decorations, fingerprint_changed)
        };
        assert!(fingerprint_changed, "the tag must trigger a watch refresh");
        assert!(decorations, "Show must display the new tag");
        assert!(expanded, "watch refresh must keep the nested file expanded");
        assert_eq!(
            after, before,
            "watch refresh must preserve the reading line"
        );
    }

    #[test]
    fn binary_summaries_and_previews_ignore_configured_diff_prefixes() {
        let results = {
            let directory = TestDirectory::new();
            let _guard = CurrentDirGuard::enter(directory.path());
            let git = |args: &[&str]| {
                let output = Command::new("git").args(args).output().unwrap();
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
            };
            git(&["init", "-q"]);
            git(&["config", "user.name", "Test"]);
            git(&["config", "user.email", "test@example.com"]);
            git(&["config", "commit.gpgsign", "false"]);
            fs::create_dir("b").unwrap();
            let path = "b/picture space.png";
            fs::write(path, b"before\0image").unwrap();
            git(&["add", "."]);
            git(&["commit", "-qm", "base"]);
            fs::write(path, b"committed\0image").unwrap();
            git(&["commit", "-qam", "image"]);
            fs::write(path, b"staged\0image").unwrap();
            git(&["add", "."]);
            fs::write(path, b"worktree\0image").unwrap();
            let mut results = Vec::new();
            for (name, value) in [("diff.noprefix", "true"), ("diff.mnemonicPrefix", "true")] {
                git(&["config", name, value]);
                let apps = [
                    load_show_app(&["--stat".into()]).unwrap(),
                    load_diff_app(&["--stat".into()]).unwrap(),
                    load_diff_app(&["--stat".into(), "--cached".into()]).unwrap(),
                    load_diff_app(&["--stat".into(), "HEAD~..HEAD".into()]).unwrap(),
                ];
                for mut app in apps {
                    let summary = app.show_rows
                        [app.show_rows.iter().position(|r| r.summary).unwrap()]
                    .text
                    .clone();
                    app.images.enabled = true;
                    app.images.root = directory.path().to_owned();
                    app.show_cursor = app.show_rows.iter().position(|r| r.summary).unwrap();
                    app.toggle_show_file();
                    let previews = app.show_rows.iter().filter(|r| r.preview.is_some()).count();
                    results.push((name, summary, previews));
                }
                git(&["config", "--unset", name]);
            }
            results
        };
        // Assert outside the cwd guard so a failing regression cannot poison it.
        for (config, summary, previews) in results {
            assert_eq!(summary, "▶ b/picture space.png | binary", "{config}");
            assert_eq!(
                previews, 24,
                "{config}: both image previews must be available"
            );
        }
    }

    #[test]
    fn merge_resolution_has_expandable_file_summaries() {
        let mut app = {
            let directory = TestDirectory::new();
            let _guard = CurrentDirGuard::enter(directory.path());
            let git = |args: &[&str]| {
                let output = Command::new("git").args(args).output().unwrap();
                assert!(
                    output.status.success(),
                    "{}",
                    String::from_utf8_lossy(&output.stderr)
                );
            };
            git(&["init", "-q"]);
            git(&["config", "user.name", "Test"]);
            git(&["config", "user.email", "test@example.com"]);
            git(&["config", "commit.gpgsign", "false"]);
            fs::write("file.txt", "base\n").unwrap();
            git(&["add", "."]);
            git(&["commit", "-qm", "base"]);
            git(&["checkout", "-qb", "side"]);
            fs::write("file.txt", "side\n").unwrap();
            git(&["commit", "-qam", "side"]);
            git(&["checkout", "-qb", "other", "HEAD~"]);
            fs::write("file.txt", "other\n").unwrap();
            git(&["commit", "-qam", "other"]);
            let merge = Command::new("git")
                .args(["merge", "side"])
                .output()
                .unwrap();
            assert_eq!(merge.status.code(), Some(1));
            fs::write("file.txt", "resolved\n").unwrap();
            git(&["add", "."]);
            git(&["commit", "-qm", "merge"]);
            load_show_app(&["--stat".into()]).unwrap()
        };
        let summary = app
            .show_rows
            .iter()
            .position(|row| row.summary)
            .expect("merge file summary");
        assert_eq!(app.show_rows[summary].text, "▶ file.txt | +1 −2");
        assert!(!app
            .show_rows
            .iter()
            .any(|row| row.text.contains("++resolved")));
        app.show_cursor = summary;
        app.toggle_show_file();
        assert!(app
            .show_rows
            .iter()
            .any(|row| crate::ansi::plain(&row.text) == "++resolved"));
        app.toggle_show_file();
        assert!(app.show_rows[app.show_cursor].folded);
    }

    #[test]
    fn watch_refresh_updates_show_decorations_and_cached_views() {
        use crate::app::Mode;
        let directory = TestDirectory::new();
        let _guard = CurrentDirGuard::enter(directory.path());
        let git = |args: &[&str]| {
            let output = Command::new("git").args(args).output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&["config", "user.name", "Test"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "commit.gpgsign", "false"]);
        fs::write("file.txt", "before\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "initial"]);
        let mut app = crate::app::App::new(load_watch_log().unwrap());
        app.watch = true;
        app.selected = 1;
        app.switch_mode();
        let cursor = app.show_rows.iter().position(|row| row.folded).unwrap();
        app.show_cursor = cursor;
        git(&["tag", "live-tag"]);
        app.replace_commits(load_watch_log().unwrap());
        assert!(
            app.show_text.contains("live-tag"),
            "Show must refresh newly added decorations"
        );
        assert_eq!(app.show_cursor, cursor);
        app.switch_mode();
        assert_eq!(app.mode, Mode::Log);
        git(&["tag", "-d", "live-tag"]);
        app.replace_commits(load_watch_log().unwrap());
        app.switch_mode();
        assert!(
            !app.show_text.contains("live-tag"),
            "Reopening Show must not reuse stale decorations"
        );
    }

    struct WatchReadingContext {
        cursor_text: String,
        offset: usize,
        screen_position: isize,
        expanded_files: Vec<String>,
    }

    // Return snapshots before making regression assertions, so an expected
    // failure does not poison the process-wide current-directory lock.
    fn working_tree_watch_refresh(
        kind: CommitKind,
        insert_lines: bool,
    ) -> (WatchReadingContext, WatchReadingContext) {
        use ratatui::{backend::TestBackend, Terminal};

        let directory = TestDirectory::new();
        let _guard = CurrentDirGuard::enter(directory.path());
        let git = |args: &[&str]| {
            let output = Command::new("git").args(args).output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "Test"]);
        fs::write("Cargo.lock", "original\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "initial"]);
        let mut contents = String::new();
        for i in 0..30 {
            writeln!(contents, "dependency-{i:02}").unwrap();
        }
        fs::write("Cargo.lock", contents).unwrap();
        if kind == CommitKind::Staged {
            git(&["add", "Cargo.lock"]);
        }

        let mut app = crate::app::App::new(working_tree_entries().unwrap());
        app.watch = true;
        app.selected = app
            .commits
            .iter()
            .position(|commit| commit.kind == kind)
            .unwrap();
        app.switch_mode();
        app.show_cursor = app.show_rows.iter().position(|row| row.folded).unwrap();
        app.toggle_show_file();
        let mut terminal = Terminal::new(TestBackend::new(100, 8)).unwrap();
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        app.show_cursor = app
            .show_rows
            .iter()
            .position(|row| row.text.contains("dependency-15"))
            .unwrap();
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        app.scroll_show(2);
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();

        let snapshot = |app: &crate::app::App| WatchReadingContext {
            cursor_text: app.show_rows[app.show_cursor].text.clone(),
            offset: app.show_offset,
            screen_position: app.show_row_starts[app.show_cursor] as isize
                - app.show_offset as isize,
            expanded_files: app
                .show_rows
                .iter()
                .filter(|row| row.text.contains("diff --git") && row.text.contains("Cargo.lock"))
                .map(|row| row.text.clone())
                .collect(),
        };
        let before = snapshot(&app);
        assert!(before.offset > 0);
        assert_eq!(before.expanded_files.len(), 1);
        let fingerprint = watch_fingerprint().unwrap();
        if insert_lines {
            let contents = fs::read_to_string("Cargo.lock").unwrap();
            fs::write(
                "Cargo.lock",
                format!("new-one\nnew-two\nnew-three\n{contents}"),
            )
            .unwrap();
            if kind == CommitKind::Staged {
                git(&["add", "Cargo.lock"]);
            }
        } else {
            // An unrelated ref change triggers refresh while the patch stays identical.
            git(&["branch", "unrelated"]);
        }
        assert_ne!(watch_fingerprint().unwrap(), fingerprint);
        app.replace_commits(working_tree_entries().unwrap());
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        assert_eq!(app.commits[app.selected].kind, kind);
        (before, snapshot(&app))
    }

    #[test]
    fn watch_refresh_preserves_unstaged_reading_position() {
        let (before, after) = working_tree_watch_refresh(CommitKind::Unstaged, false);
        assert_eq!(
            (after.cursor_text, after.offset),
            (before.cursor_text, before.offset),
            "refresh must preserve the unstaged cursor and viewport"
        );
    }

    #[test]
    fn watch_refresh_preserves_staged_reading_position() {
        let (before, after) = working_tree_watch_refresh(CommitKind::Staged, false);
        assert_eq!(
            (after.cursor_text, after.offset),
            (before.cursor_text, before.offset),
            "refresh must preserve the staged cursor and viewport"
        );
    }

    #[test]
    fn watch_refresh_preserves_unstaged_expanded_folds() {
        let (before, after) = working_tree_watch_refresh(CommitKind::Unstaged, false);
        assert_eq!(
            after.expanded_files, before.expanded_files,
            "refresh must keep the unstaged lockfile expanded"
        );
    }

    #[test]
    fn watch_refresh_preserves_staged_expanded_folds() {
        let (before, after) = working_tree_watch_refresh(CommitKind::Staged, false);
        assert_eq!(
            after.expanded_files, before.expanded_files,
            "refresh must keep the staged lockfile expanded"
        );
    }

    #[test]
    fn watch_refresh_follows_staged_line_after_insertions() {
        let (before, after) = working_tree_watch_refresh(CommitKind::Staged, true);
        assert_eq!(after.cursor_text, before.cursor_text);
        assert_eq!(after.screen_position, before.screen_position);
        assert_eq!(after.expanded_files, before.expanded_files);
        assert!(
            after.offset > before.offset,
            "the viewport must follow the displaced line"
        );
    }

    #[test]
    fn watch_refresh_follows_unstaged_line_after_insertions() {
        let (before, after) = working_tree_watch_refresh(CommitKind::Unstaged, true);
        assert_eq!(after.cursor_text, before.cursor_text);
        assert_eq!(after.screen_position, before.screen_position);
        assert_eq!(after.expanded_files, before.expanded_files);
        assert!(
            after.offset > before.offset,
            "the viewport must follow the displaced line"
        );
    }

    #[test]
    fn status_arrows_navigate_virtual_commit_and_lazy_history() {
        use crate::{
            app::{App, Mode},
            input,
        };
        use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
        let directory = TestDirectory::new();
        let _guard = CurrentDirGuard::enter(directory.path());
        let git = |args: &[&str]| {
            let output = Command::new("git").args(args).output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "Test"]);
        let key = |code| Event::Key(KeyEvent::new(code, KeyModifiers::NONE));
        let mut unborn = App::new(vec![working_tree_commit("Clean")]);
        unborn.watch = true;
        unborn.pending_history = Some(Vec::new());
        unborn.open_status();
        input::handle(key(KeyCode::Right), &mut unborn);
        assert_eq!(unborn.mode, Mode::Status);
        assert_eq!(unborn.selected, 0);
        for subject in ["first", "second"] {
            fs::write("tracked.txt", subject).unwrap();
            git(&["add", "."]);
            git(&["commit", "-qm", subject]);
        }
        for lazy in [true, false] {
            let mut app = App::new(if lazy {
                vec![working_tree_commit("Clean")]
            } else {
                load_watch_log().unwrap()
            });
            app.watch = true;
            if lazy {
                app.pending_history = Some(Vec::new());
            }
            app.open_status();
            app.status_view.as_mut().unwrap().cursor = 1;
            input::handle(key(KeyCode::Right), &mut app);
            assert_eq!(app.mode, Mode::Show);
            assert_eq!(app.commits[app.selected].subject, "second");
            assert!(app.show_text.contains("second"));
            input::handle(key(KeyCode::Left), &mut app);
            assert_eq!(app.mode, Mode::Status);
            assert_eq!(app.selected, 0);
            assert_eq!(app.status_view.as_ref().unwrap().cursor, 1);
            input::handle(key(KeyCode::Left), &mut app);
            assert_eq!(app.mode, Mode::Status);
            assert_eq!(app.selected, 0);
            input::handle(
                Event::Key(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT)),
                &mut app,
            );
            assert_eq!(app.mode, Mode::Status);
            assert_eq!(app.status_view.as_ref().unwrap().horizontal, 4);
        }
    }

    #[test]
    fn status_load_failure_still_opens_status_and_retries() {
        use crate::{
            app::{App, Mode},
            input,
        };
        use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
        use ratatui::{backend::TestBackend, Terminal};
        let directory = TestDirectory::new();
        let _guard = CurrentDirGuard::enter(directory.path());
        let git = |args: &[&str]| {
            let output = Command::new("git").args(args).output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "Test"]);
        fs::write("tracked.txt", "contents\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "initial"]);
        let mut app = App::new(load_watch_log().unwrap());
        app.watch = true;
        app.selected = 1;
        app.mode = Mode::Show;
        app.load_show();
        assert!(app.show_text.contains("initial"));
        fs::write(".git/index", "corrupt").unwrap();
        input::handle(
            Event::Key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)),
            &mut app,
        );
        assert_eq!(app.mode, Mode::Status);
        assert_eq!(app.selected, 0);
        let view = app.status_view.as_mut().unwrap();
        assert!(view.error.is_some());
        let mut terminal = Terminal::new(TestBackend::new(80, 10)).unwrap();
        let heading = |view: &mut crate::status::StatusView,
                       terminal: &mut Terminal<TestBackend>| {
            terminal
                .draw(|frame| view.draw(frame, frame.area()))
                .unwrap();
            let buffer = terminal.backend().buffer();
            (0..80).map(|x| buffer[(x, 0)].symbol()).collect::<String>()
        };
        assert!(heading(view, &mut terminal).contains("Working tree status unavailable"));
        fs::remove_file(".git/index").unwrap();
        git(&["reset", "-q"]);
        view.refresh().unwrap();
        assert_eq!(view.error, None);
        assert!(heading(view, &mut terminal).contains("Working tree clean"));
    }

    #[cfg(unix)]
    #[test]
    fn images_preserve_exact_repository_paths() {
        let directory = TestDirectory::new();
        for root in exact_repository_paths(directory.path()) {
            let (actual, expected) = {
                let _guard = CurrentDirGuard::enter(&root);
                assert!(Command::new("git")
                    .args(["init", "-q"])
                    .status()
                    .unwrap()
                    .success());
                let setting = env::var_os("GLOG_IMAGES");
                env::set_var("GLOG_IMAGES", "kitty");
                let images = crate::images::Images::from_env();
                match setting {
                    Some(value) => env::set_var("GLOG_IMAGES", value),
                    None => env::remove_var("GLOG_IMAGES"),
                }
                (images.root, env::current_dir().unwrap())
            };
            assert_eq!(actual, expected, "image sources must use the exact root");
        }
    }

    #[cfg(unix)]
    #[test]
    fn submodules_preserve_exact_repository_paths() {
        let directory = TestDirectory::new();
        for root in exact_repository_paths(directory.path()) {
            let result = {
                let _guard = CurrentDirGuard::enter(&root);
                let git = |args: &[&str]| {
                    let output = Command::new("git")
                        .args([
                            "-c",
                            "user.name=Test",
                            "-c",
                            "user.email=test@example.com",
                            "-c",
                            "commit.gpgsign=false",
                        ])
                        .args(args)
                        .output()
                        .unwrap();
                    assert!(output.status.success(), "{:?}", output);
                    String::from_utf8(output.stdout).unwrap().trim().to_owned()
                };
                git(&["init", "-q"]);
                git(&["init", "-q", "module"]);
                fs::write("module/file.txt", "before\n").unwrap();
                git(&["-C", "module", "add", "."]);
                git(&["-C", "module", "commit", "-qm", "before"]);
                let old = git(&["-C", "module", "rev-parse", "HEAD"]);
                fs::write("module/file.txt", "after\n").unwrap();
                git(&["-C", "module", "commit", "-qam", "after"]);
                let new = git(&["-C", "module", "rev-parse", "HEAD"]);
                show_submodule(None, "module", &old, &new)
            };
            let (module, patch) = result.expect("submodule lookup must use the exact root");
            assert_eq!(module, root.join("module").canonicalize().unwrap());
            let plain = crate::ansi::plain(&patch);
            assert!(plain.contains("-before"));
            assert!(plain.contains("+after"));
        }
    }

    #[cfg(unix)]
    fn exact_repository_paths(parent: &Path) -> Vec<PathBuf> {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
        let mut roots = Vec::new();
        for name in [b"repo\n\n".as_slice(), b"repo ", b"repo-\xff"] {
            let root = parent.join(OsStr::from_bytes(name));
            if let Err(error) = fs::create_dir(&root) {
                // The default macOS filesystem rejects non-UTF-8 names.
                #[cfg(target_os = "macos")]
                if error.raw_os_error() == Some(92) {
                    continue;
                }
                panic!("could not create repository fixture: {error}");
            }
            roots.push(root);
        }
        roots
    }

    #[cfg(unix)]
    #[test]
    fn status_preserves_repository_path_bytes_and_trailing_newlines() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
        let directory = TestDirectory::new();
        for name in [b"repo\n\n".as_slice(), b"repo-\xff".as_slice()] {
            let root = directory.path().join(OsStr::from_bytes(name));
            if let Err(error) = fs::create_dir(&root) {
                // The default macOS filesystem rejects non-UTF-8 names.
                #[cfg(target_os = "macos")]
                if error.raw_os_error() == Some(92) {
                    continue;
                }
                panic!("could not create repository fixture: {error}");
            }
            let _guard = CurrentDirGuard::enter(&root);
            assert!(Command::new("git")
                .args(["init", "-q"])
                .status()
                .unwrap()
                .success());
            fs::write("new.txt", "contents\n").unwrap();
            let mut view = crate::status::StatusView::load()
                .expect("Status must preserve the repository's exact path");
            assert_eq!(view.summary(), "1 untracked file");
            fs::write("other.txt", "more contents\n").unwrap();
            view.refresh().unwrap();
            assert_eq!(view.summary(), "2 untracked files");
        }
    }

    #[test]
    fn status_loads_from_subdirectory_and_refreshes_expanded_patches() {
        use ratatui::{backend::TestBackend, Terminal};
        let directory = TestDirectory::new();
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .current_dir(directory.path())
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "Test"]);
        fs::write(directory.path().join("tracked.txt"), "original\n").unwrap();
        fs::write(directory.path().join("rename.txt"), "rename content\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "initial"]);
        fs::create_dir(directory.path().join("nested")).unwrap();
        let _guard = CurrentDirGuard::enter(&directory.path().join("nested"));
        let mut view = crate::status::StatusView::load().unwrap();
        let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
        let render = |view: &mut crate::status::StatusView,
                      terminal: &mut Terminal<TestBackend>| {
            terminal
                .draw(|frame| view.draw(frame, frame.area()))
                .unwrap();
            (0..40)
                .map(|y| {
                    (0..120)
                        .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
        };
        assert!(render(&mut view, &mut terminal)[0].contains("Working tree clean"));
        git(&["mv", "rename.txt", "moved.txt"]);
        fs::write(directory.path().join("tracked.txt"), "staged-content\n").unwrap();
        git(&["add", "tracked.txt"]);
        fs::write(directory.path().join("tracked.txt"), "unstaged-content\n").unwrap();
        fs::write(directory.path().join("new file.txt"), "untracked-content\n").unwrap();
        view.refresh().unwrap();
        let rows = render(&mut view, &mut terminal);
        assert!(rows.iter().any(|row| row.contains("Staged (2)")));
        assert!(rows.iter().any(|row| row.contains("Unstaged (1)")));
        assert!(rows.iter().any(|row| row.contains("Untracked (1)")));
        assert_eq!(
            rows.iter()
                .filter(|row| row.contains("modified  tracked.txt"))
                .count(),
            2
        );
        view.cursor = rows
            .iter()
            .position(|row| row.contains("renamed  rename.txt → moved.txt"))
            .unwrap()
            - 1;
        // Regular patches, including renames, start expanded.
        assert!(render(&mut view, &mut terminal)
            .iter()
            .any(|row| row.contains("rename from rename.txt")));
        // Collapse the rename, then load and refresh an untracked file lazily.
        view.toggle();
        let rows = render(&mut view, &mut terminal);
        view.cursor = rows
            .iter()
            .position(|row| row.contains("new  new file.txt"))
            .unwrap()
            - 1;
        view.toggle();
        assert!(render(&mut view, &mut terminal)
            .iter()
            .any(|row| row.contains("+untracked-content")));
        fs::write(directory.path().join("new file.txt"), "updated-content\n").unwrap();
        view.refresh().unwrap();
        assert!(render(&mut view, &mut terminal)
            .iter()
            .any(|row| row.contains("+updated-content")));
        fs::remove_file(directory.path().join("new file.txt")).unwrap();
        view.refresh().unwrap();
        assert!(!render(&mut view, &mut terminal)
            .iter()
            .any(|row| row.contains("new file.txt")));
    }

    #[test]
    fn log_grep_can_search_for_a_display_option() {
        let directory = TestDirectory::new();
        let _guard = CurrentDirGuard::enter(directory.path());
        for args in [
            vec!["init", "-q"],
            vec![
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "--allow-empty",
                "-qm",
                "Document --oneline usage",
            ],
        ] {
            let output = Command::new("git").args(args).output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let (_, args) =
            crate::log_format::parse_args(&["--grep".into(), "--oneline".into()]).unwrap();
        let commits = load_log(&args).unwrap();
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].subject, "Document --oneline usage");
    }

    #[test]
    fn ordinary_log_excludes_working_tree_entries_with_display_options() {
        let directory = TestDirectory::new();
        let _guard = CurrentDirGuard::enter(directory.path());
        let git = |args: &[&str]| {
            let output = Command::new("git").args(args).output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "Test"]);
        fs::write("tracked.txt", "original\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "first"]);
        fs::write("tracked.txt", "staged\n").unwrap();
        git(&["add", "."]);
        fs::write("tracked.txt", "unstaged\n").unwrap();

        for options in [
            vec![],
            vec!["--date=short"],
            vec!["--date", "short"],
            vec!["--relative-date"],
            vec!["--date=relative", "--date=short"],
        ] {
            let mut args = vec!["--format=%h %ad %s".to_owned()];
            args.extend(options.iter().map(|arg| (*arg).to_owned()));
            let (_, args) = crate::log_format::parse_args(&args).unwrap();
            let commits = load_log(&args).unwrap();
            assert_eq!(
                commits.iter().map(|commit| commit.kind).collect::<Vec<_>>(),
                [CommitKind::Revision],
                "{options:?} must only change date display"
            );
            if options.last() == Some(&"short") || options.last() == Some(&"--date=short") {
                assert_eq!(commits[0].author_date.len(), 10);
            }
            for restriction in [
                vec!["HEAD"],
                vec!["-1"],
                vec!["--", "tracked.txt"],
                vec!["--", "--date=short"],
            ] {
                let mut restricted = args.clone();
                restricted.extend(restriction.into_iter().map(str::to_owned));
                assert!(load_log(&restricted)
                    .unwrap()
                    .iter()
                    .all(|commit| commit.kind == CommitKind::Revision));
            }
        }
        // Git log can read history even when the index cannot be inspected.
        fs::write(".git/index", "invalid index").unwrap();
        assert_eq!(load_log(&[]).unwrap().len(), 1);
        assert!(load_watch_log().is_err());
    }

    #[test]
    fn show_pathspecs_filter_staged_unstaged_and_untracked_patches() {
        let directory = TestDirectory::new();
        let _guard = CurrentDirGuard::enter(directory.path());
        let git = |args: &[&str]| {
            let output = Command::new("git").args(args).output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "Test"]);
        for dir in ["wanted", "unrelated"] {
            fs::create_dir(dir).unwrap();
            fs::write(format!("{dir}/tracked.txt"), "original\n").unwrap();
        }
        git(&["add", "."]);
        git(&["commit", "-qm", "first"]);
        for dir in ["wanted", "unrelated"] {
            fs::write(format!("{dir}/tracked.txt"), "staged\n").unwrap();
        }
        git(&["add", "."]);
        for dir in ["wanted", "unrelated"] {
            fs::write(format!("{dir}/tracked.txt"), "unstaged\n").unwrap();
            fs::write(format!("{dir}/new.txt"), "untracked\n").unwrap();
        }

        for paths in [vec!["wanted/"], vec![".", ":(exclude)unrelated/"]] {
            let mut args = vec!["--".to_owned()];
            args.extend(paths.into_iter().map(str::to_owned));
            let mut app = load_show_app(&args).unwrap();
            assert!(app.show_text.contains("wanted/tracked.txt"));
            assert!(!app.show_text.contains("unrelated/"));
            app.switch_mode();
            assert!(app
                .commits
                .iter()
                .all(|commit| commit.kind == CommitKind::Revision));
            let mut patches = Vec::new();
            // Path filtering remains available to the patch loader, independently
            // of which entries the ordinary Log exposes.
            for kind in [CommitKind::Staged, CommitKind::Unstaged] {
                let entry = working_tree_entries()
                    .unwrap()
                    .into_iter()
                    .find(|entry| entry.kind == kind)
                    .unwrap();
                app.show_text = show(&entry, &app.show_paths).unwrap();
                assert!(app.show_text.contains("wanted/tracked.txt"));
                if kind == CommitKind::Unstaged {
                    assert!(app.show_text.contains("wanted/new.txt"));
                    assert!(app.show_text.contains("glog-lazy-untracked:"));
                }
                patches.push((kind, app.show_text.clone()));
            }
            for (kind, patch) in patches {
                assert!(
                    !patch.contains("unrelated/"),
                    "{kind:?} ignored pathspecs: {patch}"
                );
            }
        }
    }

    #[test]
    fn untracked_nested_repositories_report_why_they_have_no_patch() {
        let directory = TestDirectory::new();
        for repository in [directory.path().to_owned(), directory.path().join("nested")] {
            let status = Command::new("git")
                .args(["init", "-q"])
                .arg(&repository)
                .status()
                .unwrap();
            assert!(status.success());
        }
        fs::write(directory.path().join("nested/file.txt"), "nested\n").unwrap();

        let error = show_untracked_in(directory.path(), Path::new("nested/")).unwrap_err();
        assert!(error.contains("nested/"), "{error}");
        assert!(error.contains("repository"), "{error}");
    }

    #[test]
    fn unstaged_patches_label_untracked_files_relative_to_the_repository() {
        let directory = TestDirectory::new();
        let git = |args: &[&str]| {
            let output = Command::new("git")
                .current_dir(directory.path())
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "Test"]);
        fs::write(directory.path().join("a.lock"), "original\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "initial"]);
        fs::write(directory.path().join("a.lock"), "modified\n").unwrap();
        fs::create_dir(directory.path().join("nested")).unwrap();
        fs::write(directory.path().join("nested/a.lock"), "nested\n").unwrap();
        fs::write(directory.path().join("outside.txt"), "outside\n").unwrap();
        let _guard = CurrentDirGuard::enter(&directory.path().join("nested"));

        let entry = working_tree_entries()
            .unwrap()
            .into_iter()
            .find(|entry| entry.kind == CommitKind::Unstaged)
            .unwrap();
        let text = show(&entry, &[]).unwrap();
        let files = crate::diff::file_sections(&text);
        let paths: Vec<_> = files.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(paths, ["a.lock", "nested/a.lock", "outside.txt"]);
        for file in files.iter().filter(|file| file.untracked) {
            let path = file.lazy_untracked_path.as_deref().unwrap();
            assert!(show_untracked(&raw_path(path))
                .unwrap()
                .contains(&format!("b/{}", file.path)));
        }
    }

    #[test]
    fn show_opens_exact_commit_and_loads_history_on_navigation() {
        let directory = TestDirectory::new();
        let _guard = CurrentDirGuard::enter(directory.path());
        let git = |args: &[&str]| {
            let output = Command::new("git").args(args).output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "Test"]);
        fs::write("first.txt", "first\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "first"]);
        git(&["tag", "-a", "first-tag", "-m", "tag"]);
        fs::write("second.txt", "second\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "second"]);
        fs::write("first.txt", "staged\n").unwrap();
        git(&["add", "first.txt"]);
        fs::write("dirty.txt", "dirty\n").unwrap();

        let mut app = load_show_app(&[]).unwrap();
        assert_eq!(app.mode, crate::app::Mode::Show);
        assert_eq!(app.commits.len(), 1);
        assert_eq!(app.commits[0].subject, "second");
        assert!(app.show_text.contains("second.txt"));
        assert!(app.pending_history.is_some());
        app.switch_mode();
        assert!(app
            .commits
            .iter()
            .all(|commit| commit.kind == CommitKind::Revision));
        assert_eq!(app.selected, 0);
        assert_eq!(app.commits[app.selected].subject, "second");
        app.switch_mode();
        assert!(app.move_selection(1));
        assert_eq!(app.commits[app.selected].subject, "first");
        assert!(app.pending_history.is_none());

        // A path absent from the selected commit must not select an ancestor.
        let mut app = load_show_app(&["HEAD".into(), "--".into(), "first.txt".into()]).unwrap();
        assert_eq!(app.commits[0].subject, "second");
        assert!(!app.show_text.contains("second.txt"));
        app.switch_mode();
        assert_eq!(app.mode, crate::app::Mode::Log);
        assert_eq!(app.commits.len(), 2);
        assert_eq!(app.selected, 0);

        for args in [
            vec!["--stat"],
            vec!["--stat", "HEAD"],
            vec!["HEAD", "--stat"],
            vec!["--stat", "HEAD", "--", "second.txt"],
        ] {
            let mut summary =
                load_show_app(&args.into_iter().map(str::to_owned).collect::<Vec<_>>()).unwrap();
            assert!(summary.show_stat);
            assert!(summary
                .show_rows
                .iter()
                .any(|row| row.summary && row.folded && row.text.contains("second.txt")));
            assert!(!summary
                .show_rows
                .iter()
                .any(|row| row.text.contains("diff --git")));
            summary.show_cursor = summary
                .show_rows
                .iter()
                .position(|row| row.summary)
                .unwrap();
            summary.toggle_show_file();
            assert!(summary
                .show_rows
                .iter()
                .any(|row| row.summary && !row.folded));
            assert!(summary
                .show_rows
                .iter()
                .any(|row| row.text.contains("diff --git")));
        }
        // Options after -- are pathspecs, even when named like presentation flags.
        let summary_path = load_show_app(&["--".into(), "--stat".into()]).unwrap();
        assert!(!summary_path.show_stat);
        assert!(!summary_path.show_rows.iter().any(|row| row.summary));

        let app = load_show_app(&["first-tag".into()]).unwrap();
        assert_eq!(app.commits[0].subject, "first");
        for args in [
            vec!["missing"],
            vec!["HEAD", "HEAD~1"],
            vec!["HEAD~1..HEAD"],
            vec!["--watch"],
            vec!["HEAD:first.txt"],
        ] {
            assert!(
                load_show_app(&args.into_iter().map(str::to_owned).collect::<Vec<_>>()).is_err()
            );
        }
    }

    #[test]
    fn search_selects_unloaded_untracked_files_without_revealing_placeholders() {
        let directory = TestDirectory::new();
        let _guard = CurrentDirGuard::enter(directory.path());
        let git = |args: &[&str]| {
            let output = Command::new("git").args(args).output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "Test"]);
        git(&["config", "commit.gpgsign", "false"]);
        fs::write("tracked.txt", "one\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "first"]);
        fs::write("tracked.txt", "two\n").unwrap();
        fs::write("new.txt", "untracked\n").unwrap();
        for args in [vec![], vec!["--stat".to_owned()]] {
            let mut app = load_diff_app(&args).unwrap();
            // "7874" appears only in the hex-encoded placeholder path.
            app.search = Some("7874".into());
            app.next_match(false);
            assert!(
                !app.show_rows
                    .iter()
                    .any(|row| row.text.contains("glog-lazy-untracked:")),
                "{args:?}"
            );
            app.search = Some("new.txt".into());
            app.next_match(false);
            let row = &app.show_rows[app.show_cursor];
            assert!(row.folded, "{args:?}: {}", row.text);
            assert!(row.text.contains("new.txt"), "{args:?}: {}", row.text);
            assert!(
                !app.show_rows
                    .iter()
                    .any(|row| row.text.contains("glog-lazy-untracked:")),
                "{args:?}"
            );
        }
    }
    #[test]
    fn diff_opens_requested_changes_and_switches_to_committed_history() {
        let directory = TestDirectory::new();
        let _guard = CurrentDirGuard::enter(directory.path());
        let git = |args: &[&str]| {
            let output = Command::new("git").args(args).output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "Test"]);
        let cached = ["--cached".to_owned()];
        assert!(load_diff_app(&[]).unwrap().commits.is_empty());
        assert!(load_diff_app(&cached).unwrap().commits.is_empty());
        fs::write("new.txt", "untracked\n").unwrap();
        assert!(load_log(&[]).unwrap().is_empty());
        assert_eq!(load_watch_log().unwrap()[0].kind, CommitKind::WorkingTree);
        let mut app = load_diff_app(&[]).unwrap();
        assert_eq!(app.mode, crate::app::Mode::Show);
        assert_eq!(app.commits[0].kind, CommitKind::Unstaged);
        assert!(app.show_text.contains("glog-lazy-untracked:"));
        app.switch_mode();
        assert!(app.commits.is_empty()); // Unborn HEAD has no committed history.
        git(&["add", "new.txt"]);
        assert!(load_diff_app(&[]).unwrap().commits.is_empty());
        assert_eq!(
            load_diff_app(&cached).unwrap().commits[0].kind,
            CommitKind::Staged
        );
        git(&["commit", "-qm", "first"]);
        assert!(load_diff_app(&[]).unwrap().commits.is_empty());
        assert!(load_diff_app(&cached).unwrap().commits.is_empty());
        fs::write("new.txt", "staged-content\n").unwrap();
        git(&["add", "new.txt"]);
        fs::write("new.txt", "unstaged-content\n").unwrap();
        for (args, kind, content) in [
            (&[][..], CommitKind::Unstaged, "unstaged-content"),
            (&cached[..], CommitKind::Staged, "staged-content"),
        ] {
            let mut app = load_diff_app(args).unwrap();
            assert_eq!(app.mode, crate::app::Mode::Show);
            assert_eq!(app.commits.len(), 1);
            assert_eq!(app.commits[0].kind, kind);
            assert!(crate::ansi::plain(&app.show_text).contains(content));
            assert!(app.pending_history.is_some());
            app.switch_mode();
            assert_eq!(app.commits.len(), 1);
            assert_eq!(app.selected, 0);
            assert_eq!(app.commits[app.selected].kind, CommitKind::Revision);
            assert!(app.pending_history.is_none());
            for direction in [-1, 1] {
                let mut direct = load_diff_app(args).unwrap();
                assert!(direct.move_selection(direction));
                assert_eq!(direct.commits[direct.selected].kind, CommitKind::Revision);
                assert_eq!(direct.commits[direct.selected].subject, "first");
            }
        }
        for (args, kind) in [
            (vec!["--stat"], CommitKind::Unstaged),
            (vec!["--stat", "--cached"], CommitKind::Staged),
            (vec!["--cached", "--stat"], CommitKind::Staged),
        ] {
            let summary =
                load_diff_app(&args.into_iter().map(str::to_owned).collect::<Vec<_>>()).unwrap();
            assert!(summary.show_stat);
            assert_eq!(summary.commits[0].kind, kind);
            assert!(summary
                .show_rows
                .iter()
                .any(|row| row.summary && row.folded));
            assert!(!summary
                .show_rows
                .iter()
                .any(|row| row.text.contains("diff --git")));
        }
        assert!(load_diff_app(&["HEAD".into()]).is_ok());
        assert!(load_diff_app(&["--watch".into()]).is_err());
    }

    #[test]
    fn single_commit_diff_previews_recorded_images_after_worktree_edits() {
        let directory = TestDirectory::new();
        let _guard = CurrentDirGuard::enter(directory.path());
        let git = |args: &[&str]| {
            let output = Command::new("git").args(args).output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "Test"]);
        let save_image = |color| {
            image::RgbaImage::from_pixel(2, 2, image::Rgba(color))
                .save("picture.png")
                .unwrap();
        };
        save_image([255, 0, 0, 255]);
        git(&["add", "."]);
        git(&["commit", "-qm", "red"]);
        let before = git(&["rev-parse", "HEAD:picture.png"]);
        save_image([0, 0, 255, 255]);
        git(&["commit", "-qam", "blue"]);
        let after = git(&["rev-parse", "HEAD:picture.png"]);
        save_image([0, 255, 0, 255]);

        for revision in ["HEAD^!", "HEAD^-", "HEAD^..HEAD"] {
            let mut app = load_diff_app(&[revision.into()]).unwrap();
            app.images.enabled = true;
            app.images.root = directory.path().to_owned();
            app.show_rows.clear();
            app.ensure_show_rows();
            let sources: Vec<_> = app
                .show_rows
                .iter()
                .filter_map(|row| row.preview.as_ref())
                .filter(|preview| preview.row == 0)
                .map(|preview| preview.source.clone())
                .collect();
            assert_eq!(sources, vec![
                crate::images::Source::Blob(directory.path().to_owned(), before.clone()),
                crate::images::Source::Blob(directory.path().to_owned(), after.clone()),
            ], "{revision} must preview the committed red and blue images, not the green working file");
        }
    }

    #[test]
    fn diff_compares_revisions_paths_and_image_sources() {
        let directory = TestDirectory::new();
        let _guard = CurrentDirGuard::enter(directory.path());
        let git = |args: &[&str]| {
            let output = Command::new("git").args(args).output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        let open = |args: &[&str]| {
            load_diff_app(&args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>()).unwrap()
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "Test"]);
        fs::write("note.txt", "base\n").unwrap();
        fs::write("picture.png", b"base\0image").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "base"]);
        git(&["tag", "base"]);
        git(&["checkout", "-qb", "left"]);
        fs::write("note.txt", "left\n").unwrap();
        fs::write("picture.png", b"left\0image").unwrap();
        git(&["commit", "-qam", "left"]);
        git(&["checkout", "-qb", "right", "base"]);
        fs::write("note.txt", "right\n").unwrap();
        fs::write("picture.png", b"right\0image").unwrap();
        fs::write("--stat", "flag-named path\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "right"]);
        fs::write("note.txt", "index\n").unwrap();
        fs::write("picture.png", b"index\0image").unwrap();
        git(&["add", "."]);
        fs::write("note.txt", "worktree\n").unwrap();
        fs::write("picture.png", b"worktree\0image").unwrap();
        fs::write("untracked.txt", "untracked\n").unwrap();

        for (args, before, after, worktree) in [
            (vec!["left.."], "left", "right", false),
            (vec!["..left"], "right", "left", false),
            (vec!["left..right"], "left", "right", false),
            (vec!["left", "right"], "left", "right", false),
            (vec!["left...right"], "base", "right", false),
            (vec!["left..."], "base", "right", false),
            (vec!["...left"], "base", "left", false),
            (vec!["left^!"], "base", "left", false),
            (vec!["right^-"], "base", "right", false),
            (vec!["right^@"], "base", "worktree", true),
            (vec!["base^!"], "base", "worktree", true),
            (vec!["base^@"], "index", "worktree", true),
            (vec!["left"], "left", "worktree", true),
            (vec!["--cached", "left"], "left", "index", false),
        ] {
            let mut app = open(&args);
            let patch = crate::ansi::plain(&app.show_text);
            assert!(patch.contains(&format!("-{before}\n")), "{args:?}: {patch}");
            assert!(patch.contains(&format!("+{after}\n")), "{args:?}: {patch}");
            assert!(!patch.contains("untracked.txt"));
            assert_eq!(app.commits[0].kind, CommitKind::Comparison { worktree });
            // Enabling previews must select historical blobs for commit/index
            // comparisons and the current file only for worktree comparisons.
            app.images.enabled = true;
            app.images.root = directory.path().to_owned();
            app.show_rows.clear();
            app.ensure_show_rows();
            let previews: Vec<_> = app
                .show_rows
                .iter()
                .filter_map(|row| row.preview.as_ref())
                .collect();
            assert!(!previews.is_empty());
            assert_eq!(
                previews
                    .iter()
                    .any(|preview| matches!(preview.source, crate::images::Source::File(..))),
                worktree
            );
            assert!(!app.has_log_view());
            assert!(app.pending_history.is_none());
            let original = app.show_text.clone();
            app.switch_mode();
            assert_eq!(app.mode, crate::app::Mode::Show);
            assert_eq!(app.show_text, original);
        }
        let mut summary = open(&["left..right", "--stat", "--", "note.txt"]);
        assert!(summary.show_stat);
        assert!(!summary.show_text.contains("picture.png"));
        assert!(summary
            .show_rows
            .iter()
            .any(|row| row.summary && row.folded));
        summary.show_cursor = summary
            .show_rows
            .iter()
            .position(|row| row.summary)
            .unwrap();
        summary.toggle_show_file();
        assert!(summary
            .show_rows
            .iter()
            .any(|row| crate::ansi::plain(&row.text).contains("+right")));
        let named_flag = open(&["base..right", "--", "--stat"]);
        assert!(!named_flag.show_stat);
        assert!(named_flag.show_text.contains("flag-named path"));
        assert!(open(&["left..left"]).commits.is_empty());
        assert!(open(&["left..right", "--", "missing.txt"])
            .commits
            .is_empty());
        assert!(open(&["--", "missing.txt"]).commits.is_empty());
        assert!(open(&["--", "untracked.txt"])
            .show_text
            .contains("glog-lazy-untracked:"));
        assert!(
            crate::ansi::plain(&open(&["--cached", "--", "note.txt"]).show_text).contains("+index")
        );
        assert!(!open(&["--cached", "--", "note.txt"])
            .show_text
            .contains("picture.png"));
        // Like git diff, paths may follow revisions without a separator.
        for (args, before, after) in [
            (vec!["note.txt"], "index", "worktree"),
            (vec!["--cached", "note.txt"], "right", "index"),
            (vec!["left", "right", "note.txt"], "left", "right"),
        ] {
            let patch = crate::ansi::plain(&open(&args).show_text);
            assert!(patch.contains(&format!("-{before}\n")), "{args:?}: {patch}");
            assert!(patch.contains(&format!("+{after}\n")), "{args:?}: {patch}");
            assert!(!patch.contains("picture.png"), "{args:?}: {patch}");
        }
        let summary = open(&["left..right", "--stat", "note.txt"]);
        assert!(summary.show_stat);
        assert!(!summary.show_text.contains("picture.png"));
        assert_eq!(
            open(&["left", "right", "note.txt"]).commits[0].subject,
            "Diff left right"
        );
        let mut navigation = open(&["left..right"]);
        assert!(!navigation.move_selection(1));
        assert!(!navigation.move_selection(-1));
        assert_eq!(
            navigation.commits[navigation.selected].subject,
            "Diff left..right"
        );
        for args in [
            vec!["missing"],
            vec!["--watch"],
            vec!["left", "right", "base"],
            vec!["--cached", "left", "right"],
            vec!["left..right", "note.txt", "--stat"],
            vec!["left..right", "missing.txt"],
        ] {
            assert!(
                load_diff_app(&args.into_iter().map(str::to_owned).collect::<Vec<_>>()).is_err()
            );
        }
    }

    #[test]
    fn loads_format_metadata_with_git_date_options() {
        let directory = TestDirectory::new();
        let _guard = CurrentDirGuard::enter(directory.path());
        assert!(Command::new("git")
            .args(["init", "-q"])
            .status()
            .unwrap()
            .success());
        let output = Command::new("git")
            .args([
                "-c",
                "user.name=Alice",
                "-c",
                "user.email=alice@example.com",
                "commit",
                "--allow-empty",
                "-qm",
                "A subject",
            ])
            .env("GIT_AUTHOR_DATE", "2001-09-14T12:00:00+03:00")
            .env("GIT_COMMITTER_DATE", "2001-09-14T12:00:00+03:00")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        // When no setting is inherited, verify the fallback in two timezones
        // without changing the test process's environment.
        let read_date = |args: &[String], timezone: &str| {
            let output = log_command(args)
                .unwrap()
                .env("TZ", timezone)
                .output()
                .unwrap();
            assert!(output.status.success());
            parse_log(&String::from_utf8_lossy(&output.stdout)).unwrap()[0]
                .author_date
                .clone()
        };
        let configured_date = Command::new("git")
            .args(["config", "--get", "log.date"])
            .output()
            .unwrap();
        if configured_date.status.code() == Some(1) {
            assert_eq!(read_date(&[], "UTC0"), "2001-09-14 09:00");
            assert_eq!(read_date(&[], "EST5"), "2001-09-14 04:00");
        }
        assert!(Command::new("git")
            .args(["config", "log.date", "format:%Y/%m/%d"])
            .status()
            .unwrap()
            .success());
        assert_eq!(read_date(&[], "UTC0"), "2001/09/14");
        assert_eq!(
            read_date(&["--date".into(), "short".into()], "UTC0"),
            "2001-09-14"
        );
        assert!(read_date(&["--relative-date".into()], "UTC0").contains("ago"));
        let (format, args) = crate::log_format::parse_args(&[
            "--pretty=format:%h %ad (%an) %s".into(),
            "--date=short".into(),
        ])
        .unwrap();
        let commits = load_log(&args).unwrap();
        let commit = &commits[0];
        assert_eq!(commit.author, "Alice");
        assert_eq!(commit.author_email, "alice@example.com");
        assert_eq!(commit.author_date, "2001-09-14");
        assert_eq!(
            format.text(commit),
            format!("{} 2001-09-14 (Alice) A subject", commit.short_hash)
        );
        assert_eq!(
            load_log(&["--date=format:%Y".into()]).unwrap()[0].author_date,
            "2001"
        );
    }

    #[test]
    fn multiline_dates_report_an_error_instead_of_hiding_commits() {
        let directory = TestDirectory::new();
        let _guard = CurrentDirGuard::enter(directory.path());
        for args in [
            vec!["init", "-q"],
            vec![
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "--allow-empty",
                "-qm",
                "Visible commit",
            ],
        ] {
            let output = Command::new("git").args(args).output().unwrap();
            assert!(output.status.success());
        }

        for date in ["format:%Y%n%m", "format:%Y\n%m", "format:%Y\r%m"] {
            let error = load_log(&[format!("--date={date}")]).unwrap_err();
            assert!(error.contains("one line"), "{error}");
            assert!(Command::new("git")
                .args(["config", "log.date", date])
                .status()
                .unwrap()
                .success());
            for options in [vec![], vec!["--oneline".into()]] {
                let (_, args) = crate::log_format::parse_args(&options).unwrap();
                let error = load_log(&args).unwrap_err();
                assert!(error.contains("one line"), "{error}");
            }
            let error = load_show_app(&[])
                .err()
                .expect("Show must reject multiline dates");
            assert!(error.contains("one line"), "{error}");

            // A valid explicit option still overrides the invalid configuration.
            let commits = load_log(&["--date=short".into()]).unwrap();
            assert_eq!(commits.len(), 1);
            assert_eq!(commits[0].subject, "Visible commit");
        }
    }

    #[test]
    fn codex_noreply_address_is_recognized_and_deduplicated() {
        for trailers in [
            "Codex <noreply@openai.com>".to_owned(),
            ["Codex <NOREPLY@OPENAI.COM>", "Codex <codex@openai.com>"].join(&COAUTHOR.to_string()),
        ] {
            assert_eq!(
                Collaborators::parse(&trailers, "yairchu@gmail.com"),
                Collaborators {
                    codex: true,
                    claude: false,
                    others: 0,
                }
            );
        }
    }

    #[test]
    fn collaborator_identities_are_deduplicated_and_match_exact_emails() {
        let trailers = [
            "Codex <CODEX@OPENAI.COM>",
            "Codex <codex@openai.com>",
            "Claude Opus <noreply@anthropic.com>",
            "Claude Code <noreply@anthropic.com>",
            "Alice <ALICE@example.com>",
            "Bob <bob@example.com>",
            "Bob again <BOB@example.com>",
            "Codex <human@example.com>",
            "not an identity",
            "Missing close <missing@example.com",
        ]
        .join(&COAUTHOR.to_string());
        assert_eq!(
            Collaborators::parse(&trailers, "alice@example.com"),
            Collaborators {
                codex: true,
                claude: true,
                others: 2,
            }
        );
        assert_eq!(
            Collaborators::parse("", "alice@example.com"),
            Collaborators::default()
        );
    }

    #[test]
    fn loads_coauthor_trailers_without_turning_message_text_into_graph_rows() {
        let directory = TestDirectory::new();
        let _guard = CurrentDirGuard::enter(directory.path());
        assert!(Command::new("git")
            .args(["init", "-q"])
            .status()
            .unwrap()
            .success());
        for message in [
            "Mentions only\n\nThis discusses Codex and Claude Code without crediting either.",
            "Collaborative change\n\nCo-authored-by: Codex <codex@openai.com>\nco-authored-by: Claude Code <noreply@anthropic.com>\nCo-authored-by: Bob <bob@example.com>\nCo-authored-by: Bob again <BOB@example.com>\nCo-authored-by: Alice <alice@example.com>\nReviewed-by: Other <other@example.com>",
        ] {
            let output = Command::new("git").args([
                "-c", "user.name=Alice", "-c", "user.email=alice@example.com",
                "commit", "--allow-empty", "-qm", message,
            ]).output().unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        }
        let commits = load_log(&[]).unwrap();
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].subject, "Collaborative change");
        assert_eq!(
            commits[0].collaborators,
            Collaborators {
                codex: true,
                claude: true,
                others: 1
            }
        );
        assert_eq!(commits[1].collaborators, Collaborators::default());
        assert!(commits.iter().all(|commit| commit.graph.len() == 1));
    }

    #[test]
    fn parses_full_identity_and_graph_lines() {
        let input = "|\\\n* \u{1e}abcdef\u{1f}abcdef0\u{1f}HEAD -> main\u{1f}Alice\u{1f}alice@example.com\u{1f}2026-09-14\u{1f}\u{1f}hello\n| * \u{1e}123456\u{1f}1234567\u{1f}\u{1f}Bob\u{1f}bob@example.com\u{1f}2026-09-13\u{1f}\u{1f}world\n";
        let commits = parse_log(input).unwrap();
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].hash, "abcdef");
        assert_eq!(commits[0].graph, ["|\\", "* "]);
        assert_eq!(commits[1].subject, "world");
    }

    #[test]
    fn parses_empty_output() {
        assert!(parse_log("").unwrap().is_empty());
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

        assert!(untracked_regular_file_diff(
            std::path::Path::new("generated.bin"),
            std::path::Path::new("generated.bin"),
            &mut output
        )
        .unwrap());
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
        let shown = show(&commits[0], &[]).unwrap();
        let clean_item = load_watch_log().unwrap().remove(0);
        assert_eq!(clean_item.kind, CommitKind::WorkingTree);
        assert_eq!(clean_item.subject, "Working tree · Clean");
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
        let ordinary = load_log(&[]).unwrap();
        assert_eq!(ordinary.len(), 1);
        assert_eq!(ordinary[0].kind, CommitKind::Revision);
        let watched = load_watch_log().unwrap();
        assert_eq!(watched.len(), 2);
        assert_eq!(watched[0].kind, CommitKind::WorkingTree);
        assert_eq!(
            watched[0].subject,
            "Working tree · 1 changed file · 1 untracked file"
        );
        assert_eq!(watched[0].hash, clean_item.hash);
        let mut watched_app = crate::app::App::new(watched.clone());
        watched_app.watch = true;
        watched_app.switch_mode();
        assert_eq!(watched_app.mode, crate::app::Mode::Status);
        watched_app.switch_mode();
        assert_eq!(watched_app.mode, crate::app::Mode::Log);
        assert_eq!(watched_app.selected, 0);
        watched_app.replace_commits(vec![clean_item.clone(), watched[1].clone()]);
        assert_eq!(
            watched_app.commits[watched_app.selected].hash,
            clean_item.hash
        );
        assert_eq!(watched[1].kind, CommitKind::Revision);
        let commits = working_tree_entries().unwrap();
        assert_eq!(commits[0].kind, CommitKind::Unstaged);
        assert_eq!(commits[1].kind, CommitKind::Staged);
        assert!(show(&commits[0], &[])
            .unwrap()
            .contains("glog-lazy-untracked:"));
        assert!(show_untracked(std::path::Path::new("new.txt"))
            .unwrap()
            .contains("UNTRACKED LONGER"));
        assert!(show(&commits[1], &[]).unwrap().contains("staged"));

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
