# anyback

[![release](https://img.shields.io/github/v/tag/stevelr/anytype?sort=semver&include_prereleases&filter=anyback-v*&label=release)](https://github.com/stevelr/anytype/releases?q=anyback-v&expanded=true)
[![docs.rs](https://img.shields.io/docsrs/anyback?label=docs.rs)](https://docs.rs/anyback)
[![crates.io](https://img.shields.io/crates/v/anyback.svg)](https://crates.io/crates/anyback)

**[Anytype Toolbox documentation](https://docs.anytype-toolbox.org/) ·
[Backup and restore guide](https://docs.anytype-toolbox.org/guides/backup-restore/) ·
[Rust API](https://docs.rs/anyback)**

This package provides backup capabilities for Anytype: Backup an entire space, Backup a partial space using selection filters, and Visually explore a backup with a TUI inspector.

Backup cli documentation is at [Anytype Toolbox](https://docs.anytype-toolbox.org/guides/backup-restore/).

To use the library from a Rust application, see [anyback - Rust docs](https://docs.rs/anyback).

Please report any problems found.

API Stability: The library is functionally complete, and passes an extensive test suite. The `archive`, `metadata`, and `markdown` modules are intended to remain stable. The cli module, including its clap types and command entrypoints, is subject to change and will be moved to the anyr module. Consequently, the cli module is not included in the published rustdoc documentation.

## Library surface

`anyback` provides three reusable API modules:

- `archive`: ZIP and directory traversal and file access.
- `metadata`: backup manifests, snapshot metadata decoding, and restore reports.
- `markdown`: snapshot rendering and object extraction.

The default `cli` feature enables all three modules and the commands embedded in
`anyr`. With `default-features = false`, `archive` remains available; enable
`metadata` to add metadata APIs without enabling the embedded CLI or inspector.
The `markdown` module currently requires `cli`. The `tui` feature adds the
interactive inspector used by `anyr backup inspect`.

## Archive and restore design

Backups use Anytype's protobuf exporter by default and publish a ZIP archive
plus a sibling manifest. The manifest binds the staged archive's byte length
and SHA-256 digest before the archive becomes visible at its final path.
Readers also accept archives without a manifest, including archives created by
Anytype desktop and direct pre-delete backups.

Full restores pass the archive path to Heart's import operation. Selective
restore uses snapshot import behind the `snapshot-import` feature. Snapshot
import accepts protobuf snapshots; JSON-encoded protobuf snapshots are not
supported.

Create and restore operations share an absolute workflow deadline. Local
archive, manifest, report, and result publication use staged writes under that
deadline. Restore completion is correlated with the import process when Heart
returns a collection identifier; ordinary object imports without one are
bound to the dispatch generation.

`anyr backup create|export` and non-dry-run `restore|import` require a running
Anytype CLI server and gRPC credentials. Restore and import with `--dry-run`
validate the local archive and resolve the destination space over HTTP without
dispatching an import.

## Development

Run the offline crate checks:

```sh
cargo test -p anyback
cargo clippy -p anyback --all-targets
cargo fmt --all -- --check
```

The ignored live and integrity suites require a disposable, authenticated
Anytype server. Their admission variables and exact test commands are defined
next to the test targets and in the workspace CI configuration.
