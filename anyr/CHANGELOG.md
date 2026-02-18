# Changelog

All notable changes to this project will be documented in this file.

The format is based on Keep a Changelog, and this project adheres to Semantic Versioning.

## [0.4.1]

### Added

- auth updates:
  - `anyr auth set-grpc --bip39` to derive and save gRPC account credentials from a BIP39 mnemonic.
  - `anyr auth find-grpc [--program PREFIX]` to discover a local Anytype gRPC listener port.

## [0.4.0] - anyr - 2026-02-16

### Added

- new space archive commands:
  - `anyr space count-archived SPACE_NAME_OR_ID`
  - `anyr space delete-archived SPACE_NAME_OR_ID [--confirm]`
- `anyr chat create SPACE_NAME_OR_ID CHAT_NAME` to create chat objects in a space

### Changed

- **Breaking**: chat command argument order is now consistent for space-scoped chat operations:
  - `anyr chat get SPACE CHAT`
  - `anyr chat read SPACE CHAT`
  - `anyr chat unread SPACE CHAT`
  - `anyr chat messages list/get/send/edit/delete SPACE CHAT ...`

## [0.3.0] - anyr - 2026-01-28

### Added

- File commands: list/search/get/update/delete, plus `file download` and `file upload` for raw bytes.
- File list/search filters for name, type, extension, and size.
- Auth commands now support `set-http` and `set-grpc` to update credentials in the keystore.
  - Example: `anyr auth set-grpc [ --account-key | --session-token ]` to store a gRPC account key or session token.
- `--grpc` flag to override the gRPC endpoint url.
- Chat commands (gRPC): `anyr chat list/get/messages list/get/send/edit/delete/read/unread/listen`
- `anyr object link` generates web link for an object

### Changed

- protoc and libgit2 must be installed for build from source or cargo install
- Auth status now reports HTTP vs gRPC credential status with ping checks.
- file-based keystore uses sqlite (turso native rust implementation)
- Apache-2.0 license

### BREAKING

- authentication-related environment variables and flags have changed
  - `--keyfile`, `--keyfile-path`, and `--keyring` now replaced by `--keystore`.
  - omit to use platform default keystore
  - `--keystore file` to use file-based keystore in default path (~/.local/state/keystore.db)
  - `--keystore file:path=/path/to/keystore.db` to use file keystore in custom path
  - `--keystore secret-store` to use dbus secret store on linux (default kernel 'keyutils')

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

## [0.2.2] - anyr - 2026-01-12

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
