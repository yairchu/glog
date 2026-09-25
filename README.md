# glog

`glog` — an interactive browser for Git history and working-tree changes.

It is a small, strictly read-only terminal UI for exploring Git history. Git remains responsible for revision parsing, pathspecs, traversal, decorations, and patch generation; `glog` adds selection, scrolling, search, and Log and detail views.

With no arguments, `glog` shows committed history, loaded once, without inspecting working-tree changes. Use `glog --watch` for a live view: a single `Working tree` item appears ahead of `HEAD`. Open it to inspect grouped staged, unstaged, untracked, and conflicting files. The item remains available as `Working tree · clean` when there are no changes.

## Install and run

Requires the latest stable Rust toolchain and an installed `git` executable.

```bash
cargo build --release
./target/release/glog
```

Or install from a checkout with `cargo install --path .`.

Install the `glog-tui` package from crates.io; the executable is named `glog`:

```bash
cargo install glog-tui
```

Use `glog --help` for command-line help and `glog --version` to print the
installed version.

### Oh My Zsh

Oh My Zsh's optional `git` plugin defines `glog` as an alias for
`git log --oneline --decorate --graph`. A shell alias takes precedence over the
installed executable, so affected users may appear to get the old command after
installing glog. Check with:

```zsh
type -a glog
```

Run glog once without expanding the alias using `\glog`. To make glog the
default permanently, add `unalias glog` to `~/.zshrc` after Oh My Zsh is
loaded:

```zsh
source $ZSH/oh-my-zsh.sh
unalias glog
```

Restart the shell or run `source ~/.zshrc`. This also allows glog's Zsh
completion adapter to take effect.

### Zsh completion

The included completion adapter reuses Zsh's `git log` completer, including branches, tags, revisions, paths, and Git log options. Explicit `log`, `show`,
and `diff` commands select the corresponding Git completer. From a checkout, install it with:

```zsh
mkdir -p ~/.cargo/share/zsh/site-functions
cp completions/_glog ~/.cargo/share/zsh/site-functions/_glog
```

Add the completion directory before `compinit` in `~/.zshrc`:

```zsh
fpath=(~/.cargo/share/zsh/site-functions $fpath)
autoload -Uz compinit
compinit
```

Restart Zsh or run those three lines in the current shell. `glog ma<Tab>` should then complete branch names just like `git log ma<Tab>`.

Arguments are passed through to `git log`, including revision ranges, traversal options, filters, and pathspecs:

```bash
glog
glog A...B
glog --all
glog --first-parent main
glog --since="2 weeks ago"
glog main -- src/
```

### Folding merge histories

In Log, select a merge and press `z` to collapse or expand the commits it brought
in. Enter still opens the merge's Show view. A collapsed merge displays a `▶`
and the number of folded commits; its first-parent history remains visible.
Nested merges can be folded independently. Press `m` to fold all merge histories,
or expand them when all are folded. If folding hides the selected commit, selection
moves to its containing merge.

Start with merges folded using:

```bash
glog --fold-merges
glog --watch --fold-merges
glog --fold-merges --all
```

Navigation skips folded commits, including Left/Right in Show. Searching Log
still searches the full loaded history and expands folds containing a match.
Watch refreshes preserve fold choices and the selected commit; new merges start
folded when `--fold-merges` is enabled. The graph is redrawn around hidden commits.
Folding respects the loaded revision, path, and count filters: it does not fetch
or load excluded commits. In particular, `--first-parent` omits side history,
so use `--fold-merges` instead when you want to expand that history later.

### Show and Diff

Open directly in Show with `glog show`, optionally naming a commit (hash, branch,
tag, or revision expression):

```bash
glog show                 # HEAD, even with a dirty working tree
glog show --stat main~3    # expandable file summary
glog show v0.1.0 -- src/
glog log show             # log for a branch named "show"
glog log --all
glog status               # live working-tree overview
```

Show accepts one commit and optional pathspecs after `--`. Pathspecs restrict
committed patches. With an
explicit commit, history remains rooted at that commit. With
no commit argument, Log shows committed history and keeps HEAD selected. History loads on
first switching to Log with `Escape` (or cycling tabs) or navigating with the arrow keys;
`q` quits directly. The existing folding, search, and file navigation work as
usual. `glog log` explicitly selects Log mode and otherwise accepts the same
arguments as the shorthand `glog` invocation.

Open working-tree changes or revision comparisons directly in Show:

```bash
glog diff                 # unstaged changes, including untracked files
glog diff --cached        # staged changes
glog diff --cached --stat # staged file summary
glog diff A..             # A to HEAD
glog diff ..A             # HEAD to A
glog diff A..B            # A to B (also: glog diff A B)
glog diff A...B           # merge base of A and B to B
glog diff A               # A to the working tree
glog diff --cached A      # A to the index
glog diff A..B --stat -- src/ # comparison summary limited to src/
glog log diff             # log for a branch named "diff"
```

Revision comparisons follow Git’s diff semantics; omitted range endpoints use
HEAD. Use `-- pathspec...` to limit any diff to selected paths; as with
`git diff`, the `--` may be omitted when the paths exist. Only plain
`glog diff` (optionally with path filters) includes untracked files.

If the requested changes are empty, glog prints a brief message and exits
successfully without opening the terminal UI. Every `diff` opens a standalone
view with the command shown as a context header, without tabs or
adjacent-commit navigation; use `glog --watch` or `glog status` to move between
working-tree changes and history.
The `diff` commands work without watch mode. Changes are read when opened and
untracked-file contents are read when expanded; this is not a session-wide
snapshot and does not refresh automatically. The `diff` command accepts only
the optional `--cached` and `--stat` flags.

Use `--stat` with `show` or `diff` to start with the commit message (if any)
and compact per-file summaries showing additions and deletions. Each file starts
collapsed; `Enter`/`z` expands its patch beneath the summary and collapses it again.
Binary files are labeled, and lazy untracked files show “contents not loaded”
until expanded. Search reveals matches inside collapsed patches already loaded.

Committed submodule pointer changes start collapsed, showing the old and new
commit IDs. Press `Enter`/`z` to load nested file summaries, then expand each file
with the same keys. This uses the recorded commits, independent of the submodule's
checkout, and requires an initialized submodule with both commits available locally.
Missing history is reported without fetching. Dirty working-tree contents and
submodule additions/deletions are not expanded. Combined merge diffs are expandable
when all parents record the same submodule commit.

Status labels submodules with modified or untracked contents as dirty. Press
`Enter`/`z` to expand their live Staged, Unstaged, and Untracked sections, then
expand individual files with the same keys. Nested changes refresh along with
Status, preserving expanded files. Diff summaries retain the dirty indicator;
their expansion still compares the recorded commits.

Merge commits with combined diffs also have expandable summaries; their counts
count each displayed changed line once across all parents.

In Show, press `s` to switch between the file summary and patch views. Collapsing
selects the current file; switching back restores the line you were reading.
This is an interactive presentation option, so the full patch remains available.
The default Show presentation is unchanged, including for large commits.

Use watch mode to refresh the default `HEAD` view when commits, branches, tags,
remote-tracking refs, staged changes, unstaged changes, or untracked files
change:

```bash
glog --watch
```

For its initial implementation, `--watch` accepts only the optional
`--fold-merges` display flag. Combinations such as `glog --watch --all` fail with
a concise error rather than providing partial watch semantics.

## Working-tree status

`glog status` opens the live **Status** view directly. It shows the current branch,
upstream ahead/behind counts when available, and a clean-tree message or
expandable **Staged**, **Unstaged**, and **Untracked** sections. **Conflicts**
appear first when present. Renames show both paths; files with staged and
unstaged edits appear in both sections. It also works before the first commit
and in detached HEAD state.

Sections and regular patches start open, while lockfiles and new files start
folded, just like Show. `Enter`/`z` toggles sections and individual files. Use Up/Down or j/k,
Page Up/Down, Home/End (or `g`/`G`, `<`/`>`), and the mouse to navigate. Left/Right navigates history,
just like Show: Right opens the newest commit, and Left from that commit returns
to Working tree. Shift-Left/Right pans long lines in Status.
Status refreshes once per second while visible, preserving expanded files and
the reading position where possible. File statistics always show additions in
green and deletions in red. Press `s` to switch between patches and collapsed
summaries; Enter/z expands a summary, and toggling back restores the reading
line. Summary mode carries between Status and Show.
Everything remains read-only: staging, unstaging, and conflict resolution happen
outside glog. Status currently accepts no command-line options.

Log and its detail view are the two navigation tabs. Opening a commit shows
**Show**; opening **Working tree** shows **Status**. `Tab` switches between Log
and the selected item's detail view, and `Escape` returns to Log. Both tabs are
clickable. `glog status` starts a watch session; returning to Log selects the
Working tree item. Ordinary `glog` remains committed-history-only.
Use `glog log status` to browse a branch named `status`.

## Log row format

Choose a Git-style format without requiring aligned columns:

```bash
glog --pretty=format:"%h %ad %an %s" --date=short
glog --format="%h [%ad] (%an) %s" --date=relative
glog --oneline
```

Supported placeholders are `%h` (short hash), `%H` (full hash), `%ad` (author
date), `%an` (author name), `%ae` (author email), `%d` (refs with parentheses),
`%D` (bare refs), `%s` (subject), and `%%` (literal percent). `--pretty` and
`--format` accept either `=VALUE` or a separate value, with optional `format:`
or `tformat:` prefixes. Other placeholders, named presets other than `oneline`,
and multiline formats are rejected. Git handles `--date`, including
`--date=format:...`; choose a date format that fits on one line. Dates containing
newlines or carriage returns are rejected with an error, including those from
Git's `log.date` setting; use `--date=short` to override that setting in Log.

Dates default to `YYYY-MM-DD HH:MM` in your local timezone, without seconds
or a timezone suffix. An explicit `--date` (or `--relative-date`) takes
precedence over Git's `log.date` setting, which takes precedence over glog's
fallback (`format-local:%Y-%m-%d %H:%M`).

In Log, press `a`, `d`, `r`, `x`, or `s` to toggle author, date, refs, hash, or
subject. Fields keep their position, punctuation, and color when restored.
For example, hiding author changes `%h [%ad] (%an) %s` into `%h [%ad] %s`.
All occurrences of the toggled field change together, including both author
name and email. A field absent from a custom format is inserted before the
subject, or at the end if there is no subject.

Literal labels, opening punctuation, and spaces before a placeholder belong
to that field; closing brackets/quotes and immediately following punctuation
belong to the preceding field. A final literal belongs to the last field.
Use wrappers around individual fields, such as ` (author: %an)` or ` [%ad]`,
for predictable toggling. Empty fields also omit their punctuation.

The default layout shows hash, date, author (with collaborator badges), refs,
and subject. Press `d` or `a` to hide date or author, or use `--oneline` for
the compact hash/refs/subject view. Hashes are yellow, dates gray, authors cyan, and refs
green. Search follows the currently rendered fields. Toggles last for the
session and survive switching views and watch refreshes. Synthetic working-tree
entries retain their identifying hash and subject regardless of format.

A collaborator badge appears once after the last visible author name/email field.
A single coauthor uses `꩜` for Codex in the terminal foreground color, `❋` for
Claude Code in warm terracotta, or a gray `1` for an unrecognized coauthor.
Two or more coauthors always use a gray total count, including Codex and Claude
together. The badge has a medium gray `+` separator: `Alice+❋`, `Alice+1`, or
`Alice+2`. The separator has its own color, independent of the icon or count.
They are static and follow the author toggle. Credits come only from
`Co-authored-by` trailers: `codex@openai.com` identifies Codex and
`noreply@anthropic.com` identifies Claude Code, including model-specific names.
Other identities use the generic badge. Emails are deduplicated without regard
to case, and the primary author is excluded. Full credits remain in Show.

## Keys

| Key | Log | Show |
| --- | --- | --- |
| `Tab` | open selected item | return to Log |
| `Escape` | — | return to Log |
| `↑` / `k`, `↓` / `j` | select commit | move patch cursor |
| `Page Up` / `b`, `Page Down` / `Space` | move by page | scroll by page |
| `f` | — | page forward |
| `a`, `d`, `r`, `x`, `s` | toggle author/date/refs/hash/subject | — |
| `←`, `→` | select previous/newer or next/older commit | previous/newer or next/older commit |
| `[`, `]` | — | previous/next changed file |
| `Enter` | open selected commit | expand/fold current folded file |
| `z` | expand/fold selected merge history | expand/fold current folded file |
| `L` | — | expand/fold all lockfiles |
| `m` | expand/fold all merge histories | — |
| `s` | toggle subject | toggle file summary / patch |
| `g` / `<` / `Home`, `G` / `>` / `End` | first/last commit | top/bottom |
| `/`, `?`, `n`, `N` | search forward/backward; repeat/opposite | search forward/backward; repeat/opposite |
| `h` | toggle key help | toggle key help |
| `Ctrl-L` | redraw the screen | redraw the screen |
| header click | switch tabs | switch tabs |
| log-row click | select; click again or double-click to open | — |
| mouse wheel | move selection | scroll patch |
| `q`, `Ctrl-C` | quit | quit |

While entering a `/` or `?` search, press `↑` to recall earlier searches for
editing and `↓` to move forward again. Moving past the newest search restores
what you were typing. Search history is shared between Log and Show for the
current session; only submitted, nonempty queries are saved.

The selected Log commit is the one displayed by Show. Show output is loaded lazily and a small cache keeps recently viewed patches.

Show highlights the current patch row. Navigation and search move this cursor,
and `Enter`/`z` acts on the file beneath it even when the cursor is in the last
screenful.

Common lockfiles (`*.lock`, `package-lock.json`, and `pnpm-lock.yaml`) start
folded in Show so source changes remain prominent. The placeholder always shows
the file and its added/deleted line counts. Press `Enter` or `z`, or click the
placeholder, to reveal the complete diff. Searching reveals a folded lockfile
automatically when it contains the selected match.

New untracked files also start folded individually, keeping large generated
trees navigable while allowing each file to be expanded independently. Their
contents are loaded and formatted only when expanded, so repositories with
many untracked artifacts still open quickly.

## Inline image previews

Binary PNG, JPEG, GIF, BMP, ICO, and WebP changes show labeled Before/After
previews inside expanded diffs in Show, Diff, and Status. Added files show only
After; deleted files show only Before. Merge commits show each parent's image
and the result. Previews scroll and fold with the patch,
preserve aspect ratio, and load in the background when visible. Animated images
show a still frame. Corrupt, unsupported, or oversized images keep the binary
notice and show an unavailable-preview message.

Previews use the Kitty graphics protocol's Unicode placeholders. They are
enabled automatically in Kitty and Ghostty outside tmux. Other terminals keep
the ordinary binary-file notice. To explicitly enable or disable previews:

```bash
GLOG_IMAGES=kitty glog diff
GLOG_IMAGES=off glog
```

For tmux, explicitly enable previews and enable tmux's `allow-passthrough`
setting; both tmux and the outer terminal must support Unicode placeholders.
Previews occupy up to 64 columns and 12 rows per image. Files larger than 16 MiB
or images exceeding 16 million pixels / 8192 pixels on either axis are skipped.
SVG rendering and pixel-difference overlays are not currently supported.

## Delta and color

`glog` automatically tries [`delta`](https://github.com/dandavison/delta) when it is on `PATH`. Delta is run with paging disabled; if it is absent or fails, ordinary colored `git show` output is used. ANSI colors from either program are rendered in the TUI.

Set `GLOG_DELTA=0` to always use plain Git output:

```bash
GLOG_DELTA=0 glog --all
```

`glog` never invokes commands that modify repository state.
