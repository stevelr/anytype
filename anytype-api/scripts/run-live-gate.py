#!/usr/bin/env python3
"""Run one validated tier of the Anytype API disposable live gate."""

import os
import re
import selectors
import signal
import subprocess
import sys
import time
import tomllib
from ipaddress import ip_address
from pathlib import Path
from urllib.parse import urlsplit


OUTPUT_LIMIT = 1024 * 1024
PROCESS_TIMEOUT = 600
TARGET = re.compile(r"[A-Za-z0-9][A-Za-z0-9_-]*\Z")
TEST = re.compile(r"[A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*\Z")
PREFIX = re.compile(r"[A-Za-z0-9_-]{1,485}\Z")
SERVICE = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,127}\Z")
SUMMARY = re.compile(
    rb"(?m)^test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured; [0-9]+ filtered out; finished in [0-9]+(?:\.[0-9]+)?s$"
)
SUMMARY_LINE = re.compile(rb"(?m)^test result:.*$")


class RunnerError(Exception):
    """A fixed-classification child-process failure."""


def fail(message: str) -> None:
    print(message, file=sys.stderr)
    raise SystemExit(1)


def terminate(process: subprocess.Popen[bytes]) -> None:
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    process.wait()


def run_bounded(
    command: list[str], timeout: float = PROCESS_TIMEOUT
) -> tuple[int, bytes]:
    process = subprocess.Popen(
        command,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        start_new_session=True,
    )
    assert process.stdout is not None
    output = bytearray()
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ)
    deadline = time.monotonic() + timeout
    try:
        while selector.get_map():
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                terminate(process)
                raise RunnerError("timeout")
            for key, _ in selector.select(remaining):
                chunk = os.read(key.fd, 65_536)
                if not chunk:
                    selector.unregister(key.fileobj)
                    continue
                if len(output) + len(chunk) > OUTPUT_LIMIT:
                    terminate(process)
                    raise RunnerError("overflow")
                output.extend(chunk)
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            terminate(process)
            raise RunnerError("timeout")
        try:
            return process.wait(timeout=remaining), bytes(output)
        except subprocess.TimeoutExpired:
            terminate(process)
            raise RunnerError("timeout") from None
    finally:
        selector.close()
        if process.poll() is None:
            terminate(process)
        process.stdout.close()


def manifest_entries(tier: str, path: Path) -> list[dict[str, str]]:
    try:
        with path.open("rb") as manifest_file:
            manifest = tomllib.load(manifest_file)
    except (OSError, tomllib.TOMLDecodeError):
        fail("live gate manifest is unavailable or invalid")
    entries = manifest.get(tier)
    if manifest.get("version") != 1 or not isinstance(entries, list) or not entries:
        fail("live gate manifest is unavailable or invalid")
    for entry in entries:
        if (
            not isinstance(entry, dict)
            or not isinstance(entry.get("target"), str)
            or not TARGET.fullmatch(entry["target"])
            or not isinstance(entry.get("test"), str)
            or not TEST.fullmatch(entry["test"])
            or entry.get("serial_group") != "disposable_anytype_api"
        ):
            fail("live gate manifest contains an unsafe entry")
    return entries


def endpoint_is_loopback(value: str | None) -> bool:
    try:
        endpoint = urlsplit(value or "")
        if (
            endpoint.scheme not in {"http", "https"}
            or not ip_address(endpoint.hostname or "").is_loopback
        ):
            return False
        return endpoint.port is not None or endpoint.scheme in {"http", "https"}
    except ValueError:
        return False


def environment_is_admitted(environment: dict[str, str]) -> bool:
    allowed = {
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
            and not environment["ANYTYPE_KEY_ACCOUNT_ID"]
        )
        and all(
            name in allowed for name in environment if name.startswith("ANYTYPE_KEY_")
        )
        and environment.get("ANYTYPE_RATE_LIMIT_MAX_RETRIES", "5") == "5"
        and endpoint_is_loopback(environment.get("ANYTYPE_URL"))
        and endpoint_is_loopback(environment.get("ANYTYPE_GRPC_ENDPOINT"))
    )


def authenticate() -> None:
    status, _ = run_bounded(
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
        ]
    )
    if status != 0:
        fail("live gate authentication preflight failed")


def run_entry(index: int, entry: dict[str, str]) -> None:
    command = [os.environ.get("CARGO", "cargo"), "test", "--locked", "-p", "anytype"]
    command.extend(
        ["--lib", entry["test"]]
        if entry["target"] == "lib"
        else ["--test", entry["target"], entry["test"]]
    )
    command.extend(["--", "--ignored", "--exact", "--test-threads=1", "--nocapture"])
    try:
        status, output = run_bounded(command)
    except RunnerError:
        fail(f"live gate entry {index} failed")
    summaries = SUMMARY_LINE.findall(output)
    if (
        status != 0
        or len(summaries) != 1
        or not SUMMARY.fullmatch(summaries[0])
        or b"skipped" in output.lower()
    ):
        fail(f"live gate entry {index} failed")


def main() -> None:
    if len(sys.argv) != 3 or sys.argv[1] not in {"required", "soak"}:
        fail("usage: run-live-gate.py required|soak MANIFEST")
    if not environment_is_admitted(dict(os.environ)):
        fail("live gate environment is invalid")
    try:
        authenticate()
    except RunnerError:
        fail("live gate authentication preflight failed")
    for index, entry in enumerate(manifest_entries(sys.argv[1], Path(sys.argv[2])), 1):
        run_entry(index, entry)
    print("live gate completed")


if __name__ == "__main__":
    main()
