#!/usr/bin/env python3
"""Validate repository marketplace catalogs for Anytype Toolbox Skills."""

from __future__ import annotations

import argparse
import json
import pathlib
import sys
from typing import Any


MARKETPLACE_NAME = "anytype-toolbox"
PLUGIN_NAME = "anytype-toolbox-skills"
PLUGIN_PATH = pathlib.PurePosixPath("skills")
CODEX_MARKETPLACE = pathlib.PurePosixPath(".agents/plugins/marketplace.json")
CLAUDE_MARKETPLACE = pathlib.PurePosixPath(".claude-plugin/marketplace.json")
SHARED_FIELDS = (
    "name",
    "version",
    "description",
    "author",
    "homepage",
    "repository",
    "license",
    "keywords",
)


class MarketplaceValidationError(ValueError):
    """Reports one deterministic marketplace validation failure."""


def require(condition: bool, message: str) -> None:
    """Require a marketplace invariant or report its stable diagnostic."""

    if not condition:
        raise MarketplaceValidationError(message)


def load_object(path: pathlib.Path, context: str) -> dict[str, Any]:
    """Load one UTF-8 JSON object."""

    require(path.is_file() and not path.is_symlink(), f"{context}: file is missing or unsafe")
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except UnicodeDecodeError as error:
        raise MarketplaceValidationError(
            f"{context}: expected UTF-8 text ({error})"
        ) from error
    except json.JSONDecodeError as error:
        raise MarketplaceValidationError(
            f"{context}: malformed JSON at line {error.lineno}, column {error.colno}"
        ) from error
    require(isinstance(value, dict), f"{context}: top-level value must be an object")
    return value


def sole_plugin(catalog: dict[str, Any], context: str) -> dict[str, Any]:
    """Return the catalog's single Anytype Toolbox plugin entry."""

    plugins = catalog.get("plugins")
    require(
        isinstance(plugins, list) and len(plugins) == 1,
        f"{context}: expected exactly one plugin",
    )
    plugin = plugins[0]
    require(isinstance(plugin, dict), f"{context}: plugin entry must be an object")
    require(
        plugin.get("name") == PLUGIN_NAME, f"{context}: plugin name must be {PLUGIN_NAME!r}"
    )
    return plugin


def resolve_source(root: pathlib.Path, source: Any, context: str) -> pathlib.Path:
    """Resolve a catalog source while confining it to the repository."""

    require(
        isinstance(source, str) and source.startswith("./"),
        f"{context}: source must start with ./",
    )
    require(
        "\\" not in source and "\x00" not in source,
        f"{context}: source uses unsafe separators",
    )
    relative = pathlib.PurePosixPath(source.removeprefix("./"))
    require(
        relative.parts and ".." not in relative.parts,
        f"{context}: source escapes the repository",
    )
    resolved = root.joinpath(*relative.parts).resolve()
    try:
        resolved.relative_to(root)
    except ValueError as error:
        raise MarketplaceValidationError(
            f"{context}: source escapes the repository"
        ) from error
    require(resolved == root / PLUGIN_PATH, f"{context}: source must resolve to ./skills")
    require(
        resolved.is_dir() and not resolved.is_symlink(),
        f"{context}: plugin directory is missing or unsafe",
    )
    return resolved


def validate_catalogs(root: pathlib.Path) -> None:
    """Validate both catalogs against the package manifests."""

    root = root.resolve()
    codex = load_object(root / CODEX_MARKETPLACE, CODEX_MARKETPLACE.as_posix())
    claude = load_object(root / CLAUDE_MARKETPLACE, CLAUDE_MARKETPLACE.as_posix())
    codex_manifest = load_object(
        root / PLUGIN_PATH / ".codex-plugin/plugin.json", "Codex plugin manifest"
    )
    claude_manifest = load_object(
        root / PLUGIN_PATH / ".claude-plugin/plugin.json", "Claude plugin manifest"
    )

    require(codex.get("name") == MARKETPLACE_NAME, "Codex marketplace name is inconsistent")
    require(claude.get("name") == MARKETPLACE_NAME, "Claude marketplace name is inconsistent")
    require(
        codex.get("interface") == {"displayName": "Anytype Toolbox"},
        "Codex marketplace display metadata is inconsistent",
    )
    codex_entry = sole_plugin(codex, "Codex marketplace")
    source = codex_entry.get("source")
    require(isinstance(source, dict), "Codex marketplace: source must be an object")
    require(source.get("source") == "local", "Codex marketplace: source kind must be local")
    resolve_source(root, source.get("path"), "Codex marketplace")
    require(
        codex_entry.get("policy")
        == {"installation": "AVAILABLE", "authentication": "ON_INSTALL"},
        "Codex marketplace policy is inconsistent",
    )
    require(
        codex_entry.get("category") == "Productivity",
        "Codex marketplace category is inconsistent",
    )

    claude_entry = sole_plugin(claude, "Claude marketplace")
    resolve_source(root, claude_entry.get("source"), "Claude marketplace")
    require(
        claude_entry.get("strict") is True, "Claude marketplace must use strict plugin loading"
    )
    for field in SHARED_FIELDS:
        require(
            claude_entry.get(field) == claude_manifest.get(field),
            f"Claude marketplace {field} does not match the plugin manifest",
        )
    require(
        codex_manifest.get("name") == PLUGIN_NAME, "Codex plugin manifest name is inconsistent"
    )
    require(
        codex_manifest.get("version") == claude_manifest.get("version"),
        "plugin manifest versions disagree",
    )
    author = claude_manifest.get("author")
    require(isinstance(author, dict), "Claude plugin manifest author must be an object")
    expected_owner = {field: author[field] for field in ("name", "email") if field in author}
    require(
        claude.get("owner") == expected_owner,
        "Claude marketplace owner does not match plugin author",
    )
    metadata = claude.get("metadata")
    require(isinstance(metadata, dict), "Claude marketplace metadata must be an object")
    require(
        metadata.get("version") == claude_manifest.get("version"),
        "Claude marketplace version does not match plugin manifest",
    )


def parse_arguments() -> argparse.Namespace:
    """Parse command-line arguments."""

    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("repository", nargs="?", default=".", type=pathlib.Path)
    return parser.parse_args()


def main() -> int:
    """Validate repository catalogs and return a process status."""

    arguments = parse_arguments()
    try:
        validate_catalogs(arguments.repository)
    except (OSError, MarketplaceValidationError) as error:
        print(f"skills marketplace validation failed: {error}", file=sys.stderr)
        return 1
    print(f"validated {MARKETPLACE_NAME} marketplace catalogs for {PLUGIN_NAME}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
