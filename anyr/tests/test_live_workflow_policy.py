"""Offline policy checks for the protected anyr live-gate workflow cell."""

from pathlib import Path
import unittest


class LiveWorkflowPolicyTests(unittest.TestCase):
    def test_required_cell_admits_exact_tests_with_private_cleanup(self):
        root = Path(__file__).resolve().parents[2]
        workflow = (root / ".github/workflows/anyr-anyback-live.yml").read_text(
            encoding="utf-8"
        )
        self.assertIn("group: anytype-headless-live", workflow)
        self.assertIn("trap 'rm -rf -- \"$gate_dir\"' EXIT", workflow)
        self.assertIn("ANYR_PY_REQUIRE_LIVE=1", workflow)
        self.assertIn("python3 anyr/tests/run_required_python_cli.py", workflow)
        self.assertIn(
            "cargo test --locked -p anyr --bins cli::types::tests::live_add_property_preserves_exact_replaceable_set -- ",
            workflow,
        )
        self.assertIn("--ignored --exact --test-threads=1 --nocapture", workflow)
        self.assertIn("live_gate_policy.py python", workflow)
        self.assertIn("live_gate_policy.py rust", workflow)
        self.assertNotIn("tee", workflow)
        self.assertIn("required anyr live gate failed", workflow)
        self.assertIn("required anyr type-property gate failed", workflow)


if __name__ == "__main__":
    unittest.main()
