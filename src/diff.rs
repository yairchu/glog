use crate::ansi;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileSection {
    pub start: usize,
    pub end: usize,
    pub path: String,
    pub lockfile: bool,
    pub untracked: bool,
    pub lazy_untracked_path: Option<String>,
    pub additions: usize,
    pub deletions: usize,
}

pub fn file_sections(text: &str) -> Vec<FileSection> {
    let lines: Vec<_> = text.lines().collect();
    let starts: Vec<_> = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            ansi::plain(line)
                .starts_with("diff --git ")
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
            let path = lazy_untracked_path
                .clone()
                .or_else(|| diff_path(&visible))
                .unwrap_or_else(|| "changed file".to_owned());
            let additions = visible
                .iter()
                .filter(|line| line.starts_with('+') && !line.starts_with("+++"))
                .count();
            let deletions = visible
                .iter()
                .filter(|line| line.starts_with('-') && !line.starts_with("---"))
                .count();
            FileSection {
                start: *start,
                end,
                lockfile: is_lockfile(&path),
                untracked: visible
                    .iter()
                    .any(|line| line.starts_with("new file mode ")),
                lazy_untracked_path,
                path,
                additions,
                deletions,
            }
        })
        .collect()
}

fn hex_decode(hex: &str) -> Option<String> {
    if hex.len() % 2 != 0 {
        return None;
    }
    let bytes = (0..hex.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&hex[index..index + 2], 16))
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    String::from_utf8(bytes).ok()
}

fn diff_path(lines: &[String]) -> Option<String> {
    let candidate = lines
        .iter()
        .find_map(|line| line.strip_prefix("+++ "))
        .filter(|path| *path != "/dev/null")
        .or_else(|| {
            lines
                .iter()
                .find_map(|line| line.strip_prefix("--- "))
                .filter(|path| *path != "/dev/null")
        })
        .or_else(|| {
            lines
                .first()
                .and_then(|line| line.strip_prefix("diff --git ")?.split_whitespace().last())
        })?;
    let candidate = candidate.trim_matches('"');
    Some(
        candidate
            .strip_prefix("b/")
            .or_else(|| candidate.strip_prefix("a/"))
            .unwrap_or(candidate)
            .to_owned(),
    )
}

fn is_lockfile(path: &str) -> bool {
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
        assert_eq!(sections[0].lazy_untracked_path.as_deref(), Some("new.txt"));
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
