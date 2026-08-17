#!/usr/bin/env bash

set -euo pipefail

validator=.github/scripts/validate-release-tag.sh

for tag in \
  0.5.0 \
  0.5.1-2 \
  0.5.1-beta.1 \
  0.5.1-pre.6 \
  anyr-v2.0.0 \
  anyr-v1.10.100-1
do
  bash "$validator" "$tag"
done

for tag in \
  v0.5.0 \
  anytype-v0.5.0 \
  anyr-v1.2 \
  1.2 \
  1.2.3- \
  1.2.3-beta. \
  1.2.3-beta..1 \
  1.2.3+build \
  1.2.3_beta \
  release-1.2.3
do
  if bash "$validator" "$tag" >/dev/null 2>&1; then
    printf 'invalid release tag was accepted: %s\n' "$tag" >&2
    exit 1
  fi
done
