#!/usr/bin/env bash

set -euo pipefail

repository_root=$(cd "$(dirname "$0")/../.." && pwd)
script=$repository_root/.github/scripts/sign-macos-release.sh
test_root=$(mktemp -d "${TMPDIR:-/tmp}/test-sign-macos-release.XXXXXX")
trap 'rm -rf "$test_root"' EXIT

mock_bin=$test_root/bin
fixture_dir=$test_root/fixture
uploaded_dir=$test_root/uploaded
mkdir -p "$mock_bin" "$fixture_dir" "$uploaded_dir"

source_commit=0123456789abcdef0123456789abcdef01234567
release_tag=anyr-v9.9.9-fixture.1
source_run_id=999999999
candidate_run_id=888888888
team_id=TESTTEAM01
printf '#!/bin/sh\nprintf anyr\n' > "$fixture_dir/anyr"
chmod 0755 "$fixture_dir/anyr"
binary_hash=$(python3 - "$fixture_dir/anyr" <<'PY'
import hashlib
import pathlib
import sys

print(hashlib.sha256(pathlib.Path(sys.argv[1]).read_bytes()).hexdigest())
PY
)
jq -n \
  --argjson source_run_id "$source_run_id" \
  --argjson candidate_run_id "$candidate_run_id" \
  --arg source_commit "$source_commit" \
  --arg release_tag "$release_tag" \
  --arg binary_sha256 "$binary_hash" \
  '{
    schema_version: 1,
    repository: "stevelr/anytype",
    source_run_id: $source_run_id,
    candidate_run_id: $candidate_run_id,
    source_commit: $source_commit,
    release_tag: $release_tag,
    target: "aarch64-apple-darwin",
    binary_sha256: $binary_sha256,
    flake_lock_sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
  }' > "$fixture_dir/manifest.json"

cat > "$mock_bin/uname" <<'EOF'
#!/usr/bin/env bash
printf 'Darwin\n'
EOF

cat > "$mock_bin/security" <<'EOF'
#!/usr/bin/env bash
if [[ "$1 $2 $3" == 'default-keychain -d user' ]]; then
  printf '    "/Users/test/Library/Keychains/login.keychain-db"\n'
  exit 0
fi
exit 2
EOF

cat > "$mock_bin/otool" <<'EOF'
#!/usr/bin/env bash
printf '%s:\n\t/usr/lib/libSystem.B.dylib\n' "${@: -1}"
EOF

cat > "$mock_bin/codesign" <<'EOF'
#!/usr/bin/env bash
for argument in "$@"; do
  if [[ "$argument" == --display ]]; then
    {
      printf 'Identifier=com.stevelr.anyr\n'
      printf 'Authority=Developer ID Application: Test Operator (TESTTEAM01)\n'
      printf 'TeamIdentifier=TESTTEAM01\n'
    } >&2
    exit 0
  fi
  if [[ "$argument" == --force ]]; then
    printf 'developer-signature' >> "${@: -1}"
    exit 0
  fi
done
exit 0
EOF

cat > "$mock_bin/ditto" <<'EOF'
#!/usr/bin/env bash
printf 'notary archive' > "${@: -1}"
EOF

cat > "$mock_bin/spctl" <<'EOF'
#!/usr/bin/env bash
case "${MOCK_SPCTL_MODE:-notarized-cli}" in
  accepted)
    printf '%s: accepted\n' "${@: -1}"
    ;;
  notarized-cli)
    printf '%s: rejected (the code is valid but does not seem to be an app)\n' "${@: -1}" >&2
    exit 3
    ;;
  *)
    printf '%s: rejected\n' "${@: -1}" >&2
    exit 3
    ;;
esac
EOF

cat > "$mock_bin/xcrun" <<'EOF'
#!/usr/bin/env bash
if [[ "$1 $2" == 'notarytool submit' ]]; then
  printf '{"id":"00000000-0000-4000-8000-000000000000","status":"Accepted"}\n'
  exit 0
fi
if [[ "$1 $2" == 'notarytool log' ]]; then
  printf '{"status":"Accepted","issues":[]}\n' > "${@: -1}"
  exit 0
fi
exit 2
EOF

cat > "$mock_bin/shasum" <<'EOF'
#!/usr/bin/env python3
import hashlib
import pathlib
import sys

if len(sys.argv) != 4 or sys.argv[1:3] != ["-a", "256"]:
    print("unexpected shasum invocation", file=sys.stderr)
    raise SystemExit(2)
path = pathlib.Path(sys.argv[3])
print(f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path}")
EOF

cat > "$mock_bin/gh" <<'EOF'
#!/usr/bin/env bash
case "$1 $2" in
  'repo view')
    printf 'stevelr/anytype\n'
    ;;
  'variable get')
    printf '%s\n' "$MOCK_TEAM_ID"
    ;;
  'api repos/stevelr/anytype/actions/runs/999999999')
    jq -n --arg sha "$MOCK_SOURCE_COMMIT" '{
      conclusion: "success",
      event: "push",
      path: ".github/workflows/release.yml",
      head_sha: $sha
    }'
    ;;
  'api repos/stevelr/anytype/commits/anyr-v9.9.9-fixture.1')
    printf '%s\n' "$MOCK_SOURCE_COMMIT"
    ;;
  'run download')
    destination=
    while [[ "$#" -gt 0 ]]; do
      if [[ "$1" == --dir ]]; then
        destination=$2
        break
      fi
      shift
    done
    cp "$MOCK_FIXTURE_DIR/anyr" "$destination/anyr"
    cp "$MOCK_FIXTURE_DIR/manifest.json" "$destination/manifest.json"
    ;;
  'release view')
    [[ -f "$MOCK_STATE_DIR/draft" ]] || exit 1
    printf 'true\n'
    ;;
  'release create')
    touch "$MOCK_STATE_DIR/draft"
    printf 'draft created\n'
    ;;
  'release upload')
    printf '%s\n' "$*" >> "$MOCK_LOG"
    for argument in "$@"; do
      if [[ -f "$argument" ]]; then
        cp "$argument" "$MOCK_UPLOADED_DIR/"
      fi
    done
    ;;
  'workflow run')
    printf '%s\n' "$*" >> "$MOCK_LOG"
    ;;
  *)
    printf 'unexpected gh invocation: %s\n' "$*" >&2
    exit 2
    ;;
esac
EOF

chmod +x "$mock_bin"/*
export MOCK_FIXTURE_DIR=$fixture_dir
export MOCK_LOG=$test_root/gh.log
export MOCK_SOURCE_COMMIT=$source_commit
export MOCK_STATE_DIR=$test_root
export MOCK_TEAM_ID=$team_id
export MOCK_UPLOADED_DIR=$uploaded_dir

command_log=$test_root/sign-command.log
if ! PATH="$mock_bin:$PATH" "$script" \
  --run-id "$source_run_id" \
  --identity "Developer ID Application: Test Operator (TESTTEAM01)" \
  --notary-profile anyr-notary \
  --repo stevelr/anytype >"$command_log" 2>&1
then
  tail -c 8192 -- "$command_log" >&2
  exit 1
fi
grep -Fq 'rejected (the code is valid but does not seem to be an app)' "$command_log"
grep -Fq 'notarization submission: 00000000-0000-4000-8000-000000000000' "$command_log"
grep -Fq "finalization dispatched for $release_tag from run $source_run_id" "$command_log"

test -f "$uploaded_dir/anyr-aarch64-apple-darwin.signed"
test -f "$uploaded_dir/anyr-aarch64-apple-darwin.signed.json"
jq -e \
  --arg source_commit "$source_commit" \
  --arg release_tag "$release_tag" \
  --argjson candidate_run_id "$candidate_run_id" \
  --arg team_id "$team_id" '
    .source_commit == $source_commit and
    .release_tag == $release_tag and
    .candidate_run_id == $candidate_run_id and
    .team_id == $team_id and
    .signing_authority == "Developer ID Application: Test Operator (TESTTEAM01)" and
    .identifier == "com.stevelr.anyr"
  ' "$uploaded_dir/anyr-aarch64-apple-darwin.signed.json" >/dev/null
grep -q 'release upload' "$MOCK_LOG"
grep -q -- 'workflow run finalize-release.yml --repo stevelr/anytype --ref anyr-v9.9.9-fixture.1' "$MOCK_LOG"

if PATH="$mock_bin:$PATH" "$script" \
  --run-id invalid \
  --identity test \
  --notary-profile test >/dev/null 2>&1
then
  printf 'invalid run ID was accepted\n' >&2
  exit 1
fi

if MOCK_SPCTL_MODE=rejected PATH="$mock_bin:$PATH" "$script" \
  --run-id "$source_run_id" \
  --identity "Developer ID Application: Test Operator (TESTTEAM01)" \
  --notary-profile anyr-notary \
  --repo stevelr/anytype >/dev/null 2>&1
then
  printf 'a genuine Gatekeeper rejection was accepted\n' >&2
  exit 1
fi

printf 'validated mocked macOS signing, notarization, and finalize dispatch\n'
