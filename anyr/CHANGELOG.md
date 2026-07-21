# Changelog

All notable changes to this project will be documented in this file.

The format is based on Keep a Changelog, and this project adheres to Semantic Versioning.

## [Unreleased]

### Added

- `anyr chat --transport auto|rest|grpc` selects the transport policy for chat
  operations (default `auto`). `rest` rejects operations that only gRPC can
  serve (for example cross-space list, chat text search, rich `get`, `unread`,
  structured `--blocks-json` send/edit, and multi-chat `listen`) with an
  actionable error, and `grpc` rejects the REST-only `messages search`; the
  resolved policy backend is reported in verbose diagnostics (`-v`). Under
  `auto`, REST-capable operations (single-space `list`, `create`, `messages
  list|get|send|edit|delete|search|react`, `read`/`read-reactions`/`read-all`,
  and single-`--chat` `listen` with `--space`) route through the REST
  `SpaceChatsClient`; everything else falls back to gRPC.
- `anyr chat list --filter KEY=VALUE` applies property filters to a space-scoped
  REST listing (no `--text`).
- `anyr chat create` gained `--icon-emoji` / `--icon-file` (mutually exclusive)
  to set the new chat object's icon.
- `anyr chat messages send` gained `--reply-to MESSAGE` (reply to an existing
  message by id or order id) and `--blocks-json JSON` (structured message blocks
  as a JSON array via `@file`, `@-`, or `-`; routes through gRPC). `messages
  edit` likewise gained `--blocks-json`.
- `anyr chat messages search SPACE CHAT QUERY` runs a REST-only full-text search
  over a chat's messages; `anyr chat messages react SPACE CHAT MESSAGE EMOJI`
  toggles a reaction on a message.
- `anyr chat read-reactions SPACE CHAT` marks reactions read (optionally through
  an order id), and `anyr chat read-all SPACE CHAT` marks every message in a
  chat as read (both REST).
- `anyr chat listen` gained a REST SSE listener for a single `--chat` with
  `--space`: `--initial-limit N` replays the last N messages when the stream
  opens and `--heartbeat SECONDS` (1-60) sets the keep-alive interval. The
  gRPC-only options `--include-history`, `--after`, `--previews`, and `--buffer`
  (and a `--chat`-only listen without `--space`) route through the reconnecting
  gRPC listener.
- program used to generate test vectors for account key generation
- `anyr type update` property-list controls (mutually exclusive with
  `--add-property`):
  - `--set-property KEY:FORMAT:NAME` replaces the complete non-featured
    property list.
  - `--clear-properties` removes all non-featured recommended properties.
- `anyr file delete --permanent` deletes a file object permanently (skips the
  bin) instead of moving it to the bin.
- `anyr file search` sorting: `--sort PROPERTY` orders results by a property key
  (for example `name` or `last_modified_date`), and `--desc` selects descending
  order (requires `--sort`).
- `anyr file upload` gained richer sources and gRPC-only options:
  - `--url URL` uploads a remote file, `--stdin` uploads bytes read from stdin
    (requires `--name`), and `--mime` sets the MIME type for a REST upload.
  - `--file-type`, `--style`, `--details JSON_OR_@FILE`, `--created-in-context`,
    and `--created-in-context-ref` route the upload through the gRPC backend.
- `anyr file preload SPACE (--file FILE | --url URL)` preloads a file (gRPC) from
  a local path or a remote URL and returns a preload file id; `anyr file
  discard-preload SPACE FILE_ID` discards one.
- `anyr file metadata SPACE FILE` issues a REST `HEAD` request and reports the
  HTTP status plus the header metadata (etag, content-type, content-length,
  last-modified, ...) in both JSON and table output; supports `--width` and the
  conditional headers (`--if-match`, `--if-none-match`, `--if-modified-since`,
  `--if-unmodified-since`).
- `anyr file download SPACE FILE` gained REST options: `--width`, `--range`, and
  the conditional headers `--if-match`, `--if-none-match`, `--if-modified-since`,
  `--if-unmodified-since`, and `--if-range`.
- `anyr file download-via-heart FILE_ID` performs a legacy gRPC (anytype-heart)
  download, writing bytes with `--dir`/`--file` destinations.

### Changed

- `anyr auth status` now reports HTTP and gRPC credentials separately with an
  explicit present/missing indicator per set, so it is clear which credential
  set a REST versus gRPC command needs.
- **Breaking**: `anyr list objects` now requires `--view` (view name or id); it
  is no longer optional.
- **Breaking**: `anyr file delete` no longer accepts `--http`; the flag has been
  removed and deletion now uses the REST files client (add `--permanent` to skip
  the bin).
- **Breaking**: `anyr file download` now uses REST unconditionally and takes
  `SPACE` as a required leading positional (`anyr file download SPACE FILE`); the
  `--http` and `--space` flags have been removed, and the REST options
  (`--width`, `--range`, `--if-*`) are no longer gated behind `--http`. JSON now
  reports `{status, written, path, bytes, metadata}` (previously `{path}`), and
  table output is now `status N PATH` (previously the bare path); a
  `304`/`412`/`416` response leaves the destination file untouched and reports
  `written: false`. The legacy gRPC (anytype-heart) download moved to the
  separate `anyr file download-via-heart FILE_ID` subcommand.
- `anyr file upload --http` is now a deprecated no-op (a plain upload already
  uses REST); it prints a deprecation warning and is rejected when combined with
  any gRPC-only option (`--url`, `--file-type`, `--style`, `--details`, or a
  `--created-in-context*` option), since those select the gRPC transport. The
  REST-only options `--mime` and `--stdin` are likewise rejected up front when
  combined with a gRPC-only option instead of being silently dropped.
- `anyr property update` now requires at least one of `--name` or `--key` and
  rejects a no-flag invocation before any network I/O. A key-only update reuses
  the property's current name so it still satisfies the REST contract.
- Normalized spellchecker configuration formatting.
- name and id resolution (space, type, chat, view, property) moved into the
  anytype crate (`anytype::resolve`); anyr now calls the shared
  `AnytypeClient::resolve_*` methods. Behavior is unchanged, except: a type
  lookup that matches nothing now reports "not found" instead of "ambiguous",
  and not-found/ambiguous messages use the shared `AnytypeError` formats.
- chat order-id-to-message-id resolution now delegates to the shared
  `AnytypeClient::resolve_message_id` / `resolve_message_ids` resolver; the CLI
  retains only its hex order-id encode/decode helpers (no user-facing change).

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
