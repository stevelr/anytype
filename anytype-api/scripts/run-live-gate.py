#!/usr/bin/env python3
"""Run one validated tier of the Anytype API disposable live gate."""

import os
import re
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path


TARGET = re.compile(r"[A-Za-z0-9][A-Za-z0-9_-]*\Z")
TEST = re.compile(r"[A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*\Z")
SUMMARY = re.compile(
    rb"test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured; [0-9]+ filtered out;"
)


def fail(message: str) -> None:
    print(message, file=sys.stderr)
    raise SystemExit(1)


def manifest_entries(tier: str, path: Path) -> list[dict[str, str]]:
    try:
        with path.open("rb") as manifest_file:
            manifest = tomllib.load(manifest_file)
    except (OSError, tomllib.TOMLDecodeError):
        fail("live gate manifest is unavailable or invalid")
    if manifest.get("version") != 1:
        fail("live gate manifest has an unsupported version")
    entries = manifest.get(tier)
    if not isinstance(entries, list) or not entries:
        fail("live gate manifest has no selected tier entries")
    for entry in entries:
        if not isinstance(entry, dict):
            fail("live gate manifest contains an invalid entry")
        target = entry.get("target")
        test = entry.get("test")
        serial_group = entry.get("serial_group")
        if (
            not isinstance(target, str)
            or not TARGET.fullmatch(target)
            or not isinstance(test, str)
            or not TEST.fullmatch(test)
            or serial_group != "disposable_anytype_api"
        ):
            fail("live gate manifest contains an unsafe entry")
    return entries


def run_entry(index: int, entry: dict[str, str]) -> None:
    cargo = os.environ.get("CARGO", "cargo")
    command = [cargo, "test", "--locked", "-p", "anytype"]
    if entry["target"] == "lib":
        command.extend(["--lib", entry["test"]])
    else:
        command.extend(["--test", entry["target"], entry["test"]])
    command.extend(["--", "--ignored", "--exact", "--test-threads=1", "--nocapture"])
    descriptor, output_path = tempfile.mkstemp(prefix=f"anytype-api-live-{index:02d}-")
    try:
        with os.fdopen(descriptor, "wb") as output:
            status = subprocess.run(
                command, stdout=output, stderr=subprocess.STDOUT
            ).returncode
        output = Path(output_path).read_bytes()
        if status != 0:
            fail(f"live gate entry {index} failed")
        if not SUMMARY.search(output):
            fail(f"live gate entry {index} did not execute exactly one test")
        if b"skipped" in output.lower():
            fail(f"live gate entry {index} reported skipped admission")
    finally:
        Path(output_path).unlink(missing_ok=True)


def main() -> None:
    if len(sys.argv) != 3 or sys.argv[1] not in {"required", "soak"}:
        fail("usage: run-live-gate.py required|soak MANIFEST")
    entries = manifest_entries(sys.argv[1], Path(sys.argv[2]))
    for index, entry in enumerate(entries, start=1):
        run_entry(index, entry)
    print("live gate completed")


if __name__ == "__main__":
    main()
