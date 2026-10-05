//! `glog blame`: one row per line of a file, each carrying the commit that
//! last changed it.

use std::{collections::HashMap, path::PathBuf, process::Command, sync::Arc};

use ratatui::{
    style::{Color, Style},
    text::Span,
};

use crate::{
    git::{self, Annotation, Collaborators, Commit, CommitKind},
    log_format::{Field, LogFormat},
};

const UNCOMMITTED_AUTHOR: &str = "Not committed yet";
const MAX_AUTHOR_WIDTH: usize = 20;
const TAB_WIDTH: usize = 4;
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

pub fn load_file(revision: &str, path: &str) -> Result<crate::app::App, String> {
    if revision.is_empty() || path.is_empty() {
        return Err("use glog show REV:path to read a committed text file".into());
    }
    let output = Command::new("git")
        .args(["rev-parse", "--verify", "--end-of-options"])
        .arg(format!("{revision}^{{commit}}"))
        .output()
        .map_err(|error| format!("could not resolve commit: {error}"))?;
    if !output.status.success() {
        return Err(git::stderr_message(
            "could not resolve commit",
            &output.stderr,
        ));
    }
    let revision = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let mut path = path.as_bytes().to_vec();
    // REV:path is rooted at the repository, except for Git's explicit ./ and
    // ../ forms. Resolve those before moving commands to the repository root.
    if path.starts_with(b"./") || path.starts_with(b"../") {
        let prefix = Command::new("git")
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
    let output = git::repository_env(&mut Command::new("git"), &top)
        .current_dir(&top)
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
    let output = git::repository_env(&mut Command::new("git"), &top)
        .current_dir(&top)
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
    let rows = render_lines(lines, HashMap::new(), top.clone(), "", &path);
    let mut app = crate::app::App::new(rows);
    app.history = crate::app::History::Blame;
    for field in [Field::Hash, Field::Author, Field::Date] {
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
    top: PathBuf,
    date: String,
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
    let output = git::repository_env(&mut Command::new("git"), &line.file.top)
        .current_dir(&line.file.top)
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
    let rows = blame(Some(parent), path, Some(&line.file.top), &line.file.date)?;
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
    for header in diff.lines().filter_map(|l| l.strip_prefix("@@ -")) {
        let Some((old, new)) = header.split_once(" +") else {
            continue;
        };
        let new = new.split(' ').next().unwrap_or(new);
        let range = |range: &str| -> Option<(usize, usize)> {
            match range.split_once(',') {
                Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
                None => Some((range.parse().ok()?, 1)),
            }
        };
        let (Some((old_start, old_count)), Some((new_start, new_count))) = (range(old), range(new))
        else {
            continue;
        };
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
    let mut command = Command::new("git");
    if let Some(top) = top {
        git::repository_env(&mut command, top).current_dir(top);
    }
    let output = command
        .env("LC_ALL", "C")
        .args(["-c", "core.quotepath=true", "blame", "--porcelain"])
        .args(revision)
        .arg("--")
        .arg(git::raw_path(path))
        .output()
        .map_err(|error| format!("could not run git blame: {error}"))?;
    if !output.status.success() {
        return Err(git::stderr_message("git blame failed", &output.stderr));
    }
    let lines = parse_porcelain(&String::from_utf8_lossy(&output.stdout))?;
    let top = match top {
        Some(top) => top.clone(),
        None => git::repository_directory()?,
    };
    let details = commit_details(&lines, date)?;
    Ok(render_lines(lines, details, top, date, path))
}

fn render_lines(
    lines: Vec<PorcelainLine>,
    details: HashMap<String, Details>,
    top: PathBuf,
    date: &str,
    path: &[u8],
) -> Vec<Commit> {
    let mut highlighted = highlight(&crate::diff::display_path(path), &lines).map(Vec::into_iter);
    let width = |text: &str| Span::raw(text).width();
    let file = Arc::new(BlameFile {
        top,
        date: date.to_owned(),
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
                code: display_code(match highlighted.as_mut().and_then(Iterator::next) {
                    Some(spans) => spans,
                    None => vec![Span::raw(line.code)],
                }),
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
fn commit_details(lines: &[PorcelainLine], date: &str) -> Result<HashMap<String, Details>, String> {
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
        Command::new("git")
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

/// Expand tabs and replace control characters, keeping each span's style.
fn display_code(spans: Vec<Span<'static>>) -> Vec<Span<'static>> {
    let mut column = 0;
    let mut output = Vec::with_capacity(spans.len());
    for span in spans {
        let content = span.content.strip_suffix('\r').unwrap_or(&span.content);
        let mut text = String::with_capacity(content.len());
        for (index, segment) in content.split('\t').enumerate() {
            if index > 0 {
                let spaces = TAB_WIDTH - column % TAB_WIDTH;
                text.extend(std::iter::repeat_n(' ', spaces));
                column += spaces;
            }
            let start = text.len();
            text.extend(segment.chars().map(|ch| {
                if ch.is_control() {
                    char::REPLACEMENT_CHARACTER
                } else {
                    ch
                }
            }));
            // Measure rendered text together so combining marks and emoji
            // sequences use the same display width as the terminal UI.
            column += Span::raw(&text[start..]).width();
        }
        output.push(Span::styled(text, span.style));
    }
    output
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
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 20)).unwrap();
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        let rendered: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(rendered.contains("File"));
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

        let spec = format!("{snapshot}:file.txt");
        let mut parent = git::load_show_app(std::slice::from_ref(&spec)).unwrap();
        key(KeyCode::Char('p'), &mut parent);
        assert!(parent.status.is_none(), "{:?}", parent.status);
        assert_eq!(code(parent.commits[0].blame().unwrap()), "a");
        key(KeyCode::Backspace, &mut parent);
        assert_eq!(code(parent.commits[0].blame().unwrap()), "A");
        let mut detail = git::load_show_app(&[spec]).unwrap();
        key(KeyCode::Enter, &mut detail);
        assert_eq!(detail.mode, Mode::Show);
        assert_eq!(detail.commits[detail.selected].hash, snapshot);
        assert!(detail.pending_blame.is_none());
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

        for spec in ["HEAD^{/handle {}:file.txt", "brace{branch:file.txt"] {
            let mut app = git::load_show_app(&[spec.into()])
                .unwrap_or_else(|error| panic!("{spec}: {error}"));
            assert!(app.pending_blame.is_some());
            assert_eq!(app.log_format.text(&app.commits[0]), "1 contents");
            assert!(app.ensure_blame(), "{spec}: {:?}", app.status);
            assert_eq!(app.log_format.text(&app.commits[0]), "1 contents");
        }
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
        let spans = display_code(vec![Span::raw("\tab"), Span::styled("\tc\x07\r", style)]);
        assert_eq!(spans[0].content, "    ab");
        assert_eq!(spans[1].content, "  c\u{fffd}");
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
                let output = display_code(spans);
                let text: String = output.iter().map(|span| span.content.as_ref()).collect();
                assert_eq!(text, format!("{prefix}  X"));
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
            let output = display_code(spans);
            let text: String = output.iter().map(|span| span.content.as_ref()).collect();
            assert_eq!(text, "e\u{301}   X");
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
        key(KeyCode::Char('d'), &mut app);

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
