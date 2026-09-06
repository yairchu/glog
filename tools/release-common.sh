#!/usr/bin/env bash

set -euo pipefail

die() {
  printf 'release: %s\n' "$*" >&2
  exit 1
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"
}

release_version() {
  sed -n '/^\[package\]$/,/^\[/s/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1
}

release_preflight() {
  require_command git
  require_command cargo
  require_command gh

  git rev-parse --show-toplevel >/dev/null 2>&1 || die "not inside a Git repository"
  local root
  root=$(git rev-parse --show-toplevel)
  cd "$root"

  test -z "$(git status --porcelain)" || die "working tree is not clean"
  test "$(git branch --show-current)" = main || die "release must run from the main branch"

  RELEASE_VERSION=$(release_version)
  test -n "$RELEASE_VERSION" || die "could not read the package version from Cargo.toml"
  RELEASE_TAG="v$RELEASE_VERSION"
  RELEASE_SHA=$(git rev-parse HEAD)

  local lock_version
  lock_version=$(sed -n '/^name = "glog-tui"$/,+1s/^version = "\([^"]*\)"/\1/p' Cargo.lock)
  test "$lock_version" = "$RELEASE_VERSION" ||
    die "Cargo.lock version $lock_version does not match Cargo.toml $RELEASE_VERSION"
  grep -Fq "## $RELEASE_VERSION - " CHANGELOG.md ||
    die "CHANGELOG.md has no release heading for $RELEASE_VERSION"

  local remote_sha
  remote_sha=$(git ls-remote origin refs/heads/main | awk '{print $1}')
  test -n "$remote_sha" || die "could not resolve origin/main"
  test "$remote_sha" = "$RELEASE_SHA" ||
    die "HEAD ($RELEASE_SHA) is not the published origin/main commit ($remote_sha)"

  export RELEASE_VERSION RELEASE_TAG RELEASE_SHA
}

verify_release_ci() {
  local run
  run=$(gh run list \
    --workflow CI \
    --event push \
    --commit "$RELEASE_SHA" \
    --limit 1 \
    --json databaseId,status,conclusion,headSha \
    --template '{{range .}}{{.databaseId}}|{{.status}}|{{.conclusion}}|{{.headSha}}{{"\n"}}{{end}}')
  test -n "$run" || die "no CI push run found for $RELEASE_SHA"

  local run_id status conclusion head_sha
  IFS='|' read -r run_id status conclusion head_sha <<<"$run"
  test "$head_sha" = "$RELEASE_SHA" || die "CI run does not belong to HEAD"
  if test "$status" != completed; then
    printf 'Waiting for CI run %s...\n' "$run_id"
    gh run watch "$run_id" --exit-status
  elif test "$conclusion" != success; then
    die "CI run $run_id completed with conclusion: $conclusion"
  fi
  printf 'CI passed for %s.\n' "$RELEASE_SHA"
}

confirm_exactly() {
  local expected=$1
  local reply
  printf 'Type "%s" to continue: ' "$expected" >&2
  IFS= read -r reply || die "confirmation cancelled"
  test "$reply" = "$expected" || die "confirmation did not match"
}
