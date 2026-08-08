"""Run exactly the manifest-pinned Python CLI cases for the protected gate."""

import sys
import unittest

from anyr.tests.live_gate_policy import PYTHON_TEST_IDS


def classify_failure_diagnostics(diagnostics: str) -> str:
    """Map private unittest details to a fixed public failure category."""
    lowered = diagnostics.casefold()
    if "space inventory" in lowered:
        return "inventory-invalid"
    if "ownership is ambiguous" in lowered:
        return "create-ambiguous"
    if "identity mismatch" in lowered or "invalid space identity" in lowered:
        return "identity-mismatch"
    if "clean up disposable space" in lowered or "cleanup" in lowered:
        return "cleanup-failed"
    return "required-test-failed"


def failure_diagnostics(result: unittest.TestResult) -> str:
    """Collect private test diagnostics for classification without emitting them."""
    return "\n".join(detail for _, detail in [*result.failures, *result.errors])


def fixed_failure_message(diagnostics: str) -> str:
    """Return the fixed category only, leaving captured details private."""
    return (
        f"required anyr Python gate failed: {classify_failure_diagnostics(diagnostics)}"
    )


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
        print(fixed_failure_message(""), file=sys.stderr)
        raise SystemExit(1)
    if not result.wasSuccessful():
        print(fixed_failure_message(failure_diagnostics(result)), file=sys.stderr)
        raise SystemExit(1)


if __name__ == "__main__":
    main()
