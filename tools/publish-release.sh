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

if git rev-parse -q --verify "refs/tags/$RELEASE_TAG" >/dev/null; then
  die "tag already exists locally: $RELEASE_TAG"
fi
if test -n "$(git ls-remote --tags origin "refs/tags/$RELEASE_TAG")"; then
  die "tag already exists on origin: $RELEASE_TAG"
fi

cargo publish --dry-run --locked

if "$check_only"; then
  printf 'Publication checks passed for glog-tui %s.\n' "$RELEASE_VERSION"
  exit 0
fi

confirm_exactly "publish glog-tui $RELEASE_VERSION"
cargo publish --locked
printf 'Published glog-tui %s. Run tools/tag-release.sh after crates.io serves it.\n' \
  "$RELEASE_VERSION"
