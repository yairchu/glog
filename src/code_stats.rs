//! Classify full file snapshots, then count only lines touched by Git's patch.
use std::{io::Read, ops::Range, path::Path, process::Stdio, sync::OnceLock};

use mezura_core::{EngineConfig, Languages, SpanKind};

use crate::{ansi, diff::FileSection, git};

const MAX_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub additions: usize,
    pub deletions: usize,
}

/// None means the file cannot be classified reliably; retain its ordinary stats.
pub fn count(patch: &str, file: &FileSection, repository: &git::Repository) -> Option<Counts> {
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
    let old = snapshot(ids.0, absent_old, file, repository)?;
    let new = snapshot(ids.1, absent_new, file, repository)?;
    let path = git::raw_path(&file.path_bytes);
    let old_path = headers
        .iter()
        .find_map(|line| {
            let (bytes, _) =
                crate::diff::unquote_path(line.strip_prefix("--- ")?.trim_end_matches('\t'))?;
            bytes.strip_prefix(b"a/").map(git::raw_path)
        })
        .unwrap_or_else(|| path.clone());
    let old_code = classify(&old_path, &old)?;
    let new_code = classify(&path, &new)?;
    count_patch(&patch, &old, &new, &old_code, &new_code)
}

fn snapshot(
    id: &str,
    absent: bool,
    file: &FileSection,
    repository: &git::Repository,
) -> Option<String> {
    if absent {
        return Some(String::new());
    }
    let bytes = if id.bytes().all(|byte| byte == b'0') {
        let path = repository
            .work_tree()
            .ok()?
            .join(git::raw_path(&file.path_bytes));
        // Never follow a changed symlink or read a device as source code.
        let metadata = std::fs::symlink_metadata(&path).ok()?;
        if !metadata.is_file() || metadata.len() > MAX_BYTES {
            return None;
        }
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .ok()?
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        bytes
    } else {
        if !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None; // Combined index headers have several parent IDs.
        }
        let mut child = git::read_command_at(Some(repository))
            .args(["cat-file", "blob", id])
            .env("GIT_NO_LAZY_FETCH", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let mut bytes = Vec::new();
        let read = child
            .stdout
            .take()?
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes);
        if read.is_err() || bytes.len() as u64 > MAX_BYTES {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        if !child.wait().ok()?.success() {
            // Git can print a worktree blob's hash without writing the object.
            // Use the file only when its bytes still match that exact hash.
            let candidate = snapshot("0", false, file, repository)?;
            let hash = git::pipe_through(
                git::read_command_at(Some(repository)).args(["hash-object", "--stdin"]),
                candidate.as_bytes(),
            )?;
            return hash.trim().starts_with(id).then_some(candidate);
        }
        bytes
    };
    if bytes.len() as u64 > MAX_BYTES || bytes.contains(&0) {
        return None;
    }
    String::from_utf8(bytes).ok()
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
            let [(start, _)] = header.old[..] else {
                return None;
            };
            position = Some((
                start.saturating_sub(1) as usize,
                header.new.0.saturating_sub(1) as usize,
            ));
            continue;
        }
        let Some((a, b)) = &mut position else {
            continue;
        };
        let Some(code) = line.get(1..) else { continue };
        match line.as_bytes()[0] {
            b'-' => {
                if old.get(*a).copied()? != code {
                    return None;
                }
                counts.deletions += usize::from(*old_code.get(*a)?);
                *a += 1;
            }
            b'+' => {
                if new.get(*b).copied()? != code {
                    return None;
                }
                counts.additions += usize::from(*new_code.get(*b)?);
                *b += 1;
            }
            b' ' => {
                // Mismatches can indicate textconv, word diffs, an ignored
                // whitespace change, or a worktree modified since the patch.
                if old.get(*a).copied()? != code || new.get(*b).copied()? != code {
                    return None;
                }
                *a += 1;
                *b += 1;
            }
            _ => {}
        }
    }
    Some(counts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::tests::{git, CurrentDirGuard, TestDirectory};

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
