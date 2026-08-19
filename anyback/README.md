# anyback

[![release](https://img.shields.io/github/v/tag/stevelr/anytype?sort=semver&include_prereleases&filter=anyback-v*&label=release)](https://github.com/stevelr/anytype/releases?q=anyback-v&expanded=true)
[![docs.rs](https://img.shields.io/docsrs/anyback?label=docs.rs)](https://docs.rs/anyback)
[![crates.io](https://img.shields.io/crates/v/anyback.svg)](https://crates.io/crates/anyback)

**[Anytype Toolbox documentation](https://docs.anytype-toolbox.org/) ·
[Backup and restore guide](https://docs.anytype-toolbox.org/guides/backup-restore/) ·
[Rust API](https://docs.rs/anyback)**

The `anyback` package supplies the `anyback_reader` Rust library and the
archive commands embedded in `anyr backup`. It does not install an `anyback`
executable. See the backup and restore guide for command usage.

Status: alpha. Test restores before relying on an archive as your only copy.

## Library surface

`anyback_reader` reads ZIP archives and unpacked archive directories. Its
public modules provide archive traversal, protobuf snapshot inspection, and
Markdown rendering. The default `cli` feature also builds the command types
and `run_command` entry point used by `anyr`; `default-features = false`
excludes those dependencies.

The `tui` feature adds the interactive inspector used by
`anyr backup inspect`.

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
