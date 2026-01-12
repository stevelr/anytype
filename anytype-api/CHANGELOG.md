# Changelog
All notable changes to this project will be documented in this file.

The format is based on Keep a Changelog, and this project adheres to Semantic Versioning.

## Unreleased
### Changed
- BREAKING: `Property.as_date()` return type was `Option<&str>`, now `Option<DateTime<FixedOffset>>`, to match `Object.get_property_date()`.

## [0.2.5] - 2025-01-10
### Added
- Initial GitHub release.

[Unreleased]: https://github.com/stevelr/anytype/compare/ba28bda...HEAD
[0.2.5]: https://github.com/stevelr/anytype/releases/tag/v0.2.5
