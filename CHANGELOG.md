# Changelog

Notable user-visible changes to `glog` are recorded here. This project follows
[Semantic Versioning](https://semver.org/).

## Unreleased

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

[Unreleased]: https://github.com/yairchu/glog/compare/v0.1.3...HEAD
[0.1.3]: https://github.com/yairchu/glog/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/yairchu/glog/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/yairchu/glog/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/yairchu/glog/releases/tag/v0.1.0
