"""Offline security-policy checks for the credentialed anyr live-gate workflow.

These assertions pin the reviewed security posture of the workflow: which
event sources may start a credentialed gate, action pinning, runner
provenance, and the secret-safety of the gate's failure paths. Workflow
shape (job layout, scheduling triggers, the exact admitted-test inventory)
is deliberately not asserted here and may evolve without review.
"""

from pathlib import Path
import unittest


class LiveWorkflowPolicyTests(unittest.TestCase):
    def test_credentialed_gate_keeps_reviewed_security_invariants(self):
        root = Path(__file__).resolve().parents[2]
        workflow = (root / ".github/workflows/anyr-anyback-live.yml").read_text(
            encoding="utf-8"
        )
        # A fork pull request must never start a credentialed live gate.
        self.assertNotIn("  pull_request:\n", workflow)
        self.assertNotIn("pull_request_target", workflow)
        # Every action reference must be pinned to a full commit SHA.
        for line in workflow.splitlines():
            stripped = line.strip()
            if stripped.startswith("- uses:") or stripped.startswith("uses:"):
                reference = stripped.split("@", 1)[1].split()[0]
                self.assertRegex(reference, r"^[0-9a-f]{40}$", stripped)
        # The gate provisions its own disposable server; never a shared host.
        self.assertNotIn("self-hosted", workflow)
        # Gate state is private and removed even on failure.
        self.assertIn("umask 077", workflow)
        self.assertIn("trap 'rm -rf -- \"$gate_dir\"' EXIT", workflow)
        # Failure evidence is bounded and never streamed unredacted.
        self.assertIn("tail -c 65536", workflow)
        self.assertNotIn("tee", workflow)
        # The failure category is trusted only from a private, owned,
        # non-symlinked, bounded file.
        self.assertIn('! -L "$category_file"', workflow)
        self.assertIn("stat -c '%u' -- \"$category_file\"", workflow)
        self.assertIn("stat -c '%a' -- \"$category_file\"", workflow)
        self.assertIn("stat -c '%s' -- \"$category_file\"", workflow)


if __name__ == "__main__":
    unittest.main()
