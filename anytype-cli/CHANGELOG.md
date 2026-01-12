# Changelog
All notable changes to this project will be documented in this file.

The format is based on Keep a Changelog, and this project adheres to Semantic Versioning.

## Unreleased
### Added
- `anyr view objects` command to list view items with view-defined columns, plus `--cols/--columns` for table output.
- Table date formatting with `--date-format` or `ANYTYPE_DATE_FORMAT`, defaulting to `%Y-%m-%d %H:%M:%S`.
- gRPC client support to fetch view definitions and allow gRPC auth via stored API key.
- Space name resolution for commands that accept `space_id`.
- Type name resolution with `@key` support for commands that accept types.
- View name resolution for `anyr view objects --view` arguments.

### Changed
- Table output resolves view column names using type property metadata and maps member IDs to display names.

[Unreleased]: https://github.com/stevelr/anytype/compare/ba28bda...HEAD
