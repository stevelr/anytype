+++
title = "Backup and restore"
weight = 20
+++

# Back up and restore Anytype spaces

`anyr backup` creates, restores, and inspects Anytype archives. Creating,
exporting, or applying a restore requires
[gRPC access](/reference/connections/). Archive inspection works offline; a
restore dry run uses HTTP only to resolve its destination space.

## Create a backup

Write a timestamped ZIP archive beneath an existing directory:

```sh
anyr backup create --space "Work" --dir ./backups --include-files
```

Use `--dest` when a script needs an exact, non-existing output path:

```sh
anyr backup create --space "Work" \
  --dest ./backups/work.zip --include-files
```

Useful selection options include:

- `--objects FILE` reads one object ID per line. Use `--objects -` for standard
  input.
- `--types page,note` selects object types.
- `--include-files` includes file objects and their payloads.
- `--include-nested`, `--include-archived`, and `--include-backlinks` include
  related content that the default selection omits.
- For an incremental backup, use `--mode incremental --since TIMESTAMP`.

Run `anyr backup create --help` for the complete selection and archive options.

## Check an archive

List objects and file payloads before restoring:

```sh
anyr backup list ./backups/work.zip --files --table
anyr backup manifest ./backups/work.zip --pretty
```

Compare two archives or extract one payload:

```sh
anyr backup diff ./backups/old.zip ./backups/new.zip --table
anyr backup extract ./backups/work.zip "$OBJECT_ID" ./object.md
```

`list`, `manifest`, `diff`, `extract`, and `inspect` accept ZIP archives. The
commands that traverse archive contents also accept unpacked archive
directories.

Open the interactive archive browser with:

```sh
anyr backup inspect ./backups/work.zip
```

The inspector owns the terminal until you quit it.

## Restore a backup

Validate the archive and target without importing:

```sh
anyr backup restore ./backups/work.zip \
  --space "Restored Work" --dry-run
```

Restore and write a machine-readable report:

```sh
anyr backup restore ./backups/work.zip \
  --space "Restored Work" --log ./restore.json
```

The default `--import-mode ignore-errors` continues after an object error.
`--import-mode all-or-nothing` stops after the first error, but does not roll
back objects already imported. Use `--replace` only when restored objects may
overwrite matching objects in the destination.

## Export portable Markdown

`backup export` uses Anytype's native exporters. Use it for interchange or
indexing; use `backup create` when you need the protobuf representation for a
later restore. This command requires gRPC access. The
[Markdown export guide](/guides/export-markdown/) covers selection,
attachments, and front matter.

## Output and deadlines

Non-interactive backup commands follow the global `anyr` output contract:
compact JSON by default, `--pretty`, `--table`, `--quiet`, or `--output FILE`.
Progress and diagnostics use stderr.

Create and restore share one outer deadline. Set
`ANYBACK_WORKFLOW_TIMEOUT_SECS` to decimal seconds up to `7200`; `0` disables
only this outer boundary. Lower-level request and process deadlines still
apply. A timeout after an import was dispatched leaves that mutation's outcome
indeterminate, so inspect the destination before retrying.
