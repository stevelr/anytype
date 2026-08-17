#!/usr/bin/env bash

set -euo pipefail

if [[ "$#" -ne 1 ]]; then
  printf 'usage: %s TAG\n' "${0##*/}" >&2
  exit 2
fi

release_tag=$1
if [[ ! "$release_tag" =~ ^(anyr-v)?[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z]+([.-][0-9A-Za-z]+)*)?$ ]]; then
  printf 'unsupported release tag: %s\n' "$release_tag" >&2
  exit 1
fi
