//! Follow code removed elsewhere by the same commit, without guessing between
//! equally good origins. Copies that remain in the parent keep normal mapping.

use super::{git, BlameLine, Commit};

pub(super) fn origin(
    commit: &Commit,
    line: &BlameLine,
    parent: &str,
) -> Result<Option<(Vec<u8>, usize)>, String> {
    // Moved code comes from removed lines, which files the commit added lack.
    // Skipping them keeps large imports cheap; the line's own file is still
    // needed when the commit added it, and excluded by the first diff.
    let mut changes = diff(commit, line, parent, "--diff-filter=a", None)?;
    changes.push_str(&diff(
        commit,
        line,
        parent,
        "--diff-filter=A",
        Some(&line.source_path),
    )?);
    Ok(find_origin(&changes, &line.source_path, line.source_number))
}

fn diff(
    commit: &Commit,
    line: &BlameLine,
    parent: &str,
    filter: &str,
    path: Option<&[u8]>,
) -> Result<String, String> {
    let output = git::read_command_at(Some(&line.file.repository))
        .env("LC_ALL", "C")
        .args([
            "-c",
            "core.quotepath=true",
            "--literal-pathspecs",
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-renames",
            "--no-relative",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            "--inter-hunk-context=0",
            "-U0",
            filter,
            if line.file.textconv {
                "--textconv"
            } else {
                "--no-textconv"
            },
        ])
        .args([parent, &commit.hash, "--"])
        .args(path.map(git::raw_path))
        .output()
        .map_err(|error| format!("could not find moved code: {error}"))?;
    if !output.status.success() {
        return Err(git::stderr_message(
            "could not find moved code",
            &output.stderr,
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

struct Block<'a> {
    path: Vec<u8>,
    start: usize,
    added: bool,
    lines: Vec<&'a str>,
}

fn blocks(diff: &str) -> Vec<Block<'_>> {
    let lines: Vec<_> = diff.lines().collect();
    let mut blocks: Vec<Block<'_>> = Vec::new();
    for section in crate::diff::file_sections(diff) {
        let mut position = None;
        let mut continuing = false;
        for line in &lines[section.start..section.end] {
            if let Some(header) = crate::hunk::header(line) {
                if let [(old, _)] = header.old[..] {
                    position = Some((old as usize, header.new.0 as usize));
                }
                continuing = false;
                continue;
            }
            let Some((old, new)) = position.as_mut() else {
                continue;
            };
            let added = line.starts_with('+');
            if let Some(code) = line.strip_prefix(['+', '-']) {
                let number = if added { &mut *new } else { &mut *old };
                if !continuing || blocks.last().is_none_or(|block| block.added != added) {
                    blocks.push(Block {
                        path: section.path_bytes.clone(),
                        start: *number,
                        added,
                        lines: Vec::new(),
                    });
                }
                blocks.last_mut().unwrap().lines.push(code.trim());
                *number += 1;
                continuing = true;
            } else if !line.starts_with('\\') {
                *old += 1;
                *new += 1;
                continuing = false;
            }
        }
    }
    blocks
}

fn find_origin(diff: &str, path: &[u8], number: usize) -> Option<(Vec<u8>, usize)> {
    let blocks = blocks(diff);
    let target = blocks.iter().find(|block| {
        block.added
            && block.path == path
            && (block.start..block.start + block.lines.len()).contains(&number)
    })?;
    let selected = number - target.start;
    let mut best = None;
    let mut best_score = 0;
    let mut ambiguous = false;
    for source in blocks.iter().filter(|block| !block.added) {
        for (index, code) in source.lines.iter().enumerate() {
            if *code != target.lines[selected] {
                continue;
            }
            let mut before = 0;
            while before < index
                && before < selected
                && source.lines[index - before - 1] == target.lines[selected - before - 1]
            {
                before += 1;
            }
            let mut after = 1;
            while index + after < source.lines.len()
                && selected + after < target.lines.len()
                && source.lines[index + after] == target.lines[selected + after]
            {
                after += 1;
            }
            let matched = &source.lines[index - before..index + after];
            // Require enough code to avoid following tiny, incidental matches.
            let score = matched
                .iter()
                .flat_map(|line| line.chars())
                .filter(|c| c.is_alphanumeric())
                .count();
            if score < 20 {
                continue;
            }
            if score > best_score {
                best = Some((source.path.clone(), source.start + index));
                best_score = score;
                ambiguous = false;
            } else if score == best_score {
                ambiguous = true;
            }
        }
    }
    best.filter(|_| !ambiguous)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surrounding_lines_resolve_repeated_code_even_after_reindentation() {
        let diff = "diff --git a/old b/old\n--- a/old\n+++ b/old\n\
@@ -1,3 +0,0 @@\n-first_context();\n-return result;\n-}\n\
@@ -10,3 +6,0 @@\n-second_context();\n-return result;\n-}\n\
diff --git a/new b/new\n--- a/new\n+++ b/new\n\
@@ -2,0 +3,3 @@\n+    second_context();\n+    return result;\n+    }\n";
        assert_eq!(find_origin(diff, b"new", 4), Some((b"old".to_vec(), 11)));
        assert_eq!(find_origin(diff, b"new", 5), Some((b"old".to_vec(), 12)));
    }

    #[test]
    fn identical_origins_and_isolated_punctuation_keep_the_fallback() {
        let diff = "diff --git a/old b/old\n--- a/old\n+++ b/old\n\
@@ -1,2 +0,0 @@\n-duplicate_code_with_long_name();\n-}\n\
@@ -10,2 +7,0 @@\n-duplicate_code_with_long_name();\n-}\n\
diff --git a/new b/new\n--- a/new\n+++ b/new\n\
@@ -2,0 +3,2 @@\n+duplicate_code_with_long_name();\n+}\n";
        assert_eq!(find_origin(diff, b"new", 3), None);
        assert_eq!(find_origin(diff, b"new", 4), None);
        let punctuation = "diff --git a/file b/file\n--- a/file\n+++ b/file\n\
@@ -1 +0,0 @@\n-}\n@@ -8,0 +8 @@\n+}\n";
        assert_eq!(find_origin(punctuation, b"file", 8), None);
    }
}
