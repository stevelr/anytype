"""Run exactly the manifest-pinned Python CLI cases for the protected gate."""

import sys
import unittest

from anyr.tests.live_gate_policy import PYTHON_TEST_IDS


def _flatten(suite: unittest.TestSuite):
    for item in suite:
        if isinstance(item, unittest.TestSuite):
            yield from _flatten(item)
        else:
            yield item


def required_suite() -> unittest.TestSuite:
    """Construct the exact manifest collection, rejecting stale identifiers."""
    loader = unittest.defaultTestLoader
    suite = unittest.TestSuite()
    for test_id in PYTHON_TEST_IDS:
        expected = f"anyr.tests.cli_commands.{test_id}"
        loaded = loader.loadTestsFromName(expected)
        tests = list(_flatten(loaded))
        if len(tests) != 1 or tests[0].id() != expected:
            raise RuntimeError("required anyr Python test manifest is invalid")
        suite.addTest(tests[0])
    return suite


def main() -> None:
    try:
        suite = required_suite()
    except RuntimeError:
        print("required anyr Python test manifest is invalid", file=sys.stderr)
        raise SystemExit(1) from None
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    if result.testsRun != len(PYTHON_TEST_IDS) or result.skipped:
        print("required anyr Python test result is invalid", file=sys.stderr)
        raise SystemExit(1)
    raise SystemExit(0 if result.wasSuccessful() else 1)


if __name__ == "__main__":
    main()
