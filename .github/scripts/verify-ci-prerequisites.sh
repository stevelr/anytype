#!/usr/bin/env bash

set -euo pipefail

check_gate=true
check_repository=false
for argument in "$@"; do
  case "$argument" in
    --without-gate) check_gate=false ;;
    --check-repository) check_repository=true ;;
    *)
      printf 'unknown argument: %s\n' "$argument" >&2
      exit 2
      ;;
  esac
done

rustup_channel=$(sed -n 's/^channel = "\([^"]*\)"/\1/p' rust-toolchain.toml)
test -n "$rustup_channel"
test "$(rustc --version | cut -d ' ' -f 2)" = "$rustup_channel"
test "$(python3 -c 'import sys; print(f"{sys.version_info.major}.{sys.version_info.minor}")')" = 3.14

python3 - <<'PY'
import contextlib
import dataclasses
import hashlib
import importlib.util
import io
import ipaddress
import json
import os
import pathlib
import re
import secrets
import selectors
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import time
import tomllib
import unittest
import urllib
PY

protoc --version
just --version
jq --version
gcc --version | head -n 1
bash .github/scripts/test-release-tag-policy.sh
PYTHONDONTWRITEBYTECODE=1 python3 .github/scripts/test_release_scripts.py
PYTHONDONTWRITEBYTECODE=1 python3 .github/scripts/test_skills_package.py
PYTHONDONTWRITEBYTECODE=1 python3 .github/scripts/test_skills_release.py
PYTHONDONTWRITEBYTECODE=1 python3 .github/scripts/validate_skills_package.py skills
bash .github/scripts/test-skills-release-ref.sh
PYTHONDONTWRITEBYTECODE=1 python3 anyr/tests/test_live_workflow_policy.py
bash .github/scripts/test-package-nix-dist-archive.sh
bash .github/scripts/test-sign-macos-release.sh

if "$check_gate"; then
  gate --version
  if "$check_repository"; then
    gate check
  fi
elif "$check_repository"; then
  printf '%s\n' '--check-repository requires gate' >&2
  exit 2
fi
