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
PYTHON_EXPECTED_TESTS = 22


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
        and PYTHON_RAN.findall(output) == [str(PYTHON_EXPECTED_TESTS)]
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
