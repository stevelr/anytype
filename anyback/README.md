# anyback

`anyback` is a command-line tool for backing up and restoring Anytype spaces.

See `anyback.1.md` for detailed CLI documentation.

**0.3.0 - Alpha**

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
- List archive contents, compare archives, extract objects

## Commands

```
anyback backup  --space NAME_OR_ID [OPTIONS]
anyback restore ARCHIVE --space NAME_OR_ID [OPTIONS]
anyback list    ARCHIVE [--brief|--expanded|--files]
anyback manifest ARCHIVE
anyback diff    ARCHIVE1 ARCHIVE2
anyback extract ARCHIVE ID OUTPUT
anyback inspect ARCHIVE
anyback auth    ...
```

`export` and `import` are aliases for `backup` and `restore`.

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
- **Archive formats**: `list`, `diff`, `inspect`, and `restore` accept both `.zip` archives and unpacked archive directories.
- **Manifest**: anyback writes manifest metadata to `<archive>.manifest.json`. Archives without manifests (e.g. desktop-generated backups) are still supported.

---

## Development

### Library Crate

This package also exposes a reusable Rust library crate, `anyback_reader`, for archive traversal and snapshot file inspection. Library consumers can use `default-features = false` to exclude CLI-only dependencies.

### Restore Transport

- Default restore transport is path-based (`PbParams.path`).
- Snapshot transport is compiled behind the opt-in `snapshot-import` cargo feature and used for selective restore (`--objects`).
- Snapshot transport supports `*.pb` archives; `*.pb.json` restore is not yet supported.
- Snapshot chunk limits (env overrides):
  - `ANYBACK_IMPORT_MAX_SINGLE_SNAPSHOT_BYTES` (default 2 MiB)
  - `ANYBACK_IMPORT_MAX_BATCH_BYTES` (default 3 MiB)
  - `ANYBACK_IMPORT_MAX_BATCH_SNAPSHOTS` (default 128)

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
