#!/usr/bin/env bash
 
# This script initializes the headless cli, generates grpc and http credentials,
# and stores the credentials in the keystore.
#
# If all you need are http credentials, you don't need this script, as you can do the
# interactive authentication with the anytype crate, anyr, or any-edit.
# 
# The anytype cli should be running and 'anytype' should be in your path. You can specify
# the executable path with ANYTYPE_CLI_BIN, e.g., "export ANYTYPE_CLI_BIN=anytype-cli/dist/anytype"
# 
# To generate grpc credentials for an external app, you need an account-key, which is printed
# on the terminal when the cli initializes (in response to "any auth create ...").
# This script runs the cli initialization, captures the account key, and saves it
# in the keystore; and runs "any apikey create ..." to generate an http token,
# and save that in the keystore. 
#
# For future grpc connections, the account_key is retrieved from the keystore and used
# to generate a session_token. For http connections, the http token is retrieved from the keystore.
# 
# While it is possible to use grpc with the desktop client, without the headless cli,
# the procedure is much more complicated, because (a) different key generation algorithm
# is used, and (b) the app uses a different random grpc port each time it starts.
#

set -euo pipefail

ANYTYPE_CLI_BIN="${ANYTYPE_CLI_BIN:-anytype}"

#################
#   SETUP
#################
 
# 1. In the desktop app, create a space with "test" in the name, and get an invite
#    to the space (Settings, Members, Invite link. click to enable Editor access, Copy)
# 2. Paste the invite link below to set "space_invite=""
#    The value should be in one of these formats:
#      - anytype app invite: `anytype://invite/?cid=<cid>&key-<key>`
#      - self-hosted networks: `http(s)://<any-host>/<cid>#<key>`
#      - anytype app invite (old?): `https://invite.any.coop/<cid>#<key>`
space_invite="${space_invite:=""}"

# 3. If running cargo tests anytype-api folder, uncomment these two lines:
#    They use a different service name and db path to isolate tests and prevent overwriting non-test data
#export ANYTYPE_KEYSTORE=file:path=$HOME/.local/state/anytype-test-keys.db
#export ANYTYPE_KEYSTORE_SERVICE=anytype_test

# 4. Set endpoints for the headless cli server
export ANYTYPE_URL="http://127.0.0.1:31012"
export ANYTYPE_GRPC_ENDPOINT=http://127.0.0.1:31010

# 4. (not recommended) for desktop, http is 31009 and grpc is random.
# export ANYTYPE_URL="http://127.0.0.1:31009"
# export ANYTYPE_GRPC_ENDPOINT=http://127.0.0.1:xxxxx

# 5 (optional) set name for the bot. This will be used for the user id for chat messages.
#   If not defined here, account will be named "bot_NNNN" where NNNN is a numeric timestamp.
#export ANY_USER="polly"
 
# 6. Make sure the endpoint urls ANYTYPE_URL and ANYTYPE_GRPC_ENDPOINT,
#    and keystore are the same in this script and in the environment where
#    you'll run tools like anyr and any-edit.
#    (or use args `--url`, `--grpc`, and `--keystore`, respectively)
 
# 7. Make sure your path contains
#  - `any` (aka `anytype`) (headless cli server)
#  - `anyr` (`https://github.com/stevelr/anytype/tree/main/anyr#install`)
#  - `jq`, `sed`
  

#################
#   END SETUP
#################

 
# it would be convenient if the cli auth commands supported json output, or even text output,
# but it doesn't, so we need to strip out the ascii art (box around the key)
extract_account_key() {
  sed -n 's/^║[[:space:]]*\([A-Za-z0-9+/=]\{20,\}\)[[:space:]]*║$/\1/p'
}

extract_http_token() {
  sed -n 's/Key: \([A-Za-z0-9+/=]\{20,\}\)[[:space:]]*$/\1/p'
}

init_cli_and_keystore() {
    
    # # if ~/.anytype/config.json has accountKey and sessionToken, we can import from there. Those creds are in config.json only if the server didn't use the os keyring
    # if [ "$(jq -r '.accountKey' <"$HOME"/.anytype/config.json)" != "null" ]; then
    #     anyr auth set-grpc  --config ~/.anytype/config.json
    #     "$ANYTYPE_CLI_BIN" auth apikey create http  | extract_http_token | anyr auth set-http
    #     return
    # fi

    # if user name not set, generate a unique bot name with timestamp
    tstamp=$(date '+%s')
    ANY_USER=${ANY_USER:-"bot_${tstamp}"}

    account_key=$("$ANYTYPE_CLI_BIN" auth create "$ANY_USER" 2>/dev/null | extract_account_key)
    if [ -z "$account_key" ]; then
      echo "acctount_key failed. exiting. Make sure headless server is running ('"$ANYTYPE_CLI_BIN" service start' or '"$ANYTYPE_CLI_BIN" serve')"
      exit 1
    fi
      
    http_token=$("$ANYTYPE_CLI_BIN" auth apikey create "api_${tstamp}" | extract_http_token)
    if [ -z "$http_token" ]; then
      echo "token failed"
      exit 1
    fi
    
    anyr auth set-http  <<<"$http_token"
    anyr auth set-grpc --account-key <<<"$account_key"
    anyr auth status
}

# headless cli - join space
join_space() {

    "$ANYTYPE_CLI_BIN" space join "$space_invite"
    sleep 2
    "$ANYTYPE_CLI_BIN" space list

    sleep 4
    anyr space list -t
}

init_cli_and_keystore

if [ -n "$space_invite" ]; then
  join_space
else
  echo "no invite code. skipping space join"
fi

# Remind the user to use the headless client for BOTH http and grpc.
echo "Don't forget to set ANYTYPE_URL=$ANYTYPE_URL"
