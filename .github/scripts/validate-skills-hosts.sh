#!/usr/bin/env bash

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
repository_root=$(cd -- "$script_dir/../.." && pwd -P)
plugin_root=${1:-"$repository_root/skills"}

python3 "$script_dir/validate_skills_package.py" "$plugin_root"

if command -v skills-ref >/dev/null 2>&1; then
  for skill in "$plugin_root"/skills/*; do
    if [[ -d "$skill" ]]; then
      skills-ref validate "$skill"
    fi
  done
else
  printf '%s\n' 'skills-ref unavailable; skipped reference validator' >&2
fi

codex_root=${CODEX_HOME:-"${HOME}/.codex"}
codex_validator="$codex_root/skills/.system/plugin-creator/scripts/validate_plugin.py"
if [[ -f "$codex_validator" ]]; then
  python3 "$codex_validator" "$plugin_root"
else
  printf '%s\n' 'Codex plugin validator unavailable; skipped host validator' >&2
fi

if command -v claude >/dev/null 2>&1; then
  claude plugin validate "$plugin_root" --strict
else
  printf '%s\n' 'Claude Code unavailable; skipped strict plugin validator' >&2
fi
