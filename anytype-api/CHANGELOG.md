# Changelog

All notable changes to this project will be documented in this file.

The format is based on Keep a Changelog, and this project adheres to Semantic Versioning.

## [0.3.0] - anytype

### Added

- gRPC files module with list/search/get/upload/download/preload support.
- gRPC file list/search filters for name, extension, size, and file type.
- gRPC file downloads now support explicit destination file paths via `to_file()` (and `to_dir()` alias).

### Changed

- simplified KeyStore implementation leveraging new keyring_core apis.
  - KeyStoreFile replaced by db-keystore::DbKeyStore. Uses local sqlite file (turso rust-native implementation), with optional encryption. Default key store is still OS keyring.
- gRPC feature is enabled by default; disable with `default-features = false` if you only need REST.
- Apache-2.0 license

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
