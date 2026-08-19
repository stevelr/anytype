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
import json
import subprocess
import sys

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
    "native gates (archlinux-x86_64)",
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


def latest_conclusions(check_runs: list[dict]) -> dict[str, str | None]:
    """Return the conclusion of the most recently completed run per name.

    A run that has not completed (``completed_at`` missing) counts as the
    newest observation for its name and yields ``None`` so that an in-progress
    rerun never inherits an older success.
    """

    latest: dict[str, tuple[str, str | None]] = {}
    for run in check_runs:
        name = run.get("name")
        if not isinstance(name, str):
            continue
        completed_at = run.get("completed_at") or "9999-99-99T99:99:99Z"
        conclusion = run.get("conclusion") if run.get("status") == "completed" else None
        previous = latest.get(name)
        if previous is None or completed_at >= previous[0]:
            latest[name] = (completed_at, conclusion)
    return {name: conclusion for name, (_, conclusion) in latest.items()}


def evaluate(
    check_runs: list[dict], required: tuple[str, ...] = REQUIRED_CHECKS
) -> tuple[list[str], list[tuple[str, str | None]]]:
    """Split the required names into missing and not-successful groups.

    Returns ``(missing, failing)`` where ``missing`` lists names without any
    check run on the commit and ``failing`` pairs names whose latest run did
    not conclude ``success`` with that conclusion (``None`` = not completed).
    """

    conclusions = latest_conclusions(check_runs)
    missing = [name for name in required if name not in conclusions]
    failing = [
        (name, conclusions[name])
        for name in required
        if name in conclusions and conclusions[name] != "success"
    ]
    return missing, failing


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
    arguments = parser.parse_args()

    check_runs = fetch_check_runs(arguments.repo, arguments.sha)
    missing, failing = evaluate(check_runs)
    eligible = not missing and not failing
    if arguments.json:
        print(
            json.dumps(
                {
                    "sha": arguments.sha,
                    "eligible": eligible,
                    "missing": missing,
                    "failing": [{"name": n, "conclusion": c} for n, c in failing],
                }
            )
        )
    else:
        for name in REQUIRED_CHECKS:
            if name in missing:
                state = "MISSING"
            else:
                state = next((c or "incomplete" for n, c in failing if n == name), "success")
            print(f"{state:>10}  {name}")
        verdict = "eligible" if eligible else "NOT eligible"
        print(f"release candidate {arguments.sha} is {verdict}")
    return 0 if eligible else 1


if __name__ == "__main__":
    sys.exit(main())
