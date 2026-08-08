"""Hermetic checks for the disposable live-gate driver."""

import importlib.util
import os
import sys
import tempfile
import unittest
from pathlib import Path


SPEC = importlib.util.spec_from_file_location(
    "run_live_gate", Path(__file__).with_name("run-live-gate.py")
)
assert SPEC is not None and SPEC.loader is not None
GATE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(GATE)
SUMMARY = b"test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.68s"
ENTRY = {
    "target": "test_body",
    "test": "test_case",
    "serial_group": "disposable_anytype_api",
}


class LiveGateTests(unittest.TestCase):
    def test_manifest_and_environment_validation(self) -> None:
        environment = {
            "ANYTYPE_DISPOSABLE_TEST_PROCESS": "1",
            "ANYTYPE_TEST_SPACE_PREFIX": "gate_1",
            "ANYTYPE_KEYSTORE": "env",
            "ANYTYPE_KEYSTORE_SERVICE": "anyr.test-1",
            "ANYTYPE_KEY_HTTP_TOKEN": "x",
            "ANYTYPE_KEY_SESSION_TOKEN": "x",
            "ANYTYPE_URL": "http://127.0.0.1",
            "ANYTYPE_GRPC_ENDPOINT": "https://[::1]",
        }
        self.assertTrue(GATE.environment_is_admitted(environment))
        self.assertFalse(
            GATE.environment_is_admitted({**environment, "ANYTYPE_KEY_BAD": "x"})
        )
        self.assertFalse(
            GATE.environment_is_admitted(
                {**environment, "ANYTYPE_KEY_SESSION_TOKEN": ""}
            )
        )
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "manifest.toml"
            path.write_text(
                "version = 1\n[[required]]\ntarget = '../bad'\ntest = 'x'\nserial_group = 'disposable_anytype_api'\n"
            )
            with self.assertRaises(SystemExit):
                GATE.manifest_entries("required", path)

    def test_exact_command_and_summary_rejection(self) -> None:
        commands = []
        original = GATE.run_bounded
        try:
            GATE.run_bounded = lambda command, timeout=GATE.PROCESS_TIMEOUT: (
                commands.append(command) or (0, SUMMARY)
            )
            GATE.run_entry(1, ENTRY)
            self.assertEqual(
                commands[0][0:5],
                [os.environ.get("CARGO", "cargo"), "test", "--locked", "-p", "anytype"],
            )
            self.assertIn("--exact", commands[0])
            for output in [
                b"",
                b"skipped\n" + SUMMARY,
                SUMMARY + b"\n" + SUMMARY,
                b"test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 0.68s",
                b"test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in someday",
            ]:
                GATE.run_bounded = (
                    lambda command, timeout=GATE.PROCESS_TIMEOUT, output=output: (
                        0,
                        output,
                    )
                )
                with self.assertRaises(SystemExit):
                    GATE.run_entry(1, ENTRY)
            GATE.run_bounded = lambda command, timeout=GATE.PROCESS_TIMEOUT: (
                1,
                SUMMARY,
            )
            with self.assertRaises(SystemExit):
                GATE.run_entry(1, ENTRY)
        finally:
            GATE.run_bounded = original

    def test_bounded_runner_overflow_timeout_and_cleanup(self) -> None:
        original_limit = GATE.OUTPUT_LIMIT
        try:
            GATE.OUTPUT_LIMIT = 16
            with self.assertRaises(GATE.RunnerError):
                GATE.run_bounded([sys.executable, "-c", "print('x' * 100)"], timeout=2)
            with self.assertRaises(GATE.RunnerError):
                GATE.run_bounded(
                    [sys.executable, "-c", "import time; time.sleep(5)"], timeout=0.05
                )
            with self.assertRaises(GATE.RunnerError):
                GATE.run_bounded(
                    [
                        sys.executable,
                        "-c",
                        "import os, time; os.close(1); os.close(2); time.sleep(5)",
                    ],
                    timeout=0.05,
                )
        finally:
            GATE.OUTPUT_LIMIT = original_limit


if __name__ == "__main__":
    unittest.main()
