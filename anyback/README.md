# anyback

[![release](https://img.shields.io/github/v/tag/stevelr/anytype?sort=semver&include_prereleases&filter=anyback-v*&label=release)](https://github.com/stevelr/anytype/releases?q=anyback-v&expanded=true)
[![docs.rs](https://img.shields.io/docsrs/anyback?label=docs.rs)](https://docs.rs/anyback)
[![crates.io](https://img.shields.io/crates/v/anyback.svg)](https://crates.io/crates/anyback)

`anyback` provides the backup and restore library used by the consolidated
`anyr backup` command.
Run `anyr backup --help` to view its alphabetically sorted archive operations.

See `anyback.1.md` for detailed CLI documentation.

**Alpha Release**

- This is an alpha version - Testing is still in progress. If you're adventurous, please give it a try. File any issues on github.

## Features

- Backup Anytype spaces (full or selective)
  - incremental backup using `--since` (timestamp)
  - selective backup using `--objects` (object list)
  - optional: `--include-files`, `--include-nested`, `--include-archived`
- Restore backups to original or new space
  - optional `--replace` to overwrite existing objects
  - `--dry-run` to validate without importing
- Browse archives with an interactive TUI (`inspect`)
  - view object properties, metadata, and markdown preview
  - export files and simplified object markdown
  - configurable preview cache budget with `--max-cache` (default `200 MiB`)
- List archive contents, compare archives, extract objects

## Commands

```
anyr backup create  --space NAME_OR_ID [OPTIONS]
anyr backup restore ARCHIVE --space NAME_OR_ID [OPTIONS]
anyr backup list    ARCHIVE [--brief|--expanded|--files]
anyr backup manifest ARCHIVE
anyr backup diff    ARCHIVE1 ARCHIVE2
anyr backup extract ARCHIVE ID OUTPUT
anyr backup inspect ARCHIVE [--max-cache SIZE]
```

`export` and `import` remain alternate archive workflows under `anyr backup`.

## Usage Notes

- **Object lists**: `--objects FILE` reads one object ID per line (blank lines and `#` comments ignored). Use `--objects -` for stdin.
- **Backup output**:
  - `--dir DIR` creates a timestamped archive in an existing directory.
  - `--dest PATH` creates an archive at a specific path.
  - `--prefix PREFIX` sets the archive name prefix (with `--dir` or default `.`).
  - Backup produces `.zip` archives.
- **Import modes**:
  - `--import-mode ignore-errors` (default): continue after errors.
  - `--import-mode all-or-nothing`: stop on first error (not transactional; already-imported objects are not rolled back).
- **Restore reports**: `--log REPORT.json` writes a JSON report with success/failure details.
- **Result output**: every non-interactive command writes one result document,
  shaped by the global `anyr` output options - compact JSON (default), `--pretty`,
  `--table` for the human-readable summary, `--quiet` to suppress it, and `-o FILE`
  to write it to a file. The interactive `inspect` command renders a TUI instead.
  Progress and diagnostics stay on stderr. `anyr` rejects impossible combinations
  (several format flags at once, `--quiet` with `-o FILE`, any output flag on
  `inspect`, or a result file that aliases an input or generated artifact) instead
  of silently choosing one.
- **Archive formats**: `list`, `diff`, `inspect`, and `restore` accept both `.zip` archives and unpacked archive directories.
- **Raw extraction**: `extract` writes archived payload bytes for file-layout
  snapshots 8 through 12. A missing payload returns the typed
  `MissingRawPayloadError` without creating the destination.
- **Pre-delete archives**: `anyr space delete SPACE --archive PATH` writes a complete protobuf `.zip` to the exact non-existing path before deletion. Validate the selected file with `anyr backup list PATH --files`.
- **Manifest**: anyback writes manifest metadata to `<archive>.manifest.json`. Archives without manifests (including direct pre-delete and desktop-generated backups) are still supported.

### Workflow deadlines

`backup create` and `backup restore` share one outer deadline per command. The
default is one hour. `ANYBACK_WORKFLOW_TIMEOUT_SECS` accepts canonical decimal
seconds up to 7200; exactly `0` disables only the outer workflow boundary.
Nested HTTP, gRPC, and restore progress limits still apply. Expiry before an
import dispatch is reported as pre-dispatch; after dispatch, the mutation
outcome is indeterminate. Archive, sidecar, report, and result-file writes are
staged under the same deadline, so expiry cannot publish a prepared replacement.
Archive publication binds an owned staging identity before creating the final
path. Its manifest records the exact staged archive length and SHA-256 digest,
then the archive becomes the final commit record. A sidecar left before that
commit cannot validate against a missing or different archive.
Each restore batch binds completion to the collection correlation returned by
its import RPC when the server supplies one: completion then requires the same
correlation on the server's `importFinish` event, and a reused correlation
fails closed. Ordinary object imports create no root collection, so the server
returns no correlation; those batches complete on the dispatch generation alone
(the import process started after the batch's dispatch barrier). Uncorrelated
lifecycle events cannot establish success, while terminal failure events still
fail the restore closed.

Restore progress limits use seconds and remain mandatory:

| Environment variable                   | Default | Maximum |
| -------------------------------------- | ------: | ------: |
| `ANYBACK_EVENT_STREAM_CONNECT_TIMEOUT` |      10 |     120 |
| `ANYBACK_PROCESS_START_TIMEOUT`        |      20 |     300 |
| `ANYBACK_PROCESS_IDLE_TIMEOUT`         |      30 |     300 |
| `ANYBACK_PROCESS_DONE_TIMEOUT`         |    1800 |    3600 |

Every timeout value rejects empty strings, whitespace, signs, leading zeros,
non-decimal input, overflow, and values above its maximum before client
construction.

---

## Development

### Library Crate

This package also exposes a reusable Rust library crate, `anyback_reader`, for
archive traversal and snapshot file inspection. The command types and
`run_command` entry point are embedded by `anyr backup`; the crate has no
standalone top-level parser. Library consumers can use
`default-features = false` to exclude CLI-only dependencies.
Consumers that construct the Anytype client themselves can call
`WorkflowDeadline::from_env` first and pass it to `run_command_with_deadline`;
this validates configuration before client construction.

### Restore Transport

- Default restore transport is path-based (`PbParams.path`).
- Snapshot transport is compiled behind the opt-in `snapshot-import` cargo feature and used for selective restore (`--objects`).
- Snapshot transport supports `*.pb` archives; `*.pb.json` restore is not yet supported.
- Snapshot chunk limits (env overrides):
  - `ANYBACK_IMPORT_MAX_SINGLE_SNAPSHOT_BYTES` (default 2 MiB)
  - `ANYBACK_IMPORT_MAX_BATCH_BYTES` (default 3 MiB)
  - `ANYBACK_IMPORT_MAX_BATCH_SNAPSHOTS` (default 128)

### Required Installed-Binary Live Gate

The protected `anyr-anyback-live` workflow installs `anyr` with `cargo install`,
then runs one exact ignored test serially. The gate creates a source object in a
new prefix-authorized disposable space, runs `anyr backup create`, restores into
a second disposable space, verifies the restored name and canonical Markdown
body without relying on source IDs, and proves that both spaces were removed.

Operators must provide authenticated HTTP and gRPC settings, an absolute path to
a reviewed redacted server log, `ANYTYPE_DISPOSABLE_TEST_PROCESS=1`, and a unique
`ANYTYPE_TEST_SPACE_PREFIX`. Missing admission, credentials, pings, archive
output, callback evidence, or restored content fails the gate. Run this target
alone with `--ignored --exact --test-threads=1`; do not share its server with
parallel mutation suites. Normal workspace tests remain offline because the
live test stays ignored.

The shared live-test command harness validates non-TTY stderr on every `anyr`
attempt, including attempts made by lock-retry policies. Failure reports expose
only exit status, byte lengths, and terminal-control categories; malformed
result documents and archive paths are not echoed into diagnostics.

### Restore Content-Fidelity Live Matrix

Four ignored tests extend the protected disposable-space pattern beyond the
smoke gate:

- `e2e_restore_preserves_file_payload_metadata_and_host_attachment` compares
  restored bytes exactly, checks the file name and MIME type through the current
  file APIs, and resolves a restored host object's file property independently.
- `e2e_restore_preserves_chat_order_reply_and_attachment` verifies two unique
  messages in server order, a destination-resolved reply, and a restored file
  attachment with stable metadata.
- `e2e_restore_preserves_custom_schema_keys_formats_and_featured_membership`
  verifies a custom type key, text/number/checkbox/URL property keys and formats,
  and destination-resolved featured-property membership.
- `e2e_restore_preserves_typed_object_value_fidelity` creates a custom type and
  verifies text, number, checkbox, URL, relation, select, and multi-select host
  values against independently resolved destination resources.

Each test creates and removes unique prefix-owned source and destination spaces,
requires healthy authenticated HTTP and gRPC pings plus a reviewed redacted
server log, and fails rather than using an ambient fixture. Run one test at a
time with `--ignored --exact --test-threads=1`; do not overlap these tests with
another mutation suite.

### Integrity Testing

Fuzz testing for backup/restore roundtrips:

```
cargo test -p anyback --test integrity_nightly -- --ignored --nocapture
```

Environment controls:

| Variable                                      | Example values                     |
| --------------------------------------------- | ---------------------------------- |
| `ANYBACK_INTEGRITY_PROFILE`                   | `tiny`, `small`, `medium`, `large` |
| `ANYBACK_INTEGRITY_ITERATIONS`                | number of iterations               |
| `ANYBACK_INTEGRITY_MAX_OBJECTS_PER_ITERATION` | max objects per iteration          |
| `ANYBACK_INTEGRITY_MAX_BODY_BYTES`            | max body bytes per object          |
| `ANYBACK_INTEGRITY_MAX_SECONDS`               | time limit                         |
| `ANYBACK_INTEGRITY_MAX_TOTAL_OBJECTS`         | total object cap                   |
| `ANYBACK_INTEGRITY_MAX_TOTAL_BODY_BYTES`      | total byte cap                     |
| `ANYBACK_INTEGRITY_SEED`                      | RNG seed for reproducibility       |
| `ANYBACK_INTEGRITY_TYPES`                     | `page,note,task,...`               |
| `ANYBACK_INTEGRITY_FORMAT`                    | `pb` or `pb-json`                  |
