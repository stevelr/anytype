#!/usr/bin/env bash
# Run one bounded live driver in a uniquely named transient user scope.

set -euo pipefail

if [[ "$#" -lt 4 || "$3" != "--" ]]; then
  printf '%s\n' 'required live cgroup invocation failed' >&2
  exit 2
fi

mode="$1"
label="$2"
shift 3
case "$mode:$label" in
  command:auth | command:reset | test:direct | test:stdio | test:discussions) ;;
  *)
    printf '%s\n' 'required live cgroup invocation failed' >&2
    exit 2
    ;;
esac

if ! systemctl --user show-environment >/dev/null 2>&1; then
  printf '%s\n' 'required live cgroup manager unavailable' >&2
  exit 1
fi

unit_suffix=""
if ! unit_suffix="$(python3 -c 'import secrets; print(secrets.token_hex(8))' 2>/dev/null)"; then
  printf '%s\n' 'required live cgroup invocation failed' >&2
  exit 1
fi
if [[ ! "$unit_suffix" =~ ^[0-9a-f]{16}$ ]]; then
  printf '%s\n' 'required live cgroup invocation failed' >&2
  exit 1
fi
unit="any-mcp-${label}-${unit_suffix}.scope"

cleanup() {
  systemctl --user stop "$unit" >/dev/null 2>&1 || true
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

python3 any-mcp/scripts/run-live-gate.py "$mode" "$label" -- \
  systemd-run --user --scope --quiet --collect --same-dir \
  --unit="$unit" --property=RuntimeMaxSec=1100s -- "$@"
