#!/usr/bin/env bash

set -euo pipefail

if [[ "$#" -ne 4 ]]; then
  printf 'usage: %s REPOSITORY RELEASE_TAG ASSET_DIR EXPECTED_TEAM_ID\n' "${0##*/}" >&2
  exit 2
fi

repository=$1
release_tag=$2
asset_dir=$3
expected_team_id=$4

if [[ ! "$repository" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]]; then
  printf 'invalid repository: %s\n' "$repository" >&2
  exit 2
fi
if [[ -z "$expected_team_id" ]]; then
  printf 'MACOS_DEVELOPER_TEAM_ID is required\n' >&2
  exit 2
fi
if [[ ! -d "$asset_dir" ]]; then
  printf 'release asset directory is missing: %s\n' "$asset_dir" >&2
  exit 1
fi

script_dir=$(cd "$(dirname "$0")" && pwd)
bash "$script_dir/validate-release-tag.sh" "$release_tag"

sha256_files() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$@"
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$@"
  else
    printf 'SHA-256 checksum tool is required\n' >&2
    return 127
  fi
}

sha256_check() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum --check "$1"
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 --check "$1"
  else
    printf 'SHA-256 checksum tool is required\n' >&2
    return 127
  fi
}

asset_count=$(find "$asset_dir" -maxdepth 1 -type f | wc -l | tr -d ' ')
if [[ "$asset_count" -eq 0 ]]; then
  printf 'release contains no downloaded assets\n' >&2
  exit 1
fi

work_dir=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/anyr-release-audit.XXXXXX")
trap 'rm -rf "$work_dir"' EXIT

asset_list=$work_dir/assets
find "$asset_dir" -maxdepth 1 -type f -print | LC_ALL=C sort > "$asset_list"
while IFS= read -r asset_path; do
  gh attestation verify "$asset_path" \
    --repo "$repository" \
    --signer-workflow "$repository/.github/workflows/finalize-release.yml" \
    --source-ref "refs/tags/$release_tag" \
    --deny-self-hosted-runners >/dev/null
done < "$asset_list"

checksum_list=$work_dir/checksum-files
find "$asset_dir" -maxdepth 1 -type f -name '*.sha256' -print | LC_ALL=C sort > "$checksum_list"
checksum_count=$(wc -l < "$checksum_list" | tr -d ' ')
if [[ "$checksum_count" -eq 0 || ! -f "$asset_dir/sha256.sum" ]]; then
  printf 'release checksum files are incomplete\n' >&2
  exit 1
fi
while IFS= read -r checksum_path; do
  (cd "$asset_dir" && sha256_check "${checksum_path##*/}")
done < "$checksum_list"
(cd "$asset_dir" && sha256_check sha256.sum)

macos_archive=$asset_dir/anyr-aarch64-apple-darwin.tar.xz
notarization_manifest=$asset_dir/anyr-aarch64-apple-darwin.notarization.json
if [[ ! -f "$macos_archive" || ! -f "$notarization_manifest" ]]; then
  printf 'macOS archive or notarization evidence is missing\n' >&2
  exit 1
fi

extract_dir=$work_dir/extracted
mkdir -p "$extract_dir"
tar -xJf "$macos_archive" -C "$extract_dir"
macos_binary=$extract_dir/anyr-aarch64-apple-darwin/anyr
if [[ ! -f "$macos_binary" ]]; then
  printf 'macOS archive does not contain the expected anyr binary\n' >&2
  exit 1
fi

signed_hash=$(sha256_files "$macos_binary" | awk '{print $1}')
jq -e \
  --arg repository "$repository" \
  --arg release_tag "$release_tag" \
  --arg signed_hash "$signed_hash" \
  --arg team_id "$expected_team_id" '
    .schema_version == 1 and
    .repository == $repository and
    .release_tag == $release_tag and
    .target == "aarch64-apple-darwin" and
    .signed_sha256 == $signed_hash and
    .team_id == $team_id and
    .identifier == "com.stevelr.anyr" and
    (.source_commit | test("^[0-9a-f]{40}$")) and
    (.notary_submission_id | test("^[0-9A-Fa-f-]{36}$")) and
    (.signing_authority | startswith("Developer ID Application:"))
  ' "$notarization_manifest" >/dev/null

codesign --verify --strict --verbose=4 "$macos_binary"
codesign_details=$extract_dir/codesign-details.txt
codesign --display --verbose=4 "$macos_binary" 2> "$codesign_details"
test "$(sed -n 's/^TeamIdentifier=//p' "$codesign_details" | head -n 1)" = "$expected_team_id"
test "$(sed -n 's/^Identifier=//p' "$codesign_details" | head -n 1)" = com.stevelr.anyr
test "$(sed -n 's/^Authority=//p' "$codesign_details" | head -n 1)" = \
  "$(jq -r .signing_authority "$notarization_manifest")"

assessment_status=0
assessment=$(spctl --assess --type execute --verbose=4 "$macos_binary" 2>&1) || assessment_status=$?
if [[ "$assessment_status" -ne 0 ]]; then
  printf '%s\n' "$assessment" >&2
  [[ "$assessment" == *'the code is valid but does not seem to be an app'* ]] || exit 1
fi

printf 'verified %s release assets for %s\n' "$asset_count" "$release_tag"
