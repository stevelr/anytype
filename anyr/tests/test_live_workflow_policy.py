"""Offline policy checks for the protected anyr live-gate workflow cell."""

from pathlib import Path
import unittest


class LiveWorkflowPolicyTests(unittest.TestCase):
    def test_required_cell_admits_exact_tests_with_private_cleanup(self):
        root = Path(__file__).resolve().parents[2]
        workflow = (root / ".github/workflows/anyr-anyback-live.yml").read_text(
            encoding="utf-8"
        )
        self.assertIn("  workflow_dispatch:\n", workflow)
        self.assertNotIn("  pull_request:\n", workflow)
        self.assertNotIn("  push:\n", workflow)
        self.assertNotIn("  schedule:\n", workflow)
        self.assertNotRegex(workflow, r"uses:\s+\S+@v\d")
        # The disposable per-runner server replaced the retired self-hosted
        # anytype-headless runner: the gate provisions its own isolated
        # server and credentials instead of leasing a shared host.
        self.assertNotIn("self-hosted", workflow)
        self.assertIn(
            "provision-headless-server.sh ANYR_ANYBACK_HEADLESS", workflow
        )
        self.assertIn("trap 'rm -rf -- \"$gate_dir\"' EXIT", workflow)
        self.assertIn("ANYR_PY_REQUIRE_LIVE=1", workflow)
        self.assertIn("python3 -B -m anyr.tests.run_required_python_cli", workflow)
        self.assertIn("python3 -B anyr/tests/live_gate_policy.py python", workflow)
        self.assertIn("python3 -B anyr/tests/live_gate_policy.py rust", workflow)
        self.assertIn("--category-file \"$category_file\"", workflow)
        self.assertIn("stat -c '%u' -- \"$category_file\"", workflow)
        self.assertIn("stat -c '%a' -- \"$category_file\"", workflow)
        self.assertIn("stat -c '%s' -- \"$category_file\"", workflow)
        self.assertIn("! -L \"$category_file\"", workflow)
        self.assertIn(
            "inventory-invalid|create-ambiguous|identity-mismatch|cleanup-failed|required-test-failed",
            workflow,
        )
        self.assertIn("rm -f -- \"$output\" \"$category_file\"", workflow)
        self.assertIn(
            "cargo test --locked -p anyr --bins cli::types::tests::live_add_property_preserves_exact_replaceable_set -- ",
            workflow,
        )
        self.assertIn("--ignored --exact --test-threads=1 --nocapture", workflow)
        self.assertIn("live_gate_policy.py python", workflow)
        self.assertIn("live_gate_policy.py rust", workflow)
        self.assertNotIn("tee", workflow)
        self.assertIn("required anyr live gate failed: %s", workflow)
        failure_cleanup = workflow.index('rm -f -- "$output" "$category_file"')
        failure_message = workflow.index(
            "printf 'required anyr live gate failed: %s\\n' \"$category\""
        )
        self.assertLess(failure_cleanup, failure_message)
        self.assertIn("required anyr type-property gate failed", workflow)


if __name__ == "__main__":
    unittest.main()
