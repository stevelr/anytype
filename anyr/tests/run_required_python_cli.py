"""Run exactly the manifest-pinned Python CLI cases for the protected gate."""

import argparse
import os
import sys
import unittest
from pathlib import Path

from anyr.tests.live_gate_policy import PYTHON_TEST_IDS


FAILURE_CATEGORIES = frozenset(
    {
        "inventory-invalid",
        "create-ambiguous",
        "identity-mismatch",
        "cleanup-failed",
        "required-test-failed",
    }
)


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


def write_failure_category(category_file: Path, category: str) -> None:
    """Atomically write one allowlisted failure category with private permissions."""
    if category not in FAILURE_CATEGORIES or not category_file.is_absolute():
        raise RuntimeError("required anyr Python category destination is invalid")
    temporary = category_file.with_name(f".{category_file.name}.{os.getpid()}.tmp")
    descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with os.fdopen(descriptor, "w", encoding="ascii") as handle:
            handle.write(f"{category}\n")
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, category_file)
    except BaseException:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass
        raise


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


def parse_arguments() -> argparse.Namespace:
    """Parse the protected workflow's private category destination."""
    parser = argparse.ArgumentParser(add_help=False)
    parser.add_argument("--category-file", required=True, type=Path)
    return parser.parse_args()


def fail(category_file: Path, diagnostics: str) -> None:
    """Persist a safe category while leaving all unittest details private."""
    category = classify_failure_diagnostics(diagnostics)
    write_failure_category(category_file, category)
    print(fixed_failure_message(diagnostics), file=sys.stderr)
    raise SystemExit(1)


def main() -> None:
    arguments = parse_arguments()
    try:
        suite = required_suite()
    except RuntimeError:
        fail(arguments.category_file, "")
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    if result.testsRun != len(PYTHON_TEST_IDS) or result.skipped:
        fail(arguments.category_file, "")
    if not result.wasSuccessful():
        fail(arguments.category_file, failure_diagnostics(result))


if __name__ == "__main__":
    main()
