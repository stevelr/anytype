# Changelog

All notable changes to this project will be documented in this file.

The format is based on Keep a Changelog, and this project adheres to Semantic Versioning.

## [0.3.0] - anytype-rpc - 2026-02-16

### Changes

- protobuf definitions no longer included in this repo. The build is now faster and doesn't require `protoc`.
- Generated source is in `src/gen`. Instructions for regenerating `src/gen/*.rs` from protobuf definitions in github:anytype-heart are in the README.

- **Breaking**
  - Updated proto files to 0.48.0

### Added

- new api to backup space `AnytypeGrpcClient::backup_space`
- cli additions
  - space create, delete, invite (create/show/revoke), enable-sharing, disable-sharing
- gRPC client channel now sets explicit transport defaults for long-running operations:
  - connect timeout: 30s
  - TCP keepalive: 60s
  - HTTP/2 keepalive interval/timeout: 30s/10s
  - keepalive while idle enabled

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
