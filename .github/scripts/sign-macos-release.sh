#!/usr/bin/env bash

set -euo pipefail

usage() {
  cat <<'EOF'
Usage: sign-macos-release.sh --run-id ID --identity IDENTITY --notary-profile PROFILE [OPTIONS]

Download a macOS Nix release input, verify it, sign and notarize it locally,
upload the signed handoff to a draft GitHub Release, and dispatch the release
finalization workflow.

Required:
  --run-id ID              Successful tag-triggered release.yml workflow run
  --identity IDENTITY      Developer ID Application common name or SHA-1 hash
  --notary-profile NAME    notarytool keychain profile created with store-credentials

Options:
  --identifier ID          Code-signing identifier (default: com.stevelr.anyr)
  --keychain PATH          Keychain containing the identity (default: user default)
  --repo OWNER/REPO        GitHub repository (default: repository for this checkout)
  --keep-work-dir          Keep downloaded and generated files after completion
  -h, --help               Show this help

The GitHub repository variable MACOS_DEVELOPER_TEAM_ID must contain the Team ID
expected by finalize-release.yml. The private key and notary credentials remain
in the local macOS keychain and are never uploaded.
EOF
}

die() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || die "required command is unavailable: $1"
}

sha256_of() {
  shasum -a 256 "$1" | awk '{print $1}'
}

run_id=
identity=
notary_profile=
identifier=com.stevelr.anyr
repository=
keychain_path=
keep_work_dir=false

while [[ "$#" -gt 0 ]]; do
  case "$1" in
    --run-id)
      [[ "$#" -ge 2 ]] || die '--run-id requires a value'
      run_id=$2
      shift 2
      ;;
    --identity)
      [[ "$#" -ge 2 ]] || die '--identity requires a value'
      identity=$2
      shift 2
      ;;
    --notary-profile)
      [[ "$#" -ge 2 ]] || die '--notary-profile requires a value'
      notary_profile=$2
      shift 2
      ;;
    --identifier)
      [[ "$#" -ge 2 ]] || die '--identifier requires a value'
      identifier=$2
      shift 2
      ;;
    --repo)
      [[ "$#" -ge 2 ]] || die '--repo requires a value'
      repository=$2
      shift 2
      ;;
    --keychain)
      [[ "$#" -ge 2 ]] || die '--keychain requires a value'
      keychain_path=$2
      shift 2
      ;;
    --keep-work-dir)
      keep_work_dir=true
      shift
      ;;
    -h | --help)
      usage
      exit 0
      ;;
    *) die "unknown argument: $1" ;;
  esac
done

[[ "$run_id" =~ ^[1-9][0-9]*$ ]] || die '--run-id must be a positive integer'
[[ -n "$identity" ]] || die '--identity is required'
[[ -n "$notary_profile" ]] || die '--notary-profile is required'
[[ "$identifier" =~ ^[A-Za-z0-9][A-Za-z0-9.-]*$ ]] || die '--identifier is invalid'

for command_name in codesign ditto gh jq otool security shasum spctl xcrun; do
  require_command "$command_name"
done
[[ "$(uname -s)" == Darwin ]] || die 'this command must run on macOS'

if [[ -z "$keychain_path" ]]; then
  keychain_path=$(security default-keychain -d user | sed 's/^[[:space:]]*"//; s/"[[:space:]]*$//')
fi
[[ -n "$keychain_path" ]] || die 'could not determine the signing keychain'

if [[ -z "$repository" ]]; then
  repository=$(gh repo view --json nameWithOwner --jq .nameWithOwner)
fi
[[ "$repository" =~ ^[^/]+/[^/]+$ ]] || die "invalid GitHub repository: $repository"

expected_team_id=$(gh variable get MACOS_DEVELOPER_TEAM_ID --repo "$repository" 2>/dev/null || true)
if [[ -z "$expected_team_id" ]]; then
  die "GitHub variable MACOS_DEVELOPER_TEAM_ID is not set for $repository"
fi

work_dir=$(mktemp -d "${TMPDIR:-/tmp}/anyr-macos-sign.XXXXXX")
cleanup() {
  if "$keep_work_dir"; then
    printf 'kept work directory: %s\n' "$work_dir" >&2
  else
    rm -rf "$work_dir"
  fi
}
trap cleanup EXIT

run_json="$work_dir/run.json"
gh api "repos/$repository/actions/runs/$run_id" > "$run_json"

[[ "$(jq -r '.conclusion // ""' "$run_json")" == success ]] || die "workflow run $run_id did not succeed"
[[ "$(jq -r '.event // ""' "$run_json")" == push ]] || die "workflow run $run_id was not triggered by a release-tag push"
[[ "$(jq -r '.path // ""' "$run_json")" == .github/workflows/release.yml ]] || die "workflow run $run_id is not release.yml"
source_commit=$(jq -r '.head_sha // ""' "$run_json")
[[ "$source_commit" =~ ^[0-9a-f]{40}$ ]] || die 'source workflow has an invalid commit SHA'

input_dir="$work_dir/input"
mkdir -p "$input_dir"
gh run download "$run_id" --repo "$repository" --name macos-signing-input --dir "$input_dir"

input_manifest="$input_dir/manifest.json"
source_binary="$input_dir/anyr"
[[ -f "$input_manifest" ]] || die 'macOS signing-input manifest is missing'
[[ -f "$source_binary" ]] || die 'macOS signing-input binary is missing'

jq -e \
  --arg repository "$repository" \
  --arg run_id "$run_id" \
  --arg commit "$source_commit" \
  '
    .schema_version == 2 and
    .repository == $repository and
    (.source_run_id | tostring) == $run_id and
    (.candidate_run_id | type == "number" and . > 0) and
    .source_commit == $commit and
    .target == "aarch64-apple-darwin" and
    (.release_tag | type == "string" and length > 0) and
    (.binary_sha256 | test("^[0-9a-f]{64}$"))
  ' "$input_manifest" >/dev/null || die 'macOS signing-input manifest failed validation'

release_tag=$(jq -r .release_tag "$input_manifest")
candidate_run_id=$(jq -r .candidate_run_id "$input_manifest")
tag_commit=$(gh api "repos/$repository/commits/$release_tag" --jq .sha)
[[ "$tag_commit" == "$source_commit" ]] || die "release tag $release_tag does not resolve to source commit $source_commit"

expected_binary_hash=$(jq -r .binary_sha256 "$input_manifest")
actual_binary_hash=$(sha256_of "$source_binary")
[[ "$actual_binary_hash" == "$expected_binary_hash" ]] || die 'downloaded Nix binary failed its SHA-256 check'

chmod 0755 "$source_binary"
codesign --verify --strict --verbose=2 "$source_binary"
if otool -L "$source_binary" | grep -q '/nix/store'; then
  die 'downloaded Nix binary references a /nix/store library'
fi

signed_dir="$work_dir/signed"
mkdir -p "$signed_dir"
signed_binary="$signed_dir/anyr"
cp "$source_binary" "$signed_binary"
chmod 0755 "$signed_binary"

codesign \
  --force \
  --keychain "$keychain_path" \
  --sign "$identity" \
  --identifier "$identifier" \
  --options runtime \
  --timestamp \
  "$signed_binary"
codesign --verify --strict --verbose=2 "$signed_binary"

signature_details="$work_dir/codesign-details.txt"
codesign --display --verbose=4 "$signed_binary" 2> "$signature_details"
team_id=$(sed -n 's/^TeamIdentifier=//p' "$signature_details" | head -n 1)
signing_authority=$(sed -n 's/^Authority=//p' "$signature_details" | head -n 1)
signed_identifier=$(sed -n 's/^Identifier=//p' "$signature_details" | head -n 1)
[[ "$team_id" == "$expected_team_id" ]] || die "signature Team ID $team_id does not match MACOS_DEVELOPER_TEAM_ID"
[[ "$signing_authority" == 'Developer ID Application:'* ]] || die 'binary was not signed by a Developer ID Application identity'
[[ "$signed_identifier" == "$identifier" ]] || die 'signed binary has an unexpected identifier'

notary_archive="$work_dir/anyr-notarization.zip"
notary_result="$work_dir/notary-submit.json"
notary_log="$work_dir/notary-log.json"
ditto -c -k --keepParent "$signed_binary" "$notary_archive"
xcrun notarytool submit "$notary_archive" \
  --keychain-profile "$notary_profile" \
  --wait \
  --output-format json > "$notary_result"
[[ "$(jq -r '.status // ""' "$notary_result")" == Accepted ]] || die 'Apple notarization did not return Accepted'
notary_submission_id=$(jq -r '.id // ""' "$notary_result")
[[ "$notary_submission_id" =~ ^[0-9A-Fa-f-]{36}$ ]] || die 'notarytool returned an invalid submission ID'
xcrun notarytool log "$notary_submission_id" \
  --keychain-profile "$notary_profile" \
  "$notary_log"
jq -e '.status == "Accepted" and ((.issues // []) | length == 0)' "$notary_log" >/dev/null \
  || die 'the notarization log reports a non-Accepted status or open issues'

# macOS 15+ spctl refuses to assess standalone executables: it exits nonzero
# with "the code is valid but does not seem to be an app" even for a correctly
# signed and notarized binary. Accept exactly that verdict; fail on any other
# rejection.
assessment_status=0
assessment=$(spctl --assess --type execute --verbose=4 "$signed_binary" 2>&1) || assessment_status=$?
if [[ "$assessment_status" -ne 0 ]]; then
  printf '%s\n' "$assessment" >&2
  [[ "$assessment" == *'the code is valid but does not seem to be an app'* ]] \
    || die 'Gatekeeper assessment rejected the signed binary'
fi

signed_binary_hash=$(sha256_of "$signed_binary")
signed_asset="$work_dir/anyr-aarch64-apple-darwin.signed"
handoff_manifest="$work_dir/anyr-aarch64-apple-darwin.signed.json"
cp "$signed_binary" "$signed_asset"
jq -n \
  --arg repository "$repository" \
  --argjson source_run_id "$run_id" \
  --argjson candidate_run_id "$candidate_run_id" \
  --arg source_commit "$source_commit" \
  --arg release_tag "$release_tag" \
  --arg unsigned_sha256 "$actual_binary_hash" \
  --arg signed_sha256 "$signed_binary_hash" \
  --arg notary_submission_id "$notary_submission_id" \
  --arg team_id "$team_id" \
  --arg signing_authority "$signing_authority" \
  --arg identifier "$signed_identifier" \
  '{
    schema_version: 2,
    repository: $repository,
    source_run_id: $source_run_id,
    candidate_run_id: $candidate_run_id,
    source_commit: $source_commit,
    release_tag: $release_tag,
    target: "aarch64-apple-darwin",
    unsigned_sha256: $unsigned_sha256,
    signed_sha256: $signed_sha256,
    notary_submission_id: $notary_submission_id,
    team_id: $team_id,
    signing_authority: $signing_authority,
    identifier: $identifier
  }' > "$handoff_manifest"

if release_state=$(gh release view "$release_tag" --repo "$repository" --json isDraft --jq .isDraft 2>/dev/null); then
  [[ "$release_state" == true ]] || die "release $release_tag already exists and is not a draft"
else
  notes_file="$work_dir/draft-notes.txt"
  printf 'Awaiting verification and packaging of the locally signed macOS artifact.\n' > "$notes_file"
  gh release create "$release_tag" \
    --repo "$repository" \
    --draft \
    --verify-tag \
    --title "$release_tag" \
    --notes-file "$notes_file"
fi

gh release upload "$release_tag" \
  "$signed_asset" \
  "$handoff_manifest" \
  --repo "$repository" \
  --clobber

gh workflow run finalize-release.yml \
  --repo "$repository" \
  --ref "$release_tag" \
  --field "source_run_id=$run_id" \
  --field "release_tag=$release_tag"

printf 'signed SHA-256: %s\n' "$signed_binary_hash"
printf 'notarization submission: %s\n' "$notary_submission_id"
printf 'finalization dispatched for %s from run %s\n' "$release_tag" "$run_id"
