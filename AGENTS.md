# Repository instructions

- Add `Co-authored-by: Codex <codex@openai.com>` to commits created or amended by Codex.

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
5. The maintainer merges and pushes the release commit, waits for CI, runs
   `cargo publish --locked`, creates an annotated `vX.Y.Z` tag on the published
   commit, and pushes the tag.

Publishing and pushing tags are maintainer actions. Do not perform them unless
the maintainer explicitly requests them.
