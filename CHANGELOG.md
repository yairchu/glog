# Changelog

Notable user-visible changes to `glog` are recorded here. This project follows
[Semantic Versioning](https://semver.org/).

## Unreleased

### Added

- Added Left/Right shortcuts in Log to select the previous/newer or next/older
  commit while staying in Log, matching commit navigation in Show and Status.

- Added `less`-style `<` and `>` shortcuts to jump to the top and bottom in
  Show, Diff, Status, and Log.

- Added revision comparisons to `glog diff`: `A..`, `..A`, `A..B`, `A B`,
  `A...B`, a single revision against the working tree, and `--cached A` against
  the index. Diff also accepts `-- pathspec...` filters, with summary mode,
  folding, and image previews available for comparisons. Explicit comparisons
  open a standalone diff with a command context header, no tabs, and no
  adjacent-commit navigation.

- Show branch names and tags beside the commit hash in Show using Git's native
  ref decorations, refreshing them in watch mode when refs change while
  preserving the reading position.

- Added inline Before/After image previews in expanded Show, Diff, and Status
  patches using Kitty graphics (automatically enabled in Kitty and Ghostty).
  Previews load lazily, scroll and fold with the diff, and display the correct
  historical, staged, or working-tree contents, regardless of Git diff prefix
  settings. `GLOG_IMAGES=off` disables them;
  `GLOG_IMAGES=kitty` explicitly enables them, including through configured tmux.

- Added `glog status` and a live Status detail view with branch/upstream
  information, clean-tree state, expandable conflict/staged/unstaged/untracked
  sections, and lazily loaded inline patches. Watch mode has one Working tree
  item that opens Status, including when clean; its Log summary always shows
  Clean or counts of changed and untracked files. Commits open Show. Left/Right
  navigates between Working tree and commits, with Shift-Left/Right for panning
  long Status lines. File statistics always show green additions and red deletions.
  Live refreshes reuse unchanged patches and file statistics, invalidating them
  when effective Git attributes change. Tracked statistics are fetched in batches.
  Untracked statistics stream file contents without
  generating patches or invoking delta. Status shares Show’s default folding rules and
  `s` summary-mode setting, restoring the patch reading line when toggled back. Enter/z can fold any file
  from its statistics line, retaining that choice across live refreshes.

- Added expandable file summaries with `glog show --stat`, `glog diff --stat`,
  and the Show `s` toggle, including combined merge diffs. Enter/z expands
  individual patches beneath their summaries, search reveals matching loaded
  patches, and switching back to the
  patch restores the reading line.

### Changed

- Rendered Git notes headings in bold in Show, keeping note bodies plain.

- Limited the Working tree Log item to watch sessions. Ordinary Log now loads
  committed history only, without inspecting working-tree changes. Direct
  `glog diff` and `glog diff --cached` remain available without watch mode.

### Fixed

- Preserved reading position and expanded files when watch mode refreshes,
  keeping the selected commit in place and following surviving lines in changed
  working-tree patches.

## 0.2.1 - 2026-09-15

### Added

- Added `Ctrl-L` to redraw the screen, including while searching or viewing help.

### Fixed

- Kept the Show viewport steady when the next or previous search match is
  already visible, scrolling only enough to reveal off-screen matches,
  accounting for wrapped lines and revealing matches in their continuations.
  Made `G`/End reach the final wrapped screen row.
- Highlighted the full Show cursor row with a lighter background, brightening
  existing diff backgrounds while preserving search highlight colors and
  following wrapped cursor rows.
- Recognized `Co-authored-by: Codex <noreply@openai.com>` as a Codex contribution.
- Made Log search match full commit hashes and longer abbreviations, including
  when hashes are hidden from the displayed rows.

## 0.2.0 - 2026-09-14

### Added

- Added compact collaborator badges beside the Log author: `꩜` for Codex,
  terracotta `❋` for Claude Code, and gray `+N` for multiple or unrecognized coauthors,
  based on deduplicated `Co-authored-by` trailers and following the author toggle.

- Added configurable, colored Log rows with `--pretty=format:...`, `--format`,
  and Git date formatting, plus `a`/`d`/`r`/`x`/`s` keys to toggle author,
  date, refs, hash, and subject together with their associated punctuation.
  Dates default to local `YYYY-MM-DD HH:MM`, respecting Git's `log.date`
  setting and explicit date options. Dates, authors, and collaborator badges
  are visible by default; `--oneline` keeps the compact hash/refs/subject view.
  Display-only options, including date formatting, preserve working-tree entries.
  Multiline Git dates are rejected explicitly instead of hiding commits.

- Added search history recall with Up/Down while entering `/` or `?` searches,
  allowing previous queries to be edited and reused across Log and Show.

- Added `glog diff` and `glog diff --cached` to open unstaged or staged changes
  directly, exiting successfully when the requested changes are empty.
- Added `glog show [commit] [-- pathspec...]` to open directly in Show, with
  history loaded when navigating, pathspecs applied to both committed and
  working-tree patches, and `glog log` as an explicit Log command.

## 0.1.3 - 2026-09-10

### Fixed

- Made folded files in the last screenful of Show keyboard-accessible by adding
  a visible cursor independent of the scroll position.
- Made mouse-wheel scrolling in Show move the viewport immediately while keeping
  the cursor visible.
- Made Show page-navigation keys scroll the viewport by the visible page height.

## 0.1.2 - 2026-09-06

### Fixed

- Made watch mode fingerprint untracked files from metadata instead of rereading
  their complete contents every second.
- Prevented unstaged views with thousands of untracked files from appearing to
  hang by folding each file individually and loading and formatting its patch
  only when expanded.

## 0.1.1 - 2026-09-06

### Fixed

- Prevented large diffs formatted by delta from hanging when delta's output
  exceeded the operating system pipe buffer.

### Documentation

- Documented the `glog` alias conflict in Oh My Zsh's optional Git plugin and
  how to invoke or unalias the installed executable.

## 0.1.0 - 2026-09-03

- Initial release of the interactive `git log` and `git show` browser.

[Unreleased]: https://github.com/yairchu/glog/compare/v0.2.1...HEAD
[0.2.1]: https://github.com/yairchu/glog/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/yairchu/glog/compare/v0.1.3...v0.2.0
[0.1.3]: https://github.com/yairchu/glog/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/yairchu/glog/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/yairchu/glog/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/yairchu/glog/releases/tag/v0.1.0
