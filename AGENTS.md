# Repository instructions

- Add `Co-authored-by: Codex <codex@openai.com>` to commits created or amended by Codex.
- Commit completed changes by default, without waiting to be asked. Do not
  push; pushing is a maintainer action.
- Never amend or otherwise rewrite a commit reachable from a remote ref unless
  the maintainer explicitly requests a history rewrite. Create a follow-up
  commit instead.
- While refining an unpushed change, such as follow-up tweaks requested
  in the same session, amend or squash it into its commit rather than
  adding small follow-up commits.
- Changelog fix entries should only describe bugs present in the previous
  release. For fixes to unreleased features, update the feature's existing
  entry if needed rather than adding a separate fix entry.
- Leave trivial cosmetic fixes, such as help-text spacing, out of the
  changelog.
- Keep the website concise and to the point: omit requirements and details
  that go without saying, such as needing Git.
- Keep the README and website focused on common workflows. Document esoteric
  options in command-line help rather than adding them to introductory docs.
- Don't add tests for behavior that works and was never broken. Add a
  regression test when something breaks or proves verifiably error-prone.
- Prefer code structures that make mistakes hard over ones that rely on
  matching or keeping separate pieces in sync, such as filtering
  user-facing text by its contents.

## Commit convention

Use Conventional Commits for new commits: `type: description` or
`type(scope): description` when a scope adds useful context.

- `feat`: new functionality
- `fix`: bug fixes
- `docs`: user-facing documentation, such as the README, changelog, and
  website copy; use `docs(dev):` for contributor and agent instructions such
  as this file
- `test`: tests and test infrastructure
- `refactor`: code restructuring without behavior changes
- `chore`: maintenance and releases

Use lowercase types and short, imperative descriptions, for example
`feat(log): filter commits by type`, `docs: simplify installation`,
`docs(dev): describe the release workflow`, or
`chore(release): prepare 0.3.2`. Mark breaking changes with `!` before the colon
and explain them in the commit body.

Apply this convention going forward; do not rewrite published history to
convert existing commit messages.

## Release workflow

1. Choose the next Semantic Versioning version and update it in both
   `Cargo.toml` and the `glog-tui` package entry in `Cargo.lock`.
2. Move the user-visible entries in `CHANGELOG.md` from `Unreleased` into a
   heading for the new version and release date. Leave a new empty `Unreleased`
   section at the top and update its comparison links.
3. Run formatting, tests, strict Clippy, and package verification with the
   lockfile:

   ```bash
   cargo fmt --check
   cargo test --locked --all-features
   cargo clippy --locked --all-targets --all-features -- -D warnings
   cargo publish --dry-run --locked
   ```

4. Commit the version, lockfile, and changelog together as the release commit.
5. The maintainer merges and pushes the release commit, then runs
   `tools/publish-release.sh`. The script verifies CI passed for the exact
   `origin/main` commit before publishing.
6. After crates.io serves the new version, the maintainer runs
   `tools/tag-release.sh`. The script verifies publication before creating and
   pushing the annotated `vX.Y.Z` tag.

Publishing and pushing tags are maintainer actions. Do not perform them unless
the maintainer explicitly requests them.
