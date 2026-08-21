#!/usr/bin/env bash

set -euo pipefail

if [[ "$#" -ne 2 ]]; then
  printf 'usage: %s TAG COMMIT\n' "${0##*/}" >&2
  exit 2
fi

release_tag=$1
expected_commit=$2
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
python3 "$script_dir/prepare_skills_release.py" version "$release_tag" >/dev/null

if [[ ! "$expected_commit" =~ ^[0-9a-f]{40}$ ]]; then
  printf 'expected commit must be a full lowercase Git object ID\n' >&2
  exit 2
fi

remote_refs=$(git ls-remote --exit-code --tags origin \
  "refs/tags/$release_tag" "refs/tags/$release_tag^{}") || {
  printf 'release tag %s does not exist on origin\n' "$release_tag" >&2
  exit 1
}
remote_commit=$(printf '%s\n' "$remote_refs" | awk '
  $2 ~ /\^\{\}$/ { peeled = $1 }
  $2 !~ /\^\{\}$/ { direct = $1 }
  END { print peeled != "" ? peeled : direct }
')
if [[ "$remote_commit" != "$expected_commit" ]]; then
  printf 'release tag %s resolves to %s on origin, expected %s\n' \
    "$release_tag" "$remote_commit" "$expected_commit" >&2
  exit 1
fi

if [[ "$(git rev-parse HEAD)" != "$expected_commit" ]]; then
  printf 'checked-out commit does not match the release commit\n' >&2
  exit 1
fi

git fetch --no-tags origin main:refs/remotes/origin/main
if ! git merge-base --is-ancestor "$expected_commit" refs/remotes/origin/main; then
  printf 'release tag %s is not reachable from origin/main\n' "$release_tag" >&2
  exit 1
fi
