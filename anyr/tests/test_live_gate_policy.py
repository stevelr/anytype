import os
import unittest
from unittest import mock

from anyr.tests import cli_commands
from anyr.tests.live_gate_policy import PYTHON_TEST_IDS, python_ok, rust_ok
from anyr.tests.run_required_python_cli import required_suite


RUST = "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 3 filtered out; finished in 0.01s"
PYTHON = "Ran 24 tests in 0.01s\n\nOK\n"


class LiveGatePolicyTests(unittest.TestCase):
    def test_exact_results_reject_skips_zero_and_duplicates(self):
        self.assertTrue(rust_ok(RUST))
        self.assertFalse(rust_ok(RUST + "\n" + RUST))
        self.assertFalse(rust_ok(RUST.replace("1 passed", "0 passed")))
        self.assertFalse(rust_ok(RUST + "\nskipped"))
        self.assertTrue(python_ok(PYTHON))
        self.assertFalse(python_ok("Ran 0 tests in 0.01s\n\nOK\n"))
        self.assertFalse(python_ok("Ran 23 tests in 0.01s\n\nOK\n"))
        self.assertFalse(python_ok(PYTHON + "Ran 24 tests in 0.01s\n\nOK\n"))
        self.assertFalse(python_ok(PYTHON.replace("OK", "OK (skipped=1)")))

    def test_required_mode_rejects_missing_binary_and_prefix(self):
        with (
            mock.patch.dict(os.environ, {"ANYR_PY_REQUIRE_LIVE": "1"}, clear=True),
            mock.patch("anyr.tests.cli_commands.anyr_bin", return_value=None),
            self.assertRaises(AssertionError),
        ):
            cli_commands.TestAnyrCommands.setUpClass()

    def test_python_test_manifest_matches_loader(self):
        classes = [
            cli_commands.TestAnyrCommands,
            cli_commands.TestDisposableSpaceCleanup,
        ]
        actual = sorted(
            f"{test_class.__name__}.{name}"
            for test_class in classes
            for name in unittest.defaultTestLoader.getTestCaseNames(test_class)
        )
        self.assertEqual(actual, list(PYTHON_TEST_IDS))
        self.assertEqual(
            [
                test.id().removeprefix("anyr.tests.cli_commands.")
                for test in required_suite()
            ],
            list(PYTHON_TEST_IDS),
        )
        with (
            mock.patch.dict(os.environ, {"ANYR_PY_REQUIRE_LIVE": "1"}, clear=True),
            mock.patch("anyr.tests.cli_commands.anyr_bin", return_value="/bin/true"),
            self.assertRaises(AssertionError),
        ):
            cli_commands.TestAnyrCommands.setUpClass()
