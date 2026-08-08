#!/usr/bin/env python3
"""Bind live evidence to one opened server-log identity and byte window."""

import hashlib
import os
import secrets
import stat
import sys
from pathlib import Path


ANCHOR_LIMIT = 4096
ARTIFACT_LIMIT = 65_536
FRESH_ARTIFACT_LIMIT = 64_000
CONTEXT_KEYS = (
    "run_marker",
    "start_device",
    "start_inode",
    "start_bytes",
    "anchor_start",
    "anchor_length",
    "anchor_hash",
)


def open_regular(path: Path) -> tuple[int, os.stat_result]:
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open(path, flags)
    metadata = os.fstat(descriptor)
    if not stat.S_ISREG(metadata.st_mode):
        os.close(descriptor)
        raise OSError("not regular")
    return descriptor, metadata


def start(source: Path, context: Path) -> None:
    descriptor, metadata = open_regular(source)
    try:
        start_bytes = metadata.st_size
        anchor_length = min(start_bytes, ANCHOR_LIMIT)
        anchor_start = start_bytes - anchor_length
        anchor = os.pread(descriptor, anchor_length, anchor_start)
        if len(anchor) != anchor_length:
            raise OSError("short anchor")
    finally:
        os.close(descriptor)
    values = {
        "run_marker": secrets.token_hex(32),
        "start_device": str(metadata.st_dev),
        "start_inode": str(metadata.st_ino),
        "start_bytes": str(start_bytes),
        "anchor_start": str(anchor_start),
        "anchor_length": str(anchor_length),
        "anchor_hash": hashlib.sha256(anchor).hexdigest(),
    }
    descriptor = os.open(context, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, "wb") as output:
        payload = "".join(f"{key}={values[key]}\n" for key in CONTEXT_KEYS).encode()
        output.write(payload)
    print(f"ANY_MCP_HEADLESS_EVIDENCE_CONTEXT={context}")
    print(f"ANY_MCP_HEADLESS_LOG_RUN_MARKER={values['run_marker']}")


def read_context(path: Path) -> dict[str, str]:
    descriptor, metadata = open_regular(path)
    try:
        if metadata.st_mode & 0o077 or metadata.st_size > 4096:
            raise OSError("unsafe context")
        contents = os.read(descriptor, metadata.st_size + 1).decode("ascii")
    finally:
        os.close(descriptor)
    values: dict[str, str] = {}
    for line in contents.splitlines():
        key, separator, value = line.partition("=")
        if not separator or key not in CONTEXT_KEYS or key in values:
            raise ValueError("invalid context")
        values[key] = value
    if tuple(values) != CONTEXT_KEYS:
        raise ValueError("incomplete context")
    for key in CONTEXT_KEYS[1:6]:
        if not values[key].isdigit():
            raise ValueError("invalid numeric context")
    if len(values["run_marker"]) != 64 or len(values["anchor_hash"]) != 64:
        raise ValueError("invalid digest context")
    int(values["run_marker"], 16)
    int(values["anchor_hash"], 16)
    return values


def reviewed_window(source: Path, context: Path) -> tuple[str, bytes]:
    values = read_context(context)
    descriptor, metadata = open_regular(source)
    try:
        start_bytes = int(values["start_bytes"])
        anchor_start = int(values["anchor_start"])
        anchor_length = int(values["anchor_length"])
        if (
            metadata.st_dev != int(values["start_device"])
            or metadata.st_ino != int(values["start_inode"])
            or metadata.st_size < start_bytes
            or anchor_start + anchor_length != start_bytes
            or anchor_length > ANCHOR_LIMIT
        ):
            raise OSError("identity changed")
        anchor = os.pread(descriptor, anchor_length, anchor_start)
        if hashlib.sha256(anchor).hexdigest() != values["anchor_hash"]:
            raise OSError("anchor changed")
        fresh_size = metadata.st_size - start_bytes
        offset = start_bytes + max(0, fresh_size - FRESH_ARTIFACT_LIMIT)
        fresh = os.pread(descriptor, min(fresh_size, FRESH_ARTIFACT_LIMIT), offset)
        return values["run_marker"], fresh
    finally:
        os.close(descriptor)


def capture(source: Path, context: Path, artifact: Path) -> None:
    try:
        marker, fresh = reviewed_window(source, context)
        payload = (
            f"any-mcp reviewed failure evidence\nrun-marker={marker}\n"
            "reviewed-source=fresh-post-start-window\n"
        ).encode() + fresh
    except (OSError, ValueError, UnicodeError):
        payload = b"any-mcp reviewed failure evidence\nreviewed-source=unavailable\n"
    payload = payload[:ARTIFACT_LIMIT]
    descriptor = os.open(artifact, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, "wb") as output:
        output.write(payload)


def main() -> None:
    try:
        if len(sys.argv) == 4 and sys.argv[1] == "start":
            start(Path(sys.argv[2]), Path(sys.argv[3]))
        elif len(sys.argv) == 5 and sys.argv[1] == "capture":
            capture(Path(sys.argv[2]), Path(sys.argv[3]), Path(sys.argv[4]))
        else:
            raise ValueError("usage")
    except (OSError, ValueError, UnicodeError):
        print("reviewed evidence operation failed", file=sys.stderr)
        raise SystemExit(1) from None


if __name__ == "__main__":
    main()
