#!/usr/bin/env bash

# Provision a disposable, sync-isolated headless Anytype server on a
# GitHub-hosted runner and mint ephemeral credentials for it.
#
# The pinned anytype-cli (expected on PATH, normally exposed from the Nix
# devshell) serves inside the outbound-blocking network namespace created by
# scripts/anytype-nonet, so test activity never reaches the Anytype network.
# Credentials are created with `anyr init-cli --save-env`, which verifies
# HTTP and gRPC authentication before saving. Nothing here outlives the
# runner: the account, keys, keystore, server state, and log are disposable.
#
# Usage: provision-headless-server.sh VAR_PREFIX
#
# Appends to $GITHUB_ENV:
#   <VAR_PREFIX>_ENV_FILE          sourceable env file with endpoints and keys
#   <VAR_PREFIX>_REDACTED_LOG_FILE server log path (stands in for the reviewed
#                                  log: with throwaway credentials on an
#                                  ephemeral runner there is nothing durable
#                                  to redact)
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

sudo apt-get update -qq
sudo apt-get install -y -qq nftables

umask 077
server_log="$RUNNER_TEMP/anytype-headless-server.log"
: > "$server_log"
anytype_bin="$(command -v anytype)" || {
  printf '%s\n' "anytype CLI is not on PATH (expose the Nix devshell first)" >&2
  exit 1
}

setsid env ANYTYPE_CLI_BIN="$anytype_bin" \
  bash scripts/anytype-nonet > "$server_log" 2>&1 < /dev/null &
# A fresh server has no account, and the HTTP API listener (31012) starts
# only after login, so first-boot readiness gates on the gRPC port instead.
up=""
for _ in $(seq 90); do
  if timeout 2 bash -c 'exec 3<>/dev/tcp/10.222.0.2/31010' 2>/dev/null; then
    up=1
    break
  fi
  sleep 2
done
if [[ -z "$up" ]]; then
  printf '%s\n' "headless server did not open 10.222.0.2:31010" >&2
  tail -c 4096 -- "$server_log" >&2
  exit 1
fi
# Give first-boot initialization a settling margin after the port opens.
sleep 20

anyr_bin="${ANYR_BIN:-}"
if [[ -z "$anyr_bin" ]]; then
  cargo build --locked -p anyr --bin anyr
  anyr_bin="$PWD/target/debug/anyr"
fi
if [[ ! -x "$anyr_bin" ]]; then
  printf '%s\n' "anyr binary is unavailable for init-cli" >&2
  exit 1
fi

# init-cli runs inside the namespace (as the runner user) because the server
# and the anytype CLI meet over namespace-local loopback there. Its account
# creation is what starts the HTTP API listener, and its single-shot
# verification can race that startup, so it gets a few bounded attempts
# (duplicate throwaway accounts on the disposable server are harmless).
env_file="$RUNNER_TEMP/headless-credentials.env"
keystore="$RUNNER_TEMP/headless-ci-keystore.db"
attempt=1
until sudo ip netns exec anycli_block runuser "$(whoami)" -c \
  "env ANYTYPE_CLI_BIN='$anytype_bin' \
    ANYTYPE_KEYSTORE='file:path=$keystore' \
    ANYTYPE_KEYSTORE_SERVICE=anyr \
    '$anyr_bin' init-cli --save-env '$env_file'"; do
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

# Account creation started the HTTP API listener; make sure the host-side
# gates can reach it through the namespace forward before handing over.
http_up=""
for _ in $(seq 30); do
  if timeout 2 bash -c 'exec 3<>/dev/tcp/10.222.0.2/31012' 2>/dev/null; then
    http_up=1
    break
  fi
  sleep 2
done
if [[ -z "$http_up" ]]; then
  printf '%s\n' "headless server did not open 10.222.0.2:31012 after login" >&2
  tail -c 4096 -- "$server_log" >&2
  exit 1
fi

# The gates connect from outside the namespace, where the server is reachable
# at the veth address instead of loopback.
sed -i 's|http://127\.0\.0\.1:|http://10.222.0.2:|g' "$env_file"
# shellcheck disable=SC2016 # the reference expands when the file is sourced
printf 'export ANYTYPE_TEST_URL="$ANYTYPE_URL"\n' >> "$env_file"

{
  printf '%s_ENV_FILE=%s\n' "$var_prefix" "$env_file"
  printf '%s_REDACTED_LOG_FILE=%s\n' "$var_prefix" "$server_log"
} >> "$GITHUB_ENV"
