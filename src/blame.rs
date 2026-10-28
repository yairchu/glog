//! `glog blame`: one row per line of a file, each carrying the commit that
//! last changed it.

use std::{
    collections::HashMap,
    io::Write,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
};

#[cfg(test)]
use std::process::Command;

use ratatui::{
    style::{Color, Style},
    text::Span,
};

use crate::{
    diff::FileSection,
    git::{self, Annotation, Collaborators, Commit, CommitKind, Version},
    log_format::{Field, LogFormat},
};

const UNCOMMITTED_AUTHOR: &str = "Not committed yet";
const MAX_AUTHOR_WIDTH: usize = 20;
const USAGE: &str = "use glog blame [-L LINE] [--date=STYLE] [revision] [--] file";

/// A pinned file snapshot whose attribution has not been requested yet.
pub struct FileRevision {
    revision: String,
    path: Vec<u8>,
    top: PathBuf,
}

impl FileRevision {
    pub fn blame(&self) -> Result<Vec<Commit>, String> {
        let date = git::configured_log_date("format-local:%Y-%m-%d")?;
        blame(Some(&self.revision), &self.path, Some(&self.top), &date)
    }
}

/// The file versions a patch compares.
pub enum PatchSides {
    /// A commit's patch. Its parent column N shows `{hash}^N`.
    Commit(String),
    /// A two-way patch. New files have no old side.
    Pair { old: Option<Version>, new: Version },
}

impl PatchSides {
    /// The sides of the patch shown for `commit`, in the section of `file`.
    pub fn of(commit: &Commit, file: &FileSection, top: &Path) -> Result<Self, String> {
        let revision = |revision: String| Some(Version::Revision(revision));
        Ok(match commit.kind {
            CommitKind::Revision => match (&commit.annotation, file.stash_layer) {
                (Some(Annotation::Stash(_)), Some(layer)) => {
                    let (old, new) = crate::stash::layer_revisions(&commit.hash, layer);
                    Self::Pair {
                        old: old.and_then(revision),
                        new: Version::Revision(new),
                    }
                }
                (Some(Annotation::Stash(_)), None) => return Err("Select a stash file".into()),
                _ => Self::Commit(commit.hash.clone()),
            },
            CommitKind::Staged => Self::Pair {
                old: revision("HEAD".into()),
                new: Version::Index,
            },
            CommitKind::Unstaged => Self::Pair {
                old: Some(Version::Index),
                new: Version::Worktree,
            },
            CommitKind::Comparison { .. } => {
                let (old, new) = git::comparison_sides(&commit.diff_args, Some(top))?
                    .ok_or("Cannot blame a combined diff of several revisions")?;
                Self::Pair {
                    old: Some(old),
                    new,
                }
            }
            CommitKind::WorkingTree => return Err("Select a patch to blame".into()),
        })
    }

    /// The version shown by a parent column, or by the result for None.
    fn side(&self, parent: Option<usize>) -> Result<Version, String> {
        match (self, parent) {
            (Self::Commit(hash), None) => Ok(Version::Revision(hash.clone())),
            (Self::Commit(hash), Some(parent)) => {
                Ok(Version::Revision(format!("{hash}^{}", parent + 1)))
            }
            (Self::Pair { new, .. }, None) => Ok(new.clone()),
            (Self::Pair { old, .. }, Some(0)) => old
                .clone()
                .ok_or_else(|| "The file is new in this patch".into()),
            (Self::Pair { .. }, Some(_)) => Err("Unexpected combined patch".into()),
        }
    }
}

/// Open the full file at the selected patch line, using its old side for deletions.
pub fn load_patch(
    sides: PatchSides,
    textconv: bool,
    patch: &str,
    row: usize,
    path: &[u8],
    top: PathBuf,
) -> Result<crate::app::App, String> {
    let (parent, number) =
        patch_position(patch, row).ok_or("Select a code line or hunk to blame")?;
    let version = sides.side(parent)?;
    let path = match (&sides, &version) {
        _ if parent.is_none() => path.to_vec(),
        (PatchSides::Commit(hash), Version::Revision(revision)) if !is_two_way(patch) => {
            // Combined diffs normally show only the result's name, even when
            // this parent used another name. Ask Git for the rename mapping.
            parent_path(revision, hash, path, &top)?
        }
        _ => crate::hunk::file_headers(patch)
            .find_map(|line| {
                let path = line.strip_prefix("--- ")?;
                let (path, _) = crate::diff::unquote_path(path.trim_end_matches('\t'))?;
                path.strip_prefix(b"a/").map(<[u8]>::to_vec)
            })
            .ok_or("Could not locate the parent file")?,
    };
    let date = git::configured_log_date_at("format-local:%Y-%m-%d", Some(&top))?;
    let rows = blame_version(&version, &path, &top, &date, textconv)?;
    if rows.is_empty() {
        return Err("The file is empty at this revision".into());
    }
    let mut app = crate::app::App::new(rows);
    app.selected = number.saturating_sub(1).min(app.commits.len() - 1);
    app.center_selection = true;
    app.context = format!(
        "blame {} -- {}",
        version_label(&version),
        crate::diff::display_path(&path)
    );
    Ok(app)
}

fn is_two_way(patch: &str) -> bool {
    crate::hunk::file_headers(patch)
        .next()
        .is_some_and(|line| line.starts_with("diff --git "))
}

/// Abbreviate the hash that starts a revision, keeping suffixes like `^2`.
fn version_label(version: &Version) -> String {
    match version {
        Version::Revision(revision) => {
            let hex = revision
                .find(|c: char| !c.is_ascii_hexdigit())
                .unwrap_or(revision.len());
            format!("{}{}", &revision[..hex.min(7)], &revision[hex..])
        }
        Version::Index => "index".into(),
        Version::Worktree => "working tree".into(),
    }
}

fn blame_version(
    version: &Version,
    path: &[u8],
    top: &Path,
    date: &str,
    textconv: bool,
) -> Result<Vec<Commit>, String> {
    let top = top.to_path_buf();
    match version {
        Version::Revision(revision) => {
            blame_with_textconv(Some(revision), path, Some(&top), date, textconv, None)
        }
        Version::Worktree => blame_with_textconv(None, path, Some(&top), date, textconv, None),
        Version::Index => {
            // Blame reads --contents as is, so convert them as the patch did.
            let mut command = git::read_command_at(Some(&top));
            command.arg("cat-file");
            command.arg(if textconv { "--textconv" } else { "blob" });
            let output = command
                .arg(blob_spec("", path))
                .output()
                .map_err(|error| format!("could not read the staged file: {error}"))?;
            if !output.status.success() {
                return Err(git::stderr_message(
                    "could not read the staged file",
                    &output.stderr,
                ));
            }
            blame_with_textconv(None, path, Some(&top), date, textconv, Some(&output.stdout))
        }
    }
}

/// The parent column (None for the result) and its one-based line number.
fn patch_position(patch: &str, row: usize) -> Option<(Option<usize>, usize)> {
    let position = crate::hunk::position(patch, row)?;
    let (parent, number) = match position.row {
        // A result range starting at 0 is empty: the file was deleted.
        crate::hunk::Row::Header if position.new == 0 => {
            let parent = position.old.iter().position(|&number| number > 0)?;
            (Some(parent), position.old[parent])
        }
        crate::hunk::Row::Header => (None, position.new),
        crate::hunk::Row::Line { result: true, .. } => (None, position.new),
        crate::hunk::Row::Line { parents, .. } => {
            let parent = parents.iter().position(|&present| present)?;
            (Some(parent), position.old[parent])
        }
        crate::hunk::Row::Other => return None,
    };
    Some((parent, number as usize))
}

fn parent_path(revision: &str, result: &str, path: &[u8], top: &Path) -> Result<Vec<u8>, String> {
    let output = git::read_command_at(Some(top))
        .args([
            "diff",
            "--name-status",
            "-z",
            "--find-renames",
            "--no-ext-diff",
            "--no-textconv",
            "--no-relative",
            "--no-exit-code",
            "--no-quiet",
            revision,
            result,
            "--",
        ])
        .output()
        .map_err(|error| format!("Could not locate the parent file: {error}"))?;
    if !output.status.success() {
        return Err(git::stderr_message(
            "Could not locate the parent file",
            &output.stderr,
        ));
    }
    let mut fields = output.stdout.split(|&byte| byte == 0);
    while let Some(status) = fields.next() {
        let Some(old) = fields.next() else { break };
        if status.starts_with(b"R") || status.starts_with(b"C") {
            let new = fields.next().ok_or("Missing rename destination")?;
            if new == path {
                return Ok(old.to_vec());
            }
        }
    }
    Ok(path.to_vec())
}

/// Open `path` as it is in `revision`, a resolved commit hash.
pub fn load_file(revision: String, path: &str) -> Result<crate::app::App, String> {
    if path.is_empty() {
        return Err("use glog show REV:path to read a committed text file".into());
    }
    let mut path = path.as_bytes().to_vec();
    // REV:path is rooted at the repository, except for Git's explicit ./ and
    // ../ forms. Resolve those before moving commands to the repository root.
    if path.starts_with(b"./") || path.starts_with(b"../") {
        let prefix = crate::git::read_command()
            .args(["rev-parse", "--show-prefix"])
            .output()
            .map_err(|error| format!("could not resolve file path: {error}"))?;
        if !prefix.status.success() {
            return Err(git::stderr_message(
                "could not resolve file path",
                &prefix.stderr,
            ));
        }
        let mut full = prefix
            .stdout
            .strip_suffix(b"\n")
            .unwrap_or(&prefix.stdout)
            .to_vec();
        full.extend(path);
        let mut parts = Vec::new();
        for part in full.split(|&byte| byte == b'/') {
            match part {
                b"" | b"." => {}
                b".." => {
                    parts.pop().ok_or("file path is outside the repository")?;
                }
                _ => parts.push(part),
            }
        }
        path = parts.join(&b'/');
    }
    let top = git::repository_directory()?;
    // Unlike `cat-file blob`, --textconv also accepts trees. Keep this view
    // restricted to files before asking Git to convert their contents.
    let output = git::read_command_at(Some(&top))
        .args(["cat-file", "-t"])
        .arg(blob_spec(&revision, &path))
        .output()
        .map_err(|error| format!("could not inspect file: {error}"))?;
    if !output.status.success() {
        return Err(git::stderr_message(
            "could not inspect file",
            &output.stderr,
        ));
    }
    if output.stdout != b"blob\n" {
        return Err("the path is not a file; this view supports text files".into());
    }
    let output = git::read_command_at(Some(&top))
        // Match blame's text conversion so loading attribution keeps the
        // displayed lines and their positions unchanged.
        .args(["cat-file", "--textconv"])
        .arg(blob_spec(&revision, &path))
        .output()
        .map_err(|error| format!("could not read file: {error}"))?;
    if !output.status.success() {
        return Err(git::stderr_message("could not read file", &output.stderr));
    }
    if output.stdout.contains(&0) {
        return Err("the file is binary; this view supports text files".into());
    }
    let contents = String::from_utf8_lossy(&output.stdout);
    let lines = contents
        .split_terminator('\n')
        .enumerate()
        .map(|(index, code)| PorcelainLine {
            hash: revision.clone(),
            number: index + 1,
            code: code.to_owned(),
            source_path: path.clone(),
            source_number: index + 1,
            previous: None,
        })
        .collect();
    let rows = render_lines(lines, HashMap::new(), top.clone(), "", &path, true);
    let mut app = crate::app::App::new(rows);
    app.history = crate::app::History::Blame;
    for field in Field::ATTRIBUTION {
        app.log_format.toggle(field);
    }
    app.pending_blame = Some(FileRevision {
        revision,
        path,
        top,
    });
    Ok(app)
}

/// The blamed file version and what its rows share.
#[derive(Debug, PartialEq, Eq)]
pub struct BlameFile {
    /// Paths in Git's blame output are relative to the repository root.
    /// In bare repositories, run historical queries from the Git directory.
    pub top: PathBuf,
    date: String,
    pub textconv: bool,
    hash_width: usize,
    date_width: usize,
    author_width: usize,
    number_width: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlameLine {
    pub file: Arc<BlameFile>,
    pub number: usize,
    pub code: Vec<Span<'static>>,
    /// The previous line came from a different commit.
    pub starts_chunk: bool,
    /// The file's path and this line's number in the commit that last
    /// changed it.
    pub source_path: Vec<u8>,
    pub source_number: usize,
    /// That commit's parent and the file's path there, unless the commit
    /// added the file without history to follow.
    pub previous: Option<(String, Vec<u8>)>,
}

/// Rows for `glog blame` arguments, and the row to select.
pub fn load(args: &[String]) -> Result<(Vec<Commit>, usize), String> {
    let mut line = None;
    let mut date = None;
    let mut positional = Vec::new();
    let mut separated = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--" => {
                let rest: Vec<_> = args.by_ref().collect();
                let [path] = rest[..] else {
                    return Err(format!("expected one file after --; {USAGE}"));
                };
                separated = Some(path.clone());
            }
            "-L" | "--date" => {
                let value = args
                    .next()
                    .ok_or_else(|| format!("{arg} requires a value"))?;
                if arg == "-L" {
                    line = Some(parse_line(value)?);
                } else {
                    date = Some(value.clone());
                }
            }
            _ if arg.starts_with("--date=") => date = Some(arg["--date=".len()..].to_owned()),
            _ if arg.starts_with("-L") => line = Some(parse_line(&arg[2..])?),
            _ if arg.starts_with('-') => {
                return Err(format!("unsupported blame argument {arg:?}; {USAGE}"))
            }
            _ => positional.push(arg.clone()),
        }
    }
    if let Some(path) = separated {
        positional.push(path);
    }
    let (revision, path) = match positional.as_slice() {
        [path] => (None, path),
        [revision, path] => (Some(revision.as_str()), path),
        [] => return Err(format!("missing file; {USAGE}")),
        _ => return Err(format!("too many arguments; {USAGE}")),
    };
    let date = match date {
        Some(date) => date,
        None => git::configured_log_date("format-local:%Y-%m-%d")?,
    };
    let rows = blame(revision, path.as_bytes(), None, &date)?;
    let selected = line
        .map_or(0, |line| line.saturating_sub(1))
        .min(rows.len().saturating_sub(1));
    Ok((rows, selected))
}

/// Accept Git's `-L START,END` form, but only to choose where to start.
fn parse_line(value: &str) -> Result<usize, String> {
    let start = value.split_once(',').map_or(value, |(start, _)| start);
    start
        .parse()
        .ok()
        .filter(|&line| line > 0)
        .ok_or_else(|| format!("-L takes a line number, not {value:?}"))
}

/// Blame the file as it was before the commit that last changed `line`,
/// selecting the parent's version of that line, or the line before where the
/// commit added it.
pub fn load_parent(commit: &Commit, line: &BlameLine) -> Result<(Vec<Commit>, usize), String> {
    let (parent, path) = line.previous.as_ref().ok_or("nothing earlier to blame")?;
    let output = git::read_command_at(Some(&line.file.top))
        .env("LC_ALL", "C")
        .args([
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "-U0",
            "--inter-hunk-context=0",
        ])
        .arg(blob_spec(parent, path))
        .arg(blob_spec(&commit.hash, &line.source_path))
        .output()
        .map_err(|error| format!("could not run git diff: {error}"))?;
    if !output.status.success() {
        return Err(git::stderr_message("git diff failed", &output.stderr));
    }
    let number = parent_line(&String::from_utf8_lossy(&output.stdout), line.source_number);
    let rows = blame_with_textconv(
        Some(parent),
        path,
        Some(&line.file.top),
        &line.file.date,
        line.file.textconv,
        None,
    )?;
    let selected = number.saturating_sub(1).min(rows.len().saturating_sub(1));
    Ok((rows, selected))
}

/// Build a revision:path argument without converting path bytes to text.
fn blob_spec(revision: &str, path: &[u8]) -> std::ffi::OsString {
    let mut spec = std::ffi::OsString::from(format!("{revision}:"));
    spec.push(git::raw_path(path));
    spec
}

/// Map a line through the hunks of a `-U0` diff to the old version: changed
/// lines map to the start of what they replaced, and added lines to the line
/// before them.
fn parent_line(diff: &str, line: usize) -> usize {
    let mut offset = 0isize;
    for header in diff.lines().filter_map(crate::hunk::header) {
        let [(old_start, old_count)] = header.old[..] else {
            continue;
        };
        let (new_start, new_count) = header.new;
        let [old_start, old_count, new_start, new_count] =
            [old_start, old_count, new_start, new_count].map(|n| n as usize);
        // An empty range names the line before the position it refers to.
        let (old_first, old_end) = if old_count == 0 {
            (old_start + 1, old_start + 1)
        } else {
            (old_start, old_start + old_count)
        };
        let (new_first, new_end) = if new_count == 0 {
            (new_start + 1, new_start + 1)
        } else {
            (new_start, new_start + new_count)
        };
        if line < new_first {
            break;
        }
        if line < new_end {
            return old_first.min(old_end.saturating_sub(1)).max(1);
        }
        offset = old_end as isize - new_end as isize;
    }
    line.saturating_add_signed(offset).max(1)
}

fn blame(
    revision: Option<&str>,
    path: &[u8],
    top: Option<&PathBuf>,
    date: &str,
) -> Result<Vec<Commit>, String> {
    blame_with_textconv(revision, path, top, date, true, None)
}

fn blame_with_textconv(
    revision: Option<&str>,
    path: &[u8],
    top: Option<&PathBuf>,
    date: &str,
    textconv: bool,
    // Blame these file contents in place of the working tree's.
    contents: Option<&[u8]>,
) -> Result<Vec<Commit>, String> {
    let mut command = git::read_command_at(top.map(PathBuf::as_path));
    command.args(["-c", "core.quotepath=true", "blame", "--porcelain"]);
    if !textconv {
        command.arg("--no-textconv");
    }
    if contents.is_some() {
        command.args(["--contents", "-"]).stdin(Stdio::piped());
    }
    let mut child = command
        .env("LC_ALL", "C")
        .args(revision)
        .arg("--")
        .arg(git::raw_path(path))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("could not run git blame: {error}"))?;
    if let (Some(contents), Some(mut stdin)) = (contents, child.stdin.take()) {
        // Git reads all contents before it writes any output. It closes its
        // input early only on errors, which its exit status reports.
        let _ = stdin.write_all(contents);
    }
    let output = child
        .wait_with_output()
        .map_err(|error| format!("could not run git blame: {error}"))?;
    if !output.status.success() {
        return Err(git::stderr_message("git blame failed", &output.stderr));
    }
    let lines = parse_porcelain(&String::from_utf8_lossy(&output.stdout))?;
    let top = match top {
        Some(top) => top.clone(),
        None => git::repository_directory()?,
    };
    let details = commit_details(&lines, date, &top)?;
    Ok(render_lines(lines, details, top, date, path, textconv))
}

fn render_lines(
    lines: Vec<PorcelainLine>,
    details: HashMap<String, Details>,
    top: PathBuf,
    date: &str,
    path: &[u8],
    textconv: bool,
) -> Vec<Commit> {
    let mut highlighted = highlight(&crate::diff::display_path(path), &lines).map(Vec::into_iter);
    let width = |text: &str| Span::raw(text).width();
    let file = Arc::new(BlameFile {
        top,
        date: date.to_owned(),
        textconv,
        hash_width: details
            .values()
            .map(|d| d.short_hash.len())
            .max()
            .unwrap_or(7),
        date_width: details.values().map(|d| width(&d.date)).max().unwrap_or(0),
        author_width: details
            .values()
            .map(|d| width(&d.author) + badge_width(&d.collaborators))
            .chain(
                lines
                    .iter()
                    .any(PorcelainLine::is_uncommitted)
                    .then(|| width(UNCOMMITTED_AUTHOR)),
            )
            .max()
            .unwrap_or(0)
            .min(MAX_AUTHOR_WIDTH),
        number_width: lines.len().to_string().len(),
    });
    let mut previous_hash = None;
    lines
        .into_iter()
        .map(|line| {
            let starts_chunk = previous_hash.as_ref() != Some(&line.hash);
            previous_hash = Some(line.hash.clone());
            let mut commit = Commit {
                kind: CommitKind::Revision,
                annotation: None,
                diff_args: Vec::new(),
                hash: line.hash.clone(),
                short_hash: line.hash[..file.hash_width.min(line.hash.len())].to_owned(),
                decorations: String::new(),
                subject: String::new(),
                author: if line.is_uncommitted() {
                    UNCOMMITTED_AUTHOR.to_owned()
                } else {
                    String::new()
                },
                author_email: String::new(),
                author_date: String::new(),
                collaborators: Collaborators::default(),
                graph: vec![String::new()],
                first_parent_only: false,
                graph_node: '*',
                parents: Vec::new(),
            };
            if let Some(details) = details.get(&line.hash) {
                commit.short_hash = details.short_hash.clone();
                commit.author = details.author.clone();
                commit.author_email = details.author_email.clone();
                commit.author_date = details.date.clone();
                commit.collaborators = details.collaborators.clone();
            } else if line.is_uncommitted() {
                // Enter opens Status for lines changed in the working tree.
                commit.kind = CommitKind::WorkingTree;
            }
            commit.annotation = Some(Annotation::Blame(BlameLine {
                file: file.clone(),
                number: line.number,
                code: crate::ansi::display_spans(
                    match highlighted.as_mut().and_then(Iterator::next) {
                        Some(spans) => spans,
                        None => vec![Span::raw(line.code)],
                    },
                ),
                starts_chunk,
                source_path: line.source_path,
                source_number: line.source_number,
                previous: line.previous,
            }));
            commit
        })
        .collect()
}

#[derive(Debug, PartialEq, Eq)]
struct PorcelainLine {
    hash: String,
    number: usize,
    code: String,
    source_path: Vec<u8>,
    source_number: usize,
    previous: Option<(String, Vec<u8>)>,
}

impl PorcelainLine {
    fn is_uncommitted(&self) -> bool {
        self.hash.bytes().all(|byte| byte == b'0')
    }
}

/// Git describes each commit once, and again only where its lines came
/// from another path, so origins carry over between line groups.
fn parse_porcelain(output: &str) -> Result<Vec<PorcelainLine>, String> {
    let invalid = || "invalid Git blame output".to_owned();
    let unquote = |path: &str| {
        crate::diff::unquote_path(path)
            .map(|(bytes, _)| bytes)
            .ok_or_else(invalid)
    };
    #[derive(Clone)]
    struct Origin {
        source_path: Vec<u8>,
        previous: Option<(String, Vec<u8>)>,
    }
    let mut origins: HashMap<String, Origin> = HashMap::new();
    let mut header: Option<(String, usize, usize)> = None;
    let mut previous = None;
    let mut lines = Vec::new();
    for line in output.split('\n') {
        if let Some(code) = line.strip_prefix('\t') {
            let (hash, source_number, number) = header.take().ok_or_else(invalid)?;
            let Origin {
                source_path,
                previous,
            } = origins.get(&hash).cloned().ok_or_else(invalid)?;
            lines.push(PorcelainLine {
                hash,
                number,
                code: code.to_owned(),
                source_path,
                source_number,
                previous,
            });
        } else if let Some((hash, _, _)) = &header {
            if let Some(rest) = line.strip_prefix("previous ") {
                let (parent, path) = rest.split_once(' ').ok_or_else(invalid)?;
                previous = Some((parent.to_owned(), unquote(path)?));
            } else if let Some(path) = line.strip_prefix("filename ") {
                origins.insert(
                    hash.clone(),
                    Origin {
                        source_path: unquote(path)?,
                        previous: previous.take(),
                    },
                );
            }
            // Authors and dates come from Git's log formatting instead.
        } else if !line.is_empty() {
            let mut fields = line.split(' ');
            let hash = fields
                .next()
                .filter(|hash| hash.len() >= 40 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
                .ok_or_else(invalid)?;
            let mut number = || {
                fields
                    .next()
                    .and_then(|field| field.parse().ok())
                    .ok_or_else(invalid)
            };
            let source_number = number()?;
            let final_number = number()?;
            header = Some((hash.to_owned(), source_number, final_number));
        }
    }
    if header.is_some() {
        return Err(invalid());
    }
    Ok(lines)
}

struct Details {
    short_hash: String,
    author: String,
    author_email: String,
    date: String,
    collaborators: Collaborators,
}

/// Format authors, dates, and coauthors as Log does.
fn commit_details(
    lines: &[PorcelainLine],
    date: &str,
    top: &Path,
) -> Result<HashMap<String, Details>, String> {
    let mut input = String::new();
    let mut seen = std::collections::HashSet::new();
    for line in lines {
        if !line.is_uncommitted() && seen.insert(&line.hash) {
            input.push_str(&line.hash);
            input.push('\n');
        }
    }
    if input.is_empty() {
        return Ok(HashMap::new());
    }
    let output = git::pipe_through(
        git::read_command_at(Some(top))
            .env("LC_ALL", "C")
            .args(["log", "--no-walk=unsorted", "--stdin", "--no-show-signature"])
            .arg(format!("--date={date}"))
            .arg("--format=%x1e%H%x1f%h%x1f%an%x1f%ae%x1f%ad%x1f%(trailers:key=Co-authored-by,valueonly,unfold,separator=%x1d)"),
        input.as_bytes(),
    )
    .ok_or("could not read the blamed commits")?;
    output
        .split('\x1e')
        .skip(1)
        .map(|record| {
            let fields: Vec<_> = record.trim_end_matches('\n').split('\x1f').collect();
            let [hash, short_hash, author, email, date, trailers] = fields[..] else {
                return Err("invalid Git log output".to_owned());
            };
            if date.chars().any(char::is_control) {
                return Err("Git blame dates must fit on one line; use --date=iso-strict".into());
            }
            Ok((
                hash.to_owned(),
                Details {
                    short_hash: short_hash.to_owned(),
                    author: author.to_owned(),
                    author_email: email.to_owned(),
                    date: date.to_owned(),
                    collaborators: Collaborators::parse(trailers, email),
                },
            ))
        })
        .collect()
}

/// Syntax-highlight lines with delta, as unchanged lines of a diff of the
/// file, so they share Show's theme.
fn highlight(path: &str, lines: &[PorcelainLine]) -> Option<Vec<Vec<Span<'static>>>> {
    // Escape sequences in the file would be indistinguishable from delta's.
    if !git::delta_enabled() || lines.iter().any(|line| line.code.contains('\x1b')) {
        return None;
    }
    let mut diff = format!(
        "diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n@@ -1,{0} +1,{0} @@\n",
        lines.len()
    );
    for line in lines {
        diff.push(' ');
        diff.push_str(&line.code);
        diff.push('\n');
    }
    let output = git::run_delta(diff.as_bytes())?;
    let highlighted: Vec<_> = output
        .lines()
        .skip(4)
        .map(|line| {
            let mut spans = crate::ansi::parse_line(line).spans;
            // Drop the column that marks an unchanged line.
            if let Some(first) = spans.first_mut() {
                first.content = first.content.get(1..).unwrap_or_default().to_owned().into();
            }
            spans
        })
        .collect();
    (highlighted.len() == lines.len()).then_some(highlighted)
}

fn badge_width(collaborators: &Collaborators) -> usize {
    crate::log_format::coauthor_badge(collaborators)
        .iter()
        .map(Span::width)
        .sum()
}

/// Commit details, or blanks in their place, then the line itself. The Log
/// format's field toggles choose which details are shown.
pub fn spans(
    commit: &Commit,
    line: &BlameLine,
    details: bool,
    format: &LogFormat,
) -> Vec<Span<'static>> {
    let file = &line.file;
    let mut spans = Vec::new();
    let mut blank = 0;
    if format.shows(Field::Hash) {
        if details {
            spans.push(Span::styled(
                pad(&commit.short_hash, file.hash_width),
                Style::default().fg(Color::Yellow),
            ));
        } else {
            blank += file.hash_width + 1;
        }
    }
    if format.shows(Field::Date) {
        if details {
            spans.push(Span::styled(
                pad(&commit.author_date, file.date_width),
                Style::default().fg(Color::Gray),
            ));
        } else {
            blank += file.date_width + 1;
        }
    }
    if format.shows(Field::Author) {
        if details {
            let badge = crate::log_format::coauthor_badge(&commit.collaborators);
            let badge_width: usize = badge.iter().map(Span::width).sum();
            let author = truncate(
                &commit.author,
                file.author_width.saturating_sub(badge_width),
            );
            let padding = file
                .author_width
                .saturating_sub(Span::raw(author.as_str()).width() + badge_width);
            spans.push(Span::styled(author, Style::default().fg(Color::Cyan)));
            spans.extend(badge);
            spans.push(Span::raw(" ".repeat(padding + 1)));
        } else {
            blank += file.author_width + 1;
        }
    }
    if blank > 0 {
        spans.push(Span::raw(" ".repeat(blank)));
    }
    spans.push(Span::styled(
        format!("{:>width$} ", line.number, width = file.number_width),
        Style::default().fg(Color::DarkGray),
    ));
    spans.extend(line.code.iter().cloned());
    spans
}

fn pad(text: &str, width: usize) -> String {
    let padding = width.saturating_sub(Span::raw(text).width());
    format!("{text}{} ", " ".repeat(padding))
}

fn truncate(text: &str, width: usize) -> String {
    if Span::raw(text).width() <= width {
        return text.to_owned();
    }
    let mut output = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let ch_width = Span::raw(ch.encode_utf8(&mut [0; 4]).to_owned()).width();
        if used + ch_width + 1 > width {
            break;
        }
        output.push(ch);
        used += ch_width;
    }
    output.push('…');
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::{App, History, Mode},
        git::tests::{CurrentDirGuard, TestDirectory},
        input,
    };
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use std::fs;

    fn code(line: &BlameLine) -> String {
        line.code.iter().map(|span| span.content.as_ref()).collect()
    }

    fn git(args: &[&str]) {
        let output = Command::new("git")
            .args([
                "-c",
                "user.name=Alice",
                "-c",
                "user.email=alice@example.com",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .env("GIT_AUTHOR_NAME", "Alice")
            .env("GIT_AUTHOR_EMAIL", "alice@example.com")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }

    fn press(app: &mut App, key: KeyCode) {
        input::handle(Event::Key(KeyEvent::new(key, KeyModifiers::NONE)), app);
    }

    fn review_line(revision: &str, text: &str) -> App {
        select_line(crate::git::load_show_app(&[revision.into()]).unwrap(), text)
    }

    fn select_line(mut app: App, text: &str) -> App {
        while let Some(cursor) = app.show_rows.iter().position(|row| row.folded) {
            app.show_cursor = cursor;
            app.toggle_show_file();
        }
        app.show_cursor = app
            .show_rows
            .iter()
            .position(|row| crate::ansi::plain(&row.text) == text)
            .unwrap_or_else(|| panic!("missing {text:?} in {}", app.show_text));
        app.show_offset = app.show_cursor.saturating_sub(2);
        app
    }

    #[test]
    fn review_blame_uses_patch_sides_and_returns_through_parent_history() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        let original = (1..=25).map(|n| format!("line {n}\n")).collect::<String>();
        fs::write("old name.txt", &original).unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "original"]);
        git(&["mv", "old name.txt", "new name.txt"]);
        let changed = original
            .replace("line 2\n", "inserted\nreplacement\n")
            .replace("line 22\n", "last replacement\n");
        fs::write("new name.txt", &changed).unwrap();
        git(&["commit", "-qam", "rename and change"]);
        fs::write("new name.txt", "a later committed version\n").unwrap();
        git(&["commit", "-qam", "later"]);
        fs::write("new name.txt", "uncommitted contents\n").unwrap();
        fs::create_dir("launch").unwrap();
        std::env::set_current_dir("launch").unwrap();

        for (patch_line, code_line, number, parent) in [
            ("+inserted", "inserted", 2, false),
            ("+replacement", "replacement", 3, false),
            (" line 3", "line 3", 4, false),
            ("-line 2", "line 2", 2, true),
            ("+last replacement", "last replacement", 23, false),
            ("-line 22", "line 22", 22, true),
        ] {
            let mut app = review_line("HEAD^", patch_line);
            app.watch = true;
            let (cursor, offset, text, hash) = (
                app.show_cursor,
                app.show_offset,
                app.show_text.clone(),
                app.commits[app.selected].hash.clone(),
            );
            press(&mut app, KeyCode::Char('B'));
            assert_eq!(app.status, None, "{patch_line}");
            assert_eq!(app.history, History::Blame);
            assert_eq!(app.mode, Mode::Log);
            assert!(!app.watch);
            assert_eq!(app.commits.len(), if parent { 25 } else { 26 });
            let line = app.commits[app.selected].blame().unwrap();
            assert_eq!(line.number, number);
            assert_eq!(code(line), code_line);
            assert_eq!(
                line.source_path,
                if parent {
                    b"old name.txt".to_vec()
                } else if patch_line.starts_with('+') {
                    b"new name.txt".to_vec()
                } else {
                    b"old name.txt".to_vec()
                }
            );

            if patch_line == "+replacement" {
                for back in [KeyCode::Backspace, KeyCode::Esc] {
                    press(&mut app, KeyCode::Char('p'));
                    assert_eq!(app.status, None);
                    assert_eq!(app.commits.len(), 25);
                    press(&mut app, back);
                    assert_eq!(app.history, History::Blame);
                    assert_eq!(
                        code(app.commits[app.selected].blame().unwrap()),
                        "replacement"
                    );
                }
                // Backspace in a parent blame's Show steps back through it, too.
                press(&mut app, KeyCode::Char('p'));
                press(&mut app, KeyCode::Enter);
                assert_eq!(app.mode, Mode::Show);
                press(&mut app, KeyCode::Backspace);
                assert_eq!(app.history, History::Blame);
                assert_eq!(app.mode, Mode::Log);
                assert_eq!(
                    code(app.commits[app.selected].blame().unwrap()),
                    "replacement"
                );
            }
            press(&mut app, KeyCode::Backspace);
            assert_eq!(app.mode, Mode::Show);
            assert_eq!(app.history, History::Log);
            assert_eq!(app.show_cursor, cursor);
            assert_eq!(app.show_offset, offset);
            assert_eq!(app.show_text, text);
            assert_eq!(app.commits[app.selected].hash, hash);
            assert!(app.watch);
            app.switch_mode();
            app.switch_mode();
            assert_eq!(app.show_cursor, cursor);
            assert_eq!(app.show_offset, offset);
            // Escape also restores the review, including when glog was
            // launched with Escape configured to exit to the calling tool.
            app.exit_on_esc = parent;
            press(&mut app, KeyCode::Char('B'));
            press(&mut app, KeyCode::Esc);
            assert_eq!(app.mode, Mode::Show);
            assert_eq!(app.show_cursor, cursor);
            assert_eq!(app.show_offset, offset);
            assert_eq!(app.show_text, text);
            assert!(!app.quit);
        }

        // An unsuccessful jump keeps the review and its return path intact.
        let mut app = review_line("HEAD^", "+replacement");
        app.show_cursor = 0;
        let text = app.show_text.clone();
        press(&mut app, KeyCode::Char('B'));
        assert_eq!(app.mode, Mode::Show);
        assert_eq!(app.show_text, text);
        assert!(!app.has_reference_back());
        assert_eq!(app.status.as_deref(), Some("No file selected"));
    }

    #[test]
    fn review_blame_opens_both_sides_of_uncommitted_and_stashed_patches() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        fs::write("file.txt", "a\nb\nc\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "base"]);
        fs::write("file.txt", "a\nB\nc\n").unwrap();
        git(&["commit", "-qam", "change"]);
        fs::write("file.txt", "a\nB\nstaged\n").unwrap();
        git(&["add", "."]);
        fs::write("file.txt", "a\nB\nworktree\n").unwrap();

        let check = |app: App, text: &str, label: &str, committed: bool| {
            let mut app = select_line(app, text);
            press(&mut app, KeyCode::Char('B'));
            assert_eq!(app.status, None, "{label} {text}");
            assert_eq!(app.context, format!("blame {label} -- file.txt"), "{text}");
            let commit = &app.commits[app.selected];
            assert_eq!(code(commit.blame().unwrap()), &text[1..], "{label}");
            assert_eq!(commit.kind == CommitKind::Revision, committed, "{text}");
        };
        let diff = |args: &[&str]| {
            let args: Vec<_> = args.iter().map(|arg| arg.to_string()).collect();
            crate::git::load_diff_app(&args).unwrap()
        };
        let short = |revision: &str| {
            let output = Command::new("git")
                .args(["rev-parse", "--short=7", revision])
                .output()
                .unwrap();
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        };
        let (base, head) = (short("HEAD^"), short("HEAD"));
        check(diff(&["--cached"]), "-c", "HEAD", true);
        check(diff(&["--cached"]), "+staged", "index", false);
        check(diff(&[]), "-staged", "index", false);
        check(diff(&[]), "+worktree", "working tree", false);
        check(diff(&["HEAD^"]), "-b", &base, true);
        check(diff(&["HEAD^"]), "+worktree", "working tree", false);
        check(diff(&["--cached", "HEAD^"]), "+staged", "index", false);
        check(diff(&["HEAD^", "HEAD"]), "-b", &base, true);
        check(diff(&["HEAD^...HEAD"]), "+B", &head, true);

        git(&["stash", "-q"]);
        let stash = short("stash");
        let mut app = crate::stash::load_app(&[]).unwrap();
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.mode, Mode::Show);
        check(app, "-staged", &format!("{stash}^2"), true);
    }

    #[test]
    fn review_blame_opens_deleted_file_from_its_hunk_header() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        fs::write("file.txt", "a\nb\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "add"]);
        git(&["rm", "-q", "file.txt"]);
        git(&["commit", "-qm", "delete"]);

        let mut app = review_line("HEAD", "@@ -1,2 +0,0 @@");
        press(&mut app, KeyCode::Char('B'));
        assert_eq!(app.status, None);
        assert_eq!(app.history, History::Blame);
        assert_eq!(code(app.commits[app.selected].blame().unwrap()), "a");
    }

    #[test]
    fn review_blame_uses_recorded_submodule_revisions_and_repository() {
        use crate::git::tests::git as output;
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        git(&["init", "-q", "module space"]);
        fs::write("module space/file.txt", "before\n").unwrap();
        git(&["-C", "module space", "add", "."]);
        git(&["-C", "module space", "commit", "-qm", "original module"]);
        let old = output(&["-C", "module space", "rev-parse", "HEAD"]);
        fs::write(
            ".gitmodules",
            "[submodule \"module space\"]\n\tpath = module space\n\turl = ./unused\n",
        )
        .unwrap();
        git(&["add", ".gitmodules", "module space"]);
        git(&["commit", "-qm", "add module"]);
        git(&["submodule", "absorbgitdirs"]);
        // The recorded old revision is not the new revision's parent.
        for (contents, message) in [
            ("intermediate\n", "intermediate module"),
            ("after\n", "updated module"),
        ] {
            fs::write("module space/file.txt", contents).unwrap();
            git(&["-C", "module space", "commit", "-qam", message]);
        }
        let new = output(&["-C", "module space", "rev-parse", "HEAD"]);
        git(&["add", "module space"]);
        git(&["commit", "-qm", "update module"]);
        git(&["-C", "module space", "checkout", "-q", old.trim()]);
        fs::write("module space/file.txt", "uncommitted contents\n").unwrap();
        fs::create_dir("launch").unwrap();
        std::env::set_current_dir("launch").unwrap();

        for (patch_line, expected, hash, subject) in [
            ("+after", "after", new.trim(), "updated module"),
            ("-before", "before", old.trim(), "original module"),
        ] {
            let mut app = crate::git::load_show_app(&["HEAD".into()]).unwrap();
            while let Some(cursor) = app.show_rows.iter().position(|row| row.folded) {
                app.show_cursor = cursor;
                app.toggle_show_file();
            }
            app.show_cursor = app
                .show_rows
                .iter()
                .position(|row| crate::ansi::plain(&row.text).trim_start() == patch_line)
                .unwrap();
            let cursor = app.show_cursor;
            let text = app.show_text.clone();
            press(&mut app, KeyCode::Char('B'));
            assert_eq!(app.status, None, "{patch_line}");
            assert_eq!(app.history, History::Blame);
            let line = app.commits[app.selected].blame().unwrap();
            assert_eq!(code(line), expected);
            assert_eq!(line.number, 1);
            assert_eq!(app.commits[app.selected].hash, hash);
            assert_eq!(line.source_path, b"file.txt");
            // Opening the blamed commit must query the module, too.
            press(&mut app, KeyCode::Enter);
            assert_eq!(app.status, None);
            assert!(app.show_text.contains(subject), "{}", app.show_text);
            while let Some(cursor) = app.show_rows.iter().position(|row| row.folded) {
                app.show_cursor = cursor;
                app.toggle_show_file();
            }
            app.show_cursor = app
                .show_rows
                .iter()
                .position(|row| crate::ansi::plain(&row.text) == format!("+{expected}"))
                .unwrap_or_else(|| panic!("missing {expected} in {:?}", app.show_rows));
            press(&mut app, KeyCode::Char('B'));
            assert_eq!(app.status, None);
            assert_eq!(code(app.commits[app.selected].blame().unwrap()), expected);
            press(&mut app, KeyCode::Backspace);
            press(&mut app, KeyCode::Esc);
            if patch_line == "+after" {
                press(&mut app, KeyCode::Char('p'));
                assert_eq!(app.status, None);
                assert_eq!(
                    code(app.commits[app.selected].blame().unwrap()),
                    "intermediate"
                );
                press(&mut app, KeyCode::Backspace);
                assert_eq!(code(app.commits[app.selected].blame().unwrap()), "after");
            }
            press(&mut app, KeyCode::Backspace);
            assert_eq!(app.mode, Mode::Show);
            assert_eq!(app.show_cursor, cursor);
            assert_eq!(app.show_text, text);
        }
    }

    fn module_review(textconv: bool) -> (App, String, String) {
        use crate::git::tests::git as output;
        git(&["init", "-q"]);
        git(&[
            "remote",
            "add",
            "origin",
            "https://github.com/example/parent.git",
        ]);
        git(&["init", "-q", "module"]);
        git(&[
            "-C",
            "module",
            "remote",
            "add",
            "origin",
            "https://github.com/example/module.git",
        ]);
        if textconv {
            fs::write("module/.gitattributes", "file.txt diff=readable\n").unwrap();
            git(&[
                "-C",
                "module",
                "config",
                "diff.readable.textconv",
                "sed '1d'",
            ]);
        }
        fs::write("module/file.txt", "header\nfirst\nbefore\nlast\n").unwrap();
        git(&["-C", "module", "add", "."]);
        git(&["-C", "module", "commit", "-qm", "original module"]);
        let old = output(&["-C", "module", "rev-parse", "HEAD"])
            .trim()
            .to_owned();
        fs::write(
            ".gitmodules",
            "[submodule \"module\"]\n\tpath = module\n\turl = ./unused\n",
        )
        .unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "add module"]);
        git(&["submodule", "absorbgitdirs"]);
        fs::write("module/file.txt", "header\nfirst\nafter\nlast\n").unwrap();
        git(&[
            "-C",
            "module",
            "commit",
            "-qam",
            &format!("updated module\n\nSee {old} and #42"),
        ]);
        let new = output(&["-C", "module", "rev-parse", "HEAD"])
            .trim()
            .to_owned();
        git(&["add", "module"]);
        git(&["commit", "-qm", "update module"]);
        (review_line("HEAD", "  +after"), old, new)
    }

    #[test]
    fn submodule_blame_show_previews_images_in_their_own_repository() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        module_review(false);
        fs::write("module/file.txt", "header\nfirst\nnewer\nlast\n").unwrap();
        let image = image::RgbaImage::from_pixel(1, 1, image::Rgba([255, 0, 0, 255]));
        image.save("module/picture.png").unwrap();
        git(&["-C", "module", "add", "."]);
        git(&["-C", "module", "commit", "-qm", "change text and add image"]);
        image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 255, 255]))
            .save("parent.png")
            .unwrap();
        git(&["add", "module", "parent.png"]);
        git(&["commit", "-qm", "update module and add parent image"]);

        let mut app = review_line("HEAD", "  +newer");
        // Enable previews without depending on the terminal running the test.
        app.images.enabled = true;
        app.images.root = directory.path().to_owned();
        app.toggle_show_stat();
        app.toggle_show_stat();
        press(&mut app, KeyCode::Char('B'));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.status, None);
        while let Some(cursor) = app.show_rows.iter().position(|row| row.folded) {
            app.show_cursor = cursor;
            app.toggle_show_file();
        }
        let preview = app
            .show_rows
            .iter()
            .find_map(|row| row.preview.as_ref())
            .expect("submodule image preview");
        let crate::images::Source::Blob(root, oid) = &preview.source else {
            panic!("historical preview must use a recorded blob");
        };
        assert_eq!(
            root.canonicalize().unwrap(),
            directory.path().join("module").canonicalize().unwrap()
        );
        let output = crate::git::read_command_at(Some(root))
            .args(["cat-file", "blob", oid])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(output.stdout, fs::read("module/picture.png").unwrap());

        // Returning to the superproject must also restore its image lookup.
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Backspace);
        assert_eq!(app.mode, Mode::Show);
        while let Some(cursor) = app.show_rows.iter().position(|row| row.folded) {
            app.show_cursor = cursor;
            app.toggle_show_file();
        }
        let parent = app
            .show_rows
            .iter()
            .filter_map(|row| row.preview.as_ref())
            .find(|preview| {
                matches!(&preview.source, crate::images::Source::Blob(root, _) if root.canonicalize().unwrap() == directory.path().canonicalize().unwrap())
            });
        assert!(parent.is_some(), "parent image preview after returning");
    }

    #[test]
    fn submodule_patch_blame_keeps_raw_lines_with_textconv() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        let (mut app, _, _) = module_review(true);
        for (patch_line, expected) in [
            ("+after", "after"),
            ("-before", "before"),
            (" header", "header"),
        ] {
            app.show_cursor = app
                .show_rows
                .iter()
                .position(|row| crate::ansi::plain(&row.text) == format!("  {patch_line}"))
                .unwrap();
            press(&mut app, KeyCode::Char('B'));
            assert_eq!(app.status, None);
            assert_eq!(app.commits.len(), 4);
            assert_eq!(code(app.commits[app.selected].blame().unwrap()), expected);
            if expected == "after" {
                press(&mut app, KeyCode::Char('p'));
                assert_eq!(app.status, None);
                assert_eq!(app.commits.len(), 4);
                assert_eq!(code(app.commits[app.selected].blame().unwrap()), "before");
                press(&mut app, KeyCode::Backspace);
            }
            press(&mut app, KeyCode::Backspace);
            assert_eq!(app.mode, Mode::Show);
        }
    }

    #[test]
    fn submodule_blame_links_keep_repository_through_history_and_return() {
        use crate::commit_links::Target;
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        let (mut app, old, new) = module_review(false);
        let review = app.show_text.clone();
        press(&mut app, KeyCode::Char('B'));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.status, None);
        let commit = Target::Commit(old.clone());
        assert!(app
            .show_links
            .values()
            .flatten()
            .any(|link| link.target == commit));
        app.follow_link(&commit);
        assert_eq!(app.status, None);
        assert!(app.show_text.contains("original module"));
        // Loading the followed commit's deferred history also stays in the module.
        app.switch_mode();
        assert_eq!(app.status, None);
        assert_eq!(app.commits.len(), 1);
        assert_eq!(app.commits[0].hash, old);
        app.reference_back();
        assert!(app.show_text.contains("updated module"));
        assert!(app
            .show_links
            .values()
            .flatten()
            .any(|link| link.target
                == Target::Url("https://github.com/example/module/issues/42".into())));
        app.follow_link(&Target::Range(format!("{old}..{new}")));
        assert_eq!(app.status, None);
        assert_eq!(app.commits.len(), 1);
        assert_eq!(app.commits[0].hash, new);
        app.switch_mode();
        assert_eq!(app.status, None);
        assert!(app.show_text.contains("updated module"));
        app.reference_back();
        press(&mut app, KeyCode::Backspace);
        assert_eq!(app.mode, Mode::Show);
        assert_eq!(app.show_text, review);
    }

    #[test]
    fn review_blame_opens_added_and_deleted_files_with_quoted_paths() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        // Non-ASCII names exercise Git's quoted paths while remaining valid
        // on Windows, where even fast-import rejects literal double quotes.
        let path = "quoted café.txt";
        git(&["config", "core.quotePath", "true"]);
        fs::write(path, "first\n\nlast").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "create"]);
        for (patch_line, expected, number) in [("+last", "last", 3), ("+", "", 2)] {
            let mut app = review_line("HEAD", patch_line);
            assert!(app.show_text.contains(r#"quoted caf\303\251.txt""#));
            press(&mut app, KeyCode::Char('B'));
            assert_eq!(app.status, None);
            let line = app.commits[app.selected].blame().unwrap();
            assert_eq!(code(line), expected);
            assert_eq!(line.number, number);
            assert_eq!(line.source_path, path.as_bytes());
        }
        git(&["rm", "-q", path]);
        git(&["commit", "-qm", "delete"]);
        for (patch_line, expected, number) in [("-last", "last", 3), ("-", "", 2)] {
            let mut app = review_line("HEAD", patch_line);
            assert!(app.show_text.contains(r#"quoted caf\303\251.txt""#));
            press(&mut app, KeyCode::Char('B'));
            assert_eq!(app.status, None);
            let line = app.commits[app.selected].blame().unwrap();
            assert_eq!(code(line), expected);
            assert_eq!(line.number, number);
            assert_eq!(line.source_path, path.as_bytes());
        }
        let mut app = review_line("HEAD", "\\ No newline at end of file");
        press(&mut app, KeyCode::Char('B'));
        assert_eq!(app.mode, Mode::Show);
        assert_eq!(
            app.status.as_deref(),
            Some("Select a code line or hunk to blame")
        );
    }

    #[test]
    fn review_blame_maps_combined_diff_lines_to_their_parent() {
        use crate::git::tests::git as output;
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        let tail = (1..=20).map(|n| format!("tail {n}\n")).collect::<String>();
        fs::write(
            "file.txt",
            format!("start\none\ncommon\nthree\nend\n{tail}"),
        )
        .unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "base"]);
        let base = output(&["rev-parse", "HEAD"]);
        fs::write(
            "file.txt",
            format!("start\nleft\ncommon\nthree\nend\n{tail}"),
        )
        .unwrap();
        git(&["commit", "-qam", "left"]);
        let left = output(&["rev-parse", "HEAD"]);
        git(&["checkout", "-q", "--detach", base.trim()]);
        fs::write(
            "file.txt",
            format!("start\none\ncommon\nright\nend\n{tail}"),
        )
        .unwrap();
        git(&["commit", "-qam", "right"]);
        let right = output(&["rev-parse", "HEAD"]);
        fs::rename("file.txt", "moved.txt").unwrap();
        fs::write(
            "moved.txt",
            format!("start\nmerged\ncommon\nmerged tail\nend\n{tail}"),
        )
        .unwrap();
        git(&["add", "-A"]);
        let tree = output(&["write-tree"]);
        let merge = output(&[
            "commit-tree",
            tree.trim(),
            "-p",
            left.trim(),
            "-p",
            right.trim(),
            "-m",
            "merge",
        ]);
        for (patch_line, expected, number) in [
            ("- left", "left", 2),
            (" -right", "right", 4),
            ("++merged", "merged", 2),
            ("++merged tail", "merged tail", 4),
        ] {
            let mut app = review_line(merge.trim(), patch_line);
            press(&mut app, KeyCode::Char('B'));
            assert_eq!(app.status, None, "{patch_line}");
            assert_eq!(app.history, History::Blame);
            let line = app.commits[app.selected].blame().unwrap();
            assert_eq!(code(line), expected);
            assert_eq!(line.number, number);
            assert_eq!(
                line.source_path,
                if patch_line.starts_with("++") {
                    b"moved.txt".to_vec()
                } else {
                    b"file.txt".to_vec()
                }
            );
        }
    }

    #[test]
    fn file_view_uses_textconv_before_and_after_loading_blame() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        fs::create_dir("nested").unwrap();
        fs::write(".gitattributes", "nested/* diff=readable\n").unwrap();
        git(&["config", "diff.readable.textconv", "sed '1d'"]);
        fs::write("nested/text.txt", "header\nfirst\nsecond\n").unwrap();
        fs::write("nested/binary.dat", b"header\0\nfirst\nsecond\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "files with readable text conversion"]);
        std::env::set_current_dir("nested").unwrap();

        for path in ["text.txt", "binary.dat"] {
            let mut app = git::load_show_app(&[format!("HEAD:./{path}")]).unwrap();
            let contents = |app: &App| {
                app.commits
                    .iter()
                    .map(|commit| code(commit.blame().unwrap()))
                    .collect::<Vec<_>>()
            };
            assert!(app.pending_blame.is_some());
            assert_eq!(contents(&app), ["first", "second"]);
            app.selected = 1;
            input::handle(
                Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
                &mut app,
            );
            assert!(app.status.is_none(), "{:?}", app.status);
            assert!(app.pending_blame.is_none());
            assert!(app.log_format.shows(Field::Hash));
            assert_eq!(contents(&app), ["first", "second"]);
            assert_eq!(app.selected, 1);
        }
    }

    #[test]
    fn file_view_defers_blame_and_retries_the_pinned_snapshot() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        fs::write("file.txt", "a\nb\nc\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "base"]);
        let hash = || {
            String::from_utf8(
                Command::new("git")
                    .args(["rev-parse", "HEAD"])
                    .output()
                    .unwrap()
                    .stdout,
            )
            .unwrap()
            .trim()
            .to_owned()
        };
        let base = hash();
        fs::write("file.txt", "A\nb\nc\n").unwrap();
        git(&["commit", "-qam", "edit"]);
        let snapshot = hash();
        let tree = String::from_utf8(
            Command::new("git")
                .args(["rev-parse", &format!("{base}^{{tree}}")])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_owned();
        let object = format!(".git/objects/{}/{}", &tree[..2], &tree[2..]);
        let saved = ".git/saved-parent-tree";
        // Reading a blob must work without walking history at all.
        fs::rename(&object, saved).unwrap();
        let mut app = git::load_show_app(&["HEAD:file.txt".into()]).unwrap();
        assert!(app.pending_blame.is_some());
        assert_eq!(app.log_format.text(&app.commits[0]), "1 A");
        assert_eq!(app.commits.len(), 3);
        let render = |app: &mut App| {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 20)).unwrap();
            terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
            let buffer = terminal.backend().buffer();
            buffer.content[..usize::from(buffer.area.width)]
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>()
        };
        let header = render(&mut app);
        assert!(header.contains("File"));
        // The file view has no detail tab.
        assert!(!header.contains("Show"));
        assert!(app.pending_blame.is_some());
        app.selected = 1;
        app.log_offset = 1;
        let key = |code, app: &mut App| {
            input::handle(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)), app)
        };
        key(KeyCode::Char('x'), &mut app);
        assert!(app.status.is_some());
        assert!(!app.log_format.shows(Field::Hash));
        assert!(app.pending_blame.is_some());
        assert_eq!(app.selected, 1);
        fs::rename(saved, &object).unwrap();
        fs::write("file.txt", "new tip\n").unwrap();
        git(&["commit", "-qam", "later"]);
        key(KeyCode::Char('x'), &mut app);
        assert!(app.status.is_none());
        assert!(app.pending_blame.is_none());
        assert!(app.log_format.shows(Field::Hash));
        assert!(!app.log_format.shows(Field::Author));
        assert!(!app.log_format.shows(Field::Date));
        assert_eq!(app.selected, 1);
        assert_eq!(app.log_offset, 1);
        assert_eq!(app.commits.len(), 3);
        assert_eq!(app.commits[0].hash, snapshot);
        assert_eq!(app.commits[1].hash, base);
        // Once loaded, additional columns must reuse attribution.
        fs::rename(&object, saved).unwrap();
        key(KeyCode::Char('a'), &mut app);
        assert!(app.log_format.shows(Field::Author));
        assert!(app.status.is_none());
        fs::rename(saved, &object).unwrap();
        assert!(render(&mut app).contains("Blame"));
        // Hiding attribution returns to the file view, with blame kept.
        key(KeyCode::Char('x'), &mut app);
        key(KeyCode::Char('a'), &mut app);
        let header = render(&mut app);
        assert!(header.contains("File"));
        assert!(!header.contains("Show"));
        key(KeyCode::Enter, &mut app);
        assert_eq!(app.mode, Mode::Log);
        assert!(app.status.is_some());

        // Line history needs a visible column; the file view does not load it.
        let spec = format!("{snapshot}:file.txt");
        let mut file = git::load_show_app(&[spec]).unwrap();
        for code in [
            KeyCode::Enter,
            KeyCode::Tab,
            KeyCode::Char('p'),
            KeyCode::Right,
        ] {
            key(code, &mut file);
            assert_eq!(file.mode, Mode::Log);
            assert_eq!(file.selected, 0);
            assert!(file.pending_blame.is_some());
        }
        key(KeyCode::Char('x'), &mut file);
        key(KeyCode::Char('p'), &mut file);
        assert!(file.status.is_none(), "{:?}", file.status);
        assert_eq!(code(file.commits[0].blame().unwrap()), "a");
        key(KeyCode::Backspace, &mut file);
        assert_eq!(code(file.commits[0].blame().unwrap()), "A");
        key(KeyCode::Enter, &mut file);
        assert_eq!(file.mode, Mode::Show);
        assert_eq!(file.commits[file.selected].hash, snapshot);
    }

    #[test]
    fn file_view_accepts_literal_braces_in_revisions() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        fs::write("file.txt", "contents\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "fix: handle { inputs"]);
        git(&["branch", "brace{branch"]);

        // Git uses extended regular expressions here. A bare opening brace
        // is invalid on some platforms; [{] matches it portably.
        for spec in ["HEAD^{/handle [{]}:file.txt", "brace{branch:file.txt"] {
            let mut app = git::load_show_app(&[spec.into()])
                .unwrap_or_else(|error| panic!("{spec}: {error}"));
            assert!(app.pending_blame.is_some());
            assert_eq!(app.log_format.text(&app.commits[0]), "1 contents");
            assert!(app.ensure_blame(), "{spec}: {:?}", app.status);
            assert_eq!(app.log_format.text(&app.commits[0]), "1 contents");
        }
    }

    #[test]
    fn file_view_is_not_shadowed_by_a_commit_suffix_in_another_filename() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        fs::write("file", "expected\n").unwrap();
        fs::write("file^{commit}", "other\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "files with revision syntax in their names"]);

        let mut app = git::load_show_app(&["HEAD:file".into()]).unwrap();
        assert!(app.pending_blame.is_some());
        assert_eq!(app.commits.len(), 1);
        assert_eq!(app.log_format.text(&app.commits[0]), "1 expected");
        assert!(app.ensure_blame(), "{:?}", app.status);
        assert_eq!(app.log_format.text(&app.commits[0]), "1 expected");
    }

    #[test]
    fn file_view_resolves_paths_and_rejects_non_text_objects() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        fs::create_dir("nested").unwrap();
        fs::write("root.txt", "root\n").unwrap();
        fs::write("nested/file.txt", "line\n\nlast").unwrap();
        fs::write("empty.txt", "").unwrap();
        fs::write("binary", b"a\0b").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "files: original"]);
        std::env::set_current_dir("nested").unwrap();
        for spec in ["HEAD:nested/file.txt", "HEAD:./file.txt"] {
            let mut app = git::load_show_app(&[spec.into()]).unwrap();
            assert_eq!(app.commits.len(), 3);
            assert_eq!(app.log_format.text(&app.commits[1]), "2 ");
            assert_eq!(app.log_format.text(&app.commits[2]), "3 last");
            assert!(app.ensure_blame(), "{:?}", app.status);
            assert_eq!(app.commits.len(), 3);
        }
        for spec in ["HEAD:root.txt", "HEAD:../root.txt"] {
            assert_eq!(git::load_show_app(&[spec.into()]).unwrap().commits.len(), 1);
        }
        assert_eq!(
            git::load_show_app(&["HEAD^{/files: original}".into()])
                .unwrap()
                .mode,
            Mode::Show
        );
        assert_eq!(
            git::load_show_app(&["HEAD^{/files: original}:root.txt".into()])
                .unwrap()
                .commits
                .len(),
            1
        );
        // :/files resolves, but the whole search pattern does not.
        let error = git::load_show_app(&[":/files: missing".into()])
            .err()
            .unwrap();
        assert!(error.contains("Not a valid object name"), "{error}");
        assert!(git::load_show_app(&["HEAD:empty.txt".into()])
            .unwrap()
            .commits
            .is_empty());
        for args in [
            vec!["HEAD:binary"],
            vec!["HEAD:nested"],
            vec!["HEAD:missing"],
            vec!["HEAD:../../root.txt"],
            vec!["HEAD:"],
            vec![":root.txt"],
            vec!["--stat", "HEAD:root.txt"],
            vec!["HEAD:root.txt", "--", "root.txt"],
        ] {
            assert!(
                git::load_show_app(&args.iter().map(|arg| (*arg).into()).collect::<Vec<_>>())
                    .is_err(),
                "{args:?}"
            );
        }
    }

    #[test]
    fn blame_parent_honors_relative_repository_environment() {
        with_relative_repository_environment(
            "blame_parent_honors_relative_repository_environment",
            false,
        );
    }

    #[test]
    fn historical_blame_honors_relative_repository_environment() {
        with_relative_repository_environment(
            "historical_blame_honors_relative_repository_environment",
            true,
        );
    }

    fn with_relative_repository_environment(test: &str, historical_only: bool) {
        // Isolate repository environment variables from concurrent tests.
        const CHILD: &str = "GLOG_TEST_BLAME_REPOSITORY_ENV_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let (rows, _) = load(&["../file.txt".into()]).unwrap();
            assert_eq!(code(rows[2].blame().unwrap()), "B");
            if historical_only {
                // Exercise the second command independently of the parent diff.
                let line = rows[2].blame().unwrap();
                let (parent, path) = line.previous.as_ref().unwrap();
                let rows =
                    blame(Some(parent), path, Some(&line.file.top), &line.file.date).unwrap();
                assert_eq!(code(rows[1].blame().unwrap()), "b");
            } else {
                let mut app = App::new(rows);
                app.selected = 2;
                app.blame_parent();
                assert_eq!(app.status, None);
                assert_eq!(app.selected, 1);
                assert_eq!(code(app.commits[1].blame().unwrap()), "b");
                app.blame_back();
                assert_eq!(app.status, None);
                assert_eq!(app.selected, 2);
                assert_eq!(code(app.commits[2].blame().unwrap()), "B");
            }
            return;
        }
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        fs::write("file.txt", "a\nb\nc\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "base"]);
        fs::write("file.txt", "prefix\na\nB\nc\n").unwrap();
        git(&["commit", "-qam", "edit"]);
        fs::create_dir("sub").unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &format!("blame::tests::{test}")])
            .current_dir(directory.path().join("sub"))
            .env(CHILD, "1")
            .env("GIT_DIR", "../.git")
            .env("GIT_WORK_TREE", "..")
            .output()
            .unwrap();
        drop(_cwd);
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{stdout}");
        assert!(stdout.contains("1 passed"), "{stdout}");
    }

    #[test]
    fn bare_blame_parent_preserves_object_environment() {
        with_bare_object_environment("bare_blame_parent_preserves_object_environment", false);
    }

    #[test]
    fn bare_historical_blame_preserves_object_environment() {
        with_bare_object_environment("bare_historical_blame_preserves_object_environment", true);
    }

    fn with_bare_object_environment(test: &str, historical_only: bool) {
        // Each child has its own environment and cached repository identity.
        const CHILD: &str = "GLOG_TEST_BARE_BLAME_OBJECT_ENV_CHILD";
        if std::env::var_os(CHILD).is_some() {
            let (rows, _) = load(&["HEAD".into(), "file.txt".into()]).unwrap();
            assert_eq!(code(rows[2].blame().unwrap()), "B");
            if historical_only {
                let line = rows[2].blame().unwrap();
                let (parent, path) = line.previous.as_ref().unwrap();
                let rows =
                    blame(Some(parent), path, Some(&line.file.top), &line.file.date).unwrap();
                assert_eq!(code(rows[1].blame().unwrap()), "b");
            } else {
                let mut app = App::new(rows);
                app.selected = 2;
                app.blame_parent();
                assert_eq!(app.status, None);
                assert_eq!(app.selected, 1);
                assert_eq!(code(app.commits[1].blame().unwrap()), "b");
                app.blame_back();
                assert_eq!(app.status, None);
                assert_eq!(app.selected, 2);
                assert_eq!(code(app.commits[2].blame().unwrap()), "B");
            }
            return;
        }
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q", "work"]);
        std::env::set_current_dir("work").unwrap();
        fs::write("file.txt", "a\nb\nc\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "base"]);
        fs::write("file.txt", "prefix\na\nB\nc\n").unwrap();
        git(&["commit", "-qam", "edit"]);
        git(&["clone", "--bare", "-q", ".", "../bare.git"]);
        std::env::set_current_dir(directory.path()).unwrap();
        fs::rename("bare.git/objects", "objects").unwrap();
        fs::create_dir("bare.git/objects").unwrap();
        fs::create_dir("launch").unwrap();

        // Start outside the repository to exercise both retaining these
        // variables and anchoring their paths before changing directories.
        for variable in ["GIT_ALTERNATE_OBJECT_DIRECTORIES", "GIT_OBJECT_DIRECTORY"] {
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", &format!("blame::tests::{test}")])
                .current_dir(directory.path().join("launch"))
                .env(CHILD, "1")
                .env("GIT_DIR", "../bare.git")
                .env(variable, "../objects")
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(output.status.success(), "{variable}: {stdout}\n{stderr}");
            assert!(stdout.contains("1 passed"), "{stdout}");
        }
    }

    #[test]
    fn blame_bare_repository_follows_parent_and_returns() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q", "work"]);
        std::env::set_current_dir("work").unwrap();
        fs::write("file.txt", "a\nb\nc\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "initial"]);
        fs::write("file.txt", "a\nB\nc\n").unwrap();
        git(&["commit", "-qam", "edit"]);
        git(&["clone", "--bare", "-q", ".", "../bare.git"]);
        std::env::set_current_dir("../bare.git").unwrap();

        let (rows, _) = load(&["HEAD".into(), "file.txt".into()]).unwrap();
        let mut app = App::new(rows);
        app.selected = 1;
        assert_eq!(code(app.commits[1].blame().unwrap()), "B");
        app.blame_parent();
        assert_eq!(app.status, None);
        assert_eq!(app.selected, 1);
        assert_eq!(code(app.commits[1].blame().unwrap()), "b");
        app.blame_back();
        assert_eq!(app.status, None);
        assert_eq!(app.selected, 1);
        assert_eq!(code(app.commits[1].blame().unwrap()), "B");
        app.switch_mode();
        assert_eq!(app.status, None);
        assert_eq!(app.mode, Mode::Show);
        assert!(crate::ansi::plain(&app.show_text).contains("+B"));
    }

    #[cfg(unix)]
    #[test]
    fn blame_parent_preserves_non_utf8_paths_across_renames() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        // Import historical names without creating them on disk: macOS
        // filesystems reject these bytes, but Git trees can still contain them.
        let history = concat!(
            "commit refs/heads/review\n",
            "committer Alice <alice@example.com> 1000000000 +0000\n",
            "data 4\nbase\n",
            "M 100644 inline \"old-\\377.txt\"\ndata 10\na\nb\nc\nd\ne\n\n",
            "commit refs/heads/review\n",
            "committer Alice <alice@example.com> 1000000001 +0000\n",
            "data 4\nedit\n",
            "M 100644 inline \"old-\\377.txt\"\ndata 10\na\nB\nc\nd\ne\n\n",
            "commit refs/heads/review\n",
            "committer Alice <alice@example.com> 1000000002 +0000\n",
            "data 6\nrename\nR \"old-\\377.txt\" new.txt\n\ndone\n",
        );
        assert!(crate::git::pipe_through_bytes(
            Command::new("git").args(["fast-import", "--quiet"]),
            history.as_bytes(),
        )
        .is_some());
        let (rows, _) = load(&["review".into(), "new.txt".into()]).unwrap();
        let mut app = App::new(rows);
        app.selected = 1;
        app.blame_parent();
        assert_eq!(app.status, None);
        assert_eq!(app.selected, 1);
        assert_eq!(code(app.commits[1].blame().unwrap()), "b");
        app.blame_back();
        app.switch_mode();
        let file = app.show_rows[app.show_cursor].file.unwrap();
        assert_eq!(
            crate::diff::file_sections(&app.show_text)[file].path_bytes,
            b"old-\xff.txt"
        );
    }

    #[test]
    fn blame_sha256_includes_uncommitted_lines() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q", "--object-format=sha256"]);
        fs::write("file.txt", "a\nb\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "initial"]);
        fs::write("file.txt", "a\nB\n").unwrap();

        let (rows, _) = load(&["--date=short".into(), "file.txt".into()]).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].kind, CommitKind::Revision);
        assert_eq!(rows[0].hash.len(), 64);
        assert_eq!(rows[1].kind, CommitKind::WorkingTree);
        assert_eq!(code(rows[1].blame().unwrap()), "B");
        assert!(LogFormat::default()
            .text(&rows[1])
            .contains("Not committed yet"));
    }

    #[test]
    fn blame_parent_ignores_configured_inter_hunk_context() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        fs::write("file.txt", "a\nb\nc\nd\ne\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "initial"]);
        fs::write("file.txt", "a\nB\nc\nD\ne\n").unwrap();
        git(&["commit", "-qam", "edit"]);
        git(&["config", "diff.interHunkContext", "10"]);

        let (rows, _) = load(&["file.txt".into()]).unwrap();
        let commit = &rows[3];
        let (parent, selected) = load_parent(commit, commit.blame().unwrap()).unwrap();
        assert_eq!(selected, 3);
        assert_eq!(code(parent[selected].blame().unwrap()), "d");
    }

    #[test]
    fn tabs_expand_across_highlighted_spans() {
        let style = Style::default().fg(Color::Red);
        let spans =
            crate::ansi::display_spans(vec![Span::raw("\tab"), Span::styled("\tc\x07\r", style)]);
        assert_eq!(spans[0].content, "        ab");
        assert_eq!(spans[1].content, "      c\u{fffd}");
        assert_eq!(spans[1].style, style);
    }

    #[test]
    fn tabs_after_wide_characters_use_display_columns() {
        for prefix in ["界", "👩‍💻"] {
            for spans in [
                vec![Span::raw(format!("{prefix}\tX"))],
                vec![
                    Span::raw(prefix),
                    Span::styled("\tX", Style::default().fg(Color::Red)),
                ],
            ] {
                let output = crate::ansi::display_spans(spans);
                let text: String = output.iter().map(|span| span.content.as_ref()).collect();
                assert_eq!(text, format!("{prefix}      X"));
            }
        }
    }

    #[test]
    fn tabs_after_combining_marks_use_display_columns() {
        for spans in [
            vec![Span::raw("e\u{301}\tX")],
            vec![
                Span::raw("e"),
                Span::styled("\u{301}\tX", Style::default().fg(Color::Red)),
            ],
        ] {
            let output = crate::ansi::display_spans(spans);
            let text: String = output.iter().map(|span| span.content.as_ref()).collect();
            assert_eq!(text, "e\u{301}       X");
        }
    }

    #[test]
    fn parent_lines_follow_hunks_and_land_on_what_a_change_replaced() {
        // Old: a b c d e. New: a B c x d (b changed, x added, e deleted).
        let diff = "@@ -2 +2 @@\n-b\n+B\n@@ -3,0 +4 @@\n+x\n@@ -5 +5,0 @@\n-e\n";
        for (line, expected) in [(1, 1), (2, 2), (3, 3), (4, 3), (5, 4), (6, 6)] {
            assert_eq!(parent_line(diff, line), expected, "line {line}");
        }
        assert_eq!(parent_line("@@ -1,2 +0,0 @@\n-a\n-b\n", 1), 3);
        assert_eq!(parent_line("@@ -0,0 +1 @@\n+a\n", 1), 1);
        assert_eq!(parent_line("Binary files differ\n", 7), 7);
    }

    #[test]
    fn patch_positions_count_empty_context_lines() {
        // diff.suppressBlankEmpty prints blank context lines without a space.
        let patch = "@@ -1,3 +1,3 @@\n a\n\n-b\n+c\n";
        assert_eq!(patch_position(patch, 2), Some((None, 2)));
        assert_eq!(patch_position(patch, 3), Some((Some(0), 3)));
        assert_eq!(patch_position(patch, 4), Some((None, 3)));
    }

    #[test]
    fn blame_follows_renames_to_parents_and_back() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        let git = |args: &[&str], date: &str| {
            let output = std::process::Command::new("git")
                .args([
                    "-c",
                    "user.name=Committer",
                    "-c",
                    "user.email=committer@example.com",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .env("GIT_AUTHOR_NAME", "Alice")
                .env("GIT_AUTHOR_EMAIL", "alice@example.com")
                .env("GIT_AUTHOR_DATE", date)
                .env("GIT_COMMITTER_DATE", date)
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
        };
        let first = "2020-01-01T12:00:00+00:00";
        let second = "2020-02-01T12:00:00+00:00";
        git(&["init", "-q"], first);
        fs::write("old.txt", "a\nb\nc\nd\ne\n").unwrap();
        git(&["add", "."], first);
        git(
            &[
                "commit",
                "-qm",
                "feat: add",
                "-m",
                "Co-authored-by: Claude <noreply@anthropic.com>",
            ],
            first,
        );
        git(&["mv", "old.txt", "new.txt"], second);
        fs::write("new.txt", "a\nB\nc\nx\nd\n").unwrap();
        git(&["commit", "-qam", "fix: change"], second);
        fs::write("new.txt", "a\nB\nc\nx\nd!\n").unwrap();
        fs::create_dir("sub").unwrap();
        // The guard restores the original directory.
        std::env::set_current_dir("sub").unwrap();

        let (rows, selected) = load(&["-L".into(), "4".into(), "../new.txt".into()]).unwrap();
        assert_eq!(selected, 3);
        let lines: Vec<_> = rows.iter().map(|row| row.blame().unwrap()).collect();
        assert_eq!(
            lines.iter().map(|line| code(line)).collect::<Vec<_>>(),
            ["a", "B", "c", "x", "d!"]
        );
        assert_eq!(
            lines
                .iter()
                .map(|line| line.starts_chunk)
                .collect::<Vec<_>>(),
            [true, true, true, true, true]
        );
        assert_eq!(rows[0].hash, rows[2].hash);
        assert_eq!(lines[0].source_path, b"old.txt");
        assert_eq!(lines[1].source_path, b"new.txt");
        assert_eq!(rows[0].author_date, "2020-01-01");
        assert!(rows[0].collaborators.claude);
        assert_eq!(rows[4].kind, CommitKind::WorkingTree);
        let text: String = spans(&rows[0], lines[0], true, &LogFormat::default())
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(
            text,
            format!("{} 2020-01-01 Alice+❋           1 a", rows[0].short_hash)
        );

        let mut app = App::new(rows);
        app.selected = selected;
        assert_eq!(app.history, History::Blame);
        let key = |code, app: &mut App| {
            input::handle(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)), app)
        };
        key(KeyCode::Left, &mut app);
        assert_eq!(app.selected, 2);
        key(KeyCode::Right, &mut app);
        assert_eq!(app.selected, 3);
        for code in ['t', 'M', 'z', 'm', 'r', 's'] {
            key(KeyCode::Char(code), &mut app);
        }
        assert!(app.status.is_none());
        let text = |app: &App, index: usize| app.log_format.text(&app.commits[index]);
        assert!(app.log_format.shows(crate::log_format::Field::Subject));
        for code in ['x', 'd', 'a'] {
            key(KeyCode::Char(code), &mut app);
        }
        assert_eq!(text(&app, 3), "4 x");
        key(KeyCode::Char('d'), &mut app);
        assert_eq!(text(&app, 3), "2020-02-01 4 x");

        // x was added in the rename, so the parent's old.txt opens at the
        // line before it.
        key(KeyCode::Char('p'), &mut app);
        assert_eq!(app.status, None);
        assert!(app.context.ends_with(" -- old.txt"), "{}", app.context);
        assert_eq!(app.commits.len(), 5);
        assert_eq!(app.selected, 2);
        assert_eq!(code(app.commits[2].blame().unwrap()), "c");
        key(KeyCode::Char('p'), &mut app);
        assert!(app.status.as_ref().unwrap().contains("added this file"));
        key(KeyCode::Backspace, &mut app);
        assert_eq!(app.commits.len(), 5);
        assert_eq!(app.selected, 3);
        assert_eq!(code(app.commits[4].blame().unwrap()), "d!");
        key(KeyCode::Backspace, &mut app);
        assert!(app.status.as_ref().unwrap().contains("No earlier blame"));

        app.selected = 1;
        key(KeyCode::Enter, &mut app);
        assert_eq!(app.mode, Mode::Show);
        let row = &app.show_rows[app.show_cursor];
        assert!(row.text.contains("new.txt"), "{}", row.text);
        key(KeyCode::Esc, &mut app);
        assert_eq!(app.mode, Mode::Log);
        app.selected = 4;
        key(KeyCode::Char('p'), &mut app);
        assert!(app.status.as_ref().unwrap().contains("not committed"));
    }

    #[test]
    fn blame_rejects_missing_and_extra_arguments() {
        for args in [
            vec![],
            vec!["a", "b", "c"],
            vec!["--", "a", "b"],
            vec!["-L", "x", "a"],
            vec!["-L", "0", "a"],
            vec!["--watch", "a"],
        ] {
            let args: Vec<_> = args.into_iter().map(str::to_owned).collect();
            assert!(load(&args).is_err(), "{args:?}");
        }
    }
}
