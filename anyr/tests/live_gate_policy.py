"""Fixed-output validators for protected anyr live-gate command output."""

import re
import sys
from pathlib import Path


RUST = re.compile(
    r"^test result: ok\. 1 passed; 0 failed; 0 ignored; 0 measured; [0-9]+ filtered out; finished in [0-9]+(?:\.[0-9]+)?s$",
    re.MULTILINE,
)
RUST_LINE = re.compile(r"^test result:.*$", re.MULTILINE)
PYTHON_RAN = re.compile(
    r"^Ran ([1-9][0-9]*) tests? in [0-9]+(?:\.[0-9]+)?s$", re.MULTILINE
)
PYTHON_TEST_IDS = (
    "TestAnyrCommands.test_auth",
    "TestAnyrCommands.test_consolidated_cli_surfaces",
    "TestAnyrCommands.test_file",
    "TestAnyrCommands.test_file_operations",
    "TestAnyrCommands.test_list",
    "TestAnyrCommands.test_member",
    "TestAnyrCommands.test_object",
    "TestAnyrCommands.test_property",
    "TestAnyrCommands.test_real_operations",
    "TestAnyrCommands.test_search",
    "TestAnyrCommands.test_space",
    "TestAnyrCommands.test_space_delete_backup_failure_preserves_source",
    "TestAnyrCommands.test_space_delete_non_interactive_archive_is_exact_and_valid",
    "TestAnyrCommands.test_space_delete_prompted_cancellation_and_confirmation",
    "TestAnyrCommands.test_tag",
    "TestAnyrCommands.test_template",
    "TestAnyrCommands.test_top_level",
    "TestAnyrCommands.test_type",
    "TestDisposableSpaceCleanup.test_create_owned_space_reconciliation_refuses_ambiguity",
    "TestDisposableSpaceCleanup.test_create_owned_space_rejects_ambient_id_and_accepts_exact_identity",
    "TestDisposableSpaceCleanup.test_diagnostics_before_not_found_still_prove_absence",
    "TestDisposableSpaceCleanup.test_explicit_not_found_proves_absence",
    "TestDisposableSpaceCleanup.test_server_failure_does_not_prove_absence",
    "TestDisposableSpaceCleanup.test_transport_failure_does_not_prove_absence",
)


def rust_ok(output: str) -> bool:
    summaries = RUST_LINE.findall(output)
    return (
        len(summaries) == 1
        and RUST.fullmatch(summaries[0]) is not None
        and "skipped" not in output.casefold()
    )


def python_ok(output: str) -> bool:
    return (
        "skipped" not in output.casefold()
        and PYTHON_RAN.findall(output) == [str(len(PYTHON_TEST_IDS))]
        and len(re.findall(r"^OK$", output, re.MULTILINE)) == 1
    )


def main() -> None:
    if len(sys.argv) != 3 or sys.argv[1] not in {"rust", "python"}:
        raise SystemExit(2)
    output = Path(sys.argv[2]).read_text(encoding="utf-8", errors="replace")
    valid = rust_ok(output) if sys.argv[1] == "rust" else python_ok(output)
    raise SystemExit(0 if valid else 1)


if __name__ == "__main__":
    main()
