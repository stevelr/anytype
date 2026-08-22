#!/usr/bin/env python3
"""Run one any-mcp live command without exposing its transcript."""

import os
import re
import selectors
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path


OUTPUT_LIMIT = 1024 * 1024
PROCESS_TIMEOUT = 1200
ANSI_CSI = re.compile(rb"\x1b\[[0-?]*[ -/]*[@-~]")
SUMMARY_LINE = re.compile(rb"(?m)^test result:.*$")
FAILED_TEST_LINE = re.compile(rb"(?m)^test ([A-Za-z0-9_]+(?:::[A-Za-z0-9_]+)*) \.\.\. FAILED$")
TEST_SUMMARY = re.compile(
    rb"test result: ok\. ([0-9]+) passed; 0 failed; 0 ignored; 0 measured; "
    rb"[0-9]+ filtered out; finished in [0-9]+(?:\.[0-9]+)?s"
)
SKIPPED_ADMISSION = re.compile(
    rb"(?m)^.*skipped.*(?:PrefixNotConfigured|PrefixInvalid|"
    rb"PlatformIsolationUnavailable|ProcessIsolationUnavailable|"
    rb"EnvironmentProvisioningUnavailable).*$"
)
EXPECTED = {"direct": 38, "stdio": 30, "discussions": 1}
COMMAND_LABELS = {"auth", "reset"}


class RunnerError(Exception):
    """A child failure whose raw details must remain private."""


def fail(
    label: str,
    reason: str = "invocation",
    failed_tests: tuple[str, ...] = (),
) -> None:
    tests = f" tests={','.join(failed_tests)}" if failed_tests else ""
    print(
        f"required live gate {label} failed reason={reason}{tests}",
        file=sys.stderr,
    )
    raise SystemExit(1)


def failed_test_names(label: str, output: bytes) -> tuple[str, ...]:
    output = ANSI_CSI.sub(b"", output)
    names = tuple(
        sorted({match.decode("ascii") for match in FAILED_TEST_LINE.findall(output)})
    )
    if len(names) > EXPECTED.get(label, 0):
        return ()
    return names


def terminate(process: subprocess.Popen[bytes]) -> None:
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    process.wait()


def run_bounded(command: list[str], private_dir: Path) -> tuple[int, bytes]:
    private_dir.mkdir(mode=0o700, parents=True, exist_ok=True)
    with tempfile.TemporaryFile(dir=private_dir) as transcript:
        os.chmod(transcript.fileno(), 0o600)
        process = subprocess.Popen(
            command,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        if process.stdout is None:
            raise RunnerError("pipe")
        selector = selectors.DefaultSelector()
        selector.register(process.stdout, selectors.EVENT_READ)
        deadline = time.monotonic() + PROCESS_TIMEOUT
        size = 0
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
                    size += len(chunk)
                    if size > OUTPUT_LIMIT:
                        terminate(process)
                        raise RunnerError("overflow")
                    transcript.write(chunk)
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                terminate(process)
                raise RunnerError("timeout")
            try:
                status = process.wait(timeout=remaining)
            except subprocess.TimeoutExpired:
                terminate(process)
                raise RunnerError("timeout") from None
            transcript.seek(0)
            return status, transcript.read(OUTPUT_LIMIT + 1)
        finally:
            selector.close()
            if process.poll() is None:
                terminate(process)
            process.stdout.close()


def main() -> None:
    if len(sys.argv) < 5 or sys.argv[3] != "--":
        print("usage: run-live-gate.py command|test LABEL -- COMMAND", file=sys.stderr)
        raise SystemExit(2)
    mode, label, command = sys.argv[1], sys.argv[2], sys.argv[4:]
    admitted = (mode == "test" and label in EXPECTED) or (
        mode == "command" and label in COMMAND_LABELS
    )
    if not admitted or not command:
        fail(label)
    private_dir_value = os.environ.get("ANY_MCP_LIVE_PRIVATE_DIR", "")
    private_dir = Path(private_dir_value)
    if not private_dir.is_absolute():
        fail(label)
    try:
        status, output = run_bounded(command, private_dir)
    except OSError:
        fail(label, "runner_io")
    except RunnerError:
        fail(label, "runner_bound")
    if status != 0:
        reason = "child_signal" if status < 0 else "child_exit"
        fail(label, reason, failed_test_names(label, output))
    if mode == "test":
        summaries = SUMMARY_LINE.findall(output)
        if len(summaries) != 1:
            fail(label, "summary_count")
        match = TEST_SUMMARY.fullmatch(summaries[0])
        if match is None:
            fail(label, "summary_shape")
        if int(match.group(1)) != EXPECTED[label]:
            fail(label, "test_count")
        if SKIPPED_ADMISSION.search(output) is not None:
            fail(label, "skipped_admission")
    print(f"required live gate {label} completed")


if __name__ == "__main__":
    main()
