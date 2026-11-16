//! Classify full file snapshots, then count only lines touched by Git's patch.
use std::{
    collections::HashMap,
    io::Read,
    ops::Range,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use mezura_core::{EngineConfig, Languages, SpanKind};

use crate::{ansi, diff::FileSection, git};

pub(crate) const MAX_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub additions: usize,
    pub deletions: usize,
}

/// None means the file cannot be classified reliably; retain its ordinary stats.
#[cfg(test)]
pub fn count(patch: &str, file: &FileSection, repository: &git::Repository) -> Option<Counts> {
    count_all(&[(patch, file)], repository).remove(0)
}

/// Count each file's patch, reading every snapshot it needs with a fixed
/// number of Git processes rather than several per file. None means the file
/// cannot be classified reliably; retain its ordinary stats.
pub fn count_all(
    files: &[(&str, &FileSection)],
    repository: &git::Repository,
) -> Vec<Option<Counts>> {
    let requests: Vec<_> = files
        .iter()
        .map(|(patch, file)| Request::new(patch, file, repository))
        .collect();
    let ids: Vec<_> = requests
        .iter()
        .flatten()
        .flat_map(|request| [&request.old, &request.new])
        .filter_map(|side| match side {
            Side::Blob(id) => Some(id.as_str()),
            _ => None,
        })
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let blobs = read_blobs(&ids, repository);
    // Git can print a worktree blob's hash without writing the object. Use
    // the file only when its bytes still match that exact hash.
    let mut fallbacks: HashMap<PathBuf, Option<Vec<u8>>> = HashMap::new();
    for request in requests.iter().flatten() {
        for side in [&request.old, &request.new] {
            if let Side::Blob(id) = side {
                if matches!(blobs.get(id.as_str()), Some(Blob::Missing)) {
                    if let Some(worktree) = &request.worktree {
                        fallbacks
                            .entry(worktree.clone())
                            .or_insert_with(|| read_worktree(worktree));
                    }
                }
            }
        }
    }
    let hashed: Vec<_> = fallbacks
        .iter()
        .filter(|(_, bytes)| bytes.is_some())
        .map(|(path, _)| path.clone())
        .collect();
    let hashes: HashMap<_, _> = hashed.iter().zip(hash_files(&hashed, repository)).collect();
    let snapshot = |side: &Side, worktree: &Option<PathBuf>| -> Option<String> {
        let bytes = match side {
            Side::Absent => return Some(String::new()),
            Side::Worktree => read_worktree(worktree.as_ref()?)?,
            Side::Blob(id) => match blobs.get(id.as_str())? {
                Blob::Contents(bytes) => bytes.clone(),
                Blob::Unusable => return None,
                Blob::Missing => {
                    let worktree = worktree.as_ref()?;
                    let hash = hashes.get(worktree)?.as_deref()?;
                    if !hash.starts_with(id.as_str()) {
                        return None;
                    }
                    fallbacks.get(worktree)?.clone()?
                }
            },
        };
        if bytes.len() as u64 > MAX_BYTES || bytes.contains(&0) {
            return None;
        }
        String::from_utf8(bytes).ok()
    };
    let count = |request: &Option<Request>| {
        let request = request.as_ref()?;
        let old = snapshot(&request.old, &request.worktree)?;
        let new = snapshot(&request.new, &request.worktree)?;
        let old_code = classify(&request.old_path, &old)?;
        let new_code = classify(&request.path, &new)?;
        count_patch(&request.patch, &old, &new, &old_code, &new_code)
    };
    // Classification dominates once snapshots are read; files are independent.
    let threads = std::thread::available_parallelism().map_or(1, usize::from);
    let chunk = requests.len().div_ceil(threads).max(1);
    std::thread::scope(|scope| {
        let workers: Vec<_> = requests
            .chunks(chunk)
            .map(|chunk| {
                let worker = scope.spawn(|| chunk.iter().map(count).collect::<Vec<_>>());
                (chunk.len(), worker)
            })
            .collect();
        // Keep results aligned with their files even if a worker panics.
        workers
            .into_iter()
            .flat_map(|(len, worker)| worker.join().unwrap_or_else(|_| vec![None; len]))
            .collect()
    })
}

/// Where one side of a patch reads its snapshot from.
enum Side {
    Absent,
    Worktree,
    Blob(String),
}

struct Request {
    patch: String,
    old: Side,
    new: Side,
    path: PathBuf,
    old_path: PathBuf,
    /// The file in the checkout, for sides Git describes by worktree contents.
    worktree: Option<PathBuf>,
}

impl Request {
    fn new(patch: &str, file: &FileSection, repository: &git::Repository) -> Option<Self> {
        if file.submodule.is_some() || file.lazy_untracked_path.is_some() {
            return None;
        }
        let patch = ansi::plain(patch);
        let headers: Vec<_> = crate::hunk::file_headers(&patch).collect();
        let absent_old = headers.iter().any(|line| line == "--- /dev/null");
        let absent_new = headers.iter().any(|line| line == "+++ /dev/null");
        let ids = headers
            .iter()
            .find_map(|line| line.strip_prefix("index "))
            .and_then(|line| line.split_whitespace().next())
            .and_then(|ids| ids.split_once(".."))
            // The lazy untracked reader synthesizes an added-file patch without
            // object IDs. Its body is checked against the worktree below.
            .or_else(|| absent_old.then_some(("0", "0")))?;
        let side = |id: &str, absent: bool| {
            if absent {
                Some(Side::Absent)
            } else if id.bytes().all(|byte| byte == b'0') {
                Some(Side::Worktree)
            } else if id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                Some(Side::Blob(id.to_owned()))
            } else {
                None // Combined index headers have several parent IDs.
            }
        };
        let old = side(ids.0, absent_old)?;
        let new = side(ids.1, absent_new)?;
        let path = git::raw_path(&file.path_bytes);
        let old_path = headers
            .iter()
            .find_map(|line| {
                let (bytes, _) =
                    crate::diff::unquote_path(line.strip_prefix("--- ")?.trim_end_matches('\t'))?;
                bytes.strip_prefix(b"a/").map(git::raw_path)
            })
            .unwrap_or_else(|| path.clone());
        let worktree = repository.work_tree().ok().map(|root| root.join(&path));
        Some(Self {
            patch,
            old,
            new,
            path,
            old_path,
            worktree,
        })
    }
}

fn read_worktree(path: &Path) -> Option<Vec<u8>> {
    // Never follow a changed symlink or read a device as source code.
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > MAX_BYTES {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    Some(bytes)
}

enum Blob {
    Contents(Vec<u8>),
    /// Git does not have the object, as for a worktree blob in a diff.
    Missing,
    /// Not a blob, or too large to classify.
    Unusable,
}

/// The size of each object that exists, in one Git process.
pub(crate) fn blob_sizes(ids: &[&str], repository: &git::Repository) -> Vec<Option<(String, u64)>> {
    let queries: Vec<_> = ids.iter().map(|id| id.to_string()).collect();
    git::batch_check(
        &queries,
        "%(objectname) %(objecttype) %(objectsize)",
        Some(repository),
    )
    .into_iter()
    .map(|answer| match answer?.split(' ').collect::<Vec<_>>()[..] {
        [name, "blob", size] => Some((name.to_owned(), size.parse().ok()?)),
        _ => None,
    })
    .collect()
}

fn read_blobs<'a>(ids: &[&'a str], repository: &git::Repository) -> HashMap<&'a str, Blob> {
    let sizes = blob_sizes(ids, repository);
    let wanted: Vec<_> = sizes
        .iter()
        .flatten()
        .filter(|(_, size)| *size <= MAX_BYTES)
        .map(|(name, _)| name.clone())
        .collect();
    let input: String = wanted.iter().map(|name| format!("{name}\n")).collect();
    let output = if wanted.is_empty() {
        Vec::new()
    } else {
        git::pipe_through_bytes(
            git::read_command_at(Some(repository))
                .args(["cat-file", "--batch"])
                .env("GIT_NO_LAZY_FETCH", "1"),
            input.as_bytes(),
        )
        .unwrap_or_default()
    };
    let mut contents = HashMap::new();
    let mut rest = output.as_slice();
    for name in wanted {
        let Some(end) = rest.iter().position(|&byte| byte == b'\n') else {
            break;
        };
        let header = String::from_utf8_lossy(&rest[..end]).into_owned();
        rest = &rest[end + 1..];
        let fields: Vec<_> = header.split(' ').collect();
        let [found, "blob", size] = fields[..] else {
            continue; // A blob that vanished since the size check.
        };
        let Some(size) = size.parse::<usize>().ok().filter(|&size| size < rest.len()) else {
            break;
        };
        if found == name {
            contents.insert(name, rest[..size].to_vec());
        }
        rest = &rest[size + 1..];
    }
    ids.iter()
        .zip(sizes)
        .map(|(&id, size)| {
            let blob = match size {
                None => Blob::Missing,
                Some((name, size)) if size <= MAX_BYTES => contents
                    .get(name.as_str())
                    .map_or(Blob::Unusable, |bytes| Blob::Contents(bytes.clone())),
                Some(_) => Blob::Unusable,
            };
            (id, blob)
        })
        .collect()
}

/// Hash each file's exact bytes as Git would store them, in one Git process.
fn hash_files(paths: &[PathBuf], repository: &git::Repository) -> Vec<Option<String>> {
    // The protocol is line-based, so a path spanning lines cannot be hashed.
    let hashable: Vec<_> = paths
        .iter()
        .filter(|path| !path.as_os_str().as_encoded_bytes().contains(&b'\n'))
        .collect();
    let mut input = Vec::new();
    for path in &hashable {
        input.extend_from_slice(path.as_os_str().as_encoded_bytes());
        input.push(b'\n');
    }
    let output = if hashable.is_empty() {
        String::new()
    } else {
        git::pipe_through(
            git::read_command_at(Some(repository)).args([
                "hash-object",
                "--no-filters",
                "--stdin-paths",
            ]),
            &input,
        )
        .unwrap_or_default()
    };
    let hashes: HashMap<_, _> = hashable.into_iter().zip(output.lines()).collect();
    paths
        .iter()
        .map(|path| hashes.get(path).map(|hash| hash.to_string()))
        .collect()
}

fn classify(path: &Path, source: &str) -> Option<Vec<bool>> {
    // The library currently exposes its line classifier through a file API.
    // Use an isolated snapshot, never a checkout or repository configuration.
    static ENGINE: OnceLock<(EngineConfig, Languages)> = OnceLock::new();
    let (config, languages) = ENGINE.get_or_init(|| {
        let config = EngineConfig {
            detect_tests: false,
            ..EngineConfig::default()
        };
        let (languages, _) = Languages::shipped(&config);
        (config, languages)
    });
    let directory = tempfile::tempdir().ok()?;
    let temporary = directory.path().join(path.file_name()?);
    std::fs::write(&temporary, source).ok()?;
    let explanation = mezura_core::explain_file(&temporary, config, languages.clone()).ok()?;
    let mut docs = if explanation.language == "Python" {
        python_docstrings(source)?
    } else {
        Vec::new()
    };
    docs.sort_by_key(|range| range.start);
    let mut offset = 0;
    let mut result = Vec::new();
    for (text, line) in source.split_inclusive('\n').zip(&explanation.lines) {
        // Mixed code/comment lines count as code. Ordinary multiline strings
        // and heredoc data remain code, including lines starting with '#'.
        let code = line
            .spans
            .iter()
            .filter(|span| span.kind != SpanKind::Comment)
            .any(|span| {
                text.as_bytes()
                    .get(span.from..span.to)
                    .is_some_and(|bytes| {
                        bytes.iter().enumerate().any(|(index, byte)| {
                            !byte.is_ascii_whitespace() && {
                                let position = offset + span.from + index;
                                let index = docs.partition_point(|doc| doc.end <= position);
                                !docs.get(index).is_some_and(|doc| doc.contains(&position))
                            }
                        })
                    })
            });
        result.push(code);
        offset += text.len();
    }
    (result.len() == source.lines().count()).then_some(result)
}

/// A docstring is the first string expression of a module, class, or function.
/// Assignments, bytes, and formatted strings are executable data instead.
fn python_docstrings(source: &str) -> Option<Vec<Range<usize>>> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_python::LANGUAGE.into())
        .ok()?;
    let tree = parser.parse(source, None)?;
    if tree.root_node().has_error() {
        return None;
    }
    fn string(node: tree_sitter::Node<'_>, source: &str) -> bool {
        match node.kind() {
            "string" => {
                let text = &source[node.byte_range()];
                let prefix = text
                    .split(['\'', '"'])
                    .next()
                    .unwrap_or("")
                    .to_ascii_lowercase();
                !prefix.contains(['b', 'f'])
            }
            "concatenated_string" | "parenthesized_expression" => {
                let mut cursor = node.walk();
                let children: Vec<_> = node
                    .named_children(&mut cursor)
                    .filter(|child| child.kind() != "comment")
                    .collect();
                !children.is_empty() && children.iter().all(|child| string(*child, source))
            }
            _ => false,
        }
    }
    let mut docs = Vec::new();
    let mut todo = vec![tree.root_node()];
    while let Some(node) = todo.pop() {
        let body = match node.kind() {
            "module" => Some(node),
            "function_definition" | "class_definition" => node.child_by_field_name("body"),
            _ => None,
        };
        if let Some(body) = body {
            let mut cursor = body.walk();
            let first = body
                .named_children(&mut cursor)
                .find(|child| child.kind() != "comment");
            if let Some(first) = first {
                if first.kind() == "expression_statement"
                    && first.named_child_count() == 1
                    && first
                        .named_child(0)
                        .is_some_and(|child| string(child, source))
                {
                    docs.push(first.byte_range());
                }
            }
        }
        let mut cursor = node.walk();
        todo.extend(node.named_children(&mut cursor));
    }
    Some(docs)
}

fn count_patch(
    patch: &str,
    old: &str,
    new: &str,
    old_code: &[bool],
    new_code: &[bool],
) -> Option<Counts> {
    let old: Vec<_> = old.lines().collect();
    let new: Vec<_> = new.lines().collect();
    let mut position = None;
    let mut counts = Counts::default();
    for line in patch.lines() {
        if let Some(header) = crate::hunk::header(line) {
            let [(start, old_count)] = header.old[..] else {
                return None;
            };
            position = Some((
                start.saturating_sub(1) as usize,
                header.new.0.saturating_sub(1) as usize,
                old_count,
                header.new.1,
            ));
            continue;
        }
        let Some((a, b, old_remaining, new_remaining)) = &mut position else {
            continue;
        };
        // diff.suppressBlankEmpty omits the space prefix on blank context
        // lines. Validate and advance them just like ordinary context.
        let marker = line.as_bytes().first().copied().unwrap_or(b' ');
        let code = line.get(1..).unwrap_or("");
        match marker {
            b'-' => {
                if old.get(*a).copied()? != code {
                    return None;
                }
                counts.deletions += usize::from(*old_code.get(*a)?);
                *a += 1;
                *old_remaining = old_remaining.checked_sub(1)?;
            }
            b'+' => {
                if new.get(*b).copied()? != code {
                    return None;
                }
                counts.additions += usize::from(*new_code.get(*b)?);
                *b += 1;
                *new_remaining = new_remaining.checked_sub(1)?;
            }
            b' ' => {
                // Mismatches can indicate textconv, word diffs, an ignored
                // whitespace change, or a worktree modified since the patch.
                if old.get(*a).copied()? != code || new.get(*b).copied()? != code {
                    return None;
                }
                *a += 1;
                *b += 1;
                *old_remaining = old_remaining.checked_sub(1)?;
                *new_remaining = new_remaining.checked_sub(1)?;
            }
            _ => {}
        }
        if *old_remaining == 0 && *new_remaining == 0 {
            position = None;
        }
    }
    Some(counts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::tests::{git, CurrentDirGuard, TestDirectory};

    #[test]
    fn code_stats_read_snapshots_without_a_git_process_per_file() {
        struct Trace(Option<std::ffi::OsString>);
        impl Drop for Trace {
            fn drop(&mut self) {
                match self.0.take() {
                    Some(value) => std::env::set_var("GIT_TRACE", value),
                    None => std::env::remove_var("GIT_TRACE"),
                }
            }
        }
        let traced = |name: &str, action: &mut dyn FnMut()| {
            let path = std::env::current_dir().unwrap().join(name);
            let trace = Trace(std::env::var_os("GIT_TRACE"));
            std::env::set_var("GIT_TRACE", &path);
            action();
            drop(trace);
            let calls = std::fs::read_to_string(&path).unwrap();
            std::fs::remove_file(path).unwrap();
            calls
        };

        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        let names: Vec<_> = (0..30).map(|n| format!("file {n} café.py")).collect();
        for name in &names {
            std::fs::write(name, "# docs\nvalue = 1\n").unwrap();
        }
        git(&["add", "."]);
        git(&["commit", "-qm", "base"]);
        for name in &names {
            std::fs::write(name, "# new docs\nvalue = 2\n").unwrap();
        }
        git(&["commit", "-qam", "edit"]);
        for name in &names[..15] {
            std::fs::write(name, "# more docs\nvalue = 3\n").unwrap();
        }
        git(&["add", "."]);
        // Unstaged worktree snapshots are absent from the object store.
        for name in &names {
            std::fs::write(name, "# docs again\nvalue = 4\n").unwrap();
        }

        let mut app = git::load_show_app(&["--stat".into()]).unwrap();
        let calls = traced("show-trace", &mut || app.toggle_code_stats());
        assert!(app
            .show_rows
            .iter()
            .any(|row| row.text == "▶ file 7 café.py | +1 −1"));
        assert!(calls.matches("git cat-file").count() <= 2, "{calls}");

        let mut view = crate::status::StatusView::load().unwrap();
        let calls = traced("status-trace", &mut || view.set_code_stats(true));
        assert_eq!(
            view.code_stats_for("file 3 café.py"),
            ["+1 −1", "+1 −1"],
            "staged and unstaged"
        );
        assert!(calls.matches("git cat-file").count() <= 3, "{calls}");
        assert!(calls.matches("git hash-object").count() <= 1, "{calls}");
        assert!(calls.matches("--full-index").count() <= 2, "{calls}");
    }

    #[test]
    fn code_stats_handle_suppressed_blank_context_lines() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        std::fs::write("file.py", "value = 1\n\n# old\nother = 2\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "base"]);
        git(&["config", "diff.suppressBlankEmpty", "true"]);
        std::fs::write("file.py", "value = 1\n\n# new\nother = 3\n").unwrap();

        let app = git::load_diff_app(&[]).unwrap();
        let file = crate::diff::file_sections(&app.show_text).remove(0);
        assert_eq!(
            count(&app.show_text, &file, &git::Repository::main().unwrap()),
            Some(Counts {
                additions: 1,
                deletions: 1
            })
        );
    }

    #[test]
    fn committed_stats_use_full_snapshots_and_keep_filters_and_folder_totals() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        std::fs::create_dir("src").unwrap();
        let docs: String = (0..20)
            .map(|n| format!("documentation line {n}\n"))
            .collect();
        let old = format!("\"\"\"\n{docs}\"\"\"\nvalue = 1\n");
        std::fs::write("src/a.py", &old).unwrap();
        std::fs::write("src/b.sh", "# old comment\necho hi\n").unwrap();
        std::fs::write("other.unknown-glog", "old\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "base"]);
        let new = old
            .replace("documentation line 10", "changed documentation")
            .replace("value = 1", "value = 2");
        std::fs::write("src/a.py", new).unwrap();
        std::fs::write("src/b.sh", "# new comment\necho hi\n").unwrap();
        std::fs::write("other.unknown-glog", "new\n").unwrap();
        git(&["commit", "-qam", "edit"]);
        let mut app = crate::git::load_show_app(&["--stat".into()]).unwrap();
        app.toggle_code_stats();
        assert!(app
            .show_rows
            .iter()
            .any(|row| row.text == "▶ src/a.py | +1 −1"));
        assert!(app
            .show_rows
            .iter()
            .any(|row| row.text == "▶ src/b.sh | +0 −0"));
        assert!(app
            .show_rows
            .iter()
            .any(|row| row.text == "▶ other.unknown-glog | +1 −1"));
        assert!(app
            .show_rows
            .last()
            .unwrap()
            .text
            .contains("2 insertions(+), 2 deletions(-)"));
        assert!(app.show_rows.last().unwrap().text.ends_with("(code stats)"));
        app.show_cursor = app
            .show_rows
            .iter()
            .position(|row| row.text.contains("src/a.py"))
            .unwrap();
        app.move_summary_path(false);
        app.toggle_show_file();
        assert!(app
            .show_rows
            .iter()
            .any(|row| row.text == "▶ src/ (2 files) | +1 −1"));
        app.toggle_file_filter(Some(crate::file_filters::Category::Directory(
            b"src/".to_vec(),
        )));
        assert!(app
            .show_rows
            .last()
            .unwrap()
            .text
            .contains("1 insertion(+), 1 deletion(-)"));
        app.toggle_code_stats();
        app.toggle_file_filter(None);
        assert!(app
            .show_rows
            .last()
            .unwrap()
            .text
            .contains("4 insertions(+), 4 deletions(-)"));
    }

    #[test]
    fn snapshot_stats_handle_additions_renames_and_stale_worktree_data() {
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        std::fs::write("script.py", "# docs\nvalue = 1\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "base"]);
        let repository = git::Repository::main().unwrap();
        let app = git::load_show_app(&[]).unwrap();
        let file = crate::diff::file_sections(&app.show_text).remove(0);
        assert_eq!(
            count(&app.show_text, &file, &repository),
            Some(Counts {
                additions: 1,
                deletions: 0
            })
        );
        git(&["mv", "script.py", "renamed.py"]);
        git(&["commit", "-qm", "rename"]);
        // The original added-file patch must keep using its committed blob,
        // even after that path disappears from the checkout.
        assert_eq!(
            count(&app.show_text, &file, &repository),
            Some(Counts {
                additions: 1,
                deletions: 0
            })
        );
        std::fs::write("renamed.py", "# changed docs\nvalue = 2\n").unwrap();
        let app = git::load_diff_app(&[]).unwrap();
        let file = crate::diff::file_sections(&app.show_text).remove(0);
        assert_eq!(
            count(&app.show_text, &file, &repository),
            Some(Counts {
                additions: 1,
                deletions: 1
            })
        );
        std::fs::write("renamed.py", "unrelated = 3\n").unwrap();
        assert!(count(&app.show_text, &file, &repository).is_none());
    }

    #[test]
    fn python_distinguishes_docstrings_from_assigned_and_formatted_strings() {
        let source = "\"\"\"Module docs.\nmore docs\n\"\"\"\n# comment\nvalue = '''data\n# still data\n'''\ndef f():\n    # before docs\n    ('function docs')\n    return '# literal' # comment\nclass C:\n    \"class docs\"\nf'not docs'\n";
        assert_eq!(
            classify(Path::new("file.py"), source).unwrap(),
            [
                false, false, false, false, true, true, true, true, false, false, true, true,
                false, true
            ]
        );
        assert_eq!(
            classify(
                Path::new("file.py"),
                "(\n    'module ' # explanation\n    'docs'\n)\nvalue = 1\n"
            )
            .unwrap(),
            [false, false, false, false, true]
        );
    }

    #[test]
    fn shell_keeps_heredoc_data_and_quoted_hashes() {
        let source =
            "#!/bin/sh\n# comment\necho '# literal' # comment\ncat <<'END'\n# data\nEND\n\n";
        assert_eq!(
            classify(Path::new("file.sh"), source).unwrap(),
            [false, false, true, true, true, true, false]
        );
    }

    #[test]
    fn other_languages_and_unsupported_files() {
        assert_eq!(
            classify(
                Path::new("file.rs"),
                "/* docs\nmore docs */\nlet s = \"// data\";\n}\n"
            )
            .unwrap(),
            [false, false, true, true]
        );
        assert!(classify(
            Path::new("file.unknown-glog"),
            "# not necessarily a comment\n"
        )
        .is_none());
    }

    #[test]
    fn counts_replacements_instead_of_subtracting_whole_file_totals() {
        let patch = "@@ -1,2 +1,2 @@\n-# old comment\n-old()\n+# new comment\n+new()\n";
        assert_eq!(
            count_patch(
                patch,
                "# old comment\nold()\n",
                "# new comment\nnew()\n",
                &[false, true],
                &[false, true]
            ),
            Some(Counts {
                additions: 1,
                deletions: 1
            })
        );
        assert!(count_patch(patch, "different\n", "", &[], &[]).is_none());
    }
}
