#!/usr/bin/env python3
"""Offline regression tests for the private live-gate helpers."""

import importlib.util
import contextlib
import io
import os
import stat
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path


SCRIPT_DIR = Path(__file__).parent
sys.dont_write_bytecode = True
RUNNER = SCRIPT_DIR / "run-live-gate.py"
EVIDENCE = SCRIPT_DIR / "reviewed-evidence.py"
REVIEWER = SCRIPT_DIR / "review-server-log.py"
spec = importlib.util.spec_from_file_location("reviewed_evidence", EVIDENCE)
assert spec is not None and spec.loader is not None
reviewed_evidence = importlib.util.module_from_spec(spec)
spec.loader.exec_module(reviewed_evidence)
reviewer_spec = importlib.util.spec_from_file_location("review_server_log", REVIEWER)
assert reviewer_spec is not None and reviewer_spec.loader is not None
review_server_log = importlib.util.module_from_spec(reviewer_spec)
reviewer_spec.loader.exec_module(review_server_log)


class RunnerTests(unittest.TestCase):
    def invoke(self, mode: str, label: str, child: str) -> subprocess.CompletedProcess[str]:
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

    def test_colored_failed_test_name_is_reported_without_its_transcript(self) -> None:
        result = self.invoke(
            "test",
            "discussions",
            "print('test module_name::colored_case ... \\x1b[31mFAILED\\x1b[0m'); print('PRIVATE_SECRET'); raise SystemExit(2)",
        )
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, "")
        self.assertNotIn("PRIVATE_SECRET", result.stderr)
        self.assertEqual(
            result.stderr,
            "required live gate discussions failed reason=child_exit tests=module_name::colored_case\n",
        )

    def invoke_with_diagnostics(
        self, label: str, child: str
    ) -> tuple[subprocess.CompletedProcess[str], str | None, int | None]:
        with tempfile.TemporaryDirectory() as private:
            diagnostics = Path(private) / "diagnostics"
            environment = dict(
                os.environ,
                ANY_MCP_LIVE_PRIVATE_DIR=private,
                ANY_MCP_LIVE_DIAGNOSTICS_DIR=str(diagnostics),
            )
            result = subprocess.run(
                [sys.executable, RUNNER, "test", label, "--", sys.executable, "-c", child],
                text=True,
                capture_output=True,
                env=environment,
                check=False,
            )
            entries = sorted(diagnostics.iterdir()) if diagnostics.exists() else []
            self.assertLessEqual(len(entries), 1)
            if entries:
                mode = os.stat(entries[0]).st_mode
                self.assertTrue(stat.S_ISREG(mode))
                self.assertEqual(stat.S_IMODE(mode), 0o600)
                self.assertEqual(entries[0].name, f"{label}-failure-diagnostics.txt")
                return result, entries[0].read_text(), stat.S_IMODE(diagnostics.stat().st_mode)
            return result, None, None

    def test_failure_diagnostics_echo_scrubbed_panics_and_retain_scrubbed_tail(self) -> None:
        jwt = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzZWVkIjoiYXdTbWNFYUkifQ.NBkENfTreAGZugc8GgrtB2TRV"
        child = (
            "print('test module_name::case_name ... FAILED');"
            "print('PRIVATE_SECRET');"
            "print('Authorization: Bearer abcdef');"
            f"print('stream {jwt} context canceled');"
            "print('object bafyreie376qinigjrf2plgbbsust6n6l3vdjdm7oreruqpt6girv4uqtem missing');"
            "print('marker 0607ae6aa45892ecb256f1a9a2b6b830ca6939d22fac4027da5461b9d6db3245');"
            "print('thread headless_stdio_all_optional_toolsets_compose_in_rw_and_preview_ro_children named');"
            "print('blob QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVphYmNkZWZnaGlqa2xtbm9wcXJzdHV2d3h5ejAxMjM0NTY3ODk=');"
            "print(\"thread 'module_name::case_name' (4242) panicked at any-mcp/tests/x.rs:12:9:\");"
            "print('assertion failed: spawn refused with PRIVATE_SECRET');"
            "print(\"thread 'other' panicked at src/lib.rs:1:1:\");"
            "print('plain message');"
            "raise SystemExit(101)"
        )
        result, diagnostics, directory_mode = self.invoke_with_diagnostics(
            "discussions", child
        )
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, "")
        self.assertNotIn("PRIVATE_SECRET", result.stderr)
        self.assertNotIn("abcdef", result.stderr)
        self.assertEqual(
            result.stderr.splitlines(),
            [
                "required live gate discussions panic: thread 'module_name::case_name' (4242) panicked at any-mcp/tests/x.rs:12:9: <redacted line>",
                "required live gate discussions panic: thread 'other' panicked at src/lib.rs:1:1: plain message",
                "required live gate discussions failed reason=child_exit tests=module_name::case_name",
            ],
        )
        self.assertIsNotNone(diagnostics)
        assert diagnostics is not None
        self.assertEqual(directory_mode, 0o700)
        self.assertIn("required live gate discussions failed reason=child_exit", diagnostics)
        self.assertNotIn("PRIVATE_SECRET", diagnostics)
        self.assertNotIn("Bearer", diagnostics)
        self.assertNotIn("abcdef", diagnostics)
        self.assertNotIn(jwt, diagnostics)
        self.assertNotIn("bafyreie376", diagnostics)
        self.assertNotIn("0607ae6aa458", diagnostics)
        self.assertIn("stream <jwt> context canceled", diagnostics)
        self.assertIn("object <cid> missing", diagnostics)
        self.assertIn("marker <hex>", diagnostics)
        self.assertIn(
            "thread headless_stdio_all_optional_toolsets_compose_in_rw_and_preview_ro_children named",
            diagnostics,
        )
        self.assertIn("blob <blob>", diagnostics)
        self.assertNotIn("QUJDREVG", diagnostics)
        self.assertIn("test module_name::case_name ... FAILED", diagnostics)
        self.assertIn("plain message", diagnostics)

    def test_failure_diagnostics_are_bounded_to_the_transcript_tail(self) -> None:
        child = (
            "import sys; sys.stdout.write('PRIVATE_SECRET\\n' + ('x' * 100 + '\\n') * 2000 + 'tail marker\\n');"
            "raise SystemExit(2)"
        )
        result, diagnostics, _ = self.invoke_with_diagnostics("discussions", child)
        self.assertEqual(result.returncode, 1)
        self.assertEqual(
            result.stderr, "required live gate discussions failed reason=child_exit\n"
        )
        assert diagnostics is not None
        self.assertLess(len(diagnostics), 70_000)
        self.assertNotIn("PRIVATE_SECRET", diagnostics)
        self.assertIn("tail marker", diagnostics)

    def test_successful_run_writes_no_diagnostics(self) -> None:
        result, diagnostics, _ = self.invoke_with_diagnostics(
            "discussions",
            "print('test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 3 filtered out; finished in 0.01s')",
        )
        self.assertEqual(result.returncode, 0)
        self.assertIsNone(diagnostics)

    def test_relative_diagnostics_dir_is_refused_without_transcript(self) -> None:
        with tempfile.TemporaryDirectory() as private:
            environment = dict(
                os.environ,
                ANY_MCP_LIVE_PRIVATE_DIR=private,
                ANY_MCP_LIVE_DIAGNOSTICS_DIR="relative",
            )
            result = subprocess.run(
                [
                    sys.executable,
                    RUNNER,
                    "test",
                    "discussions",
                    "--",
                    sys.executable,
                    "-c",
                    "print('PRIVATE_SECRET'); raise SystemExit(2)",
                ],
                text=True,
                capture_output=True,
                env=environment,
                check=False,
                cwd=private,
            )
            self.assertEqual(result.returncode, 1)
            self.assertNotIn("PRIVATE_SECRET", result.stdout + result.stderr)
            self.assertEqual(
                result.stderr,
                "required live gate discussions diagnostics unavailable\n"
                "required live gate discussions failed reason=child_exit\n",
            )
            self.assertEqual(list(Path(private).iterdir()), [])

    def test_private_libtest_log_reports_failure_without_transcript(self) -> None:
        result = self.invoke(
            "test",
            "discussions",
            "from pathlib import Path; import stat, sys; path = Path(sys.argv[-1]); assert sys.argv[-2] == '--logfile'; assert stat.S_IMODE(path.stat().st_mode) == 0o600; path.write_text('failed module_name::logged_case\\n'); print('PRIVATE_SECRET'); raise SystemExit(2)",
        )
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, "")
        self.assertNotIn("PRIVATE_SECRET", result.stderr)
        self.assertEqual(
            result.stderr,
            "required live gate discussions failed reason=child_exit tests=module_name::logged_case\n",
        )

    def test_private_libtest_log_reports_last_completed_test_after_abort(self) -> None:
        result = self.invoke(
            "test",
            "direct",
            "from pathlib import Path; import sys; Path(sys.argv[-1]).write_text('ok module_name::first_case\\n'); print('PRIVATE_SECRET'); raise SystemExit(2)",
        )
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, "")
        self.assertNotIn("PRIVATE_SECRET", result.stderr)
        self.assertEqual(
            result.stderr,
            "required live gate direct failed reason=child_exit last_completed=module_name::first_case completed=1\n",
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

    def test_unbounded_numeric_summary_fails_without_traceback_or_artifact(self) -> None:
        result = self.invoke(
            "test",
            "discussions",
            "print('test result: ok. ' + '9' * 5000 + ' passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s')",
        )
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, "")
        self.assertNotIn("Traceback", result.stderr)
        self.assertEqual(
            result.stderr,
            "required live gate discussions failed reason=test_count\n",
        )

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

    def test_runner_bound_removes_private_libtest_progress(self) -> None:
        result = self.invoke(
            "test",
            "discussions",
            "from pathlib import Path; import sys; Path(sys.argv[-1]).write_text('ok module_name::private_progress\\n'); sys.stdout.write('PRIVATE_SECRET' + 'x' * 1048576)",
        )
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, "")
        self.assertNotIn("PRIVATE_SECRET", result.stderr)
        self.assertEqual(
            result.stderr,
            "required live gate discussions failed reason=runner_bound\n",
        )

    def test_oversized_libtest_progress_is_bounded_and_removed(self) -> None:
        result = self.invoke(
            "test",
            "discussions",
            "from pathlib import Path; import sys; Path(sys.argv[-1]).write_bytes(b'PRIVATE_SECRET' + b'x' * 1048576); raise SystemExit(2)",
        )
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stdout, "")
        self.assertNotIn("PRIVATE_SECRET", result.stderr)
        self.assertEqual(
            result.stderr,
            "required live gate discussions failed reason=runner_bound\n",
        )

    @unittest.skipUnless(os.name == "posix", "POSIX logfile replacement policy")
    def test_special_file_replacements_are_not_opened_and_are_removed(self) -> None:
        replacements = [
            "path.unlink(); path.symlink_to('/dev/null')",
            "path.unlink(); os.mkfifo(path)",
            "path.unlink(); path.mkdir()",
        ]
        for replacement in replacements:
            result = self.invoke(
                "test",
                "discussions",
                f"from pathlib import Path; import os, sys; path = Path(sys.argv[-1]); {replacement}; print('PRIVATE_SECRET'); raise SystemExit(2)",
            )
            self.assertEqual(result.returncode, 1)
            self.assertEqual(result.stdout, "")
            self.assertNotIn("PRIVATE_SECRET", result.stderr)
            self.assertEqual(
                result.stderr,
                "required live gate discussions failed reason=runner_io\n",
            )


class ReviewerTests(unittest.TestCase):
    def test_raw_content_is_reduced_to_fixed_categories(self) -> None:
        cases = [
            (b'{"level":"INFO","msg":"PRIVATE_SECRET"}', b'"server_event"'),
            (b'{"level":"ERROR","msg":"bearer PRIVATE_SECRET"}', b'"server_error"'),
            (b"runtime panic PRIVATE_SECRET", b'"server_fatal"'),
        ]
        for raw, category in cases:
            reviewed = review_server_log.review_line(raw)
            self.assertIn(category, reviewed)
            self.assertNotIn(b"PRIVATE_SECRET", reviewed)
            self.assertLessEqual(len(reviewed), 256)

    def test_oversized_input_has_a_fixed_error_category(self) -> None:
        reviewed = review_server_log.review_line(b"PRIVATE_SECRET", oversized=True)
        self.assertIn(b'"severity":"error"', reviewed)
        self.assertIn(b'"category":"server_oversized"', reviewed)
        self.assertNotIn(b"PRIVATE_SECRET", reviewed)

    @unittest.skipUnless(os.name == "posix", "POSIX process and mode policy")
    def test_follower_emits_only_reviewed_events(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, destination = root / "raw", root / "reviewed"
            source.write_bytes(b"")
            destination.write_bytes(b"")
            source.chmod(0o600)
            destination.chmod(0o600)
            process = subprocess.Popen(
                [sys.executable, REVIEWER, source, destination],
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
            try:
                with source.open("ab") as stream:
                    stream.write(b'PRIVATE_SECRET {"level":"ERROR"}\n')
                    stream.flush()
                deadline = time.monotonic() + 2
                while destination.stat().st_size == 0 and time.monotonic() < deadline:
                    time.sleep(0.02)
                reviewed = destination.read_bytes()
                self.assertIn(b'"category":"server_error"', reviewed)
                self.assertNotIn(b"PRIVATE_SECRET", reviewed)
                self.assertIsNone(process.poll())
            finally:
                process.terminate()
                process.wait(timeout=2)


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

    def test_oversized_fresh_window_is_never_disclosed(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source, context, artifact = (
                root / "source-oversized",
                root / "context-oversized",
                root / "artifact-oversized",
            )
            source.write_bytes(b"baseline\n")
            source.chmod(0o600)
            with contextlib.redirect_stdout(io.StringIO()):
                reviewed_evidence.start(source, context)
            with source.open("ab") as output:
                output.truncate(
                    source.stat().st_size + reviewed_evidence.FRESH_ARTIFACT_LIMIT + 1
                )
            reviewed_evidence.capture(source, context, artifact)
            self.assertEqual(
                artifact.read_bytes(),
                b"any-mcp reviewed failure evidence\nreviewed_log_invalid\nevent_count=0\n",
            )


if __name__ == "__main__":
    unittest.main()
