# Changelog

All notable changes to Anytype Toolbox Skills are documented in this file.

The format is based on Keep a Changelog, and this package uses Semantic
Versioning independently from the Anytype Toolbox Rust crates.

## [0.1.1]

### Added

- Add `anytype-setup` for `anyr` installation, explicit desktop or headless
  connection selection, authentication, status checks, and bounded recovery.

### Changed

- Keep ordinary any-mcp HTTP workflows independent of gRPC preflight and route
  structured backend setup failures to the connection skill.

## [0.1.0]

### Added

- Package the `anyr` and `any-mcp` Agent Skills in one self-contained Codex and
  Claude Code plugin.
- Declare the external Anytype, CLI, MCP, and optional workflow prerequisites
  without embedding credentials or checkout-specific setup.
- Validate package directories and release ZIPs offline, including host
  manifests, skill metadata and references, version consistency, required
  files, public-path hygiene, credential patterns, and archive safety.
- Release the plugin independently from Rust binaries with reproducible ZIP
  and tar.gz archives, checksums, version-specific notes, and a non-latest
  GitHub Release.
- Add repository catalogs for Codex and Claude Code, with installation,
  exact-release, update, removal, prerequisite, and security-review guidance.
- Prove disposable end-to-end discovery, selected installation, release-archive
  loading, host marketplace upgrades, and clean removal across the skills CLI,
  Codex, and Claude Code.
