#!/usr/bin/env bash

set -euo pipefail

script=.github/scripts/package-nix-dist-archive.sh
test_root=$(mktemp -d "${TMPDIR:-/tmp}/package-nix-dist-test.XXXXXX")
trap 'rm -rf "$test_root"' EXIT

mkdir -p "$test_root/bin" "$test_root/repo/anyr"
cp "$script" "$test_root/repo/package.sh"
printf '#!/usr/bin/env bash\nprintf "nix-built-anyr\\n"\n' > "$test_root/repo/anyr-bin"
chmod +x "$test_root/repo/anyr-bin"
printf 'changelog\n' > "$test_root/repo/anyr/CHANGELOG.md"
printf 'readme\n' > "$test_root/repo/anyr/README.md"
printf 'license\n' > "$test_root/repo/LICENSE-APACHE"

cat > "$test_root/bin/dist" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
target=
for argument in "$@"; do
  case "$argument" in
    --target=*) target=${argument#--target=} ;;
  esac
done
archive="anyr-$target.tar.xz"
jq -n \
  --arg archive "$archive" \
  --arg path "$PWD/target/distrib/$archive" \
  '{
    artifacts: {
      ($archive): {name: $archive, path: $path, checksums: {}},
      ($archive + ".sha256"): {
        name: ($archive + ".sha256"),
        path: ($path + ".sha256"),
        checksums: {}
      }
    },
    upload_files: [$path, ($path + ".sha256")]
  }'
EOF
chmod +x "$test_root/bin/dist"

(
  cd "$test_root/repo"
  PATH="$test_root/bin:$PATH" RELEASE_TAG=anyr-v0.5.0 \
    bash package.sh x86_64-unknown-linux-gnu ./anyr-bin

  archive=target/distrib/anyr-x86_64-unknown-linux-gnu.tar.xz
  checksum="$archive.sha256"
  test -f "$archive"
  test -f "$checksum"
  test -f dist-manifest.json

  # Byte order keeps the expected listing stable across host locales.
  tar -tf "$archive" | LC_ALL=C sort > archive-files.txt
  diff -u - archive-files.txt <<'EOF'
anyr-x86_64-unknown-linux-gnu/
anyr-x86_64-unknown-linux-gnu/CHANGELOG.md
anyr-x86_64-unknown-linux-gnu/LICENSE-APACHE
anyr-x86_64-unknown-linux-gnu/README.md
anyr-x86_64-unknown-linux-gnu/anyr
EOF

  expected_hash=$(shasum -a 256 "$archive" | awk '{print $1}')
  test "$(awk 'NR == 1 { print $1 }' "$checksum")" = "$expected_hash"
  test "$(jq -r '.artifacts["anyr-x86_64-unknown-linux-gnu.tar.xz"].checksums.sha256' dist-manifest.json)" = "$expected_hash"

  mkdir extracted
  tar -xJf "$archive" -C extracted
  cmp anyr-bin extracted/anyr-x86_64-unknown-linux-gnu/anyr
)

if (
  cd "$test_root/repo"
  PATH="$test_root/bin:$PATH" bash package.sh x86_64-apple-darwin ./anyr-bin
) >/dev/null 2>&1; then
  printf 'unsupported target was accepted\n' >&2
  exit 1
fi
