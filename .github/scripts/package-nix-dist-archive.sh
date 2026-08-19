#!/usr/bin/env bash

set -euo pipefail

if [[ "$#" -ne 2 ]]; then
  printf 'usage: %s TARGET BINARY\n' "${0##*/}" >&2
  exit 2
fi

target=$1
binary=$2

case "$target" in
  aarch64-apple-darwin | aarch64-unknown-linux-gnu | x86_64-unknown-linux-gnu) ;;
  *)
    printf 'unsupported Nix release target: %s\n' "$target" >&2
    exit 2
    ;;
esac

if [[ ! -f "$binary" || ! -x "$binary" ]]; then
  printf 'Nix release binary is missing or not executable: %s\n' "$binary" >&2
  exit 1
fi

for asset in anyr/CHANGELOG.md anyr/README.md LICENSE-APACHE; do
  if [[ ! -f "$asset" ]]; then
    printf 'release asset is missing: %s\n' "$asset" >&2
    exit 1
  fi
done

archive_root="anyr-$target"
archive_name="$archive_root.tar.xz"
dist_dir=target/distrib
stage_dir="$dist_dir/$archive_root"
archive_path="$dist_dir/$archive_name"
checksum_path="$archive_path.sha256"
manifest_path=dist-manifest.json
manifest_tmp="$manifest_path.tmp"

rm -rf "$stage_dir"
mkdir -p "$stage_dir"
cp "$binary" "$stage_dir/anyr"
cp anyr/CHANGELOG.md "$stage_dir/CHANGELOG.md"
cp anyr/README.md "$stage_dir/README.md"
cp LICENSE-APACHE "$stage_dir/LICENSE-APACHE"
chmod 0755 "$stage_dir/anyr"

rm -f "$archive_path" "$checksum_path" "$manifest_path" "$manifest_tmp"
tar -cJf "$archive_path" -C "$dist_dir" "$archive_root"

# coreutils on Linux, Perl shasum on macOS; both print "<hex>  <file>".
sha256_of() {
  if command -v sha256sum > /dev/null 2>&1; then
    sha256sum "$1"
  else
    shasum -a 256 "$1"
  fi
}

archive_hash=$(sha256_of "$archive_path" | awk '{print $1}')
printf '%s *%s\n\n' "$archive_hash" "$archive_name" > "$checksum_path"

dist_args=(
  manifest
  --artifacts=local
  "--target=$target"
  --output-format=json
)
if [[ -n "${RELEASE_TAG:-}" ]]; then
  dist_args+=("--tag=$RELEASE_TAG")
fi
dist "${dist_args[@]}" > "$manifest_tmp"

jq --exit-status \
  --arg archive "$archive_name" \
  --arg hash "$archive_hash" \
  '
    if .artifacts[$archive] == null then
      error("dist manifest omitted the Nix release archive")
    else
      .artifacts[$archive].checksums.sha256 = $hash
    end
  ' "$manifest_tmp" > "$manifest_path"
rm -f "$manifest_tmp"

extract_dir=$(mktemp -d "${TMPDIR:-/tmp}/anyr-dist-archive.XXXXXX")
trap 'rm -rf "$extract_dir"' EXIT
tar -xJf "$archive_path" -C "$extract_dir"
cmp "$binary" "$extract_dir/$archive_root/anyr"

printf 'Nix binary SHA-256: '
sha256_of "$binary"

