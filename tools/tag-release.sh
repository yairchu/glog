#!/usr/bin/env bash

set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=tools/release-common.sh
source "$script_dir/release-common.sh"

test "$#" -le 1 || die "usage: $0 [--check]"
check_only=false
if test "${1:-}" = --check; then
  check_only=true
elif test "$#" -ne 0; then
  die "usage: $0 [--check]"
fi

release_preflight
verify_release_ci

if test -n "$(git ls-remote --tags origin "refs/tags/$RELEASE_TAG")"; then
  die "tag already exists on origin: $RELEASE_TAG"
fi

local_tag=false
if git rev-parse -q --verify "refs/tags/$RELEASE_TAG" >/dev/null; then
  test "$(git cat-file -t "$RELEASE_TAG")" = tag ||
    die "local $RELEASE_TAG exists but is not an annotated tag"
  test "$(git rev-list -n 1 "$RELEASE_TAG")" = "$RELEASE_SHA" ||
    die "local $RELEASE_TAG does not point to HEAD"
  local_tag=true
fi

published=$(cargo search glog-tui --limit 1 | sed -n 's/^glog-tui = "\([^"]*\)".*/\1/p')
test "$published" = "$RELEASE_VERSION" ||
  die "crates.io reports glog-tui $published, expected $RELEASE_VERSION"

if "$check_only"; then
  printf 'Tagging checks passed for %s at %s.\n' "$RELEASE_TAG" "$RELEASE_SHA"
  exit 0
fi

if ! "$local_tag"; then
  git tag -a "$RELEASE_TAG" -m "glog $RELEASE_VERSION" "$RELEASE_SHA"
  printf 'Created local annotated tag %s at %s.\n' "$RELEASE_TAG" "$RELEASE_SHA"
fi
confirm_exactly "push $RELEASE_TAG"
git push origin "refs/tags/$RELEASE_TAG"
