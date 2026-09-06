# Changelog

Notable user-visible changes to `glog` are recorded here. This project follows
[Semantic Versioning](https://semver.org/).

## Unreleased

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

[Unreleased]: https://github.com/yairchu/glog/compare/v0.1.2...HEAD
[0.1.2]: https://github.com/yairchu/glog/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/yairchu/glog/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/yairchu/glog/releases/tag/v0.1.0
