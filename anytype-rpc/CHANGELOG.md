# Changelog

All notable changes to this project will be documented in this file.

The format is based on Keep a Changelog, and this project adheres to Semantic Versioning.

## [Unreleased] - anytype-rpc

### Changes

- protobuf definitions no longer included in this repo. The build is now faster and doesn't require `protoc`.
- Generated source is in `src/gen`. Instructions for regenerating `src/gen/*.rs` from protobuf definitions in github:anytype-heart are in the README.

- **Breaking**
  - Updated proto files to 0.48-rc.5 (anytype-heart 362cd4edda47e656fe199fb9b705a54b47792c9d)

### Added

- new storage package anytype.storage
  - `anytype_rpc::storage::FileInfo`
  - `anytype_rpc::storage::FileKeys`
  - `anytype_rpc::storage::ImageResizeSchema`
  - `anytype_rpc::storage::Link`
  - `anytype_rpc::storage::Step`

- updated protobuf definitions with file upload context fields, toggle header text styles, `TemplateNamePrefillType`, and `invalidateObjectsIndex`

- added compatibility table to README

## [0.2.1] - anytype-rpc - 2026-01-28

### Added

- get_endpoint() to AnytypeGrpcClient
- optional account_id field to AnytypeHeadlessConfig

### Changed

- Apache-2.0 license

## [0.2.0] - anytype-rpc - 2026-01-17

### Added

- View metadata helpers to fetch table/list view columns and relation names via gRPC.
- AnytypeGrpcConfig, AnytypeGrpcClient
- Helper functions for auth token generation
- New `error` module with unified error types using snafu

### Changed

- **Breaking:** Consolidated error types into `AnytypeGrpcError` with nested sub-errors:
  - `AuthError` for authentication errors (formerly in `auth` module)
  - `ConfigError` for config errors (formerly `AnytypeConfigError` in `config` module)
  - `ViewError` for view errors (formerly in `views` module)
  - `Transport` variant for connection errors (formerly `AnytypeGrpcClientError::Transport`)
- Error types are re-exported from their original modules for convenience
