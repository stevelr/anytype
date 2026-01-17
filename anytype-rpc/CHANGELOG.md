# Changelog

All notable changes to this project will be documented in this file.

The format is based on Keep a Changelog, and this project adheres to Semantic Versioning.

## [0.2.0] - anytype-rpc - 2025-01-17

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
