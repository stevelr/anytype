import os
import stat
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from anyr.tests import cli_commands
from anyr.tests.live_gate_policy import PYTHON_TEST_IDS, python_ok, rust_ok
from anyr.tests.run_required_python_cli import (
    FAILURE_CATEGORIES,
    classify_failure_diagnostics,
    fixed_failure_message,
    required_suite,
    write_failure_category,
)


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

    def test_failure_category_is_fixed_and_never_echoes_private_details(self):
        self.assertEqual(
            classify_failure_diagnostics("space inventory pagination is invalid"),
            "inventory-invalid",
        )
        self.assertEqual(
            classify_failure_diagnostics(
                "disposable space create ownership is ambiguous"
            ),
            "create-ambiguous",
        )
        self.assertEqual(
            classify_failure_diagnostics("token=secret-untrusted-message"),
            "required-test-failed",
        )
        message = fixed_failure_message("token=secret-untrusted-message")
        self.assertEqual(
            message, "required anyr Python gate failed: required-test-failed"
        )
        self.assertNotIn("secret", message)

    def test_category_files_surface_only_allowlisted_values(self):
        with tempfile.TemporaryDirectory() as directory:
            for category in FAILURE_CATEGORIES:
                category_file = Path(directory, f"{category}.category")
                write_failure_category(category_file, category)
                self.assertEqual(
                    category_file.read_text(encoding="ascii"), f"{category}\n"
                )
                self.assertEqual(stat.S_IMODE(category_file.stat().st_mode), 0o600)
        with self.assertRaisesRegex(RuntimeError, "destination"):
            write_failure_category(Path("relative.category"), "token=secret")

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
