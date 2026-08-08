#!/usr/bin/env python3
"""Run one validated tier of the Anytype API disposable live gate."""

import os
import re
import subprocess
import sys
import tempfile
import tomllib
from ipaddress import ip_address
from pathlib import Path
from urllib.parse import urlsplit


TARGET = re.compile(r"[A-Za-z0-9][A-Za-z0-9_-]*\Z")
TEST = re.compile(r"[A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*\Z")
PREFIX = re.compile(r"[A-Za-z0-9_-]{1,485}\Z")
SUMMARY = re.compile(
    rb"test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured; [0-9]+ filtered out;"
)
SERVICE = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,127}\Z")


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


def loopback_endpoint(value: str | None) -> bool:
    if not value:
        return False
    try:
        endpoint = urlsplit(value)
        return (
            endpoint.scheme in {"http", "https"}
            and ip_address(endpoint.hostname or "").is_loopback
            and endpoint.port is not None
        )
    except ValueError:
        return False


def environment_is_admitted(environment: dict[str, str]) -> bool:
    allowed_credentials = {
        "ANYTYPE_KEY_HTTP_TOKEN",
        "ANYTYPE_KEY_ACCOUNT_ID",
        "ANYTYPE_KEY_ACCOUNT_KEY",
        "ANYTYPE_KEY_SESSION_TOKEN",
    }
    service = environment.get("ANYTYPE_KEYSTORE_SERVICE", "")
    return (
        environment.get("ANYTYPE_DISPOSABLE_TEST_PROCESS") == "1"
        and bool(PREFIX.fullmatch(environment.get("ANYTYPE_TEST_SPACE_PREFIX", "")))
        and environment.get("ANYTYPE_KEYSTORE") == "env"
        and bool(SERVICE.fullmatch(service))
        and bool(environment.get("ANYTYPE_KEY_HTTP_TOKEN"))
        and bool(
            environment.get("ANYTYPE_KEY_SESSION_TOKEN")
            or environment.get("ANYTYPE_KEY_ACCOUNT_KEY")
        )
        and not (
            "ANYTYPE_KEY_ACCOUNT_ID" in environment
            and environment["ANYTYPE_KEY_ACCOUNT_ID"] == ""
        )
        and all(
            name in allowed_credentials
            for name in environment
            if name.startswith("ANYTYPE_KEY_")
        )
        and environment.get("ANYTYPE_RATE_LIMIT_MAX_RETRIES", "5") == "5"
        and loopback_endpoint(environment.get("ANYTYPE_URL"))
        and loopback_endpoint(environment.get("ANYTYPE_GRPC_ENDPOINT"))
    )


def authenticate() -> None:
    descriptor, output_path = tempfile.mkstemp(prefix="anytype-api-live-auth-")
    try:
        with os.fdopen(descriptor, "wb") as output:
            status = subprocess.run(
                [
                    os.environ.get("CARGO", "cargo"),
                    "run",
                    "--locked",
                    "-q",
                    "-p",
                    "anyr",
                    "--",
                    "auth",
                    "status",
                    "--pretty",
                ],
                stdout=output,
                stderr=subprocess.STDOUT,
            ).returncode
        if status != 0:
            fail("live gate authentication preflight failed")
    finally:
        Path(output_path).unlink(missing_ok=True)


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
    if not environment_is_admitted(dict(os.environ)):
        fail("live gate environment is invalid")
    authenticate()
    entries = manifest_entries(sys.argv[1], Path(sys.argv[2]))
    for index, entry in enumerate(entries, start=1):
        run_entry(index, entry)
    print("live gate completed")


if __name__ == "__main__":
    main()
