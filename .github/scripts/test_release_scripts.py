#!/usr/bin/env python3
"""Offline unit tests for the release eligibility and qualification scripts."""

from __future__ import annotations

import importlib.util
import pathlib
import unittest


def load(name: str):
    path = pathlib.Path(__file__).with_name(f"{name}.py")
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


eligibility = load("release_eligibility")
qual = load("release_qual")


def run(
    name: str, conclusion: str | None, completed_at: str | None, status: str = "completed"
):
    return {
        "name": name,
        "status": status,
        "conclusion": conclusion,
        "completed_at": completed_at,
    }


class EligibilityTests(unittest.TestCase):
    def test_every_required_check_green_is_eligible(self):
        runs = [
            run(name, "success", "2026-08-19T10:00:00Z")
            for name in eligibility.REQUIRED_CHECKS
        ]
        self.assertEqual(eligibility.evaluate(runs), ([], []))

    def test_missing_and_failed_checks_are_reported_separately(self):
        runs = [
            run(name, "success", "2026-08-19T10:00:00Z")
            for name in eligibility.REQUIRED_CHECKS
        ]
        runs = [r for r in runs if r["name"] != "stdio to headless Anytype"]
        runs = [
            run("native gates (macos-aarch64)", "failure", "2026-08-19T10:00:00Z")
            if r["name"] == "native gates (macos-aarch64)"
            else r
            for r in runs
        ]
        missing, failing = eligibility.evaluate(runs)
        self.assertEqual(missing, ["stdio to headless Anytype"])
        self.assertEqual(failing, [("native gates (macos-aarch64)", "failure")])

    def test_latest_rerun_wins_over_an_older_success(self):
        runs = [
            run("clippy (linux-aarch64)", "success", "2026-08-19T10:00:00Z"),
            run("clippy (linux-aarch64)", "failure", "2026-08-19T11:00:00Z"),
        ]
        self.assertEqual(
            eligibility.latest_conclusions(runs), {"clippy (linux-aarch64)": "failure"}
        )

    def test_in_progress_rerun_masks_an_older_success(self):
        runs = [
            run("clippy (linux-aarch64)", "success", "2026-08-19T10:00:00Z"),
            run("clippy (linux-aarch64)", None, None, status="in_progress"),
        ]
        self.assertEqual(
            eligibility.latest_conclusions(runs), {"clippy (linux-aarch64)": None}
        )
        _, failing = eligibility.evaluate(runs, ("clippy (linux-aarch64)",))
        self.assertEqual(failing, [("clippy (linux-aarch64)", None)])

    def test_unrelated_check_runs_are_ignored(self):
        runs = [
            run(name, "success", "2026-08-19T10:00:00Z")
            for name in eligibility.REQUIRED_CHECKS
        ]
        runs.append(run("build-global-artifacts", "failure", "2026-08-19T10:00:00Z"))
        self.assertEqual(eligibility.evaluate(runs), ([], []))


class QualificationTests(unittest.TestCase):
    def test_test_stage_dispatches_the_five_verification_workflows_with_full_inputs(self):
        plan = qual.stage_plan("test", "main", "all", "required")
        self.assertEqual(
            [name for name, _ in plan],
            [
                "smoke.yml",
                "ci.yml",
                "any-mcp.yml",
                "anytype-api-live.yml",
                "anyr-anyback-live.yml",
            ],
        )
        inputs = dict(plan)
        self.assertEqual(inputs["ci.yml"], {"platform": "all"})
        self.assertEqual(inputs["any-mcp.yml"], {"tier": "all"})
        self.assertEqual(inputs["anytype-api-live.yml"], {"tier": "required"})

    def test_build_stage_runs_artifacts_and_dist_plan_on_the_candidate_ref(self):
        plan = dict(qual.stage_plan("build", "release-candidate", "all", "all"))
        self.assertEqual(plan["build.yml"], {"platform": "all"})
        self.assertEqual(
            plan["release.yml"], {"source_ref": "release-candidate", "architecture": "all"}
        )

    def test_unknown_stage_is_rejected(self):
        with self.assertRaises(ValueError):
            qual.stage_plan("publish", "main", "all", "all")

    def test_dispatched_run_selection_ignores_older_runs(self):
        runs = [
            {"databaseId": 3, "createdAt": "2026-08-19T10:00:30Z"},
            {"databaseId": 2, "createdAt": "2026-08-19T10:00:10Z"},
            {"databaseId": 1, "createdAt": "2026-08-19T09:59:00Z"},
        ]
        selected = qual.select_dispatched_run(runs, "2026-08-19T10:00:05Z")
        self.assertEqual(selected["databaseId"], 3)
        self.assertIsNone(qual.select_dispatched_run(runs, "2026-08-19T10:01:00Z"))

    def test_outcome_requires_every_run_to_succeed(self):
        runs = [
            {"workflow": "ci.yml", "status": "completed", "conclusion": "success"},
            {"workflow": "smoke.yml", "status": "completed", "conclusion": "failure"},
            {"workflow": "any-mcp.yml", "status": "in_progress", "conclusion": None},
        ]
        self.assertEqual(qual.outcome(runs), (False, ["smoke.yml", "any-mcp.yml"]))
        self.assertEqual(qual.outcome(runs[:1]), (True, []))


if __name__ == "__main__":
    unittest.main()
