#!/usr/bin/env python3
"""Convert an append-only Anytype log into fixed, content-free events."""

import json
import os
import stat
import sys
import time
from pathlib import Path


READ_BYTES = 8192
LINE_BYTES = 64 * 1024


def review_line(line: bytes, oversized: bool = False) -> bytes:
    """Return one fixed-category event without copying source content."""
    lowered = line.lower()
    if oversized:
        severity, category = "error", "server_oversized"
    elif b"panic" in lowered or b'"level":"fatal"' in lowered or b"\tfatal\t" in lowered:
        severity, category = "fatal", "server_fatal"
    elif b'"level":"error"' in lowered or b"\terror\t" in lowered:
        severity, category = "error", "server_error"
    elif b'"level":"warn"' in lowered or b"\twarn\t" in lowered:
        severity, category = "warning", "server_warning"
    else:
        severity, category = "info", "server_event"
    return json.dumps(
        {"severity": severity, "component": "anytype", "category": category},
        separators=(",", ":"),
    ).encode("ascii")


def open_private_regular(path: Path, flags: int) -> int:
    descriptor = os.open(
        path,
        flags | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0),
    )
    metadata = os.fstat(descriptor)
    if (
        not stat.S_ISREG(metadata.st_mode)
        or metadata.st_mode & 0o777 != 0o600
        or metadata.st_uid != os.geteuid()
    ):
        os.close(descriptor)
        raise OSError("unsafe log")
    return descriptor


def write_all(descriptor: int, content: bytes) -> None:
    """Write one complete reviewed event to the retained descriptor."""
    offset = 0
    while offset < len(content):
        written = os.write(descriptor, content[offset:])
        if written == 0:
            raise OSError("reviewed log write made no progress")
        offset += written


def review_stream(source_path: Path, destination_path: Path) -> None:
    """Follow one retained source descriptor and append reviewed events."""
    source = open_private_regular(source_path, os.O_RDONLY)
    destination = open_private_regular(destination_path, os.O_WRONLY | os.O_APPEND)
    pending = bytearray()
    oversized = False
    try:
        while True:
            chunk = os.read(source, READ_BYTES)
            if not chunk:
                time.sleep(0.05)
                continue
            for byte in chunk:
                if byte == ord("\n"):
                    event = review_line(bytes(pending), oversized)
                    write_all(destination, event + b"\n")
                    pending.clear()
                    oversized = False
                elif len(pending) < LINE_BYTES:
                    pending.append(byte)
                else:
                    oversized = True
    finally:
        os.close(destination)
        os.close(source)


def main() -> None:
    if len(sys.argv) != 3:
        print("usage: review-server-log.py SOURCE DESTINATION", file=sys.stderr)
        raise SystemExit(2)
    try:
        review_stream(Path(sys.argv[1]), Path(sys.argv[2]))
    except OSError:
        print("server log reviewer failed", file=sys.stderr)
        raise SystemExit(1) from None


if __name__ == "__main__":
    main()
