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

Arguments are passed through to `git log`, including revision ranges, traversal options, filters, and pathspecs:

```bash
glog
glog A...B
glog --all
glog --first-parent main
glog --since="2 weeks ago"
glog main -- src/
```

Output-format options such as `--format`, `--pretty`, and `--oneline` are reserved by `glog`, since its parser requires a machine-readable format.

## Keys

| Key | Log | Show |
| --- | --- | --- |
| `Tab` | switch to Show | switch to Log |
| `Enter`, `Escape` | open selected commit; Escape does nothing | Enter does nothing; return to Log |
| `↑` / `k`, `↓` / `j` | select commit | scroll patch |
| `Page Up` / `b`, `Page Down` / `Space` | move by page | scroll by page |
| `f` | — | page forward |
| `g`, `G` | first/last commit | top/bottom |
| `/`, `?`, `n`, `N` | search forward/backward; repeat/opposite | search forward/backward; repeat/opposite |
| `h` | toggle key help | toggle key help |
| header click | switch tabs or open help | switch tabs or open help |
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
