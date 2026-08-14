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

    def test_secret_failure_transcript_is_never_emitted(self) -> None:
        result = self.invoke(
            "test",
            "discussions",
            "print('PRIVATE_SECRET'); raise SystemExit(2)",
        )
        self.assertEqual(result.returncode, 1)
        self.assertNotIn("PRIVATE_SECRET", result.stdout + result.stderr)
        self.assertEqual(
            result.stderr,
            "required live gate discussions failed reason=child_exit\n",
        )

    def test_failed_test_name_is_reported_without_its_transcript(self) -> None:
        result = self.invoke(
            "test",
            "discussions",
            "print('test module_name::case_name ... FAILED'); print('PRIVATE_SECRET'); raise SystemExit(2)",
        )
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, "")
        self.assertNotIn("PRIVATE_SECRET", result.stderr)
        self.assertEqual(
            result.stderr,
            "required live gate discussions failed reason=child_exit tests=module_name::case_name\n",
        )

    def test_only_typed_disposable_skip_reasons_fail_admission(self) -> None:
        harmless = self.invoke(
            "test",
            "discussions",
            "print('ordinary operation skipped a redundant read PRIVATE_SECRET'); print('test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s')",
        )
        self.assertEqual(harmless.returncode, 0)
        self.assertNotIn("PRIVATE_SECRET", harmless.stdout + harmless.stderr)
        self.assertEqual(harmless.stdout, "required live gate discussions completed\n")

        admission = self.invoke(
            "test",
            "discussions",
            "print('disposable workflow skipped before callback: PrefixNotConfigured'); print('test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s')",
        )
        self.assertEqual(admission.returncode, 1)
        self.assertEqual(
            admission.stderr,
            "required live gate discussions failed reason=skipped_admission\n",
        )

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
        self.assertEqual(
            result.stderr,
            "required live gate discussions failed reason=runner_bound\n",
        )


class EvidenceTests(unittest.TestCase):
    @unittest.skipUnless(os.name == "posix", "POSIX ownership and mode policy")
    def test_non_private_source_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, context = root / "source", root / "context"
            source.write_bytes(b"reviewed event\n")
            source.chmod(0o640)
            with self.assertRaises(OSError):
                with contextlib.redirect_stdout(io.StringIO()):
                    reviewed_evidence.start(source, context)

    def test_only_post_start_bytes_are_captured(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, context, artifact = (
                root / "source",
                root / "context",
                root / "artifact",
            )
            source.write_bytes(b"stale-allowlisted-event\n")
            source.chmod(0o600)
            with contextlib.redirect_stdout(io.StringIO()):
                reviewed_evidence.start(source, context)
            with source.open("ab") as output:
                output.write(b'{"severity":"info","category":"body_acceptance"}\n')
            reviewed_evidence.capture(source, context, artifact)
            payload = artifact.read_bytes()
            self.assertNotIn(b"stale-allowlisted-event", payload)
            self.assertNotIn(b"body_acceptance", payload)
            self.assertEqual(
                payload,
                b"any-mcp reviewed failure evidence\nreviewed_log_valid\nevent_count=1\n",
            )
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
                source.chmod(0o600)
                with contextlib.redirect_stdout(io.StringIO()):
                    reviewed_evidence.start(source, context)
                if mutation == "replace":
                    replacement = root / "replacement"
                    replacement.write_bytes(b"PRIVATE_REPLACEMENT")
                    replacement.chmod(0o600)
                    os.replace(replacement, source)
                else:
                    source.write_bytes(b"changed-anchor-PRIVATE")
                reviewed_evidence.capture(source, context, artifact)
                self.assertEqual(
                    artifact.read_bytes(),
                    b"any-mcp reviewed failure evidence\nreviewed_log_unavailable\nevent_count=0\n",
                )

    def test_invalid_or_credential_like_fresh_bytes_are_never_disclosed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name, fresh in [
                ("malformed", b"PRIVATE_MALFORMED\n"),
                (
                    "duplicate",
                    b'{"severity":"info","severity":"error","category":"safe"}\n',
                ),
                (
                    "credential",
                    b'{"severity":"info","category":"bearer PRIVATE_SECRET"}\n',
                ),
            ]:
                source, context, artifact = (
                    root / f"source-{name}",
                    root / f"context-{name}",
                    root / f"artifact-{name}",
                )
                source.write_bytes(b"baseline\n")
                source.chmod(0o600)
                with contextlib.redirect_stdout(io.StringIO()):
                    reviewed_evidence.start(source, context)
                with source.open("ab") as output:
                    output.write(fresh)
                reviewed_evidence.capture(source, context, artifact)
                self.assertEqual(
                    artifact.read_bytes(),
                    b"any-mcp reviewed failure evidence\nreviewed_log_invalid\nevent_count=0\n",
                )


if __name__ == "__main__":
    unittest.main()
