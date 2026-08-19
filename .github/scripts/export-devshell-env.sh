#!/usr/bin/env bash

# Promote the `nix develop` environment to later workflow steps.
#
# Runs inside `nix develop --command`. Every variable the devshell sets or
# changes relative to the caller's environment (captured in the file named
# by $1) is appended to $GITHUB_ENV, so later steps see the same toolchain
# PATH and the same compiler-wrapper configuration (NIX_CFLAGS_COMPILE,
# NIX_LDFLAGS, SDK/framework paths on macOS, ...) that `nix develop` gives
# without wrapping each command. Shell-local bookkeeping and the derivation's
# lowercase build attributes stay out of the promoted set.
#
# Usage: nix develop --command bash .github/scripts/export-devshell-env.sh OUTER_ENV_FILE

set -euo pipefail

outer_env=${1:?usage: export-devshell-env.sh OUTER_ENV_FILE}
test -n "${GITHUB_ENV:-}"

skip_name() {
  case "$1" in
    HOME | PWD | OLDPWD | SHLVL | SHELL | TERM | TMPDIR | TEMPDIR | TEMP | TMP | \
    HOST_PATH | NIX_BUILD_TOP | NIX_BUILD_CORES | NIX_LOG_FD | NIX_REMOTE | \
    IN_NIX_SHELL | CI | _) return 0 ;;
    GITHUB_* | RUNNER_* | ACTIONS_*) return 0 ;;
  esac
  # Derivation build attributes (name, out, buildInputs, shellHook, ...) are
  # lowercase; exported configuration is uppercase.
  case "$1" in
    [A-Z]*) return 1 ;;
    *) return 0 ;;
  esac
}

promoted=0
while IFS= read -r -d '' entry; do
  name=${entry%%=*}
  value=${entry#*=}
  case "$name" in
    *[!A-Za-z0-9_]*) continue ;;
  esac
  if skip_name "$name"; then
    continue
  fi
  if grep -z -q -F -x -- "$entry" "$outer_env"; then
    continue
  fi
  delimiter="ANYTYPE_DEVSHELL_$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')"
  case "$value" in
    *"$delimiter"*)
      printf 'cannot promote %s: value contains the heredoc delimiter\n' "$name" >&2
      exit 1
      ;;
  esac
  printf '%s<<%s\n%s\n%s\n' "$name" "$delimiter" "$value" "$delimiter" >> "$GITHUB_ENV"
  promoted=$((promoted + 1))
done < <(env -0)

printf 'promoted %d devshell variables\n' "$promoted"
