# Changelog

All notable changes to this project will be documented in this file.

The format is based on Keep a Changelog, and this project adheres to Semantic Versioning.

## [Unreleased]

### Fixed

- examples and integration-test helpers now satisfy workspace rustdoc and lint
  checks without needless borrows
- property updates now reject key-only requests before sending them because the
  REST API requires `name`; type updates now expose full property replacement
  and explicit clearing while preserving omission when properties are unchanged
- view_list_objects() requires setting `.view(view_id)` before invoking `.list()`
  and rejects unsafe selected view path identifiers before building the URL.

- integration-test clients now honor `ANYTYPE_KEYSTORE` instead of always using
  a separate default test database without the configured HTTP credentials
- chat event streams continue polling when callers keep the event stream but
  drop the optional control handle, avoiding reconnect spins and missed events
- integration tests accept the current missing-token authentication error, and
  known-broken boolean/numeric filter cases reference
  [anytype-heart#2879](https://github.com/anyproto/anytype-heart/issues/2879)
- Set view integration cases that require a preconfigured internal `set_of`
  source are ignored in environments where REST cannot create that fixture

### Added

- finite, incrementally enforced HTTP response ceilings for generic JSON,
  document JSON, bounded error bodies, and separately governed raw file
  downloads; object reads support a per-request ceiling within the configured
  document allowance, and oversized responses return the secret-safe
  `ResponseTooLarge` error

- REST file download/delete APIs (`download_bytes`, `delete`) and unified file
  upload selection: simple path/byte uploads use REST, while URL uploads and
  uploads with style, details, or creation context retain the richer gRPC path
- configurable REST file requests with image widths, `HEAD` metadata, byte
  ranges, conditional headers and preserved HTTP control statuses, plus
  permanent deletion through the `skip_bin` option
- space-scoped REST chat APIs for chat listing/creation, plain message listing,
  single-message lookup, message search, deletion, reactions, and read state
- direct REST chat message add/edit builders, dynamic filters for chat listings,
  and typed SSE message streams with configurable initial-message limits and
  heartbeat intervals
- structured gRPC chat message blocks, pin state, unread-reaction state, and
  attachment replacement for rich message publishing and full-fidelity reads
- new `resolve` module: name and id resolution helpers as `AnytypeClient` methods —
  `resolve_space_id`, `resolve_type`, `resolve_type_id`, `resolve_type_ids`,
  `resolve_type_key`, `resolve_view_id`, `resolve_property_id`, `resolve_chat_target`
  (returns the new `ChatTarget` struct), `resolve_chat_ids`, and `resolve_chat_name`.
  Moved from the anyr cli so all clients share the same "name or id" conventions.
  `ChatTarget` and `DEFAULT_CHAT_NAME` are exported in the prelude.
- new error variant `AnytypeError::Ambiguous`, returned by the `resolve_*` helpers
  when a space, type, chat, or view name matches more than one item; ambiguity
  errors now include up to 10 deterministic, deduplicated candidate ids and
  display names through the new `ResolveCandidate` type; resolver scans use a
  hard 1,000-row limit and return `ResolutionLimitExceeded` rather than a
  possibly false unique/not-found result, preserve direct view-id priority,
  and select candidates independently of upstream row order; safe duplicate
  representatives take precedence over malformed alternatives with the same id

### Changed

- Normalized troubleshooting examples and repository configuration formatting.
- `list_chats_in` now uses REST; cross-space chat discovery, structured message
  publishing/full-fidelity reads, and reconnecting multi-chat subscriptions
  continue to use gRPC
- chat discovery, CRUD, REST overlap, and normal streaming tests now exercise
  the configured real server; only disconnect/reconnect fault injection retains
  the mock gRPC server
- successful REST mutation endpoints may return an empty body, which is now
  handled as a unit response
- removed skia as dependency (was used to generate image file for the files example)
- files example requires setting path and file type to path and type of existing
  local files, instead of generating them locally

- bumped dependencies: zbus-secret-service-keyring-store from 0.2.2 to 0.3.0

## [0.3.2]

### Added

- new helper `client::find_grpc(program)` to discover a local Anytype gRPC port by scanning listeners for a process prefix and probing candidate ports.

## [0.3.1] - anytype - 2026-02-16

### Added

- new function `backup_space()` to export any space, format: Markdown, Protobuf, or Json; with/without Files, and other options.
- file upload/preload request options: `created_in_context` and `created_in_context_ref`
- chat message text styles: `toggle_header1`, `toggle_header2`, `toggle_header3`
- new gRPC `process_watcher` module for reusable process lifecycle tracking (subscribe/wait/reconnect/unsubscribe), with cancellation-channel support and configurable timeouts/fallbacks.
- archived object management APIs on `AnytypeClient`:
  - `list_archived(space_id)` builder with `limit`, `offset`, and `types` filters.
  - `count_archived(space_id)` to count archived objects.
  - `delete_archived(space_id, &[String])` to hard-delete archived objects in gRPC batches of 200.
  - `delete_all_archived(space_id)` to delete all archived objects by paging archived IDs and deleting in repeated batches (200 per delete request) with settle delay and progress debug logs.

### Changed

- bumped anytype-rpc to 0.3.0
- removed generate-markdown example

## [0.3.0] - anytype - 2026-01-28

Major update:

- adds gRPC backend for Files and Chats.
- Refactored keystore to use db-keystore (sqlite) for file-based keystore

### Added

- `take_items()` on `PaginatedResult<T>`
- gRPC files module with list/search/get/upload/download/preload support.
- gRPC file list/search filters for name, extension, size, and file type.
- gRPC file downloads now support explicit destination file paths via `to_file()` (and `to_dir()` alias).
- gRPC chat streaming API with subscription control, reconnect, and preview support.
- chat message send with helpers for text marks
- functions to generate web links: `Object::get_link`, `Object::get_link_shared`, and `objects::object_link`, `objects::object_link_shared`
- new example: [agenda](./examples/agenda.rs) - Collect top-10 tasks (sorted by date modified and priority) and recent documents, and send in a chat message.

### Changed

- simplified KeyStore implementation leveraging new keyring_core apis.
  - KeyStoreFile replaced by db-keystore::DbKeyStore. Uses local sqlite file (turso rust-native implementation), with optional encryption. Default key store is still OS keyring.
- gRPC feature is enabled by default; disable with `default-features = false` if you only need REST.
- Apache-2.0 license
- bumped dependencies (markdown2pdf -> 0.2.1)

### BREAKING

- Build changes
  - protoc and libgit2 must be installed for build from source or cargo install
- ClientConfig::base_url changed from String to Option<String>
- Changes to authentication apis
  - is_authenticated() replaced with auth_status().http.is_authenticated() and auth_status().grpc.is_authenticated().
  - keystore is now configured in ClientConfig. set_key_store() and load_key() no longer needed.
  - If using file-based keystore, default path is ~/.local/state/keystore.db
  - removed SecretApiKey

## [0.2.9] - anytype - 2026-01-17

### Added

- Documentation (README.md): listed limitations of the rest api
- Optional feature flags to select os keystore flavor on linux

### Changed

- clippy fixes

## [0.2.8] - anytype - 2026-01-12

### Changed

- Switch reqwest to rustls with native roots to avoid OpenSSL build-time dependencies.

## [0.2.7] - anytype - 2026-01-12

### Changed

- BREAKING: `Property.as_date()` return type was `Option<&str>`, now `Option<DateTime<FixedOffset>>`, to match `Object.get_property_date()`.

## [0.2.5] - anytype 2026-01-10

### Added

- Initial GitHub release.
