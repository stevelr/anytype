"""Offline security-policy checks for the credentialed anyr live-gate workflow.

These assertions pin the reviewed security posture of the workflow: which
event sources may start a credentialed gate, action pinning, runner
provenance, and the secret-safety of the gate's failure paths. Workflow
shape (job layout, scheduling triggers, the exact admitted-test inventory)
is deliberately not asserted here and may evolve without review.
"""

from pathlib import Path
import unittest


def top_level_mapping(document: str, name: str) -> dict[str, str]:
    """Return direct scalar or nested keys from one top-level YAML mapping."""
    marker = f"{name}:"
    in_mapping = False
    entries: dict[str, str] = {}
    for raw_line in document.splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        indent = len(raw_line) - len(raw_line.lstrip(" "))
        if not in_mapping:
            if indent == 0 and line == marker:
                in_mapping = True
            continue
        if indent == 0:
            break
        if indent != 2 or line.startswith("-"):
            continue
        key, separator, value = line.partition(":")
        if not separator:
            raise AssertionError(f"invalid {name} mapping entry {line!r}")
        if key in entries:
            raise AssertionError(f"duplicate {name} mapping key {key!r}")
        entries[key] = value.strip()
    if not in_mapping:
        raise AssertionError(f"workflow has no top-level {name} mapping")
    return entries


def permission_values(document: str) -> list[str]:
    """Return direct values from every workflow permission mapping."""
    lines = document.splitlines()
    values: list[str] = []
    for index, raw_line in enumerate(lines):
        if raw_line.strip() != "permissions:":
            continue
        parent_indent = len(raw_line) - len(raw_line.lstrip(" "))
        for child in lines[index + 1 :]:
            line = child.strip()
            if not line or line.startswith("#"):
                continue
            indent = len(child) - len(child.lstrip(" "))
            if indent <= parent_indent:
                break
            if indent == parent_indent + 2:
                _, separator, value = line.partition(":")
                if not separator:
                    raise AssertionError(f"invalid permissions entry {line!r}")
                values.append(value.strip())
    return values


class LiveWorkflowPolicyTests(unittest.TestCase):
    def test_credentialed_gate_keeps_reviewed_security_invariants(self):
        root = Path(__file__).resolve().parents[2]
        workflow = (root / ".github/workflows/anyr-anyback-live.yml").read_text(
            encoding="utf-8"
        )
        # Only reviewed repository events may start a credentialed live gate.
        self.assertEqual(
            set(top_level_mapping(workflow, "on")),
            {"push", "schedule", "workflow_dispatch"},
        )
        self.assertIn("  push:\n    branches:\n      - main\n", workflow)
        self.assertNotIn("pull_request_target", workflow)
        self.assertEqual(top_level_mapping(workflow, "permissions"), {"contents": "read"})
        self.assertTrue(all(value == "read" for value in permission_values(workflow)))
        # Every action reference must be pinned to a full commit SHA.
        for line in workflow.splitlines():
            stripped = line.strip()
            if stripped.startswith("- uses:") or stripped.startswith("uses:"):
                reference = stripped.split("@", 1)[1].split()[0]
                self.assertRegex(reference, r"^[0-9a-f]{40}$", stripped)
        self.assertEqual(
            workflow.count("actions/checkout@"),
            workflow.count("persist-credentials: false"),
        )
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
