#!/usr/bin/env python3
"""Offline tests for the repository skills marketplace catalogs."""

from __future__ import annotations

import json
import pathlib
import shutil
import sys
import tempfile
import unittest


SCRIPT_ROOT = pathlib.Path(__file__).resolve().parent
REPOSITORY_ROOT = SCRIPT_ROOT.parents[1]
sys.path.insert(0, str(SCRIPT_ROOT))

import validate_skills_marketplaces as marketplaces  # noqa: E402


class MarketplaceTests(unittest.TestCase):
    """Exercise catalog identity, source confinement, and version drift."""

    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="skills-marketplace-test-")
        self.root = pathlib.Path(self.temporary.name)
        for relative in (".agents", ".claude-plugin", "skills"):
            shutil.copytree(REPOSITORY_ROOT / relative, self.root / relative)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def mutate(self, relative: str, operation: object) -> None:
        """Apply a callable mutation to one fixture JSON object."""

        path = self.root / relative
        value = json.loads(path.read_text(encoding="utf-8"))
        assert callable(operation)
        operation(value)
        path.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")

    def test_repository_catalogs_validate(self) -> None:
        marketplaces.validate_catalogs(self.root)

    def test_codex_source_cannot_escape_repository(self) -> None:
        self.mutate(
            ".agents/plugins/marketplace.json",
            lambda value: value["plugins"][0]["source"].update(path="./../skills"),
        )
        with self.assertRaisesRegex(marketplaces.MarketplaceValidationError, "escapes"):
            marketplaces.validate_catalogs(self.root)

    def test_claude_source_must_resolve_to_package(self) -> None:
        (self.root / "other").mkdir()
        self.mutate(
            ".claude-plugin/marketplace.json",
            lambda value: value["plugins"][0].update(source="./other"),
        )
        with self.assertRaisesRegex(
            marketplaces.MarketplaceValidationError, "resolve to ./skills"
        ):
            marketplaces.validate_catalogs(self.root)

    def test_claude_entry_version_must_match_manifest(self) -> None:
        self.mutate(
            ".claude-plugin/marketplace.json",
            lambda value: value["plugins"][0].update(version="9.9.9"),
        )
        with self.assertRaisesRegex(
            marketplaces.MarketplaceValidationError, "version does not match"
        ):
            marketplaces.validate_catalogs(self.root)

    def test_catalog_owner_must_match_plugin_author(self) -> None:
        self.mutate(
            ".claude-plugin/marketplace.json",
            lambda value: value.update(owner={"name": "Anytype"}),
        )
        with self.assertRaisesRegex(
            marketplaces.MarketplaceValidationError, "owner does not match"
        ):
            marketplaces.validate_catalogs(self.root)


if __name__ == "__main__":
    unittest.main()
