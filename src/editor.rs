use std::{path::PathBuf, process::Command};

use crate::{ansi, git};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Textconv {
    Allowed,
    Disabled,
}

pub struct Target {
    root: PathBuf,
    path: PathBuf,
    line: Option<u32>,
    /// The result-side blob, when the displayed file is not the working tree.
    blob: Option<String>,
    textconv: Textconv,
}

impl Target {
    pub fn from_patch(
        root: PathBuf,
        path: PathBuf,
        patch: &str,
        row: usize,
        worktree: bool,
        textconv: Textconv,
    ) -> Self {
        let mut blob = None;
        let mut line = None;
        let mut parents = 1;
        for (index, text) in patch.lines().enumerate() {
            let text = ansi::plain(text);
            if line.is_none() && !worktree {
                if let Some(ids) = text.strip_prefix("index ") {
                    blob = ids
                        .split_whitespace()
                        .next()
                        .and_then(|ids| ids.split_once(".."))
                        .map(|(_, result)| result)
                        .filter(|id| {
                            (4..=64).contains(&id.len())
                                && id.bytes().all(|b| b.is_ascii_hexdigit())
                                && id.bytes().any(|b| b != b'0')
                        })
                        .map(str::to_owned);
                }
            }
            if let Some((_, (start, _))) = hunk(&text) {
                parents = text.bytes().take_while(|&b| b == b'@').count() - 1;
                line = Some(start.max(1));
            }
            if index == row {
                break;
            }
            if line.is_some() {
                let prefix = text.as_bytes().get(..parents).unwrap_or_default();
                // Combined deletions have a '-' in at least one parent column;
                // they are absent from the result. Context and additions survive.
                if text.is_empty()
                    || (prefix.len() == parents && prefix.iter().all(|b| matches!(b, b' ' | b'+')))
                {
                    line = line.map(|n| n.saturating_add(1));
                }
            }
        }
        Self {
            root,
            path,
            line,
            blob,
            textconv,
        }
    }

    fn working_line(&self) -> Result<Option<u32>, String> {
        let path = self.root.join(&self.path);
        if !std::fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_file()) {
            return Err(format!(
                "{} is not a regular working-tree file",
                path.display()
            ));
        }
        // Textconv may change the number and meaning of lines. Its displayed
        // positions cannot be translated through a diff of the raw blobs.
        if self.line.is_some() && self.textconv == Textconv::Allowed && self.uses_textconv()? {
            return Ok(None);
        }
        if let (Some(line), Some(blob)) = (self.line, &self.blob) {
            let output = self
                .git()
                .args([
                    "diff",
                    "--no-ext-diff",
                    "--no-textconv",
                    "--no-color",
                    "--no-relative",
                    "--unified=0",
                    "--inter-hunk-context=0",
                    "--no-renames",
                    "--no-exit-code",
                    "--no-quiet",
                ])
                .arg(blob)
                .arg("--")
                .arg(&self.path)
                .env("GIT_NO_LAZY_FETCH", "1")
                .output()
                .map_err(|error| error.to_string())?;
            if !output.status.success() {
                return Err(format!(
                    "Could not locate the working-tree line: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ));
            }
            return Ok(Some(map_line(
                line,
                &String::from_utf8_lossy(&output.stdout),
            )));
        }
        Ok(self.line)
    }

    fn git(&self) -> Command {
        let mut command = git::patch_command_at(Some(&self.root));
        command.arg("--literal-pathspecs").arg("-C").arg(&self.root);
        command
    }

    fn uses_textconv(&self) -> Result<bool, String> {
        let output = self
            .git()
            .args(["check-attr", "-z", "diff", "--"])
            .arg(&self.path)
            .output()
            .map_err(|error| error.to_string())?;
        if !output.status.success() {
            return Err(git::stderr_message(
                "Could not read Git attributes",
                &output.stderr,
            ));
        }
        let driver = output
            .stdout
            .split(|&b| b == 0)
            .nth(2)
            .ok_or("Missing Git diff attribute")?;
        if matches!(driver, b"set" | b"unset" | b"unspecified") {
            return Ok(false);
        }
        let mut key = std::ffi::OsString::from("diff.");
        key.push(git::raw_path(driver));
        key.push(".textconv");
        let output = self
            .git()
            .args(["config", "--get"])
            .arg(key)
            .output()
            .map_err(|error| error.to_string())?;
        match output.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(git::stderr_message(
                "Could not read textconv configuration",
                &output.stderr,
            )),
        }
    }

    pub fn command(self) -> Result<Command, String> {
        let line = self.working_line()?;
        let editor = editor_command::Editor::new()
            .map_err(|error| format!("Could not load editor; set VISUAL or EDITOR: {error}"))?;
        Ok(command_for(&editor, &self.root.join(self.path), line))
    }
}

fn command_for(
    editor: &editor_command::Editor,
    path: &std::path::Path,
    line: Option<u32>,
) -> Command {
    #[cfg(target_os = "macos")]
    if let Some(line) = line {
        let command = editor.open(path);
        if std::path::Path::new(command.get_program()).file_name() == Some("xed".as_ref()) {
            let args: Vec<_> = command.get_args().collect();
            let mut positioned = Command::new(command.get_program());
            positioned.args(&args[..args.len() - 1]);
            positioned.args(["--line", &line.to_string()]).arg(path);
            return positioned;
        }
    }
    let mut command = match line {
        Some(line) => editor.open_at(path, line, 1),
        None => editor.open(path),
    };
    // The crate's generic path:line:column formatter uses Path::display(),
    // which loses non-UTF-8 bytes. Preserve the original path in that argument.
    if let Some(line) = line.filter(|_| path.to_str().is_none()) {
        let args: Vec<_> = command.get_args().map(std::ffi::OsStr::to_owned).collect();
        if args
            .last()
            .is_some_and(|arg| arg == format!("{}:{line}:1", path.display()).as_str())
        {
            let mut exact = path.as_os_str().to_owned();
            exact.push(format!(":{line}:1"));
            let mut corrected = Command::new(command.get_program());
            corrected.args(&args[..args.len() - 1]).arg(exact);
            command = corrected;
        }
    }
    let program = std::path::Path::new(command.get_program())
        .file_stem()
        .and_then(|name| name.to_str());
    if line.is_some()
        && matches!(program, Some("code" | "code-insiders" | "codium"))
        && !command.get_args().any(|arg| arg == "-g" || arg == "--goto")
    {
        command.arg("--goto");
    }
    command
}

type Range = (u32, u32);

fn hunk(line: &str) -> Option<(Range, Range)> {
    if !line.starts_with("@@") {
        return None;
    }
    let mut fields = line.split_whitespace();
    fields.next()?;
    let old = range(fields.next()?.strip_prefix('-')?)?;
    let new = fields.find_map(|field| field.strip_prefix('+').and_then(range))?;
    Some((old, new))
}

fn range(text: &str) -> Option<Range> {
    let (start, count) = text.split_once(',').unwrap_or((text, "1"));
    Some((start.parse().ok()?, count.parse().ok()?))
}

/// Map unchanged lines exactly; replaced/deleted lines land on the nearest
/// replacement or surviving line. Zero-length old ranges insert *after* start.
fn map_line(line: u32, patch: &str) -> u32 {
    let mut offset = 0_i64;
    for ((old, count), (new, added)) in patch.lines().filter_map(hunk) {
        // Empty ranges name the preceding line, not their first line.
        let old = old.saturating_add(u32::from(count == 0));
        let new = new.saturating_add(u32::from(added == 0));
        if line < old {
            break;
        }
        if line < old.saturating_add(count) {
            return new
                .saturating_add((line - old).min(added.saturating_sub(1)))
                .max(1);
        }
        offset = i64::from(new) + i64::from(added) - i64::from(old) - i64::from(count);
    }
    (i64::from(line) + offset).clamp(1, i64::from(u32::MAX)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn editor(command: &str) -> editor_command::Editor {
        editor_command::EditorBuilder::new()
            .string(Some(command))
            .build()
            .unwrap()
    }

    #[test]
    fn vscode_goto_preserves_user_arguments_and_paths() {
        for program in [
            "code",
            "code-insiders",
            "codium",
            "/Applications/VS Code/bin/code",
        ] {
            let config = format!("'{program}' --wait --reuse-window");
            let command = command_for(&editor(&config), Path::new("/repo/file space.rs"), Some(42));
            assert_eq!(command.get_program(), program);
            assert_eq!(
                command.get_args().collect::<Vec<_>>(),
                [
                    "--wait",
                    "--reuse-window",
                    "/repo/file space.rs:42:1",
                    "--goto"
                ]
            );
        }
        for flag in ["-g", "--goto"] {
            let command = command_for(
                &editor(&format!("code {flag}")),
                Path::new("/file"),
                Some(3),
            );
            assert_eq!(command.get_args().collect::<Vec<_>>(), [flag, "/file:3:1"]);
        }
        let command = command_for(&editor("code"), Path::new("/file"), None);
        assert_eq!(command.get_args().collect::<Vec<_>>(), ["/file"]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn xcode_uses_a_separate_line_option() {
        let command = command_for(
            &editor("xed --wait"),
            Path::new("/repo/file space"),
            Some(42),
        );
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            ["--wait", "--line", "42", "/repo/file space"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn editor_line_arguments_preserve_non_utf8_paths() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
        let path = Path::new(OsStr::from_bytes(b"/repo/file-\xff.rs"));
        let command = command_for(&editor("code"), path, Some(2));
        assert_eq!(
            command.get_args().next().unwrap().as_bytes(),
            b"/repo/file-\xff.rs:2:1"
        );
        let command = command_for(&editor("vim"), path, Some(2));
        assert!(command.get_args().any(|arg| arg == path));
    }

    #[test]
    fn patch_positions_count_result_lines_and_skip_deletions() {
        let patch = "diff --git a/f b/f\nindex aaaa..bbbb 100644\n--- a/f\n+++ b/f\n@@ -10,4 +10,5 @@\n context\n-old\n+new\n+extra\n\n tail\n\\ No newline at end of file\n@@ -30 +31 @@\n-last\n+end\n";
        for (row, expected) in [
            (0, None),
            (4, Some(10)),
            (5, Some(10)),
            (6, Some(11)),
            (7, Some(11)),
            (8, Some(12)),
            (9, Some(13)),
            (10, Some(14)),
            (13, Some(31)),
            (14, Some(31)),
        ] {
            let target = Target::from_patch(
                PathBuf::new(),
                "f".into(),
                patch,
                row,
                false,
                Textconv::Allowed,
            );
            assert_eq!(target.line, expected, "row {row}");
        }
        let combined = "diff --cc f\nindex aaaa,bbbb..cccc\n@@@ -3,2 -3,2 +3,3 @@@\n  context\n- old\n +new\n++extra\n";
        for (row, line) in [(2, 3), (3, 3), (4, 4), (5, 4), (6, 5)] {
            let target = Target::from_patch(
                PathBuf::new(),
                "f".into(),
                combined,
                row,
                false,
                Textconv::Allowed,
            );
            assert_eq!(target.line, Some(line), "combined row {row}");
            assert_eq!(target.blob.as_deref(), Some("cccc"));
        }
    }

    #[test]
    fn line_translation_handles_insertions_deletions_and_replacements() {
        let patch = "@@ -0,0 +1,2 @@\n+a\n+b\n@@ -4,2 +5,0 @@\n-old\n-gone\n@@ -9,2 +9,3 @@\n-old\n-old\n+new\n+new\n+extra\n";
        for (line, expected) in [
            (1, 3),
            (3, 5),
            (4, 6),
            (5, 6),
            (6, 6),
            (8, 8),
            (9, 9),
            (10, 10),
            (11, 12),
        ] {
            assert_eq!(map_line(line, patch), expected, "line {line}");
        }
        assert_eq!(map_line(1, "@@ -1,2 +0,0 @@\n-a\n-b\n"), 1);
    }

    #[test]
    fn textconv_patch_positions_do_not_become_working_file_lines() {
        use crate::git::tests::{git, CurrentDirGuard, TestDirectory};
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        std::fs::create_dir("nested").unwrap();
        std::fs::write(".gitattributes", "nested/* diff=readable\n").unwrap();
        git(&["config", "diff.readable.textconv", "sed '1d'"]);
        std::fs::write("nested/file space", "header\none\nold\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "initial"]);
        std::fs::write("nested/file space", "header\none\nnew\n").unwrap();
        git(&["add", "."]);
        std::env::set_current_dir("nested").unwrap();

        for (args, worktree) in [
            (
                vec!["diff", "--cached", "--textconv", "--full-index"],
                false,
            ),
            (
                vec!["show", "--format=", "--textconv", "--full-index"],
                false,
            ),
            (vec!["diff", "--textconv", "--full-index"], true),
        ] {
            if args[0] == "show" {
                git(&["commit", "-qm", "change"]);
            } else if worktree {
                std::fs::write("file space", "header\none\nnewer\n").unwrap();
            }
            let patch = git(&args);
            let row = patch
                .lines()
                .position(|line| line.starts_with("+new"))
                .unwrap();
            let target = Target::from_patch(
                directory.path().into(),
                "nested/file space".into(),
                &patch,
                row,
                worktree,
                Textconv::Allowed,
            );
            assert_eq!(target.line, Some(2), "the displayed line is transformed");
            assert_eq!(target.working_line().unwrap(), None, "{args:?}");

            // Status and expanded submodules explicitly disable textconv,
            // so the same attributes must not suppress their raw positions.
            let mut raw_args = args.clone();
            raw_args.push("--no-textconv");
            let raw = git(&raw_args);
            let row = raw
                .lines()
                .position(|line| line.starts_with("+new"))
                .unwrap();
            let target = Target::from_patch(
                directory.path().into(),
                "nested/file space".into(),
                &raw,
                row,
                worktree,
                Textconv::Disabled,
            );
            assert_eq!(target.working_line().unwrap(), Some(3), "{raw_args:?}");
        }
    }

    #[test]
    fn staged_and_historical_lines_follow_worktree_edits() {
        use crate::git::tests::{git, CurrentDirGuard, TestDirectory};
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        std::fs::write("file space", "one\ntwo\nthree\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "initial"]);
        std::fs::write("file space", "one\nnew\nthree\n").unwrap();
        git(&["add", "."]);
        let patch = git(&["diff", "--cached", "--full-index", "--color=always"]);
        let row = patch
            .lines()
            .position(|line| ansi::plain(line) == "+new")
            .unwrap();
        std::fs::write("file space", "intro\none\nnew\nthree\n").unwrap();
        git(&["config", "diff.relative", "true"]);
        git(&["config", "diff.interHunkContext", "20"]);
        let target = Target::from_patch(
            directory.path().into(),
            "file space".into(),
            &patch,
            row,
            false,
            Textconv::Allowed,
        );
        assert_eq!(target.working_line().unwrap(), Some(3));
        // The same patch remains meaningful after it becomes a commit.
        git(&["commit", "-qm", "change"]);
        assert_eq!(target.working_line().unwrap(), Some(3));
        std::fs::remove_file("file space").unwrap();
        assert!(target
            .working_line()
            .unwrap_err()
            .contains("not a regular working-tree file"));
    }

    #[test]
    fn expanded_untracked_files_keep_raw_editor_line_positions() {
        use crate::git::tests::{git, CurrentDirGuard, TestDirectory};
        let directory = TestDirectory::new();
        let _cwd = CurrentDirGuard::enter(directory.path());
        git(&["init", "-q"]);
        std::fs::write(".gitattributes", "*.txt diff=readable\n").unwrap();
        git(&["config", "diff.readable.textconv", "sed '1d'"]);
        git(&["add", "."]);
        git(&["commit", "-qm", "initial"]);
        std::fs::write("new.txt", "header\none\ntwo\n").unwrap();
        let mut app = crate::git::load_diff_app(&[]).unwrap();
        app.show_cursor = app.show_rows.iter().position(|row| row.folded).unwrap();
        app.toggle_show_file();
        app.show_cursor = app
            .show_rows
            .iter()
            .position(|row| ansi::plain(&row.text) == "+two")
            .unwrap();

        let previous_visual = std::env::var_os("VISUAL");
        std::env::set_var("VISUAL", "code");
        app.request_editor();
        match previous_visual {
            Some(value) => std::env::set_var("VISUAL", value),
            None => std::env::remove_var("VISUAL"),
        }
        let command = app.pending_editor.expect("editor command");
        // Git's repository root can use a different path spelling than
        // canonicalize() on Windows (C:/... versus \\?\C:\...).
        let mut positioned = crate::git::repository_root()
            .unwrap()
            .join("new.txt")
            .into_os_string();
        positioned.push(":3:1");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [positioned.as_os_str(), "--goto".as_ref()],
            "the untracked patch shows raw lines despite its textconv attribute"
        );
    }
}
