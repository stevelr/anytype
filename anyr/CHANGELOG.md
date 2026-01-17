# Changelog

All notable changes to this project will be documented in this file.

The format is based on Keep a Changelog, and this project adheres to Semantic Versioning.

## [0.2.4] - anyr - 2026-01-17

### Added

- Documentation (README.md): example command for listing items in query or collection

### Changed

- Fix: 'view objects' with query views found results in table output format only. Now gives result in table or json format. Property metadata resolved before output formatting, and removed get_type call from json output path.
- removed undocumented --keyring-service arg

## [0.2.3] - anyr - 2026-01-12

### Changed

- Use rustls (native roots) for HTTP TLS to avoid OpenSSL install errors.
- Uses anytype-v0.2.8.

## [0.2.2] - anyr - 2025-01-12

### Added

- New command `anyr view objects` to list view items for grid and list views.
  - Json output includes all properties/view columns.
  - Table output defaults to name column only, and supports `--columns`/`--cols` for specific property keys

- Table display formatting improvements:
  - Column names from property names
  - Format dates with strftime format: `--date-format` or `ANYTYPE_DATE_FORMAT`, defaults to `%Y-%m-%d %H:%M:%S`.
  - For members, replace member Id with display name.

- Resolvers allow names or keys in place of ids for many cli args:
  - Resolve space id from name for any command that requires space_id. Changed arg name from `space_id` to `space`.
  - Resolve type id from type_name or type_id. Disambiguation rules:
    - if arg has '@' prefix, match type_key only. If arg begins with upper case letter, match name only.
    - type_id always works and is unambiguous.
  - Resolve view id from name. (applies to `view objects subcommand`)
  - Resolve property id from property key

- Improved README documentation on key storage, configuration, authentication, and more examples

- Improved cli help docs

### Changed

- Removed cli config file to simplify. Options can be configured by cli args or environment variables.
- Uses anytype-v0.2.7
