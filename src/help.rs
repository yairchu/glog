//! Command-line help, separate from Git argument parsing and interactive key help.

use crate::{parse_command, Command};

pub fn information(args: &[String]) -> Option<String> {
    let (command, command_args) = parse_command(args);
    match command_args {
        [flag] if flag == "--version" || flag == "-V" => {
            Some(concat!("glog ", env!("CARGO_PKG_VERSION")).into())
        }
        [flag] if is_help(flag) => {
            if args.len() == command_args.len() {
                Some(OVERVIEW.into())
            } else {
                Some(page(command, None).render())
            }
        }
        [topic, flag]
            if is_help(flag)
                && matches!(
                    (command, topic.as_str()),
                    (Command::Stash, "list" | "show") | (Command::Bisect, "log")
                ) =>
        {
            Some(page(command, Some(topic)).render())
        }
        _ => None,
    }
}

fn is_help(arg: &str) -> bool {
    matches!(arg, "--help" | "-h")
}

const OVERVIEW: &str = "glog — an interactive git log and git show browser

Usage: glog [git log arguments] [--] [pathspec...]
       glog <command> [arguments]

Commands:
  log             Browse commit history (the default)
  show            View a commit or a file at a revision
  diff            View working-tree, staged, or commit differences
  branch          Browse local branches and shared ancestors
  status          Watch working-tree changes
  reflog          Browse local ref updates
  blame           Browse line attribution for a file
  stash           Browse saved work
  bisect log      Browse the current bisect session
  range-diff      Compare two versions of a commit series
  grep            Search repository text
  gh search code  Search code on GitHub

Common options:
  --watch         Refresh automatically (W toggles)
  --date=STYLE    Format dates using Git (e.g. short, relative, iso)
  -h, --help      Print help
  -V, --version   Print version

Run glog <command> --help for command options and details.
Inside the TUI, press h for key help, Tab to switch views, and q to quit.";

struct Page {
    usage: &'static str,
    options: &'static [&'static str],
    sections: &'static [(&'static str, &'static str)],
}

impl Page {
    fn render(&self) -> String {
        let mut text = format!("Usage: {}\n", self.usage);
        if !self.options.is_empty() {
            text.push_str("\nOptions:\n");
            for option in self.options {
                indent(&mut text, option);
            }
        }
        for (title, body) in self
            .sections
            .iter()
            .copied()
            .chain([("Navigation", NAVIGATION), ("Environment", ENVIRONMENT)])
        {
            text.push_str(&format!("\n{title}:\n"));
            indent(&mut text, body);
        }
        text.push_str("\nRun glog --help for the command overview.");
        text
    }
}

fn indent(text: &mut String, body: &str) {
    for line in body.lines() {
        text.push_str("  ");
        text.push_str(line);
        text.push('\n');
    }
}

fn page(command: Command, topic: Option<&str>) -> Page {
    use Command::*;
    match (command, topic) {
        (Log, _) => Page {
            usage: "glog [log] [options] [git log arguments] [--] [pathspec...]",
            options: &[LOG_OPTIONS, DATE, WATCH, PATCH_OPTIONS],
            sections: &[("History", LOG), ("Graph", GRAPH)],
        },
        (Branch, _) => Page {
            usage: "glog branch [--watch] [--date=STYLE]",
            options: &[WATCH, DATE],
            sections: &[("Branches", BRANCH), ("Comparing series", BRANCH_RANGE_DIFF)],
        },
        (Bisect, _) => Page {
            usage: "glog bisect log [--watch] [--date=STYLE]",
            options: &[WATCH, DATE],
            sections: &[("Bisect graph", BISECT)],
        },
        (Reflog, _) => Page {
            usage: "glog reflog [--watch] [ref] [--all] [-n COUNT] [--date=STYLE]",
            options: &[WATCH, DATE, "--all         Show updates across all refs\n-n COUNT      Limit the number of entries"],
            sections: &[("Ref updates", REFLOG)],
        },
        (Blame, _) => Page {
            usage: "glog blame [-L LINE] [--date=STYLE] [revision] [--] file",
            options: &["-L LINE       Open Blame at a line", DATE],
            sections: &[("Line attribution", BLAME)],
        },
        (Stash, Some("show")) => Page {
            usage: "glog stash show [--stat] [stash]",
            options: &["--stat        Start with expandable file summaries", PATCH_OPTIONS],
            sections: &[("Saved changes", STASH_SHOW), ("Moved code", MOVED_CODE), ("Code statistics", STATS)],
        },
        (Stash, Some("list")) => Page {
            usage: "glog stash list [--watch] [-n COUNT] [--date=STYLE]",
            options: &[WATCH, DATE, "-n COUNT      Limit the number of stashes"],
            sections: &[("Saved work", STASH_LIST)],
        },
        (Stash, _) => Page {
            usage: "glog stash [list [--watch] [-n COUNT] [--date=STYLE]]\n       glog stash show [--stat] [stash]",
            options: &[WATCH, DATE, "-n COUNT      Limit the number of stashes (stash list)"],
            sections: &[("Saved work", STASH_LIST), ("Saved changes", STASH_SHOW)],
        },
        (Show, _) => Page {
            usage: "glog show [--stat] [commit] [-- pathspec...]\n       glog show REV:path",
            options: &["--stat        Start with expandable file summaries", PATCH_OPTIONS],
            sections: &[("Commits", SHOW), ("Links", LINKS), ("Files", FILE), ("Summaries", SUMMARY), ("Moved code", MOVED_CODE), ("Code statistics", STATS)],
        },
        (Diff, _) => Page {
            usage: "glog diff [--cached] [--stat] [revision [revision]] [[--] pathspec...]",
            options: &["--cached      Show staged changes\n--stat        Start with expandable file summaries", PATCH_OPTIONS],
            sections: &[("Differences", DIFF), ("Summaries", SUMMARY), ("Moved code", MOVED_CODE), ("Code statistics", STATS)],
        },
        (RangeDiff, _) => Page {
            usage: "glog range-diff [git range-diff arguments]",
            options: &[],
            sections: &[("Comparing series", RANGE_DIFF), ("Branch comparison", BRANCH_RANGE_DIFF)],
        },
        (Grep, _) => Page {
            usage: "glog grep [matching options] [-e] pattern [revision...] [-- pathspec...]",
            options: &["--cached      Search the index\n--untracked   Include untracked files\n-i, -w, -F, -E, -e, -f\n              Matching options passed to git grep"],
            sections: &[("Text search", GREP)],
        },
        (GithubSearch, _) => Page {
            usage: "glog gh search code [query] [gh search code options]",
            options: &["--repo REPO, --owner OWNER, --language LANGUAGE, --limit COUNT\n              Search options passed to gh search code"],
            sections: &[("GitHub code search", GITHUB)],
        },
        (Status, _) => Page {
            usage: "glog status",
            options: &[PATCH_OPTIONS],
            sections: &[("Working tree", "Status opens the Working tree detail view in a watch session."), ("Bisect graph", "When bisecting, click the bisect indicator or press B to open its graph.
Return with Esc or Backspace. See glog bisect log --help for graph details."), ("Moved code", MOVED_CODE), ("Code statistics", STATS)],
        },
    }
}

const WATCH: &str = "--watch       Refresh automatically; W toggles watching";
const DATE: &str = "--date=STYLE  Format dates using Git (e.g. short, relative, iso)";
const LOG_OPTIONS: &str = "--pretty=format:FORMAT / --format=FORMAT
              Format Log rows (%h %H %ad %an %ae %d %D %s %%)
--oneline     Use the compact hash, refs, and subject layout
--ascii-graph Use Git-style ASCII paths instead of Unicode lines
--fold-merges Start Log with merge histories collapsed; z expands a merge
--hide-merges Hide merge commits; M toggles this filter
--hide-types=TYPES
              Hide comma-separated commit types (e.g. test,refactor,docs);
              T clears filters
--exit-on-esc Exit from the Log list with Escape";
const PATCH_OPTIONS: &str = "--color-moved[=MODE]
              Highlight moved code for this session (bare flag: zebra).
              Modes: no, default, plain, blocks, zebra, dimmed-zebra
--no-color-moved
              Disable moved-code highlighting for this session
--color-moved-ws=MODES
              Set moved-code whitespace matching (comma-separated):
              no, ignore-space-at-eol, ignore-space-change, ignore-all-space,
              or allow-indentation-change (cannot combine with ignore modes)
--no-color-moved-ws
              Match moved-code whitespace exactly for this session";

const BISECT: &str =
    "When bisecting, click the bisect indicator in Status or press B to open its graph.
Return from the graph to Status with Esc or Backspace.
The header shows Git's estimated steps after the current test. HEAD is cyan;
recorded good/bad results keep their own colors. HEAD and culprit have separate
columns: ! marks the culprit, ? a possible culprit. Commit columns align like Log;
refs retain branches, remotes, and tags without repeating the HEAD decoration.
Bisect log shows recorded results and HEAD in a compact ancestry graph. Step
numbers preserve decision order; shared ancestors connect divergent histories.
Range rows between landmarks exclude endpoints and count each commit once.
Select a commit or range with Up/Down; Enter opens the commit or expands the
range. Click a range to focus, then click again to expand; Enter/z toggles it.
z within a gap collapses it. Left/Right also select a source commit's gaps, and
Shift-Left/Right move between commits.
Enter opens the selected commit; Esc or Backspace returns. --watch or W refreshes
as the session changes. This view does not change bisect results or checkout.";

const BRANCH: &str =
    "Branch lists local branches and shared ancestors in a compact graph, including
origin's stored default tip (origin/HEAD, or origin/main / origin/master).
Rows use Git's standard ref decorations and show the last branch-ref update
from its reflog, or the newest update when branch tips coincide. Shared
ancestors and tips without reflogs use the commit date as a fallback.
Use --date=STYLE or d to format or hide dates.
Enter or click opens the commits since the next shared node; Backspace or
Escape returns from that Log. Roots and merged branches show full history.
Merged tips do not cut off other Logs unless they are also a shared ancestor
of open branches. Merged status uses the local branch named by origin/HEAD,
falling back to main or master. With only the current branch (or no branches),
print git branch output and exit, unless --watch keeps the picker open.
On quitting, leave the branch graph in the terminal for reference and copying.";

const LOG: &str = "Log shows committed history, loaded once unless --watch or W enables refresh.
Watching preserves revision/path arguments; Logs that name no revision or path
also include a Working tree item. W toggles watching in Log, Branches, Stashes, Reflog, and Bisect.
Use glog log show (or glog log diff) to browse a branch named after a command.
Log arguments are passed through to git log.
Paths after -- filter both Show patches and the Log history.";

const GRAPH: &str = "Graphs use rounded paths, ● for commits, • for empty non-merge commits or a
clean working tree with nothing in progress, ⦵ for shared ancestors with other
local or remote branches when no branch ref points there, and ○ for boundary
commits. Interchanges take precedence over empty-commit markers; comparison and
boundary markers stay intact.
┼ crosses paths without joining them.";

const REFLOG: &str =
    "Reflog shows local ref updates with their update times; Enter opens the commit.
Its graph keeps event order: ▲/▼ and sideways ◀/▶ nodes point from parents
into commits. ◆ marks merges, and ○ marks checkout/reset visits. ● has no
shown parent; ↕ has shown parents on both sides. • marks a repeated visit to
the same commit. Side-path arrowheads point into child commits.
Paths use the nearest parent visit on the same ref when available, and pass
through folded steps without adding rows.
Finished or aborted rebases, amend chains, and consecutive visits to the same
commit start folded in Reflog, represented by the commit-producing entry when
available. Fold controls follow the hash: ▶ is closed; ▲/▼/↕ show expanded
entries above/below/both. z cycles visits, nested amend versions, then closed
when they share a header. Search reveals hidden entries.";

const STASH_LIST: &str =
    "Stash lists saved work; Enter opens its changes, including saved untracked files.";

const STASH_SHOW: &str = "Stash show opens a stash by index or reference (the latest by default),
including saved untracked files.";

const BLAME: &str = "Blame shows which commit last changed each line; Enter opens it, p blames
the file before it, and Backspace returns.";

const GITHUB: &str =
    "GitHub code search uses gh authentication and accepts gh search code query options,
including --repo, --owner, --language, and --limit. Results group by repository,
folder, and file. Enter opens the exact remote file; e opens it on GitHub.
Inside a remote file, e opens the selected line; Esc or Backspace returns.
Files load on demand without a checkout. Output options (--json, --jq, --template,
--web) are reserved for glog's interface. GitHub's API uses legacy code search;
results can differ from the website and regular expressions are not supported.";

const GREP: &str = "Grep searches tracked text files, --cached searches the index, and --untracked
also searches untracked files. Accept matching options such as -i, -w, -F, -E,
-e and -f, compound expressions, and commit revisions. Binary files are skipped.
Results group by folder and file: Enter opens a match in Blame or toggles a
group; z folds the current group or a match's file, and m folds/expands all.
t hides the selected file extension; T clears filters. Click extension buttons
to toggle filters. Left/right pan; / and ? search within results and reveal folds.
Grep supplies the output layout; context, count, file-only, textconv, and
submodule-recursion output modes are not supported.";

const RANGE_DIFF: &str =
    "Range-diff compares two versions of a commit series using Git's colored output.
Use glog range-diff OLD_BASE..OLD_TIP NEW_BASE..NEW_TIP, or Git's other forms
and options. Scroll with the mouse wheel or paging keys; / and ? search.";

const BRANCH_RANGE_DIFF: &str =
    "In Branches, r or the Range-diff button compares the selected branch with its
stored upstream, or origin's same-named branch when no upstream is configured.
Both series require a single resolved branching point. The remote range is
resolved with its tip replacing the local branch; no fetch is performed.";

const SHOW: &str = "Show opens HEAD or the specified commit, with history available via Tab.
Breaking-change markers, section headings, and footer labels are emphasized.
Paths after -- filter both Show patches and the Log history.";

const MOVED_CODE: &str = "Patches highlight moved blocks using diff.colorMoved, or zebra if unset.
Git's diff.colorMovedWS controls whitespace matching unless --color-moved-ws
or --no-color-moved-ws overrides it. Session moved-code options apply across
views, including watch mode.";

const LINKS: &str =
    "Click underlined commit hashes in Show messages to follow them; Backspace returns.
Hashes use lowercase hex: 7+ characters, or with a # prefix, 7+ or 4-5
including a letter. On GitHub repositories, # plus 1-5 digits opens an issue
in the browser.";

const FILE: &str = "Show REV:path reads the complete text file; x/d/a show the hash/date/author
column and turn it into a blame, loading attribution only then.";

const DIFF: &str = "Diff opens unstaged changes (including untracked files), or staged changes
with --cached. Revisions compare commits (A..B, A...B, A B) or a commit
against the working tree (A); omitted range endpoints default to HEAD.
Diffs open standalone, without a Log tab or adjacent-commit navigation.
Diff exits if empty.";

const STATS: &str = "C toggles code statistics in Show, Diff, and Status summaries. This excludes
blank and comment-only lines in supported languages, and Python module, class,
and function docstrings. Mixed code/comment lines and other strings still count.
Unsupported languages, combined diffs, oversized files, and unavailable snapshots
keep ordinary counts. Counts follow Git's changed lines,
not just the difference in file size. Normal Git line counts remain the default.";

const NAVIGATION: &str = "Press h for key help, Tab to switch between Log and its detail view,
Ctrl-L to redraw the screen, and q or Ctrl-C to quit.
Press V to switch between Status and Log, Branches, Stashes, and Reflog, keeping your place.
Views opened through V watch by default; W freezes or resumes each view.
Search history persists between sessions; use Up/Down at / or ? to recall it.";

const ENVIRONMENT: &str = "GLOG_TITLE=0 disables terminal titles.
GLOG_HISTFILE overrides the history path; set it to - to disable persistence.
The default is $XDG_STATE_HOME/glog/search_history, or
$HOME/.local/state/glog/search_history (Windows: %LOCALAPPDATA%/glog/search_history).";

const SUMMARY: &str = "Add --stat to Show or Diff to start with expandable file summaries;
press s in Show to toggle summary / patch.";

#[cfg(test)]
mod tests {
    use super::*;

    fn information_for(args: &[&str]) -> Option<String> {
        information(&args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>())
    }

    #[test]
    fn overview_points_to_command_help() {
        assert_eq!(information_for(&["--help"]).as_deref(), Some(OVERVIEW));
        assert_eq!(information_for(&["-h"]).as_deref(), Some(OVERVIEW));
        assert!(OVERVIEW.contains("glog <command> --help"));
        assert!(!OVERVIEW.contains("--color-moved"));
    }

    #[test]
    fn help_follows_the_selected_command_and_nested_topic() {
        for (command, usage, detail) in [
            (vec!["log"], "glog [log]", "--fold-merges"),
            (vec!["branch"], "glog branch", "Merged tips"),
            (vec!["bisect"], "glog bisect log", "estimated steps"),
            (vec!["bisect", "log"], "glog bisect log", "estimated steps"),
            (vec!["reflog"], "glog reflog", "repeated visit"),
            (vec!["blame"], "glog blame", "-L LINE"),
            (vec!["stash"], "glog stash", "saved work"),
            (vec!["stash", "list"], "glog stash", "-n COUNT"),
            (vec!["stash", "show"], "glog stash show", "--color-moved"),
            (vec!["show"], "glog show", "REV:path"),
            (vec!["diff"], "glog diff", "--cached"),
            (vec!["range-diff"], "glog range-diff", "OLD_BASE..OLD_TIP"),
            (vec!["grep"], "glog grep", "compound expressions"),
            (
                vec!["gh", "search", "code"],
                "glog gh search code",
                "--repo",
            ),
            (vec!["status"], "glog status", "Working tree"),
        ] {
            for flag in ["--help", "-h"] {
                let mut args = command.clone();
                args.push(flag);
                let help = information_for(&args).unwrap();
                assert!(help.starts_with(&format!("Usage: {usage}")), "{args:?}");
                assert!(help.contains(detail), "{args:?}");
                assert_ne!(help, OVERVIEW);
                assert!(help.lines().all(|line| line.is_empty()
                    || line.starts_with(' ')
                    || line.starts_with("Usage:")
                    || line.ends_with(':')
                    || line == "Run glog --help for the command overview."));
            }
        }
        let branch = information_for(&["branch", "--help"]).unwrap();
        assert!(!branch.contains("--color-moved"));
        assert!(!branch.contains("GitHub code search"));
    }

    #[test]
    fn help_does_not_consume_git_arguments_or_search_patterns() {
        for args in [
            vec!["--all"],
            vec!["--help", "main"],
            vec!["log", "--help", "main"],
            vec!["log", "--", "--help"],
            vec!["show", "HEAD:--help"],
            vec!["grep", "-e", "--help"],
            vec!["grep", "--", "--help"],
            vec!["gh", "search", "code", "query", "--help"],
            vec!["bisect", "start", "--help"],
            vec!["stash", "drop", "--help"],
        ] {
            assert_eq!(information_for(&args), None, "{args:?}");
        }
    }

    #[test]
    fn version_remains_available() {
        for args in [vec!["--version"], vec!["-V"], vec!["log", "--version"]] {
            assert_eq!(
                information_for(&args).as_deref(),
                Some(concat!("glog ", env!("CARGO_PKG_VERSION")))
            );
        }
    }
}
