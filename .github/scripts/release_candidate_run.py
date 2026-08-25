#!/usr/bin/env python3
"""Select the successful main release-candidate run for an exact commit."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import re
import sys


COMMIT = re.compile(r"^[0-9a-f]{40}$")
WORKFLOW = ".github/workflows/release.yml"


def select_run(document: dict, commit: str) -> int:
    """Return the newest trusted producer run ID for one exact commit."""

    if not COMMIT.fullmatch(commit):
        raise ValueError("candidate commit must be a lowercase 40-character SHA")
    runs = document.get("workflow_runs")
    if not isinstance(runs, list):
        raise ValueError("workflow-runs response has no run list")
    candidates = [
        run
        for run in runs
        if isinstance(run, dict)
        and run.get("head_sha") == commit
        and run.get("head_branch") == "main"
        and run.get("event") == "push"
        and run.get("status") == "completed"
        and run.get("conclusion") == "success"
        and run.get("path") in (WORKFLOW, f"{WORKFLOW}@main")
        and isinstance(run.get("id"), int)
        and run["id"] > 0
        and isinstance(run.get("created_at"), str)
    ]
    if not candidates:
        raise ValueError(f"no successful main release-candidate run found for {commit}")
    selected = max(candidates, key=lambda run: (run["created_at"], run["id"]))
    return selected["id"]


def main() -> int:
    """Select one run from a bounded GitHub workflow-runs response."""

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--commit", required=True)
    arguments = parser.parse_args()
    try:
        if (
            not arguments.input.is_file()
            or arguments.input.is_symlink()
            or arguments.input.stat().st_size > 10 * 1024 * 1024
        ):
            raise ValueError("workflow-runs response is missing, linked, or oversized")
        document = json.loads(arguments.input.read_text(encoding="utf-8"))
        if not isinstance(document, dict):
            raise ValueError("workflow-runs response must be a JSON object")
        print(select_run(document, arguments.commit))
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"release candidate run selection failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
