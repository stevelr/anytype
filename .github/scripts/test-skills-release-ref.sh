#!/usr/bin/env bash

set -euo pipefail

validator=$PWD/.github/scripts/validate-skills-release-ref.sh
test_root=$(mktemp -d "${TMPDIR:-/tmp}/skills-release-ref-test.XXXXXX")
trap 'rm -rf "$test_root"' EXIT

git init --bare --initial-branch=main "$test_root/remote.git" >/dev/null
git clone "$test_root/remote.git" "$test_root/repo" >/dev/null 2>&1
(
  cd "$test_root/repo"
  git config user.name 'Skills Release Test'
  git config user.email 'skills-release@example.invalid'
  printf 'main\n' > content.txt
  git add content.txt
  git commit -m initial >/dev/null
  main_commit=$(git rev-parse HEAD)
  git push origin main >/dev/null 2>&1

  git tag anytype-toolbox-skills-v1.2.3
  git push origin anytype-toolbox-skills-v1.2.3 >/dev/null 2>&1
  bash "$validator" anytype-toolbox-skills-v1.2.3 "$main_commit"

  git tag -a anytype-toolbox-skills-v2.0.0-rc.1 -m prerelease
  git push origin anytype-toolbox-skills-v2.0.0-rc.1 >/dev/null 2>&1
  bash "$validator" anytype-toolbox-skills-v2.0.0-rc.1 "$main_commit"

  for invalid in \
    anyr-v1.2.3 \
    anytype-toolbox-skills-v1.2 \
    anytype-toolbox-skills-v01.2.3 \
    anytype-toolbox-skills-v1.2.3-01
  do
    if bash "$validator" "$invalid" "$main_commit" >/dev/null 2>&1; then
      printf 'invalid skills release tag was accepted: %s\n' "$invalid" >&2
      exit 1
    fi
  done

  wrong_commit=0000000000000000000000000000000000000000
  if bash "$validator" anytype-toolbox-skills-v1.2.3 "$wrong_commit" >/dev/null 2>&1; then
    printf 'remote tag mismatch was accepted\n' >&2
    exit 1
  fi
  if bash "$validator" anytype-toolbox-skills-v9.9.9 "$main_commit" >/dev/null 2>&1; then
    printf 'missing remote tag was accepted\n' >&2
    exit 1
  fi

  git switch -c unmerged >/dev/null 2>&1
  printf 'unmerged\n' >> content.txt
  git commit -am unmerged >/dev/null
  unmerged_commit=$(git rev-parse HEAD)
  git tag anytype-toolbox-skills-v3.0.0
  git push origin anytype-toolbox-skills-v3.0.0 >/dev/null 2>&1
  if bash "$validator" anytype-toolbox-skills-v3.0.0 "$unmerged_commit" >/dev/null 2>&1; then
    printf 'tag outside origin/main was accepted\n' >&2
    exit 1
  fi
)
