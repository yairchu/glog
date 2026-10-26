# Changelog

Notable user-visible changes to `glog` are recorded here. This project follows
[Semantic Versioning](https://semver.org/).

## Unreleased

### Fixed

- Recognize commit types with consecutive parenthesized groups, such as
  `test(failing)(dsp):`, for highlighting and filtering.

## 0.6.1 - 2026-10-20

### Added

- Draw empty non-merge commits with a smaller bullet, including empty root
  commits. Merge commits retain their filled marker.

- Show a smaller bullet for a clean working tree, keeping the larger marker
  when there are changes.

- Fold finished or aborted rebases in Reflog at their final entry; `z` expands
  or folds the steps, including amendments, cherry-picks, resets, and
  fast-forwards. Search reveals matching steps, and the entry before each
  rebase stays visible. Incomplete or ambiguous sequences stay expanded.

- Fold consecutive amendments in Reflog under the newest version, including
  the adjacent original commit when available. `z` toggles the fold and search
  reveals earlier versions. Chains stay within one ref, stop at intervening
  actions, and remain part of their rebase when inside a rebase fold.

### Fixed

- Prevent background status and diff checks from refreshing and locking the
  index, which could interrupt concurrent commits and rebases.

- Highlight the refs in Reflog's rebase abort entries, and in start and finish
  entries also for `git pull --rebase`, `git rebase -i` in older Git versions,
  and continued rebases.

## 0.6.0 - 2026-10-15

### Added

- Press `e` in Status, Diff, or Show to open the working file in `$VISUAL`
  or `$EDITOR` at the selected line. Adjust positions for staged and historical
  changes, and handle VS Code and Xcode line-navigation flags automatically.

- Draw commit graphs with Unicode lines, rounded corners, and filled commit
  dots. Crossings remain distinct from joins, including in folded and filtered
  histories, respecting first-parent traversal and marking excluded boundary
  commits with hollow circles. Preserve left/right and patch-equivalence
  markers in comparison histories. Lanes keep their columns as others end, so
  wide histories stay as compact as Git's. First-parent chains keep their
  colors through merges, folds, and filters. Use `--ascii-graph` for the original
  Git-style drawing and coloring.

- Emphasize breaking-change markers in Log, Reflog, and Show, including
  `!`, `BREAKING CHANGE:`, and `BREAKING-CHANGE:`. Show also emphasizes
  section headings and footer labels while keeping their values plain.

- Recognize Conventional Commit types case-insensitively in highlighting
  and filters, preserving the subject's original spelling. Show titles use
  the same highlighting as Log.

- Highlight branch names and revision targets in Reflog checkout, reset,
  branch creation/rename, and rebase start/finish messages.

- Mark Gemini CLI and Gemini Code Assist coauthors with a blue `✦` in Log,
  Blame, and Show.

- Click underlined commit hashes in Show messages to browse the referenced
  commit, including commits outside the current history or path filters.
  Hashes use lowercase hex and need seven or more characters, or with a `#`
  prefix, four or five including a letter. Six-character `#` references are
  left plain, as they are usually colors.
  Backspace returns through followed references, preserving the previous
  view's reading position and expanded files.

- Click URLs and issue references (`#` plus 1–5 digits) in Show messages to
  open them in the browser. The GitHub repository is taken from `origin`, or
  without one, from the only GitHub remote; numeric references with seven or
  more digits remain commit candidates. Links to that repository's commits,
  and comparisons between commit hashes, open in glog when the history is
  available locally.

### Fixed

- Folded and filtered graphs keep Git's lane colours into `<`, `>`, `=`, and
  `o` commits of comparison and boundary histories.

- Folded and filtered graphs no longer connect rows through the side branch
  of a merge omitted by Git's filters.

- Folded and filtered graphs keep lanes to parents below a count limit such
  as `-n`, as Git draws them, rather than dropping them.

- Folding merges or filtering by type in path- or grep-filtered histories no
  longer freezes the Log while Git reads the history those filters omitted.
  The request applies once the read completes, and `q` quits in the meantime.

- A click right after fast trackpad scrolling selects what's under the
  pointer, rather than what was there before the scroll.

## 0.5.2 - 2026-10-08

### Changed

- `glog status` starts in Log with the Clean working-tree row selected when
  there are no changes. Status refreshes keep the current view.

## 0.5.1 - 2026-10-07

### Added

- `glog --exit-on-esc` lets Escape exit from the Log list, for returning to
  a calling tool after browsing commits.

- `glog show REV:path` opens a complete text file. Showing a hash, date, or
  author column turns it into a blame. Configured Git textconv filters
  provide the readable contents, as in blame.

- Show marks Codex and Claude Code `Co-authored-by` trailers with the same
  icons as Log's coauthor badges.

### Changed

- Paths passed to `glog show REV -- path` now filter Log history as well as
  Show patches. Show still opens the requested commit first, even if it has
  no matching changes.

## 0.5.0 - 2026-10-04

### Added

- Added `glog stash` / `glog stash list` to browse saved work and
  `glog stash show` to open a stash by index or reference. Patches include
  separate staged, unstaged, and saved untracked changes, with search, file folding,
  summary mode, navigation between stashes, and toggleable author/date columns.
- Added `glog blame` to see which commit last changed each line of a file.
  Enter opens the commit at that file, `p` blames the file as it was before
  that commit, following renames, and `Backspace` goes back. Supports
  revisions, `-L` to open at a line, and date formatting; dates default to
  just the day. `x`, `d`, and `a` toggle the hash, date, and author columns,
  and code is syntax-highlighted with delta when it is installed.

### Fixed

- Commit-type filters keep branch points and commits labelled with refs visible
  as context, including branch tips in linear history after a rebase.
- `glog FILE` now shows the file's history, like `git log FILE`, instead of
  failing unless the path follows `--`.

## 0.4.0 - 2026-10-01

### Added

- Added `glog reflog` to browse local ref updates with commit hashes, update
  times, identities, and actions. Enter opens the commit; search and navigation
  keep repeated visits to the same commit separate. Supports named refs,
  `--all`, entry limits, and date formatting.
- Added highlighting of recognized Conventional Commit types in Log, in the
  same light blue as the type-filter controls.

## 0.3.2 - 2026-09-30

### Added

- Added `f` as a forward-page shortcut in Log, matching Space and Page Down.
- Added commit-type filtering in Log: `t` or the top-bar button hides the
  selected commit's Conventional Commit type, clicking a hidden type restores
  it, and `T` clears all filters. `--hide-types=test,refactor,docs` hides types
  at startup, including in watch mode. Scopes, `!` markers, and issue numbers
  such as `test #234(failing):` are recognized, and `doc`/`docs`,
  `tests`/`test`, and `feature`/`feat` share a filter. Untyped commits stay
  visible.
- Added merge-commit filtering in Log: `M`, the top-bar button, or
  `--hide-merges` hides merge commits while keeping their branch history.
  Merge and type filters combine, and the top bar shows how many commits are
  hidden. Navigation and search skip hidden commits, the graph keeps visible
  history connected, and watch refreshes preserve filters. Folded merges stay
  shown so their history can still be expanded.
- `--watch` now also accepts `--oneline` and `--format`/`--pretty`.

### Fixed

- Folding merges no longer recolors the graph: redrawn lanes keep the colors
  Git gave the commits they lead to.
- Mouse scrolling and clicks no longer act on the view hidden behind the help
  overlay or while a search is being typed.

## 0.3.1 - 2026-09-28

### Added

- Added merge-history folding in Log: `z` collapses or expands a merge's
  introduced commits, `m` toggles all merge histories, and `--fold-merges`
  starts with merges folded, including in watch mode. Folded merges show commit
  counts; navigation skips hidden commits, search reveals matching commits, and
  watch refreshes preserve fold choices. Nested merges fold independently, and
  revision/path filters still apply.

## 0.3.0 - 2026-09-24

### Added

- Added on-demand inline expansion of committed submodule pointer changes in
  Show and Diff, with nested file summaries, patches, and image previews from
  locally available recorded commits, including combined merge diffs whose parents record the same
  submodule commit. Expanded nested patches and their reading position survive
  watch refreshes when the recorded submodule commits are unchanged. Toggling
  summary mode back restores the reading line inside expanded submodules.
  Collapsing a nested patch keeps the cursor on that file's summary. `[` and
  `]` land on an expanded submodule's summary, and `L` keeps the cursor in
  place inside an expanded submodule.

- Added Left/Right shortcuts in Log to select the previous/newer or next/older
  commit while staying in Log, matching commit navigation in Show and Status.

- Added `less`-style `<` and `>` shortcuts to jump to the top and bottom in
  Show, Diff, Status, and Log.

- Added revision comparisons to `glog diff`: `A..`, `..A`, `A..B`, `A B`,
  `A...B`, a single revision against the working tree, and `--cached A` against
  the index. Diff also accepts pathspec filters, after `--` or, as in
  `git diff`, directly after the revisions when the paths exist. Summary mode,
  folding, and image previews are available for comparisons. Comparisons open
  a standalone diff with a command context header, no tabs, and no
  adjacent-commit navigation.

- Show branch names and tags beside the commit hash in Show using Git's native
  ref decorations, refreshing them in watch mode when refs change while
  preserving the reading position.

- Added inline Before/After image previews in expanded Show, Diff, and Status
  patches using Kitty graphics (automatically enabled in Kitty and Ghostty).
  Previews load lazily, scroll and fold with the diff, and display the correct
  historical, staged, or working-tree contents, regardless of Git diff prefix
  or relative-path settings. Merge commits preview each parent and the result.
  `GLOG_IMAGES=off` disables them; `GLOG_IMAGES=kitty` explicitly enables them,
  including through configured tmux.

- Added `glog status` and a live Status detail view with branch/upstream
  information, clean-tree state, expandable conflict/staged/unstaged/untracked
  sections, and lazily loaded inline patches. Watch mode has one Working tree
  item that opens Status, including when clean; its Log summary always shows
  Clean or counts of changed and untracked files. Commits open Show. Left/Right
  navigates between Working tree and commits, with Shift-Left/Right for panning
  long Status lines. File statistics always show green additions and red deletions.
  Live refreshes reuse unchanged patches and file statistics, invalidating them
  when effective Git attributes change. Tracked statistics are fetched in batches.
  Status uses consistent rename detection for file entries, patches, and statistics
  even when Git's status and diff rename settings disagree. Non-UTF-8 filenames
  retain distinct patches, statistics, and folding state.
  Repository paths retain non-UTF-8 bytes and trailing newlines when opening Status.
  Unreadable untracked files show individual errors while other entries keep refreshing.
  Dirty submodules can be expanded into summaries of their live staged, unstaged,
  and untracked changes, like submodules in Show, with nested file patches
  expanding individually. Diff submodule summaries retain the
  dirty indicator alongside the commit comparison.
  Untracked statistics stream file contents without
  generating patches or invoking delta. Status shares Show’s default folding rules and
  `s` summary-mode setting, restoring the patch reading line, including inside
  expanded dirty submodules, when toggled back. Enter/z can fold any file
  from its statistics line, retaining that choice across live refreshes.

- Added expandable file summaries with `glog show --stat`, `glog diff --stat`,
  and the Show `s` toggle, including combined merge diffs. Enter/z expands
  individual patches beneath their summaries, search reveals matching loaded
  patches, and switching back to the
  patch restores the reading line. Non-UTF-8 filenames retain distinct file
  identities and default lockfile folding in Show and Diff, including when
  Git's `core.quotePath` setting is disabled. Show, Diff, and Status display
  filenames with Git-style escapes for control characters, backslashes, and
  non-UTF-8 bytes, so distinct names look distinct.

### Changed

- Rendered Git notes headings in bold in Show, keeping note bodies plain.

- Limited the Working tree Log item to watch sessions. Ordinary Log now loads
  committed history only, without inspecting working-tree changes. Direct
  `glog diff` and `glog diff --cached` remain available without watch mode.

- Made `glog diff` and `glog diff --cached` standalone views, like revision
  comparisons: they show the command as a context header and no longer switch
  to Log. Use `glog --watch` or `glog status` to move between working-tree
  changes and history.

### Fixed

- Kept search from expanding untracked files in `glog diff` whose contents are
  not loaded yet, which showed an internal placeholder line. Matching their
  header now selects the file.

- Listed untracked files from the whole repository when running `glog diff`
  from a subdirectory, labeling them by repository-relative path like tracked
  changes, and refreshed watch sessions when those files change.

- Expanded untracked files with non-UTF-8 names in `glog diff`, instead of
  failing to find them, keeping similarly named files distinct.

- Preserved non-UTF-8 untracked filenames when fingerprinting watch sessions,
  preventing startup failures on filesystems that support these names.

- Explained why untracked nested repositories in `glog diff` have no patch
  when expanded, instead of removing their section.

- Preserved reading position and expanded files when watch mode refreshes,
  keeping the selected commit selected and following surviving lines in changed
  working-tree patches. Log keeps its scroll position, so changed history
  visibly moves, and scrolls only to keep the selected commit on screen.

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

[Unreleased]: https://github.com/yairchu/glog/compare/v0.6.1...HEAD
[0.6.1]: https://github.com/yairchu/glog/compare/v0.6.0...v0.6.1
[0.6.0]: https://github.com/yairchu/glog/compare/v0.5.2...v0.6.0
[0.5.2]: https://github.com/yairchu/glog/compare/v0.5.1...v0.5.2
[0.5.1]: https://github.com/yairchu/glog/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/yairchu/glog/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/yairchu/glog/compare/v0.3.2...v0.4.0
[0.3.2]: https://github.com/yairchu/glog/compare/v0.3.1...v0.3.2
[0.3.1]: https://github.com/yairchu/glog/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/yairchu/glog/compare/v0.2.1...v0.3.0
[0.2.1]: https://github.com/yairchu/glog/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/yairchu/glog/compare/v0.1.3...v0.2.0
[0.1.3]: https://github.com/yairchu/glog/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/yairchu/glog/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/yairchu/glog/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/yairchu/glog/releases/tag/v0.1.0
