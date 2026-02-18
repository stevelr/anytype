# Changelog

All notable changes to this project will be documented in this file.

The format is based on Keep a Changelog, and this project adheres to Semantic Versioning.

## [Unreleased]

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
