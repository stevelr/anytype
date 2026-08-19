#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 4 ]]; then
  printf 'usage: %s RUN_ROOT_PARENT NETNS_PREFIX BENCHMARK_BINARY CONFIG\n' "${0##*/}" >&2
  exit 2
fi

# The benchmark binary is the sole bootstrap allowed to inherit caller-open
# credentials. It marks the exact descriptor set close-on-exec before starting
# this launcher again, so ancillary helpers cannot inherit those descriptors.
if [[ ${ANY_MCP_BENCHMARK_FDS_ISOLATED:-} != 1 \
  || ${ANY_MCP_BENCHMARK_FD_SOURCE_PID:-} != "$PPID" \
  || ! /proc/$PPID/exe -ef $3 ]]; then
  if [[ $3 != /* || ! -x $3 ]]; then
    printf 'benchmark bootstrap must be an absolute executable\n' >&2
    exit 1
  fi
  exec "$3" launcher-bootstrap "$0" "$@"
fi

credential_source_pid=${ANY_MCP_BENCHMARK_FD_SOURCE_PID:-}
if [[ ! $credential_source_pid =~ ^[0-9]+$ ]]; then
  printf 'credential descriptor source is invalid\n' >&2
  exit 1
fi

run_parent=$(realpath -e -- "$1")
netns_prefix=$2
benchmark_binary=$(realpath -e -- "$3")
config=$(realpath -e -- "$4")

if [[ $(uname -s) != Linux ]]; then
  printf 'live benchmarks require Linux\n' >&2
  exit 1
fi
if [[ ! -d $run_parent || ! -x $benchmark_binary || ! -f $config ]]; then
  printf 'benchmark paths do not have the required types\n' >&2
  exit 1
fi
if [[ $(stat -c '%a' -- "$run_parent") != 700 ]]; then
  printf 'run-root parent must have mode 0700\n' >&2
  exit 1
fi
if [[ ! $netns_prefix =~ ^[A-Za-z0-9._-]{1,32}$ ]]; then
  printf 'network namespace prefix is invalid\n' >&2
  exit 1
fi

systemd_run=$(realpath -e -- "$(command -v systemd-run)")
systemctl_binary=$(realpath -e -- "$(command -v systemctl)")
timeout_binary=$(command -v timeout)
sudo_binary=$(realpath -e -- "$(command -v sudo)")
ip_binary=$(realpath -e -- "$(command -v ip)")
setpriv_binary=$(realpath -e -- "$(command -v setpriv)")
setsid_binary=$(realpath -e -- "$(command -v setsid)")
service_uid=$(id -u)
service_gid=$(id -g)
command -v od >/dev/null
command -v tr >/dev/null

nonce=$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')
if [[ ! $nonce =~ ^[0-9a-f]{32}$ ]]; then
  printf 'could not create the run nonce\n' >&2
  exit 1
fi
local_netns="${netns_prefix}-local-${nonce}"
upstream_netns="${netns_prefix}-upstream-${nonce}"
local_netns_created=false
upstream_netns_created=false
unit="any-mcp-benchmark-${nonce}.service"
unit_marker="any-mcp-benchmark-owned-${nonce}"
unit_requested=false
unit_owned=false
runner_pid=

# A process group still "exists" for kill -0 while any member is a zombie,
# and an orphaned descendant stays a zombie under a non-reaping init (for
# example a container without an init process). Zombies hold no descriptors
# and cannot run, so only runnable members count as live.
group_has_live_member() {
  group_pid=$1
  for stat_file in /proc/[0-9]*/stat; do
    read -r stat_line < "$stat_file" 2>/dev/null || continue
    # "pid (comm) state ppid pgrp ..."; comm may contain spaces or ')'.
    stat_line=${stat_line##*) }
    read -r member_state _ member_pgrp _ <<<"$stat_line"
    if [[ $member_pgrp == "$group_pid" && $member_state != Z ]]; then
      return 0
    fi
  done
  return 1
}

# Reap only after procfs proves the child is gone or a zombie, so wait cannot
# extend signal cleanup past the bounded poll.
bounded_reap() {
  reap_pid=$1
  reap_deadline=$2
  while (( SECONDS < reap_deadline )); do
    if [[ ! -r /proc/$reap_pid/stat ]]; then
      wait "$reap_pid" 2>/dev/null || true
      return 0
    fi
    process_state=$(awk '{print $3}' "/proc/$reap_pid/stat" 2>/dev/null || true)
    if [[ $process_state == Z ]]; then
      wait "$reap_pid" 2>/dev/null || true
      return 0
    fi
    sleep 0.05
  done
  return 1
}

stop_runner_tree() {
  stop_deadline=$1
  if [[ -z $runner_pid ]]; then
    return 0
  fi
  kill -TERM -- "-$runner_pid" 2>/dev/null || true
  term_deadline=$((SECONDS + 1))
  while (( SECONDS < term_deadline && SECONDS < stop_deadline )); do
    if ! kill -0 -- "-$runner_pid" 2>/dev/null || ! group_has_live_member "$runner_pid"; then
      break
    fi
    sleep 0.05
  done
  if kill -0 -- "-$runner_pid" 2>/dev/null; then
    kill -KILL -- "-$runner_pid" 2>/dev/null || true
  fi
  if ! bounded_reap "$runner_pid" "$stop_deadline"; then
    return 1
  fi
  while kill -0 -- "-$runner_pid" 2>/dev/null && group_has_live_member "$runner_pid"; do
    if (( SECONDS >= stop_deadline )); then
      return 1
    fi
    kill -KILL -- "-$runner_pid" 2>/dev/null || true
    sleep 0.05
  done
  runner_pid=
}

resolve_unit_ownership() {
  resolution_deadline=$1
  unit_resolution=uncertain
  while (( SECONDS < resolution_deadline )); do
    if load_state=$("$timeout_binary" 1s "$systemctl_binary" --user show "$unit" \
      --property=LoadState --value 2>/dev/null); then
      if [[ $load_state == not-found ]]; then
        unit_resolution=absent
      elif description=$("$timeout_binary" 1s "$systemctl_binary" --user show "$unit" \
        --property=Description --value 2>/dev/null); then
        if [[ $description == "$unit_marker" ]]; then
          unit_resolution=owned
          unit_owned=true
          return 0
        fi
        unit_resolution=foreign
        return 1
      else
        unit_resolution=uncertain
      fi
    else
      unit_resolution=uncertain
    fi
    sleep 0.05
  done
  [[ $unit_resolution == absent ]]
}

confirm_unit_stopped() {
  stopped_deadline=$1
  while (( SECONDS < stopped_deadline )); do
    if load_state=$("$timeout_binary" 1s "$systemctl_binary" --user show "$unit" \
      --property=LoadState --value 2>/dev/null); then
      if [[ $load_state == not-found ]]; then
        return 0
      fi
      if active_state=$("$timeout_binary" 1s "$systemctl_binary" --user show "$unit" \
        --property=ActiveState --value 2>/dev/null); then
        if [[ $active_state == inactive || $active_state == failed ]]; then
          return 0
        fi
      fi
    fi
    sleep 0.05
  done
  return 1
}

# shellcheck disable=SC2329
cleanup() {
  exit_status=${cleanup_status:-$?}
  trap - EXIT INT TERM
  cleanup_deadline=$((SECONDS + 8))
  cleanup_certain=true
  if ! stop_runner_tree "$cleanup_deadline"; then
    printf 'benchmark service launcher tree did not stop before cleanup deadline\n' >&2
    exit_status=1
    cleanup_certain=false
  fi
  if [[ $unit_requested == true && $unit_owned != true ]]; then
    if ! resolve_unit_ownership "$cleanup_deadline"; then
      printf 'cannot prove benchmark service ownership or absence\n' >&2
      exit_status=1
      cleanup_certain=false
    fi
  fi
  if [[ $unit_owned == true ]]; then
    if ! "$timeout_binary" 2s "$systemctl_binary" --user stop "$unit"; then
      exit_status=1
      cleanup_certain=false
    elif ! confirm_unit_stopped "$cleanup_deadline"; then
      printf 'owned benchmark service did not stop before cleanup\n' >&2
      exit_status=1
      cleanup_certain=false
    fi
  fi
  if [[ $cleanup_certain != true ]]; then
    printf 'namespace cleanup withheld because service teardown is uncertain\n' >&2
    exit "$exit_status"
  fi
  if [[ $local_netns_created == true ]]; then
    if ! "$timeout_binary" 5s "$sudo_binary" -n -- \
      "$ip_binary" netns delete "$local_netns"; then
      printf 'cannot delete owned local benchmark namespace\n' >&2
      exit_status=1
    fi
  fi
  if [[ $upstream_netns_created == true ]]; then
    if ! "$timeout_binary" 5s "$sudo_binary" -n -- \
      "$ip_binary" netns delete "$upstream_netns"; then
      printf 'cannot delete owned upstream benchmark namespace\n' >&2
      exit_status=1
    fi
  fi
  exit "$exit_status"
}
trap cleanup EXIT
trap 'cleanup_status=130; cleanup' INT
trap 'cleanup_status=143; cleanup' TERM

"$timeout_binary" 5s "$sudo_binary" -n -- "$ip_binary" netns add "$local_netns"
local_netns_created=true
"$timeout_binary" 5s "$sudo_binary" -n -- \
  "$ip_binary" -n "$local_netns" link set lo up
"$timeout_binary" 5s "$sudo_binary" -n -- "$ip_binary" netns add "$upstream_netns"
upstream_netns_created=true
"$timeout_binary" 5s "$sudo_binary" -n -- \
  "$ip_binary" -n "$upstream_netns" link set lo up
for namespace in "$local_netns" "$upstream_netns"; do
  if ! route_output=$("$timeout_binary" 5s "$sudo_binary" -n -- \
    "$ip_binary" -n "$namespace" route show default); then
    printf 'cannot prove arm namespace route isolation\n' >&2
    exit 1
  fi
  if [[ -n $route_output ]]; then
    printf 'arm namespace unexpectedly has an outbound route\n' >&2
    exit 1
  fi
done

umask 077
run_root=$(mktemp -d -- "$run_parent/any-mcp-benchmark.XXXXXXXX")
chmod 0700 -- "$run_root"
printf 'any-mcp-benchmark protected run root v1 %s\n' "$nonce" \
  >"$run_root/.any-mcp-benchmark-run-v1"
chmod 0600 -- "$run_root/.any-mcp-benchmark-run-v1"

credential_fds=${ANY_MCP_BENCHMARK_CREDENTIAL_FDS:-}
if [[ -z $credential_fds ]]; then
  printf 'ANY_MCP_BENCHMARK_CREDENTIAL_FDS must name inherited descriptors\n' >&2
  exit 1
fi
IFS=',' read -r -a inherited_fds <<<"$credential_fds"
if (( ${#inherited_fds[@]} == 0 || ${#inherited_fds[@]} > 8 )); then
  printf 'credential descriptor count is invalid\n' >&2
  exit 1
fi
open_file_properties=()
declare -A observed_descriptors=()
for index in "${!inherited_fds[@]}"; do
  descriptor=${inherited_fds[$index]}
  if [[ ! $descriptor =~ ^[0-9]+$ ]] || (( descriptor < 3 || descriptor > 64 )); then
    printf 'credential descriptor is invalid\n' >&2
    exit 1
  fi
  if [[ -n ${observed_descriptors[$descriptor]:-} ]]; then
    printf 'credential descriptors must be unique\n' >&2
    exit 1
  fi
  observed_descriptors[$descriptor]=1
  if [[ ! -r /proc/$credential_source_pid/fd/$descriptor ]]; then
    printf 'credential descriptor is not readable\n' >&2
    exit 1
  fi
  open_file_properties+=(
    "--property=OpenFile=/proc/$credential_source_pid/fd/$descriptor:benchmark-credential-$index:read-only"
  )
done

unit_requested=true
"$setsid_binary" "$systemd_run" --user --wait --pipe --collect --service-type=exec \
  --unit="$unit" \
  --property=Description="$unit_marker" \
  --property=KillMode=control-group \
  --property=RuntimeMaxSec=8h \
  "${open_file_properties[@]}" \
  --setenv=ANY_MCP_BENCHMARK_SUPERVISOR=systemd-cgroup-netns-v1 \
  --setenv=ANY_MCP_BENCHMARK_RUN_NONCE="$nonce" \
  --setenv=ANY_MCP_BENCHMARK_UNIT="$unit" \
  --setenv=ANY_MCP_BENCHMARK_LOCAL_NETNS="$local_netns" \
  --setenv=ANY_MCP_BENCHMARK_UPSTREAM_NETNS="$upstream_netns" \
  --setenv=ANY_MCP_BENCHMARK_CREDENTIAL_COUNT="${#inherited_fds[@]}" \
  --setenv=ANY_MCP_BENCHMARK_SUDO="$sudo_binary" \
  --setenv=ANY_MCP_BENCHMARK_IP="$ip_binary" \
  --setenv=ANY_MCP_BENCHMARK_SETPRIV="$setpriv_binary" \
  --setenv=ANY_MCP_BENCHMARK_SERVICE_UID="$service_uid" \
  --setenv=ANY_MCP_BENCHMARK_SERVICE_GID="$service_gid" \
  -- "$benchmark_binary" supervise "$run_root" "$config" \
  "$local_netns" "$upstream_netns" &
runner_pid=$!

registration_deadline=$((SECONDS + 5))
while (( SECONDS < registration_deadline )); do
  description=$(
    "$timeout_binary" 1s "$systemctl_binary" --user show "$unit" \
      --property=Description --value 2>/dev/null || true
  )
  if [[ $description == "$unit_marker" ]]; then
    unit_owned=true
    break
  fi
  if ! kill -0 "$runner_pid" 2>/dev/null; then
    break
  fi
  sleep 0.05
done

if [[ $unit_owned != true ]]; then
  printf 'benchmark service did not register before its deadline\n' >&2
  cleanup_status=1
  cleanup
fi

while kill -0 "$runner_pid" 2>/dev/null; do
  sleep 0.05
done
if wait "$runner_pid"; then
  runner_status=0
else
  runner_status=$?
fi
runner_pid=
if (( runner_status == 0 )); then
  unit_owned=false
fi
cleanup_status=$runner_status
cleanup
