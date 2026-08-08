#!/usr/bin/env python3
"""Offline regression tests for the private live-gate helpers."""

import importlib.util
import contextlib
import io
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


SCRIPT_DIR = Path(__file__).parent
sys.dont_write_bytecode = True
RUNNER = SCRIPT_DIR / "run-live-gate.py"
EVIDENCE = SCRIPT_DIR / "reviewed-evidence.py"
spec = importlib.util.spec_from_file_location("reviewed_evidence", EVIDENCE)
assert spec is not None and spec.loader is not None
reviewed_evidence = importlib.util.module_from_spec(spec)
spec.loader.exec_module(reviewed_evidence)


class RunnerTests(unittest.TestCase):
    def invoke(
        self, mode: str, label: str, child: str
    ) -> subprocess.CompletedProcess[str]:
        with tempfile.TemporaryDirectory() as private:
            environment = dict(os.environ, ANY_MCP_LIVE_PRIVATE_DIR=private)
            result = subprocess.run(
                [
                    sys.executable,
                    RUNNER,
                    mode,
                    label,
                    "--",
                    sys.executable,
                    "-c",
                    child,
                ],
                text=True,
                capture_output=True,
                env=environment,
                check=False,
            )
            self.assertEqual(list(Path(private).iterdir()), [])
            return result

    def test_valid_test_emits_only_fixed_completion(self) -> None:
        result = self.invoke(
            "test",
            "discussions",
            "print('test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 3 filtered out; finished in 0.01s')",
        )
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "required live gate discussions completed\n")
        self.assertEqual(result.stderr, "")

    def test_secret_and_skip_transcripts_are_never_emitted(self) -> None:
        for child in [
            "print('PRIVATE_SECRET'); raise SystemExit(2)",
            "print('skipped PRIVATE_SECRET'); print('test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s')",
        ]:
            result = self.invoke("test", "discussions", child)
            self.assertEqual(result.returncode, 1)
            self.assertNotIn("PRIVATE_SECRET", result.stdout + result.stderr)
            self.assertEqual(result.stderr, "required live gate discussions failed\n")

    def test_zero_multiple_and_replacement_counts_fail(self) -> None:
        for child in [
            "print('no summary')",
            "print('test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s')",
            "print('test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s')",
        ]:
            self.assertEqual(self.invoke("test", "discussions", child).returncode, 1)

    def test_oversized_transcript_is_bounded_and_private(self) -> None:
        result = self.invoke(
            "test",
            "discussions",
            "import sys; sys.stdout.write('PRIVATE_SECRET' + 'x' * 1048576)",
        )
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, "")
        self.assertEqual(result.stderr, "required live gate discussions failed\n")


class EvidenceTests(unittest.TestCase):
    def test_only_post_start_bytes_are_captured(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, context, artifact = (
                root / "source",
                root / "context",
                root / "artifact",
            )
            source.write_bytes(b"stale-allowlisted-event\n")
            with contextlib.redirect_stdout(io.StringIO()):
                reviewed_evidence.start(source, context)
            with source.open("ab") as output:
                output.write(b"fresh-event\n")
            reviewed_evidence.capture(source, context, artifact)
            payload = artifact.read_bytes()
            self.assertNotIn(b"stale-allowlisted-event", payload)
            self.assertIn(b"fresh-event", payload)
            self.assertLessEqual(len(payload), 65_536)
            self.assertEqual(artifact.stat().st_mode & 0o777, 0o600)

    def test_replaced_or_pre_start_modified_source_is_not_disclosed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for mutation in ("replace", "modify"):
                source, context = (
                    root / f"source-{mutation}",
                    root / f"context-{mutation}",
                )
                artifact = root / f"artifact-{mutation}"
                source.write_bytes(b"original-anchor")
                with contextlib.redirect_stdout(io.StringIO()):
                    reviewed_evidence.start(source, context)
                if mutation == "replace":
                    replacement = root / "replacement"
                    replacement.write_bytes(b"PRIVATE_REPLACEMENT")
                    os.replace(replacement, source)
                else:
                    source.write_bytes(b"changed-anchor-PRIVATE")
                reviewed_evidence.capture(source, context, artifact)
                self.assertEqual(
                    artifact.read_bytes(),
                    b"any-mcp reviewed failure evidence\nreviewed-source=unavailable\n",
                )


if __name__ == "__main__":
    unittest.main()
