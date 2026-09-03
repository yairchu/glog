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

### Zsh completion

The included completion adapter reuses Zsh's `git log` completer, including branches, tags, revisions, paths, and Git log options. From a checkout, install it with:

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

Use watch mode to refresh the default `HEAD` view when commits, staged changes, unstaged changes, or untracked files change:

```bash
glog --watch
```

For its initial implementation, `--watch` must be the only argument. Combinations such as `glog --watch --all` fail with a concise error rather than providing partial watch semantics.

Output-format options such as `--format`, `--pretty`, and `--oneline` are reserved by `glog`, since its parser requires a machine-readable format.

## Keys

| Key | Log | Show |
| --- | --- | --- |
| `Tab` | switch to Show | switch to Log |
| `Enter`, `Escape` | open selected commit; Escape does nothing | Enter does nothing; return to Log |
| `↑` / `k`, `↓` / `j` | select commit | scroll patch |
| `Page Up` / `b`, `Page Down` / `Space` | move by page | scroll by page |
| `f` | — | page forward |
| `←`, `→` | — | previous/newer or next/older commit |
| `g`, `G` | first/last commit | top/bottom |
| `/`, `?`, `n`, `N` | search forward/backward; repeat/opposite | search forward/backward; repeat/opposite |
| `h` | toggle key help | toggle key help |
| header click | switch tabs | switch tabs |
| log-row click | select; click again or double-click to open | — |
| mouse wheel | move selection | scroll patch |
| `q` | quit | quit |

The selected Log commit is the one displayed by Show. Show output is loaded lazily and a small cache keeps recently viewed patches.

## Delta and color

`glog` automatically tries [`delta`](https://github.com/dandavison/delta) when it is on `PATH`. Delta is run with paging disabled; if it is absent or fails, ordinary colored `git show` output is used. ANSI colors from either program are rendered in the TUI.

Set `GLOG_DELTA=0` to always use plain Git output:

```bash
GLOG_DELTA=0 glog --all
```

`glog` never invokes commands that modify repository state.
