//! GitHub code results and immutable remote files, using gh's authentication.
use std::{
    fmt::Write,
    process::{Command, Stdio},
    sync::{mpsc, Arc, OnceLock},
};

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Deserialize;

use crate::{
    app::{App, History, Mode},
    git, grep,
    pager::PagerView,
    range_diff::View,
};

const FIELDS: &str = "path,repository,sha,textMatches,url";
const MAX_FILE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize)]
pub struct Repository {
    #[serde(rename = "nameWithOwner")]
    pub name: String,
    url: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct File {
    pub path: String,
    pub repository: Repository,
    /// Search returns a blob SHA, not a commit SHA. Keep the supplied web URL.
    sha: String,
    url: String,
    #[serde(skip)]
    contents: Arc<OnceLock<Arc<str>>>,
}

#[derive(Deserialize)]
struct SearchResult {
    #[serde(flatten)]
    file: File,
    #[serde(rename = "textMatches", default)]
    fragments: Vec<Fragment>,
}

#[derive(Deserialize)]
struct Fragment {
    property: String,
    fragment: String,
    matches: Vec<Match>,
}

#[derive(Deserialize)]
struct Match {
    indices: [usize; 2],
}

/// Snippet offsets are relative to the fragment, never absolute file lines.
#[derive(Clone, Debug)]
pub enum Location {
    Line(usize),
    Fragment { text: Arc<str>, line: usize },
    File,
}

impl Location {
    pub fn line(&self) -> Option<usize> {
        match self {
            Self::Line(line) => Some(*line),
            _ => None,
        }
    }

    fn in_file(&self, source: &str) -> Option<usize> {
        match self {
            Self::Line(line) => Some(*line),
            Self::File => None,
            Self::Fragment { text, line } => {
                // Fragments may not keep the file's CRLF line endings, so match
                // with LF only, which keeps the newlines that count lines.
                let source = without_carriage_returns(source);
                let text = without_carriage_returns(text);
                let mut occurrences = source.match_indices(text.as_ref());
                let (offset, _) = occurrences.next()?;
                // Repeated fragments don't identify a unique source position.
                if occurrences.next().is_some() {
                    return None;
                }
                Some(
                    source[..offset]
                        .bytes()
                        .filter(|&byte| byte == b'\n')
                        .count()
                        + line
                        + 1,
                )
            }
        }
    }
}

fn without_carriage_returns(text: &str) -> std::borrow::Cow<'_, str> {
    if text.contains("\r\n") {
        text.replace("\r\n", "\n").into()
    } else {
        text.into()
    }
}

fn search_command(args: &[String]) -> Result<Command, String> {
    let mut options = true;
    let mut value = false;
    for arg in args {
        if value {
            value = false;
            continue;
        }
        if !options {
            continue;
        }
        if arg == "--" {
            options = false;
            continue;
        }
        let name = arg.split('=').next().unwrap_or(arg);
        if matches!(
            name,
            "--json" | "--jq" | "--template" | "--web" | "-q" | "-t" | "-w"
        ) || (arg.starts_with("-q") || arg.starts_with("-t") || arg.starts_with("-w"))
            && !arg.starts_with("--")
        {
            return Err(format!(
                "{name} controls gh output; glog supplies the search interface"
            ));
        }
        value = matches!(
            name,
            "--extension"
                | "--filename"
                | "--language"
                | "--limit"
                | "--match"
                | "--owner"
                | "--repo"
                | "--size"
                | "-L"
                | "-R"
        ) && !arg.contains('=');
    }
    let mut command = Command::new("gh");
    // Put glog's layout flags before user arguments, including a possible --.
    command
        .args(["search", "code", "--json", FIELDS])
        .args(args);
    Ok(command)
}

fn run(command: &mut Command) -> Result<Vec<u8>, String> {
    let output = command
        .stdin(Stdio::null())
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_PAGER", "cat")
        .output()
        .map_err(|error| {
            format!(
                "Could not run gh; install GitHub CLI and authenticate with gh auth login: {error}"
            )
        })?;
    if !output.status.success() {
        return Err(git::stderr_message("GitHub request failed", &output.stderr));
    }
    Ok(output.stdout)
}

pub fn load_app(args: &[String]) -> Result<App, String> {
    let output = run(&mut search_command(args)?)?;
    app_from_json(&output)
}

struct ReadFile<'a> {
    path: &'a str,
    repo: Option<&'a str>,
    reference: Option<&'a str>,
}

impl<'a> ReadFile<'a> {
    fn parse(args: &'a [String]) -> Result<Self, String> {
        let mut path = None;
        let mut repo = None;
        let mut reference = None;
        let mut options = true;
        let mut args = args.iter().map(String::as_str);
        while let Some(arg) = args.next() {
            if options && arg == "--" {
                options = false;
                continue;
            }
            if options && arg.starts_with('-') {
                // Accept the forms `gh` does: `--repo=X`, `--repo X`, `-R X`,
                // `-R=X`, and `-RX`.
                let (flag, attached) = match arg.strip_prefix("-R") {
                    Some(rest) if !rest.is_empty() => {
                        ("-R", Some(rest.strip_prefix('=').unwrap_or(rest)))
                    }
                    _ => match arg.split_once('=') {
                        Some((flag, value)) => (flag, Some(value)),
                        None => (arg, None),
                    },
                };
                let target = match flag {
                    "--repo" | "-R" => &mut repo,
                    "--ref" => &mut reference,
                    _ => return Err(format!("Unsupported read-file option: {flag}")),
                };
                let value = attached.or_else(|| args.next().filter(|value| *value != "--"));
                *target = Some(
                    value
                        .filter(|value| !value.is_empty())
                        .ok_or_else(|| format!("{flag} requires a value"))?,
                );
            } else if path.replace(arg).is_some() {
                return Err("Expected a single GitHub file path".into());
            }
        }
        let path = path
            .filter(|path| !path.is_empty())
            .ok_or("Expected a GitHub file path (see glog gh repo read-file --help)")?;
        Ok(Self {
            path,
            repo,
            reference,
        })
    }

    fn arguments(&self) -> Vec<String> {
        let mut args = Vec::new();
        if !self.path.starts_with('-') {
            args.push(self.path.to_owned());
        }
        args.extend(self.repo.map(|repo| format!("--repo={repo}")));
        args.extend(self.reference.map(|reference| format!("--ref={reference}")));
        if self.path.starts_with('-') {
            args.extend(["--".into(), self.path.into()]);
        }
        args
    }

    fn context(&self) -> String {
        let mut context = String::from("gh repo read-file");
        for argument in self.arguments() {
            let _ = write!(context, " {}", shell_word(&argument));
        }
        context
    }
}

fn shell_word(word: &str) -> String {
    if word
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"-_./:=".contains(&byte))
    {
        return word.to_owned();
    }
    if word.chars().any(char::is_control) {
        let mut quoted = String::from("$'");
        for byte in word.bytes() {
            let _ = write!(quoted, "\\{byte:03o}");
        }
        quoted.push('\'');
        return quoted;
    }
    format!("'{}'", word.replace('\'', "'\\''"))
}

pub fn load_file_app(args: &[String]) -> Result<App, String> {
    #[derive(Deserialize)]
    struct Response {
        path: String,
        #[serde(rename = "htmlUrl")]
        url: String,
        size: usize,
        content: String,
    }
    let request = ReadFile::parse(args)?;
    let mut command = Command::new("gh");
    command
        .args(["repo", "read-file", "--json", "path,htmlUrl,size,content"])
        .args(request.arguments());
    let response: Response = serde_json::from_slice(&run(&mut command)?)
        .map_err(|error| format!("Could not read GitHub file: {error}"))?;
    if !response.url.starts_with("https://") || response.url.chars().any(char::is_control) {
        return Err("GitHub returned an invalid file URL".into());
    }
    let contents = decode_contents(response.size, &response.content)?;
    Ok(Preview {
        view: FileView::new(&response.path, &contents, None, response.url),
        context: request.context(),
    }
    .into_app())
}

fn app_from_json(output: &[u8]) -> Result<App, String> {
    let results: Vec<SearchResult> = serde_json::from_slice(output)
        .map_err(|error| format!("Could not read gh search code results: {error}"))?;
    let mut files = Vec::new();
    let mut invalid = None;
    for result in results {
        // Skip a malformed result rather than discarding the valid ones.
        if let Err(error) = result.file.validate() {
            invalid = Some(error);
            continue;
        }
        let mut hits = Vec::new();
        for fragment in result
            .fragments
            .iter()
            .filter(|fragment| fragment.property == "content")
        {
            let text: Arc<str> = fragment.fragment.as_str().into();
            let mut offset = 0;
            for (line, source) in fragment.fragment.split_inclusive('\n').enumerate() {
                let end = offset + source.len();
                let ranges: Vec<_> = fragment
                    .matches
                    .iter()
                    .filter_map(|hit| {
                        let start = hit.indices[0].max(offset);
                        let end = hit.indices[1].min(end);
                        (start < end
                            && source.is_char_boundary(start - offset)
                            && source.is_char_boundary(end - offset))
                        .then(|| start - offset..end - offset)
                    })
                    .collect();
                if !ranges.is_empty() {
                    hits.push(grep::Hit::new(
                        Location::Fragment {
                            text: text.clone(),
                            line,
                        },
                        &highlight(source.trim_end_matches('\n'), &ranges),
                    ));
                }
                offset = end;
            }
        }
        if hits.is_empty() {
            hits.push(grep::Hit::new(Location::File, "(file match)"));
        }
        files.push((result.file, hits));
    }
    if let (true, Some(error)) = (files.is_empty(), invalid) {
        return Err(error);
    }
    let mut app = App::new(Vec::new());
    app.history = History::GithubSearch;
    app.mode = Mode::Grep;
    app.grep_view = Some(grep::View::github(files));
    Ok(app)
}

pub(crate) fn clean(text: &str) -> String {
    text.chars()
        .filter(|&ch| !ch.is_control() || ch == '\t' || ch == '\n')
        .collect()
}

fn highlight(text: &str, ranges: &[std::ops::Range<usize>]) -> String {
    let mut result = String::new();
    let mut highlighted = false;
    for (offset, ch) in text.char_indices() {
        let matched = ranges.iter().any(|range| range.contains(&offset));
        if matched != highlighted {
            result.push_str(if matched { "\x1b[1;33m" } else { "\x1b[0m" });
            highlighted = matched;
        }
        if !ch.is_control() || ch == '\t' {
            result.push(ch);
        }
    }
    if highlighted {
        result.push_str("\x1b[0m");
    }
    result
}

impl File {
    fn host(&self) -> Result<&str, String> {
        self.repository
            .url
            .strip_prefix("https://")
            .and_then(|rest| rest.split_once('/'))
            .map(|(host, _)| host)
            .filter(|host| {
                // GitHub Enterprise may be served on a port.
                let (name, port) = host.split_once(':').unwrap_or((host, "1"));
                !name.is_empty()
                    && name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b".-".contains(&byte))
                    && !port.is_empty()
                    && port.bytes().all(|byte| byte.is_ascii_digit())
            })
            .ok_or_else(|| "GitHub returned an invalid repository URL".into())
    }

    fn validate(&self) -> Result<(), String> {
        let host = self.host()?;
        let parts: Vec<_> = self.repository.name.split('/').collect();
        if parts.len() != 2
            || parts.iter().any(|part| {
                part.is_empty()
                    || *part == "."
                    || *part == ".."
                    || !part
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
            })
            || !(40..=64).contains(&self.sha.len())
            || !self.sha.bytes().all(|byte| byte.is_ascii_hexdigit())
            || !self
                .url
                .starts_with(&format!("https://{host}/{}/blob/", self.repository.name))
        {
            return Err("GitHub returned an invalid code search result".into());
        }
        Ok(())
    }

    pub fn label(&self) -> String {
        match self.host() {
            Ok("github.com") => self.repository.name.clone(),
            Ok(host) => format!("{host}/{}", self.repository.name),
            Err(_) => self.repository.name.clone(),
        }
    }

    pub fn url(&self, line: Option<usize>) -> String {
        let base = self.url.split('#').next().unwrap_or(&self.url);
        match line {
            Some(line) => format!("{base}#L{line}"),
            None => base.to_owned(),
        }
    }

    fn load(&self) -> Result<Arc<str>, String> {
        if let Some(contents) = self.contents.get() {
            return Ok(contents.clone());
        }
        let mut command = Command::new("gh");
        command
            .args(["api", "--hostname", self.host()?, "--method", "GET"])
            .arg(format!(
                "repos/{}/git/blobs/{}",
                self.repository.name, self.sha
            ));
        let contents = decode_blob(&run(&mut command)?, &self.sha)?;
        let _ = self.contents.set(contents.clone());
        Ok(contents)
    }
}

fn decode_blob(output: &[u8], expected_sha: &str) -> Result<Arc<str>, String> {
    #[derive(Deserialize)]
    struct Blob {
        sha: String,
        encoding: String,
        content: String,
        size: usize,
    }
    let blob: Blob = serde_json::from_slice(output)
        .map_err(|error| format!("Could not read GitHub file: {error}"))?;
    if blob.sha != expected_sha || blob.encoding != "base64" {
        return Err("GitHub did not return the requested file snapshot".into());
    }
    decode_contents(blob.size, &blob.content)
}

fn decode_contents(size: usize, content: &str) -> Result<Arc<str>, String> {
    if size > MAX_FILE_BYTES || content.len() > MAX_FILE_BYTES * 2 {
        return Err("GitHub file is too large to display (limit: 8 MiB)".into());
    }
    let encoded: String = content
        .chars()
        .filter(|ch| !ch.is_ascii_whitespace())
        .collect();
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|error| format!("Could not decode GitHub file: {error}"))?;
    if bytes.len() > MAX_FILE_BYTES || bytes.contains(&0) {
        return Err("GitHub file is binary or too large to display".into());
    }
    String::from_utf8(bytes)
        .map(Arc::from)
        .map_err(|_| "GitHub file is not UTF-8 text".into())
}

#[derive(Clone)]
pub struct Target {
    pub file: File,
    pub location: Location,
}

struct Loaded {
    file: File,
    contents: Arc<str>,
    line: Option<usize>,
}

impl Target {
    fn load(self) -> Result<Loaded, String> {
        let contents = self.file.load()?;
        let line = self.location.in_file(&contents);
        Ok(Loaded {
            file: self.file,
            contents,
            line,
        })
    }
}

impl Loaded {
    fn url(&self) -> String {
        self.file.url(self.line)
    }

    fn into_preview(self) -> Preview {
        let repo = self.file.label();
        let url = self.file.url(None);
        let host = self.file.host().unwrap_or("github.com");
        let prefix = format!("https://{host}/{}/blob/", self.file.repository.name);
        let reference = url
            .strip_prefix(&prefix)
            .and_then(|tail| {
                let segments = self.file.path.split('/').count();
                tail.rsplitn(segments + 1, '/').nth(segments)
            })
            .filter(|reference| !reference.is_empty());
        let context = ReadFile {
            path: &self.file.path,
            repo: Some(&repo),
            reference,
        }
        .context();
        Preview {
            view: FileView::new(&self.file.path, &self.contents, self.line, url),
            context,
        }
    }
}

impl FileView {
    fn new(path: &str, contents: &str, line: Option<usize>, url: String) -> Self {
        let lines: Vec<_> = contents.lines().map(clean).collect();
        let width = lines.len().max(1).to_string().len();
        let colored = git::highlight_sources([(path, lines.as_slice())])
            .and_then(|mut snippets| snippets.pop());
        let mut numbered = String::new();
        for (index, line) in lines.iter().enumerate() {
            let number = index + 1;
            if let Some(colored) = colored.as_ref().and_then(|lines| lines[index].as_ref()) {
                // Delta retains the leading context space, which is part of the gutter.
                let _ = writeln!(numbered, "{number:>width$} {colored}");
            } else {
                let _ = writeln!(numbered, "{number:>width$}  {line}");
            }
        }
        let mut pager = View::new(&numbered);
        // Request a reveal after the first frame has measured wrapping.
        pager.cursor = line.unwrap_or(1).saturating_sub(1);
        pager.move_by(0);
        Self { url, pager }
    }
}

pub struct Preview {
    view: FileView,
    context: String,
}

impl Preview {
    pub fn into_app(self) -> App {
        let mut app = App::new(Vec::new());
        app.history = History::GithubSearch;
        app.mode = Mode::GithubFile;
        app.context = self.context;
        app.github_file_view = Some(self.view);
        app
    }
}

pub struct FileView {
    url: String,
    pub pager: View,
}

impl FileView {
    pub fn url(&self) -> String {
        let base = self.url.split('#').next().unwrap_or(&self.url);
        format!("{base}#L{}", self.pager.cursor + 1)
    }
}

#[derive(Clone, Copy)]
pub enum Action {
    Open,
    Browse,
}

pub enum Outcome {
    Open(Box<Preview>),
    Browse(String),
}

pub struct Pending {
    pub receiver: mpsc::Receiver<Result<Outcome, String>>,
}

impl Pending {
    pub fn start(target: Target, action: Action) -> Self {
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let result = match action {
                Action::Open => target
                    .load()
                    .map(|loaded| Outcome::Open(Box::new(loaded.into_preview()))),
                Action::Browse => {
                    let file_url = target.file.url(None);
                    let url = if matches!(target.location, Location::File) {
                        file_url
                    } else {
                        // A failed position lookup must not prevent opening the file.
                        target.load().map(|loaded| loaded.url()).unwrap_or(file_url)
                    };
                    Ok(Outcome::Browse(url))
                }
            };
            let _ = sender.send(result);
        });
        Self { receiver }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ansi, pager::PagerView};
    use serde_json::json;

    const BLOB: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const COMMIT: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const CONTENTS: &str = "header\ncontext\nlet café = needle;\nneedle tail\ncontext\nafter\n";

    #[test]
    fn read_file_accepts_gh_flag_forms() {
        let args: Vec<_> = ["-Rcli/cli", "--ref", "--", "README.md"]
            .map(String::from)
            .into();
        assert!(ReadFile::parse(&args).is_err());
        let args: Vec<_> = ["-Rcli/cli", "--ref=main", "README.md"]
            .map(String::from)
            .into();
        let request = ReadFile::parse(&args).unwrap();
        assert_eq!(request.repo, Some("cli/cli"));
        assert_eq!(request.reference, Some("main"));
        assert_eq!(request.path, "README.md");
    }

    fn results() -> Vec<u8> {
        let fragment = "context\nlet café = needle;\nneedle tail\ncontext";
        let matches: Vec<_> = fragment
            .match_indices("needle")
            .map(|(offset, text)| json!({"indices": [offset, offset + text.len()], "text": text}))
            .collect();
        serde_json::to_vec(&json!([
            {
                "path": "src/file space.rs", "sha": BLOB,
                "url": format!("https://github.com/owner/one/blob/{COMMIT}/src/file%20space.rs"),
                "repository": {"nameWithOwner": "owner/one", "url": "https://github.com/owner/one"},
                "textMatches": [{"property": "content", "fragment": fragment, "matches": matches}]
            },
            {
                "path": "src/file space.rs", "sha": BLOB,
                "url": format!("https://github.com/owner/two/blob/{COMMIT}/src/file%20space.rs"),
                "repository": {"nameWithOwner": "owner/two", "url": "https://github.com/owner/two"},
                "textMatches": [{"property": "path", "fragment": "src/file space.rs", "matches": []}]
            }
        ])).unwrap()
    }

    #[test]
    fn fragments_locate_matches_in_crlf_files() {
        let mut app = app_from_json(&results()).unwrap();
        let view = app.grep_view.as_mut().unwrap();
        assert!(view.find("café", false));
        let target = view.github_target().unwrap();
        let crlf = CONTENTS.replace('\n', "\r\n");
        assert_eq!(target.location.in_file(&crlf), Some(3));
    }

    #[test]
    fn fragments_keep_byte_offsets_and_do_not_invent_absolute_line_numbers() {
        let mut app = app_from_json(&results()).unwrap();
        let view = app.grep_view.as_mut().unwrap();
        assert_eq!(view.match_count(), 3);
        assert!(view.find("café", false));
        let target = view.github_target().unwrap();
        assert_eq!(target.location.line(), None);
        assert_eq!(target.location.in_file(CONTENTS), Some(3));
        assert_eq!(target.location.in_file(&CONTENTS.repeat(2)), None);
        assert!(view.find("needle tail", false));
        let target = view.github_target().unwrap();
        assert_eq!(target.location.in_file(CONTENTS), Some(4));
        assert!(view.find("(file match)", false));
        let target = view.github_target().unwrap();
        assert_eq!(target.file.repository.name, "owner/two");
        assert_eq!(target.location.in_file(CONTENTS), None);
        assert_eq!(
            target.file.url(Some(4)),
            format!("https://github.com/owner/two/blob/{COMMIT}/src/file%20space.rs#L4")
        );
        let mut empty = app_from_json(b"[]").unwrap();
        assert!(empty.is_empty());
        assert!(!app.has_log_view());
    }

    #[test]
    fn invalid_results_do_not_hide_valid_ones() {
        let result = |name: &str, host: &str| {
            json!({
                "path": "file.rs", "sha": BLOB,
                "url": format!("https://{host}/{name}/blob/{COMMIT}/file.rs"),
                "repository": {"nameWithOwner": name, "url": format!("https://{host}/{name}")},
                "textMatches": []
            })
        };
        let output = serde_json::to_vec(&json!([
            result("owner/../escape", "github.com"),
            result("owner/valid", "github.com"),
            result("owner/enterprise", "ghe.example.com:8443"),
        ]))
        .unwrap();
        let mut app = app_from_json(&output).unwrap();
        let view = app.grep_view.as_mut().unwrap();
        assert_eq!(view.match_count(), 2);
        assert!(view.find("(file match)", false));
        let names: Vec<_> = std::iter::from_fn(|| {
            let name = view.github_target()?.file.repository.name.clone();
            view.find("(file match)", false);
            Some(name)
        })
        .take(2)
        .collect();
        assert!(names.contains(&"owner/valid".to_string()));
        assert!(names.contains(&"owner/enterprise".to_string()));
        let invalid =
            serde_json::to_vec(&json!([result("owner/../escape", "github.com")])).unwrap();
        assert!(app_from_json(&invalid).is_err());
    }

    #[test]
    fn query_arguments_cannot_replace_the_structured_output() {
        for flag in [
            "--json=path",
            "--jq",
            "--template",
            "--web",
            "-w=false",
            "-q.foo",
            "-tanything",
        ] {
            assert!(
                search_command(&["needle".into(), flag.into()]).is_err(),
                "{flag}"
            );
        }
        let args: Vec<String> = [
            "--repo",
            "owner/repo",
            "--filename",
            "--web",
            "--limit=5",
            "--",
            "needle -word --json",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        let command = search_command(&args).unwrap();
        let forwarded: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_str().unwrap())
            .collect();
        assert_eq!(forwarded[..4], ["search", "code", "--json", FIELDS]);
        assert_eq!(forwarded[4..], args);
    }

    #[test]
    fn blob_decoding_requires_the_searched_snapshot_and_text() {
        let blob = |sha, source: &[u8]| {
            serde_json::to_vec(&json!({
                "sha": sha, "encoding": "base64", "size": source.len(),
                "content": STANDARD.encode(source).as_bytes().chunks(12)
                    .map(|chunk| std::str::from_utf8(chunk).unwrap()).collect::<Vec<_>>().join("\n")
            }))
            .unwrap()
        };
        assert_eq!(
            &*decode_blob(&blob(BLOB, CONTENTS.as_bytes()), BLOB).unwrap(),
            CONTENTS
        );
        assert!(decode_blob(&blob(COMMIT, CONTENTS.as_bytes()), BLOB).is_err());
        assert!(decode_blob(&blob(BLOB, b"binary\0data"), BLOB).is_err());
        assert!(decode_blob(&blob(BLOB, b"\xff"), BLOB).is_err());
        assert_eq!(
            ansi::plain(&highlight("café needle", std::slice::from_ref(&(6..12)))),
            "café needle"
        );
    }

    #[cfg(unix)]
    #[test]
    fn remote_search_opens_pinned_files_and_browser_lines_without_a_checkout() {
        use crate::{
            git::tests::{CurrentDirGuard, TestDirectory},
            input, ui,
        };
        use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
        use ratatui::{backend::TestBackend, Terminal};
        use std::{
            fs,
            os::unix::fs::PermissionsExt,
            path::Path,
            time::{Duration, Instant},
        };

        struct Environment(Vec<(String, Option<std::ffi::OsString>)>);
        impl Environment {
            fn set(&mut self, name: &str, value: impl AsRef<std::ffi::OsStr>) {
                self.0.push((name.into(), std::env::var_os(name)));
                std::env::set_var(name, value);
            }
        }
        impl Drop for Environment {
            fn drop(&mut self) {
                for (name, old) in self.0.drain(..).rev() {
                    match old {
                        Some(value) => std::env::set_var(name, value),
                        None => std::env::remove_var(name),
                    }
                }
            }
        }
        fn executable(path: &Path, source: &str) {
            fs::write(path, source).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        fn until(mut ready: impl FnMut() -> bool) {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !ready() {
                assert!(Instant::now() < deadline, "GitHub fixture did not finish");
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        let root = directory.path();
        let mut environment = Environment(Vec::new());
        let original_path = std::env::var_os("PATH").unwrap_or_default();
        let mut paths = vec![root.to_owned()];
        paths.extend(std::env::split_paths(&original_path));
        environment.set("PATH", std::env::join_paths(paths).unwrap());
        environment.set("GLOG_TEST_DELTA_PATH", original_path);
        environment.set("GLOG_TEST_DELTA_FAIL", root.join("delta-fail"));
        environment.set("GLOG_DELTA", "1");
        executable(
            &root.join("delta"),
            r#"#!/bin/sh
if test "$3" != --tabs=0; then
    PATH="$GLOG_TEST_DELTA_PATH"
    if command -v delta >/dev/null; then exec delta "$@"; fi
    exec cat
fi
if test -f "$GLOG_TEST_DELTA_FAIL"; then
    cat >/dev/null
    echo invalid output
    exit 0
fi
awk 'NR == 3 && $0 == "+++ b/file space.rs" { fragment = 1 }
     NR == 5 && fragment && $0 != " context" { exit 1 }
     NR > 4 {
         if (length($0) > 3000) $0 = substr($0, 1, 3000)
         sub(/header/, "\033[35mheader\033[0m")
         sub(/let/, "\033[34mlet\033[0m")
         sub(/needle/, "\033[35mneedle\033[0m")
     }
     { print }'
"#,
        );
        let long_line = ["x".repeat(3500)];
        let short_line = ["let x = 1;".to_owned()];
        let highlighted =
            git::highlight_sources([("long.rs", &long_line[..]), ("short.rs", &short_line[..])])
                .unwrap();
        assert_eq!(highlighted[0], [None]);
        assert_eq!(
            highlighted[1][0].as_deref(),
            Some(" \x1b[34mlet\x1b[0m x = 1;")
        );
        for (name, file) in [
            ("GLOG_TEST_SEARCH_RESULTS", "results.json"),
            ("GLOG_TEST_SEARCH_BLOB", "blob.json"),
            ("GLOG_TEST_READ_FILE", "read-file.json"),
            ("GLOG_TEST_SEARCH_CALLS", "calls"),
            ("GLOG_TEST_SEARCH_BROWSER", "browser"),
            ("GLOG_TEST_SEARCH_FAIL", "fail"),
        ] {
            environment.set(name, root.join(file));
        }
        fs::write("results.json", results()).unwrap();
        fs::write("blob.json", serde_json::to_vec(&json!({"sha": BLOB, "size": CONTENTS.len(), "encoding": "base64", "content": STANDARD.encode(CONTENTS)})).unwrap()).unwrap();
        executable(&root.join("gh"), "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"$GLOG_TEST_SEARCH_CALLS\"\ncase \"$1\" in\nsearch) cat \"$GLOG_TEST_SEARCH_RESULTS\";;\nrepo) cat \"$GLOG_TEST_READ_FILE\";;\napi) if test -f \"$GLOG_TEST_SEARCH_FAIL\"; then echo 'fixture request denied' >&2; exit 1; fi; cat \"$GLOG_TEST_SEARCH_BLOB\";;\n*) exit 2;;\nesac\n");
        executable(
            &root.join(if cfg!(target_os = "macos") {
                "open"
            } else {
                "xdg-open"
            }),
            "#!/bin/sh\nprintf '%s' \"$1\" > \"$GLOG_TEST_SEARCH_BROWSER\"\n",
        );
        let mut app = load_app(&["needle".into(), "--repo".into(), "owner/one".into()]).unwrap();
        let press = |app: &mut App, key| {
            input::handle(Event::Key(KeyEvent::new(key, KeyModifiers::NONE)), app);
        };
        let mut terminal = Terminal::new(TestBackend::new(100, 16)).unwrap();
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        let screen = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(screen.contains("owner/one"));
        assert!(screen.contains("owner/two"));
        let cells = terminal.backend().buffer().content();
        assert!(cells
            .iter()
            .any(|cell| { cell.symbol() == "l" && cell.fg == ratatui::style::Color::Blue }));
        assert!(cells.windows("needle".len()).any(|cells| {
            cells
                .iter()
                .map(|cell| cell.symbol())
                .eq("needle".split("").filter(|ch| !ch.is_empty()))
                && cells
                    .iter()
                    .all(|cell| cell.fg == ratatui::style::Color::Yellow)
        }));
        app.search = Some("café".into());
        app.next_match(false);
        let selected = app.grep_view.as_ref().unwrap().cursor;
        fs::write("fail", "").unwrap();
        press(&mut app, KeyCode::Enter);
        until(|| {
            app.poll_github_file();
            app.status
                .as_deref()
                .is_some_and(|status| status.contains("fixture request denied"))
        });
        assert_eq!(app.mode, Mode::Grep);
        assert_eq!(app.grep_view.as_ref().unwrap().cursor, selected);
        press(&mut app, KeyCode::Char('e'));
        until(|| {
            app.poll_github_file();
            app.poll_browser();
            fs::read_to_string("browser").is_ok_and(|url| url.ends_with("/src/file%20space.rs"))
        });
        assert_eq!(
            app.mode,
            Mode::Grep,
            "failed line lookups still open the file URL"
        );
        fs::remove_file("fail").unwrap();
        press(&mut app, KeyCode::Char('e'));
        until(|| {
            app.poll_github_file();
            app.poll_browser();
            fs::read_to_string("browser").is_ok_and(|url| url.ends_with("#L3"))
        });
        assert_eq!(
            fs::read_to_string("browser").unwrap(),
            format!("https://github.com/owner/one/blob/{COMMIT}/src/file%20space.rs#L3")
        );
        assert!(app.pending_editor.is_none());
        press(&mut app, KeyCode::Enter);
        until(|| {
            app.poll_github_file();
            app.mode == Mode::GithubFile
        });
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        assert_eq!(app.github_file_view.as_ref().unwrap().pager.cursor, 2);
        let buffer = terminal.backend().buffer();
        let header: String = (0..100).map(|x| buffer[(x, 0)].symbol()).collect();
        assert!(header.contains("glog gh repo read-file 'src/file space.rs' --repo=owner/one"));
        let footer: String = (0..100)
            .map(|x| buffer[(x, buffer.area.bottom() - 1)].symbol())
            .collect();
        assert!(footer.contains("Esc/Backspace back"));
        let preview_command =
            format!("gh repo read-file 'src/file space.rs' --repo=owner/one --ref={COMMIT}");
        assert_eq!(app.context, preview_command);
        assert!(buffer
            .content()
            .iter()
            .any(|cell| { cell.symbol() == "h" && cell.fg == ratatui::style::Color::Magenta }));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Char('e'));
        until(|| {
            app.poll_browser();
            fs::read_to_string("browser").is_ok_and(|url| url.ends_with("#L4"))
        });
        assert!(app.pending_editor.is_none());
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.mode, Mode::Grep);
        assert_eq!(app.grep_view.as_ref().unwrap().cursor, selected);
        assert!(
            app.status.is_none(),
            "returning from a loaded file must not restore its loading message: {:?}",
            app.status
        );
        fs::write("delta-fail", "").unwrap();
        press(&mut app, KeyCode::Enter);
        until(|| {
            app.poll_github_file();
            app.mode == Mode::GithubFile
        });
        terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let screen: String = buffer.content().iter().map(|cell| cell.symbol()).collect();
        assert!(screen.contains("1  header"));
        assert!(screen.contains("3  let café = needle;"));
        press(&mut app, KeyCode::Esc);

        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Esc);
        assert!(
            !app.poll_github_file(),
            "cancelled loads cannot replace the results"
        );
        assert_eq!(app.mode, Mode::Grep);
        press(&mut app, KeyCode::Char('z'));
        assert!(app
            .grep_view
            .as_ref()
            .unwrap()
            .github_match_target()
            .is_none());
        press(&mut app, KeyCode::Enter);
        assert!(
            !app.cancel_github_file(),
            "Enter unfolds a file header instead of fetching it"
        );
        assert!(app
            .grep_view
            .as_ref()
            .unwrap()
            .github_match_target()
            .is_some());
        app.search = Some("(file match)".into());
        app.next_match(false);
        press(&mut app, KeyCode::Char('e'));
        until(|| {
            app.poll_github_file();
            app.poll_browser();
            fs::read_to_string("browser").is_ok_and(|url| url.contains("/owner/two/"))
        });
        let calls = fs::read_to_string("calls").unwrap();
        assert!(calls.starts_with(&format!(
            "search\ncode\n--json\n{FIELDS}\nneedle\n--repo\nowner/one\n"
        )));
        assert_eq!(
            calls
                .matches(&format!("repos/owner/one/git/blobs/{BLOB}"))
                .count(),
            3,
            "failed fetches retry; successful snapshots stay cached"
        );
        assert!(
            !calls.contains(COMMIT),
            "commit IDs must not be used as blob IDs"
        );
        let url = format!("https://github.com/owner/one/blob/{COMMIT}/src/file%20space.rs");
        fs::write(
            "read-file.json",
            serde_json::to_vec(&json!({
                "path": "src/file space.rs", "htmlUrl": url, "size": CONTENTS.len(),
                "content": STANDARD.encode(CONTENTS),
            }))
            .unwrap(),
        )
        .unwrap();
        let args = ["src/file space.rs", "-R", "owner/one", "--ref", COMMIT].map(str::to_owned);
        let mut direct = load_file_app(&args).unwrap();
        assert_eq!(direct.context, preview_command);
        assert_eq!(direct.mode, Mode::GithubFile);
        assert_eq!(
            direct.github_file_view.as_ref().unwrap().url(),
            format!("{url}#L1")
        );
        terminal.draw(|frame| ui::draw(frame, &mut direct)).unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(screen.contains("3  let café = needle;"));
        assert!(!screen.contains("Esc/Backspace back"));
        assert!(fs::read_to_string("calls").unwrap().ends_with(&format!(
            "repo\nread-file\n--json\npath,htmlUrl,size,content\nsrc/file space.rs\n--repo=owner/one\n--ref={COMMIT}\n"
        )));
        assert!(!root.join(".git").exists());
    }
}
