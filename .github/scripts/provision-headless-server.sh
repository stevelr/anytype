#!/usr/bin/env bash

# Provision a disposable headless Anytype server on a GitHub-hosted runner
# and mint ephemeral credentials for it.
#
# The anytype-cli selected by ANYTYPE_CLI_BIN, or `anytype` on PATH when it is
# unset, serves inside the outbound-blocking network namespace created by
# scripts/anytype-nonet. Set ANYTYPE_HEADLESS_NETWORK_MODE=connected only for
# a bounded gate whose API requires an Anytype network service.
# Credentials are created with `anyr init-cli --save-env`, which verifies
# HTTP and gRPC authentication before saving. Nothing here outlives the
# runner: the account, keys, keystore, server state, and log are disposable.
#
# Usage: provision-headless-server.sh VAR_PREFIX
#
# Appends to $GITHUB_ENV:
#   <VAR_PREFIX>_ENV_FILE          sourceable env file with endpoints and keys
#   <VAR_PREFIX>_REDACTED_LOG_FILE raw ephemeral server log path for bounded
#                                  descriptor-based test audits
#   <VAR_PREFIX>_REVIEWED_LOG_FILE content-free reviewed event stream for
#                                  failure evidence and strict log consumers
#
# If ANYR_BIN names an existing anyr executable it is used for init-cli;
# otherwise a debug anyr is built from the workspace.

set -euo pipefail

var_prefix="${1:?usage: provision-headless-server.sh VAR_PREFIX}"
case "$var_prefix" in
  *[!A-Z0-9_]*)
    printf '%s\n' "VAR_PREFIX must use only A-Z, 0-9, and underscores" >&2
    exit 2
    ;;
esac
network_mode="${ANYTYPE_HEADLESS_NETWORK_MODE:-isolated}"
case "$network_mode" in
  isolated | connected) ;;
  *)
    printf '%s\n' "ANYTYPE_HEADLESS_NETWORK_MODE must be isolated or connected" >&2
    exit 2
    ;;
esac

sudo apt-get update -qq
sudo apt-get install -y -qq nftables socat

umask 077
server_log="$RUNNER_TEMP/anytype-headless-server.log"
reviewed_log="$RUNNER_TEMP/anytype-headless-reviewed.log"
: > "$server_log"
: > "$reviewed_log"
setsid python3 any-mcp/scripts/review-server-log.py \
  "$server_log" "$reviewed_log" > /dev/null 2>&1 < /dev/null &
reviewer_pid=$!
ANYTYPE_CLI_BIN="${ANYTYPE_CLI_BIN:-anytype}"
anytype_bin="$(command -v -- "$ANYTYPE_CLI_BIN")" || {
  printf 'anytype CLI executable is unavailable: %s\n' "$ANYTYPE_CLI_BIN" >&2
  exit 1
}

server_host="10.222.0.2"
if [[ "$network_mode" == "isolated" ]]; then
  setsid env ANYTYPE_CLI_BIN="$anytype_bin" \
    bash scripts/anytype-nonet > "$server_log" 2>&1 < /dev/null &
else
  server_host="127.0.0.1"
  setsid "$anytype_bin" serve > "$server_log" 2>&1 < /dev/null &
fi
# A fresh server has no account, and the HTTP API listener (31012) starts
# only after login, so first-boot readiness gates on the gRPC port instead.
up=""
for _ in $(seq 90); do
  if timeout 2 bash -c "exec 3<>/dev/tcp/$server_host/31010" 2>/dev/null; then
    up=1
    break
  fi
  sleep 2
done
if [[ -z "$up" ]]; then
  printf '%s\n' "headless server did not open $server_host:31010" >&2
  tail -c 4096 -- "$server_log" >&2
  exit 1
fi
# Give first-boot initialization a settling margin after the port opens.
sleep 20

# The gate policies admit loopback endpoints only, so host loopback is
# bridged into the namespace instead of pointing the gates at the veth
# address. The forwarders die with the runner. Connected servers already
# listen on host loopback and do not need the bridge.
if [[ "$network_mode" == "isolated" ]]; then
  for port in 31010 31012; do
    setsid socat "TCP-LISTEN:$port,bind=127.0.0.1,fork,reuseaddr" \
      "TCP:10.222.0.2:$port" > /dev/null 2>&1 < /dev/null &
  done
fi

anyr_bin="${ANYR_BIN:-}"
if [[ -z "$anyr_bin" ]]; then
  cargo build --locked -p anyr --bin anyr
  anyr_bin="$PWD/target/debug/anyr"
fi
if [[ ! -x "$anyr_bin" ]]; then
  printf '%s\n' "anyr binary is unavailable for init-cli" >&2
  exit 1
fi

# In isolated mode, init-cli runs inside the namespace as the runner user.
# Its first-run account creation starts the HTTP API listener, and its
# single-shot verification can race that startup, so it gets a few bounded
# attempts. Later attempts reuse the account recorded in the CLI config.
env_file="$RUNNER_TEMP/headless-credentials.env"
keystore="$RUNNER_TEMP/headless-ci-keystore.db"
attempt=1
run_init_cli() {
  if [[ "$network_mode" == "isolated" ]]; then
    sudo ip netns exec anycli_block runuser "$(whoami)" -c \
      "env ANYTYPE_CLI_BIN='$anytype_bin' \
        ANYTYPE_KEYSTORE='file:path=$keystore' \
        ANYTYPE_KEYSTORE_SERVICE=anyr \
        '$anyr_bin' init-cli --save-env '$env_file'"
  else
    env ANYTYPE_CLI_BIN="$anytype_bin" \
      ANYTYPE_KEYSTORE="file:path=$keystore" \
      ANYTYPE_KEYSTORE_SERVICE=anyr \
      "$anyr_bin" init-cli --save-env "$env_file"
  fi
}
until run_init_cli; do
  if [[ "$attempt" -ge 3 ]]; then
    printf '%s\n' "init-cli failed after $attempt attempts" >&2
    tail -c 4096 -- "$server_log" >&2
    exit 1
  fi
  attempt=$((attempt + 1))
  rm -f -- "$env_file"
  sleep 10
done
if [[ ! -s "$env_file" ]]; then
  printf '%s\n' "init-cli produced no environment file" >&2
  exit 1
fi

# Account initialization started the HTTP API listener; make sure the host-side
# gates can reach it over loopback before handing over. The probe must see
# an HTTP response, not just a connect: the loopback forwarder accepts
# unconditionally even while the namespace-side listener is still absent.
http_up=""
for _ in $(seq 30); do
  if curl -so /dev/null --max-time 5 "http://127.0.0.1:31012/"; then
    http_up=1
    break
  fi
  sleep 2
done
if [[ -z "$http_up" ]]; then
  printf '%s\n' "headless server did not answer on 127.0.0.1:31012 after login" >&2
  tail -c 4096 -- "$server_log" >&2
  exit 1
fi
if ! kill -0 "$reviewer_pid" 2>/dev/null; then
  printf '%s\n' "headless server log reviewer stopped unexpectedly" >&2
  exit 1
fi

# The saved loopback endpoints reach either the connected server directly or
# the isolated server through the forwarders.
# shellcheck disable=SC2016 # the reference expands when the file is sourced
printf 'export ANYTYPE_TEST_URL="$ANYTYPE_URL"\n' >> "$env_file"

{
  printf '%s_ENV_FILE=%s\n' "$var_prefix" "$env_file"
  printf '%s_REDACTED_LOG_FILE=%s\n' "$var_prefix" "$server_log"
  printf '%s_REVIEWED_LOG_FILE=%s\n' "$var_prefix" "$reviewed_log"
} >> "$GITHUB_ENV"
