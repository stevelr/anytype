#!/usr/bin/env python3
"""Run one any-mcp live command without exposing its transcript."""

import os
import re
import selectors
import signal
import stat
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
TEST_LOG_LINE = re.compile(rb"(?m)^(ok|failed|ignored) ([A-Za-z0-9_]+(?:::[A-Za-z0-9_]+)*)$")
SKIPPED_ADMISSION = re.compile(
    rb"(?m)^.*skipped.*(?:PrefixNotConfigured|PrefixInvalid|"
    rb"PlatformIsolationUnavailable|ProcessIsolationUnavailable|"
    rb"EnvironmentProvisioningUnavailable).*$"
)
EXPECTED = {"direct": 38, "stdio": 30, "discussions": 1}
COMMAND_LABELS = {"auth", "reset"}
# Scrubbed failure diagnostics: opt in with ANY_MCP_LIVE_DIAGNOSTICS_DIR.
DIAGNOSTICS_LIMIT = 65_536
PANIC_LIMIT = 16
PANIC_LINE_LIMIT = 400
PANIC_LINE = re.compile(r"^thread '[^']*' \(?[0-9]*\)? ?panicked at .*:[0-9]+:[0-9]+:$")
CREDENTIAL_LINE = re.compile(
    r"(?i)(authorization|bearer|password|secret|token|api[_-]?key|"
    r"ANYTYPE_KEY_|account[_-]?key|session)"
)
MASKS = (
    (re.compile(r"[A-Za-z0-9_-]{16,}\.[A-Za-z0-9_-]{16,}\.[A-Za-z0-9_-]{16,}"), "<jwt>"),
    (re.compile(r"\bbafy[a-z2-7]{20,}\b"), "<cid>"),
    (re.compile(r"(?<![0-9a-fA-F])[0-9a-fA-F]{32,}(?![0-9a-fA-F])"), "<hex>"),
    (re.compile(r"[A-Za-z0-9+/=_-]{40,}"), "<blob>"),
)


class RunnerError(Exception):
    """A child failure whose raw details must remain private."""


def scrub_line(line: str) -> str:
    """Reduce one transcript line to a credential-free diagnostic line."""
    if CREDENTIAL_LINE.search(line):
        return "<redacted line>"
    for pattern, replacement in MASKS:
        line = pattern.sub(replacement, line)
    return line


def scrub_transcript(output: bytes) -> list[str]:
    output = ANSI_CSI.sub(b"", output)[-DIAGNOSTICS_LIMIT:]
    text = output.decode("utf-8", errors="replace")
    return [scrub_line(line.rstrip("\r")) for line in text.split("\n")]


def panic_excerpt(lines: list[str]) -> list[str]:
    """Panic headers with their message line, bounded and already scrubbed."""
    excerpt: list[str] = []
    for index, line in enumerate(lines):
        if len(excerpt) >= PANIC_LIMIT:
            excerpt.append("<further panics omitted>")
            break
        if PANIC_LINE.match(line):
            message = lines[index + 1] if index + 1 < len(lines) else ""
            excerpt.append(
                f"{line[:PANIC_LINE_LIMIT]} {message.strip()[:PANIC_LINE_LIMIT]}".rstrip()
            )
    return excerpt


def diagnostics_dir() -> Path | None:
    value = os.environ.get("ANY_MCP_LIVE_DIAGNOSTICS_DIR", "")
    if not value:
        return None
    path = Path(value)
    if not path.is_absolute():
        raise RunnerError("diagnostics_dir")
    path.mkdir(mode=0o700, parents=True, exist_ok=True)
    metadata = os.lstat(path)
    if (
        not stat.S_ISDIR(metadata.st_mode)
        or metadata.st_uid != os.geteuid()
        or stat.S_IMODE(metadata.st_mode) & 0o077
    ):
        raise RunnerError("diagnostics_dir")
    return path


def retain_diagnostics(label: str, reason: str, output: bytes) -> None:
    """Write a scrubbed transcript tail and echo scrubbed panic lines.

    Raw transcripts stay private; only credential-masked text leaves the run.
    """
    lines = scrub_transcript(output)
    for line in panic_excerpt(lines):
        print(f"required live gate {label} panic: {line}", file=sys.stderr)
    directory = diagnostics_dir()
    if directory is None:
        return
    target = directory / f"{label}-failure-diagnostics.txt"
    descriptor = os.open(
        target, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600
    )
    with os.fdopen(descriptor, "w", encoding="utf-8") as sink:
        sink.write(f"required live gate {label} failed reason={reason}\n")
        sink.write("scrubbed transcript tail (credential-like lines redacted):\n\n")
        sink.write("\n".join(lines))
        sink.write("\n")


def fail(
    label: str,
    reason: str = "invocation",
    failed_tests: tuple[str, ...] = (),
    last_completed: str | None = None,
    completed: int = 0,
) -> None:
    tests = f" tests={','.join(failed_tests)}" if failed_tests else ""
    progress = (
        f" last_completed={last_completed} completed={completed}"
        if not failed_tests and last_completed is not None
        else ""
    )
    print(
        f"required live gate {label} failed reason={reason}{tests}{progress}",
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


def read_test_progress(label: str, descriptor: int) -> tuple[tuple[str, ...], str | None, int]:
    metadata = os.fstat(descriptor)
    if (
        not stat.S_ISREG(metadata.st_mode)
        or metadata.st_uid != os.geteuid()
        or stat.S_IMODE(metadata.st_mode) != 0o600
        or metadata.st_nlink > 1
    ):
        raise RunnerError("progress")
    with os.fdopen(os.dup(descriptor), "rb") as stream:
        stream.seek(0)
        output = stream.read(OUTPUT_LIMIT + 1)
    if len(output) > OUTPUT_LIMIT:
        raise RunnerError("overflow")
    results = TEST_LOG_LINE.findall(output)
    if len(results) > EXPECTED.get(label, 0):
        raise RunnerError("progress")
    names = tuple(name.decode("ascii") for _, name in results)
    failed = tuple(
        sorted(name.decode("ascii") for result, name in results if result == b"failed")
    )
    return failed, names[-1] if names else None, len(names)


def private_test_log(private_dir: Path, label: str) -> tuple[int, Path]:
    private_dir.mkdir(mode=0o700, parents=True, exist_ok=True)
    directory = os.lstat(private_dir)
    if (
        not stat.S_ISDIR(directory.st_mode)
        or directory.st_uid != os.geteuid()
        or stat.S_IMODE(directory.st_mode) & 0o077
    ):
        raise RunnerError("private_dir")
    descriptor, value = tempfile.mkstemp(
        prefix=f"{label}-libtest-", suffix=".log", dir=private_dir
    )
    try:
        os.fchmod(descriptor, 0o600)
    except BaseException:
        os.close(descriptor)
        Path(value).unlink(missing_ok=True)
        raise
    return descriptor, Path(value)


def remove_test_log(descriptor: int, path: Path) -> None:
    original = os.fstat(descriptor)
    replaced = False
    try:
        try:
            current = os.lstat(path)
        except FileNotFoundError:
            current = None
            replaced = True
        if current is not None:
            replaced = (current.st_dev, current.st_ino) != (
                original.st_dev,
                original.st_ino,
            )
            if stat.S_ISDIR(current.st_mode):
                os.rmdir(path)
            else:
                os.unlink(path)
    finally:
        os.close(descriptor)
    if replaced:
        raise RunnerError("progress_replaced")


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
    test_log: Path | None = None
    test_log_descriptor: int | None = None
    failure: tuple[str, tuple[str, ...], str | None, int] | None = None
    try:
        if mode == "test":
            try:
                test_log_descriptor, test_log = private_test_log(private_dir, label)
            except OSError:
                failure = ("runner_io", (), None, 0)
            except RunnerError:
                failure = ("runner_bound", (), None, 0)
            if failure is None:
                command = [*command, "--logfile", os.fspath(test_log)]
        if failure is None:
            try:
                status, output = run_bounded(command, private_dir)
            except OSError:
                failure = ("runner_io", (), None, 0)
            except RunnerError:
                failure = ("runner_bound", (), None, 0)
        if failure is None:
            try:
                logged_failed, last_completed, completed = (
                    read_test_progress(label, test_log_descriptor)
                    if test_log_descriptor is not None
                    else ((), None, 0)
                )
            except OSError:
                failure = ("runner_io", (), None, 0)
            except RunnerError:
                failure = ("runner_bound", (), None, 0)
        if failure is None and status != 0:
            reason = "child_signal" if status < 0 else "child_exit"
            failed = logged_failed or failed_test_names(label, output)
            failure = (reason, failed, last_completed, completed)
            try:
                retain_diagnostics(label, reason, output)
            except (OSError, RunnerError):
                print(
                    f"required live gate {label} diagnostics unavailable",
                    file=sys.stderr,
                )
        if failure is None and mode == "test":
            summaries = SUMMARY_LINE.findall(output)
            if len(summaries) != 1:
                failure = ("summary_count", (), None, 0)
            else:
                match = TEST_SUMMARY.fullmatch(summaries[0])
                if match is None:
                    failure = ("summary_shape", (), None, 0)
                elif match.group(1) != str(EXPECTED[label]).encode("ascii"):
                    failure = ("test_count", (), None, 0)
                elif SKIPPED_ADMISSION.search(output) is not None:
                    failure = ("skipped_admission", (), None, 0)
    except (Exception, KeyboardInterrupt):
        failure = ("runner_io", (), None, 0)
    finally:
        if test_log is not None and test_log_descriptor is not None:
            try:
                remove_test_log(test_log_descriptor, test_log)
            except (Exception, KeyboardInterrupt):
                failure = ("runner_io", (), None, 0)
    if failure is not None:
        reason, failed, last_completed, completed = failure
        fail(label, reason, failed, last_completed, completed)
    print(f"required live gate {label} completed")


if __name__ == "__main__":
    main()
