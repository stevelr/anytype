#!/usr/bin/env python3
"""Decide whether a commit has passed every release-qualifying check.

A release tag may only be cut from a commit on which all five verification
workflows (smoke, CI, any-mcp, anytype-api live, anyr-anyback-live) completed
successfully. GitHub reports each workflow job as a check run on the commit,
so eligibility is evaluated per job name: the latest completed check run for
every required name must have the conclusion ``success``. Evaluating job
names rather than workflow conclusions means a manual run that selected a
narrower tier (for example any-mcp ``portable`` only) cannot satisfy the gate.

Usage: release_eligibility.py --repo OWNER/NAME --sha COMMIT [--json]

Exit status 0 means eligible. The script uses the ``gh`` CLI with ``GH_TOKEN``
for the check-runs query; evaluation itself is pure and unit-tested.
"""

from __future__ import annotations

import argparse
from collections.abc import Callable
import json
import subprocess
import sys
import time
from typing import NamedTuple

# Job names (GitHub check-run contexts) that must be green on the release
# commit. Keep in sync with the job `name:` values in the workflows named in
# the comments; the qualification workflow dispatches exactly these workflows.
REQUIRED_CHECKS: tuple[str, ...] = (
    # smoke.yml
    "repository checks, clippy, and fast tests (linux-x86_64)",
    "clippy (linux-aarch64)",
    "clippy (macos-aarch64)",
    "clippy (windows-x86_64)",
    "clippy (windows-aarch64)",
    # ci.yml
    "native gates (linux-x86_64)",
    "native gates (linux-aarch64)",
    "native gates (macos-aarch64)",
    "native gates (windows-x86_64)",
    "native gates (windows-aarch64)",
    "packaged crate isolation",
    # any-mcp.yml
    "contracts, stdio, and artifacts (linux-x86_64)",
    "contracts, stdio, and artifacts (linux-aarch64)",
    "contracts, stdio, and artifacts (macos-aarch64)",
    "contracts, stdio, and artifacts (windows-x86_64)",
    "contracts, stdio, and artifacts (windows-aarch64)",
    "stdio to headless Anytype",
    # anytype-api-live.yml
    "disposable ignored-test inventory",
    "protected disposable required tier",
    # anyr-anyback-live.yml
    "installed anyr backup create/restore",
)


class EligibilitySnapshot(NamedTuple):
    """Current terminal and non-terminal states for required checks."""

    missing: tuple[str, ...]
    pending: tuple[tuple[str, str], ...]
    failed: tuple[tuple[str, str | None], ...]

    @property
    def eligible(self) -> bool:
        """Return whether every required check completed successfully."""

        return not self.missing and not self.pending and not self.failed


def latest_runs(check_runs: list[dict]) -> dict[str, dict]:
    """Return the newest observed check run for each check name."""

    latest: dict[str, tuple[str, dict]] = {}
    for run in check_runs:
        name = run.get("name")
        if not isinstance(name, str):
            continue
        observed_at = run.get("completed_at") or run.get("started_at") or "9999"
        previous = latest.get(name)
        if previous is None or observed_at >= previous[0]:
            latest[name] = (observed_at, run)
    return {name: run for name, (_, run) in latest.items()}


def classify(
    check_runs: list[dict], required: tuple[str, ...] = REQUIRED_CHECKS
) -> EligibilitySnapshot:
    """Classify exact required checks without treating pending work as failure."""

    latest = latest_runs(check_runs)
    missing = tuple(name for name in required if name not in latest)
    pending = tuple(
        (name, str(latest[name].get("status") or "pending"))
        for name in required
        if name in latest and latest[name].get("status") != "completed"
    )
    failed = tuple(
        (name, latest[name].get("conclusion"))
        for name in required
        if name in latest
        and latest[name].get("status") == "completed"
        and latest[name].get("conclusion") != "success"
    )
    return EligibilitySnapshot(missing=missing, pending=pending, failed=failed)


def latest_conclusions(check_runs: list[dict]) -> dict[str, str | None]:
    """Return the conclusion of the most recently completed run per name.

    A run that has not completed (``completed_at`` missing) counts as the
    newest observation for its name and yields ``None`` so that an in-progress
    rerun never inherits an older success.
    """

    return {
        name: run.get("conclusion") if run.get("status") == "completed" else None
        for name, run in latest_runs(check_runs).items()
    }


def evaluate(
    check_runs: list[dict], required: tuple[str, ...] = REQUIRED_CHECKS
) -> tuple[list[str], list[tuple[str, str | None]]]:
    """Split the required names into missing and not-successful groups.

    Returns ``(missing, failing)`` where ``missing`` lists names without any
    check run on the commit and ``failing`` pairs names whose latest run did
    not conclude ``success`` with that conclusion (``None`` = not completed).
    """

    snapshot = classify(check_runs, required)
    failing = [(name, None) for name, _ in snapshot.pending]
    failing.extend(snapshot.failed)
    return list(snapshot.missing), failing


def wait_for_eligibility(
    fetch: Callable[[], list[dict]],
    *,
    required: tuple[str, ...] = REQUIRED_CHECKS,
    timeout_seconds: float,
    poll_seconds: float,
    clock: Callable[[], float] = time.monotonic,
    sleep: Callable[[float], None] = time.sleep,
    emit: Callable[[str], None] = lambda message: print(message, file=sys.stderr),
) -> tuple[EligibilitySnapshot, bool, float]:
    """Poll required checks until success, terminal failure, or timeout."""

    if timeout_seconds < 0 or poll_seconds <= 0:
        raise ValueError("timeout must be non-negative and poll interval must be positive")
    started = clock()
    deadline = started + timeout_seconds
    while True:
        snapshot = classify(fetch(), required)
        elapsed = clock() - started
        if snapshot.eligible or snapshot.failed:
            return snapshot, False, elapsed
        remaining = deadline - clock()
        if remaining <= 0:
            return snapshot, True, elapsed
        waiting = [*snapshot.missing, *(name for name, _ in snapshot.pending)]
        emit(
            f"waiting for {len(waiting)} required checks "
            f"({remaining / 60:.1f} minutes remaining): {', '.join(waiting)}"
        )
        sleep(min(poll_seconds, remaining))


def fetch_check_runs(repo: str, sha: str) -> list[dict]:
    """Fetch every check run recorded on the commit through ``gh api``."""

    completed = subprocess.run(
        [
            "gh",
            "api",
            "--paginate",
            f"repos/{repo}/commits/{sha}/check-runs?per_page=100",
            "--jq",
            ".check_runs[]",
        ],
        check=True,
        capture_output=True,
        text=True,
    )
    return [json.loads(line) for line in completed.stdout.splitlines() if line.strip()]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--repo", required=True, help="OWNER/NAME")
    parser.add_argument("--sha", required=True, help="commit to evaluate")
    parser.add_argument("--json", action="store_true", help="print a JSON report")
    parser.add_argument(
        "--wait-minutes",
        type=float,
        default=0,
        help="wait this many minutes for missing or pending checks",
    )
    parser.add_argument(
        "--poll-seconds", type=float, default=30, help="check polling interval"
    )
    arguments = parser.parse_args()

    def fetch() -> list[dict]:
        return fetch_check_runs(arguments.repo, arguments.sha)

    if arguments.wait_minutes > 0:
        snapshot, timed_out, elapsed = wait_for_eligibility(
            fetch,
            timeout_seconds=arguments.wait_minutes * 60,
            poll_seconds=arguments.poll_seconds,
        )
    else:
        snapshot = classify(fetch())
        timed_out = False
        elapsed = 0.0
    eligible = snapshot.eligible
    if arguments.json:
        print(
            json.dumps(
                {
                    "sha": arguments.sha,
                    "eligible": eligible,
                    "timed_out": timed_out,
                    "elapsed_seconds": elapsed,
                    "missing": snapshot.missing,
                    "pending": [
                        {"name": name, "status": status} for name, status in snapshot.pending
                    ],
                    "failing": [
                        {"name": name, "conclusion": conclusion}
                        for name, conclusion in snapshot.failed
                    ],
                }
            )
        )
    else:
        for name in REQUIRED_CHECKS:
            if name in snapshot.missing:
                state = "MISSING"
            elif pending := next(
                (status for pending_name, status in snapshot.pending if pending_name == name),
                None,
            ):
                state = pending
            else:
                state = next(
                    (
                        conclusion or "failure"
                        for failed_name, conclusion in snapshot.failed
                        if failed_name == name
                    ),
                    "success",
                )
            print(f"{state:>10}  {name}")
        if timed_out:
            verdict = "NOT eligible (wait timed out)"
        else:
            verdict = "eligible" if eligible else "NOT eligible"
        print(f"release candidate {arguments.sha} is {verdict}")
    return 0 if eligible else 1


if __name__ == "__main__":
    sys.exit(main())
