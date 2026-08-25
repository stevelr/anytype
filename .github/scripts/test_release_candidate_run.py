#!/usr/bin/env python3
"""Unit tests for release-candidate producer-run selection."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import unittest


def load_module():
    path = Path(__file__).with_name("release_candidate_run.py")
    spec = importlib.util.spec_from_file_location("release_candidate_run", path)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


selector = load_module()


def run(run_id: int, *, created: str, **overrides):
    candidate = {
        "id": run_id,
        "created_at": created,
        "head_sha": "a" * 40,
        "head_branch": "main",
        "event": "push",
        "status": "completed",
        "conclusion": "success",
        "path": ".github/workflows/release.yml@main",
    }
    candidate.update(overrides)
    return candidate


class ReleaseCandidateRunTests(unittest.TestCase):
    def test_newest_exact_producer_run_is_selected(self):
        document = {
            "workflow_runs": [
                run(12, created="2026-08-25T10:00:00Z"),
                run(14, created="2026-08-25T11:00:00Z"),
            ]
        }
        self.assertEqual(selector.select_run(document, "a" * 40), 14)

    def test_wrong_commit_and_untrusted_producer_are_rejected(self):
        document = {
            "workflow_runs": [
                run(20, created="2026-08-25T12:00:00Z", head_sha="b" * 40),
                run(19, created="2026-08-25T11:00:00Z", head_branch="feature"),
                run(18, created="2026-08-25T10:00:00Z", event="workflow_dispatch"),
                run(17, created="2026-08-25T09:00:00Z", conclusion="failure"),
                run(16, created="2026-08-25T08:00:00Z", path="other.yml"),
            ]
        }
        with self.assertRaisesRegex(ValueError, "no successful"):
            selector.select_run(document, "a" * 40)

    def test_malformed_response_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "run list"):
            selector.select_run({}, "a" * 40)


if __name__ == "__main__":
    unittest.main()
