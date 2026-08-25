#!/usr/bin/env python3
"""Run one release-qualification stage by dispatching workflows and waiting.

``release-qual.yml`` calls this twice: the ``test`` stage dispatches the five
verification workflows on the candidate ref (full platform matrix, live
tiers included) and the ``build`` stage, which runs only after the test stage
is green, dispatches the general artifact build. Main-branch release candidates
are produced automatically by ``release.yml`` and are not rebuilt here. Each
stage waits for every dispatched run to complete and fails unless all of them
succeed.

Usage: release_qual.py --repo OWNER/NAME --ref BRANCH --stage test|build
                       [--any-mcp-tier TIER] [--anytype-api-tier TIER]
                       [--poll-seconds N] [--timeout-minutes N]

Dispatching uses the ``gh`` CLI with ``GH_TOKEN`` (the repository token may
start ``workflow_dispatch`` runs). Planning and run selection are pure and
unit-tested; only dispatch/poll touch the network.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import subprocess
import sys
import time

STAGES: tuple[str, ...] = ("test", "build")


def stage_plan(
    stage: str, ref: str, any_mcp_tier: str, anytype_api_tier: str
) -> list[tuple[str, dict[str, str]]]:
    """Return the workflows and dispatch inputs for one stage."""

    if stage == "test":
        return [
            ("smoke.yml", {}),
            ("ci.yml", {"platform": "all"}),
            ("any-mcp.yml", {"tier": any_mcp_tier}),
            ("anytype-api-live.yml", {"tier": anytype_api_tier}),
            ("anyr-anyback-live.yml", {}),
        ]
    if stage == "build":
        return [("build.yml", {"platform": "all"})]
    raise ValueError(f"unknown stage: {stage}")


def select_dispatched_run(runs: list[dict], dispatched_at: str) -> dict | None:
    """Pick the newest run created at or after the dispatch time.

    ``runs`` is a ``gh run list`` JSON array (newest first). Timestamps are
    RFC 3339 in UTC, so lexical comparison orders them correctly.
    """

    candidates = [
        run
        for run in runs
        if isinstance(run.get("createdAt"), str) and run["createdAt"] >= dispatched_at
    ]
    if not candidates:
        return None
    return max(candidates, key=lambda run: run["createdAt"])


def outcome(runs: list[dict]) -> tuple[bool, list[str]]:
    """Return whether every run completed successfully and the failed names."""

    failed = [
        str(run.get("workflow"))
        for run in runs
        if run.get("status") != "completed" or run.get("conclusion") != "success"
    ]
    return (not failed, failed)


def gh(*arguments: str) -> str:
    completed = subprocess.run(["gh", *arguments], check=True, capture_output=True, text=True)
    return completed.stdout


def dispatch(repo: str, ref: str, workflow: str, inputs: dict[str, str]) -> dict:
    dispatched_at = (dt.datetime.now(dt.UTC) - dt.timedelta(seconds=5)).strftime(
        "%Y-%m-%dT%H:%M:%SZ"
    )
    arguments = ["workflow", "run", workflow, "--repo", repo, "--ref", ref]
    for key, value in inputs.items():
        arguments += ["-f", f"{key}={value}"]
    gh(*arguments)
    for _ in range(30):
        time.sleep(5)
        listing = json.loads(
            gh(
                "run",
                "list",
                "--repo",
                repo,
                "--workflow",
                workflow,
                "--event",
                "workflow_dispatch",
                "--branch",
                ref,
                "--limit",
                "20",
                "--json",
                "databaseId,createdAt,status,conclusion,url,headSha",
            )
        )
        run = select_dispatched_run(listing, dispatched_at)
        if run is not None:
            run["workflow"] = workflow
            print(f"dispatched {workflow}: {run['url']}", flush=True)
            return run
    raise RuntimeError(f"dispatched {workflow} but no run appeared for {ref}")


def wait(repo: str, runs: list[dict], poll_seconds: int, timeout_minutes: int) -> list[dict]:
    deadline = time.monotonic() + timeout_minutes * 60
    pending = {run["databaseId"]: run for run in runs}
    finished: list[dict] = []
    while pending:
        if time.monotonic() >= deadline:
            raise RuntimeError(
                "stage timed out waiting for: "
                + ", ".join(run["workflow"] for run in pending.values())
            )
        time.sleep(poll_seconds)
        for run_id in list(pending):
            state = json.loads(
                gh(
                    "run",
                    "view",
                    str(run_id),
                    "--repo",
                    repo,
                    "--json",
                    "status,conclusion",
                )
            )
            if state.get("status") == "completed":
                run = pending.pop(run_id)
                run.update(state)
                finished.append(run)
                print(
                    f"{run['workflow']}: {run.get('conclusion')} ({run['url']})",
                    flush=True,
                )
    return finished


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--repo", required=True)
    parser.add_argument("--ref", required=True, help="branch holding the candidate")
    parser.add_argument("--stage", required=True, choices=STAGES)
    parser.add_argument("--any-mcp-tier", default="all")
    parser.add_argument("--anytype-api-tier", default="all")
    parser.add_argument("--poll-seconds", type=int, default=60)
    parser.add_argument("--timeout-minutes", type=int, default=150)
    arguments = parser.parse_args()

    plan = stage_plan(
        arguments.stage, arguments.ref, arguments.any_mcp_tier, arguments.anytype_api_tier
    )
    runs = [dispatch(arguments.repo, arguments.ref, name, inputs) for name, inputs in plan]
    finished = wait(arguments.repo, runs, arguments.poll_seconds, arguments.timeout_minutes)
    ok, failed = outcome(finished)
    if not ok:
        print(f"{arguments.stage} stage failed: {', '.join(failed)}", file=sys.stderr)
        return 1
    print(f"{arguments.stage} stage green for {arguments.ref}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
