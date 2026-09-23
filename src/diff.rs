use crate::ansi;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileSection {
    pub start: usize,
    pub end: usize,
    // Display text is separate from the lossless identity used for folds and I/O.
    pub path: String,
    pub path_bytes: Vec<u8>,
    pub lockfile: bool,
    pub submodule: Option<(String, String)>,
    pub untracked: bool,
    pub lazy_untracked_path: Option<Vec<u8>>,
    pub additions: usize,
    pub deletions: usize,
}

pub fn file_sections(text: &str) -> Vec<FileSection> {
    let lines: Vec<_> = text.lines().collect();
    let starts: Vec<_> = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            let line = ansi::plain(line);
            (line.starts_with("diff --git ")
                || line.starts_with("diff --cc ")
                || line.starts_with("diff --combined "))
            .then_some(index)
        })
        .collect();
    starts
        .iter()
        .enumerate()
        .map(|(position, start)| {
            let end = starts.get(position + 1).copied().unwrap_or(lines.len());
            let visible: Vec<_> = lines[*start..end]
                .iter()
                .map(|line| ansi::plain(line))
                .collect();
            let lazy_untracked_path = visible.iter().find_map(|line| {
                line.strip_prefix("glog-lazy-untracked:")
                    .and_then(hex_decode)
            });
            let path_bytes = lazy_untracked_path
                .clone()
                .or_else(|| diff_path(&visible))
                .unwrap_or_else(|| b"changed file".to_vec());
            let path = String::from_utf8(path_bytes.clone()).unwrap_or_else(|_| {
                // Keep invalid bytes readable without using the display name
                // as an identity or a filesystem path.
                path_bytes
                    .iter()
                    .map(|&byte| match byte {
                        b' '..=b'~' if byte != b'\\' => char::from(byte).to_string(),
                        b'\\' => "\\\\".to_owned(),
                        _ => format!("\\{byte:03o}"),
                    })
                    .collect()
            });
            // Only the file header is metadata: hunk content can itself start
            // with `+++` or `---` (for example, an increment or a Markdown rule).
            let body_start = visible
                .iter()
                .position(|line| line.starts_with("+++ "))
                .map_or(visible.len(), |index| index + 1);
            let mut additions = 0;
            let mut deletions = 0;
            let mut parents = 1;
            for line in &visible[body_start..] {
                if line.starts_with("@@") {
                    // A combined hunk has one prefix column per parent and
                    // one more @ in its header than it has parents.
                    parents = line.bytes().take_while(|&byte| byte == b'@').count() - 1;
                    continue;
                }
                let prefix = &line.as_bytes()[..parents.min(line.len())];
                if prefix.len() == parents && prefix.iter().all(|b| matches!(b, b' ' | b'+' | b'-'))
                {
                    // Count each displayed line once, including changes that
                    // are present only in the second (or later) parent column.
                    additions += usize::from(prefix.contains(&b'+'));
                    deletions += usize::from(prefix.contains(&b'-'));
                }
            }
            FileSection {
                start: *start,
                end,
                submodule: submodule_change(&visible),
                lockfile: is_lockfile(&path),
                untracked: visible
                    .iter()
                    .any(|line| line.starts_with("new file mode ")),
                lazy_untracked_path,
                path,
                path_bytes,
                additions,
                deletions,
            }
        })
        .collect()
}

fn submodule_change(lines: &[String]) -> Option<(String, String)> {
    let index = lines.iter().find_map(|line| line.strip_prefix("index "))?;
    let (pair, mode) = index
        .split_once(' ')
        .map_or((index, None), |(pair, mode)| (pair, Some(mode)));
    let (parents, new) = pair.split_once("..")?;
    let mut parents = parents.split(',');
    let old = parents.next()?;
    let mut count = 1;
    for parent in parents {
        if parent != old {
            return None;
        }
        count += 1;
    }
    let valid = |id: &str| matches!(id.len(), 40 | 64) && id.bytes().all(|b| b.is_ascii_hexdigit());
    if !valid(old) || !valid(new) || old == new {
        return None;
    }
    if count == 1 {
        if mode != Some("160000") {
            return None;
        }
    } else {
        // Combined diffs omit the mode when all parents and the result agree.
        // Require the gitlink body as well as identical parent object IDs.
        if !lines.first().is_some_and(|line| {
            line.starts_with("diff --cc ") || line.starts_with("diff --combined ")
        }) || mode.is_some_and(|mode| mode != "160000")
            || !lines.contains(&format!("{}Subproject commit {old}", "-".repeat(count)))
            || !lines.contains(&format!("{}Subproject commit {new}", "+".repeat(count)))
        {
            return None;
        }
    }
    Some((old.to_owned(), new.to_owned()))
}

fn hex_decode(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&hex[index..index + 2], 16))
        .collect::<Result<Vec<_>, _>>()
        .ok()
}

fn diff_path(lines: &[String]) -> Option<Vec<u8>> {
    // Combined headers contain a single repository-relative path, without
    // a/ or b/ prefixes (even when the real path begins with one of them).
    if let Some(path) = lines.first().and_then(|line| {
        line.strip_prefix("diff --cc ")
            .or_else(|| line.strip_prefix("diff --combined "))
    }) {
        return unquote_path(path).map(|(path, _)| path);
    }
    if let Some(path) = lines
        .iter()
        .find_map(|line| line.strip_prefix("rename to "))
    {
        return unquote_path(path).map(|(path, _)| path);
    }
    let candidate = lines
        .iter()
        .find_map(|line| line.strip_prefix("+++ "))
        .filter(|path| *path != "/dev/null")
        .or_else(|| {
            lines
                .iter()
                .find_map(|line| line.strip_prefix("--- "))
                .filter(|path| *path != "/dev/null")
        });
    let candidate = if let Some(path) = candidate {
        path.trim_end_matches('\t')
    } else {
        let header = lines.first()?.strip_prefix("diff --git ")?;
        if header.starts_with('"') {
            let (_, consumed) = unquote_path(header)?;
            header[consumed..].trim_start()
        } else if let Some((_, path)) = header.split_once(" \"b/") {
            // Include the opening quote in the path passed to the decoder.
            &header[header.len() - path.len() - 3..]
        } else {
            // Git does not quote spaces. Prefer the split with identical paths;
            // renames have their own unambiguous "rename to" metadata above.
            let split = header
                .match_indices(" b/")
                .find(|(index, _)| {
                    header[..*index].strip_prefix("a/") == Some(&header[*index + 3..])
                })
                .map(|(index, _)| index)
                .or_else(|| header.rfind(" b/"))?;
            &header[split + 1..]
        }
    };
    let (candidate, _) = unquote_path(candidate)?;
    Some(
        candidate
            .strip_prefix(b"b/")
            .or_else(|| candidate.strip_prefix(b"a/"))
            .unwrap_or(&candidate)
            .to_owned(),
    )
}

fn unquote_path(path: &str) -> Option<(Vec<u8>, usize)> {
    if !path.starts_with('"') {
        return Some((path.as_bytes().to_vec(), path.len()));
    }
    let bytes = path.as_bytes();
    let mut output = Vec::new();
    let mut i = 1;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => return Some((output, i + 1)),
            b'\\' => {
                i += 1;
                let byte = *bytes.get(i)?;
                output.push(match byte {
                    b'n' => b'\n',
                    b't' => b'\t',
                    b'r' => b'\r',
                    b'a' => 7,
                    b'b' => 8,
                    b'v' => 11,
                    b'f' => 12,
                    b'\\' | b'"' => byte,
                    b'0'..=b'3' => {
                        let octal = std::str::from_utf8(bytes.get(i..i + 3)?).ok()?;
                        i += 2;
                        u8::from_str_radix(octal, 8).ok()?
                    }
                    _ => return None,
                });
            }
            byte => output.push(byte),
        }
        i += 1;
    }
    None
}

pub(crate) fn is_lockfile(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.ends_with(".lock")
        || matches!(
            name,
            "package-lock.json" | "pnpm-lock.yaml" | "yarn.lock" | "uv.lock"
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combined_submodules_expand_when_all_parents_agree() {
        let old = "c0fc5ab718a00684a0f94f3d36c0bc2f03e54730";
        let new = "1fa60bfed003c5f4d3eae6343043fee41bfa1bdf";
        for header in ["diff --cc", "diff --combined"] {
            for count in [2, 3] {
                let parents = vec![old; count].join(",");
                let patch = format!("{header} Radical1Presets\nindex {parents}..{new}\n--- a/Radical1Presets\n+++ b/Radical1Presets\n{} {} +1,1 {}\n{}Subproject commit {old}\n{}Subproject commit {new}\n",
                    "@".repeat(count + 1), vec!["-1,1"; count].join(" "), "@".repeat(count + 1), "-".repeat(count), "+".repeat(count));
                let files = file_sections(&patch);
                assert_eq!(files[0].path, "Radical1Presets");
                assert_eq!(files[0].submodule, Some((old.to_owned(), new.to_owned())));
                let different_parent =
                    patch.replacen(&format!("index {old}"), &format!("index {new}"), 1);
                assert!(file_sections(&different_parent)[0].submodule.is_none());
                let regular_file = patch.replace("Subproject commit ", "ordinary content ");
                assert!(file_sections(&regular_file)[0].submodule.is_none());
            }
        }
    }

    #[test]
    fn recognizes_only_two_distinct_full_gitlink_ids() {
        for length in [40, 64] {
            let old = "1".repeat(length);
            let new = "2".repeat(length);
            let patch = format!("diff --git a/module b/module\nindex {old}..{new} 160000\n");
            assert_eq!(file_sections(&patch)[0].submodule, Some((old, new)));
        }
        for index in [
            format!("{}..{} 160000", "1".repeat(40), "1".repeat(40)),
            format!("{}..{} 100644", "1".repeat(40), "2".repeat(40)),
            "1111111..2222222 160000".to_owned(),
        ] {
            let patch = format!("diff --git a/module b/module\nindex {index}\n");
            assert!(file_sections(&patch)[0].submodule.is_none());
        }
    }

    #[test]
    fn combined_sections_count_each_changed_line_once_across_parents() {
        for header in ["diff --cc", "diff --combined"] {
            let patch = format!(
                "{header} b/file name.txt\nindex 1111,2222,3333..4444\n--- a/b/file name.txt\n+++ b/b/file name.txt\n@@@@ -1,2 -1,2 -1,2 +1,2 @@@@\n---old in all parents\n  -old in third parent\n+++new in all parents\n  +new in third parent\n   context\n"
            );
            let files = file_sections(&patch);
            assert_eq!(files.len(), 1);
            assert_eq!(files[0].path, "b/file name.txt");
            assert_eq!((files[0].additions, files[0].deletions), (2, 2));
        }
        let files = file_sections(r#"diff --cc "b/quoted\tname.txt""#);
        assert_eq!(files[0].path, "b/quoted\tname.txt");
    }

    #[test]
    fn counts_header_like_content_inside_hunks() {
        let patch = "diff --git a/file b/file\n--- a/file\n+++ b/file\n@@ -1,3 +1,3 @@\n---counter;\n---- heading\n-old\n+++counter;\n++++ heading\n+new\n";
        let files = file_sections(patch);
        assert_eq!(files.len(), 1);
        assert_eq!((files[0].additions, files[0].deletions), (3, 3));
    }

    #[test]
    fn quoted_non_utf8_paths_survive_all_diff_headers() {
        for (patch, expected) in [
            (
                r#"diff --git "a/deps-\377.lock" "b/deps-\377.lock""#,
                &b"deps-\xff.lock"[..],
            ),
            (r#"diff --cc "deps-\377.lock""#, &b"deps-\xff.lock"[..]),
            (
                r#"diff --combined "deps-\376.lock""#,
                &b"deps-\xfe.lock"[..],
            ),
            (
                "diff --git a/old b/new\nrename to \"deps-\\377.lock\"",
                &b"deps-\xff.lock"[..],
            ),
        ] {
            let files = file_sections(patch);
            assert_eq!(files.len(), 1);
            assert_eq!(files[0].path_bytes, expected);
            assert!(files[0].lockfile);
            assert!(files[0].path.starts_with("deps-\\3"));
        }
    }

    #[test]
    fn binary_paths_keep_spaces_quotes_and_git_octal_utf8() {
        for (header, expected) in [
            (
                "diff --git a/picture space.png b/picture space.png",
                "picture space.png",
            ),
            (
                r#"diff --git "a/caf\303\251.png" "b/caf\303\251.png""#,
                "café.png",
            ),
            (r#"diff --git "a/a\"b.png" "b/a\"b.png""#, "a\"b.png"),
            (
                "diff --git a/dir b/image.png b/dir b/image.png",
                "dir b/image.png",
            ),
        ] {
            let sections = file_sections(&format!("{header}\nBinary files differ\n"));
            assert_eq!(sections[0].path, expected);
        }
        let sections = file_sections("diff --git a/old name.png b/new name.png\nrename from old name.png\nrename to new name.png\n");
        assert_eq!(sections[0].path, "new name.png");
    }

    #[test]
    fn finds_and_classifies_diff_sections() {
        let text = "commit header\n\x1b[1mdiff --git a/src/main.rs b/src/main.rs\x1b[0m\n--- a/src/main.rs\n+++ b/src/main.rs\n-old\n+new\ndiff --git a/Cargo.lock b/Cargo.lock\n--- a/Cargo.lock\n+++ b/Cargo.lock\n-a\n+b\n+c\n";
        let sections = file_sections(text);
        assert_eq!(sections.len(), 2);
        assert_eq!(sections[0].path, "src/main.rs");
        assert!(!sections[0].lockfile);
        assert!(!sections[0].untracked);
        assert_eq!(sections[1].path, "Cargo.lock");
        assert!(sections[1].lockfile);
        assert_eq!((sections[1].additions, sections[1].deletions), (2, 1));
    }

    #[test]
    fn recognizes_an_untracked_file() {
        let text = "diff --git a/new.txt b/new.txt\nnew file mode 100644\n--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1 @@\n+new\n";
        let sections = file_sections(text);
        assert!(sections[0].untracked);
    }

    #[test]
    fn decodes_a_lazy_untracked_path() {
        let text = "diff --git a/new.txt b/new.txt\nnew file mode 100644\nglog-lazy-untracked:6e65772e747874\n";
        let sections = file_sections(text);
        assert_eq!(
            sections[0].lazy_untracked_path.as_deref(),
            Some(&b"new.txt"[..])
        );
    }

    #[test]
    fn decodes_a_non_utf8_lazy_untracked_path() {
        let text = "diff --git \"a/a\\377.txt\" \"b/a\\377.txt\"\nnew file mode 100644\nglog-lazy-untracked:61ff2e747874\n";
        let sections = file_sections(text);
        assert!(sections[0].lazy_untracked_path.is_some());
        assert_eq!(sections[0].path_bytes, b"a\xff.txt");
    }

    #[test]
    fn recognizes_common_non_dot_lockfiles() {
        assert!(is_lockfile("web/package-lock.json"));
        assert!(is_lockfile("pnpm-lock.yaml"));
        assert!(!is_lockfile("src/lock.rs"));
    }

    #[test]
    fn recognizes_a_binary_lockfile_from_the_diff_header() {
        let sections = file_sections(
            "diff --git a/cache.lock b/cache.lock\nBinary files a/cache.lock and b/cache.lock differ\n",
        );
        assert_eq!(sections[0].path, "cache.lock");
        assert!(sections[0].lockfile);
    }
}
