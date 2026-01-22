#!/usr/bin/env bash
 
# This script initializes the headless cli, generates grpc and http credentials,
# and stores the credentials in the keystore.
#
# If all you need are http credentials, you don't need this script, as you can do the
# interactive authentication with the anytype crate, anyr, or any-edit.
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

#################
#   SETUP
#################
 
# 1. In the desktop app, create a space with "test" in the name, and get an invite
#    to the space (Settings, Members, Invite link. click to enable Editor access, Copy)
# 2. Paste the invite link below to set "space_invite=""
#    The value should look like "https://invite.any.coop/bafybei...#..."
#space_invite=""

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

# 5. Make sure the endpoint urls ANYTYPE_URL and ANYTYPE_GRPC_ENDPOINT,
#    and keystore are the same in this script and in the environment where
#    you'll run tools like anyr and any-edit.
#    (or use args `--url`, `--grpc`, and `--keystore`, respectively)
 
# 6. Make sure `any` (headless cli server) and `anyr` are in your PATH
# 

#################
#   END SETUP
#################

if [ -z "$space_invite" ]; then
  echo "set space_invite in the script"
  exit 1
fi

 
# it would be convenient if the cli auth commands supported json output, or even text output,
# but it doesn't, so we need to strip out the ascii art (box around the key)
extract_account_key() {
  sed -n 's/^║[[:space:]]*\([A-Za-z0-9+/=]\{20,\}\)[[:space:]]*║$/\1/p'
}

extract_http_token() {
  sed -n 's/Key: \([A-Za-z0-9+/=]\{20,\}\)[[:space:]]*$/\1/p'
}

init_cli_and_keystore() {

    tstamp=$(date '+%s')
    
    account_key=$(any auth create "bot_${tstamp}" 2>/dev/null | extract_account_key)
    if [ -z "$account_key" ]; then
      echo "acctount_key failed. exiting. Make sure headless server is running ('any service start' or 'any serve')"
      exit 1
    fi
      
    http_token=$(any auth apikey create "api_${tstamp}" | extract_http_token)
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

    any space join "$space_invite"
    sleep 2
    any space list

    sleep 4
    anyr space list -t
}

init_cli_and_keystore
join_space

# Remind the user to use the headless client for BOTH http and grpc.
echo "Don't forget to set ANYTYPE_URL=$ANYTYPE_URL"
