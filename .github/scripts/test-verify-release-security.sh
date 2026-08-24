#!/usr/bin/env bash

set -euo pipefail

repository_root=$(cd "$(dirname "$0")/../.." && pwd)
script=$repository_root/.github/scripts/verify-release-security.sh
finalize_workflow=$repository_root/.github/workflows/finalize-release.yml
audit_workflow=$repository_root/.github/workflows/audit-release.yml
test_root=$(mktemp -d "${TMPDIR:-/tmp}/test-verify-release-security.XXXXXX")
trap 'rm -rf "$test_root"' EXIT

mock_bin=$test_root/bin
fixture_dir=$test_root/fixture
archive_stage=$test_root/archive-stage/anyr-aarch64-apple-darwin
mkdir -p "$mock_bin" "$fixture_dir" "$archive_stage"

release_tag=anyr-v0.5.3
repository=stevelr/anytype
source_commit=0123456789abcdef0123456789abcdef01234567
team_id=TESTTEAM01
signing_authority='Developer ID Application: Test Operator (TESTTEAM01)'

printf '#!/bin/sh\nprintf anyr\n' > "$archive_stage/anyr"
chmod 0755 "$archive_stage/anyr"
tar -cJf "$fixture_dir/anyr-aarch64-apple-darwin.tar.xz" \
  -C "$test_root/archive-stage" anyr-aarch64-apple-darwin
printf 'windows archive\n' > "$fixture_dir/anyr-x86_64-pc-windows-msvc.zip"
printf 'installer\n' > "$fixture_dir/anyr-installer.sh"

macos_hash=$(shasum -a 256 "$archive_stage/anyr" | awk '{print $1}')
jq -n \
  --arg repository "$repository" \
  --arg release_tag "$release_tag" \
  --arg source_commit "$source_commit" \
  --arg signed_hash "$macos_hash" \
  --arg team_id "$team_id" \
  --arg signing_authority "$signing_authority" '
    {
      schema_version: 1,
      repository: $repository,
      source_commit: $source_commit,
      release_tag: $release_tag,
      target: "aarch64-apple-darwin",
      signed_sha256: $signed_hash,
      notary_submission_id: "12345678-1234-1234-1234-123456789abc",
      team_id: $team_id,
      signing_authority: $signing_authority,
      identifier: "com.stevelr.anyr"
    }
  ' > "$fixture_dir/anyr-aarch64-apple-darwin.notarization.json"

for asset in \
  anyr-aarch64-apple-darwin.tar.xz \
  anyr-x86_64-pc-windows-msvc.zip
do
  asset_hash=$(shasum -a 256 "$fixture_dir/$asset" | awk '{print $1}')
  printf '%s *%s\n' "$asset_hash" "$asset" > "$fixture_dir/$asset.sha256"
done
(
  cd "$fixture_dir"
  shasum -a 256 \
    anyr-aarch64-apple-darwin.tar.xz \
    anyr-x86_64-pc-windows-msvc.zip > sha256.sum
)

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
done
exit 0
EOF

cat > "$mock_bin/spctl" <<'EOF'
#!/usr/bin/env bash
if [[ "${MOCK_SPCTL_MODE:-accepted}" == rejected ]]; then
  printf 'rejected: invalid notarization\n' >&2
  exit 3
fi
if [[ "${MOCK_SPCTL_MODE:-accepted}" == standalone ]]; then
  printf 'rejected: the code is valid but does not seem to be an app\n' >&2
  exit 3
fi
printf 'accepted\n'
EOF

cat > "$mock_bin/gh" <<'EOF'
#!/usr/bin/env bash
if [[ "$1 $2" != 'attestation verify' ]]; then
  printf 'unexpected gh invocation: %s\n' "$*" >&2
  exit 2
fi
printf '%s\n' "$*" >> "$MOCK_GH_LOG"
if [[ -n "${MOCK_ATTESTATION_FAIL:-}" && "$3" == *"$MOCK_ATTESTATION_FAIL" ]]; then
  exit 1
fi
EOF

chmod +x "$mock_bin"/*
export MOCK_GH_LOG=$test_root/gh.log

PATH="$mock_bin:$PATH" "$script" "$repository" "$release_tag" "$fixture_dir" "$team_id"
expected_assets=$(find "$fixture_dir" -maxdepth 1 -type f | wc -l | tr -d ' ')
test "$(wc -l < "$MOCK_GH_LOG" | tr -d ' ')" = "$expected_assets"
grep -q -- '--signer-workflow stevelr/anytype/.github/workflows/finalize-release.yml' "$MOCK_GH_LOG"
grep -q -- '--source-ref refs/tags/anyr-v0.5.3' "$MOCK_GH_LOG"
grep -q -- '--deny-self-hosted-runners' "$MOCK_GH_LOG"

MOCK_SPCTL_MODE=standalone PATH="$mock_bin:$PATH" \
  "$script" "$repository" "$release_tag" "$fixture_dir" "$team_id" >/dev/null

tampered_dir=$test_root/tampered
cp -R "$fixture_dir" "$tampered_dir"
printf 'tampered\n' >> "$tampered_dir/anyr-x86_64-pc-windows-msvc.zip"
if PATH="$mock_bin:$PATH" \
  "$script" "$repository" "$release_tag" "$tampered_dir" "$team_id" >/dev/null 2>&1
then
  printf 'tampered archive passed checksum verification\n' >&2
  exit 1
fi

if PATH="$mock_bin:$PATH" \
  "$script" "$repository" "$release_tag" "$fixture_dir" WRONGTEAM >/dev/null 2>&1
then
  printf 'incorrect Developer ID Team ID was accepted\n' >&2
  exit 1
fi

if MOCK_SPCTL_MODE=rejected PATH="$mock_bin:$PATH" \
  "$script" "$repository" "$release_tag" "$fixture_dir" "$team_id" >/dev/null 2>&1
then
  printf 'Gatekeeper rejection was accepted\n' >&2
  exit 1
fi

if MOCK_ATTESTATION_FAIL=anyr-installer.sh PATH="$mock_bin:$PATH" \
  "$script" "$repository" "$release_tag" "$fixture_dir" "$team_id" >/dev/null 2>&1
then
  printf 'missing artifact attestation was accepted\n' >&2
  exit 1
fi

grep -q 'attestations: write' "$finalize_workflow"
grep -q 'id-token: write' "$finalize_workflow"
grep -q 'actions/attest@1e69f48acb82d1966a394da916b4c1698aa569d6' "$finalize_workflow"
grep -q 'subject-path: artifacts/\*' "$finalize_workflow"
grep -q 'anyr-aarch64-apple-darwin.notarization.json' "$finalize_workflow"
grep -Fq "test \"\$GITHUB_REF\" = \"refs/tags/\$RELEASE_TAG\"" "$finalize_workflow"
grep -q 'cron:' "$audit_workflow"
grep -q 'runs-on: macos-15' "$audit_workflow"
grep -q 'verify-release-security.sh' "$audit_workflow"
