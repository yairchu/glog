# glog

`glog` — an interactive `git log` and `git show` browser.

It is a small, strictly read-only terminal UI for exploring Git history. Git remains responsible for revision parsing, pathspecs, traversal, decorations, and patch generation; `glog` adds selection, scrolling, search, and Log/Show tabs.

With no arguments, dirty working-tree state appears ahead of `HEAD` as separate `Unstaged changes` and `Staged changes` entries, divided from committed history by a labeled separator. Untracked files are included in the unstaged diff. These synthetic entries are omitted when empty and whenever explicit `git log` arguments are supplied.

## Install and run

Requires Rust 1.80+ and an installed `git` executable.

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

Open directly in Show with `glog show`, optionally naming a commit (hash, branch,
tag, or revision expression):

```bash
glog show                 # HEAD, even with a dirty working tree
glog show main~3
glog show v0.1.0 -- src/
glog log show             # log for a branch named "show"
glog log --all
```

Show accepts one commit and optional pathspecs after `--`. Pathspecs restrict
patches. With an explicit commit, history remains rooted at that commit. With
no commit argument, Log includes the usual unstaged and staged entries and
keeps HEAD selected. History loads on
first switching to Log with `Tab`/`Escape` or navigating with the arrow keys;
`q` quits directly. The existing folding, search, and file navigation work as
usual. `glog log` explicitly selects Log mode and otherwise accepts the same
arguments as the shorthand `glog` invocation.

Open working-tree changes directly in Show:

```bash
glog diff                 # unstaged changes, including untracked files
glog diff --cached        # staged changes
glog log diff             # log for a branch named "diff"
```

If the requested changes are empty, glog prints a brief message and exits
successfully without opening the terminal UI. Switching to Log loads the
normal history, including working-tree entries, and keeps the displayed entry
selected. This also works before the repository's first commit. This initial
`diff` command accepts only the optional `--cached` flag.

Use watch mode to refresh the default `HEAD` view when commits, branches, tags,
remote-tracking refs, staged changes, unstaged changes, or untracked files
change:

```bash
glog --watch
```

For its initial implementation, `--watch` must be the only argument. Combinations such as `glog --watch --all` fail with a concise error rather than providing partial watch semantics.

Output-format options such as `--format`, `--pretty`, and `--oneline` are reserved by `glog`, since its parser requires a machine-readable format.

## Keys

| Key | Log | Show |
| --- | --- | --- |
| `Tab` | switch to Show | switch to Log |
| `Escape` | — | return to Log |
| `↑` / `k`, `↓` / `j` | select commit | move patch cursor |
| `Page Up` / `b`, `Page Down` / `Space` | move by page | scroll by page |
| `f` | — | page forward |
| `←`, `→` | — | previous/newer or next/older commit |
| `[`, `]` | — | previous/next changed file |
| `Enter` | open selected commit | expand/fold current folded file |
| `z` | — | expand/fold current folded file |
| `L` | — | expand/fold all lockfiles |
| `g`, `G` | first/last commit | top/bottom |
| `/`, `?`, `n`, `N` | search forward/backward; repeat/opposite | search forward/backward; repeat/opposite |
| `h` | toggle key help | toggle key help |
| header click | switch tabs | switch tabs |
| log-row click | select; click again or double-click to open | — |
| mouse wheel | move selection | scroll patch |
| `q`, `Ctrl-C` | quit | quit |

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

## Delta and color

`glog` automatically tries [`delta`](https://github.com/dandavison/delta) when it is on `PATH`. Delta is run with paging disabled; if it is absent or fails, ordinary colored `git show` output is used. ANSI colors from either program are rendered in the TUI.

Set `GLOG_DELTA=0` to always use plain Git output:

```bash
GLOG_DELTA=0 glog --all
```

`glog` never invokes commands that modify repository state.
