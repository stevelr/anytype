# Anytype Toolbox Skills

Anytype Toolbox Skills packages two Agent Skills for Codex and Claude Code:

- `anyr` guides command-line work with Anytype objects, spaces, schema, files,
  chats, Markdown, and backups.
- `any-mcp` guides bounded workflows through an MCP connection backed by
  `anyr mcp`.

This is an independent community project and is not affiliated with or
endorsed by Anytype.

## Prerequisites

Install `anyr`, connect it to a running Anytype desktop or headless service,
and configure endpoint-specific credentials. The `any-mcp` skill also requires
an MCP host with a configured `anyr mcp` connection. Some fallback recipes use
the `anyr` executable directly; the `save-links` recipe additionally uses
Trafilatura for web-page extraction.

The package contains instructions and metadata. It does not contain Anytype
credentials or start an MCP server when installed.

## Package ownership

This directory is the installable plugin root. The Codex and Claude manifests
share one package version, and [CHANGELOG.md](CHANGELOG.md) records that version
history. Each directory below `skills/` owns the operating guidance and
supporting references for one Agent Skill. The repository's crate READMEs own
the `anyr` and `any-mcp` runtime documentation.

Release packages are checked offline for matching manifest and changelog
versions, valid Agent Skills metadata and references, required files, safe
archive paths, and accidental private paths or credential material.

Anytype Toolbox is licensed under the Apache License 2.0. See
[LICENSE](LICENSE).
