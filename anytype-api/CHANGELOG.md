# Changelog

All notable changes to this project will be documented in this file.

The format is based on Keep a Changelog, and this project adheres to Semantic Versioning.

## [0.2.9] - anytype - 2025-01-17

### Added

- Documentation (README.md): listed limitations of the rest api
- Optional feature flags to select os keystore flavor on linux

### Changed

- clippy fixes

## [0.2.8] - anytype - 2025-01-12

### Changed

- Switch reqwest to rustls with native roots to avoid OpenSSL build-time dependencies.

## [0.2.7] - anytype - 2025-01-12

### Changed

- BREAKING: `Property.as_date()` return type was `Option<&str>`, now `Option<DateTime<FixedOffset>>`, to match `Object.get_property_date()`.

## [0.2.5] - anytype 2025-01-10

### Added

- Initial GitHub release.
